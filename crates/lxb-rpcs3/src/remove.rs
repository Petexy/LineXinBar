//! Uninstalling a game RPCS3 installed: `lxb-rpcs3 remove FOLDER`.
//!
//! What goes is what RPCS3's own "Remove game" takes by default: the game's
//! folder under `dev_hdd0/game` (the game, and the updates and add-ons
//! installed into it), its lock file there, what RPCS3 compiled for it
//! (`cache/<serial>` in RPCS3's cache) and its system cache
//! (`dev_hdd1/caches/<serial>_…`). What stays is what a console keeps when a
//! game is deleted: its saves, its trophies and its licence — and RPCS3's own
//! settings for it, which somebody chose. The package it came from is not
//! touched, so the game is one press from being installed again.
//!
//! Only ever a folder straight under RPCS3's `dev_hdd0/game` that says it is
//! an installed game (`PARAM.SFO`, category `HG`) with a serial of the
//! console's own shape: that serial is what the caches are matched by, and a
//! blank or odd one would match things that are not this game's.

use std::path::{Path, PathBuf};

use crate::report::{Removal, Trouble, PROTOCOL};
use crate::rpcs3::Console;
use crate::sfo::Sfo;

/// Uninstall the game in `folder` from `console`, whose compiled caches are
/// under `cache` (RPCS3's cache folder, where it has one).
pub fn run(folder: &Path, console: &Console, cache: Option<&Path>) -> Removal {
    let (folder, serial) = match installed_game(folder, console) {
        Ok(found) => found,
        Err(note) => return refused(note),
    };
    let mut gone = vec![folder.clone()];
    gone.extend(starting_with(&console.games().join(LOCKS), &serial, ""));
    gone.extend(caches(&serial, console, cache));

    let mut freed = 0;
    for path in &gone {
        let Ok(meta) = std::fs::symlink_metadata(path) else {
            continue;
        };
        let size = size_of(path);
        let removed = if meta.is_dir() {
            std::fs::remove_dir_all(path)
        } else {
            std::fs::remove_file(path)
        };
        match removed {
            Ok(()) => freed += size,
            // The game's own folder is the uninstall; the rest is tidying up.
            Err(err) if *path == folder => {
                return Removal {
                    protocol: PROTOCOL,
                    removed: false,
                    trouble: Some(Trouble::Other),
                    freed,
                    note: format!("{}: {err}", path.display()),
                };
            }
            Err(err) => eprintln!("remove: {} stays: {err}", path.display()),
        }
    }
    Removal {
        protocol: PROTOCOL,
        removed: true,
        trouble: None,
        freed,
        note: format!("{serial}: {} gone", gone.len()),
    }
}

/// Clear what RPCS3 has made for the game with this `serial` and keeps for
/// its next start — the menu's Clear cache. Everything [`caches`] names goes;
/// the game, its saves and everything else of it stay. RPCS3 makes it all
/// again, which is why the next start is slower.
pub fn clear_cache(serial: &str, console: &Console, cache: Option<&Path>) -> Removal {
    if !is_serial(serial) {
        return refused(format!("serial {serial:?}"));
    }
    let mut freed = 0;
    let mut left = Vec::new();
    for path in caches(serial, console, cache) {
        let size = size_of(&path);
        match std::fs::remove_dir_all(&path) {
            Ok(()) => freed += size,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => left.push(format!("{}: {err}", path.display())),
        }
    }
    Removal {
        protocol: PROTOCOL,
        removed: left.is_empty(),
        trouble: (!left.is_empty()).then_some(Trouble::Other),
        freed,
        note: if left.is_empty() {
            format!("{serial}: cache cleared")
        } else {
            left.join("; ")
        },
    }
}

/// Bytes the game with this `serial` has in RPCS3's caches — what
/// [`clear_cache`] would give back. Zero for a serial of any other shape.
pub fn cache_size(serial: &str, console: &Console, cache: Option<&Path>) -> u64 {
    if !is_serial(serial) {
        return 0;
    }
    caches(serial, console, cache)
        .iter()
        .map(|path| size_of(path))
        .sum()
}

/// What RPCS3 keeps of a game that it can make again: what it compiled for
/// it (`cache/<serial>` in its cache folder — the game's code and its
/// shaders, often the biggest thing it has) and the game's own system cache
/// (`dev_hdd1/caches/<serial>_…`).
fn caches(serial: &str, console: &Console, cache: Option<&Path>) -> Vec<PathBuf> {
    let mut found = Vec::new();
    if let Some(cache) = cache {
        found.push(cache.join("cache").join(serial));
    }
    found.extend(starting_with(&console.hdd1.join("caches"), serial, "_"));
    found
}

/// The folder RPCS3 keeps a lock file per game in, under `dev_hdd0/game` —
/// named with a full-width dollar sign, which is how RPCS3 spells it.
const LOCKS: &str = "\u{ff04}locks";

/// Where the installed game in `folder` really is, and its serial — or why
/// it is not one this may remove.
fn installed_game(folder: &Path, console: &Console) -> Result<(PathBuf, String), String> {
    let games = console
        .games()
        .canonicalize()
        .map_err(|err| format!("no installed games: {err}"))?;
    let at = folder
        .canonicalize()
        .map_err(|err| format!("{}: {err}", folder.display()))?;
    if at.parent() != Some(games.as_path()) || !at.is_dir() {
        return Err(format!("{} is not an installed game", folder.display()));
    }
    let sfo = std::fs::read(at.join("PARAM.SFO"))
        .ok()
        .and_then(|bytes| Sfo::parse(&bytes))
        .ok_or_else(|| format!("{}: no PARAM.SFO", folder.display()))?;
    if sfo.category() != Some("HG") {
        return Err(format!("{}: not an installed game", folder.display()));
    }
    let serial = sfo.title_id().unwrap_or_default().to_string();
    if !is_serial(&serial) {
        return Err(format!("{}: serial {serial:?}", folder.display()));
    }
    Ok((at, serial))
}

/// A PlayStation 3 serial: four capitals and five digits, `NPEA00014`.
fn is_serial(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 9
        && bytes[..4].iter().all(u8::is_ascii_uppercase)
        && bytes[4..].iter().all(u8::is_ascii_digit)
}

/// What in `folder` is named `<serial><then>…`.
fn starting_with(folder: &Path, serial: &str, then: &str) -> Vec<PathBuf> {
    let prefix = format!("{serial}{then}");
    let Ok(listing) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    listing
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
        .map(|entry| entry.path())
        .collect()
}

/// Bytes under `path`, not following links.
pub fn size_of(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    std::fs::read_dir(path)
        .map(|listing| listing.flatten().map(|entry| size_of(&entry.path())).sum())
        .unwrap_or(0)
}

fn refused(note: String) -> Removal {
    Removal {
        protocol: PROTOCOL,
        removed: false,
        trouble: Some(Trouble::Other),
        freed: 0,
        note,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sfo;

    fn console_in(dir: &Path) -> Console {
        Console::of(dir)
    }

    fn install(console: &Console, folder: &str, serial: &str, category: &str) -> PathBuf {
        let at = console.games().join(folder);
        std::fs::create_dir_all(at.join("USRDIR")).unwrap();
        std::fs::write(at.join("USRDIR/EBOOT.BIN"), [0u8; 64]).unwrap();
        std::fs::write(
            at.join("PARAM.SFO"),
            sfo::build(
                &[
                    ("TITLE", "SUPER STARDUST HD"),
                    ("TITLE_ID", serial),
                    ("CATEGORY", category),
                ],
                &[],
            ),
        )
        .unwrap();
        at
    }

    /// The game and everything RPCS3 made for it go; its saves, trophies and
    /// licence, and every other game's caches, stay.
    #[test]
    fn a_game_goes_with_its_caches_and_leaves_its_saves() {
        let dir = tempfile::tempdir().unwrap();
        let console = console_in(dir.path());
        let cache = dir.path().join("cache-rpcs3");
        let game = install(&console, "NPEA00014", "NPEA00014", "HG");
        let other = install(&console, "NPEA00019", "NPEA00019", "HG");
        let locks = console.games().join(LOCKS);
        std::fs::create_dir_all(&locks).unwrap();
        std::fs::write(locks.join("NPEA00014_v01.00"), b"").unwrap();
        std::fs::write(locks.join("NPEA00019_v01.00"), b"").unwrap();
        for serial in ["NPEA00014", "NPEA00019"] {
            std::fs::create_dir_all(cache.join("cache").join(serial)).unwrap();
            std::fs::create_dir_all(
                console
                    .hdd1
                    .join("caches")
                    .join(format!("{serial}_{serial}")),
            )
            .unwrap();
        }
        let saves = console.hdd0.join("home/00000001/savedata/NPEA00014-SAVE");
        let trophies = console.trophies().join("NPWR00117_00");
        let licence = console.licences().join("EP9000-NPEA00014_00-SSHD.rap");
        for keep in [&saves, &trophies] {
            std::fs::create_dir_all(keep).unwrap();
        }
        std::fs::create_dir_all(console.licences()).unwrap();
        std::fs::write(&licence, [0u8; 16]).unwrap();

        let removal = run(&game, &console, Some(&cache));
        assert!(removal.removed, "{}", removal.note);
        assert!(removal.freed >= 64 + 64, "{}", removal.freed);
        assert!(!game.exists());
        assert!(!locks.join("NPEA00014_v01.00").exists());
        assert!(!cache.join("cache/NPEA00014").exists());
        assert!(!console.hdd1.join("caches/NPEA00014_NPEA00014").exists());

        assert!(other.join("USRDIR/EBOOT.BIN").exists());
        assert!(locks.join("NPEA00019_v01.00").exists());
        assert!(cache.join("cache/NPEA00019").exists());
        assert!(console.hdd1.join("caches/NPEA00019_NPEA00019").exists());
        assert!(saves.exists() && trophies.exists() && licence.exists());
    }

    /// Nothing is removed that is not an installed game straight under
    /// `dev_hdd0/game`: a disc game's update, a folder elsewhere, a folder
    /// with no serial to match its caches by.
    /// Clear cache takes only what RPCS3 made for that game and can make
    /// again, and says how much that was.
    #[test]
    fn a_cache_is_cleared_and_the_game_stays() {
        let dir = tempfile::tempdir().unwrap();
        let console = console_in(dir.path());
        let cache = dir.path().join("cache-rpcs3");
        let game = install(&console, "NPEA00014", "NPEA00014", "HG");
        for serial in ["NPEA00014", "BLUS30464"] {
            let compiled = cache.join("cache").join(serial);
            std::fs::create_dir_all(&compiled).unwrap();
            std::fs::write(compiled.join("ppu.obj"), [0u8; 100]).unwrap();
            let own = console
                .hdd1
                .join("caches")
                .join(format!("{serial}_{serial}"));
            std::fs::create_dir_all(&own).unwrap();
            std::fs::write(own.join("data"), [0u8; 10]).unwrap();
        }
        assert_eq!(cache_size("NPEA00014", &console, Some(&cache)), 110);
        let cleared = clear_cache("NPEA00014", &console, Some(&cache));
        assert!(cleared.removed, "{}", cleared.note);
        assert_eq!(cleared.freed, 110);
        assert_eq!(cache_size("NPEA00014", &console, Some(&cache)), 0);
        assert!(game.join("USRDIR/EBOOT.BIN").exists());
        assert_eq!(cache_size("BLUS30464", &console, Some(&cache)), 110);
        // Nothing to clear is not a failure.
        assert!(clear_cache("NPEA00014", &console, Some(&cache)).removed);
        // A serial that is not one would match what is not this game's.
        assert!(!clear_cache("", &console, Some(&cache)).removed);
        assert_eq!(cache_size("", &console, Some(&cache)), 0);
    }

    #[test]
    fn only_an_installed_game_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let console = console_in(dir.path());
        let update = install(&console, "BLUS30464", "BLUS30464", "GD");
        let blank = install(&console, "ODD", "", "HG");
        let elsewhere = dir.path().join("elsewhere/NPEA00014");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::copy(update.join("PARAM.SFO"), elsewhere.join("PARAM.SFO")).unwrap();
        let nested = install(&console, "NPEA00014", "NPEA00014", "HG").join("USRDIR");

        for folder in [&update, &blank, &elsewhere, &nested, &console.games()] {
            let removal = run(folder, &console, None);
            assert!(!removal.removed, "{}", folder.display());
            assert_eq!(removal.trouble, Some(Trouble::Other));
            assert!(folder.exists(), "{}", folder.display());
        }
        assert!(is_serial("BLUS30464"));
        assert!(!is_serial("blus30464") && !is_serial("BLUS3046") && !is_serial(""));
    }
}
