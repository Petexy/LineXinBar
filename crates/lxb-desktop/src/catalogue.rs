//! Noticing that the machine has gained or lost an application.
//!
//! The bar is built from the `.desktop` files on the disk — see
//! [`crate::apps::scan`] — and that walk happens once, as the session starts.
//! Anything installed afterwards was therefore invisible until the user logged
//! out and back in, which is the one thing a console shell must never ask
//! somebody to do: they installed a game, and the screen they installed it from
//! did not have it on.
//!
//! So the directories the walk reads are watched, and the bar is read again
//! when one of them changes. Watched rather than polled: a session spends
//! hours with nothing installed, and a shell that walked a few hundred desktop
//! files every few seconds to discover that would be spending a game's frame
//! budget on an answer that is almost always "nothing". `inotify` costs one
//! file descriptor and one `read` that returns `EAGAIN`, and the kernel does
//! the noticing.
//!
//! Three things keep the cost of the *answer* bounded as well, because reading
//! the bar again is not free — it is a walk over every entry on the machine,
//! and then the marks of whatever is new:
//!
//! * only a name ending in `.desktop`, or a directory arriving or leaving,
//!   counts as a change at all. A package manager rewrites `mimeinfo.cache`
//!   and a dozen other files in the same directory on its way past, and none
//!   of those can change what is on the bar.
//! * the directories have to go [`SETTLE`] quiet before anything is read.
//!   Installing one thing writes one entry; installing a desktop writes four
//!   hundred, and the bar wants building once at the end of that rather than
//!   four hundred times during it.
//! * and two rebuilds are never closer together than [`COOLDOWN`], so a long
//!   transaction that keeps the directories busy for a minute costs a bounded
//!   number of walks rather than one per lull.

use std::collections::HashMap;
use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How often the kernel's queue is actually emptied.
///
/// Not every pass of the loop. The loop turns 125 times a second to service the
/// controller, and a `read` that answers "nothing" 125 times a second for the
/// whole of a session is a syscall spent on a question whose answer changes a
/// handful of times a day. Four times a second is far inside the queue's depth
/// and far below anything a person could notice.
const LOOK: Duration = Duration::from_millis(250);

/// How long the directories must be quiet before the bar is read again.
///
/// Long enough that one package's files land together — a `.desktop` file, its
/// icons, and whatever else the manager writes beside them — and short enough
/// that somebody who installed something and walked back to the bar finds it
/// there.
const SETTLE: Duration = Duration::from_millis(750);

/// The least time between two rebuilds.
///
/// What [`SETTLE`] cannot bound on its own: a package manager working through a
/// long transaction leaves a quiet gap after nearly every package, and each of
/// those gaps is a walk over every desktop file on the machine. This is what
/// makes a five-minute upgrade cost a known number of walks.
const COOLDOWN: Duration = Duration::from_secs(4);

/// How far below an applications directory watches are placed.
///
/// Entries nest — Wine files a whole Start menu under `wine/Programs`, and
/// `collect_from_dir` recurses into all of it — but they do not nest deeply,
/// and a depth is what keeps a symlinked tree from becoming a watch per
/// directory on the disk.
const DEPTH: usize = 4;

/// The most directories watched at once.
///
/// A watch is a few hundred bytes of kernel memory and this is nowhere near the
/// default per-instance limit; the cap is here so that a machine with a
/// pathological tree under one of these directories loses the deepest watches
/// rather than the shell losing its file descriptor.
const MOST: usize = 128;

/// What the kernel is asked to report.
///
/// `IN_CLOSE_WRITE` rather than `IN_MODIFY`, because what matters is a file
/// that has finished being written; `IN_MOVED_TO` because an installer that
/// cares about being atomic writes beside the directory and renames into it.
/// `IN_ONLYDIR` so a path that is a file when the watch is placed is refused
/// rather than watched.
const MASK: u32 = libc::IN_CREATE
    | libc::IN_DELETE
    | libc::IN_MOVED_TO
    | libc::IN_MOVED_FROM
    | libc::IN_CLOSE_WRITE
    | libc::IN_DELETE_SELF
    | libc::IN_MOVE_SELF
    | libc::IN_ONLYDIR;

/// The fixed part of one `inotify_event`: the watch, the mask, the rename
/// cookie and the length of the name after it, four 32-bit fields.
///
/// Read out of the buffer by offset rather than by casting it to a struct: the
/// layout is part of the kernel's ABI and will not move, and reading it this
/// way means the buffer needs no alignment of its own.
const HEADER: usize = 16;

/// One directory being watched, and what the shell wants out of it.
struct Watched {
    path: PathBuf,
    /// Whether this directory can hold desktop entries, or is only an ancestor
    /// being waited on until the one that can is created. See
    /// [`Watch::place_watches`].
    entries: bool,
    /// How far below an applications directory this is, so a directory created
    /// inside it knows whether it is still within [`DEPTH`].
    depth: usize,
}

/// The clock a change is answered on: when the queue was last read, when the
/// directories last moved, and when a rebuild is next allowed.
///
/// Split out from the watch itself so the pacing can be exercised without a
/// kernel to pace — see the tests at the foot of this file.
#[derive(Debug)]
struct Settling {
    /// When the directories were last seen to change, if they have.
    stirred: Option<Instant>,
    next_look: Instant,
    next_rebuild: Instant,
}

impl Settling {
    fn new(now: Instant) -> Self {
        Self {
            stirred: None,
            next_look: now,
            next_rebuild: now,
        }
    }

    /// Whether the kernel's queue should be emptied now.
    fn look_due(&mut self, now: Instant) -> bool {
        if now < self.next_look {
            return false;
        }
        self.next_look = now + LOOK;
        true
    }

    fn stir(&mut self, now: Instant) {
        self.stirred = Some(now);
    }

    /// Whether the bar should be read off the disk again.
    fn due(&self, now: Instant) -> bool {
        self.stirred
            .is_some_and(|last| now.saturating_duration_since(last) >= SETTLE)
            && now >= self.next_rebuild
    }

    fn rebuilt(&mut self, now: Instant) {
        self.stirred = None;
        self.next_rebuild = now + COOLDOWN;
    }
}

/// The applications directories, watched.
pub struct Watch {
    fd: OwnedFd,
    /// Every directory being watched, by the descriptor the kernel names it in
    /// its events.
    watching: HashMap<i32, Watched>,
    /// The applications directories themselves — what [`crate::apps::scan`]
    /// reads. Kept because one of them may not exist yet: a machine gains
    /// `~/.local/share/flatpak/exports/share/applications` when its first
    /// Flatpak is installed, and that is exactly the moment the bar has to
    /// notice something.
    roots: Vec<PathBuf>,
    clock: Settling,
}

impl Watch {
    /// Start watching the directories the catalogue is built from.
    ///
    /// `None` where the kernel would not give out an `inotify` instance, which
    /// is a session at its descriptor limit. The shell then behaves as it did
    /// before this existed — the bar is what it was when the session started —
    /// rather than failing to start over a convenience.
    pub fn start(roots: Vec<PathBuf>, now: Instant) -> Option<Watch> {
        // SAFETY: a plain flags-only syscall with no arguments to get wrong.
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if fd < 0 {
            tracing::warn!(
                error = %std::io::Error::last_os_error(),
                "cannot watch the applications directories; the bar will be \
                 what it was when the session started"
            );
            return None;
        }
        // SAFETY: `fd` is a fresh descriptor this call owns and nothing else
        // holds.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };

        let mut watch = Watch {
            fd,
            watching: HashMap::new(),
            roots,
            clock: Settling::new(now),
        };
        let _ = watch.place_watches();
        tracing::info!(
            directories = watch.watching.len(),
            "watching the applications directories"
        );
        Some(watch)
    }

    /// Whether the bar should be read off the disk again.
    ///
    /// Empties the kernel's queue on the way past, at most [`LOOK`] often, so
    /// this is safe — and cheap — to ask on every pass of the loop. The answer
    /// stays true until [`Self::rebuilt`] is told the walk happened, so a
    /// caller that cannot rebuild the bar this instant simply asks again on a
    /// later pass.
    pub fn stirred(&mut self, now: Instant) -> bool {
        if self.clock.look_due(now) {
            self.drain(now);
        }
        self.clock.due(now)
    }

    /// Told that the bar has just been read off the disk, by this or by any
    /// other route.
    ///
    /// Also by any other route: an uninstall rebuilds the bar itself, and the
    /// removal it just did is about to arrive here as a queue full of events
    /// asking for the walk that has already happened. They are read and thrown
    /// away here for that reason.
    pub fn rebuilt(&mut self, now: Instant) {
        self.drain(now);
        self.clock.rebuilt(now);
    }

    /// Watch every applications directory that exists, and the nearest
    /// ancestor of every one that does not.
    ///
    /// Called again whenever a directory is created or lost, so a root that
    /// arrives mid-session is picked up by the ancestor that was standing in
    /// for it. Answers whether a directory that can hold entries is now being
    /// watched that was not before — which is itself a change to the bar, since
    /// a directory can be created with its contents already in it.
    fn place_watches(&mut self) -> bool {
        let held: Vec<PathBuf> = self
            .watching
            .values()
            .map(|watched| watched.path.clone())
            .collect();
        let roots = self.roots.clone();
        let mut arrived = false;
        for root in roots {
            if root.is_dir() {
                if !held.contains(&root) {
                    self.watch_tree(&root, 0);
                    arrived = true;
                }
                continue;
            }
            // Not there yet. The directory above it that *is* there says when
            // it arrives, and nothing else would: no desktop file is ever
            // written to a directory that does not exist.
            let Some(ancestor) = root.ancestors().skip(1).find(|path| path.is_dir()) else {
                continue;
            };
            if !held.contains(&ancestor.to_path_buf()) {
                self.add(ancestor, false, 0);
            }
        }
        arrived
    }

    /// Watch a directory of entries and, within [`DEPTH`], everything below it.
    fn watch_tree(&mut self, dir: &Path, depth: usize) {
        if !self.add(dir, true, depth) || depth >= DEPTH {
            return;
        }
        let Ok(listing) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in listing.flatten() {
            // `file_type` rather than `path().is_dir()`: one `stat` that does
            // not follow a symlink, over a directory that on a Flatpak machine
            // is mostly symlinks pointing back into the installation.
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                self.watch_tree(&entry.path(), depth + 1);
            }
        }
    }

    /// Put one watch on the kernel's list. Answers whether there is one there.
    fn add(&mut self, dir: &Path, entries: bool, depth: usize) -> bool {
        if self.watching.len() >= MOST {
            return false;
        }
        let Ok(path) = CString::new(dir.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: `path` is a NUL-terminated string that outlives the call, on
        // a descriptor this struct owns.
        let wd = unsafe { libc::inotify_add_watch(self.fd.as_raw_fd(), path.as_ptr(), MASK) };
        if wd < 0 {
            tracing::debug!(
                directory = %dir.display(),
                error = %std::io::Error::last_os_error(),
                "could not watch a directory of applications"
            );
            return false;
        }
        // The kernel answers with the same descriptor for a directory already
        // watched, so this is also how the same path reached twice collapses to
        // one watch. A directory that holds entries never gives that up to
        // being merely an ancestor of one.
        let held = self.watching.entry(wd).or_insert(Watched {
            path: dir.to_path_buf(),
            entries,
            depth,
        });
        held.entries |= entries;
        true
    }

    /// Read everything the kernel has queued, and decide what it meant.
    fn drain(&mut self, now: Instant) {
        // Aligned to eight so the events inside it start where the kernel put
        // them, and large enough that an ordinary burst is one syscall: an
        // event is 16 bytes plus a name, so this holds a couple of hundred.
        #[repr(align(8))]
        struct Buffer([u8; 8192]);
        let mut buffer = Buffer([0; 8192]);
        // Whether the watches themselves have to be placed again: a directory
        // has gone, or one has appeared where an ancestor was standing in for
        // a root. Done once at the end rather than per event, because a
        // package removing a tree reports every directory in it.
        let mut replace = false;

        loop {
            // SAFETY: the buffer is valid for its own length, and the
            // descriptor is this struct's own and non-blocking.
            let read = unsafe {
                libc::read(
                    self.fd.as_raw_fd(),
                    buffer.0.as_mut_ptr().cast(),
                    buffer.0.len(),
                )
            };
            if read <= 0 {
                let err = std::io::Error::last_os_error();
                // The queue is empty, which is the ordinary answer.
                if read < 0 && err.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                break;
            }

            let read = read as usize;
            let mut at = 0usize;
            while at + HEADER <= read {
                let field = |offset: usize| {
                    let mut bytes = [0u8; 4];
                    bytes.copy_from_slice(&buffer.0[at + offset..at + offset + 4]);
                    bytes
                };
                let wd = i32::from_ne_bytes(field(0));
                let mask = u32::from_ne_bytes(field(4));
                let len = u32::from_ne_bytes(field(12)) as usize;
                let Some(name) = buffer.0.get(at + HEADER..at + HEADER + len) else {
                    break;
                };
                // The name is NUL-padded out to the event's alignment.
                let name = name.split(|byte| *byte == 0).next().unwrap_or_default();
                at += HEADER + len;
                replace |= self.consider(wd, mask, name, now);
            }
        }

        if replace && self.place_watches() {
            // A whole directory of entries appeared where one was being waited
            // for. Whatever was already inside it arrived with it.
            self.clock.stir(now);
        }
    }

    /// What one event means. Answers whether the watches need placing again.
    fn consider(&mut self, wd: i32, mask: u32, name: &[u8], now: Instant) -> bool {
        // The kernel dropped events because nobody read them fast enough. What
        // was in them is unknowable, so the honest answer is to read the bar.
        if mask & libc::IN_Q_OVERFLOW != 0 {
            self.clock.stir(now);
            return true;
        }
        let Some(watched) = self.watching.get(&wd) else {
            return false;
        };
        let (dir, entries, depth) = (watched.path.clone(), watched.entries, watched.depth);

        // The watched directory itself has gone, or been renamed out from
        // under the watch. The kernel drops the watch either way and says so
        // with `IN_IGNORED`.
        if mask & (libc::IN_IGNORED | libc::IN_DELETE_SELF | libc::IN_MOVE_SELF) != 0 {
            self.watching.remove(&wd);
            if entries {
                self.clock.stir(now);
            }
            return true;
        }

        let arriving = mask & (libc::IN_CREATE | libc::IN_MOVED_TO) != 0;
        if mask & libc::IN_ISDIR != 0 {
            if !entries {
                // An ancestor standing in for a root that did not exist. Only
                // a directory appearing under it can be the one being waited
                // for — every other event in a directory like `~/.local/share`
                // is somebody else's business.
                return arriving;
            }
            if arriving && depth < DEPTH {
                let path = dir.join(std::ffi::OsStr::from_bytes(name));
                self.watch_tree(&path, depth + 1);
            }
            // A directory of entries arriving or leaving changes what the walk
            // would find, whether or not this shell managed to watch it.
            self.clock.stir(now);
            return false;
        }

        // And the ordinary case: one file in a directory of entries. Only a
        // desktop entry can change the bar — a package manager rewrites its
        // caches in the same directory, and those are not the catalogue.
        if entries && name.ends_with(b".desktop") {
            self.clock.stir(now);
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TestTree(PathBuf);

    impl TestTree {
        fn new(label: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "lxb-catalogue-test-{label}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn join(&self, path: impl AsRef<Path>) -> PathBuf {
            self.0.join(path)
        }
    }

    impl Drop for TestTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Give the kernel long enough to have queued what was just written, and
    /// ask until it has. Bounded, so a machine that never reports it fails the
    /// test rather than hanging the suite.
    fn wait_for_a_stir(watch: &mut Watch) -> bool {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(2) {
            let now = Instant::now();
            // Straight past the pacing: this is about whether the event was
            // seen at all, and the pacing has tests of its own below.
            watch.clock.next_look = now;
            watch.drain(now);
            if watch.clock.stirred.is_some() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    // --- what counts as a change ------------------------------------------

    /// The thing this exists for: an entry appearing where the walk would find
    /// one.
    #[test]
    fn an_entry_arriving_is_a_change() {
        let tree = TestTree::new("arrived");
        let mut watch = Watch::start(vec![tree.join("applications")], Instant::now()).unwrap();
        // The directory does not exist yet, which is the Flatpak case: the
        // ancestor is what is watched until it does.
        std::fs::create_dir_all(tree.join("applications")).unwrap();
        assert!(wait_for_a_stir(&mut watch), "the directory being created");

        watch.clock.stirred = None;
        std::fs::write(
            tree.join("applications/example.desktop"),
            "[Desktop Entry]\nType=Application\nName=Example\nExec=true\n",
        )
        .unwrap();
        assert!(
            wait_for_a_stir(&mut watch),
            "an entry written into a directory that arrived mid-session"
        );
    }

    /// And the thing that would otherwise make this expensive: a package
    /// manager rewriting the caches it keeps beside the entries.
    #[test]
    fn the_caches_beside_the_entries_are_not() {
        let tree = TestTree::new("caches");
        std::fs::create_dir_all(tree.join("applications")).unwrap();
        let mut watch = Watch::start(vec![tree.join("applications")], Instant::now()).unwrap();

        for name in ["mimeinfo.cache", "kbuildsycoca5", "example.desktop.pacnew"] {
            std::fs::write(tree.join("applications").join(name), "x").unwrap();
        }
        // Long enough that the events are certainly queued, then read them.
        std::thread::sleep(Duration::from_millis(200));
        let now = Instant::now();
        watch.clock.next_look = now;
        watch.drain(now);
        assert!(
            watch.clock.stirred.is_none(),
            "none of those can change what is on the bar"
        );
    }

    // --- the pacing --------------------------------------------------------

    /// Nothing is read until the directories have gone quiet, so one package's
    /// files cost one walk rather than one walk per file.
    #[test]
    fn a_change_waits_for_the_directories_to_settle() {
        let start = Instant::now();
        let mut clock = Settling::new(start);
        clock.stir(start);
        assert!(!clock.due(start));
        assert!(!clock.due(start + SETTLE - Duration::from_millis(1)));
        assert!(clock.due(start + SETTLE));

        // And a second file landing during the wait moves the answer along
        // with it rather than letting it through.
        clock.stir(start + SETTLE - Duration::from_millis(1));
        assert!(!clock.due(start + SETTLE));
    }

    /// And two walks are never closer together than the cooldown, however busy
    /// the directories are.
    #[test]
    fn two_walks_are_never_close_together() {
        let start = Instant::now();
        let mut clock = Settling::new(start);
        clock.rebuilt(start);
        clock.stir(start);
        assert!(!clock.due(start + SETTLE), "inside the cooldown");
        assert!(clock.due(start + COOLDOWN));
    }

    /// The answer stands until somebody actually walks the disk. A shell with
    /// a game in the foreground holds the rebuild back, and must not lose it.
    #[test]
    fn a_change_nobody_acted_on_is_still_a_change() {
        let start = Instant::now();
        let mut clock = Settling::new(start);
        clock.stir(start);
        let after = start + SETTLE;
        assert!(clock.due(after));
        assert!(clock.due(after + Duration::from_secs(600)));
        clock.rebuilt(after + Duration::from_secs(600));
        assert!(!clock.due(after + Duration::from_secs(1200)));
    }

    /// The queue is emptied four times a second and not once a frame.
    #[test]
    fn the_queue_is_read_at_a_pace_of_its_own() {
        let start = Instant::now();
        let mut clock = Settling::new(start);
        assert!(clock.look_due(start));
        assert!(!clock.look_due(start + LOOK - Duration::from_millis(1)));
        assert!(clock.look_due(start + LOOK));
    }
}
