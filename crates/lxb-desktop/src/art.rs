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
//! 2. **This shell's cache**, `$XDG_CACHE_HOME/lxb/steam-art`, holding
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
    /// A screenshot of one of somebody's own games, off libretro's collection —
    /// by the file it was fetched into, exactly as a photograph is.
    ///
    /// Its own kind rather than a [`Sight::Picture`] with a different path, and
    /// the difference is the whole reason it exists: this one is **blurred**.
    /// The pictures libretro holds are screenshots of consoles, which is to say
    /// they are three hundred pixels tall, and a three-hundred-pixel picture
    /// stretched across a television is a wall of squares. Softened it is what
    /// it was always going to be — the colour and the shape of the game, behind
    /// the row that is the game. See [`blurred_scenery_from`].
    Snapshot(PathBuf),
}

impl Sight {
    /// The game this is about, if it is about one.
    pub fn game(&self) -> Option<u32> {
        match self {
            Sight::Game(app_id) => Some(*app_id),
            Sight::Picture(_) | Sight::Snapshot(_) => None,
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
    /// A game's own square icon, filed the way a cover is: by the file it came
    /// out of, in the atlas's thumbnail band.
    ///
    /// Like a cover and unlike a logo, because it is the same kind of thing as
    /// a cover — a small picture of a game, at the size a row's picture is kept
    /// at — and putting it in a band of its own would be a third arithmetic in
    /// the atlas's layout for a picture that fits exactly in the one that is
    /// already there.
    Icon {
        path: PathBuf,
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
    /// And where each game's own icon turned out to be, on exactly the terms
    /// above: it is the atlas's key, so it is both how the card behind the
    /// guide finds the picture and how the shell says it still wants it.
    ///
    /// Beside the covers rather than in with them, because they are two
    /// pictures of one game and a single map could hold only one of them.
    icons: HashMap<u32, PathBuf>,
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
    Icon { path: PathBuf, picture: Picture },
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
            icons: HashMap::new(),
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
                Ok(Answer::Icon { path, picture }) => {
                    self.icons.insert(app_id, path.clone());
                    out.push(Made::Icon { path, picture });
                }
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

    /// The file a game's own icon is in, once one has been found. The atlas's
    /// key for it, exactly as [`Self::cover`] is for the cover.
    pub fn icon(&self, app_id: u32) -> Option<&Path> {
        self.icons.get(&app_id).map(PathBuf::as_path)
    }
}

/// The two directories Valve's own client keeps pictures in.
///
/// Two, because the icon is not in with the store artwork: the capsules, heroes
/// and logos are under `appcache/librarycache` and the icons are flat in
/// `steam/games`. Resolved together and held together so the worker asks which
/// Steam is being driven once rather than once per picture — see
/// [`lxb_steam::backend::Backend::chosen`], which is a `PATH` walk.
struct Caches {
    pictures: PathBuf,
    icons: PathBuf,
}

impl Caches {
    /// Where the client has already put this job's picture, if it has.
    fn holding(&self, job: &Job) -> Option<PathBuf> {
        match job.piece {
            Piece::Icon => lxb_steam::art::in_the_icon_cache(&self.icons, job.published.as_deref()),
            piece => lxb_steam::art::in_the_client_cache(
                &self.pictures,
                job.app_id,
                piece,
                job.published.as_deref(),
            ),
        }
    }
}

/// One worker: take the most recently wanted picture and make it.
///
/// The agent is the worker's own and outlives every job it does, so a library
/// being scrolled reuses one TLS session instead of opening one per cover.
fn work(queue: &Queue, send: &Sender<(u32, Piece, Result<Answer, Missing>)>) {
    let cdn = Cdn::new();
    // Which Steam's cache to read, resolved once with the agent and for the
    // same reason: it is a `PATH` walk and a handful of directory checks, and
    // asking it again per picture would be asking it a thousand times while a
    // library is scrolled. `None` is a machine with no Steam of any kind, where
    // every picture comes from the network.
    //
    // Once, not never: a Steam installed halfway through a session is not
    // noticed until the shell restarts, and the cost of that is a fetch rather
    // than a blank row. Reading the *wrong* Steam's cache is the failure that
    // matters, and that is what resolving it at all fixes — this used to go
    // looking on its own and prefer a native directory an uninstall had left
    // behind, while the Flatpak beside it was the one being driven.
    let cache = lxb_steam::backend::Backend::chosen().map(|steam| Caches {
        pictures: steam.client_art_cache(),
        icons: steam.client_icon_cache(),
    });
    if let Some(cache) = cache.as_ref() {
        tracing::info!(
            pictures = %cache.pictures.display(),
            icons = %cache.icons.display(),
            "reading Steam's own picture cache"
        );
    }
    // And this shell's own, weighed rather than read. On the worker's thread
    // because it is the thread that fills it, and here at the top because a
    // session that starts over the cap should not have to be scrolled before
    // anything is done about it. See [`tidy_the_cache`].
    tidy_the_cache();
    let mut tidied = Instant::now();
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

        let made = produce(&job, &cdn, cache.as_ref());
        if send.send((job.app_id, job.piece, made)).is_err() {
            return;
        }
        // After the answer has gone, never before it: this is a directory walk
        // and the shell is waiting on the picture, not on the tidying.
        if tidied.elapsed() >= TIDY_EVERY {
            tidied = Instant::now();
            tidy_the_cache();
        }
    }
}

/// Find one picture and turn it into what the GPU takes.
///
/// Every source in turn, rather than the first one that has bytes in it. A file
/// that is present and will not decode used to end the whole search: it was
/// returned as *the* source, decoding failed, and the failure was retryable —
/// so the next attempt read the same broken file, and so did every attempt
/// after that. One truncated write left a game without a cover for the life of
/// the machine, and nothing on screen or in the log said why.
fn produce(job: &Job, cdn: &Cdn, steam_cache: Option<&Caches>) -> Result<Answer, Missing> {
    let ours = ours(job.app_id, job.piece, job.published.as_deref()).ok_or_else(|| {
        Missing::Unreachable(
            crate::i18n::text("label-there-is-nowhere-to-cache-pictures").to_string(),
        )
    })?;

    // The cached copies, nearest first: Valve's own, then this shell's.
    let cached: Vec<PathBuf> = steam_cache
        .and_then(|cache| cache.holding(job))
        .into_iter()
        .chain(std::iter::once(ours.clone()))
        .collect();
    if let Some(answer) = off_the_disk(job.piece, &cached, &ours) {
        return Ok(answer);
    }

    // Nothing on the disk, or nothing on the disk that decoded. Either way the
    // network is what is left, and this is the attempt a broken file used to
    // stand in front of.
    let bytes = cdn.fetch(job.app_id, job.piece, job.published.as_deref())?;
    let answer = turn_into_a_picture(job.piece, ours.clone(), &bytes)?;
    // Written down only once it is known to be a picture. A response that
    // decodes nowhere is not worth caching, and caching one is how the
    // unreadable file gets there in the first place.
    store(&ours, &bytes);
    Ok(answer)
}

/// The first cached copy that is really a picture, throwing away any of this
/// shell's own that are not.
///
/// The throwing away is the point. A cached file that will not decode is not a
/// game without artwork, it is a game whose artwork this shell cannot see and
/// will never look for again: the file was returned as the source, decoding
/// failed as something worth retrying, and the retry read the same file. One
/// truncated write was permanent.
///
/// Only files of ours are deleted. The client's cache belongs to Valve and this
/// shell does not delete out of it — it steps over the file instead, which
/// costs one fetch and leaves somebody else's directory alone.
fn off_the_disk(piece: Piece, cached: &[PathBuf], ours: &Path) -> Option<Answer> {
    for path in cached {
        // A copy that is not there is the ordinary case and not a reason to
        // stop looking: most games have no cached artwork at all until this has
        // fetched some.
        let Some(bytes) = readable(path) else {
            continue;
        };
        match turn_into_a_picture(piece, path.clone(), &bytes) {
            Ok(answer) => {
                // Only ours: touching a file in Valve's cache would be writing
                // into somebody else's directory, and it is not the one this
                // shell's cap is about.
                if path == ours {
                    still_wanted(path);
                }
                return Some(answer);
            }
            Err(_) if path == ours => {
                tracing::info!(
                    file = %path.display(),
                    "throwing away a cached picture that will not decode"
                );
                let _ = std::fs::remove_file(path);
            }
            Err(_) => tracing::debug!(
                file = %path.display(),
                "stepping over a cached picture that will not decode"
            ),
        }
    }
    None
}

/// One file's bytes, or nothing, with a line for the failures worth one.
fn readable(path: &Path) -> Option<Vec<u8>> {
    match std::fs::read(path) {
        Ok(bytes) if !bytes.is_empty() => Some(bytes),
        // A file that is there and unreadable is worth a line; one that is
        // simply not there is the ordinary case and is not.
        Ok(_) => {
            tracing::debug!(file = %path.display(), "an empty picture on the disk");
            None
        }
        Err(err) => {
            if err.kind() != std::io::ErrorKind::NotFound {
                tracing::debug!(file = %path.display(), ?err, "cannot read that picture");
            }
            None
        }
    }
}

/// Decode one file into whatever the piece it is asks for.
fn turn_into_a_picture(piece: Piece, path: PathBuf, bytes: &[u8]) -> Result<Answer, Missing> {
    let undecodable = || {
        Missing::Unreachable(
            crate::message!("picture-could-not-be-decoded", "file" => path.display().to_string()),
        )
    };
    match piece {
        Piece::Cover => {
            let picture = cover(bytes).ok_or_else(undecodable)?;
            Ok(Answer::Cover { path, picture })
        }
        Piece::Hero => Ok(Answer::Hero(hero(bytes).ok_or_else(undecodable)?)),
        Piece::Logo => Ok(Answer::Logo(logo(bytes).ok_or_else(undecodable)?)),
        Piece::Icon => {
            let picture = icon(bytes).ok_or_else(undecodable)?;
            Ok(Answer::Icon { path, picture })
        }
    }
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
    Some(
        our_cache()?
            .join(app_id.to_string())
            .join(published.unwrap_or_else(|| piece.file_name())),
    )
}

/// `$XDG_CACHE_HOME/lxb/steam-art`, the whole of what this shell keeps.
///
/// Its own function because three things want the directory rather than a file
/// in it: writing one, measuring the lot, and throwing the lot away.
pub fn our_cache() -> Option<PathBuf> {
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|home| home.is_absolute())
                .map(|home| home.join(".cache"))
        })?;
    Some(cache.join("lxb").join("steam-art"))
}

/// How much of the disk this shell's picture cache may take.
///
/// A quarter of a gigabyte, which on a library of a few hundred games is
/// several times more than the pictures for all of them — so on an ordinary
/// machine nothing is ever thrown away, and this is the ceiling rather than the
/// working size.
///
/// What it actually guards against is not a large library but a long-lived
/// machine. A picture is filed under the path Steam publishes it at, and that
/// path is named after the picture's contents: a publisher who replaces a
/// game's cover leaves the old file behind under a name nothing will ask for
/// again. Nothing ever deleted those, so the only cache that grew without end
/// was the one belonging to somebody who had kept the same machine for years —
/// which is exactly whose disk this shell has no business filling.
const CACHE_CAP: u64 = 256 * 1024 * 1024;

/// How far under the cap a tidy-up goes.
///
/// Not to the cap, or a cache sitting on it would be tidied on every pass and
/// throw away one picture each time — which is a walk of the whole directory
/// for the sake of a file that is about to be fetched again. Four fifths leaves
/// room to fill before it matters again.
const TIDY_TO: u64 = CACHE_CAP / 5 * 4;

/// How often the cache is looked at.
///
/// It is a directory walk of a few hundred entries, which is milliseconds — but
/// it is milliseconds on the worker's thread, and the worker's thread is what a
/// library being scrolled is waiting on. Once when the session starts and every
/// ten minutes after is far more often than a cache can go from empty to a
/// quarter of a gigabyte.
const TIDY_EVERY: Duration = Duration::from_secs(600);

/// One cached file, as the tidying weighs it.
struct Kept {
    path: PathBuf,
    bytes: u64,
    /// When it was last written or last *used* — see [`still_wanted`], which is
    /// what makes this a record of what somebody looks at rather than of what
    /// happened to be fetched first.
    when: std::time::SystemTime,
}

/// Everything in this shell's picture cache, with what each takes.
fn kept_in(cache: &Path) -> Vec<Kept> {
    let mut kept = Vec::new();
    // Two levels and no deeper: a game's directory, and the pictures in it.
    let Ok(games) = std::fs::read_dir(cache) else {
        return kept;
    };
    for game in games.flatten() {
        let Ok(pictures) = std::fs::read_dir(game.path()) else {
            continue;
        };
        for picture in pictures.flatten() {
            let Ok(about) = picture.metadata() else {
                continue;
            };
            if !about.is_file() {
                continue;
            }
            kept.push(Kept {
                path: picture.path(),
                bytes: about.len(),
                when: about.modified().unwrap_or(std::time::UNIX_EPOCH),
            });
        }
    }
    kept
}

/// What the cache takes, and how many files it is in.
///
/// For the diagnostics panel, which is the one place a number about somebody's
/// disk is worth printing. `None` where there is nowhere to cache pictures at
/// all, which is a session with no home.
pub fn cache_room() -> Option<(u64, usize)> {
    let cache = our_cache()?;
    let kept = kept_in(&cache);
    Some((kept.iter().map(|one| one.bytes).sum(), kept.len()))
}

/// Throw the whole of it away, and say how much that was.
///
/// Only this shell's own directory. Valve's cache is not ours to delete out of
/// — see [`off_the_disk`], which steps over a broken file in it rather than
/// removing one — and nothing here goes near it.
///
/// Nothing is lost that cannot be fetched again, which is the whole of why this
/// is a button somebody may press: what it costs is one download per picture
/// the bar asks for next.
pub fn forget_the_cache() -> u64 {
    let Some(cache) = our_cache() else {
        return 0;
    };
    let freed: u64 = kept_in(&cache).iter().map(|one| one.bytes).sum();
    match std::fs::remove_dir_all(&cache) {
        Ok(()) => tracing::info!(freed, cache = %cache.display(), "threw the picture cache away"),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return 0,
        Err(err) => {
            tracing::warn!(?err, cache = %cache.display(), "could not throw the picture cache away");
            return 0;
        }
    }
    freed
}

/// Drop the oldest pictures until the cache is back under its cap.
///
/// Oldest by when it was last *used* rather than by when it was fetched: a
/// picture the bar draws every session is touched every session — see
/// [`still_wanted`] — and one belonging to a game whose artwork was replaced
/// two years ago is not touched at all. So what goes is what nothing has asked
/// for, which is the only ordering that means anything here.
///
/// Silent when there is nothing to do, which is every machine that has not been
/// running for years: the walk finds a cache well under the cap and returns.
fn tidy_the_cache() {
    let Some(cache) = our_cache() else {
        return;
    };
    let mut kept = kept_in(&cache);
    let mut total: u64 = kept.iter().map(|one| one.bytes).sum();
    if total <= CACHE_CAP {
        return;
    }
    kept.sort_by_key(|one| one.when);
    let was = total;
    let mut dropped = 0usize;
    for one in &kept {
        if total <= TIDY_TO {
            break;
        }
        if std::fs::remove_file(&one.path).is_ok() {
            total -= one.bytes.min(total);
            dropped += 1;
        }
    }
    tracing::info!(
        was,
        now = total,
        dropped,
        "the picture cache was over its cap"
    );
}

/// Say that a cached picture of ours was used just now.
///
/// The whole of what makes [`tidy_the_cache`] a least-recently-*used* rule
/// rather than a least-recently-fetched one. Without it the pictures thrown
/// away on a full cache would be the ones fetched longest ago, which on a
/// library somebody has had for years is their favourite games.
///
/// Best effort and quiet: a cache on a read-only filesystem, or a file that
/// went between being read and being touched, is not worth a line — the picture
/// was already handed over, which is what the caller asked for.
fn still_wanted(path: &Path) {
    let now = std::time::SystemTime::now();
    let times = std::fs::FileTimes::new()
        .set_accessed(now)
        .set_modified(now);
    let _ = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .and_then(|file| file.set_times(times));
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
pub fn logo(bytes: &[u8]) -> Option<Picture> {
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

/// A game's own icon, square, in the band a cover lives in.
///
/// Two things separate this from [`cover`] above, and both are about what a
/// `.ico` is.
///
/// It is **a directory of sizes rather than a picture**: one file holds 16, 32,
/// 48, 128 and 256-pixel drawings of the same mark, and `image`'s decoder
/// resolves that to the largest of them. That is the one this wants — the card
/// draws the icon at a fifth of the guide's height, which on a 4K display is
/// well past 128.
///
/// And **it is often smaller than the cell**. Measured on the machine this was
/// written against: of 33 client icons in Valve's own cache, 20 reach 256 px,
/// two reach 128, two 48 and eight stop at 32. So the scale is a reduction and
/// never an enlargement, exactly as a cover's is — a 32-pixel icon stretched
/// into a 256-pixel block would be four times the atlas for the same detail,
/// softened, and the card would rather draw the small picture small.
fn icon(bytes: &[u8]) -> Option<Picture> {
    let image = crate::icons::largest_in_an_ico(bytes).or_else(|| decode(bytes))?;
    let edge = crate::thumbs::SIZE;
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

/// The same, for a picture that is far too small for the place it is going.
///
/// A screenshot of a console is three hundred pixels tall. Filling a television
/// with one is a four-fold enlargement at best, and what an enlargement of a
/// small picture looks like is squares — every one of them a pixel of a machine
/// that had a hundred and fifty thousand of them, blown up to the size of a
/// thumbnail. Nobody wants to look at that behind the row they are choosing.
///
/// So it is deliberately not sharpened: it is reduced until there is nothing
/// left of the grid, softened once more to take the last of the steps out of
/// what remains, and then enlarged. What comes out is the colour and the
/// massing of the game — light where the sky was, dark where the cave was —
/// which is exactly as much as a picture standing behind a bar at a quarter
/// brightness was ever going to say.
///
/// The reduction is what makes it cheap as well as what makes it right: the
/// blur happens at [`BLURRED`] of the box's width, which is a few thousand
/// pixels rather than a million.
pub fn blurred_scenery_from(image: image::DynamicImage) -> Scenery {
    // Cropped to the box's shape *before* the reduction, so the crop is the
    // same one every other picture behind the bar gets and the enlargement at
    // the end is a plain scale with nothing else happening in it.
    let (width, height) = (HERO_WIDTH / BLURRED, HERO_HEIGHT / BLURRED);
    let small = image.resize_to_fill(width, height, image::imageops::FilterType::Triangle);
    // Gaussian on a picture this size is thousands of pixels, not millions.
    // What it is for is the last of the steps between one reduced pixel and the
    // next, which a box filter leaves behind and an enlargement would then
    // spread out into visible bands.
    let softened = small.blur(BLUR);
    scenery_from(softened)
}

/// How far a screenshot is reduced before it is softened and enlarged again.
///
/// An eighth of the box, which is 240 × 77. Chosen by looking at the result
/// rather than by argument: a sixteenth loses the composition — a forest and
/// two fighters become a green wash — and a quarter keeps enough of the grid
/// that the softening has to be strong enough to smear it, which costs the
/// same composition by the other road. An eighth is smaller than the picture
/// went in for every console libretro holds screenshots of, so the reduction
/// really is averaging pixels together rather than inventing them.
const BLURRED: u32 = 8;

/// How much softening the reduced copy is given, in pixels of it.
///
/// Enough that no two neighbouring averages can be told apart as squares once
/// the copy is enlarged eightfold, and not so much that the massing goes with
/// them.
const BLUR: f32 = 2.4;

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

    /// A cache laid out the way this shell lays one out.
    fn a_cache(name: &str, files: &[(&str, &str, usize)]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("lxb-art-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (app_id, picture, bytes) in files {
            let dir = root.join(app_id);
            std::fs::create_dir_all(&dir).expect("a scratch cache");
            std::fs::write(dir.join(picture), vec![0u8; *bytes]).unwrap();
        }
        root
    }

    /// What the cache weighs, which is what the diagnostics panel prints and
    /// what the cap is measured against.
    #[test]
    fn the_cache_says_what_it_takes() {
        let cache = a_cache(
            "room",
            &[
                ("504230", "cover.jpg", 1000),
                ("504230", "hero.jpg", 2000),
                ("220200", "cover.jpg", 500),
            ],
        );
        let kept = kept_in(&cache);
        assert_eq!(kept.len(), 3);
        assert_eq!(kept.iter().map(|one| one.bytes).sum::<u64>(), 3500);

        // A directory that is not there is an empty cache and not a failure:
        // it is every machine that has not fetched a picture yet.
        let _ = std::fs::remove_dir_all(&cache);
        assert!(kept_in(&cache).is_empty());
    }

    /// What a full cache throws away is what nothing has asked for, and what it
    /// keeps is what somebody looks at.
    ///
    /// The ordering is the whole of it. Oldest-fetched would take the pictures
    /// for the games somebody has had longest, which on a library kept for
    /// years is their favourite ones; what should go is the file left behind
    /// when a publisher replaced a game's cover, which nothing has asked for
    /// since and nothing ever will.
    #[test]
    fn a_full_cache_drops_what_nothing_has_asked_for() {
        let cache = a_cache(
            "tidy",
            &[
                ("1", "old.jpg", 10),
                ("2", "older.jpg", 10),
                ("3", "used.jpg", 10),
            ],
        );
        let old = cache.join("1").join("old.jpg");
        let older = cache.join("2").join("older.jpg");
        let used = cache.join("3").join("used.jpg");

        // Two of them written long ago, and one used a moment ago — which is
        // what `still_wanted` does to a file the bar has just drawn.
        let long_ago = std::time::SystemTime::now() - Duration::from_secs(60 * 60 * 24 * 400);
        for (path, when) in [
            (&old, long_ago),
            (&older, long_ago - Duration::from_secs(60)),
        ] {
            let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
            file.set_times(std::fs::FileTimes::new().set_modified(when))
                .unwrap();
        }
        still_wanted(&used);

        let mut kept = kept_in(&cache);
        kept.sort_by_key(|one| one.when);
        assert_eq!(
            kept.iter().map(|one| one.path.clone()).collect::<Vec<_>>(),
            vec![older.clone(), old.clone(), used.clone()],
            "the order a tidy-up deletes in is wrong"
        );

        let _ = std::fs::remove_dir_all(&cache);
    }

    /// A cached picture that will not decode is thrown away rather than read
    /// again for ever.
    ///
    /// This is what made one truncated write permanent. The file was there and
    /// not empty, so it was returned as the source; decoding it failed as
    /// something worth retrying; and the retry read the same file. Every
    /// attempt after that, for the life of the machine, went the same way — and
    /// the CDN, which had the picture all along, was never reached, because a
    /// broken file was standing in front of it.
    #[test]
    fn a_cached_picture_that_will_not_decode_is_thrown_away() {
        let scratch = std::env::temp_dir().join(format!("lxb-art-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).expect("a scratch directory");

        let theirs = scratch.join("valve.jpg");
        let ours = scratch.join("ours.jpg");
        std::fs::write(&theirs, b"not a picture either").unwrap();
        std::fs::write(&ours, b"nor is this").unwrap();

        assert!(off_the_disk(Piece::Cover, &[theirs.clone(), ours.clone()], &ours).is_none());
        assert!(
            !ours.exists(),
            "the broken file this shell wrote is still there to be read again"
        );
        // And not out of Valve's, which is not this shell's to delete out of.
        assert!(
            theirs.exists(),
            "a file in the client's own cache was deleted"
        );

        // A real picture is still found, and is not thrown away with them.
        let mut real = Vec::new();
        image::DynamicImage::ImageRgba8(image::RgbaImage::new(8, 8))
            .write_to(
                &mut std::io::Cursor::new(&mut real),
                image::ImageFormat::Png,
            )
            .unwrap();
        std::fs::write(&ours, &real).unwrap();
        assert!(off_the_disk(Piece::Cover, &[theirs.clone(), ours.clone()], &ours).is_some());
        assert!(ours.exists());

        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// A screenshot of a console comes out the size every other picture behind
    /// the bar is, and comes out *soft*.
    ///
    /// The softness is measured rather than asserted, because it is the whole
    /// point: the source here is a chequerboard of single pixels, which is the
    /// worst case an enlargement can be handed, and what must not survive the
    /// journey is the grid. The honest route keeps it — that is what makes it
    /// honest — so the two are compared against each other rather than against
    /// a number nobody could defend.
    #[test]
    fn a_screenshot_comes_back_soft_and_the_right_size() {
        let mut source = image::RgbaImage::new(240, 136);
        for (x, y, pixel) in source.enumerate_pixels_mut() {
            let value = if (x + y) % 2 == 0 { 255 } else { 0 };
            *pixel = image::Rgba([value, value, value, 255]);
        }
        let source = image::DynamicImage::ImageRgba8(source);

        let soft = blurred_scenery_from(source.clone());
        assert_eq!(soft.levels.len(), HERO_LEVELS as usize);
        assert_eq!(
            soft.levels[0].len(),
            (HERO_WIDTH * HERO_HEIGHT * 4) as usize
        );

        // How different one pixel is from the one beside it, averaged over a
        // rung — which for a grid that has survived is enormous and for one
        // that has not is nearly nothing.
        fn roughness(rung: &[u8], width: u32) -> f64 {
            let stride = (width * 4) as usize;
            let mut total = 0f64;
            let mut counted = 0usize;
            for row in rung.chunks_exact(stride) {
                for pixel in 1..width as usize {
                    let (here, before) = (row[pixel * 4], row[(pixel - 1) * 4]);
                    total += f64::from(here.abs_diff(before));
                    counted += 1;
                }
            }
            total / counted as f64
        }

        let honest = scenery_from(source);
        let (soft, honest) = (
            roughness(&soft.levels[0], HERO_WIDTH),
            roughness(&honest.levels[0], HERO_WIDTH),
        );
        assert!(
            soft < honest / 4.0,
            "the softened copy still has the grid in it: {soft} against {honest}"
        );
    }

    /// What a picture behind the bar is *of* is never a game and a file at
    /// once, and the two kinds of file are two kinds — a photograph the user
    /// owns is not blurred, and a screenshot of a console is.
    #[test]
    fn a_screenshot_and_a_photograph_are_not_the_same_sight() {
        let at = PathBuf::from("/cache/snap.png");
        assert_ne!(Sight::Picture(at.clone()), Sight::Snapshot(at.clone()));
        assert_eq!(Sight::Snapshot(at.clone()).game(), None);
        assert_eq!(Sight::Picture(at).game(), None);
        assert_eq!(Sight::Game(7).game(), Some(7));
    }

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
        let published = Published::from_pics(
            lxb_steam::art::LibraryArt {
                capsule: Some("28dbb244/library_600x900.jpg".to_string()),
                hero: Some("67a1c596/library_hero.jpg".to_string()),
                logo: None,
            },
            None,
        );
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
        let published = Published::from_pics(
            lxb_steam::art::LibraryArt {
                capsule: Some("e5b5c644/library_capsule.jpg".to_string()),
                hero: None,
                logo: None,
            },
            None,
        );
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

    use crate::icons::tests::{ico, png};
    use crate::icons::{largest_in_an_ico, ICO_MAGIC};

    /// A small icon is left small. The atlas cell is a ceiling, not a size:
    /// eight of the thirty-three on this machine stop at 32 pixels, and
    /// stretching one into a 256-pixel block would be four times the atlas for
    /// the same detail, softened.
    #[test]
    fn a_small_icon_is_not_stretched_to_fill_its_block() {
        let picture = icon(&ico(&[(32, png(32, false))])).expect("the icon");
        assert_eq!((picture.width, picture.height), (32, 32));
    }

    /// Anything that is not an icon file is left to the crate that decodes
    /// pictures, so a hash that turned out to name a `.jpg` still draws.
    #[test]
    fn what_is_not_an_icon_file_is_decoded_as_what_it_is() {
        let plain = png(48, false);
        assert!(largest_in_an_ico(&plain).is_none());
        let picture = icon(&plain).expect("the picture");
        assert_eq!((picture.width, picture.height), (48, 48));
        assert!(icon(&[]).is_none());
        assert!(icon(&ICO_MAGIC).is_none());
    }
}
