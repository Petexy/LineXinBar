//! Steam's own pictures of a game: the cover on its row, and the picture
//! behind the whole display while it is chosen.
//!
//! A library drawn as a column of identical Steam marks says only how many
//! games somebody owns. The cover says which one each row *is*, from across a
//! room, before the name has been read — which is the whole reason a console
//! shows covers and a file manager shows names. And the picture behind it is
//! what the cross media bar always did with the thing under the cursor: the
//! screen becomes about the game you are looking at.
//!
//! ## Where the pictures come from, in order
//!
//! 1. **Valve's own cache.** A machine with the Steam client on it has already
//!    downloaded every picture its library screen has ever shown. Reading it
//!    costs a `stat` and a decode, works with no network at all, and is
//!    exactly the picture Steam would show.
//! 2. **This shell's cache**, `$XDG_CACHE_HOME/linexinbar/steam-art`, holding
//!    what had to be fetched. Kept as the bytes Steam sent rather than as
//!    decoded pixels, so it is small and so it survives a change to any size
//!    in this file.
//! 3. **Steam's content network**, once, for anything neither cache has.
//!
//! ## Nothing is fetched ahead of time
//!
//! The same rule as the pictures of the user's own files, and for a stronger
//! reason: a library is a thousand games, a hero is half a megabyte, and
//! filling a cache with all of it would be half a gigabyte of somebody's disk
//! and their whole line for a minute — to draw six rows. So the shell asks for
//! the covers around each cursor and the hero of the row actually chosen, and
//! [`crate::gpu`] drops whatever the cursor has left behind.
//!
//! ## What a picture that never arrives does
//!
//! Nothing. The row keeps the Steam mark it had and the wallpaper stays the
//! shell's own, which is what both looked like before this module existed.
//! That is why [`Missing`] is answered in two halves: a game Steam has no
//! cover for is never asked about again, while a wire that was down is asked
//! again after [`AGAIN`] — one is a fact about the game, the other is a fact
//! about this minute.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Which picture of a game is meant. The shell's callers name pieces through
/// this module rather than reaching past it into the Steam crate: what a
/// cover and a hero *are* is Valve's, but which of them this shell has any use
/// for is settled here.
pub use lxb_steam::art::Piece;
use lxb_steam::art::{Cdn, Missing};

use crate::thumbs::Picture;

/// The box the picture behind the bar is kept in, and the size Valve stores a
/// hero at.
///
/// Every layer of the array is exactly this, filled edge to edge: a picture is
/// scaled and cropped into it rather than fitted inside it, so no part of a
/// layer is ever left over from whatever was there before. That is not tidiness
/// — a blurred sample near the edge of a half-used layer would drag the
/// previous game's picture into this one's.
///
/// Cropping here costs nothing that the screen would not crop anyway: the box
/// is wider than any display, and the shader crops it further to whatever
/// shape the display is. Valve's own guidance is that a hero's subject must
/// survive being cut about at the sides, because that is what every surface
/// showing one does.
pub const HERO_WIDTH: u32 = 1920;
pub const HERO_HEIGHT: u32 = 620;

/// How many rungs of ever-smaller copies each hero carries.
///
/// The picture stands in for the wallpaper, and the wallpaper is asked for
/// softened — behind the guide, and inside every frosted pane in the shell.
/// The analytic wallpaper answers that by drawing itself dimmer and wider; a
/// photograph can only answer it by having smaller copies to sample. Five
/// rungs takes 1920 × 620 down to 120 × 38, which is well past the blur the
/// deepest frost asks for.
pub const HERO_LEVELS: u32 = 5;

/// How long a picture that could not be reached is left alone before it is
/// asked for again.
///
/// The shell asks for what is under the cursor once a frame. Without this, a
/// session with no network would spend the rest of its life opening sockets
/// for the same six games.
const AGAIN: Duration = Duration::from_secs(30);

/// How many requests may be waiting.
///
/// A stack rather than a queue, as the file thumbnailer's is: what matters is
/// the row under the cursor now, and a cover wanted eight scroll positions ago
/// is a row nobody is looking at.
const QUEUE: usize = 24;

/// How many pictures are worked on at once.
///
/// Two, because the cost is almost entirely waiting for Steam. A third would
/// not make a library fill in faster on any line; it would only put a third
/// decoder on the machine at the moment somebody is scrolling.
const WORKERS: usize = 2;

/// A picture behind the bar, as the renderer takes it: one layer of the hero
/// array, and its chain of halvings.
///
/// Level 0 is always [`HERO_WIDTH`] × [`HERO_HEIGHT`]; each after it is half
/// of the one above, rounded down, which is the size the GPU expects a mip
/// level to be.
pub struct Scenery {
    pub levels: Vec<Vec<u8>>,
}

impl Scenery {
    /// The size of one level, as the GPU counts them.
    pub fn size(level: u32) -> (u32, u32) {
        ((HERO_WIDTH >> level).max(1), (HERO_HEIGHT >> level).max(1))
    }
}

/// One finished picture.
pub enum Made {
    /// A cover, and the file it came out of — which is what the atlas keys it
    /// by, exactly as it keys a picture of one of the user's own files. No app
    /// id: by the time this is handed over, the path is what everything
    /// downstream holds the picture under.
    Cover {
        path: PathBuf,
        picture: Picture,
    },
    Hero {
        app_id: u32,
        scenery: Scenery,
    },
}

/// The workers, and what has been asked of them.
pub struct Art {
    queue: Arc<Queue>,
    done: Receiver<(u32, Piece, Result<Answer, Missing>)>,
    /// Asked for and not yet answered, so a row on screen for a hundred frames
    /// is asked for once.
    asked: HashSet<(u32, Piece)>,
    /// Steam has no such picture for these. A permanent answer.
    barren: HashSet<(u32, Piece)>,
    /// And these could not be reached, at that moment. See [`AGAIN`].
    later: HashMap<(u32, Piece), Instant>,
    /// Where each game's cover turned out to be, once one has been found. Held
    /// for the whole session because it is the key the atlas holds the picture
    /// under: without it the shell could neither find a resident cover nor say
    /// that it still wants it.
    covers: HashMap<u32, PathBuf>,
    /// Whether this session's library is a real one.
    ///
    /// False for `--debug-steam-library`, whose games are invented and whose
    /// app ids are small integers that belong to real titles — app 10 is
    /// Counter-Strike. A fixture that quietly drew Valve's artwork for
    /// somebody's made-up library would be a screenshot of something that does
    /// not exist, so an invented library is drawn with the marks it always had
    /// and nothing is asked of Steam or of the disk.
    real: bool,
}

/// What the workers take their work from.
struct Queue {
    /// Most recently wanted at the front.
    jobs: Mutex<VecDeque<(u32, Piece)>>,
    ready: Condvar,
}

/// What one worker got.
enum Answer {
    Cover { path: PathBuf, picture: Picture },
    Hero(Scenery),
}

impl Art {
    /// Start the workers, unless there is nothing real for them to fetch.
    pub fn start(real: bool) -> Art {
        let queue = Arc::new(Queue {
            jobs: Mutex::new(VecDeque::new()),
            ready: Condvar::new(),
        });
        let (send, done) = mpsc::channel();
        if real {
            for _ in 0..WORKERS {
                let queue = Arc::clone(&queue);
                let send = send.clone();
                std::thread::spawn(move || work(&queue, &send));
            }
        }
        Art {
            queue,
            done,
            asked: HashSet::new(),
            barren: HashSet::new(),
            later: HashMap::new(),
            covers: HashMap::new(),
            real,
        }
    }

    /// Whether asking for this picture now could achieve anything.
    ///
    /// Its own answer rather than a run of early returns inside [`Self::want`]
    /// so that it can be checked without workers: everything this decides is
    /// about *not* reaching Steam, and a test that had to start two threads
    /// and a TLS session to see it would be a test that reaches Steam.
    fn worth_asking(&self, key: (u32, Piece)) -> bool {
        !self.barren.contains(&key)
            && !self.asked.contains(&key)
            && !self
                .later
                .get(&key)
                .is_some_and(|when| when.elapsed() < AGAIN)
    }

    /// Ask for one picture of one game, unless it is already being fetched,
    /// already known not to exist, or was unreachable a moment ago.
    pub fn want(&mut self, app_id: u32, piece: Piece) {
        let key = (app_id, piece);
        if !self.real || !self.worth_asking(key) {
            return;
        }
        self.later.remove(&key);
        self.asked.insert(key);
        let Ok(mut jobs) = self.queue.jobs.lock() else {
            return;
        };
        jobs.push_front(key);
        jobs.truncate(QUEUE);
        drop(jobs);
        self.queue.ready.notify_one();
    }

    /// Everything finished since the last look.
    ///
    /// Failures are recorded here rather than handed back: there is nothing
    /// for the caller to do about a game with no cover, and asking again next
    /// frame is the one thing that must not happen.
    pub fn take(&mut self) -> Vec<Made> {
        let mut out = Vec::new();
        // The workers only stop if they cannot report, which they cannot do
        // while this end is held — so an empty channel and a dead one are the
        // same thing here: nothing more this frame.
        while let Ok((app_id, piece, answer)) = self.done.try_recv() {
            self.asked.remove(&(app_id, piece));
            match answer {
                Ok(Answer::Cover { path, picture }) => {
                    self.covers.insert(app_id, path.clone());
                    out.push(Made::Cover { path, picture });
                }
                Ok(Answer::Hero(scenery)) => out.push(Made::Hero { app_id, scenery }),
                Err(why) => {
                    if why.worth_retrying() {
                        tracing::debug!(
                            app_id,
                            ?piece,
                            ?why,
                            "that picture can be asked for again"
                        );
                        self.later.insert((app_id, piece), Instant::now());
                    } else {
                        tracing::debug!(app_id, ?piece, "Steam has no such picture for this game");
                        self.barren.insert((app_id, piece));
                    }
                }
            }
        }
        out
    }

    /// Whether Steam has already said it has no such picture for this game.
    ///
    /// Asked by the shell about the picture behind the bar, which is the one
    /// place where "not yet" and "never" have to be told apart: a picture
    /// still coming is worth holding the last one on screen for, and one that
    /// is never coming is a display that should go back to its wallpaper
    /// instead of showing the previous game for the rest of the session.
    pub fn hopeless(&self, app_id: u32, piece: Piece) -> bool {
        self.barren.contains(&(app_id, piece))
    }

    /// The file a game's cover is in, once one has been found.
    ///
    /// The atlas's key for it, so this is both how a row finds its picture and
    /// how the shell says it still wants it.
    pub fn cover(&self, app_id: u32) -> Option<&Path> {
        self.covers.get(&app_id).map(PathBuf::as_path)
    }
}

/// One worker: take the most recently wanted picture and make it.
///
/// The agent is the worker's own and outlives every job it does, so a library
/// being scrolled reuses one TLS session instead of opening one per cover.
fn work(queue: &Queue, send: &Sender<(u32, Piece, Result<Answer, Missing>)>) {
    let cdn = Cdn::new();
    loop {
        let (app_id, piece) = {
            let Ok(mut jobs) = queue.jobs.lock() else {
                return;
            };
            loop {
                if let Some(job) = jobs.pop_front() {
                    break job;
                }
                let Ok(waited) = queue.ready.wait(jobs) else {
                    return;
                };
                jobs = waited;
            }
        };

        let made = produce(app_id, piece, &cdn);
        if send.send((app_id, piece, made)).is_err() {
            return;
        }
    }
}

/// Find one picture and turn it into what the GPU takes.
fn produce(app_id: u32, piece: Piece, cdn: &Cdn) -> Result<Answer, Missing> {
    let (path, bytes) = source(app_id, piece, cdn)?;
    match piece {
        Piece::Cover => {
            let picture = cover(&bytes).ok_or_else(|| {
                Missing::Unreachable(format!("{} could not be decoded", path.display()))
            })?;
            Ok(Answer::Cover { path, picture })
        }
        Piece::Hero => {
            let scenery = hero(&bytes).ok_or_else(|| {
                Missing::Unreachable(format!("{} could not be decoded", path.display()))
            })?;
            Ok(Answer::Hero(scenery))
        }
    }
}

/// The bytes of one picture, and the file they belong to.
///
/// Both caches are read before anything is asked of Steam, and what is fetched
/// is written into this shell's own so the next session does not ask again.
fn source(app_id: u32, piece: Piece, cdn: &Cdn) -> Result<(PathBuf, Vec<u8>), Missing> {
    let ours = ours(app_id, piece)
        .ok_or_else(|| Missing::Unreachable("there is nowhere to cache pictures".to_string()))?;
    let already = lxb_steam::art::in_the_client_cache(app_id, piece)
        .into_iter()
        .chain(std::iter::once(ours.clone()));
    for path in already {
        match std::fs::read(&path) {
            Ok(bytes) if !bytes.is_empty() => return Ok((path, bytes)),
            // A file that is there and unreadable is worth a line; one that is
            // simply not there is the ordinary case and is not.
            Ok(_) => tracing::debug!(file = %path.display(), "an empty picture on the disk"),
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
                tracing::debug!(file = %path.display(), ?err, "cannot read that picture");
            }
            Err(_) => {}
        }
    }

    let bytes = cdn.fetch(app_id, piece)?;
    store(&ours, &bytes);
    Ok((ours, bytes))
}

/// Where this shell keeps what it had to fetch.
fn ours(app_id: u32, piece: Piece) -> Option<PathBuf> {
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|home| home.is_absolute())
                .map(|home| home.join(".cache"))
        })?;
    Some(
        cache
            .join("linexinbar")
            .join("steam-art")
            .join(app_id.to_string())
            .join(piece.file_name()),
    )
}

/// Write a fetched picture where the next session will find it.
///
/// Into place by rename, so another session reading the cache never sees half
/// a file at a name that says it is whole. A cache that cannot be written
/// costs one fetch next time and nothing else, so every failure here is a line
/// in the log and no more.
fn store(path: &Path, bytes: &[u8]) {
    let Some(dir) = path.parent() else {
        return;
    };
    if let Err(err) = std::fs::create_dir_all(dir) {
        tracing::debug!(?err, dir = %dir.display(), "cannot make the picture cache");
        return;
    }
    let part = path.with_extension("part");
    if let Err(err) = std::fs::write(&part, bytes) {
        tracing::debug!(?err, file = %part.display(), "cannot write that picture");
        return;
    }
    if let Err(err) = std::fs::rename(&part, path) {
        tracing::debug!(?err, file = %path.display(), "cannot put that picture in place");
        let _ = std::fs::remove_file(&part);
    }
}

/// A cover, scaled to the size the atlas holds a picture at.
///
/// The same size as a picture of one of the user's own files, because it goes
/// in the same band of the same atlas: a cover and a photograph are both a
/// picture on a card, and giving them separate machinery would be two of
/// everything for one behaviour.
fn cover(bytes: &[u8]) -> Option<Picture> {
    let image = decode(bytes)?;
    let edge = crate::thumbs::SIZE;
    // Scaled down, never up: a capsule smaller than the atlas cell is already
    // its own thumbnail, and stretching it would cost four times the atlas to
    // store the same detail, softened.
    let scaled = if image.width() > edge || image.height() > edge {
        image.resize(edge, edge, image::imageops::FilterType::Lanczos3)
    } else {
        image
    };
    let rgba = scaled.to_rgba8();
    Some(Picture {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
    })
}

/// A hero, cropped to fill the box and reduced to its chain of halvings.
fn hero(bytes: &[u8]) -> Option<Scenery> {
    let image = decode(bytes)?;
    // Fill and crop rather than fit: see [`HERO_WIDTH`]. `resize_to_fill`
    // keeps the middle, which is where Valve's guidance puts a hero's subject
    // precisely because everything that shows one crops it.
    let filled = image.resize_to_fill(
        HERO_WIDTH,
        HERO_HEIGHT,
        image::imageops::FilterType::Lanczos3,
    );
    let mut rung = filled.to_rgba8();
    let mut levels = vec![rung.as_raw().clone()];
    for level in 1..HERO_LEVELS {
        let (width, height) = Scenery::size(level);
        // Triangle, not Lanczos: this is a chain of halvings whose whole
        // purpose is to be blurry, and a sharpening filter on a rung nobody
        // looks at closely is work for nothing.
        rung = image::imageops::resize(&rung, width, height, image::imageops::FilterType::Triangle);
        levels.push(rung.as_raw().clone());
    }
    Some(Scenery { levels })
}

/// Decode a picture Steam sent, with a ceiling on what it may cost.
///
/// The ceiling is the point: this is a file from the network, and a decoder
/// asked to allocate whatever a header claims is the largest allocation the
/// shell would ever make, on a worker thread, on somebody else's say-so.
fn decode(bytes: &[u8]) -> Option<image::DynamicImage> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    match reader.decode() {
        Ok(image) => Some(image),
        Err(err) => {
            tracing::debug!(?err, "that picture could not be decoded");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every rung is the size the GPU expects a mip level to be, and the chain
    /// ends well before it runs out of pixels.
    #[test]
    fn the_halvings_are_the_sizes_a_mip_chain_has() {
        assert_eq!(Scenery::size(0), (HERO_WIDTH, HERO_HEIGHT));
        assert_eq!(Scenery::size(1), (960, 310));
        let (width, height) = Scenery::size(HERO_LEVELS - 1);
        assert!(width > 1 && height > 1, "{width} × {height}");
    }

    /// A hero fills its layer edge to edge whatever shape it arrived in. Half
    /// a layer would blur the last game's picture into this one's at the seam.
    #[test]
    fn a_picture_of_any_shape_fills_the_whole_box() {
        for (width, height) in [(1920, 620), (600, 900), (64, 64)] {
            let source = image::DynamicImage::new_rgba8(width, height);
            let mut bytes = Vec::new();
            source
                .write_to(
                    &mut std::io::Cursor::new(&mut bytes),
                    image::ImageFormat::Png,
                )
                .expect("a picture to decode");
            let made = hero(&bytes).expect("a hero");
            assert_eq!(made.levels.len(), HERO_LEVELS as usize);
            for (level, pixels) in made.levels.iter().enumerate() {
                let (width, height) = Scenery::size(level as u32);
                assert_eq!(
                    pixels.len(),
                    (width * height * 4) as usize,
                    "rung {level} of a {width} × {height} picture"
                );
            }
        }
    }

    /// A cover keeps its shape and is never blown up past the atlas cell.
    #[test]
    fn a_cover_is_scaled_down_and_keeps_its_shape() {
        let source = image::DynamicImage::new_rgba8(600, 900);
        let mut bytes = Vec::new();
        source
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .expect("a picture to decode");
        let made = cover(&bytes).expect("a cover");
        assert_eq!(made.height, crate::thumbs::SIZE);
        assert_eq!(
            made.width,
            (crate::thumbs::SIZE as f32 * 600.0 / 900.0).round() as u32
        );
        assert_eq!(made.rgba.len(), (made.width * made.height * 4) as usize);
    }

    /// An invented library asks nothing of Steam and nothing of the disk: its
    /// app ids belong to real games, and drawing their artwork would be a
    /// fixture pretending to be somebody's library.
    #[test]
    fn an_invented_library_is_never_fetched_for() {
        let mut art = Art::start(false);
        art.want(10, Piece::Cover);
        art.want(10, Piece::Hero);
        assert!(art.queue.jobs.lock().expect("the queue").is_empty());
        assert!(art.take().is_empty());
        assert_eq!(art.cover(10), None);
    }

    /// A picture that could not be reached is asked for again later, and one
    /// Steam says does not exist is never asked for again at all.
    #[test]
    fn only_a_picture_worth_asking_for_twice_is_asked_for_twice() {
        let key = (504230, Piece::Cover);
        let mut art = Art::start(false);
        assert!(art.worth_asking(key), "nothing is known about it yet");

        art.asked.insert(key);
        assert!(!art.worth_asking(key), "it is already being fetched");

        art.asked.clear();
        art.barren.insert(key);
        assert!(!art.worth_asking(key), "Steam has no cover for this game");

        art.barren.clear();
        art.later.insert(key, Instant::now());
        assert!(!art.worth_asking(key), "the wire was down a moment ago");

        art.later.insert(key, Instant::now() - AGAIN * 2);
        assert!(art.worth_asking(key), "that was a minute ago");
    }
}
