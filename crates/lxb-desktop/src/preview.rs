//! A PlayStation 3 or PlayStation Portable game's film and music, while its
//! row is chosen — the way each console's own menu played them.
//!
//! A PS3 game carries a short film, `ICON1.PAM`, that the XMB plays in place
//! of the game's icon when the game is highlighted, and music, `SND0.AT3`,
//! that plays behind it. A PSP game carries the same two, the film as
//! `ICON1.PMF`. The user asked for the same here, sound and all, for both:
//! the film plays on the game's own card and the music under it, and both
//! stop the moment the cursor moves on or anything covers the start screen.
//!
//! ## How the film reaches the card
//!
//! Not through a drawing path of its own. The card already draws the game's
//! icon out of a block of the thumbnail atlas (see `gpu::Gpu::put_thumbnail`),
//! so each frame of the film is written into *that block*, at the size the
//! icon was put there at, and the card carries on drawing what it always
//! draws. When the film stops the icon is written back over the last frame.
//! Nothing downstream of the atlas knows a film was ever there.
//!
//! The film is decoded on a thread of its own, by the same library the
//! wallpaper's films are, paced by the film's own frame rate and looped.
//! The music is decoded whole on the same thread — a PS3 game's music is a
//! minute or two — and handed to [`crate::sound::Sounds`], which loops it.
//!
//! ## When
//!
//! After the cursor has rested on the row for [`DWELL`], as on the console:
//! scrolling down a column past thirty games is not thirty films starting and
//! stopping, and nothing is decoded for a row passed over.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ffmpeg_next as ffmpeg;

/// How long the cursor rests on a game before its film and music begin.
pub const DWELL: Duration = Duration::from_millis(900);

/// What a chosen row asks for: the icon's own file (which names its block of
/// the atlas), and the film and the music where the game has them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Want {
    pub icon: PathBuf,
    pub film: Option<PathBuf>,
    pub music: Option<PathBuf>,
}

/// One frame of the film, straight RGBA at the icon's size.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// A game's music, decoded: interleaved samples, and how to read them.
pub struct Music {
    pub channels: u16,
    pub rate: u32,
    pub samples: Vec<f32>,
}

/// What the worker hands back, in one place the shell takes it from.
#[derive(Default)]
struct Out {
    frame: Option<Frame>,
    music: Option<Arc<Music>>,
}

struct Playing {
    want: Want,
    /// The size the icon is drawn at in the atlas, which every frame is
    /// scaled to.
    size: (u32, u32),
    stop: Arc<AtomicBool>,
    out: Arc<Mutex<Out>>,
    /// Whether any frame has been written over the icon, and so whether the
    /// icon has to be written back.
    painted: bool,
}

/// The preview for the display that has the cursor.
#[derive(Default)]
pub struct Preview {
    /// What is wanted now, and since when — the dwell counts from here.
    wanted: Option<(Want, Instant)>,
    playing: Option<Playing>,
    /// An icon whose block holds a film's frame and has to be put back.
    to_restore: Option<PathBuf>,
    /// The music playing, for the shell's sound to follow.
    music: Option<Arc<Music>>,
}

impl Preview {
    /// Say what the chosen row wants — or `None` for a row with nothing to
    /// play, a cursor elsewhere, or a start screen something has covered.
    ///
    /// `size` is the icon's size in the atlas, where it is there at all: a
    /// film is only started over an icon that is being drawn.
    pub fn want(&mut self, want: Option<Want>, size: Option<(u32, u32)>, now: Instant) {
        if self.wanted.as_ref().map(|(held, _)| held) != want.as_ref() {
            self.stop();
            self.wanted = want.map(|want| (want, now));
        }
        let Some((want, since)) = self.wanted.clone() else {
            return;
        };
        if self.playing.is_some() || now.saturating_duration_since(since) < DWELL {
            return;
        }
        let Some(size) = size.filter(|(width, height)| *width > 0 && *height > 0) else {
            // No icon on the card yet to play over: the film waits for it; the
            // music does not need one.
            if want.film.is_some() && want.music.is_none() {
                return;
            }
            self.start(want, (0, 0));
            return;
        };
        self.start(want, size);
    }

    fn start(&mut self, want: Want, size: (u32, u32)) {
        let stop = Arc::new(AtomicBool::new(false));
        let out = Arc::new(Mutex::new(Out::default()));
        let (worker_stop, worker_out, worker_want) =
            (Arc::clone(&stop), Arc::clone(&out), want.clone());
        let started = std::thread::Builder::new()
            .name("lxb-game-preview".to_string())
            .spawn(move || run(&worker_want, size, &worker_stop, &worker_out));
        if let Err(err) = started {
            tracing::warn!(%err, "no thread for a game's preview");
            return;
        }
        tracing::debug!(icon = %want.icon.display(), film = ?want.film, music = ?want.music, "a game's preview begins");
        self.playing = Some(Playing {
            want,
            size,
            stop,
            out,
            painted: false,
        });
    }

    /// Stop whatever is playing: the film's worker is told to end, the icon is
    /// due to be put back, and the music is let go.
    pub fn stop(&mut self) {
        if let Some(playing) = self.playing.take() {
            playing.stop.store(true, Ordering::Relaxed);
            if playing.painted {
                self.to_restore = Some(playing.want.icon);
            }
        }
        self.music = None;
        self.wanted = None;
    }

    /// The newest frame of the film, for the icon's block — `None` when there
    /// is nothing new since the last one.
    pub fn frame(&mut self) -> Option<(PathBuf, Frame)> {
        let playing = self.playing.as_mut()?;
        let frame = playing.out.lock().ok()?.frame.take()?;
        if (frame.width, frame.height) != playing.size {
            return None;
        }
        playing.painted = true;
        Some((playing.want.icon.clone(), frame))
    }

    /// An icon a film was drawn over and has stopped, to be written back.
    pub fn restore(&mut self) -> Option<PathBuf> {
        self.to_restore.take()
    }

    /// The music that should be playing now, once it has been decoded.
    pub fn music(&mut self) -> Option<Arc<Music>> {
        if let Some(playing) = self.playing.as_ref() {
            if let Some(music) = playing.out.lock().ok().and_then(|mut out| out.music.take()) {
                self.music = Some(music);
            }
        }
        self.music.clone()
    }
}

impl Drop for Preview {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The worker: the music first (it is quick, and it is what is heard), then
/// the film, looped until told to stop.
fn run(want: &Want, size: (u32, u32), stop: &AtomicBool, out: &Mutex<Out>) {
    if ffmpeg::init().is_err() {
        return;
    }
    ffmpeg::log::set_level(ffmpeg::log::Level::Fatal);
    // The game's own music, or where it has none, the film's soundtrack.
    let music = want
        .music
        .as_deref()
        .and_then(|at| decode_music(at, stop))
        .or_else(|| want.film.as_deref().and_then(|at| decode_music(at, stop)));
    if let Some(music) = music {
        if let Ok(mut out) = out.lock() {
            out.music = Some(Arc::new(music));
        }
    }
    if let (Some(film), true) = (want.film.as_deref(), size.0 > 0) {
        if play_film(film, size, stop, out).is_none() {
            tracing::info!(film = %film.display(), "this game's film could not be played");
        }
    }
}

/// Decode a whole sound file into interleaved samples, or `None` for one with
/// no sound this can read. A PS3 or PSP game's music is ATRAC3 or ATRAC3plus
/// in a RIFF file, which `libavcodec` decodes as planar floats.
fn decode_music(path: &Path, stop: &AtomicBool) -> Option<Music> {
    let mut input = open(path)?;
    let (index, parameters) = {
        let stream = input.streams().best(ffmpeg::media::Type::Audio)?;
        (stream.index(), stream.parameters())
    };
    let mut decoder = ffmpeg::codec::context::Context::from_parameters(parameters)
        .ok()?
        .decoder()
        .audio()
        .ok()?;
    // Ten minutes at most: a game's music loops after a minute or two, and a
    // file that goes on for longer is not a game's music.
    const MOST: usize = 10 * 60 * 48_000 * 2;
    let mut samples: Vec<f32> = Vec::new();
    let mut channels = 0u16;
    let mut rate = 0u32;
    let mut frame = ffmpeg::frame::Audio::empty();
    for (from, packet) in input.packets() {
        if stop.load(Ordering::Relaxed) || samples.len() > MOST {
            return None;
        }
        if from.index() != index || decoder.send_packet(&packet).is_err() {
            continue;
        }
        while decoder.receive_frame(&mut frame).is_ok() {
            channels = frame.channels();
            rate = frame.rate();
            interleave(&frame, &mut samples);
        }
    }
    if samples.is_empty() || channels == 0 || rate == 0 {
        return None;
    }
    Some(Music {
        channels,
        rate,
        samples,
    })
}

/// One decoded frame's samples, as interleaved floats, whatever layout the
/// decoder chose. The formats a game's music comes out in; anything else is
/// skipped rather than played as noise.
fn interleave(frame: &ffmpeg::frame::Audio, into: &mut Vec<f32>) {
    use ffmpeg::format::sample::{Sample, Type};
    let channels = frame.channels() as usize;
    let count = frame.samples();
    if channels == 0 {
        return;
    }
    let read = |plane: usize, at: usize, kind: Sample| -> Option<f32> {
        let data = frame.data(plane);
        Some(match kind {
            Sample::F32(_) => f32::from_ne_bytes(data.get(at * 4..at * 4 + 4)?.try_into().ok()?),
            Sample::I16(_) => {
                f32::from(i16::from_ne_bytes(
                    data.get(at * 2..at * 2 + 2)?.try_into().ok()?,
                )) / 32768.0
            }
            Sample::I32(_) => {
                i32::from_ne_bytes(data.get(at * 4..at * 4 + 4)?.try_into().ok()?) as f32
                    / 2_147_483_648.0
            }
            _ => return None,
        })
    };
    let kind = frame.format();
    for sample in 0..count {
        for channel in 0..channels {
            let value = match kind {
                Sample::F32(Type::Planar)
                | Sample::I16(Type::Planar)
                | Sample::I32(Type::Planar) => read(channel, sample, kind),
                Sample::F32(Type::Packed)
                | Sample::I16(Type::Packed)
                | Sample::I32(Type::Packed) => read(0, sample * channels + channel, kind),
                _ => None,
            };
            match value {
                Some(value) => into.push(value),
                None => return,
            }
        }
    }
}

/// Decode the film and hand each frame over at its own pace, from the start
/// again at the end, until told to stop. `None` when there is no picture in
/// it that this can scale.
fn play_film(
    path: &Path,
    (width, height): (u32, u32),
    stop: &AtomicBool,
    out: &Mutex<Out>,
) -> Option<()> {
    let mut input = open(path)?;
    let (index, interval, parameters) = {
        let stream = input.streams().best(ffmpeg::media::Type::Video)?;
        (
            stream.index(),
            pace(stream.avg_frame_rate()),
            stream.parameters(),
        )
    };
    let mut decoder = ffmpeg::codec::context::Context::from_parameters(parameters)
        .ok()?
        .decoder()
        .video()
        .ok()?;
    // Checked before a scaler is asked for: `libswscale` aborts the process on
    // a format it has no descriptor for — see `paper::decode`.
    if decoder.format() == ffmpeg::format::Pixel::None
        || decoder.width() == 0
        || decoder.height() == 0
    {
        return None;
    }
    let mut scaler = ffmpeg::software::scaling::Context::get(
        decoder.format(),
        decoder.width(),
        decoder.height(),
        ffmpeg::format::Pixel::RGBA,
        width,
        height,
        ffmpeg::software::scaling::Flags::BILINEAR,
    )
    .ok()?;
    let mut decoded = ffmpeg::frame::Video::empty();
    let mut scaled = ffmpeg::frame::Video::empty();
    let mut due = Instant::now();
    let mut drawn = false;
    loop {
        let mut any = false;
        for (from, packet) in input.packets() {
            if from.index() != index || decoder.send_packet(&packet).is_err() {
                continue;
            }
            while decoder.receive_frame(&mut decoded).is_ok() {
                if scaler.run(&decoded, &mut scaled).is_err() {
                    continue;
                }
                let row = width as usize * 4;
                let stride = scaled.stride(0);
                let source = scaled.data(0);
                let mut pixels = Vec::with_capacity(row * height as usize);
                for line in 0..height as usize {
                    pixels.extend_from_slice(source.get(line * stride..line * stride + row)?);
                }
                if let Ok(mut out) = out.lock() {
                    out.frame = Some(Frame {
                        width,
                        height,
                        pixels,
                    });
                }
                any = true;
                drawn = true;
                // Counted from when the last frame was due, so decoding comes
                // out of the interval rather than being added to it — the
                // wallpaper's rule, for the wallpaper's reason.
                let now = Instant::now();
                if due + interval < now {
                    due = now;
                }
                due += interval;
                while Instant::now() < due {
                    if stop.load(Ordering::Relaxed) {
                        return Some(());
                    }
                    std::thread::sleep((due - Instant::now()).min(Duration::from_millis(20)));
                }
                if stop.load(Ordering::Relaxed) {
                    return Some(());
                }
            }
        }
        if !drawn || !any {
            return drawn.then_some(());
        }
        decoder.flush();
        if input.seek(0, ..).is_err() {
            return Some(());
        }
    }
}

/// Open a film or a sound for reading.
///
/// `ICON1.PAM` is Sony's PAMF — an MPEG program stream behind a 2048-byte
/// header of its own — and `.pam` is also the Portable Arbitrary Map, an image
/// format, which is what `libavformat` takes a file of that name for when it
/// is left to guess: one frame, no stream it can decode. So a PAM is opened as
/// an MPEG program stream by name, which finds its way past the header on its
/// own. The PSP's `ICON1.PMF` is the same thing a console earlier — PSMF, the
/// same header before the same kind of stream, H.264 in it — and is opened the
/// same way rather than left to a guess that happens to come out right today.
/// Anything else is left to the library.
fn open(path: &Path) -> Option<ffmpeg::format::context::Input> {
    let pamf = path.extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("pam") || extension.eq_ignore_ascii_case("pmf")
    });
    if !pamf {
        return ffmpeg::format::input(path).ok();
    }
    // SAFETY: `av_find_input_format` looks a demuxer up by name and returns a
    // pointer to its static description, or null; it is only read from.
    let found = unsafe { ffmpeg::ffi::av_find_input_format(c"mpeg".as_ptr()) };
    if found.is_null() {
        return None;
    }
    // SAFETY: a non-null pointer to a static demuxer description, as above.
    let format = unsafe { ffmpeg::format::format::Input::wrap(found.cast_mut()) };
    match ffmpeg::format::open(path, &ffmpeg::format::format::Format::Input(format)).ok()? {
        ffmpeg::format::context::Context::Input(input) => Some(input),
        ffmpeg::format::context::Context::Output(_) => None,
    }
}

/// How long one frame is on the card, from the rate the stream declares —
/// clamped against a stream that declares something absurd.
fn pace(rate: ffmpeg::Rational) -> Duration {
    let (numerator, denominator) = (rate.numerator() as f64, rate.denominator() as f64);
    if numerator <= 0.0 || denominator <= 0.0 {
        return Duration::from_millis(33);
    }
    Duration::from_secs_f64((denominator / numerator).clamp(1.0 / 120.0, 0.5))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn want(icon: &str) -> Want {
        Want {
            icon: PathBuf::from(icon),
            film: None,
            music: None,
        }
    }

    /// Nothing starts while the cursor is passing over a row.
    #[test]
    fn a_preview_waits_for_the_cursor_to_rest() {
        let mut preview = Preview::default();
        let now = Instant::now();
        preview.want(Some(want("/a/ICON0.PNG")), Some((256, 141)), now);
        assert!(preview.playing.is_none());
        // Another row before the dwell is out: the clock starts again.
        preview.want(
            Some(want("/b/ICON0.PNG")),
            Some((256, 141)),
            now + DWELL / 2,
        );
        preview.want(Some(want("/b/ICON0.PNG")), Some((256, 141)), now + DWELL);
        assert!(preview.playing.is_none());
        preview.want(
            Some(want("/b/ICON0.PNG")),
            Some((256, 141)),
            now + DWELL * 2,
        );
        assert!(preview.playing.is_some());
        preview.want(None, None, now + DWELL * 3);
        assert!(preview.playing.is_none());
        // Nothing was drawn over the icon, so nothing has to be put back.
        assert_eq!(preview.restore(), None);
    }

    /// A film laid out the way Sony lays one out — a 2048-byte header starting
    /// with `magic`, then an MPEG program stream — made with `ffmpeg` out of a
    /// test pattern. `None` where there is no encoder to make one.
    fn sony_film(
        dir: &Path,
        name: &str,
        magic: &[u8],
        size: &str,
        codec: &[&str],
    ) -> Option<PathBuf> {
        let stream = dir.join(format!("{name}.vob"));
        let made = std::process::Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
            ])
            .arg(format!("testsrc=size={size}:rate=30"))
            .args(["-t", "1"])
            .args(codec)
            .args(["-f", "vob"])
            .arg(&stream)
            .status()
            .is_ok_and(|status| status.success());
        if !made {
            return None;
        }
        let mut film = magic.to_vec();
        film.resize(2048, 0);
        film.extend(std::fs::read(&stream).ok()?);
        let at = dir.join(name);
        std::fs::write(&at, film).ok()?;
        Some(at)
    }

    /// The first frame the film's worker hands over, as its size and length.
    /// One frame is enough to know it decodes: stopped as soon as one is out.
    fn first_frame(film: &Path, size: (u32, u32)) -> Option<(u32, u32, usize)> {
        ffmpeg::init().unwrap();
        let input = open(film).expect("opened as MPEG");
        assert!(input.streams().best(ffmpeg::media::Type::Video).is_some());
        let stop = AtomicBool::new(false);
        let out = Mutex::new(Out::default());
        std::thread::scope(|scope| {
            scope.spawn(|| play_film(film, size, &stop, &out));
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(frame) = out.lock().unwrap().frame.take() {
                    stop.store(true, Ordering::Relaxed);
                    break Some((frame.width, frame.height, frame.pixels.len()));
                }
                if Instant::now() > deadline {
                    stop.store(true, Ordering::Relaxed);
                    break None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        })
    }

    /// A PAMF film opens as the MPEG program stream it is, whatever its
    /// extension says — built here the way ICON1.PAM is laid out: a 2048-byte
    /// header, then the stream. Skipped where there is no encoder to make one.
    #[test]
    fn a_pamf_film_is_read_past_its_header() {
        let dir = std::env::temp_dir().join(format!("lxb-pamf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let Some(film) = sony_film(
            &dir,
            "ICON1.PAM",
            b"PAMF0041",
            "320x176",
            &["-c:v", "mpeg2video"],
        ) else {
            eprintln!("no ffmpeg to build a film with; skipped");
            return;
        };
        assert_eq!(
            first_frame(&film, (256, 141)),
            Some((256, 141, 256 * 141 * 4))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A PSP's film is the same thing a console earlier: PSMF, the same header
    /// before an MPEG program stream, with H.264 in it at the icon's 144 by 80.
    /// Skipped where there is no H.264 encoder to make one.
    #[test]
    fn a_psp_film_is_read_past_its_header() {
        let dir = std::env::temp_dir().join(format!("lxb-psmf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let codec = ["-c:v", "libx264", "-pix_fmt", "yuv420p"];
        let Some(film) = sony_film(&dir, "ICON1.PMF", b"PSMF0014", "144x80", &codec) else {
            eprintln!("no ffmpeg with H.264 to build a film with; skipped");
            return;
        };
        assert_eq!(first_frame(&film, (144, 80)), Some((144, 80, 144 * 80 * 4)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A game's music comes out as interleaved samples whichever layout the
    /// decoder hands over: packed 16-bit (a plain WAV) and planar float (what
    /// ATRAC3 decodes to, and Vorbis here, which is the same layout).
    #[test]
    fn music_is_interleaved_from_either_layout() {
        let dir = std::env::temp_dir().join(format!("lxb-music-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, codec) in [("SND0.AT3", "pcm_s16le"), ("music.ogg", "libvorbis")] {
            let at = dir.join(name);
            let format = if name.ends_with(".ogg") { "ogg" } else { "wav" };
            let made = std::process::Command::new("ffmpeg")
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                ])
                .arg("sine=frequency=440:duration=1")
                .args(["-ac", "2", "-ar", "44100", "-c:a", codec, "-f", format])
                .arg(&at)
                .status()
                .is_ok_and(|status| status.success());
            if !made {
                eprintln!("no ffmpeg {codec} to build music with; skipped");
                continue;
            }
            ffmpeg::init().unwrap();
            let music = decode_music(&at, &AtomicBool::new(false)).expect("music");
            assert_eq!((music.channels, music.rate), (2, 44100), "{name}");
            // A second of stereo, give or take an encoder's padding.
            assert!(
                (80_000..100_000).contains(&music.samples.len()),
                "{name}: {}",
                music.samples.len()
            );
            let loudest = music
                .samples
                .iter()
                .fold(0.0f32, |most, sample| most.max(sample.abs()));
            assert!(loudest > 0.05, "{name} is silent: {loudest}");
            assert!(
                music.samples.iter().all(|sample| sample.abs() <= 1.01),
                "{name} is not in range"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_film_s_pace_is_its_own_within_reason() {
        assert_eq!(
            pace(ffmpeg::Rational::new(30, 1)),
            Duration::from_secs_f64(1.0 / 30.0)
        );
        assert_eq!(pace(ffmpeg::Rational::new(0, 1)), Duration::from_millis(33));
        assert_eq!(
            pace(ffmpeg::Rational::new(1, 60)),
            Duration::from_millis(500)
        );
    }
}
