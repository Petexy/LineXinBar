//! Steam's own pictures of a game: the cover on its row, the picture behind
//! the whole display while it is chosen, and the title as artwork for the
//! moment the game is starting.
//!
//! A library drawn as a column of identical Steam marks says only how many
//! games somebody owns. The cover says which one each row *is*, from across a
//! room, before the name has been read — which is the whole reason a console
//! shows covers and a file manager shows names. And the picture behind it is
//! what the lattice always does with the thing under the cursor: the
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
//! All three are asked at the path Steam publishes the picture at, which the
//! library carries for every game it lists and which cannot be worked out from
//! the game's id — see [`lxb_steam::art::Published`]. A picture asked for by
//! name alone is one that a recent game does not have anywhere.
//!
//! ## Nothing is fetched ahead of time
//!
//! The same rule as the pictures of the user's own files, and for a stronger
//! reason: a library is a thousand games, a hero is half a megabyte, and
//! filling a cache with all of it would be half a gigabyte of somebody's disk
//! and their whole line for a minute — to draw six rows. So the shell asks for
//! the covers around each cursor, and the hero and logo of the row actually
//! chosen, and [`crate::gpu`] drops whatever the cursor has left behind.
//!
//! The logo is asked for on the row rather than on the press, which looks like
//! an exception and is not: it is the same *row* as the hero, fetched at the
//! same moment and dropped at the same one. What decides it is that the thing
//! it is wanted for cannot wait — a launch splash is on screen a fifth of a
//! second after the button goes down.
//!
//! ## What a picture that never arrives does
//!
//! Nothing. The row keeps the Steam mark it had and the wallpaper stays the
//! shell's own, which is what both looked like before this module existed.
//! That is why [`Missing`] is answered in two halves: a game Steam has no
//! cover for is never asked about again, while a wire that was down is asked
//! again after [`AGAIN`] — one is a fact about the game, the other is a fact
//! about this minute.
//!
//! "Never again" holds only while the question is the same one. The shell lists
//! the games on the disk the moment it signs in and learns where their pictures
//! are a second later, so its first question about an installed game is asked
//! without knowing where to look — and an answer to *that* must not settle the
//! matter for the session. Each conclusion is kept with the path it was reached
//! from, and a game whose published path turns out to be another one is asked
//! once more.
//!
//! ## The one thing here that is not Steam's
//!
//! A game is not the only thing that can stand behind a display. A photograph
//! the user is standing on in Files puts itself there too, and that picture is
//! read off their own disk by [`crate::thumbs`] rather than fetched from
//! anybody. What the two share is the far end and only the far end: the box in
//! [`HERO_WIDTH`], the crop, the chain of halvings, and the layer of the
//! texture it all ends up in. So the box and [`scenery_from`] are declared
//! here, where the shape of a picture behind the bar is settled, and [`Sight`]
//! is what everything downstream holds instead of an app id.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use lxb_steam::art::{Cdn, Missing};

/// Which picture of a game is meant, and where Steam publishes it. The shell's
/// callers name pieces through this module rather than reaching past it into
/// the Steam crate: what a cover and a hero *are* is Valve's, but which of them
/// this shell has any use for is settled here.
pub use lxb_steam::art::{Piece, Published};

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

/// The box a game's logo is held in.
///
/// Valve's ceiling and not a choice of this shell's: every `logo.png` in the
/// client's cache is at most 640 across, and the ones that are not are
/// narrower rather than wider. Holding it at anything smaller would be
/// throwing away detail the splash then has to invent again — the logo is
/// drawn a third of a display wide there, which is most of the way back to
/// this number on a 1080p screen and past it on a 4K one.
///
/// A box, not a size. Nothing is stretched to it and nothing is padded out to
/// it: a wordmark set in one line arrives 640 × 113 and is kept that way.
pub const LOGO_SIZE: u32 = 640;

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

/// What the picture standing behind a display is *of*.
///
/// Two things put one there and they arrive by different routes — Steam's key
/// art for the game under the cursor, fetched by this module, and one of the
/// user's own photographs under the cursor in Files, decoded off their disk by
/// [`crate::thumbs`]. Past the moment it is made, nothing cares which: a layer
/// is a layer, a crossfade is a crossfade, and the renderer is handed the same
/// two numbers either way.
///
/// So this is the key everything downstream holds — which display is looking
/// at what, which layer is holding which picture, and what may be let go of.
/// An app id would have meant a second set of all three, and two crossfades on
/// one display that could each believe they owned the wallpaper.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Sight {
    /// A Steam title, by the id its pictures are published under.
    Game(u32),
    /// One of the user's own pictures, by the file it is in. The file rather
    /// than a name or an index: it is what the shell asked for the picture by,
    /// what it is handed back under, and what says two photographs called
    /// `IMG_0001.jpg` in two folders are not the same photograph.
    Picture(PathBuf),
}

impl Sight {
    /// The game this is about, if it is about one.
    pub fn game(&self) -> Option<u32> {
        match self {
            Sight::Game(app_id) => Some(*app_id),
            Sight::Picture(_) => None,
        }
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
    /// A logo, by app id rather than by the file it came out of — unlike a
    /// cover, which shares the atlas's thumbnail band with the user's own
    /// pictures and is therefore filed the way those are. Only one thing in
    /// the shell ever asks for a logo and it asks by game.
    Logo {
        app_id: u32,
        picture: Picture,
    },
}

/// One picture to make: which game's, which piece, and where Steam says it is.
///
/// The path travels with the request rather than being looked up by the worker,
/// because it is the library's to know: it arrives in the same PICS record as
/// the game's name, and a worker thread has no library to ask.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Job {
    app_id: u32,
    piece: Piece,
    /// Where Valve publishes it, when the catalogue said — see
    /// [`lxb_steam::art::Published`]. `None` for a game that is on the disk
    /// without being in the account's catalogue, which is asked for by name.
    published: Option<String>,
}

/// The workers, and what has been asked of them.
pub struct Art {
    queue: Arc<Queue>,
    done: Receiver<(u32, Piece, Result<Answer, Missing>)>,
    /// Asked for and not yet answered, so a row on screen for a hundred frames
    /// is asked for once. Against the path it was asked for at, which is what
    /// an answer of "there is none" has to be recorded against.
    asked: HashMap<(u32, Piece), Option<String>>,
    /// Steam has no such picture for these, at the path it was asked for.
    ///
    /// The path is half of the answer, and leaving it out is what made a game
    /// keep an empty row for a whole session: the shell lists the games on the
    /// disk the moment it signs in and learns where their pictures are a second
    /// later, so the first thing it asks about a freshly installed game is asked
    /// without knowing where to look. "Not there" was then remembered as a fact
    /// about the game rather than about that question, and the answer that
    /// arrived with the catalogue was never asked for.
    ///
    /// So a conclusion is kept with the question it answers. A game whose
    /// published path is not the one this was decided from is asked again — once
    /// — and a game asked the same way twice is not.
    barren: HashMap<(u32, Piece), Option<String>>,
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
    jobs: Mutex<VecDeque<Job>>,
    ready: Condvar,
}

/// What one worker got.
enum Answer {
    Cover { path: PathBuf, picture: Picture },
    Hero(Scenery),
    Logo(Picture),
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
            asked: HashMap::new(),
            barren: HashMap::new(),
            later: HashMap::new(),
            covers: HashMap::new(),
            real,
        }
    }

    /// Whether asking for this picture, at this path, could achieve anything.
    ///
    /// Its own answer rather than a run of early returns inside [`Self::want`]
    /// so that it can be checked without workers: everything this decides is
    /// about *not* reaching Steam, and a test that had to start two threads
    /// and a TLS session to see it would be a test that reaches Steam.
    fn worth_asking(&self, key: (u32, Piece), published: Option<&str>) -> bool {
        // A game Steam has no picture for is not asked again — unless what is
        // known about where to look has changed since it said so, which makes
        // this a different question with its own answer.
        !self
            .barren
            .get(&key)
            .is_some_and(|asked| asked.as_deref() == published)
            && !self.asked.contains_key(&key)
            && !self
                .later
                .get(&key)
                .is_some_and(|when| when.elapsed() < AGAIN)
    }

    /// Ask for one picture of one game, unless it is already being fetched,
    /// already known not to exist where it would be looked for, or was
    /// unreachable a moment ago.
    ///
    /// `where_they_are` is the game's own record of where Steam publishes its
    /// pictures, which the library carries. Without it the piece is asked for by
    /// its plain name, which is right for everything published before Valve
    /// began addressing artwork by its contents and finds nothing for anything
    /// published since — so the shell asks again if it learns better, and that
    /// is what the path recorded here is for.
    pub fn want(&mut self, app_id: u32, piece: Piece, where_they_are: Option<&Published>) {
        let key = (app_id, piece);
        let published = where_they_are
            .and_then(|published| published.of(piece))
            .map(str::to_owned);
        if !self.real || !self.worth_asking(key, published.as_deref()) {
            return;
        }
        self.later.remove(&key);
        // Whatever was concluded from asking a different way is no longer what
        // is known: this is now the question, and its answer replaces that one.
        self.barren.remove(&key);
        self.asked.insert(key, published.clone());
        let Ok(mut jobs) = self.queue.jobs.lock() else {
            return;
        };
        jobs.push_front(Job {
            app_id,
            piece,
            published,
        });
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
            // The path it was asked for at, which is what a "there is none"
            // has to be filed under to be worth anything later.
            let asked_for = self.asked.remove(&(app_id, piece)).unwrap_or_default();
            match answer {
                Ok(Answer::Cover { path, picture }) => {
                    self.covers.insert(app_id, path.clone());
                    out.push(Made::Cover { path, picture });
                }
                Ok(Answer::Hero(scenery)) => out.push(Made::Hero { app_id, scenery }),
                Ok(Answer::Logo(picture)) => out.push(Made::Logo { app_id, picture }),
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
                        tracing::debug!(
                            app_id,
                            ?piece,
                            asked_for,
                            "Steam has no such picture for this game"
                        );
                        self.barren.insert((app_id, piece), asked_for);
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
    ///
    /// "Never" as far as anything known now goes: a game whose published path
    /// arrives later is asked again, and this then answers the other way.
    pub fn hopeless(&self, app_id: u32, piece: Piece) -> bool {
        self.barren.contains_key(&(app_id, piece))
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
        let job = {
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

        let made = produce(&job, &cdn);
        if send.send((job.app_id, job.piece, made)).is_err() {
            return;
        }
    }
}

/// Find one picture and turn it into what the GPU takes.
fn produce(job: &Job, cdn: &Cdn) -> Result<Answer, Missing> {
    let piece = job.piece;
    let (path, bytes) = source(job, cdn)?;
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
        Piece::Logo => {
            let picture = logo(&bytes).ok_or_else(|| {
                Missing::Unreachable(format!("{} could not be decoded", path.display()))
            })?;
            Ok(Answer::Logo(picture))
        }
    }
}

/// The bytes of one picture, and the file they belong to.
///
/// Both caches are read before anything is asked of Steam, and what is fetched
/// is written into this shell's own so the next session does not ask again.
fn source(job: &Job, cdn: &Cdn) -> Result<(PathBuf, Vec<u8>), Missing> {
    let (app_id, piece, published) = (job.app_id, job.piece, job.published.as_deref());
    let ours = ours(app_id, piece, published)
        .ok_or_else(|| Missing::Unreachable("there is nowhere to cache pictures".to_string()))?;
    let already = lxb_steam::art::in_the_client_cache(app_id, piece, published)
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

    let bytes = cdn.fetch(app_id, piece, published)?;
    store(&ours, &bytes);
    Ok((ours, bytes))
}

/// Where this shell keeps what it had to fetch.
///
/// Under the path Steam publishes the picture at, which is how the client's own
/// cache is laid out and is worth copying for the reason Valve did it: that path
/// is named after the picture's contents, so a game whose artwork is replaced
/// asks for a file this cache has never held instead of showing last year's
/// cover for the rest of the machine's life. A game with no published path
/// keeps the plain name, which is where the last session left it.
fn ours(app_id: u32, piece: Piece, published: Option<&str>) -> Option<PathBuf> {
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
            .join(published.unwrap_or_else(|| piece.file_name())),
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

/// A logo, at its own shape inside [`LOGO_SIZE`].
///
/// Fitted, never filled and never padded: a wordmark is transparent almost
/// everywhere, and the whole of what makes it usable is that the drawing
/// reaches the edges of the picture it is in. Cropping one to a box would cut
/// the ends off the title, and centring one in a square would leave the splash
/// no way of knowing how much of what it is drawing is nothing at all.
///
/// Scaled down only. Nearly every logo Valve holds is already at the ceiling
/// or under it, and stretching a small one would be storing invented pixels in
/// an atlas block that is measured in megabytes.
fn logo(bytes: &[u8]) -> Option<Picture> {
    let image = decode(bytes)?;
    let scaled = if image.width() > LOGO_SIZE || image.height() > LOGO_SIZE {
        image.resize(LOGO_SIZE, LOGO_SIZE, image::imageops::FilterType::Lanczos3)
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
    Some(scenery_from(decode(bytes)?))
}

/// One decoded picture, made into the picture behind a display.
///
/// Split from [`hero`] so that a photograph of the user's own can become one
/// without coming through Steam: what a picture behind the bar *is* — the box,
/// the crop, the rungs — is settled here, and where the bytes came from is the
/// caller's business. [`crate::thumbs`] reads them off the disk with its own
/// ceiling on what a decode may cost, which is a different ceiling from the one
/// this module puts on a file arriving over the wire.
pub fn scenery_from(image: image::DynamicImage) -> Scenery {
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
    Scenery { levels }
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

    /// A logo keeps whatever shape it arrived in. Every other picture here is
    /// cropped or fitted into a box the shell chose; this one is a drawing of
    /// a title and its shape *is* the drawing — a wordmark squared off would
    /// be a title with air stamped onto one end of it, and the splash has no
    /// way of telling that air from the transparency the logo is mostly made
    /// of.
    #[test]
    fn a_logo_keeps_its_own_shape() {
        let drawn = |width, height| {
            let source = image::DynamicImage::new_rgba8(width, height);
            let mut bytes = Vec::new();
            source
                .write_to(
                    &mut std::io::Cursor::new(&mut bytes),
                    image::ImageFormat::Png,
                )
                .expect("a picture to decode");
            let made = logo(&bytes).expect("a logo");
            assert_eq!(made.rgba.len(), (made.width * made.height * 4) as usize);
            (made.width, made.height)
        };

        // A one-line wordmark, at Valve's own ceiling: kept exactly.
        assert_eq!(drawn(640, 113), (640, 113));
        // A stacked one, likewise.
        assert_eq!(drawn(640, 360), (640, 360));
        // Smaller than the box, so it stays smaller: blowing it up would store
        // invented pixels in a block measured in megabytes.
        assert_eq!(drawn(320, 180), (320, 180));
        // And one past the box comes down to it with its shape intact.
        assert_eq!(drawn(1280, 360), (LOGO_SIZE, 180));
    }

    /// An invented library asks nothing of Steam and nothing of the disk: its
    /// app ids belong to real games, and drawing their artwork would be a
    /// fixture pretending to be somebody's library.
    #[test]
    fn an_invented_library_is_never_fetched_for() {
        let mut art = Art::start(false);
        art.want(10, Piece::Cover, None);
        art.want(10, Piece::Hero, None);
        assert!(art.queue.jobs.lock().expect("the queue").is_empty());
        assert!(art.take().is_empty());
        assert_eq!(art.cover(10), None);
    }

    /// Where the picture is travels with the request, one piece's path per
    /// request. A worker handed the game's whole record would have to decide
    /// which of three paths this job meant, and a worker handed nothing would
    /// go looking for a file name Valve stopped publishing.
    #[test]
    fn a_request_carries_the_path_of_the_piece_it_is_for() {
        let published = Published::from(lxb_steam::art::LibraryArt {
            capsule: Some("28dbb244/library_600x900.jpg".to_string()),
            hero: Some("67a1c596/library_hero.jpg".to_string()),
            logo: None,
        });
        let mut art = Art::start(false);
        // Started with no workers, so nothing here reaches Steam; the queue is
        // the thing under test and it is filled the same way either way.
        art.real = true;
        art.want(3288210, Piece::Cover, Some(&published));
        art.want(3288210, Piece::Logo, Some(&published));
        art.want(440, Piece::Cover, None);

        let jobs = art.queue.jobs.lock().expect("the queue");
        assert_eq!(
            jobs.iter().cloned().collect::<Vec<_>>(),
            vec![
                // Most recently wanted at the front.
                Job {
                    app_id: 440,
                    piece: Piece::Cover,
                    published: None,
                },
                Job {
                    app_id: 3288210,
                    piece: Piece::Logo,
                    published: None,
                },
                Job {
                    app_id: 3288210,
                    piece: Piece::Cover,
                    published: Some("28dbb244/library_600x900.jpg".to_string()),
                },
            ]
        );
    }

    /// A picture that could not be reached is asked for again later, and one
    /// Steam says does not exist is never asked for again at all.
    #[test]
    fn only_a_picture_worth_asking_for_twice_is_asked_for_twice() {
        let key = (504230, Piece::Cover);
        let mut art = Art::start(false);
        assert!(art.worth_asking(key, None), "nothing is known about it yet");

        art.asked.insert(key, None);
        assert!(!art.worth_asking(key, None), "it is already being fetched");

        art.asked.clear();
        art.barren.insert(key, None);
        assert!(
            !art.worth_asking(key, None),
            "Steam has no cover for this game"
        );

        art.barren.clear();
        art.later.insert(key, Instant::now());
        assert!(
            !art.worth_asking(key, None),
            "the wire was down a moment ago"
        );

        art.later.insert(key, Instant::now() - AGAIN * 2);
        assert!(art.worth_asking(key, None), "that was a minute ago");
    }

    /// Learning where a picture is makes a game that had none worth asking
    /// about again.
    ///
    /// The shell lists the games on the disk as soon as it signs in and learns
    /// where their pictures are a moment later, so the first thing it asks about
    /// an installed game is asked without knowing where to look. A "there is
    /// none" kept as a fact about the game rather than about that question is a
    /// row that stays empty until the shell is restarted — and, since the same
    /// race runs every time it starts, one that stays empty after that too.
    #[test]
    fn a_game_is_asked_again_once_the_shell_knows_where_to_look() {
        let key = (3812600, Piece::Cover);
        let published = Published::from(lxb_steam::art::LibraryArt {
            capsule: Some("e5b5c644/library_capsule.jpg".to_string()),
            hero: None,
            logo: None,
        });
        let mut art = Art::start(false);
        art.real = true;

        // Asked before the catalogue arrived, with nothing to go on, and Steam
        // answered that there is no such picture by that name.
        art.want(key.0, key.1, None);
        art.asked.clear();
        art.barren.insert(key, None);
        assert!(
            !art.worth_asking(key, None),
            "the same question has the same answer"
        );

        // Then the catalogue arrives and says where the cover is.
        assert!(
            art.worth_asking(key, Some("e5b5c644/library_capsule.jpg")),
            "a different place to look is a different question"
        );
        art.want(key.0, key.1, Some(&published));
        assert_eq!(
            art.queue
                .jobs
                .lock()
                .expect("the queue")
                .front()
                .map(|job| job.published.clone()),
            Some(Some("e5b5c644/library_capsule.jpg".to_string()))
        );
        assert!(
            !art.barren.contains_key(&key),
            "what was concluded from not knowing is not still held"
        );
        assert!(!art.hopeless(key.0, key.1), "it is being asked about again");

        // And once the answer for that path is in, it is not asked a third time.
        art.asked.clear();
        art.barren
            .insert(key, Some("e5b5c644/library_capsule.jpg".to_string()));
        assert!(!art.worth_asking(key, Some("e5b5c644/library_capsule.jpg")));
    }
}
