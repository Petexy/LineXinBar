//! Where a screenshot goes.
//!
//! The compositor takes the picture — it is the only half of the session that
//! has the window's pixels — but *where* it lands is a question about the
//! user's home directory, which is the shell's half. So the path is worked out
//! here and sent with the request.
//!
//! ## Why this is not `$HOME/Pictures`
//!
//! It is on an English installation and on no other. `xdg-user-dirs` creates
//! those folders in the language the account was made in and writes down where
//! it put them, so the same directory is `Bilder` in German, `Imagens` in
//! Portuguese, `Изображения` in Russian and `画像` in Japanese. A shell that
//! joined the literal string "Pictures" onto `$HOME` would silently make a
//! second, English folder beside the user's real one on most of the machines in
//! the world, and the screenshots would be in the one they never open.
//!
//! So the answer is read where the desktop keeps it, in the order the
//! specification lays down:
//!
//! 1. `XDG_PICTURES_DIR` in the environment, for a session that sets it;
//! 2. `user-dirs.dirs` under the config directory, which is where
//!    `xdg-user-dirs-update` records the translated names — the file every
//!    other desktop reads too;
//! 3. `$HOME/Pictures`, which is what `xdg-user-dirs` itself falls back to when
//!    it has never run.
//!
//! The `Screenshots` folder inside it is deliberately *not* translated: nothing
//! records a localised name for it, and it is the name Spectacle and
//! GNOME's own tool both use — a user with two screenshot tools should end up
//! with one folder, not three.

use std::path::{Path, PathBuf};

/// The folder screenshots are filed in, inside the user's pictures.
const FOLDER: &str = "Screenshots";

/// Where to write a screenshot taken now, creating the folder if it is not
/// there yet.
///
/// `None` when there is nowhere to write: no home directory, or a folder that
/// cannot be created. The caller has to be able to say so — a shell that
/// reports a screenshot it never took is worse than one that reports the
/// failure.
pub fn destination(taken_at: &libc::tm) -> Option<PathBuf> {
    let folder = pictures_dir()?.join(FOLDER);
    if let Err(err) = std::fs::create_dir_all(&folder) {
        tracing::warn!(?err, ?folder, "cannot make a folder for screenshots");
        return None;
    }
    Some(folder.join(file_name(taken_at, 0)))
}

/// The user's pictures folder, whatever it is called on this machine.
fn pictures_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_absolute());
    if let Some(named) = std::env::var_os("XDG_PICTURES_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
    {
        return Some(named);
    }
    let home = home?;
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".config"));
    let recorded = std::fs::read_to_string(config.join("user-dirs.dirs"))
        .ok()
        .and_then(|file| user_dir(&file, "XDG_PICTURES_DIR", &home));
    Some(recorded.unwrap_or_else(|| home.join("Pictures")))
}

/// Read one directory out of a `user-dirs.dirs` file.
///
/// The format is a shell fragment the desktop's own tools source, so the value
/// is quoted and normally starts with `$HOME`. Only that one expansion is
/// performed: this is not a shell, and a line naming any other variable is left
/// alone rather than half-resolved into a path that does not exist.
fn user_dir(file: &str, key: &str, home: &Path) -> Option<PathBuf> {
    for line in file.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if name.trim() != key {
            continue;
        }
        let value = value.trim().trim_matches('"');
        let path = match value.strip_prefix("$HOME") {
            // `$HOME/` — and also a bare `$HOME`, which is how a user who keeps
            // their pictures loose in their home directory is recorded.
            Some(rest) => home.join(rest.trim_start_matches('/')),
            None if value.starts_with('/') => PathBuf::from(value),
            // Anything else is relative to something this cannot know, or names
            // a variable this does not expand.
            None => continue,
        };
        return Some(path);
    }
    None
}

/// What one screenshot is called: the date and the time it was taken, and a
/// number after that only when a second one lands in the same second.
///
/// Sortable rather than pretty — a folder of these is browsed in order — and
/// with no colons in it, because a picture the user may well copy onto a memory
/// stick should not be a file name that cannot be written to one.
fn file_name(tm: &libc::tm, again: u32) -> String {
    let stamp = format!(
        "Screenshot {:04}-{:02}-{:02} {:02}-{:02}-{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    );
    match again {
        0 => format!("{stamp}.png"),
        n => format!("{stamp} ({n}).png"),
    }
}

/// A folder as a panel should print it: the user's home written as `~`.
///
/// The panel has one line to say where the picture went, and half of that line
/// is `/home/<user>/` on every machine — the half the person reading it already
/// knows. Shortening it is what lets the part they do not know fit.
pub fn abbreviated(folder: &Path) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match home.and_then(|home| folder.strip_prefix(home).ok().map(Path::to_path_buf)) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Some(rest) => format!("~/{}", rest.display()),
        None => folder.display().to_string(),
    }
}

/// The first name in `folder` that is not taken, starting from `wanted`.
///
/// Two screenshots in the same second are one press of a button that repeated,
/// and the second one must not overwrite the first.
pub fn unclaimed(wanted: PathBuf, taken_at: &libc::tm) -> PathBuf {
    if !wanted.exists() {
        return wanted;
    }
    let Some(folder) = wanted.parent() else {
        return wanted;
    };
    // Bounded: a hundred pictures in one second is a stuck button, and the
    // hundredth may be overwritten rather than searched for forever.
    (1..100)
        .map(|again| folder.join(file_name(taken_at, again)))
        .find(|candidate| !candidate.exists())
        .unwrap_or(wanted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(year: i32, month: i32, day: i32, hour: i32, minute: i32, second: i32) -> libc::tm {
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        tm.tm_year = year - 1900;
        tm.tm_mon = month - 1;
        tm.tm_mday = day;
        tm.tm_hour = hour;
        tm.tm_min = minute;
        tm.tm_sec = second;
        tm
    }

    #[test]
    fn a_translated_pictures_folder_is_read_out_of_the_desktop_s_own_file() {
        // A German account: the folder is called Bilder, and joining the
        // English word onto $HOME would miss it entirely.
        let file = "# This file is written by xdg-user-dirs-update\n\
             XDG_DESKTOP_DIR=\"$HOME/Schreibtisch\"\n\
             XDG_PICTURES_DIR=\"$HOME/Bilder\"\n\
             XDG_VIDEOS_DIR=\"$HOME/Videos\"\n";
        assert_eq!(
            user_dir(file, "XDG_PICTURES_DIR", Path::new("/home/lena")),
            Some(PathBuf::from("/home/lena/Bilder"))
        );
    }

    /// The rest of the shapes that file comes in: a commented-out line, a
    /// picture folder that is the home directory itself, an absolute path, and
    /// a key that simply is not there.
    #[test]
    fn the_awkward_lines_of_a_user_dirs_file() {
        let home = Path::new("/home/kenji");
        assert_eq!(
            user_dir(
                "#XDG_PICTURES_DIR=\"$HOME/写真\"\n",
                "XDG_PICTURES_DIR",
                home
            ),
            None,
            "a commented-out line is not a choice the user made"
        );
        assert_eq!(
            user_dir("XDG_PICTURES_DIR=\"$HOME\"\n", "XDG_PICTURES_DIR", home),
            Some(home.to_path_buf())
        );
        assert_eq!(
            user_dir(
                "XDG_PICTURES_DIR=\"/mnt/photos\"\n",
                "XDG_PICTURES_DIR",
                home
            ),
            Some(PathBuf::from("/mnt/photos"))
        );
        assert_eq!(
            user_dir("XDG_MUSIC_DIR=\"$HOME/音楽\"\n", "XDG_PICTURES_DIR", home),
            None
        );
        assert_eq!(
            user_dir(
                "XDG_PICTURES_DIR=\"$OTHER/pics\"\n",
                "XDG_PICTURES_DIR",
                home
            ),
            None,
            "this is not a shell; a variable it cannot expand is not a path"
        );
    }

    /// The panel has one line for the folder, and half of any path on this
    /// machine is the part the reader already knows.
    #[test]
    fn a_folder_is_printed_against_the_home_it_is_in() {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let Some(home) = home.filter(|home| home.is_absolute()) else {
            return; // No home to shorten against; nothing to assert.
        };
        assert_eq!(
            abbreviated(&home.join("Bilder/Screenshots")),
            "~/Bilder/Screenshots"
        );
        assert_eq!(abbreviated(&home), "~");
        assert_eq!(abbreviated(Path::new("/mnt/photos")), "/mnt/photos");
    }

    #[test]
    fn a_picture_is_named_after_the_moment_it_was_taken() {
        let tm = at(2026, 8, 8, 2, 16, 45);
        assert_eq!(file_name(&tm, 0), "Screenshot 2026-08-08 02-16-45.png");
        assert_eq!(file_name(&tm, 3), "Screenshot 2026-08-08 02-16-45 (3).png");
    }
}
