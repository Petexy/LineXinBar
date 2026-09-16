//! The pictures people are recognised by on Steam.
//!
//! A friends list drawn as forty identical marks says only how many friends
//! somebody has. The picture is what makes a row a *person* — the same argument
//! [`crate::art`] makes about a game's cover, at a twentieth of the size — so
//! this is that module's shape with everything a game needed taken out.
//!
//! ## Why it is not part of `art`
//!
//! [`crate::art`] is keyed by app id and by which of Valve's four store pieces
//! is meant, and every one of its sources is about a game: the client's own
//! artwork cache, the published path out of a PICS record, the content network
//! that serves those paths. An avatar has none of that. It is on a host of its
//! own, it is addressed by a hash of the file and by nothing else, and the only
//! thing that knows the hash is the persona state Steam pushed a moment ago.
//! Threading a picture with no app id and no published path through that
//! machinery would have meant a fifth `Piece` that skips every source and every
//! cache the other four share.
//!
//! What the two do share is the far end, and they share it exactly: a picture
//! comes back as a [`Picture`] filed under the path it was cached at, which is
//! the key the atlas holds every thumbnail under. So an avatar and a cover and
//! a photograph of the user's own are one kind of thing to [`crate::gpu`], and
//! this module adds nothing to the atlas at all.
//!
//! ## Nothing is fetched ahead of time
//!
//! The same rule as everywhere else pictures are fetched here, though it costs
//! less to break: an avatar is a few kilobytes, and a hundred of them is a
//! fraction of one game's cover. The panel asks for the rows it is drawing and
//! a few either side, and a row whose picture has not arrived draws the mark it
//! would have drawn anyway.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};

use lxb_steam::art::{Cdn, Missing};
use lxb_steam::AvatarSize;

use crate::thumbs::Picture;

/// How many avatars are fetched at once.
///
/// One. Each is a few kilobytes and a JPEG decode of a 184-pixel square, so the
/// cost is the round trip rather than the work — and a panel of forty rows is
/// forty round trips over a second, which one thread on a keep-alive connection
/// does without anybody watching it fill in. Two would be two TLS handshakes to
/// the same host to halve a wait nobody is having.
const WORKERS: usize = 1;

/// How many outstanding requests are kept.
///
/// A screen's worth several times over. What this guards against is somebody
/// holding a direction on a list of five hundred friends: every row passed
/// wants a picture, and without a bound the queue would be five hundred deep
/// behind the rows the user actually stopped on. Newest first, so the oldest
/// falls off the end — see [`Avatars::want`].
const QUEUE: usize = 64;

/// How long before a picture that could not be reached is asked for again.
const AGAIN: Duration = Duration::from_secs(30);

/// How long a cached avatar is kept after the last time anything wanted it.
///
/// Thirty days. An avatar is filed under a hash of its own contents, so
/// somebody who changes their picture leaves the old one behind under a name
/// nothing will ever ask for again — which on a machine kept for years is the
/// only way this directory grows at all. Each of those is a few kilobytes, so
/// this is tidiness rather than a cap: the cache of somebody with a hundred
/// friends is well under a megabyte.
const KEEP_FOR: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Which picture of which account: the hash Steam files it under, and the size
/// it is wanted at.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Job {
    hash: String,
    size: AvatarSize,
}

/// What one worker hands back: which picture it was, and the file it decoded
/// into — or why there is none.
type Made = (Job, Result<(PathBuf, Picture), Missing>);

/// The workers, and what has been asked of them.
struct Queue {
    /// Most recently wanted at the front.
    jobs: Mutex<VecDeque<Job>>,
    ready: Condvar,
}

/// Everybody's picture, as far as this session has got.
pub struct Avatars {
    queue: Arc<Queue>,
    done: Receiver<Made>,
    /// Asked for and not yet answered, so a row on screen for a hundred frames
    /// is asked for once.
    asked: HashSet<Job>,
    /// Steam has no such picture. Unlike a game's cover this is very nearly
    /// never true — an account with no picture of its own has no hash at all
    /// and never reaches this module — so it is here for the one case that
    /// does happen: a hash from a persona state old enough that Valve has since
    /// swept the file.
    barren: HashSet<Job>,
    /// And these could not be reached, at that moment. See [`AGAIN`].
    later: HashMap<Job, Instant>,
    /// Where each picture turned out to be, once one has been found. That path
    /// is the key the atlas holds it under, so this is both how a row finds a
    /// resident picture and how the shell says it still wants it.
    held: HashMap<Job, PathBuf>,
    /// Whether this session fetches anything at all. False with no Steam
    /// integration, and false for a made-up library — see [`crate::art::Art`],
    /// which is refused for the same reason: a fixture that quietly drew real
    /// people's faces would be a screenshot of somebody who is not there.
    real: bool,
}

impl Avatars {
    /// Start the workers, unless there is nothing real to fetch.
    pub fn start(real: bool) -> Avatars {
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
        Avatars {
            queue,
            done,
            asked: HashSet::new(),
            barren: HashSet::new(),
            later: HashMap::new(),
            held: HashMap::new(),
            real,
        }
    }

    /// Ask for one account's picture, unless it is already being fetched,
    /// already known not to be there, or was unreachable a moment ago.
    ///
    /// A picture this module has already found once is asked for *again*, and
    /// that is deliberate. The atlas gives a cell away the moment nothing wants
    /// it, which is every time the panel is dismissed — so a face fetched on
    /// the first opening is gone from the atlas by the second, and a guard on
    /// [`Self::held`] here would refuse the one request that could put it back.
    /// The whole panel came up wearing the figure that stands for nobody, and
    /// stayed that way for the rest of the session.
    ///
    /// Asking again is cheap because the file is on the disk by then: the
    /// worker finds it in the cache and never reaches the network. It is also
    /// exactly what [`crate::art`] does with a cover, and for the same reason.
    /// The caller's own guard — is the atlas holding this? — is what stops it
    /// being asked once a frame.
    pub fn want(&mut self, hash: &str, size: AvatarSize) {
        let job = Job {
            hash: hash.to_string(),
            size,
        };
        if !self.real
            || self.asked.contains(&job)
            || self.barren.contains(&job)
            || self
                .later
                .get(&job)
                .is_some_and(|when| when.elapsed() < AGAIN)
        {
            return;
        }
        self.later.remove(&job);
        let Ok(mut jobs) = self.queue.jobs.lock() else {
            return;
        };
        if jobs.len() >= QUEUE {
            if let Some(evicted) = jobs.pop_back() {
                // An evicted job has no worker answer coming that could clear
                // the in-flight mark. Forget it now so stopping on that row
                // can ask again instead of wearing the placeholder forever.
                self.asked.remove(&evicted);
            }
        }
        self.asked.insert(job.clone());
        jobs.push_front(job);
        drop(jobs);
        self.queue.ready.notify_one();
    }

    /// Everything finished since the last look, as the atlas takes it.
    ///
    /// Failures are recorded here rather than handed back: there is nothing for
    /// the caller to do about a face that will not arrive, and asking again
    /// next frame is the one thing that must not happen.
    pub fn take(&mut self) -> Vec<(PathBuf, Picture)> {
        let mut out = Vec::new();
        while let Ok((job, answer)) = self.done.try_recv() {
            self.asked.remove(&job);
            match answer {
                Ok((path, picture)) => {
                    self.held.insert(job, path.clone());
                    out.push((path, picture));
                }
                Err(Missing::NotThere) => {
                    self.barren.insert(job);
                }
                Err(Missing::Unreachable(why)) => {
                    tracing::debug!(hash = %job.hash, %why, "an avatar could not be fetched");
                    self.later.insert(job, Instant::now());
                }
            }
        }
        out
    }

    /// Where one account's picture is on this disk, once it has arrived.
    ///
    /// The atlas's key, which is what both a row drawing the picture and the
    /// pass that decides which cells to keep are asking for.
    pub fn at(&self, hash: &str, size: AvatarSize) -> Option<&Path> {
        self.held
            .get(&Job {
                hash: hash.to_string(),
                size,
            })
            .map(PathBuf::as_path)
    }

    /// Where it is on this disk *now*, whether or not this session fetched it.
    ///
    /// [`Self::at`] answers only about pictures this session has asked for and
    /// been handed; this looks at the cache directly. The two are for different
    /// questions and both are needed: the atlas is keyed on what arrived, and
    /// this is for something that has to name a file it cannot wait for — an
    /// announcement, which is built once and carries whatever picture existed
    /// at the moment it was raised. See `Shell::announce_a_message`.
    ///
    /// One `stat`, on a path this module already computes. `None` for a face
    /// nobody has ever fetched, which is an ordinary answer: the caller falls
    /// back to the figure that stands for somebody with no picture.
    pub fn on_disk(&self, hash: &str, size: AvatarSize) -> Option<PathBuf> {
        let path = ours(&Job {
            hash: hash.to_string(),
            size,
        })?;
        path.is_file().then_some(path)
    }
}

/// One worker: fetch, cache, decode, hand back.
fn work(queue: &Queue, send: &Sender<Made>) {
    let cdn = Cdn::new();
    // Once, on the thread that fills the directory, before anything is put in
    // it. See [`KEEP_FOR`].
    tidy_the_cache();
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
        if send.send((job, made)).is_err() {
            return;
        }
    }
}

/// Find one picture and turn it into what the GPU takes.
fn produce(job: &Job, cdn: &Cdn) -> Result<(PathBuf, Picture), Missing> {
    let ours = ours(job).ok_or_else(|| {
        Missing::Unreachable(
            crate::i18n::text("label-there-is-nowhere-to-cache-pictures").to_string(),
        )
    })?;

    // The copy on this disk, if the last session fetched it. A file that is
    // there and will not decode is thrown away rather than stepped over: it is
    // ours, and leaving it would be a face that never arrives for the life of
    // the machine. The same failure `art::off_the_disk` was written for.
    if let Some(bytes) = readable(&ours) {
        match square(&bytes) {
            Some(picture) => {
                still_wanted(&ours);
                return Ok((ours, picture));
            }
            None => {
                tracing::info!(
                    file = %ours.display(),
                    "throwing away a cached avatar that will not decode"
                );
                let _ = std::fs::remove_file(&ours);
            }
        }
    }

    let bytes = cdn.get(&url_of(job))?;
    let picture = square(&bytes).ok_or_else(|| {
        Missing::Unreachable(
            crate::message!("picture-could-not-be-decoded", "file" => job.hash.as_str()),
        )
    })?;
    // Written down only once it is known to be a picture, for the reason above.
    store(&ours, &bytes);
    Ok((ours, picture))
}

/// Where Steam publishes one account's picture.
///
/// Built from the constant host in [`lxb_steam::friends`] and a hash Steam
/// itself sent, which is the whole of the address: there is nothing about an
/// account that can be turned into where its picture is.
fn url_of(job: &Job) -> String {
    lxb_steam::Person {
        steam_id: 0,
        name: String::new(),
        presence: lxb_steam::Presence::Offline,
        game: None,
        app_id: None,
        avatar: Some(job.hash.clone()),
    }
    .avatar_url(job.size)
    // A `Person` with an avatar always has a URL; the `Option` is about an
    // account with no picture, and one of those never becomes a job.
    .unwrap_or_default()
}

/// Where this shell keeps what it had to fetch.
///
/// Under the hash, which is what Steam names the file by — so an account that
/// changes its picture asks for a file this cache has never held rather than
/// showing last year's face for the life of the machine.
fn ours(job: &Job) -> Option<PathBuf> {
    let name = match job.size {
        AvatarSize::Small => format!("{}.jpg", job.hash),
        AvatarSize::Medium => format!("{}_medium.jpg", job.hash),
        AvatarSize::Full => format!("{}_full.jpg", job.hash),
    };
    Some(our_cache()?.join(name))
}

/// `$XDG_CACHE_HOME/lxb/steam-avatars`.
///
/// Beside the artwork cache rather than in it, so that throwing away every
/// picture of a game — which the Settings page offers — does not also throw
/// away every face, and so that neither directory's tidying has to know about
/// the other's rules.
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
    Some(cache.join("lxb").join("steam-avatars"))
}

/// One file's bytes, or nothing.
fn readable(path: &Path) -> Option<Vec<u8>> {
    match std::fs::read(path) {
        Ok(bytes) if !bytes.is_empty() => Some(bytes),
        _ => None,
    }
}

/// Say that a cached file is still in use, so the tidying below can tell a
/// picture nobody has wanted for a month from one drawn this morning.
fn still_wanted(path: &Path) {
    let now = SystemTime::now();
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
/// Into place by rename, so another session reading the cache never sees half a
/// file at a name that says it is whole.
fn store(path: &Path, bytes: &[u8]) {
    let Some(dir) = path.parent() else {
        return;
    };
    if let Err(err) = std::fs::create_dir_all(dir) {
        tracing::debug!(?err, dir = %dir.display(), "cannot make the avatar cache");
        return;
    }
    let part = path.with_extension("part");
    if let Err(err) = std::fs::write(&part, bytes) {
        tracing::debug!(?err, file = %part.display(), "cannot write that avatar");
        return;
    }
    if let Err(err) = std::fs::rename(&part, path) {
        tracing::debug!(?err, file = %path.display(), "cannot put that avatar in place");
        let _ = std::fs::remove_file(&part);
    }
}

/// Throw away every cached face nothing has wanted for [`KEEP_FOR`].
fn tidy_the_cache() {
    let Some(dir) = our_cache() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let mut gone = 0usize;
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .ok()
            .and_then(|data| data.accessed().or_else(|_| data.modified()).ok())
            .and_then(|when| when.elapsed().ok())
            .is_some_and(|since| since > KEEP_FOR);
        if stale && std::fs::remove_file(entry.path()).is_ok() {
            gone += 1;
        }
    }
    if gone > 0 {
        tracing::info!(gone, "threw away avatars nothing has wanted for a month");
    }
}

/// Decode one avatar into what the atlas takes.
///
/// Scaled down only, and never up. Valve serves an avatar at 32, 64 or 184
/// pixels and every one of those is already under the atlas cell, so this is a
/// ceiling that is never reached rather than a resize that happens — it is here
/// so that a picture Valve one day serves larger cannot quietly take four times
/// the atlas block the panel budgeted for.
fn square(bytes: &[u8]) -> Option<Picture> {
    let image = image::load_from_memory(bytes).ok()?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A session with nothing real behind it asks for nothing at all, and so
    /// starts no threads and reaches no network. The same rule `art::Art` is
    /// held to.
    #[test]
    fn an_unreal_session_fetches_nothing() {
        let mut avatars = Avatars::start(false);
        avatars.want("abc", AvatarSize::Medium);
        assert!(avatars.queue.jobs.lock().unwrap().is_empty());
        assert!(avatars.at("abc", AvatarSize::Medium).is_none());
    }

    /// The same face is asked for once however many rows want it.
    #[test]
    fn a_face_on_screen_for_a_hundred_frames_is_asked_for_once() {
        let mut avatars = Avatars::start(false);
        avatars.real = true;
        for _ in 0..100 {
            avatars.want("abc", AvatarSize::Medium);
        }
        assert_eq!(avatars.queue.jobs.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_job_evicted_from_the_bounded_queue_can_be_asked_for_again() {
        let mut avatars = Avatars::start(false);
        avatars.real = true;
        for index in 0..=QUEUE {
            avatars.want(&format!("face-{index}"), AvatarSize::Medium);
        }
        assert_eq!(avatars.queue.jobs.lock().unwrap().len(), QUEUE);
        assert!(!avatars.asked.contains(&Job {
            hash: "face-0".to_string(),
            size: AvatarSize::Medium,
        }));

        avatars.want("face-0", AvatarSize::Medium);
        assert!(avatars.asked.contains(&Job {
            hash: "face-0".to_string(),
            size: AvatarSize::Medium,
        }));
        assert_eq!(avatars.queue.jobs.lock().unwrap().len(), QUEUE);
    }

    /// And the two sizes of one face are two pictures, because they are.
    #[test]
    fn the_head_and_the_row_are_different_pictures() {
        let mut avatars = Avatars::start(false);
        avatars.real = true;
        avatars.want("abc", AvatarSize::Medium);
        avatars.want("abc", AvatarSize::Full);
        assert_eq!(avatars.queue.jobs.lock().unwrap().len(), 2);
    }

    /// A picture already found once is asked for again, because the atlas gives
    /// its cell away every time the panel is dismissed and this is the only
    /// thing that puts it back. See [`Avatars::want`], where the guard that
    /// used to refuse this is written up.
    #[test]
    fn a_face_the_atlas_has_dropped_is_fetched_again() {
        let mut avatars = Avatars::start(false);
        avatars.real = true;
        avatars.held.insert(
            Job {
                hash: "abc".to_string(),
                size: AvatarSize::Medium,
            },
            PathBuf::from("/tmp/abc_medium.jpg"),
        );
        avatars.want("abc", AvatarSize::Medium);
        assert_eq!(avatars.queue.jobs.lock().unwrap().len(), 1);
        assert_eq!(
            avatars.at("abc", AvatarSize::Medium),
            Some(Path::new("/tmp/abc_medium.jpg"))
        );
    }

    /// Every size is addressed at the host Steam publishes it on, under the
    /// hash and nothing else.
    #[test]
    fn a_face_is_addressed_by_its_hash() {
        let url = url_of(&Job {
            hash: "deadbeef".to_string(),
            size: AvatarSize::Full,
        });
        assert_eq!(url, "https://avatars.steamstatic.com/deadbeef_full.jpg");
    }

    /// And cached under a name that says which size it is, so the row's copy
    /// and the head's cannot be written over each other.
    ///
    /// A face on the disk is found whether or not this session fetched it.
    ///
    /// The two questions are different and both are needed: the atlas is keyed
    /// on what *arrived*, and an announcement has to name a file it cannot wait
    /// for. See [`Avatars::on_disk`].
    #[test]
    fn a_cached_face_is_found_without_having_been_asked_for() {
        let scratch = std::env::temp_dir().join(format!("lxb-faces-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        let was = std::env::var_os("XDG_CACHE_HOME");
        unsafe { std::env::set_var("XDG_CACHE_HOME", &scratch) };

        let avatars = Avatars::start(false);
        // Nothing on the disk: nothing to name.
        assert!(avatars.on_disk("abc", AvatarSize::Medium).is_none());

        let cache = our_cache().expect("a cache directory");
        std::fs::create_dir_all(&cache).expect("the cache directory is made");
        let face = cache.join("abc_medium.jpg");
        std::fs::write(&face, b"not really a picture").expect("a file is written");
        assert_eq!(avatars.on_disk("abc", AvatarSize::Medium), Some(face));
        // The size is part of the file's name, so the other one is still not
        // there — which is exactly why a conversation's head has to ask for
        // `Full` rather than relying on the row it was opened from.
        assert!(avatars.on_disk("abc", AvatarSize::Full).is_none());

        match was {
            Some(value) => unsafe { std::env::set_var("XDG_CACHE_HOME", value) },
            None => unsafe { std::env::remove_var("XDG_CACHE_HOME") },
        }
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// The real cache directory is asked for and nothing is written to it: what
    /// is under test is the name, and a test that moved `XDG_CACHE_HOME` would
    /// be moving it for every other test on the same process.
    #[test]
    fn the_two_sizes_are_cached_apart() {
        let name = |size| {
            ours(&Job {
                hash: "abc".to_string(),
                size,
            })
        };
        let (Some(medium), Some(full)) = (name(AvatarSize::Medium), name(AvatarSize::Full)) else {
            // A machine with neither `XDG_CACHE_HOME` nor `HOME` caches
            // nothing, which is its own tested answer above.
            return;
        };
        assert_ne!(medium, full);
        assert!(medium.ends_with("abc_medium.jpg"));
        assert!(full.ends_with("abc_full.jpg"));
        assert!(medium
            .parent()
            .is_some_and(|dir| dir.ends_with("steam-avatars")));
    }
}
