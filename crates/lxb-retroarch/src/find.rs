//! Whether this machine has a RetroArch, which one it would use, and where the
//! cores it can load are.
//!
//! Three places are looked in, in this order, and the order is the whole of the
//! policy: **the distribution's own package first**, then a flatpak this user
//! installed, then a flatpak somebody installed for the machine. A native
//! package is preferred because it is not in a sandbox — it can open a ROM
//! wherever the user keeps one, and a flatpak can only open what its filesystem
//! permissions reach, which for the Flathub build is this user's home directory
//! and no further. Between the two flatpaks the user's own comes first because
//! that is the one this helper installs, so a machine that has both is a
//! machine where somebody deliberately added one.
//!
//! ## Why a core has two paths
//!
//! A core is a `.so` this helper has to *find* on the disk and RetroArch has to
//! *load* by name on its command line, and for a flatpak those are two
//! different spellings of one file: the app's own files are under
//! `/var/lib/flatpak/app/…/files` out here and mounted at `/app` in there. Pass
//! the outside spelling to `-L` and the core is simply not there.
//!
//! The exception, and it is worth knowing about because it looks like a
//! mistake: cores the user downloaded through RetroArch's own updater land
//! under `~/.var/app/org.libretro.RetroArch`, and flatpak binds that directory
//! into the sandbox **at the same path**. So those two spellings are equal, and
//! the pair below is not redundant — it is one case where the two halves happen
//! to agree.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::report::{Core, Installation, Kind};

/// The application id this integration is about, on Flathub and everywhere
/// else.
pub const FLATPAK_ID: &str = "org.libretro.RetroArch";

/// What a libretro core's file is called, after its name.
pub const CORE_SUFFIX: &str = "_libretro.so";

/// The RetroArch this machine would use, or `None` for one that has none.
pub fn installation() -> Option<Installation> {
    system()
        .or_else(|| flatpak(Kind::FlatpakUser))
        .or_else(|| flatpak(Kind::FlatpakSystem))
}

/// The distribution's own RetroArch, if `retroarch` is on the path.
fn system() -> Option<Installation> {
    let program = on_path("retroarch")?;
    Some(Installation {
        kind: Kind::System,
        command: vec![program.to_string_lossy().into_owned()],
        // `retroarch --version` prints one line and exits. A build that
        // refuses to say is still a RetroArch; this is for the log.
        version: first_line(Command::new(&program).arg("--version")),
    })
}

/// A RetroArch flatpak in one of the two installations.
fn flatpak(kind: Kind) -> Option<Installation> {
    let scope = scope(kind)?;
    // `flatpak info` answers with the deploy directory and fails outright for
    // an application that is not installed, which is both questions at once.
    deploy(scope)?;
    let command = vec![
        "flatpak".to_string(),
        "run".to_string(),
        scope.to_string(),
        FLATPAK_ID.to_string(),
    ];
    Some(Installation {
        kind,
        command,
        version: flatpak_version(scope),
    })
}

/// What the installed flatpak calls itself.
///
/// Out of the column listing rather than `flatpak info`, which prints a labelled
/// block whose field order is not promised. There is no `--show-version`; the
/// two-column listing is the one form that answers this in a shape worth
/// parsing, and it is the same form `lxb-desktop`'s `appinfo` reads for the
/// same reason.
fn flatpak_version(scope: &str) -> Option<String> {
    let listing = first_output(Command::new("flatpak").args([
        "list",
        scope,
        "--app",
        "--columns=application,version",
    ]))?;
    listing.lines().find_map(|line| {
        let (id, version) = line.split_once('\t')?;
        (id.trim() == FLATPAK_ID).then(|| version.trim().to_string())
    })
}

/// The flag that names one of the two flatpak installations, for the kinds that
/// are one.
fn scope(kind: Kind) -> Option<&'static str> {
    match kind {
        Kind::FlatpakUser => Some("--user"),
        Kind::FlatpakSystem => Some("--system"),
        Kind::System => None,
    }
}

/// Where flatpak has put the application's own files, out here.
fn deploy(scope: &str) -> Option<PathBuf> {
    let at =
        first_line(Command::new("flatpak").args(["info", scope, "--show-location", FLATPAK_ID]))?;
    let at = PathBuf::from(at);
    at.is_dir().then_some(at)
}

/// Where this installation keeps its own configuration.
///
/// The same directory the cores go under, one level up: `~/.config/retroarch`
/// for a distribution package, and `~/.var/app/<id>/config/retroarch` for a
/// flatpak — which is bound into the sandbox at the path it has out here, and
/// is therefore the one place the shell can put a file and name it on
/// RetroArch's command line.
pub fn config_dir(installation: &Installation) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    Some(match installation.kind {
        Kind::System => home.join(".config/retroarch"),
        Kind::FlatpakUser | Kind::FlatpakSystem => home
            .join(".var/app")
            .join(FLATPAK_ID)
            .join("config/retroarch"),
    })
}

/// Where this installation keeps libretro's own descriptions of its cores.
///
/// One `<core>_libretro.info` per core, maintained by libretro and shipped with
/// RetroArch, and the only place on this machine that says what a core needs
/// beside itself — see [`crate::firmware`]. Read out here and never named on a
/// command line, so unlike a core it has one spelling and not two.
///
/// `None` where the files are not on the disk, which is not an error: a
/// RetroArch installed without them is one this helper simply has nothing to
/// say about, and saying nothing is the same answer it gives for a core that
/// needs nothing.
pub fn info_dir(installation: &Installation) -> Option<PathBuf> {
    let places: Vec<PathBuf> = match installation.kind {
        Kind::System => [
            "/usr/share/libretro/info",
            "/usr/share/retroarch/info",
            "/usr/local/share/libretro/info",
        ]
        .iter()
        .map(PathBuf::from)
        .collect(),
        kind @ (Kind::FlatpakUser | Kind::FlatpakSystem) => scope(kind)
            .and_then(deploy)
            .map(|at| vec![at.join("files/share/libretro/info")])
            .unwrap_or_default(),
    };
    places.into_iter().find(|at| at.is_dir())
}

/// Whether a core that is on the disk can actually be loaded here.
///
/// A core downloading cleanly is not the same as a core that runs. libretro's
/// build server compiles against a general-purpose Linux, and RetroArch's
/// flatpak runs against a runtime that is not one — so a core can arrive whole,
/// pass every check this helper makes, and then fail in RetroArch's dynamic
/// linker with a message that goes to a log nobody is reading.
///
/// Answered by `ldd` **in the environment the core will be loaded in** — inside
/// the sandbox for a flatpak, out here for a distribution package — because
/// anything else is this helper guessing at a linker's search path, and the
/// whole point is that the two environments differ.
///
/// One subprocess, and for a flatpak a whole sandbox start, so this is for the
/// moment a core is installed and never for a scan: a library of forty consoles
/// would otherwise open forty sandboxes to draw its rows.
pub fn loads(installation: &Installation, at: &Path) -> Result<(), String> {
    // The path is the same inside and out, which is true of every core this
    // helper installs and is why this can be checked at all — see the module
    // note on a core's two spellings.
    let mut command = match installation.kind {
        Kind::System => {
            let mut command = Command::new("ldd");
            command.arg(at);
            command
        }
        kind @ (Kind::FlatpakUser | Kind::FlatpakSystem) => {
            let Some(scope) = scope(kind) else {
                return Ok(());
            };
            let mut command = Command::new("flatpak");
            command.arg("run").arg(scope);
            command.arg("--command=ldd").arg(FLATPAK_ID).arg(at);
            command
        }
    };
    let Some(said) = first_output(&mut command) else {
        // No `ldd`, or it could not be run. Not an answer, and not a reason to
        // throw away a core that may be perfectly good.
        return Ok(());
    };
    let missing: Vec<&str> = said
        .lines()
        .filter(|line| line.contains("=> not found"))
        .filter_map(|line| line.split("=>").next())
        .map(str::trim)
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(missing.join(", "))
    }
}

/// Where the names of cores this machine has already proved it cannot load are
/// kept.
///
/// A cache and not a setting: it is a fact about a build of RetroArch and a
/// build of a core, both of which change under it, and losing the file costs
/// one wasted download.
fn refused_at() -> Option<PathBuf> {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|at| at.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|home| home.is_absolute())
                .map(|home| home.join(".cache"))
        })
        .map(|cache| cache.join("lxb").join("cores-that-will-not-load"))
}

/// The cores this machine has downloaded once and found it could not open.
///
/// Read by the scan, which cannot afford to check for itself — see [`loads`] —
/// and which without this would go on preferring a core that has already failed
/// here, offer the download again on the next press, and fail again. A person
/// pressing the same button twice and getting the same nothing is the shape of
/// bug this whole file exists to avoid.
pub fn refused() -> Vec<String> {
    let Some(at) = refused_at() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(at) else {
        return Vec::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// Take the cores this machine has already failed to open out of a console's
/// list.
///
/// The table's own order is otherwise kept exactly. What is best for a console
/// is a question about emulators and belongs in `consoles.rs`; the only thing
/// this machine gets a say in is which of them it has proved it cannot run.
pub fn drop_refused(cores: &mut Vec<String>, refused: &[String]) {
    cores.retain(|core| !refused.iter().any(|had| had == core));
}

/// Remember that this core cannot be opened here.
///
/// Silent on failure, and deliberately: a cache that cannot be written is a
/// download offered twice, which is not worth a word in a panel about
/// something else.
pub fn refuse(core: &str) {
    let Some(at) = refused_at() else {
        return;
    };
    let mut known = refused();
    if known.iter().any(|had| had == core) {
        return;
    }
    known.push(core.to_string());
    if let Some(folder) = at.parent() {
        let _ = std::fs::create_dir_all(folder);
    }
    let _ = std::fs::write(at, known.join("\n") + "\n");
}

/// Whether this machine has flatpak at all, which is what decides whether the
/// shell may offer to install anything.
pub fn flatpak_available() -> bool {
    on_path("flatpak").is_some()
}

/// Everywhere a core might be, as the pair of spellings each place has.
///
/// `.0` is where this process looks; `.1` is what goes on RetroArch's command
/// line. They differ only for a flatpak's own bundled cores — see the module
/// note.
pub struct Cores {
    places: Vec<(PathBuf, PathBuf)>,
    /// The one of them that belongs to this user, which is where a core
    /// downloaded now is written. `None` only where there is no `HOME` to hang
    /// it under.
    own: Option<PathBuf>,
}

impl Cores {
    /// The places this installation's cores can be, best first.
    ///
    /// Cores the user chose to download come before the ones that shipped: a
    /// person who has installed a second core for a console has said which they
    /// would rather use, and the table's own preference is what decides between
    /// cores they have said nothing about.
    pub fn of(installation: &Installation) -> Cores {
        let mut places = Vec::new();
        let mut own = None;
        let home = std::env::var_os("HOME").map(PathBuf::from);

        match installation.kind {
            Kind::System => {
                if let Some(home) = &home {
                    let at = home.join(".config/retroarch/cores");
                    own = Some(at.clone());
                    places.push(same(at));
                }
                for at in [
                    "/usr/lib/libretro",
                    "/usr/lib64/libretro",
                    "/usr/local/lib/libretro",
                    "/usr/lib/x86_64-linux-gnu/libretro",
                    "/usr/lib/aarch64-linux-gnu/libretro",
                ] {
                    places.push(same(PathBuf::from(at)));
                }
            }
            kind @ (Kind::FlatpakUser | Kind::FlatpakSystem) => {
                // Bound into the sandbox at the path it has out here, which is
                // why this one is `same` like a native installation's.
                if let Some(home) = &home {
                    let at = home
                        .join(".var/app")
                        .join(FLATPAK_ID)
                        .join("config/retroarch/cores");
                    own = Some(at.clone());
                    places.push(same(at));
                }
                if let Some(deploy) = scope(kind).and_then(deploy) {
                    places.push((
                        deploy.join("files/lib/libretro"),
                        PathBuf::from("/app/lib/libretro"),
                    ));
                }
            }
        }

        Cores { places, own }
    }

    /// Where a core fetched now is written.
    ///
    /// This user's own core directory, which is the *first* place looked in and
    /// is the same one RetroArch's own updater writes to — so a core fetched by
    /// this shell is one RetroArch itself then lists as installed, and a core
    /// fetched by RetroArch is one this finds without being told. The
    /// alternative, somewhere of this integration's own, would be two
    /// collections of cores on one machine that each thought it had none.
    ///
    /// Never a system directory: writing to `/usr/lib/libretro` needs root, and
    /// the whole of what makes this offer one the shell can put behind a plain
    /// Yes is that it needs nobody's permission.
    pub fn own(&self) -> Option<&Path> {
        self.own.as_deref()
    }

    /// The first of `wanted` that is actually on this disk.
    pub fn first_of(&self, wanted: &[&str]) -> Option<Core> {
        wanted.iter().find_map(|name| self.find(name))
    }

    /// One core by name, if it is here.
    pub fn find(&self, name: &str) -> Option<Core> {
        let file = format!("{name}{CORE_SUFFIX}");
        self.places.iter().find_map(|(here, there)| {
            here.join(&file).is_file().then(|| Core {
                name: name.to_string(),
                path: there.join(&file).to_string_lossy().into_owned(),
            })
        })
    }

    /// Where a core's file is *on this disk*, which is not the path that loads
    /// it inside a sandbox.
    ///
    /// [`Cores::find`] answers with the spelling RetroArch needs on its command
    /// line; this is the one this process can open. For a flatpak they differ
    /// for the shipped cores and happen to agree for the downloaded ones — see
    /// the module note, which is where that trap is written down.
    pub fn on_this_disk(&self, name: &str) -> Option<PathBuf> {
        let file = format!("{name}{CORE_SUFFIX}");
        self.places
            .iter()
            .map(|(here, _)| here.join(&file))
            .find(|at| at.is_file())
    }

    /// Every core installed, by name, in no particular order.
    ///
    /// For the log and for the probe record. Nothing decides anything from it —
    /// which core runs a console is [`Cores::first_of`], asked per console —
    /// but a machine where nothing will start is one where the first question
    /// is what it has, and an integration that could not answer that would send
    /// somebody to a forum.
    pub fn installed(&self) -> Vec<String> {
        let mut names = Vec::new();
        for (here, _) in &self.places {
            let Ok(entries) = std::fs::read_dir(here) else {
                continue;
            };
            for entry in entries.flatten() {
                let file = entry.file_name();
                let Some(name) = file
                    .to_str()
                    .and_then(|file| file.strip_suffix(CORE_SUFFIX))
                else {
                    continue;
                };
                if !names.iter().any(|had| had == name) {
                    names.push(name.to_string());
                }
            }
        }
        names.sort();
        names
    }
}

/// A place whose two spellings are the same one.
fn same(at: PathBuf) -> (PathBuf, PathBuf) {
    (at.clone(), at)
}

/// Where an executable of this name is, walking `PATH` the way a shell does.
///
/// Written out rather than shelled out to `which`: this is three lines and a
/// process is a fork, and the answer wanted here is a path rather than an exit
/// status.
fn on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|at| is_executable(at))
}

fn is_executable(at: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(at).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// Run something and take the first line of what it said, or `None` where it
/// could not be run or said nothing.
fn first_line(command: &mut Command) -> Option<String> {
    let said = first_output(command)?;
    let line = said.lines().next()?.trim();
    (!line.is_empty()).then(|| line.to_string())
}

/// Run something and take the whole of what it said, or `None` where it could
/// not be run or ended in failure.
fn first_output(command: &mut Command) -> Option<String> {
    let out = command.output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A core this machine has already failed to open is not offered again.
    ///
    /// libretro builds its cores against a general-purpose Linux and a flatpak
    /// RetroArch runs against a runtime that is not one, so a core can download
    /// whole and still not load. Offering it a second time spends another
    /// download on the same nothing.
    #[test]
    fn a_core_that_would_not_open_is_out_of_the_running() {
        let mut cores = vec!["pcsx2".to_string(), "play".to_string()];
        drop_refused(&mut cores, &["play".to_string()]);
        assert_eq!(cores, ["pcsx2"]);
    }

    /// And nothing else is reordered. What a console is best played with is a
    /// question about emulators, answered by the table in `consoles.rs`; the
    /// only say this machine gets is which of them it has proved it cannot run.
    ///
    /// It did have more of a say, once. A core that could boot without a BIOS
    /// was sorted in front of one that could not, which on a machine with no
    /// PlayStation 2 BIOS put Play! ahead of pcsx2 — a core that starts and
    /// plays almost nothing ahead of the one people use. The answer to a
    /// missing BIOS is to ask for the BIOS.
    #[test]
    fn the_tables_own_order_is_otherwise_kept() {
        let mut cores = vec!["pcsx2".to_string(), "play".to_string()];
        drop_refused(&mut cores, &[]);
        assert_eq!(cores, ["pcsx2", "play"]);
    }

    /// The flatpak's two spellings of the same directory, which is the one
    /// thing in this file that is not obvious and the one that stops a game
    /// from starting when it is wrong.
    #[test]
    fn a_flatpak_core_is_found_here_and_loaded_there() {
        let cores = Cores {
            places: vec![(
                PathBuf::from("/var/lib/flatpak/app/x/files/lib/libretro"),
                PathBuf::from("/app/lib/libretro"),
            )],
            own: None,
        };
        // Nothing is on this disk at that path, so the lookup answers nothing
        // — the point of the test is the shape of the pair, which the arm
        // below reads back.
        assert!(cores.find("ppsspp").is_none());
        assert_eq!(cores.places[0].1, PathBuf::from("/app/lib/libretro"));
    }

    /// A place whose two halves agree is what every native installation's is,
    /// and what a flatpak's downloaded cores are as well.
    #[test]
    fn a_native_place_is_spelt_one_way() {
        let (here, there) = same(PathBuf::from("/usr/lib/libretro"));
        assert_eq!(here, there);
    }
}
