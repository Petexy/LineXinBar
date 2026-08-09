//! Getting rid of one of the user's own files, the way the rest of their
//! desktop does it.
//!
//! The bar lists the music, films and photographs under `$HOME`, so the menu
//! over one of those rows has to be able to offer to get rid of it. What it
//! must not do is `unlink`. The shell has no undo, the row it is acting on is a
//! photograph somebody took, and the difference between a wrong press being an
//! annoyance and a wrong press being a loss is entirely this module.
//!
//! So Delete means the freedesktop trash — the same `~/.local/share/Trash` that
//! Dolphin, Nautilus, Thunar and `gio trash` all use. A file put there by the
//! shell can be found and restored from any of them, and from a terminal, by
//! somebody who has never heard of LineXinBar. That interoperability is the
//! whole reason to implement a specification rather than to move the file to a
//! folder of this shell's own choosing.
//!
//! ## The specification, and the two trash directories
//!
//! A trash directory holds `files/` and `info/`. A file is trashed by claiming
//! a name in `info/` — with `O_EXCL`, which is what makes two programs trashing
//! `holiday.mp4` at the same moment produce two entries rather than one lost
//! file — writing a `.trashinfo` under that name saying where the file came
//! from and when it went, and then renaming the file into `files/` under the
//! same name.
//!
//! Which trash directory is the awkward half, and it is awkward for one
//! reason: a rename cannot cross a filesystem. The home trash only works for
//! files that are on the same volume as the home directory, and a home
//! directory with a media drive mounted inside it is an ordinary arrangement —
//! it is, in fact, the arrangement of anybody whose collection is big enough
//! for this bar to be worth having. For those the spec provides a trash at the
//! top of the volume the file is actually on, and this implements both.
//!
//! Copying the file instead is not an option and is not offered. A copy of a
//! forty-gigabyte film into the home directory to "delete" it would fill the
//! disk it was meant to free, and it would not be atomic — a press of the power
//! switch half way through leaves the user with two half-files instead of one
//! whole one.

use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// How many names are tried before giving up on a folder.
///
/// The first collision is ordinary — the user has deleted two files called
/// `IMG_0001.jpg` from two different folders — and the hundredth means
/// something is wrong that trying again will not fix.
const NAMES: u32 = 100;

/// Put `path` in the trash. Returns the trash directory it went to, which is
/// what the shell says out loud afterwards.
///
/// Nothing here removes anything. Every failure leaves the file exactly where
/// it was, which is the one property that matters: a Delete that reports
/// failure and a Delete that reports success must be the only two outcomes
/// there are.
pub fn discard(path: &Path) -> io::Result<PathBuf> {
    discard_into(path, home_trash())
}

/// The same, told where the home trash is.
///
/// Split off so the whole journey can be exercised against a directory made
/// for the purpose, rather than against the trash of whoever is running the
/// tests. Reaching into the environment inside a test would also be reaching
/// into it for every other test running beside it.
fn discard_into(path: &Path, home: Option<PathBuf>) -> io::Result<PathBuf> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "a path with no file name"))?;
    let facts = std::fs::symlink_metadata(path)?;
    let trash = trash_for(path, facts.dev(), home)?;

    let files = trash.dir.join("files");
    let info = trash.dir.join("info");
    std::fs::create_dir_all(&files)?;
    std::fs::create_dir_all(&info)?;

    // What the entry will say it came from: the whole path for the home trash,
    // and the path within the volume for a trash at the top of one — so that a
    // drive trashed from and later mounted somewhere else still restores to the
    // right place inside itself.
    let original = match &trash.within {
        Some(top) => path.strip_prefix(top).unwrap_or(path),
        None => path,
    };
    let deleted_at = stamp();

    for attempt in 1..=NAMES {
        let claimed = numbered(name, attempt);
        let ticket = info.join(format!("{claimed}.trashinfo"));
        // `create_new` is `O_EXCL`: the name is claimed by the file being made,
        // not by having looked and found nothing there a moment ago.
        let mut writing = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&ticket)
        {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        };
        writing.write_all(ticket_body(original, &deleted_at).as_bytes())?;
        writing.flush()?;
        drop(writing);

        match std::fs::rename(path, files.join(&claimed)) {
            Ok(()) => {
                tracing::info!(
                    file = %path.display(),
                    trash = %trash.dir.display(),
                    as_name = claimed,
                    "moved a file to the trash"
                );
                return Ok(trash.dir);
            }
            Err(err) => {
                // The claim is given back. An `info` entry with nothing in
                // `files` is a trash directory every other desktop's trash
                // viewer would show an empty row for.
                let _ = std::fs::remove_file(&ticket);
                return Err(err);
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "the trash already holds a hundred files of this name",
    ))
}

/// A trash directory, and the volume it belongs to if it is not the home one.
struct Trash {
    dir: PathBuf,
    /// The top of the volume, for a trash that is not the home trash — which
    /// is what the paths inside it are written relative to.
    within: Option<PathBuf>,
}

/// Which trash a file on device `dev` goes to.
///
/// The home trash whenever the file is on the same volume as it, because that
/// is the one every desktop looks in first and the one a user knows how to
/// empty. The volume's own otherwise, which is not a fallback so much as the
/// only thing a rename can do.
fn trash_for(path: &Path, dev: u64, home: Option<PathBuf>) -> io::Result<Trash> {
    if let Some(home) = home {
        // The parent, not the trash itself: it may not exist yet, and what is
        // being asked is which volume it *would* be made on.
        let anchor = home.parent().unwrap_or(&home);
        if let Ok(facts) = std::fs::metadata(anchor) {
            if facts.dev() == dev {
                return Ok(Trash {
                    dir: home,
                    within: None,
                });
            }
        }
    }

    let top = top_directory(path, dev).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "no home trash, and no volume to make one on",
        )
    })?;
    Ok(Trash {
        dir: volume_trash(&top)?,
        within: Some(top),
    })
}

/// `$XDG_DATA_HOME/Trash`, or the default the spec gives for it.
fn home_trash() -> Option<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|home| home.is_absolute())
                .map(|home| home.join(".local/share"))
        })?;
    Some(data.join("Trash"))
}

/// The trash at the top of a volume: the administrator's `.Trash` where there
/// is a usable one, and this user's own `.Trash-1000` otherwise.
///
/// The three conditions on `$topdir/.Trash` are the spec's and every one of
/// them is a way the shared directory could be a trap. It must not be a
/// symbolic link, which could point anywhere; it must be a directory; and it
/// must have the sticky bit, without which any user on the machine could delete
/// another's trashed files or replace them. A `.Trash` that fails any of the
/// three is left alone entirely rather than repaired — it is not this shell's
/// directory to fix.
fn volume_trash(top: &Path) -> io::Result<PathBuf> {
    let uid = unsafe { libc::getuid() };
    let shared = top.join(".Trash");
    if let Ok(facts) = std::fs::symlink_metadata(&shared) {
        let sticky = facts.permissions().mode() & 0o1000 != 0;
        if facts.is_dir() && !facts.file_type().is_symlink() && sticky {
            let mine = shared.join(uid.to_string());
            std::fs::create_dir_all(&mine)?;
            return Ok(mine);
        }
    }

    let own = top.join(format!(".Trash-{uid}"));
    std::fs::create_dir_all(&own)?;
    // Nobody else's business. The shared directory above is readable by every
    // user on the machine by design; this one is a folder of the user's own
    // deleted files sitting at the top of a drive, and it is made the way the
    // rest of the shell makes private things.
    let _ = std::fs::set_permissions(&own, std::fs::Permissions::from_mode(0o700));
    Ok(own)
}

/// Where the volume `path` is on is mounted: the last directory going up that
/// is still on device `dev`.
///
/// Walked rather than read out of `/proc/self/mountinfo`. The device number is
/// what actually decides whether a rename will work, a mount table has to be
/// parsed and matched back to a path, and the walk is a handful of `stat` calls
/// on a path that is already in the page cache.
fn top_directory(path: &Path, dev: u64) -> Option<PathBuf> {
    let mut top = path.parent()?.to_path_buf();
    loop {
        let Some(parent) = top.parent() else {
            return Some(top);
        };
        match std::fs::metadata(parent) {
            Ok(facts) if facts.dev() == dev => top = parent.to_path_buf(),
            // The parent is on another volume, so this directory is where the
            // one the file is on is mounted. An unreadable parent is treated
            // the same way: it is as far as this can see.
            _ => return Some(top),
        }
    }
}

/// The name a file is filed under, once collisions have been counted.
///
/// The number goes on the end of the whole name rather than before the
/// extension, which is what `gio trash` does and therefore what a trash folder
/// already looks like on most machines. It is only ever a name inside `files/`
/// — what the file is called if it is restored is written down separately, and
/// is untouched by this.
fn numbered(name: &std::ffi::OsStr, attempt: u32) -> String {
    let name = name.to_string_lossy();
    match attempt {
        1 => name.into_owned(),
        n => format!("{name}.{n}"),
    }
}

/// What goes in the `.trashinfo` file.
///
/// Two keys and a group header, and the whole of the format. `Path` is escaped
/// the way a URL's path is — by the same rule and with the same reserved set as
/// the thumbnail spec's file URIs, which is why the two share one encoder —
/// because a file called `Don't Stop.mp3` has to survive a round trip through
/// an `ini` file that has no quoting.
fn ticket_body(original: &Path, deleted_at: &str) -> String {
    format!(
        "[Trash Info]\nPath={}\nDeletionDate={}\n",
        crate::thumbs::percent_encoded(original),
        deleted_at
    )
}

/// Now, as the spec writes a deletion date: local time, to the second, in the
/// `YYYY-MM-DDThh:mm:ss` form RFC 3339 gives for a time with no zone on it.
///
/// Local rather than UTC because that is what the format says and what every
/// trash viewer prints back unchanged — a file deleted at nine in the evening
/// should not be listed as having been deleted at seven.
fn stamp() -> String {
    let Some(tm) = crate::local_time() else {
        // A clock that cannot be read is not a reason to refuse to delete
        // anything. The epoch is a date, it is obviously not the real one, and
        // the file still lands where it can be found and restored.
        return "1970-01-01T00:00:00".to_string();
    };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

/// Whether a path is one the shell will offer to delete at all.
///
/// The trash is for the user's own things. A file that is not under their home
/// directory is somebody else's — a shared music folder on `/srv`, a mounted
/// disc, a system sample — and the row is drawn greyed rather than removed, so
/// the menu keeps its shape wherever it is raised.
///
/// This is about the *offer*, not about what would work: trashing a file on
/// another volume is perfectly possible and is implemented above. It is about
/// not putting a one-press way to delete other people's files into a shell
/// whose whole point is being driven from a sofa with a controller.
pub fn is_the_users_own(path: &Path) -> bool {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return false;
    };
    home.is_absolute() && path.starts_with(&home) && path != home
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole of the file format, against a name with everything in it a
    /// music collection has: a space, an apostrophe, brackets and a non-ASCII
    /// letter.
    #[test]
    fn the_ticket_says_where_the_file_came_from_and_when_it_went() {
        let body = ticket_body(
            Path::new("/home/x/Music/Don't Stop (Café mix).mp3"),
            "2026-08-09T14:03:05",
        );
        assert_eq!(
            body,
            "[Trash Info]\n\
             Path=/home/x/Music/Don't%20Stop%20(Caf%C3%A9%20mix).mp3\n\
             DeletionDate=2026-08-09T14:03:05\n"
        );
        // The separators stay separators, or nothing could read the path back.
        assert!(body.contains("Path=/home/x/Music/"));
    }

    #[test]
    fn a_taken_name_is_numbered_rather_than_overwritten() {
        let name = std::ffi::OsStr::new("holiday.mp4");
        assert_eq!(numbered(name, 1), "holiday.mp4");
        assert_eq!(numbered(name, 2), "holiday.mp4.2");
        assert_eq!(numbered(name, 17), "holiday.mp4.17");
    }

    /// The date is a date, in the form the spec asks for. What it *says* is the
    /// machine's clock and cannot be asserted; that it is nineteen characters
    /// with the separators in the right places can be.
    #[test]
    fn the_deletion_date_is_written_the_way_the_spec_spells_one() {
        let stamp = stamp();
        assert_eq!(stamp.len(), 19, "{stamp}");
        assert_eq!(&stamp[4..5], "-");
        assert_eq!(&stamp[7..8], "-");
        assert_eq!(&stamp[10..11], "T");
        assert_eq!(&stamp[13..14], ":");
        assert_eq!(&stamp[16..17], ":");
        assert!(stamp[..4].chars().all(|digit| digit.is_ascii_digit()));
    }

    /// A whole trip through the two directories, on a real filesystem: the file
    /// leaves, the entry arrives under both names, and a second file of the
    /// same name does not overwrite the first.
    #[test]
    fn a_file_goes_to_the_trash_and_leaves_a_ticket_behind() {
        let Some(scratch) = scratch("trash-round-trip") else {
            return;
        };
        let trash = scratch.join("Trash");
        let files = trash.join("files");
        let info = trash.join("info");

        for round in 1..=2 {
            let song = scratch.join("Don't Stop.mp3");
            std::fs::write(&song, format!("round {round}")).unwrap();
            assert_eq!(
                discard_into(&song, Some(trash.clone())).ok(),
                Some(trash.clone()),
                "round {round}"
            );
            assert!(!song.exists(), "the file is still where it was");
        }

        // Two files of one name are two entries, and the first is untouched.
        assert_eq!(read_dir(&files), ["Don't Stop.mp3", "Don't Stop.mp3.2"]);
        assert_eq!(
            read_dir(&info),
            ["Don't Stop.mp3.2.trashinfo", "Don't Stop.mp3.trashinfo"]
        );
        assert_eq!(
            std::fs::read_to_string(files.join("Don't Stop.mp3")).unwrap(),
            "round 1"
        );

        let ticket = std::fs::read_to_string(info.join("Don't Stop.mp3.trashinfo")).unwrap();
        assert!(ticket.starts_with("[Trash Info]\nPath="), "{ticket}");
        assert!(
            ticket.contains(&format!("Path={}/Don't%20Stop.mp3\n", scratch.display())),
            "{ticket}"
        );
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// A file the home trash cannot take — because it is on another volume —
    /// goes to a trash at the top of the volume it is actually on, and the
    /// path in its ticket is written relative to that volume.
    ///
    /// The two volumes are faked by pointing the home trash at a directory that
    /// does not exist on any device, which is exactly what `trash_for` has to
    /// cope with on a machine whose `$HOME` is somewhere the shell cannot stat.
    #[test]
    fn a_file_the_home_trash_cannot_take_goes_to_the_volume_s_own() {
        let Some(scratch) = scratch("trash-other-volume") else {
            return;
        };
        let song = scratch.join("holiday.mp4");
        std::fs::write(&song, "film").unwrap();

        let nowhere = scratch.join("no-such-place/Trash");
        let Ok(landed) = discard_into(&song, Some(nowhere)) else {
            return; // No writable volume top; nothing to assert.
        };
        assert!(!song.exists());
        assert!(
            landed
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".Trash"),
            "{}",
            landed.display()
        );
        let ticket =
            std::fs::read_to_string(landed.join("info/holiday.mp4.trashinfo")).unwrap_or_default();
        assert!(ticket.contains("Path="), "{ticket}");
        assert!(
            !ticket.contains("Path=/"),
            "a volume trash writes the path within the volume: {ticket}"
        );

        // Put the volume back as it was found, as far as that can be done
        // without touching anything that was already there: each remove fails
        // harmlessly if the directory holds somebody else's trashed files.
        let _ = std::fs::remove_file(landed.join("files/holiday.mp4"));
        let _ = std::fs::remove_file(landed.join("info/holiday.mp4.trashinfo"));
        let _ = std::fs::remove_dir(landed.join("files"));
        let _ = std::fs::remove_dir(landed.join("info"));
        let _ = std::fs::remove_dir(&landed);
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// Only the user's own files are offered up to the Delete row.
    #[test]
    fn a_file_outside_the_home_directory_is_not_offered() {
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
            return;
        };
        if !home.is_absolute() {
            return;
        }
        assert!(is_the_users_own(&home.join("Music/a.mp3")));
        assert!(!is_the_users_own(Path::new("/srv/music/a.mp3")));
        assert!(!is_the_users_own(&home), "the home directory is not a file");
    }

    /// A directory of this test's own under the system's temporary folder, or
    /// `None` where there is nowhere to write — in which case the test that
    /// wanted it says nothing rather than failing.
    fn scratch(name: &str) -> Option<PathBuf> {
        let dir = std::env::temp_dir().join(format!("lxb-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir)
    }

    fn read_dir(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}
