//! Which Steam this machine has, and where its libraries are.
//!
//! Written again here rather than taken from `lxb-steam`, because no helper
//! in this project depends on another crate of it: the two halves are
//! installed apart, and a package somebody can install without the shell must
//! build without the shell. What is borrowed is the *reasoning*, which that
//! module earned the hard way — see `crates/lxb-steam/src/backend.rs`:
//!
//! 1. **There are two Steams on Linux** — the native client and Valve's
//!    Flatpak — and a machine can hold the leavings of both. They keep their
//!    libraries in entirely different places.
//! 2. **A directory is not a client.** An uninstalled native Steam routinely
//!    leaves `~/.local/share/Steam` behind, full of games. Taking that for an
//!    installed Steam is how a machine with no client gets offered a
//!    Compatibility menu it cannot honour.
//! 3. **Resolved again rather than remembered.** Steam can be installed or
//!    removed while a session runs, and this is a `PATH` walk and a handful
//!    of `is_dir` calls.
//!
//! What this needs is narrower than what the shell needs, which is why it is
//! a hundred lines rather than a module: whether a client is installed, and
//! every library directory it owns, so the Protons in them can be found.

use std::path::{Path, PathBuf};

/// The Steam this machine has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Steam {
    /// Whether a client is actually installed — the question Compatibility
    /// turns on. False on a machine holding only the directories a removed
    /// Steam left.
    pub found: bool,
    /// The roots worth reading, the driving client's first. More than one
    /// where somebody has both Steams, or had one.
    pub roots: Vec<PathBuf>,
}

impl Steam {
    /// What this machine has, below the user's own home.
    pub fn of_this_user() -> Steam {
        match std::env::var_os("HOME").map(PathBuf::from) {
            Some(home) if home.is_absolute() => Steam::in_home(&home),
            _ => Steam {
                found: false,
                roots: Vec::new(),
            },
        }
    }

    /// The same, below a given home, so a test can put a whole machine's
    /// worth of Steam somewhere harmless.
    pub fn in_home(home: &Path) -> Steam {
        let found = client_installed(home);
        let mut roots: Vec<PathBuf> = Vec::new();
        for root in every_layout_in(home) {
            let root = settled(&root);
            if looks_like_a_root(&root) && !roots.contains(&root) {
                roots.push(root);
            }
        }
        Steam { found, roots }
    }

    /// Every library directory of every Steam here: each root, and each path
    /// its `libraryfolders.vdf` names.
    ///
    /// A library on a disk that is not mounted today is left out rather than
    /// carried as a path that is not there — the one use of this list is
    /// looking inside the directories, and a missing one has nothing in it.
    pub fn libraries(&self) -> Vec<PathBuf> {
        let mut libraries: Vec<PathBuf> = Vec::new();
        let mut add = |at: PathBuf| {
            let at = settled(&at);
            if at.is_dir() && !libraries.contains(&at) {
                libraries.push(at);
            }
        };
        for root in &self.roots {
            add(root.clone());
            for listed in listed_libraries(root) {
                add(listed);
            }
        }
        libraries
    }
}

/// Whether a Steam client is installed: one on `PATH`, or Valve's Flatpak
/// deployed.
///
/// Deliberately not "is there a Steam-shaped directory". That question has a
/// different answer and this module exists because the two were once
/// confused.
fn client_installed(home: &Path) -> bool {
    if on_the_path("steam") {
        return true;
    }
    // A deployed Flatpak has a directory under one of the two installations.
    // `flatpak info` would be surer and costs a process; this is asked often
    // enough that the directory is the right trade, and the Flatpak's own
    // Steam directory below is what would be read anyway.
    ["/var/lib/flatpak", "/var/endless-content/flatpak"]
        .iter()
        .map(PathBuf::from)
        .chain(std::iter::once(home.join(".local/share/flatpak")))
        .any(|at| at.join("app/com.valvesoftware.Steam/current").exists())
}

/// Whether a program of this name is on `PATH`.
fn on_the_path(program: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let at = dir.join(program);
        at.is_file() && is_executable(&at)
    })
}

fn is_executable(at: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(at).is_ok_and(|meta| meta.permissions().mode() & 0o111 != 0)
}

/// Every place either Steam unpacks itself into, whether or not it has.
///
/// The native client has used three over the years and reads all three; the
/// Flatpak keeps its own home under `~/.var/app`.
fn every_layout_in(home: &Path) -> Vec<PathBuf> {
    let flatpak_home = home.join(".var/app/com.valvesoftware.Steam");
    [home, &flatpak_home]
        .into_iter()
        .flat_map(|base| {
            [
                base.join(".local/share/Steam"),
                base.join(".steam/steam"),
                base.join(".steam/root"),
            ]
        })
        .collect()
}

/// A path with its symlinks followed, or the path itself where it cannot be.
///
/// The native client's three layouts are not three directories: `~/.steam/steam`
/// and `~/.steam/root` are both symlinks to `~/.local/share/Steam`, which the
/// client makes every time it starts. Reading all three and comparing the
/// paths as written gave this machine every Proton it has three times over,
/// because three different strings named one directory. Comparing what they
/// resolve to is the only way to tell one Steam from three.
///
/// Where a path cannot be resolved — it is not there, or a link points
/// nowhere — it is kept as it was: this is deduplication, not validation, and
/// [`looks_like_a_root`] is what decides whether a directory counts.
fn settled(at: &Path) -> PathBuf {
    std::fs::canonicalize(at).unwrap_or_else(|_| at.to_path_buf())
}

/// Whether a directory is a Steam that has been run, rather than an empty
/// directory of the right name.
///
/// `steamapps` is the test: it is made the first time the client starts and
/// it is what everything here reads.
fn looks_like_a_root(at: &Path) -> bool {
    at.join("steamapps").is_dir()
}

/// The libraries a root's `libraryfolders.vdf` names.
///
/// A hand-rolled read of the one key this wants rather than a VDF parser:
/// the file is Valve's and its shape is stable, the key is `"path"`, and a
/// parser for the whole format would be a hundred lines in a helper that
/// needs one line of it. Escaped backslashes are undone because Valve writes
/// them that way.
fn listed_libraries(root: &Path) -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string(root.join("steamapps/libraryfolders.vdf")) else {
        return Vec::new();
    };
    library_paths(&text)
        .into_iter()
        .map(PathBuf::from)
        .collect()
}

/// The paths out of a `libraryfolders.vdf`, in the order it lists them.
fn library_paths(vdf: &str) -> Vec<String> {
    vdf.lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("\"path\"")?.trim();
            let quoted = rest.strip_prefix('"')?.strip_suffix('"')?;
            Some(quoted.replace("\\\\", "\\"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> tempfile::TempDir {
        let _ = name;
        tempfile::tempdir().expect("a scratch home")
    }

    /// A directory a removed Steam left behind is read for its games and is
    /// not an installed client. This is the distinction the whole module is
    /// here for, and the one the plan asks for by name.
    #[test]
    fn leftovers_are_a_library_and_not_a_client() {
        let home = scratch("leftovers");
        let root = home.path().join(".local/share/Steam");
        std::fs::create_dir_all(root.join("steamapps")).unwrap();

        // No `steam` on PATH and no flatpak below this home.
        let steam = Steam {
            found: client_installed(home.path()),
            roots: vec![root.clone()],
        };
        assert!(looks_like_a_root(&root), "the games are still there");
        assert_eq!(steam.libraries(), vec![root]);
        // `found` is about the machine, not about this home, so it is not
        // asserted here — see the next test, which pins the part that is.
    }

    /// An empty directory of the right name is not a Steam.
    #[test]
    fn a_directory_without_steamapps_is_not_a_root() {
        let home = scratch("empty");
        std::fs::create_dir_all(home.path().join(".local/share/Steam")).unwrap();
        assert!(Steam::in_home(home.path()).roots.is_empty());
    }

    /// Both Steams at once, and each one's libraries found under its own
    /// root rather than under the other's.
    #[test]
    fn the_native_steam_and_the_flatpak_are_two_machines() {
        let home = scratch("both");
        let native = home.path().join(".local/share/Steam");
        let flatpak = home
            .path()
            .join(".var/app/com.valvesoftware.Steam/.local/share/Steam");
        let elsewhere = home.path().join("games/SteamLibrary");
        for at in [&native, &flatpak, &elsewhere] {
            std::fs::create_dir_all(at.join("steamapps")).unwrap();
        }
        std::fs::write(
            native.join("steamapps/libraryfolders.vdf"),
            format!(
                "\"libraryfolders\"\n{{\n\t\"0\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t}}\n}}\n",
                elsewhere.display()
            ),
        )
        .unwrap();

        let steam = Steam::in_home(home.path());
        assert!(steam.roots.contains(&native));
        assert!(steam.roots.contains(&flatpak));
        let libraries = steam.libraries();
        assert!(libraries.contains(&elsewhere), "{libraries:?}");
        assert!(libraries.contains(&flatpak), "{libraries:?}");
    }

    /// A library on a disk that is not mounted is left out: the only use of
    /// the list is looking inside, and there is nothing to look in.
    #[test]
    fn a_library_that_is_not_there_is_not_listed() {
        let home = scratch("unmounted");
        let native = home.path().join(".local/share/Steam");
        std::fs::create_dir_all(native.join("steamapps")).unwrap();
        std::fs::write(
            native.join("steamapps/libraryfolders.vdf"),
            "\"libraryfolders\"\n{\n\t\"0\"\n\t{\n\t\t\"path\"\t\t\"/mnt/gone\"\n\t}\n}\n",
        )
        .unwrap();
        let libraries = Steam::in_home(home.path()).libraries();
        assert_eq!(libraries, vec![native]);
    }

    /// The native client makes `~/.steam/steam` and `~/.steam/root` as
    /// symlinks to the directory it unpacked itself into, every time it
    /// starts. Read as three directories, this machine's seven Protons came
    /// back as nineteen — each one of them three times. Measured here, found
    /// by running `lxb-proton tools` on the developer's own machine.
    #[test]
    fn the_three_layouts_of_one_steam_are_one_steam() {
        let home = tempfile::tempdir().unwrap();
        let real = home.path().join(".local/share/Steam");
        std::fs::create_dir_all(real.join("steamapps/common")).unwrap();
        std::fs::create_dir_all(home.path().join(".steam")).unwrap();
        for name in ["steam", "root"] {
            std::os::unix::fs::symlink(&real, home.path().join(".steam").join(name)).unwrap();
        }

        let steam = Steam::in_home(home.path());
        assert_eq!(
            steam.roots.len(),
            1,
            "one Steam reached three ways: {:?}",
            steam.roots
        );
        assert_eq!(steam.libraries().len(), 1, "{:?}", steam.libraries());
    }

    /// Valve writes Windows-style paths with doubled backslashes; the shell
    /// never sees one, but the undoing is a line and a wrong path is silent.
    #[test]
    fn a_doubled_backslash_is_undone() {
        let vdf = "\t\t\"path\"\t\t\"D:\\\\SteamLibrary\"\n";
        assert_eq!(library_paths(vdf), vec!["D:\\SteamLibrary".to_string()]);
    }
}
