//! The picture behind everything, when it is one of the user's own.
//!
//! The wallpaper this shell draws is a function — see `shaders.wgsl` — and
//! everything about the way it is used depends on that: a pane of glass refracts
//! it by evaluating it again at the bent coordinate, the guide softens it by
//! drawing it wider and dimmer, the overview draws the whole of it inside a
//! card. A photograph can do none of that by being a photograph. So a custom
//! wallpaper is not a second background drawn somewhere else; it is a texture
//! the one wallpaper function reads *instead of* computing a scene, with a chain
//! of ever-smaller copies under it so that everything which asks the wallpaper
//! for a blurred version of itself still gets one.
//!
//! This module is the part of that on the CPU: which file, getting frames out of
//! it, and keeping a copy of it where the shell can be sure of finding it again.
//!
//! ## Everything is a film with one frame in it
//!
//! A picture and a film arrive by exactly the same path — `libavformat` opens
//! the file, `libavcodec` decodes the video stream, `libswscale` scales what
//! comes out into the box the shell holds wallpapers in. A JPEG is a stream of
//! one frame and a film is a stream of thousands, and the only thing that
//! differs is whether the thread stays alive after the first one.
//!
//! That is worth more than the obvious alternative of decoding pictures with the
//! `image` crate and films with this: it is one decoder to be right about, and
//! it is the *widest* one — a shell whose file listing calls an `.avif` and a
//! `.heic` pictures should not then refuse to show them, and `image` is built
//! here without either. The one exception is a vector drawing, which has no
//! pixels to decode and is rendered at the size it will be shown; see
//! [`vector`].
//!
//! ## Which is also why it cannot make a sound
//!
//! A wallpaper that started playing somebody's holiday video's soundtrack over
//! the shell's own music would be a wallpaper nobody could use, and there are
//! three reasons it cannot happen — none of which is a flag anybody has to
//! remember to set:
//!
//! * **The audio stream is never opened.** The demuxer's packets are filtered to
//!   the one video stream's index and everything else is dropped on the floor,
//!   and no audio decoder is ever made to hand them to.
//! * **The crate is cut** to `codec`, `format` and `software-scaling` — no
//!   `software-resampling`, no `device` — so nothing in this program's own API
//!   surface can turn a compressed soundtrack into samples or open an output to
//!   put them on. (`libswresample` is in the process all the same: the system's
//!   `libavformat` links it for itself. Nothing here can reach it.)
//! * **The only thing in this shell that makes a noise is [`crate::sound`]**,
//!   and it plays four embedded clips and one embedded piece of music. There is
//!   no path from a file the user chose to a sound device.
//!
//! ## What it costs, and when it costs nothing
//!
//! A film is decoded and scaled at its own frame rate and no faster, into
//! [`FILM`] at the most, and the newest frame overwrites an older one nobody
//! drew — a shell drawing at 30 fps in front of a 60 fps film costs 30 uploads a
//! second, not 60. When the wallpaper stops being drawn at all, because an
//! application has the screen or the display is resting, the shell stops saying
//! it wants frames and the decoder parks on a condition variable within
//! [`PATIENCE`]. A film behind a full-screen game costs one sleeping thread.

use ffmpeg_next as ffmpeg;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// The largest a still picture is held at, down its longest edge.
///
/// A wallpaper is stretched across the whole display, which is the one thing in
/// this shell that is never drawn small — so it is held at the size of the
/// largest display anybody is likely to put it on rather than at the size of a
/// thumbnail. Above this it is scaled down: a forty-megapixel photograph off a
/// camera is detail no screen can show, kept in memory for the whole session.
pub const STILL: u32 = 3840;

/// And the largest a film's frames are decoded at.
///
/// Deliberately lower than [`STILL`], because a film pays this cost sixty times
/// a second rather than once: a frame at this size is eight megabytes to scale,
/// hand over and upload, and at 4K it would be four times that for a picture
/// that is behind everything and is mostly being looked past. What a 4K display
/// shows is this scaled up, which for moving pictures is a trade nobody can see
/// and a machine can afford.
pub const FILM: u32 = 1920;

/// How long the decoder goes on working after the shell last said it wanted a
/// frame, before it parks.
///
/// Long enough to cover a frame the shell was slow to draw, short enough that a
/// game launched over the top of a moving wallpaper has the machine to itself
/// almost at once. See [`Paper::wanted`].
const PATIENCE: Duration = Duration::from_millis(500);

/// One frame, ready to be handed to the GPU.
///
/// `RGBA`, eight bits a channel, sRGB — which is what the texture holding it is,
/// so nothing between here and the screen has to convert anything.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// The user's own wallpaper: which file it is, and the frames coming out of it.
pub struct Paper {
    /// The file being shown. The file the user pressed at first, and the shell's
    /// own copy of it from the moment that copy exists — see [`Paper::kept`].
    showing: Option<PathBuf>,
    reel: Option<Arc<Reel>>,
    /// A copy being made on a thread, and where it landed.
    ///
    /// On a thread because a wallpaper can be a four-gigabyte film, and copying
    /// one on the thread that draws would be a shell that stopped for a minute
    /// because somebody chose a wallpaper. Nothing waits for it: the picture is
    /// on screen from the file the user pressed within a frame or two, and the
    /// copy quietly becomes the file being read when it is ready.
    keeping: Option<std::sync::mpsc::Receiver<std::io::Result<PathBuf>>>,
}

impl Paper {
    pub fn new() -> Paper {
        Paper {
            showing: None,
            reel: None,
            keeping: None,
        }
    }

    /// Show this file, from its first frame.
    ///
    /// Starting again from the beginning even if it is the file already showing:
    /// this is called when the setting changes, and the honest answer to
    /// "show me this" is the picture, not wherever a film happened to have got
    /// to. Cheap either way — a still delivers one frame and parks.
    pub fn show(&mut self, path: &Path) {
        // The reel and not the copy: this is called the moment the setting
        // changes, which on a press is a fraction of a second after the copy of
        // that very file was started. Giving up the copy here would mean the
        // setting went on naming somebody's Downloads folder for good, which is
        // the one thing keeping a copy is for.
        self.end_reel();
        let reel = Arc::new(Reel::new());
        let worker = Arc::clone(&reel);
        let file = path.to_path_buf();
        // A thread of its own rather than a pool: there is exactly one wallpaper
        // and it is either being decoded or it is not.
        std::thread::Builder::new()
            .name("lxb-wallpaper".to_string())
            .spawn(move || run(&worker, &file))
            .ok();
        self.showing = Some(path.to_path_buf());
        self.reel = Some(reel);
    }

    /// Stop showing anything: the setting has been changed back to one of the
    /// shell's own materials, or the file could not be read.
    pub fn stop(&mut self) {
        self.end_reel();
        self.showing = None;
        // A copy still being made is abandoned rather than waited for: the
        // thread finishes it and nothing reads the answer. What it leaves behind
        // is a whole file in the shell's own directory, which is exactly what
        // the next choice overwrites.
        self.keeping = None;
    }

    /// Give up whatever is being decoded, and nothing else.
    fn end_reel(&mut self) {
        if let Some(reel) = self.reel.take() {
            reel.stop();
        }
    }

    /// The file being shown, if any.
    pub fn showing(&self) -> Option<&Path> {
        self.showing.as_deref()
    }

    /// Start copying `source` into the shell's own directory, on a thread.
    ///
    /// Called on the press that chooses a wallpaper, beside [`Paper::show`] and
    /// after it: what the user is waiting to see is the picture, and the copy is
    /// insurance against a folder they tidy next month.
    pub fn keep_later(&mut self, source: &Path) {
        let (send, done) = std::sync::mpsc::channel();
        let source = source.to_path_buf();
        std::thread::Builder::new()
            .name("lxb-wallpaper-copy".to_string())
            .spawn(move || {
                let _ = send.send(keep(&source));
            })
            .ok();
        self.keeping = Some(done);
    }

    /// Where the copy landed, once it has, and only once.
    ///
    /// The file being shown becomes the copy at the same moment, and the reel
    /// reading it is deliberately left alone: it is the same bytes under another
    /// name, and starting the film again from the beginning because the shell
    /// finished tidying up would be a visible answer to something invisible.
    ///
    /// `None` while it is still being copied, and `None` for a copy that could
    /// not be made — which is not a failure of the wallpaper. The file the user
    /// pressed is still on the disk and still being drawn; what is lost is only
    /// the promise that it will still be there if they move it, and that is said
    /// in the log rather than to their face.
    pub fn kept(&mut self) -> Option<PathBuf> {
        use std::sync::mpsc::TryRecvError;
        let keeping = self.keeping.as_ref()?;
        match keeping.try_recv() {
            Ok(Ok(kept)) => {
                self.keeping = None;
                self.showing = Some(kept.clone());
                Some(kept)
            }
            Ok(Err(error)) => {
                tracing::warn!(
                    %error,
                    "the wallpaper could not be copied, so it is drawn from where it is"
                );
                self.keeping = None;
                None
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.keeping = None;
                None
            }
        }
    }

    /// Say that the wallpaper is being drawn, which is what keeps a film moving.
    ///
    /// Called once a frame from the drawing, and it is the whole of the pacing
    /// policy: a display nobody can see is not drawn, so nothing says it wants a
    /// frame, so the decoder parks. See [`PATIENCE`].
    pub fn wanted(&self) {
        if let Some(reel) = &self.reel {
            reel.wanted();
        }
    }

    /// The newest frame nobody has drawn yet, if one has arrived.
    ///
    /// `spare` is a buffer a previous frame left behind once it had been
    /// uploaded; handing it back lets the decoder fill it again instead of
    /// allocating eight megabytes sixty times a second.
    pub fn take(&mut self, spare: Option<Vec<u8>>) -> Option<Frame> {
        let reel = self.reel.as_ref()?;
        let mut held = reel.held.lock().unwrap_or_else(|held| held.into_inner());
        if let Some(spare) = spare {
            held.spare = Some(spare);
        }
        held.newest.take()
    }

    /// Whether the file being shown turned out not to be one this shell can
    /// draw: it is not there, nothing decodes it, or it holds no picture.
    ///
    /// Asked rather than reported, because there is nothing to report *to* from
    /// a decoder thread — and because the two callers want opposite things from
    /// the answer. A press puts the setting back the way it was and says so out
    /// loud; a session starting up leaves the setting alone and draws the
    /// shell's own wallpaper, because the file may be on a drive that is not
    /// plugged in this morning.
    pub fn trouble(&self) -> bool {
        self.reel
            .as_ref()
            .is_some_and(|reel| reel.trouble.load(Ordering::Relaxed))
    }
}

/// What the decoder and the shell hand back and forth.
struct Reel {
    held: Mutex<Held>,
    /// When the shell last said it wanted a frame, and whether this reel has
    /// been given up. One lock for the two of them because the decoder waits on
    /// exactly this pair: work when one is recent, stop when the other is set.
    watch: Mutex<Watch>,
    wake: Condvar,
    trouble: AtomicBool,
}

/// The one frame in flight, and the buffer the last one left behind.
#[derive(Default)]
struct Held {
    /// Newest wins: a frame the shell never drew is a frame nobody will miss,
    /// and holding a queue of them would be a wallpaper running late rather than
    /// one dropping frames.
    newest: Option<Frame>,
    spare: Option<Vec<u8>>,
}

struct Watch {
    wanted: Instant,
    stopped: bool,
}

impl Reel {
    fn new() -> Reel {
        Reel {
            held: Mutex::new(Held::default()),
            watch: Mutex::new(Watch {
                // Wanted as of now, so the first frame is decoded before
                // anything has had a chance to ask for one. A wallpaper that
                // waited for the first draw to be asked for would arrive a frame
                // after the setting changed, which is exactly the frame somebody
                // is looking at when they press.
                wanted: Instant::now(),
                stopped: false,
            }),
            wake: Condvar::new(),
            trouble: AtomicBool::new(false),
        }
    }

    fn wanted(&self) {
        let mut watch = self.watch.lock().unwrap_or_else(|held| held.into_inner());
        watch.wanted = Instant::now();
        drop(watch);
        self.wake.notify_all();
    }

    fn stop(&self) {
        let mut watch = self.watch.lock().unwrap_or_else(|held| held.into_inner());
        watch.stopped = true;
        drop(watch);
        self.wake.notify_all();
    }

    fn stopped(&self) -> bool {
        self.watch
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .stopped
    }

    /// Hand a finished frame over, and take back whatever buffer is going
    /// spare.
    fn deliver(&self, frame: Frame) -> Option<Vec<u8>> {
        let mut held = self.held.lock().unwrap_or_else(|held| held.into_inner());
        let spare = held.spare.take();
        let dropped = held.newest.replace(frame);
        // A frame that was overtaken is a buffer of exactly the right size.
        Some(
            dropped
                .map(|frame| frame.pixels)
                .or(spare)
                .unwrap_or_default(),
        )
    }

    /// Wait until `until`, or until the reel is given up, or until the shell
    /// starts asking for frames again. `false` once there is nothing more to do.
    ///
    /// The two waits a decoder does are the same wait: sleeping to the moment a
    /// frame is due, and parking because nothing is being drawn. Both end early
    /// if the file is changed underneath them, which is what makes a wallpaper
    /// change instant rather than up to a frame late.
    fn rest(&self, until: Instant) -> bool {
        let mut watch = self.watch.lock().unwrap_or_else(|held| held.into_inner());
        loop {
            if watch.stopped {
                return false;
            }
            let now = Instant::now();
            // Nobody is looking: park until somebody says otherwise. The
            // deadline is dropped rather than kept, because a film that came
            // back from behind a game should carry on from now and not race
            // through the minutes it was parked for.
            if now.duration_since(watch.wanted) > PATIENCE {
                watch = self
                    .wake
                    .wait(watch)
                    .unwrap_or_else(|held| held.into_inner());
                continue;
            }
            if now >= until {
                return true;
            }
            // Never longer than the patience: the parking above is only reached
            // by waking up to notice, and a frame due in a minute must not hold
            // a decoder awake in front of a game for a minute.
            let (guard, _) = self
                .wake
                .wait_timeout(watch, (until - now).min(PATIENCE))
                .unwrap_or_else(|held| held.into_inner());
            watch = guard;
        }
    }
}

/// Decode `path` into `reel` until there is nothing more to decode or nobody
/// wants it.
fn run(reel: &Reel, path: &Path) {
    let moving = matches!(crate::media::kind_of(path), Some(crate::media::Kind::Video));
    let made = if is_vector(path) {
        vector(path).map(|frame| {
            reel.deliver(frame);
        })
    } else {
        decode(reel, path, moving)
    };
    if made.is_none() {
        tracing::warn!(file = %path.display(), "that wallpaper could not be drawn");
        reel.trouble.store(true, Ordering::Relaxed);
    }
}

/// Whether this file is a drawing rather than a picture: no pixels of its own,
/// so it is rendered at the size it will be shown rather than decoded and
/// scaled. The same rule the thumbnailer applies to the same file.
fn is_vector(path: &Path) -> bool {
    path.extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|extension| extension.eq_ignore_ascii_case("svg"))
}

/// One frame out of a vector drawing, rendered at the height a wallpaper is
/// held at.
fn vector(path: &Path) -> Option<Frame> {
    let data = std::fs::read(path).ok()?;
    let side = STILL / 2;
    let pixels = crate::icons::rasterise_svg(&data, path.parent(), side)?;
    Some(Frame {
        width: side,
        height: side,
        pixels,
    })
}

/// The box a file of this kind is decoded into: the picture is scaled to fit
/// inside it, keeping its own shape, and never scaled *up*.
///
/// Never up because there is nothing to gain: the shader stretches whatever it
/// is given across the display anyway, and a small picture blown up on the CPU
/// first would cost memory and bandwidth to arrive at exactly the same blur.
fn fitted(width: u32, height: u32, box_edge: u32) -> (u32, u32) {
    let longest = width.max(height);
    if longest <= box_edge || longest == 0 {
        return (width.max(1), height.max(1));
    }
    let scale = box_edge as f64 / longest as f64;
    (
        ((width as f64 * scale).round() as u32).max(1),
        ((height as f64 * scale).round() as u32).max(1),
    )
}

/// How long one frame of a film is on screen, from the rate the stream declares.
///
/// A film with no honest rate — a picture, a stream that declares nothing — is
/// given a rate that is never used: a still delivers one frame and then finds
/// there is nothing left to decode.
fn pace(rate: ffmpeg::Rational) -> Duration {
    let (numerator, denominator) = (rate.numerator() as f64, rate.denominator() as f64);
    if numerator <= 0.0 || denominator <= 0.0 {
        return Duration::from_millis(40);
    }
    let seconds = denominator / numerator;
    // Clamped at both ends against a stream that declares something absurd:
    // a thousand frames a second would spin a core, and one frame a minute
    // would look like a wallpaper that had stopped.
    Duration::from_secs_f64(seconds.clamp(1.0 / 240.0, 1.0))
}

/// Open `path`, decode its video stream, and hand every frame to `reel`.
///
/// `None` if there is nothing here to draw. `Some(())` when the file was drawn
/// to the end — which for a still is after one frame, and for a film is when
/// somebody changed the setting.
fn decode(reel: &Reel, path: &Path, moving: bool) -> Option<()> {
    // Idempotent, and cheap after the first call: this is the one thread that
    // ever touches the library, but registering is a per-process matter and
    // saying so here keeps the whole of the decoder in one file.
    ffmpeg::init().ok()?;
    // And this shell speaks for itself. `libav` writes its own account of every
    // file it cannot make sense of straight to stderr, around `tracing` and in
    // its own spelling — three lines about signatures and `probesize` for the
    // one case below that this decoder already has an answer for, and they land
    // in the middle of a build's test output looking like something went wrong
    // when what happened is a file was correctly refused. Above `Fatal` is
    // silenced; `Fatal` and `Panic` still come through, because those are the
    // ones that end with the process gone rather than with a `None`.
    ffmpeg::log::set_level(ffmpeg::log::Level::Fatal);

    let mut input = ffmpeg::format::input(path)
        .map_err(|error| tracing::debug!(file = %path.display(), %error, "cannot be opened"))
        .ok()?;
    // Taken out of the stream in one go, because the stream borrows the input
    // and the input is about to be read from.
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
        .map_err(|error| tracing::debug!(file = %path.display(), %error, "nothing decodes this"))
        .ok()?;

    // What the decoder says it will produce, checked before anything is built
    // out of it — and this check is not defensive tidiness. `libswscale`
    // *aborts the process* when it is asked for a context whose input format has
    // no descriptor: `Assertion desc failed at libswscale/swscale_internal.h`,
    // and the shell is gone with it. A file whose parameters could not be worked
    // out arrives here exactly that way — format `none`, size zero — which is
    // what a `.png` that is not a PNG looks like from the demuxer's side, and
    // the picker will happily offer one of those because a name is all it has to
    // go on.
    let format = decoder.format();
    if format == ffmpeg::format::Pixel::None || decoder.width() == 0 || decoder.height() == 0 {
        tracing::debug!(
            file = %path.display(),
            width = decoder.width(),
            height = decoder.height(),
            "there is no picture in this to scale"
        );
        return None;
    }

    let (width, height) = fitted(
        decoder.width(),
        decoder.height(),
        if moving { FILM } else { STILL },
    );
    let mut scaler = ffmpeg::software::scaling::Context::get(
        format,
        decoder.width(),
        decoder.height(),
        // The one place the format is named, and it is the format of the
        // texture at the far end. `libswscale` does the conversion from
        // whatever the film is really in — which is nearly always some flavour
        // of planar YUV — in the same pass as the scaling.
        ffmpeg::format::Pixel::RGBA,
        width,
        height,
        ffmpeg::software::scaling::Flags::BILINEAR,
    )
    .ok()?;

    let mut drawn = false;
    let mut due = Instant::now();
    let mut scaled = ffmpeg::frame::Video::empty();
    let mut decoded = ffmpeg::frame::Video::empty();
    loop {
        let mut any = false;
        // `packets()` borrows the input, so the loop over it is where the
        // whole of a pass through the file happens; going round the outer loop
        // is what starts the film again.
        for (from, packet) in input.packets() {
            // The video stream and nothing else. This is where a film's
            // soundtrack is dropped: its packets are never handed to a decoder,
            // and no audio decoder was made to hand them to.
            if from.index() != index {
                continue;
            }
            if decoder.send_packet(&packet).is_err() {
                continue;
            }
            while decoder.receive_frame(&mut decoded).is_ok() {
                if scaler.run(&decoded, &mut scaled).is_err() {
                    continue;
                }
                let frame = copy_out(&scaled, width, height, reel)?;
                any = true;
                drawn = true;
                // The schedule is counted from where it was, not from now: a
                // frame is due one interval after the *last one was due*, so
                // decoding and scaling it comes out of that interval rather than
                // being added to it. Counting from now instead is a film that
                // plays at one over (interval + work) — visibly slow motion on
                // anything but a tiny picture, and the first thing this got
                // wrong.
                //
                // Unless the schedule has been left behind altogether: a film
                // whose frames genuinely cost more than they last, or one that
                // has just come back from being parked behind a game for ten
                // minutes. Then it starts again from now, rather than racing
                // through ten minutes of frames nobody is going to see.
                let now = Instant::now();
                if due + interval < now {
                    due = now;
                }
                due += interval;
                let spare = reel.deliver(frame);
                let mut held = reel.held.lock().unwrap_or_else(|held| held.into_inner());
                held.spare = spare;
                drop(held);
                if !reel.rest(due) {
                    return Some(());
                }
            }
        }
        if !drawn {
            return None;
        }
        if !moving || !any {
            // Nothing left to decode and nothing to loop: a picture. Stand
            // there showing it until the setting changes, which is what ends
            // the wait.
            while reel.rest(Instant::now() + PATIENCE) {
                if reel.stopped() {
                    break;
                }
            }
            return Some(());
        }
        // Round again. The decoder is flushed first because the frames it is
        // holding belong to the end of the film and the next packet it is given
        // belongs to the beginning.
        decoder.flush();
        if input.seek(0, ..).is_err() {
            return Some(());
        }
        due = Instant::now();
    }
}

/// Copy one scaled frame out of `libswscale`'s buffer into a plain `Vec`,
/// reusing whatever buffer the last frame left behind.
///
/// The copy is not avoidable: the frame the library fills is padded to a stride
/// of its own choosing, and a texture upload wants rows packed end to end.
fn copy_out(scaled: &ffmpeg::frame::Video, width: u32, height: u32, reel: &Reel) -> Option<Frame> {
    let stride = scaled.stride(0);
    let row = width as usize * 4;
    if stride < row {
        return None;
    }
    let source = scaled.data(0);
    let mut pixels = {
        let mut held = reel.held.lock().unwrap_or_else(|held| held.into_inner());
        held.spare.take().unwrap_or_default()
    };
    pixels.clear();
    pixels.reserve(row * height as usize);
    for line in 0..height as usize {
        let at = line * stride;
        pixels.extend_from_slice(source.get(at..at + row)?);
    }
    Some(Frame {
        width,
        height,
        pixels,
    })
}

/// Where the shell keeps its copy of the picture it was given.
///
/// `$XDG_DATA_HOME/linexinbar/wallpaper`, which is data rather than cache on
/// purpose: a cache is a thing that may be deleted because it can be made again,
/// and this one cannot — the file it was made from is somebody's own and may
/// have been on a stick, or thrown away since.
///
/// A directory holding exactly one file, rather than one file with a fixed name,
/// so that the copy can keep **the name the user knows it by**. That name is
/// what the Settings row says under Custom wallpaper — somebody coming back a
/// month later wants to see `Sunset over Ålesund.jpg` and not `wallpaper.jpg` —
/// and a fixed name would have thrown it away the moment the copy landed.
pub fn kept_in() -> Option<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|home| home.is_absolute())
                .map(|home| home.join(".local/share"))
        })?;
    Some(data.join("linexinbar").join("wallpaper"))
}

/// The name the kept copy takes: the source's own.
///
/// Its own and nothing else's — [`Path::file_name`] is one component, so what
/// comes back cannot hold a separator and cannot be a way out of the directory
/// the copy is written into. A path with no name at all falls back to
/// `wallpaper`.
///
/// Keeping the name matters, and so does keeping the extension on the end of it:
/// the name is what the Settings row shows the user, and the extension is what
/// this decoder and the listing that offered the file both read to tell a film
/// from a picture.
fn kept_name(source: &Path) -> std::ffi::OsString {
    match source.file_name() {
        Some(name) if !name.is_empty() && name != ".." => name.to_os_string(),
        _ => std::ffi::OsString::from("wallpaper"),
    }
}

/// Copy the chosen file into the shell's own directory, and answer with where
/// it landed.
///
/// The wallpaper is the one setting in this shell that points at something the
/// user can move, rename or delete without the shell being anywhere near it. A
/// path alone would mean a machine that comes up with no wallpaper because
/// somebody tidied their Pictures folder, or because the stick the photograph
/// was on is in a drawer. So the file itself is what is kept.
///
/// Written beside the destination and renamed on to it, which is what makes the
/// destination's existence mean "a whole file": a copy interrupted half way —
/// the disk filling, the session ending — leaves the part-written file behind
/// rather than a wallpaper with the bottom half missing.
///
/// The previous copy is taken away once the new one is in place, and only then:
/// the directory holds one wallpaper, two copies have different names whenever
/// the two files do, and the one thing this must not do is leave the shell
/// pointing at a file it has just deleted.
pub fn keep(source: &Path) -> std::io::Result<PathBuf> {
    let directory = kept_in().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "there is no home directory to keep a wallpaper in",
        )
    })?;
    std::fs::create_dir_all(&directory)?;
    let destination = directory.join(kept_name(source));
    // Choosing the kept copy itself — which is a folder the picker can walk
    // into like any other. Copying a file on to itself truncates it.
    if same_file(source, &destination) {
        return Ok(destination);
    }

    let partial = directory.join(".part");
    std::fs::copy(source, &partial).inspect_err(|_| {
        let _ = std::fs::remove_file(&partial);
    })?;
    std::fs::rename(&partial, &destination).inspect_err(|_| {
        let _ = std::fs::remove_file(&partial);
    })?;
    forget_other_copies(&directory, &destination);
    Ok(destination)
}

/// Whether two paths are the same file on the disk, rather than two names that
/// happen to look alike.
fn same_file(one: &Path, other: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let (Ok(one), Ok(other)) = (std::fs::metadata(one), std::fs::metadata(other)) else {
        return false;
    };
    one.dev() == other.dev() && one.ino() == other.ino()
}

/// Take away the copies left by earlier choices.
///
/// Everything in the directory but the file just written, and it is safe to be
/// that broad *because* of what the directory is: `linexinbar/wallpaper` holds
/// one wallpaper and nothing else has any business writing there. That is the
/// whole reason it is a directory of its own rather than a file beside whatever
/// else this shell may come to keep — a sweep of `linexinbar` itself would be a
/// routine that could delete something somebody else put there.
///
/// Files only. Anything else in there was not put there by this, and is left
/// where it is rather than removed recursively.
fn forget_other_copies(directory: &Path, keeping: &Path) {
    let Ok(listing) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in listing.flatten() {
        let path = entry.path();
        if path == keeping || !path.is_file() {
            continue;
        }
        let _ = std::fs::remove_file(&path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder to work in, taken away with the test.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let at = std::env::temp_dir().join(format!("lxb-paper-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&at);
            std::fs::create_dir_all(&at).expect("a scratch directory");
            Scratch(at)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The kept copy is the user's file under the user's own name for it: that
    /// name is what the Settings row shows them, and the extension on the end of
    /// it is what says whether this is a film.
    ///
    /// And it is one name. What must not come out of here is anything that could
    /// name a file outside the directory the copy is written into.
    #[test]
    fn a_kept_copy_keeps_the_name_it_was_given() {
        assert_eq!(
            kept_name(Path::new("/x/Sunset over Ålesund.JPG")),
            "Sunset over Ålesund.JPG"
        );
        assert_eq!(kept_name(Path::new("/x/reel.mp4")), "reel.mp4");
        assert_eq!(kept_name(Path::new("/x/no-extension")), "no-extension");
        for odd in ["/x/..", "/", "..", ""] {
            let name = kept_name(Path::new(odd));
            assert!(!name.is_empty(), "{odd:?}");
            let joined = Path::new("/keep/here").join(&name);
            assert_eq!(joined.parent(), Some(Path::new("/keep/here")), "{odd:?}");
        }
    }

    /// A picture is scaled down to the box and never up to it, and it keeps its
    /// own shape either way — the crop that makes it fill a display is the
    /// shader's, and it needs the picture's real proportions to do it.
    #[test]
    fn a_picture_is_fitted_into_the_box_without_being_stretched() {
        assert_eq!(fitted(1920, 1080, 3840), (1920, 1080));
        assert_eq!(fitted(7680, 4320, 3840), (3840, 2160));
        assert_eq!(fitted(2000, 8000, 3840), (960, 3840));
        assert_eq!(fitted(0, 0, 3840), (1, 1));
    }

    /// A rate nothing sensible can be made of must not spin the decoder or stop
    /// it: both ends are clamped.
    #[test]
    fn a_films_pace_comes_from_its_own_rate_within_reason() {
        assert_eq!(
            pace(ffmpeg::Rational::new(25, 1)),
            Duration::from_millis(40)
        );
        assert_eq!(pace(ffmpeg::Rational::new(0, 0)), Duration::from_millis(40));
        assert!(pace(ffmpeg::Rational::new(10_000, 1)) >= Duration::from_secs_f64(1.0 / 240.0));
        assert!(pace(ffmpeg::Rational::new(1, 600)) <= Duration::from_secs(1));
    }

    /// A file that is not what its name says is refused, and refusing it is the
    /// whole of what happens.
    ///
    /// This one is not ordinary defensiveness. `libswscale` **aborts the
    /// process** when it is asked for a context whose input format has no
    /// descriptor — `Assertion desc failed at libswscale/swscale_internal.h` —
    /// and a demuxer that could not work out what is in a file hands back
    /// exactly that: format `none`, size zero. The wallpaper picker offers a
    /// file on the strength of its name, so somebody's `.png` that is really a
    /// text file is a press away, and it took the whole shell down until the
    /// check in [`decode`] was put in front of it.
    ///
    /// If that check is ever removed this test does not fail. It aborts the
    /// entire test binary, which is the same thing happening to a smaller
    /// program.
    #[test]
    fn a_file_that_is_not_what_it_says_is_refused_rather_than_fatal() {
        let scratch = Scratch::new("broken");
        let file = scratch.0.join("broken.png");
        std::fs::write(&file, b"this is not a picture at all").expect("a file to point at");

        let reel = Reel::new();
        assert!(decode(&reel, &file, false).is_none());
        // And nothing was handed over to be drawn, which is what makes the shell
        // put the setting back rather than show a frame of nothing.
        assert!(reel
            .held
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .newest
            .is_none());
    }

    /// The whole point of keeping a copy: the wallpaper survives the file it was
    /// made from being thrown away.
    #[test]
    fn the_copy_survives_the_original() {
        let scratch = Scratch::new("keep");
        let source = scratch.0.join("holiday.png");
        std::fs::write(&source, b"not really a png").expect("a source file");
        std::env::set_var("XDG_DATA_HOME", scratch.0.join("data"));

        let kept = keep(&source).expect("a copy");
        assert_eq!(kept.file_name().unwrap(), "holiday.png");
        std::fs::remove_file(&source).expect("the original goes");
        assert_eq!(
            std::fs::read(&kept).expect("the copy stays"),
            b"not really a png"
        );

        // A second choice replaces the first rather than piling up beside it,
        // whatever it is called and whatever kind of file it is.
        let film = scratch.0.join("reel.mkv");
        std::fs::write(&film, b"not really a film").expect("a second source");
        let second = keep(&film).expect("a second copy");
        assert_eq!(second.file_name().unwrap(), "reel.mkv");
        assert!(!kept.exists(), "the first copy was left behind");

        // And choosing the copy itself does not truncate it, which is what
        // copying a file on to itself would do.
        let again = keep(&second).expect("the copy itself");
        assert_eq!(again, second);
        assert_eq!(
            std::fs::read(&again).expect("still there"),
            b"not really a film"
        );
    }
}
