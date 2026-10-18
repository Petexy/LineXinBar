//! Noticing that another program has written into the shell's settings file.
//!
//! `shell.toml` is read once, as the session starts, and rewritten by the shell
//! whenever a row is pressed. For nearly everything in it the shell is the only
//! writer. Two of its keys are not only the shell's: the language and the clock
//! are one answer for the whole desktop, read by the login screen and by every
//! application built on lxb-toolkit, and another program may give that answer
//! too. The live medium's installer is the one this is for. The session it runs
//! in exists for it, and the language somebody picks on its first page is
//! written into the live account's `shell.toml` — and a shell that read the file
//! only at its start went on speaking English behind an installer speaking
//! Polish, and so did every program opened from it afterwards.
//!
//! So the directory the file lives in is watched, and the file read again when
//! it changes. The directory and not the file: the careful way to write a file
//! is beside it and renamed over it — the shell's own [`crate::settings`] does
//! exactly that — and a rename replaces the inode a watch on the file would be
//! holding, so the first careful write would be the last one seen. And where the
//! directory does not exist yet, which on a live medium's first boot it does
//! not, the nearest directory above it that does is watched until it arrives.
//!
//! This module only says *when* to read. What the file says, and whether it is
//! news — the shell's own writes come back through here like anybody's — is
//! [`crate::settings::follow_the_file`]'s to decide.
//!
//! Watched rather than polled, for the catalogue's reason: a session spends
//! hours with nothing written here, and `inotify` costs one descriptor and one
//! `read` that answers "nothing" a few times a second. See [`crate::catalogue`],
//! which watches the applications directories the same way.

use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How often the kernel's queue is emptied.
///
/// Not every pass of the loop, which turns 125 times a second for the
/// controller's sake, and not as rarely as the catalogue's: somebody who has
/// just picked their language is looking at the screen for it to change.
const LOOK: Duration = Duration::from_millis(200);

/// How long the file must be left alone before it is read.
///
/// A writer that does it in more than one step — a temporary and a rename, an
/// editor's backup and then the file — is caught at the end rather than in the
/// middle, and a held volume direction, which writes the file on every step,
/// costs one read at the end rather than one per step.
const SETTLE: Duration = Duration::from_millis(150);

/// What is asked of the kernel about the directory the file lives in.
///
/// A file finished being written in place (`IN_CLOSE_WRITE`), one renamed
/// into place (`IN_MOVED_TO`), and the file going (`IN_DELETE`,
/// `IN_MOVED_FROM`) — plus the directory itself going, after which the watch
/// is placed again.
const ON_THE_DIRECTORY: u32 = libc::IN_CLOSE_WRITE
    | libc::IN_MOVED_TO
    | libc::IN_MOVED_FROM
    | libc::IN_DELETE
    | libc::IN_DELETE_SELF
    | libc::IN_MOVE_SELF
    | libc::IN_ONLYDIR;

/// What is asked about a directory above it, while it does not exist: only a
/// directory arriving, which may be the one being waited for.
const ON_AN_ANCESTOR: u32 = libc::IN_CREATE
    | libc::IN_MOVED_TO
    | libc::IN_DELETE_SELF
    | libc::IN_MOVE_SELF
    | libc::IN_ONLYDIR;

/// The fixed part of one `inotify_event`; see [`crate::catalogue`].
const HEADER: usize = 16;

/// Which directory the one watch is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Place {
    /// The directory the file lives in.
    Directory,
    /// The nearest directory above that one that exists, standing in for it.
    Ancestor,
}

/// What one event means.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Meaning {
    /// The file may say something new.
    read: bool,
    /// The watch has to be placed again: its directory has gone, or a
    /// directory has arrived that may be the one being waited for.
    place: bool,
}

/// What one event on the watched directory means, given the file's name.
///
/// Pure, so what counts as a change can be held without a kernel.
fn meaning(mask: u32, name: &[u8], file: &[u8], place: Place) -> Meaning {
    // The kernel dropped events because nobody read them. What was in them is
    // unknowable, so the honest answer is to look at everything again.
    if mask & libc::IN_Q_OVERFLOW != 0 {
        return Meaning {
            read: true,
            place: true,
        };
    }
    // The watched directory itself has gone or been renamed away; the kernel
    // drops the watch and says so with `IN_IGNORED`. Where that directory was
    // the file's own, the file went with it.
    if mask & (libc::IN_IGNORED | libc::IN_DELETE_SELF | libc::IN_MOVE_SELF) != 0 {
        return Meaning {
            read: place == Place::Directory,
            place: true,
        };
    }
    match place {
        // Only the file. The shell's own temporary (`shell.toml.new`), the
        // compositor's `config.toml` and the per-application `apps.toml` all
        // live in the same directory and none of them is this.
        Place::Directory => Meaning {
            read: name == file
                && mask
                    & (libc::IN_CLOSE_WRITE
                        | libc::IN_MOVED_TO
                        | libc::IN_MOVED_FROM
                        | libc::IN_DELETE)
                    != 0,
            place: false,
        },
        // Only a directory arriving: every file written in `~/.config` is
        // somebody else's business.
        Place::Ancestor => Meaning {
            read: false,
            place: mask & libc::IN_ISDIR != 0 && mask & (libc::IN_CREATE | libc::IN_MOVED_TO) != 0,
        },
    }
}

/// Whether a change seen at `stirred` has been left alone long enough to read.
fn settled(stirred: Option<Instant>, now: Instant) -> bool {
    stirred.is_some_and(|at| now.saturating_duration_since(at) >= SETTLE)
}

/// The settings file's directory, watched.
pub struct Watch {
    fd: OwnedFd,
    file: PathBuf,
    /// The one watch: the descriptor the kernel names it by in its events,
    /// which directory it is on, and that directory.
    placed: Option<(i32, Place, PathBuf)>,
    /// When the file was last seen to change and has not been read since.
    stirred: Option<Instant>,
    next_look: Instant,
}

impl Watch {
    /// Start watching for `file` to be written.
    ///
    /// `None` where the kernel would not give out an `inotify` instance. The
    /// shell then reads the file at its start and never again, which is what
    /// it did before this existed — a convenience lost, not a session.
    pub fn start(file: PathBuf, now: Instant) -> Option<Watch> {
        // SAFETY: a plain flags-only syscall with no arguments to get wrong.
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if fd < 0 {
            tracing::warn!(
                error = %std::io::Error::last_os_error(),
                "cannot watch the shell settings; a language set by another program \
                 waits for the next session"
            );
            return None;
        }
        // SAFETY: `fd` is a fresh descriptor this call owns and nothing else
        // holds.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let mut watch = Watch {
            fd,
            file,
            placed: None,
            stirred: None,
            next_look: now,
        };
        watch.place(now);
        tracing::info!(
            file = %watch.file.display(),
            watching = ?watch.placed.as_ref().map(|(_, _, at)| at.display().to_string()),
            "watching the shell settings for a language or a clock set by another program"
        );
        Some(watch)
    }

    /// Whether the file should be read again now.
    ///
    /// Empties the kernel's queue on the way past, at most [`LOOK`] often, so
    /// it is cheap to ask on every pass of the loop. The answer stays true
    /// until [`Self::read`] is told the file has been read.
    pub fn stirred(&mut self, now: Instant) -> bool {
        if now >= self.next_look {
            self.next_look = now + LOOK;
            self.drain(now);
        }
        settled(self.stirred, now)
    }

    /// Told that the file has just been read.
    pub fn read(&mut self) {
        self.stirred = None;
    }

    /// Put the one watch on the file's directory, or on the nearest directory
    /// above it while that does not exist. Nothing changes where it already is.
    fn place(&mut self, now: Instant) {
        let Some(directory) = self.file.parent() else {
            return;
        };
        let (at, place) = match directory.is_dir() {
            true => (directory.to_path_buf(), Place::Directory),
            false => match directory.ancestors().skip(1).find(|path| path.is_dir()) {
                Some(ancestor) => (ancestor.to_path_buf(), Place::Ancestor),
                None => return,
            },
        };
        if self
            .placed
            .as_ref()
            .is_some_and(|(_, was, path)| *was == place && *path == at)
        {
            return;
        }
        if let Some((wd, ..)) = self.placed.take() {
            // SAFETY: a watch this struct placed, on a descriptor it owns. The
            // `IN_IGNORED` it answers with names a watch no longer held, and
            // is passed over.
            unsafe { libc::inotify_rm_watch(self.fd.as_raw_fd(), wd) };
        }
        let mask = match place {
            Place::Directory => ON_THE_DIRECTORY,
            Place::Ancestor => ON_AN_ANCESTOR,
        };
        let Some(wd) = add_watch(&self.fd, &at, mask) else {
            return;
        };
        self.placed = Some((wd, place, at));
        // The directory may have arrived with the file already in it — a
        // `mkdir -p` and a write, both before this saw the first of them. And
        // at the start, the file may have been written between the shell
        // reading it and this watch being placed. Reading it costs one small
        // file, and a text the shell already knows is nothing new.
        if place == Place::Directory && self.file.exists() {
            self.stirred = Some(now);
        }
    }

    /// Read everything the kernel has queued, and decide what it meant.
    fn drain(&mut self, now: Instant) {
        #[repr(align(8))]
        struct Buffer([u8; 4096]);
        let mut buffer = Buffer([0; 4096]);
        let file = self
            .file
            .file_name()
            .map(|name| name.as_bytes().to_vec())
            .unwrap_or_default();
        let mut place_again = false;
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
                if read < 0
                    && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
                {
                    continue;
                }
                // The queue is empty, which is the ordinary answer.
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
                // An overflow names no watch; every other event names the one
                // held, or one let go a moment ago that is passed over.
                let place = match &self.placed {
                    _ if mask & libc::IN_Q_OVERFLOW != 0 => Place::Directory,
                    Some((held, place, _)) if *held == wd => *place,
                    _ => continue,
                };
                let meant = meaning(mask, name, &file, place);
                if meant.read {
                    self.stirred = Some(now);
                }
                if meant.place {
                    // The kernel has dropped a watch on a directory that went,
                    // so it is not asked to drop it again.
                    if mask & (libc::IN_IGNORED | libc::IN_DELETE_SELF | libc::IN_MOVE_SELF) != 0 {
                        self.placed = None;
                    }
                    place_again = true;
                }
            }
        }
        if place_again {
            self.place(now);
        }
    }
}

/// Put one watch on the kernel's list, and answer its descriptor.
fn add_watch(fd: &OwnedFd, directory: &Path, mask: u32) -> Option<i32> {
    let path = CString::new(directory.as_os_str().as_bytes()).ok()?;
    // SAFETY: `path` is a NUL-terminated string that outlives the call, on a
    // descriptor the caller owns.
    let wd = unsafe { libc::inotify_add_watch(fd.as_raw_fd(), path.as_ptr(), mask) };
    if wd < 0 {
        tracing::debug!(
            directory = %directory.display(),
            error = %std::io::Error::last_os_error(),
            "could not watch the directory of the shell settings"
        );
        return None;
    }
    Some(wd)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &[u8] = b"shell.toml";

    /// What a writer of the file does is a change; what the shell's
    /// neighbours in the same directory do is not.
    #[test]
    fn only_the_file_itself_is_a_change() {
        let on = |mask, name: &[u8]| meaning(mask, name, FILE, Place::Directory);
        // Renamed into place, which is the careful writer and the shell itself.
        assert!(on(libc::IN_MOVED_TO, FILE).read);
        // Written in place and closed, which is everybody else.
        assert!(on(libc::IN_CLOSE_WRITE, FILE).read);
        // Gone, which is nobody having chosen.
        assert!(on(libc::IN_DELETE, FILE).read);
        assert!(on(libc::IN_MOVED_FROM, FILE).read);
        // The shell's own temporary on its way to becoming the file, and the
        // two other files that live beside it.
        assert_eq!(
            on(libc::IN_CLOSE_WRITE, b"shell.toml.new"),
            Meaning::default()
        );
        assert_eq!(
            on(libc::IN_MOVED_FROM, b"shell.toml.new"),
            Meaning::default()
        );
        assert_eq!(on(libc::IN_CLOSE_WRITE, b"config.toml"), Meaning::default());
        assert_eq!(on(libc::IN_MOVED_TO, b"apps.toml"), Meaning::default());
    }

    /// The directory going takes the file with it and the watch with both; a
    /// directory arriving above it may be the one being waited for.
    #[test]
    fn a_directory_coming_or_going_places_the_watch_again() {
        assert_eq!(
            meaning(libc::IN_DELETE_SELF, b"", FILE, Place::Directory),
            Meaning {
                read: true,
                place: true,
            }
        );
        assert_eq!(
            meaning(libc::IN_IGNORED, b"", FILE, Place::Ancestor),
            Meaning {
                read: false,
                place: true,
            }
        );
        assert_eq!(
            meaning(
                libc::IN_CREATE | libc::IN_ISDIR,
                b"lxb",
                FILE,
                Place::Ancestor
            ),
            Meaning {
                read: false,
                place: true,
            }
        );
        assert_eq!(
            meaning(
                libc::IN_MOVED_TO | libc::IN_ISDIR,
                b"lxb",
                FILE,
                Place::Ancestor
            ),
            Meaning {
                read: false,
                place: true,
            }
        );
        // A file in `~/.config` is not a directory arriving, even one called
        // what the file is.
        assert_eq!(
            meaning(libc::IN_CREATE, FILE, FILE, Place::Ancestor),
            Meaning::default()
        );
        // And events nobody can account for are both.
        assert_eq!(
            meaning(libc::IN_Q_OVERFLOW, b"", FILE, Place::Ancestor),
            Meaning {
                read: true,
                place: true,
            }
        );
    }

    /// A scratch directory of the test's own, so that what the kernel reports
    /// is about nothing but what the test wrote.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "lxb-settings-watch-{label}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Ask the kernel until it has reported a change to the file, straight past
    /// the pacing. Bounded, so a kernel that never reports it fails the test
    /// rather than hanging the suite.
    fn wait_for_a_change(watch: &mut Watch) -> bool {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(2) {
            watch.drain(Instant::now());
            if watch.stirred.take().is_some() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    /// The two ways the file is written — beside it and renamed over it, and
    /// in place — are both seen, in a directory that did not exist when the
    /// watch was started, as `~/.config/lxb` does not on a live medium's first
    /// boot. The shell's own temporary on its own is not.
    #[test]
    fn a_write_and_rename_into_a_directory_that_arrived_later_is_seen() {
        let scratch = Scratch::new("arrives");
        let directory = scratch.0.join("config/lxb");
        let file = directory.join("shell.toml");
        let mut watch = Watch::start(file.clone(), Instant::now()).unwrap();
        assert_eq!(
            watch.placed.as_ref().map(|(_, place, _)| *place),
            Some(Place::Ancestor)
        );

        std::fs::create_dir_all(&directory).unwrap();
        // Two levels arrived at once; the watch climbs down as each is seen.
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(2)
            && watch.placed.as_ref().map(|(_, place, _)| *place) != Some(Place::Directory)
        {
            watch.drain(Instant::now());
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            watch.placed.as_ref().map(|(_, place, _)| *place),
            Some(Place::Directory)
        );
        watch.stirred = None;

        let temporary = directory.join("shell.toml.new");
        std::fs::write(&temporary, "language = \"pl\"\n").unwrap();
        std::thread::sleep(Duration::from_millis(100));
        watch.drain(Instant::now());
        assert!(watch.stirred.is_none(), "the temporary is not the file");
        std::fs::rename(&temporary, &file).unwrap();
        assert!(wait_for_a_change(&mut watch), "renamed over the file");

        std::fs::write(&file, "language = \"de\"\n").unwrap();
        assert!(wait_for_a_change(&mut watch), "written in place");

        std::fs::remove_file(&file).unwrap();
        assert!(wait_for_a_change(&mut watch), "taken away");
    }

    /// Nothing is read until the file has been left alone for a moment, and a
    /// second write in that moment moves the read along with it.
    #[test]
    fn a_change_is_read_once_the_writer_has_finished() {
        let start = Instant::now();
        assert!(!settled(None, start + SETTLE * 10));
        assert!(!settled(Some(start), start));
        assert!(!settled(
            Some(start),
            start + SETTLE - Duration::from_millis(1)
        ));
        assert!(settled(Some(start), start + SETTLE));
        let again = start + SETTLE - Duration::from_millis(1);
        assert!(!settled(Some(again), start + SETTLE));
    }
}
