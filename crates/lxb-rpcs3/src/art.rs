//! The shell's copy of each game's pictures, film and music.
//!
//! A PlayStation 3 game carries its own: the icon the XMB lists it with
//! (`ICON0.PNG`), the backdrop the screen turns to while it is highlighted
//! (`PIC1.PNG`), the picture drawn over that backdrop (`PIC0.PNG`), the short
//! film that plays in place of the icon (`ICON1.PAM`) and the music that plays
//! behind it (`SND0.AT3`). The user asked for exactly those, taken from the
//! game rather than fetched from anywhere, and kept.
//!
//! For a game that is a folder — installed, or unpacked — the files are
//! already on the disk and are named where they are. For a disc image or a
//! package they are inside something the shell cannot open, so they are
//! copied out once into `$XDG_CACHE_HOME/lxb/rpcs3/art/<game>/`, beside a
//! stamp of the file they came from; a scan finds the stamp unchanged and
//! reads nothing again. A folder whose game is no longer in any scan is
//! removed, so a game taken off the drive does not leave its pictures behind.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// The files a game's art is made of, as the game names them, in the order a
/// package keeps them — which is the order a zipped one is cheapest to read.
pub const FILES: &[&str] = &["ICON0.PNG", "PIC0.PNG", "PIC1.PNG", "ICON1.PAM", "SND0.AT3"];

/// The most any one of them may be. The biggest of the five is the film, and a
/// real one is a few megabytes; anything past this is not an icon.
pub const LIMIT: u64 = 32 * 1024 * 1024;

/// Where this helper keeps what it keeps: `$XDG_CACHE_HOME/lxb/rpcs3`.
pub fn cache_root() -> Option<PathBuf> {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| crate::rpcs3::home().map(|home| home.join(".cache")))
        .map(|cache| cache.join("lxb/rpcs3"))
}

/// Where the copies are: `art` under [`cache_root`]. The scan is handed this
/// rather than finding it, so a test's scan reads and sweeps a scratch
/// directory of its own and never the real one.
pub fn art_root() -> Option<PathBuf> {
    cache_root().map(|root| root.join("art"))
}

/// The folder one game's copies live in.
pub fn folder(root: &Path, id: &str) -> PathBuf {
    root.join(format!("{:016x}", fnv(id.as_bytes())))
}

/// What a copy was made from: the source file's size and when it last
/// changed. A scan that finds the same stamp reads nothing.
pub fn stamp(source: &Path) -> Option<String> {
    let meta = std::fs::metadata(source).ok()?;
    let modified = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some(format!(
        "{} {} {}\n",
        source.display(),
        meta.len(),
        modified
    ))
}

/// Whether `folder` already holds the copies made from a source with this
/// stamp.
pub fn fresh(folder: &Path, stamp: &str) -> bool {
    std::fs::read_to_string(folder.join("source")).is_ok_and(|kept| kept == stamp)
}

/// Mark `folder` as holding the copies made from this stamp. Written last, so
/// a scan that was stopped halfway is done again rather than trusted.
pub fn seal(folder: &Path, stamp: &str) {
    if let Err(err) = std::fs::write(folder.join("source"), stamp) {
        eprintln!("art: {} could not be sealed: {err}", folder.display());
    }
}

/// Start `folder` over, for a source that has changed.
pub fn clear(folder: &Path) {
    let _ = std::fs::remove_dir_all(folder);
    if let Err(err) = std::fs::create_dir_all(folder) {
        eprintln!("art: {} could not be made: {err}", folder.display());
    }
}

/// Keep one file. Written beside and renamed over, so the shell never opens a
/// half-written picture.
pub fn keep(folder: &Path, name: &str, bytes: &[u8]) -> Option<PathBuf> {
    if bytes.is_empty() {
        return None;
    }
    let at = folder.join(name);
    let partial = folder.join(format!(".{name}.partial"));
    std::fs::write(&partial, bytes)
        .and_then(|()| std::fs::rename(&partial, &at))
        .map_err(|err| eprintln!("art: {} could not be written: {err}", at.display()))
        .ok()?;
    Some(at)
}

/// A kept file, where the folder has it.
pub fn kept(folder: &Path, name: &str) -> Option<PathBuf> {
    let at = folder.join(name);
    at.is_file().then_some(at)
}

/// Take away every game's folder but these.
///
/// Only called after a scan that could read everything it was asked to, so a
/// drive that is not plugged in this morning does not cost its games their
/// pictures.
pub fn sweep(root: &Path, keep: &HashSet<PathBuf>) {
    let Ok(listing) = std::fs::read_dir(root) else {
        return;
    };
    for entry in listing.filter_map(Result::ok) {
        let path = entry.path();
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) && !keep.contains(&path) {
            match std::fs::remove_dir_all(&path) {
                Ok(()) => eprintln!("art: removed {}", path.display()),
                Err(err) => eprintln!("art: {} could not be removed: {err}", path.display()),
            }
        }
    }
}

/// FNV-1a, 64-bit: a name for a folder that is the same every run and on every
/// machine, which the standard library's hasher does not promise.
pub fn fnv(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_is_named_the_same_every_time() {
        assert_eq!(fnv(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv(b"a"), 0xaf63_dc4c_8601_ec8c);
        let root = Path::new("/cache");
        assert_eq!(
            folder(root, "/x/Tekken 6.iso"),
            folder(root, "/x/Tekken 6.iso")
        );
        assert_ne!(
            folder(root, "/x/Tekken 6.iso"),
            folder(root, "/x/Skate 3.iso")
        );
    }

    #[test]
    fn a_stamp_changes_with_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("game.iso");
        std::fs::write(&source, b"one").unwrap();
        let first = stamp(&source).unwrap();
        let art = dir.path().join("art");
        clear(&art);
        assert!(!fresh(&art, &first));
        keep(&art, "ICON0.PNG", b"\x89PNG").unwrap();
        seal(&art, &first);
        assert!(fresh(&art, &first));
        std::fs::write(&source, b"longer").unwrap();
        assert!(!fresh(&art, &stamp(&source).unwrap()));
        assert_eq!(kept(&art, "ICON0.PNG"), Some(art.join("ICON0.PNG")));
        assert_eq!(kept(&art, "ICON1.PAM"), None);
    }
}
