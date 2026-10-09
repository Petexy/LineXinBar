//! Getting a Windows game into a prefix, and finding out what to run
//! afterwards.
//!
//! Two shapes arrive through **Run .exe**, and the flow must tell them apart
//! without asking somebody a question they have no way to answer:
//!
//! - **An installer.** It is run inside a fresh prefix, it writes the game
//!   somewhere under `drive_c`, and it exits. What to play afterwards is
//!   *not* the installer, and it is not the first executable found either —
//!   a setup leaves uninstallers, crash handlers, redistributables and
//!   DirectX stubs lying about. So the prefix is searched afterwards and the
//!   candidates are ranked, and the shell confirms one.
//! - **A portable game**, which is a folder somebody already has. Copying
//!   only the executable out of it gives a game with no data files, so what
//!   is imported is the folder — and the folder somebody points at is the
//!   one they say holds the whole game.
//!
//! ## Why the ranking is not a guess
//!
//! It is a ranking, not a choice: [`candidates`] puts the likeliest first
//! and hands over all of them, and a person picks. Everything it knows is
//! written down in [`rank`] — how deep in `drive_c` a file is, whether its
//! folder is one Windows puts system things in, whether its name is one of
//! the words setups leave behind, and how large it is, because the game is
//! usually the biggest executable somebody installed.

use std::path::{Path, PathBuf};

/// Where a Windows installation starts, inside a prefix.
const DRIVE_C: &str = "pfx/drive_c";

/// Folders under `drive_c` that hold Windows itself and the things every
/// installer drops beside it. Nothing anybody plays lives in them.
const NOT_THE_GAME: &[&str] = &[
    "windows",
    "programdata",
    "users",
    "openxr",
    "$recycle.bin",
    "system volume information",
];

/// Names that belong to a setup rather than to a game.
const NOT_A_GAME_EXE: &[&str] = &[
    "unins",
    "uninstall",
    "setup",
    "install",
    "vcredist",
    "dxsetup",
    "dotnetfx",
    "directx",
    "crashhandler",
    "crashreport",
    "crashpad_handler",
    "ue4prereqsetup",
    "ueprereqsetup",
    "oalinst",
    "redist",
    "dxwebsetup",
    "7z",
];

/// One executable found inside a prefix, and how likely it is to be the
/// game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// Where it is, relative to the game's own directory — the same form
    /// `lxb.json` keeps, so moving the games folder breaks nothing.
    pub at: String,
    /// What to call it on the row that offers it.
    ///
    /// A program's name without `.exe`, because that is what the game is
    /// called — but the *whole* file name for anything else, because the
    /// ending is then the only thing telling two rows apart. A great many
    /// games ship `Game.exe` beside a `Game.bat` that starts it with
    /// arguments, and a list offering **Game** twice is a list nobody can
    /// choose from — which would break the one promise this list makes, that
    /// it ranks rather than decides.
    pub name: String,
    pub bytes: u64,
}

/// Every executable inside a prefix that could be the game, likeliest
/// first.
///
/// Bounded deliberately: a prefix with a hundred thousand files in it is a
/// prefix somebody has installed a great deal into, and walking all of it
/// to offer a list nobody will read to the end of is work for nothing.
pub fn candidates(game_dir: &Path) -> Vec<Candidate> {
    // The walk answers relative to whatever it was pointed at, so the
    // prefix's own `pfx/drive_c` is put back on here. It belongs to this
    // question — "where inside the game is it" — and not to the walk, which
    // is also asked about folders that are not prefixes at all. It used to
    // be written on inside the walk, and `best_in` then answered with paths
    // that did not exist.
    ranked(&game_dir.join(DRIVE_C))
        .into_iter()
        .map(|found| Candidate {
            at: format!("{DRIVE_C}/{}", found.at),
            ..found
        })
        .collect()
}

/// The likeliest program in a folder that is not a prefix.
///
/// For the moment before an import: the folder is still where somebody
/// keeps it, nothing has been copied, and the question is what this game is
/// going to be called. See `pe::product_name`, which is asked of whatever
/// this answers.
pub fn best_in(folder: &Path) -> Option<Candidate> {
    ranked(folder).into_iter().next()
}

/// Every executable under `root`, likeliest first.
fn ranked(root: &Path) -> Vec<Candidate> {
    let mut found: Vec<(i64, Candidate)> = Vec::new();
    walk(root, root, 0, &mut found);
    found.sort_by(|(a_rank, a), (b_rank, b)| b_rank.cmp(a_rank).then_with(|| a.at.cmp(&b.at)));
    found.into_iter().map(|(_, found)| found).collect()
}

/// How far down to look. A game's own executable is near the top of the
/// folder somebody installed it into; eight levels is well past that and
/// stops a prefix full of a Unity project being walked to the bottom.
const DEEP_ENOUGH: usize = 8;

/// How many executables are worth offering at all.
const ENOUGH: usize = 200;

fn walk(root: &Path, at: &Path, depth: usize, found: &mut Vec<(i64, Candidate)>) {
    if depth > DEEP_ENOUGH || found.len() >= ENOUGH {
        return;
    }
    let Ok(listing) = std::fs::read_dir(at) else {
        return;
    };
    for entry in listing.filter_map(Result::ok) {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        // Never followed: a prefix's `drive_c/users/steamuser` is a nest of
        // links back out to the real home, and following them would walk
        // somebody's whole disk and offer them their own programs.
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            if depth == 0 && NOT_THE_GAME.contains(&name.as_str()) {
                continue;
            }
            walk(root, &path, depth + 1, found);
            continue;
        }
        if !could_be_a_game(&path) {
            continue;
        }
        let bytes = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        let Some(name) = shown_as(&path) else {
            continue;
        };
        found.push((
            rank(relative, &name, bytes),
            Candidate {
                at: relative.display().to_string(),
                name,
                bytes,
            },
        ));
    }
}

/// The endings a *game* can be, case-insensitively — a disk written by
/// Windows holds `GAME.EXE` as readily as `game.exe`.
///
/// `.exe` is nearly always the answer. `.bat` and `.cmd` are here because a
/// game that is really a launcher script is a real thing — a pack built
/// round an emulator or a Java program ships `start.bat` and nothing else —
/// and a folder imported with one of those in it would otherwise land as a
/// row nobody can press. They rank last anyway, being a few hundred bytes
/// against a game's megabytes, so where there is an `.exe` beside one it
/// wins.
///
/// **`.msi` is deliberately not here**, although the walk that picks a
/// program to *run* does offer one. An installer package is never the game:
/// it is the thing that puts the game there, and `msiexec` is what reads
/// it. Offering one as the answer to "what does this play" would be
/// offering to install the game again every time somebody pressed it.
const COULD_BE_A_GAME: &[&str] = &["exe", "bat", "cmd"];

/// What one of these is called on the row that offers it — see
/// [`Candidate::name`].
fn shown_as(at: &Path) -> Option<String> {
    let ends_in_exe = at
        .extension()
        .and_then(|end| end.to_str())
        .is_some_and(|end| end.eq_ignore_ascii_case("exe"));
    match ends_in_exe {
        true => at.file_stem()?.to_str().map(str::to_string),
        false => at.file_name()?.to_str().map(str::to_string),
    }
}

/// Whether this file could be the game itself.
pub fn could_be_a_game(at: &Path) -> bool {
    at.extension()
        .and_then(|end| end.to_str())
        .is_some_and(|end| {
            COULD_BE_A_GAME
                .iter()
                .any(|known| end.eq_ignore_ascii_case(known))
        })
}

/// How likely one executable is to be the game.
///
/// Every term here is a reason, and none of them is decisive on its own —
/// which is the point: this orders a list that a person then chooses from,
/// and a confident wrong answer would be worse than an ordered one.
fn rank(relative: &Path, name: &str, bytes: u64) -> i64 {
    let mut score = 0i64;

    let lower = name.to_lowercase();
    if NOT_A_GAME_EXE.iter().any(|word| lower.contains(word)) {
        score -= 1000;
    }

    // A game's executable sits in the folder it was installed into, not
    // eight levels down beside a redistributable.
    let depth = relative.components().count() as i64;
    score -= depth * 40;

    // `Program Files` is where an installer puts a game; the rest of the
    // disk is where it puts everything else.
    let path = relative.to_string_lossy().to_lowercase();
    if path.starts_with("program files") {
        score += 60;
    }
    if path.contains("/redist") || path.contains("\\redist") {
        score -= 200;
    }

    // And the game is nearly always the biggest thing the installer wrote.
    // Logarithmic, so a hundred-megabyte executable beats a one-megabyte
    // one without a one-gigabyte launcher-updater beating everything.
    score += (bytes.max(1) as f64).log2() as i64 * 6;

    score
}

/// Copy a folder into a prefix, for a game that is already built.
///
/// Into `drive_c/Games/<name>`, which is a folder a person opening the
/// prefix will understand, and which is not one of the places Windows keeps
/// its own things. Answers how many bytes were copied.
///
/// `watch` is told how far it has got, 0 to 1.
pub fn import(
    game_dir: &Path,
    from: &Path,
    name: &str,
    watch: &mut dyn FnMut(f32),
) -> std::io::Result<PathBuf> {
    let into = game_dir.join(DRIVE_C).join("Games").join(name);
    std::fs::create_dir_all(&into)?;
    let total = weigh(from).max(1);
    let mut done = 0u64;
    let mut told = -1i32;
    copy_into(from, &into, total, &mut done, &mut told, watch)?;
    Ok(into)
}

/// How much a folder holds, in bytes. Links are counted as nothing rather
/// than followed, for the reason [`walk`] does not follow them.
fn weigh(at: &Path) -> u64 {
    let Ok(listing) = std::fs::read_dir(at) else {
        return 0;
    };
    listing
        .filter_map(Result::ok)
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_symlink() => 0,
            Ok(kind) if kind.is_dir() => weigh(&entry.path()),
            Ok(kind) if kind.is_file() => entry.metadata().map(|meta| meta.len()).unwrap_or(0),
            _ => 0,
        })
        .sum()
}

fn copy_into(
    from: &Path,
    into: &Path,
    total: u64,
    done: &mut u64,
    told: &mut i32,
    watch: &mut dyn FnMut(f32),
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(from)?.filter_map(Result::ok) {
        let kind = entry.file_type()?;
        let target = into.join(entry.file_name());
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            std::fs::create_dir_all(&target)?;
            copy_into(&entry.path(), &target, total, done, told, watch)?;
            continue;
        }
        if !kind.is_file() {
            continue;
        }
        let copied = std::fs::copy(entry.path(), &target)?;
        *done += copied;
        let percent = ((*done).min(total) * 100 / total) as i32;
        if percent != *told {
            *told = percent;
            watch(percent as f32 / 100.0);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exe(at: &Path, bytes: usize) {
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        std::fs::write(at, vec![0u8; bytes]).unwrap();
    }

    /// A prefix after a real-shaped install: the game, its uninstaller, the
    /// redistributables every setup drops, and Windows' own things.
    #[test]
    fn the_game_is_offered_before_the_things_a_setup_left_behind() {
        let dir = tempfile::tempdir().unwrap();
        let c = dir.path().join(DRIVE_C);
        exe(
            &c.join("Program Files/Stardew Valley/Stardew Valley.exe"),
            40_000,
        );
        exe(&c.join("Program Files/Stardew Valley/unins000.exe"), 70_000);
        exe(
            &c.join("Program Files/Stardew Valley/redist/vcredist_x64.exe"),
            90_000,
        );
        exe(&c.join("windows/system32/notepad.exe"), 90_000);
        exe(&c.join("users/steamuser/Temp/setup.exe"), 90_000);

        let found = candidates(dir.path());
        let names: Vec<&str> = found.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names.first(),
            Some(&"Stardew Valley"),
            "the game comes first: {names:?}"
        );
        assert!(
            !names.contains(&"notepad"),
            "Windows' own folder is not searched: {names:?}"
        );
        assert!(
            !names.contains(&"setup"),
            "nor is the user folder a setup unpacked itself into: {names:?}"
        );
        // The uninstaller and the redistributable are still offered, last:
        // this ranks, it does not decide.
        assert!(names.contains(&"unins000"), "{names:?}");
        assert!(names.contains(&"vcredist_x64"), "{names:?}");
        assert_eq!(names.last(), Some(&"vcredist_x64"), "{names:?}");
    }

    /// `GAME.EXE` is as common on a Windows disk as `game.exe`, which is
    /// what the plan means by accepting the extension case-insensitively.
    #[test]
    fn the_extension_is_read_whatever_case_it_is_written_in() {
        assert!(could_be_a_game(Path::new("/a/GAME.EXE")));
        assert!(could_be_a_game(Path::new("/a/game.Exe")));
        assert!(!could_be_a_game(Path::new("/a/game.exe.bak")));
        assert!(!could_be_a_game(Path::new("/a/game.dll")));
    }

    /// A pack built round an emulator or a Java program ships `start.bat`
    /// and nothing else, and a folder imported with one of those in it used
    /// to land as a row nobody could press. An installer package is a
    /// different matter: it is never the game, it is what puts the game
    /// there, so it is offered to *run* and never to play.
    ///
    /// And a script keeps its ending on the row, because the moment more
    /// than one ending can be offered the ending is the only thing telling
    /// `Game.exe` from the `Game.bat` beside it.
    #[test]
    fn a_launcher_script_can_be_the_game_and_an_installer_package_never_is() {
        let dir = tempfile::tempdir().unwrap();
        let c = dir.path().join(DRIVE_C);
        exe(&c.join("Games/Some Pack/start.bat"), 400);
        exe(&c.join("Games/Some Pack/setup.msi"), 90_000);

        let found = candidates(dir.path());
        let names: Vec<&str> = found.iter().map(|one| one.name.as_str()).collect();
        assert_eq!(
            names,
            ["start.bat"],
            "and it keeps its ending, which is all that tells it from a              Game.exe beside it: {names:?}"
        );

        // And where there is a real program beside the script, the program
        // is the game: a launcher is a few hundred bytes and a game is not.
        exe(&c.join("Games/Some Pack/ThePack.exe"), 400_000);
        let found = candidates(dir.path());
        let names: Vec<&str> = found.iter().map(|one| one.name.as_str()).collect();
        assert_eq!(names, ["ThePack", "start.bat"], "{names:?}");
    }

    /// A prefix's `users` folder is a nest of links back out to the real
    /// home. Following one would walk the whole disk and offer somebody
    /// their own programs as the game they just installed.
    #[test]
    fn a_link_out_of_the_prefix_is_never_followed() {
        let dir = tempfile::tempdir().unwrap();
        let c = dir.path().join(DRIVE_C);
        let elsewhere = dir.path().join("the-real-home");
        exe(&elsewhere.join("Documents/private.exe"), 10);
        std::fs::create_dir_all(c.join("Program Files")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, c.join("Program Files/linked")).unwrap();
        exe(&c.join("Program Files/Game/game.exe"), 100);

        let found = candidates(dir.path());
        let names: Vec<&str> = found.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["game"], "{names:?}");
    }

    /// A portable game is imported whole: copying only the executable gives
    /// a game with no data files, which starts and then stops.
    #[test]
    fn a_portable_game_is_imported_with_everything_it_needs() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("Portable Game");
        exe(&from.join("Game.exe"), 2048);
        std::fs::create_dir_all(from.join("Data/Textures")).unwrap();
        std::fs::write(from.join("Data/Textures/sky.png"), vec![7u8; 4096]).unwrap();
        std::fs::write(from.join("readme.txt"), "hello").unwrap();

        let game_dir = dir.path().join("games/portable-game");
        let mut seen: Vec<f32> = Vec::new();
        let into = import(&game_dir, &from, "Portable Game", &mut |done| {
            seen.push(done)
        })
        .unwrap();

        assert!(into.join("Game.exe").is_file());
        assert!(into.join("Data/Textures/sky.png").is_file());
        assert_eq!(
            std::fs::read_to_string(into.join("readme.txt")).unwrap(),
            "hello"
        );
        assert_eq!(seen.last(), Some(&1.0), "it says when it has finished");

        // And it is then found as the game, under the folder it was put in.
        let found = candidates(&game_dir);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].at, "pfx/drive_c/Games/Portable Game/Game.exe");
    }

    /// Nothing in an empty prefix, rather than an error.
    #[test]
    fn a_prefix_with_nothing_in_it_offers_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(candidates(dir.path()).is_empty());
    }
}
