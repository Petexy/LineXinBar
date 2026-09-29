//! Whether this machine has an RPCS3, which one would be used, and where that
//! one keeps the PlayStation 3 it emulates.
//!
//! ## Which RPCS3
//!
//! Four kinds are looked for, and the order is the policy:
//!
//! 1. **The distribution's own package** — `rpcs3` on `PATH`. Somebody chose
//!    it, and it is in no sandbox.
//! 2. **A flatpak in this user's own installation** — the one this helper
//!    installs when there is none, the same as the RetroArch helper does. It
//!    comes before an AppImage on purpose: the user asked for the flatpak to be
//!    the default way RPCS3 arrives, and once it is here it holds the games
//!    installed through the shell, so a stray AppImage found afterwards must
//!    not take them off the bar.
//! 3. **A flatpak installed for the whole machine.**
//! 4. **An AppImage** — RPCS3's own build, which is how many people get it: a
//!    desktop entry that starts one, or a file called `rpcs3…AppImage` in the
//!    folders AppImages are kept in.
//!
//! ## Where its PlayStation 3 is
//!
//! RPCS3 keeps the console's own drives as folders under its configuration
//! directory — `dev_hdd0` is the hard disk games install to, `dev_flash` the
//! system software — unless its `vfs.yml` moves them. The flatpak's is under
//! `~/.var/app/net.rpcs3.RPCS3`, which flatpak binds into the sandbox at the
//! same path, so a path worked out here means the same thing in there.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::report::{Installation, Kind};

/// RPCS3's application id on Flathub.
pub const FLATPAK_ID: &str = "net.rpcs3.RPCS3";

/// Set to anything to leave AppImages out of the search. Nothing sets it but a
/// test run that has to see the first-time setup on a machine which happens to
/// keep an AppImage — which is how the setup was first proven.
const IGNORE_APPIMAGE: &str = "LXB_RPCS3_IGNORE_APPIMAGE";

/// The RPCS3 this machine would use, or `None` for one that has none.
pub fn installation() -> Option<Installation> {
    system()
        .or_else(|| flatpak(Kind::FlatpakUser))
        .or_else(|| flatpak(Kind::FlatpakSystem))
        .or_else(|| {
            std::env::var_os(IGNORE_APPIMAGE)
                .is_none()
                .then(appimage)
                .flatten()
        })
}

fn system() -> Option<Installation> {
    let program = on_path("rpcs3")?;
    let command = vec![program.to_string_lossy().into_owned()];
    Some(Installation {
        kind: Kind::System,
        play: command.clone(),
        command,
        // `rpcs3 --version` starts half the emulator to say one line, and a
        // version is only ever for the log. Not asked.
        version: None,
    })
}

fn flatpak(kind: Kind) -> Option<Installation> {
    let scope = scope(kind)?;
    let listing = output(Command::new("flatpak").args([
        "list",
        scope,
        "--app",
        "--columns=application,version",
    ]))?;
    let version = listing.lines().find_map(|line| {
        let (id, version) = line.split_once('\t').unwrap_or((line, ""));
        (id.trim() == FLATPAK_ID).then(|| version.trim().to_string())
    })?;
    Some(Installation {
        kind,
        command: vec![
            "flatpak".to_string(),
            "run".to_string(),
            scope.to_string(),
            FLATPAK_ID.to_string(),
        ],
        // Not `--unset-env=FLATPAK_ID`: flatpak sets that variable itself
        // after applying what it was told, so the sandbox has it whatever
        // the command line says. It is taken away inside the sandbox instead,
        // by starting the flatpak's own `rpcs3` through `env`.
        play: vec![
            "flatpak".to_string(),
            "run".to_string(),
            scope.to_string(),
            "--command=env".to_string(),
            FLATPAK_ID.to_string(),
            "-u".to_string(),
            "FLATPAK_ID".to_string(),
            "rpcs3".to_string(),
        ],
        version: (!version.is_empty()).then_some(version),
    })
}

/// The flag that names one of flatpak's two installations.
pub fn scope(kind: Kind) -> Option<&'static str> {
    match kind {
        Kind::FlatpakUser => Some("--user"),
        Kind::FlatpakSystem => Some("--system"),
        Kind::System | Kind::AppImage => None,
    }
}

/// An RPCS3 AppImage: first one a desktop entry starts, then one sitting in a
/// folder people keep AppImages in.
fn appimage() -> Option<Installation> {
    let found = desktop_appimage().or_else(loose_appimage)?;
    let command = vec![found.to_string_lossy().into_owned()];
    Some(Installation {
        kind: Kind::AppImage,
        play: command.clone(),
        command,
        version: None,
    })
}

fn desktop_appimage() -> Option<PathBuf> {
    for directory in application_dirs() {
        let Ok(listing) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in listing.filter_map(Result::ok) {
            let path = entry.path();
            if path
                .extension()
                .is_none_or(|extension| extension != "desktop")
            {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            if let Some(program) = appimage_of_entry(&text) {
                return Some(program);
            }
        }
    }
    None
}

/// The AppImage a desktop entry starts, where it is RPCS3's and still there.
///
/// Integration tools write entries like `Exec=env DESKTOPINTEGRATION=1
/// /home/…/rpcs3.appimage %f`, so the program is the first word that is a path
/// to an AppImage, not the first word.
fn appimage_of_entry(text: &str) -> Option<PathBuf> {
    let mut exec = None;
    let mut rpcs3 = false;
    let mut in_entry = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "Exec" | "TryExec" if exec.is_none() || key.trim() == "Exec" => {
                exec = Some(value.trim().to_string());
            }
            "Name" | "StartupWMClass" | "X-AppImage-Name" => {
                rpcs3 |= value.to_ascii_lowercase().contains("rpcs3");
            }
            _ => {}
        }
    }
    if !rpcs3 {
        return None;
    }
    exec?
        .split_whitespace()
        .map(|word| word.trim_matches('"'))
        .find(|word| word.to_ascii_lowercase().ends_with(".appimage"))
        .map(PathBuf::from)
        .filter(|program| is_executable(program))
}

fn loose_appimage() -> Option<PathBuf> {
    let home = home()?;
    for folder in [
        "AppImages",
        "Applications",
        "appimages",
        ".local/bin",
        "Downloads",
    ] {
        let Ok(listing) = std::fs::read_dir(home.join(folder)) else {
            continue;
        };
        let mut found: Vec<PathBuf> = listing
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        let name = name.to_ascii_lowercase();
                        name.contains("rpcs3") && name.ends_with(".appimage")
                    })
                    && is_executable(path)
            })
            .collect();
        // The newest build, where somebody kept more than one.
        found.sort();
        if let Some(last) = found.pop() {
            return Some(last);
        }
    }
    None
}

fn application_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(data) = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| home().map(|home| home.join(".local/share")))
    {
        out.push(data.join("applications"));
    }
    let shared = std::env::var("XDG_DATA_DIRS")
        .unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string());
    out.extend(
        shared
            .split(':')
            .filter(|dir| !dir.is_empty())
            .map(|dir| Path::new(dir).join("applications")),
    );
    out
}

/// Where this installation keeps its configuration and, unless moved, the
/// console's drives.
pub fn config_dir(installation: &Installation) -> Option<PathBuf> {
    Some(match installation.kind {
        Kind::FlatpakUser | Kind::FlatpakSystem => home()?
            .join(".var/app")
            .join(FLATPAK_ID)
            .join("config/rpcs3"),
        Kind::System | Kind::AppImage => std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())
            .or_else(|| home().map(|home| home.join(".config")))?
            .join("rpcs3"),
    })
}

/// Somewhere RPCS3 can read a file this helper puts there — a package being
/// installed, the system software. The flatpak's own cache is the one place
/// both sides of its sandbox see and it may write; the others can read any
/// cache.
pub fn scratch_dir(installation: &Installation) -> Option<PathBuf> {
    Some(match installation.kind {
        Kind::FlatpakUser | Kind::FlatpakSystem => {
            home()?.join(".var/app").join(FLATPAK_ID).join("cache/lxb")
        }
        Kind::System | Kind::AppImage => crate::art::cache_root()?.join("scratch"),
    })
}

/// Where RPCS3 keeps what it compiles for each game (`cache/<serial>`): its
/// `fs::get_cache_dir`, which asks `XDG_CACHE_HOME`, then `XDG_CONFIG_HOME`,
/// then `~/.cache` — inside the sandbox, for the flatpak.
pub fn cache_dir(installation: &Installation) -> Option<PathBuf> {
    Some(match installation.kind {
        Kind::FlatpakUser | Kind::FlatpakSystem => home()?
            .join(".var/app")
            .join(FLATPAK_ID)
            .join("cache/rpcs3"),
        Kind::System | Kind::AppImage => ["XDG_CACHE_HOME", "XDG_CONFIG_HOME"]
            .iter()
            .filter_map(std::env::var_os)
            .map(PathBuf::from)
            .find(|dir| dir.is_absolute())
            .or_else(|| home().map(|home| home.join(".cache")))?
            .join("rpcs3"),
    })
}

/// One RPCS3's PlayStation 3: where its drives are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Console {
    pub config: PathBuf,
    pub hdd0: PathBuf,
    pub hdd1: PathBuf,
    pub flash: PathBuf,
}

impl Console {
    pub fn of(config: &Path) -> Self {
        let vfs = std::fs::read_to_string(config.join("vfs.yml")).unwrap_or_default();
        let setting = |key: &str| -> Option<String> {
            vfs.lines().find_map(|line| {
                let (name, value) = line.split_once(": ")?;
                (unquote(name.trim()) == key).then(|| unquote(value.trim()).to_string())
            })
        };
        let mut emulator = setting("$(EmulatorDir)")
            .filter(|dir| !dir.is_empty())
            .unwrap_or_else(|| format!("{}/", config.display()));
        if !emulator.ends_with('/') {
            emulator.push('/');
        }
        let drive = |key: &str, default: &str| -> PathBuf {
            let value = setting(key)
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| format!("$(EmulatorDir){default}"));
            PathBuf::from(value.replace("$(EmulatorDir)", &emulator))
        };
        Console {
            config: config.to_path_buf(),
            hdd0: drive("/dev_hdd0/", "dev_hdd0/"),
            hdd1: drive("/dev_hdd1/", "dev_hdd1/"),
            flash: drive("/dev_flash/", "dev_flash/"),
        }
    }

    /// The version of the system software installed, `4.93`, or `None` where
    /// there is none — which is what RPCS3 cannot boot a game without.
    pub fn firmware(&self) -> Option<String> {
        let text = std::fs::read_to_string(self.flash.join("vsh/etc/version.txt")).ok()?;
        firmware_version(&text)
    }

    /// Where installed games are.
    pub fn games(&self) -> PathBuf {
        self.hdd0.join("game")
    }

    /// The signed-in user of the emulated console, whose folder under
    /// `dev_hdd0/home` holds licences and trophies — RPCS3's own setting where
    /// it has one, and its default user where it has not.
    ///
    /// RPCS3 keeps it in `persistent_settings.dat` (its `GetCurrentUser`);
    /// builds before that kept it in `CurrentSettings.ini`, which is asked
    /// second.
    pub fn user(&self) -> String {
        ["persistent_settings.dat", "CurrentSettings.ini"]
            .iter()
            .find_map(|name| {
                let settings =
                    std::fs::read_to_string(self.config.join("GuiConfigs").join(name)).ok()?;
                active_user(&settings)
            })
            .unwrap_or_else(|| "00000001".to_string())
    }

    /// Where the user's licences go: a package's `.rap` is copied here, named
    /// after its content ID, which is what RPCS3's own installer does.
    pub fn licences(&self) -> PathBuf {
        self.hdd0.join("home").join(self.user()).join("exdata")
    }

    pub fn trophies(&self) -> PathBuf {
        self.hdd0.join("home").join(self.user()).join("trophy")
    }

    /// Where RPCS3 looks for a disc key by the image's name — its
    /// `get_redump_key_dir`, which is `data/redump/` under the configuration.
    pub fn keys(&self) -> PathBuf {
        self.config.join("data/redump")
    }
}

/// `release:04.9300:` → `4.93`, as the PS3 itself shows it.
fn firmware_version(text: &str) -> Option<String> {
    let release = text
        .lines()
        .find_map(|line| line.strip_prefix("release:"))?
        .trim_end_matches(':');
    let (major, minor) = release.split_once('.')?;
    let major: u32 = major.parse().ok()?;
    let minor = minor.get(..2)?;
    minor
        .bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| format!("{major}.{minor}"))
}

fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(value)
}

/// `[Users] active_user` of one of RPCS3's settings files, where it names a
/// user: eight digits.
fn active_user(settings: &str) -> Option<String> {
    let mut in_users = false;
    for line in settings.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_users = line == "[Users]";
            continue;
        }
        if let Some(value) = line.strip_prefix("active_user=") {
            let value = value.trim();
            if in_users && value.len() == 8 && value.bytes().all(|b| b.is_ascii_digit()) {
                return Some(value.to_string());
            }
        }
    }
    None
}

pub fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
}

pub fn flatpak_available() -> bool {
    on_path("flatpak").is_some()
}

pub fn on_path(program: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")?
        .to_str()?
        .split(':')
        .map(|dir| Path::new(dir).join(program))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

fn output(command: &mut Command) -> Option<String> {
    let out = command.stderr(std::process::Stdio::null()).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_system_software_version_as_the_console_shows_it() {
        let text =
            "release:04.9300:\nbuild:68500,20260108:tetsu@tetsu-linux17\ntarget:0001:CEX-ww\n";
        assert_eq!(firmware_version(text).as_deref(), Some("4.93"));
        assert_eq!(
            firmware_version("release:03.5500:").as_deref(),
            Some("3.55")
        );
        assert_eq!(firmware_version("nonsense"), None);
    }

    #[test]
    fn the_drives_are_under_the_configuration_unless_moved() {
        let dir = tempfile::tempdir().unwrap();
        let console = Console::of(dir.path());
        assert_eq!(console.hdd0, dir.path().join("dev_hdd0/"));
        assert_eq!(console.flash, dir.path().join("dev_flash/"));
        assert_eq!(console.user(), "00000001");
        assert_eq!(console.keys(), dir.path().join("data/redump"));

        std::fs::write(
            dir.path().join("vfs.yml"),
            "$(EmulatorDir): \"\"\n/dev_hdd0/: /mnt/big/hdd0/\n/dev_flash/: $(EmulatorDir)dev_flash/\n",
        )
        .unwrap();
        let moved = Console::of(dir.path());
        assert_eq!(moved.hdd0, PathBuf::from("/mnt/big/hdd0/"));
        assert_eq!(moved.flash, dir.path().join("dev_flash/"));
    }

    #[test]
    fn a_chosen_user_is_the_one_whose_licences_are_read() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("GuiConfigs")).unwrap();
        std::fs::write(
            dir.path().join("GuiConfigs/CurrentSettings.ini"),
            "[Meta]\nactive_user=99999999\n[Users]\nactive_user=00000002\n",
        )
        .unwrap();
        let console = Console::of(dir.path());
        assert_eq!(console.user(), "00000002");
        assert!(console.licences().ends_with("home/00000002/exdata"));
        // Where RPCS3 keeps it now, which wins.
        std::fs::write(
            dir.path().join("GuiConfigs/persistent_settings.dat"),
            "[Playtime]\nBLUS30464=89871\n\n[Users]\nactive_user=00000003\n",
        )
        .unwrap();
        assert_eq!(console.user(), "00000003");
        assert!(console.trophies().ends_with("home/00000003/trophy"));
    }

    /// The entry an AppImage integration tool wrote on the machine this was
    /// first built on.
    #[test]
    fn an_appimage_is_found_through_the_entry_that_starts_it() {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("rpcs3.appimage");
        std::fs::write(&program, b"#!/bin/sh\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let entry = format!(
            "[Desktop Entry]\nType=Application\nName=RPCS3\nTryExec={0}\n\
             Exec=env DESKTOPINTEGRATION=1 {0} %f\nStartupWMClass=rpcs3\n",
            program.display()
        );
        assert_eq!(appimage_of_entry(&entry), Some(program.clone()));
        // An entry that is not RPCS3's is not RPCS3, whatever it starts.
        let other = format!(
            "[Desktop Entry]\nName=osu!\nExec={} %f\nStartupWMClass=osu\n",
            program.display()
        );
        assert_eq!(appimage_of_entry(&other), None);
    }
}
