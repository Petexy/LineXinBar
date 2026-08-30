//! Reading somebody's ROM folder.
//!
//! The shape is the one the shell asks the user for and the one every emulator
//! front end has assumed for twenty years: one folder, a subfolder per console,
//! the games inside. Nothing here guesses at a console from the files — see
//! [`crate::consoles`] for why the folder's name is the only statement of
//! intent there is — and nothing is read outside the folder that was named.
//!
//! ## How far down it looks, and why not one level
//!
//! Disc games are usually kept a folder each: a `.cue` and its `.bin` halves, a
//! `.m3u` and three discs, a folder of `.chd`s. So the walk goes down
//! [`DEPTH`] levels below the console and lists what it finds flat, under each
//! file's own name. Deeper than that is somebody's whole home directory
//! arriving in a column, which [`CEILING`] is the second guard against.
//!
//! ## The three things it leaves out
//!
//! 1. **What is not a game.** Saves, states, patches, box art — see
//!    [`crate::consoles::NEVER`], which is what an unknown console is filtered
//!    by, and the per-console extension list, which is what a known one is
//!    filtered by.
//! 2. **The halves of a disc image.** A `.bin` beside a `.cue` of the same name
//!    is not a game anybody can start; the `.cue` is. Same for `.gdi`, `.ccd`
//!    and the `.iso` a `.m3u` names.
//! 3. **Everything hidden.** A dotfile is not somebody's game, and `.thumbnails`
//!    under a ROM folder is thousands of pictures nobody asked to see listed.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::consoles::{self, Machine};
use crate::report::{Console, Library, Rom, PROTOCOL};

/// How far below a console's own folder a game may be.
///
/// Two is a game in a folder of its own with its own subfolder — which is what
/// a dumped disc looks like when the dumper kept the extras.
pub const DEPTH: usize = 3;

/// The most games one console may contribute.
///
/// A guard against a folder that is not what it was said to be, rather than a
/// limit anybody is expected to meet: an arcade collection is thousands of
/// files and is meant to work. What it stops is a ROM folder pointed at a home
/// directory turning into a column with a hundred thousand rows in it, which
/// would be the shell's memory rather than an inconvenience.
pub const CEILING: usize = 8192;

/// The files that stand for a whole disc, and are therefore what a game *is*
/// when one of them is beside the pieces it names.
const SHEETS: &[&str] = &["cue", "gdi", "ccd", "m3u"];

/// Read the folder, and answer with the consoles that have something in them.
pub fn library(roms: &Path) -> Library {
    let mut consoles = Vec::new();
    let entries = match std::fs::read_dir(roms) {
        Ok(entries) => entries,
        Err(err) => {
            return Library {
                protocol: PROTOCOL,
                roms: roms.to_string_lossy().into_owned(),
                consoles,
                system: None,
                unreadable: Some(err.to_string()),
            }
        }
    };

    let mut folders: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|at| at.is_dir() && !hidden(at))
        .collect();
    // The disk answers in whatever order it likes, and a column that came back
    // in a different order after a game was added would be a bar that moved
    // under somebody who had not touched it.
    folders.sort();

    for at in folders {
        if let Some(console) = console(&at) {
            consoles.push(console);
        }
    }
    // By the name the user will read, rather than by the folder name it came
    // from: the column says "Nintendo 64" and sorting it under `n64` would be
    // sorting a list by something nobody can see.
    consoles.sort_by_key(|console| console.title.to_lowercase());

    Library {
        protocol: PROTOCOL,
        roms: roms.to_string_lossy().into_owned(),
        consoles,
        unreadable: None,
        // Filled by `main`, which is where the RetroArch this machine would use
        // is looked up: a scan on its own walks a folder and asks the disk
        // nothing about emulators.
        system: None,
    }
}

/// One subfolder, or `None` where it holds no game.
///
/// The core is not looked up here — see `main`, which does it once for the
/// whole library because [`crate::find::Cores`] is a directory listing and
/// asking it per console would be one per column.
fn console(at: &Path) -> Option<Console> {
    let folder = at.file_name()?.to_string_lossy().into_owned();
    let machine = consoles::machine(&folder);
    let mut roms = Vec::new();
    walk(at, at, machine, 0, &mut roms);
    if roms.is_empty() {
        return None;
    }
    roms.sort_by(|a, b| {
        a.title
            .to_lowercase()
            .cmp(&b.title.to_lowercase())
            .then_with(|| a.path.cmp(&b.path))
    });

    Some(Console {
        key: folder.clone(),
        glyph: machine.map(|machine| machine.glyph.to_string()),
        title: machine.map_or(folder, |machine| machine.title.to_string()),
        core: None,
        incomplete: false,
        needs: Vec::new(),
        wanted: machine
            .map(|machine| machine.cores.iter().map(|core| core.to_string()).collect())
            .unwrap_or_default(),
        roms,
    })
}

/// Everything playable under one folder, flat.
fn walk(
    console: &Path,
    at: &Path,
    machine: Option<&'static Machine>,
    depth: usize,
    into: &mut Vec<Rom>,
) {
    if depth > DEPTH || into.len() >= CEILING {
        if into.len() >= CEILING {
            eprintln!(
                "{}: more than {CEILING} games; the rest are not listed",
                console.display()
            );
        }
        return;
    }
    let Ok(entries) = std::fs::read_dir(at) else {
        // A folder that cannot be read is one folder missing, not a failed
        // scan: a permission somewhere in a collection must not take the
        // console it is in off the bar.
        eprintln!("{}: cannot be read", at.display());
        return;
    };

    let mut files: Vec<PathBuf> = Vec::new();
    let mut folders: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if hidden(&path) {
            continue;
        }
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => folders.push(path),
            // A symlink is followed for what it is rather than for what it
            // points at: a link to a game is a game, and a link to a directory
            // is walked like any other. `is_dir` resolves it, which is the
            // behaviour wanted, and the depth ceiling is what stops a link that
            // points at its own parent.
            Ok(_) if path.is_dir() => folders.push(path),
            Ok(_) => files.push(path),
            Err(_) => {}
        }
    }
    files.sort();
    folders.sort();

    for path in playable(&files, machine) {
        if into.len() >= CEILING {
            break;
        }
        let Some(title) = title_of(&path) else {
            continue;
        };
        into.push(Rom {
            title,
            path: path.to_string_lossy().into_owned(),
            within: within(console, &path),
            // Filled in by `main` for the whole library at once, which is where
            // the cores are resolved and for the same reason: it is one read of
            // one listing per console, and asking it per game would be four
            // hundred reads of the same few thousand names.
            boxart: None,
            snap: None,
        });
    }

    for folder in folders {
        walk(console, &folder, machine, depth + 1, into);
    }
}

/// Which of one folder's files are games, with the pieces of a disc image left
/// out.
fn playable(files: &[PathBuf], machine: Option<&'static Machine>) -> Vec<PathBuf> {
    let mut spoken_for: BTreeSet<PathBuf> = BTreeSet::new();
    for file in files {
        let Some(extension) = extension(file) else {
            continue;
        };
        if !SHEETS.contains(&extension.as_str()) {
            continue;
        }
        // Everything of the same name is a piece of this one — `game.bin` under
        // `game.cue`.
        if let Some(stem) = file.file_stem() {
            for other in files {
                if other != file && other.file_stem() == Some(stem) {
                    spoken_for.insert(other.clone());
                }
            }
        }
        // And a playlist names its discs outright, which are not the same name
        // as it.
        if extension == "m3u" {
            spoken_for.extend(listed_by(file));
        }
    }

    files
        .iter()
        .filter(|file| !spoken_for.contains(*file))
        .filter(|file| extension(file).is_some_and(|ext| consoles::plays(machine, &ext)))
        .cloned()
        .collect()
}

/// The files a `.m3u` names, as paths in the folder it is in.
///
/// Relative entries are the ordinary case and absolute ones are allowed;
/// anything that is not a path this folder holds is simply not matched, which
/// is the same as not being named at all.
fn listed_by(playlist: &Path) -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string(playlist) else {
        return Vec::new();
    };
    let folder = playlist.parent().unwrap_or(Path::new("."));
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let named = Path::new(line);
            if named.is_absolute() {
                named.to_path_buf()
            } else {
                folder.join(named)
            }
        })
        .collect()
}

/// What the row says: the file's name without the extension.
fn title_of(at: &Path) -> Option<String> {
    let stem = at.file_stem()?.to_string_lossy().into_owned();
    let stem = stem.trim();
    (!stem.is_empty()).then(|| stem.to_string())
}

/// The folder under the console this was found in, where it was not the
/// console's own.
fn within(console: &Path, rom: &Path) -> Option<String> {
    let parent = rom.parent()?;
    if parent == console {
        return None;
    }
    Some(
        parent
            .strip_prefix(console)
            .unwrap_or(parent)
            .to_string_lossy()
            .into_owned(),
    )
}

fn extension(at: &Path) -> Option<String> {
    Some(at.extension()?.to_string_lossy().to_ascii_lowercase())
}

fn hidden(at: &Path) -> bool {
    at.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with('.'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch(name: &str) -> PathBuf {
        let at = std::env::temp_dir().join(format!("lxb-retroarch-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&at);
        fs::create_dir_all(&at).unwrap();
        at
    }

    fn touch(at: &Path, name: &str) {
        if let Some(parent) = at.join(name).parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(at.join(name), b"x").unwrap();
    }

    /// The example the whole feature was described with: one console with a
    /// game in it and one without, and only the first becomes a column.
    #[test]
    fn a_console_with_nothing_in_it_is_not_a_column() {
        let roms = scratch("empty");
        touch(&roms, "psp/t8.iso");
        fs::create_dir_all(roms.join("snes")).unwrap();

        let library = library(&roms);
        assert_eq!(library.consoles.len(), 1, "snes holds nothing");
        assert_eq!(library.consoles[0].title, "PlayStation Portable");
        assert_eq!(library.consoles[0].roms[0].title, "t8", "no extension");
        assert!(library.unreadable.is_none());

        let _ = fs::remove_dir_all(&roms);
    }

    /// A game kept in a folder of its own is still one game, listed under its
    /// own name.
    #[test]
    fn a_game_in_a_folder_of_its_own_is_one_row() {
        let roms = scratch("deep");
        touch(&roms, "ps2/God of War/gow.iso");
        touch(&roms, "ps2/ico.iso");

        let library = library(&roms);
        let titles: Vec<&str> = library.consoles[0]
            .roms
            .iter()
            .map(|rom| rom.title.as_str())
            .collect();
        assert_eq!(titles, vec!["gow", "ico"]);
        assert_eq!(
            library.consoles[0].roms[0].within.as_deref(),
            Some("God of War")
        );

        let _ = fs::remove_dir_all(&roms);
    }

    /// The pieces of a disc image are not games. A column offering `game.bin`
    /// beside `game.cue` is a column with a row in it that cannot be started.
    #[test]
    fn the_halves_of_a_disc_are_not_offered_as_games() {
        let roms = scratch("discs");
        touch(&roms, "psx/game.cue");
        touch(&roms, "psx/game.bin");
        touch(&roms, "psx/other.chd");
        touch(&roms, "psx/game.srm");

        let library = library(&roms);
        let titles: Vec<&str> = library.consoles[0]
            .roms
            .iter()
            .map(|rom| rom.title.as_str())
            .collect();
        assert_eq!(titles, vec!["game", "other"], "the cue, not its bin");

        let _ = fs::remove_dir_all(&roms);
    }

    /// A machine that reads discs and accepts a disc's data half has to accept
    /// the sheet that names it, or its shelf comes out empty.
    ///
    /// The two rules in [`playable`] meet here and cancel out. A `.bin` beside
    /// a `.cue` is spoken for and never offered, whichever console it belongs
    /// to; and a `.cue` on a shelf whose table has no `cue` in it is not a game
    /// either. A console with `bin` and no `cue` therefore turns the ordinary
    /// two-file dump into *nothing at all* — not a row that will not start,
    /// which somebody could at least see, but a shelf that is not on the bar.
    ///
    /// Which is what PlayStation 2 did. Both of its cores take `cue`, Play!
    /// does not take `bin` at all, and the row listed `bin` without it — so a
    /// Tekken Tag Tournament dumped the way that game comes was a PlayStation 2
    /// folder the shell said was empty.
    ///
    /// Narrowed to the machines that read discs, by `iso` or `chd` being on
    /// their list, because everywhere else a `.bin` is the whole game: a
    /// cartridge dump has no sheet to be half of, which is why Mega Drive and
    /// the Ataris carry `bin` and want no `cue`.
    #[test]
    fn a_disc_that_comes_in_two_files_has_a_shelf_to_stand_on() {
        for machine in consoles::CONSOLES {
            let has = |what: &str| machine.extensions.contains(&what);
            if !(has("iso") || has("chd")) {
                continue;
            }
            if !(has("bin") || has("img")) {
                continue;
            }
            assert!(
                machine.extensions.iter().any(|ext| SHEETS.contains(ext)),
                "{} reads discs and accepts a disc's data half, so it has to accept \
                 a sheet naming it — its list is {:?} and none of {SHEETS:?} is on it",
                machine.title,
                machine.extensions
            );
        }
    }

    /// And the PlayStation 2 dump that found it: two files in a folder of their
    /// own, which is how that game is published.
    #[test]
    fn a_playstation_2_disc_in_two_files_is_one_game() {
        let roms = scratch("ps2-cue");
        touch(
            &roms,
            "ps2/Tekken Tag Tournament (USA)/Tekken Tag Tournament (USA).cue",
        );
        touch(
            &roms,
            "ps2/Tekken Tag Tournament (USA)/Tekken Tag Tournament (USA).bin",
        );

        let library = library(&roms);
        let titles: Vec<&str> = library.consoles[0]
            .roms
            .iter()
            .map(|rom| rom.title.as_str())
            .collect();
        assert_eq!(
            titles,
            vec!["Tekken Tag Tournament (USA)"],
            "the cue, once, and not its bin"
        );

        let _ = fs::remove_dir_all(&roms);
    }

    /// And a playlist stands for the discs it names, which do not share its
    /// name.
    #[test]
    fn a_playlist_stands_for_its_discs() {
        let roms = scratch("m3u");
        touch(&roms, "psx/Final Fantasy VII (Disc 1).chd");
        touch(&roms, "psx/Final Fantasy VII (Disc 2).chd");
        fs::write(
            roms.join("psx/Final Fantasy VII.m3u"),
            "Final Fantasy VII (Disc 1).chd\nFinal Fantasy VII (Disc 2).chd\n",
        )
        .unwrap();

        let library = library(&roms);
        let titles: Vec<&str> = library.consoles[0]
            .roms
            .iter()
            .map(|rom| rom.title.as_str())
            .collect();
        assert_eq!(titles, vec!["Final Fantasy VII"]);

        let _ = fs::remove_dir_all(&roms);
    }

    /// A folder this helper has never heard of is still somebody's console.
    #[test]
    fn an_unknown_folder_keeps_its_own_name() {
        let roms = scratch("unknown");
        touch(&roms, "pico8/celeste.p8");

        let library = library(&roms);
        assert_eq!(library.consoles[0].title, "pico8");
        assert_eq!(library.consoles[0].key, "pico8");
        assert!(library.consoles[0].wanted.is_empty(), "nothing runs it");
        assert_eq!(library.consoles[0].roms[0].title, "celeste");

        let _ = fs::remove_dir_all(&roms);
    }

    /// A folder that is not there is said to be unreadable rather than being
    /// answered as a library with nothing in it.
    #[test]
    fn a_folder_that_is_gone_says_so() {
        let library = library(Path::new("/nowhere/at/all"));
        assert!(library.unreadable.is_some());
        assert!(library.consoles.is_empty());
    }
}
