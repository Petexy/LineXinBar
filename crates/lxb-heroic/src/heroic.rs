//! Whether this machine has Heroic, where it keeps everything, and how its
//! bundled legendary is run.
//!
//! ## Only the flatpak
//!
//! The integration is built on Heroic's Flathub build: its bundled legendary is
//! a Python zipapp that runs on the flatpak's own Python, its paths are the
//! sandbox's, and what `probe` hands the shell to start a game with is
//! `flatpak run`. The user's own installation is looked in first, because that
//! is the one `install` puts there; a system-wide one is used as it is and
//! never duplicated beside.
//!
//! ## Where everything is
//!
//! Under `~/.var/app/com.heroicgameslauncher.hgl/config/heroic`, whichever
//! installation it is — flatpak binds that directory into the sandbox at the
//! same path, so the spelling this process reads and the spelling Heroic writes
//! are one. [`Paths`] names every file this helper touches, and nothing outside
//! it is read.
//!
//! ## legendary is run inside the sandbox, told where Heroic's copy is
//!
//! `LEGENDARY_CONFIG_PATH` is not a detail. Without it legendary keeps its own
//! `~/.config/legendary` inside the sandbox, and an account signed in there is
//! a private copy Heroic never sees — which is the one thing a settings row in
//! this shell must never be.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::report::{Installation, Kind};

/// Heroic's application id, on Flathub and everywhere else.
pub const FLATPAK_ID: &str = "com.heroicgameslauncher.hgl";

/// What Heroic's main process is, inside its sandbox. `heroic-run` hands the
/// command line to `zypak-wrapper`, which execs this; every one of Electron's
/// own processes carries it as its first argument too.
const HEROIC_BINARY: &str = "/app/bin/heroic/heroic";

/// Where the flatpak keeps the runners it bundles, inside the sandbox, above
/// the directory named for the architecture.
const RUNNERS: &str = "/app/bin/heroic/resources/app.asar.unpacked/build/bin";

/// Every file of Heroic's this helper reads or writes.
#[derive(Debug, Clone)]
pub struct Paths {
    root: PathBuf,
}

impl Paths {
    /// Heroic's configuration directory for this user.
    pub fn of_this_user() -> Option<Paths> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|home| home.is_absolute())?;
        Some(Paths::at(
            home.join(".var/app").join(FLATPAK_ID).join("config/heroic"),
        ))
    }

    /// Heroic's configuration directory, wherever that is — a scratch
    /// directory, for a test.
    pub fn at(root: PathBuf) -> Paths {
        Paths { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Heroic's global settings: `{"defaultSettings": {…}, "version": "v0"}`.
    pub fn config(&self) -> PathBuf {
        self.root.join("config.json")
    }

    /// The electron-store copy of the same settings that Heroic's window reads.
    pub fn store_config(&self) -> PathBuf {
        self.root.join("store/config.json")
    }

    /// Heroic's Wine Manager's list of releases, and which are installed.
    pub fn wine_releases(&self) -> PathBuf {
        self.root.join("store/wine-downloader-info.json")
    }

    /// Playtime, per game.
    pub fn timestamps(&self) -> PathBuf {
        self.root.join("store/timestamp.json")
    }

    /// Where Heroic's Wine Manager puts every Proton it downloads.
    pub fn protons(&self) -> PathBuf {
        self.root.join("tools/proton")
    }

    /// legendary's configuration directory — Heroic's copy of it.
    pub fn legendary(&self) -> PathBuf {
        self.root.join("legendaryConfig/legendary")
    }

    /// The Epic session. Signed in is this file existing, for Heroic.
    pub fn user(&self) -> PathBuf {
        self.legendary().join("user.json")
    }

    pub fn metadata(&self) -> PathBuf {
        self.legendary().join("metadata")
    }

    pub fn installed(&self) -> PathBuf {
        self.legendary().join("installed.json")
    }

    /// The EA and Ubisoft titles Heroic has "installed" — see `report::Store`.
    pub fn third_party_installed(&self) -> PathBuf {
        self.legendary().join("third-party-installed.json")
    }

    /// Where Heroic keeps the other stores' installers — its `epicRedistPath`.
    pub fn redist(&self) -> PathBuf {
        self.root.join("tools/redist/legendary")
    }

    /// One game's own settings, over Heroic's global ones.
    pub fn game_config(&self, app_name: &str) -> PathBuf {
        self.root
            .join("GamesConfig")
            .join(format!("{app_name}.json"))
    }
}

/// Whether Heroic syncs saves with Epic around a launch, where a game does
/// not say otherwise: its global `autoSyncSaves`, which it leaves unset.
pub fn cloud_saves(paths: &Paths) -> bool {
    settings(paths).is_some_and(|settings| settings["autoSyncSaves"].as_bool() == Some(true))
}

/// The Wine prefix a game runs in: its own setting, or Heroic's global one.
pub fn prefix_of(paths: &Paths, app_name: &str) -> Option<PathBuf> {
    let own = std::fs::read_to_string(paths.game_config(app_name))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|config| config[app_name]["winePrefix"].as_str().map(str::to_string));
    own.or_else(|| settings(paths)?["winePrefix"].as_str().map(str::to_string))
        .map(PathBuf::from)
        .filter(|at| at.is_absolute())
}

/// Where Heroic installs games: its own `defaultInstallPath`, and its own
/// default where the setting is missing — `~/Games/Heroic`, which is also the
/// one folder under the home the flatpak may create.
pub fn install_base(paths: &Paths, home: &Path) -> PathBuf {
    let set = settings(paths)
        .and_then(|settings| settings["defaultInstallPath"].as_str().map(str::to_string))
        .map(|at| match at.strip_prefix("~/") {
            Some(rest) => home.join(rest),
            None => PathBuf::from(at),
        })
        .filter(|at| at.is_absolute());
    set.unwrap_or_else(|| home.join("Games/Heroic"))
}

/// How many download workers Heroic tells legendary to use, where it tells it
/// any: `maxWorkers`, nought meaning legendary's own choice.
pub fn max_workers(paths: &Paths) -> Option<u64> {
    settings(paths)?["maxWorkers"]
        .as_u64()
        .filter(|workers| *workers > 0)
}

/// Whether Heroic downloads games over plain HTTP: `downloadNoHttps`.
pub fn no_https(paths: &Paths) -> bool {
    settings(paths).is_some_and(|settings| settings["downloadNoHttps"].as_bool() == Some(true))
}

/// Heroic's global settings, `defaultSettings` in its `config.json`.
fn settings(paths: &Paths) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(paths.config()).ok()?;
    let mut config: serde_json::Value = serde_json::from_str(&text).ok()?;
    Some(config.get_mut("defaultSettings")?.take())
}

/// Whether a name is one of Epic's app names — letters and digits, and a dash
/// or an underscore — and so safe to hand legendary, and to name a folder or
/// a file with. It starts with a letter or a digit, so legendary can never
/// read it as one of its own options.
pub fn plain(app_name: &str) -> bool {
    app_name
        .bytes()
        .next()
        .is_some_and(|b| b.is_ascii_alphanumeric())
        && app_name.len() <= 128
        && app_name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The Heroic this machine would use, or `None` for one that has none.
pub fn installation() -> Option<Installation> {
    let paths = Paths::of_this_user()?;
    flatpak(Kind::FlatpakUser, &paths).or_else(|| flatpak(Kind::FlatpakSystem, &paths))
}

fn flatpak(kind: Kind, paths: &Paths) -> Option<Installation> {
    let scope = scope(kind);
    // `flatpak info` fails outright for an application that is not installed,
    // and answers with the deploy directory for one that is.
    deploy(scope)?;
    Some(Installation {
        kind,
        command: vec![
            "flatpak".to_string(),
            "run".to_string(),
            scope.to_string(),
            FLATPAK_ID.to_string(),
        ],
        version: flatpak_version(scope),
        config: paths.root().to_string_lossy().into_owned(),
    })
}

/// The flag naming one of the two flatpak installations.
pub fn scope(kind: Kind) -> &'static str {
    match kind {
        Kind::FlatpakUser => "--user",
        Kind::FlatpakSystem => "--system",
    }
}

fn deploy(scope: &str) -> Option<PathBuf> {
    let at =
        first_line(Command::new("flatpak").args(["info", scope, "--show-location", FLATPAK_ID]))?;
    let at = PathBuf::from(at);
    at.is_dir().then_some(at)
}

/// What the installed flatpak calls itself, from the two-column listing — the
/// one form of `flatpak`'s output whose shape is worth parsing.
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

/// Whether this machine has flatpak at all.
pub fn flatpak_available() -> bool {
    on_path("flatpak").is_some()
}

/// Heroic's own name for this machine's architecture, which is the directory
/// its bundled runners are under: Electron's `process.arch`.
fn arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    }
}

/// legendary, as a path inside the sandbox.
pub fn legendary_in_sandbox() -> String {
    format!("{RUNNERS}/{}/linux/legendary", arch())
}

/// What `flatpak` is given to run Heroic's legendary against Heroic's own
/// configuration, before legendary's own arguments.
///
/// `--die-with-parent`, because the shell stops a download by ending this
/// helper, and without it legendary would go on downloading, inside a sandbox
/// nobody is watching any more, into a folder nobody asked for.
pub fn legendary_argv(kind: Kind, paths: &Paths) -> Vec<String> {
    vec![
        "run".to_string(),
        scope(kind).to_string(),
        "--die-with-parent".to_string(),
        format!("--command={}", legendary_in_sandbox()),
        format!(
            "--env=LEGENDARY_CONFIG_PATH={}",
            paths.legendary().display()
        ),
        FLATPAK_ID.to_string(),
    ]
}

/// The same, with variables set inside the sandbox for this run.
pub fn legendary_with(
    installation: &Installation,
    paths: &Paths,
    env: &[(&str, String)],
) -> Command {
    let mut argv = legendary_argv(installation.kind, paths);
    let app = argv.pop().unwrap_or_else(|| FLATPAK_ID.to_string());
    for (name, value) in env {
        argv.push(format!("--env={name}={value}"));
    }
    argv.push(app);
    let mut command = Command::new("flatpak");
    command.args(argv).stdin(Stdio::null());
    command
}

/// A `Command` for Heroic's legendary, ready for legendary's own arguments.
///
/// Its stdin is closed: legendary asks questions on a terminal when it is not
/// told `-y`, and a question nobody can see is a process that never ends.
/// Run a command in Heroic's sandbox for its output — once more where flatpak
/// itself failed to set the sandbox up, and only then.
///
/// Several of Heroic's sandboxes started in the same moment race on the links
/// flatpak keeps for the application under `$XDG_RUNTIME_DIR/.flatpak`.
/// Measured on 2026-09-25: one of four achievements runs started together
/// failed with "Unable to update symbolic link …/dev-shm: File exists" before
/// legendary had been started at all. Nothing was done, so the run is safe to
/// repeat; a run that failed anywhere else is not repeated.
pub fn output(command: &mut Command) -> std::io::Result<std::process::Output> {
    let first = command.output()?;
    if first.status.success() || !sandbox_not_set_up(&first.stderr) {
        return Ok(first);
    }
    eprintln!("flatpak did not set Heroic's sandbox up; once more");
    std::thread::sleep(std::time::Duration::from_millis(250));
    command.output()
}

/// Whether a run failed in flatpak's own setup rather than in what it ran.
fn sandbox_not_set_up(stderr: &[u8]) -> bool {
    String::from_utf8_lossy(stderr).contains("Unable to update symbolic link")
}

pub fn legendary(installation: &Installation, paths: &Paths) -> Command {
    let mut command = Command::new("flatpak");
    command
        .args(legendary_argv(installation.kind, paths))
        .stdin(Stdio::null());
    command
}

/// Whether a Heroic is running now — its window, its tray icon, or a headless
/// one that started a game and is waiting for it.
///
/// Read off `/proc` rather than asked of `flatpak ps`, which lists this
/// helper's own legendary runs as instances of the same application.
pub fn running() -> bool {
    let Ok(listing) = std::fs::read_dir("/proc") else {
        return false;
    };
    listing.filter_map(Result::ok).any(|entry| {
        entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.bytes().all(|b| b.is_ascii_digit()))
            && std::fs::read(entry.path().join("cmdline")).is_ok_and(|line| is_heroic(&line))
    })
}

/// Whether a running Heroic is in the way of changing what it knows.
///
/// Heroic reads its account, its library and its settings as it starts and
/// keeps them, so a change made behind a running one is one it goes on not
/// knowing. Except where the shell has said (`LXB_HEROIC_IDLE`) that the one
/// running is its own background Heroic, with no window and no game: that one
/// the shell closes and starts again after the change, so it reads it.
pub fn in_the_way() -> bool {
    running() && std::env::var_os("LXB_HEROIC_IDLE").is_none()
}

/// Whether a process's command line is one of Heroic's own.
///
/// Chromium rewrites its processes' titles over their arguments, so what
/// `/proc` shows for a running Heroic is one string joined with spaces —
/// `/app/bin/heroic/heroic --no-gui` — rather than the arguments one by one.
/// Measured on 2026-09-24: an exact match on the first argument missed every
/// one of them.
fn is_heroic(cmdline: &[u8]) -> bool {
    let first = cmdline.split(|b| *b == 0).next().unwrap_or_default();
    first == HEROIC_BINARY.as_bytes()
        || first
            .strip_prefix(HEROIC_BINARY.as_bytes())
            .is_some_and(|rest| rest.first() == Some(&b' '))
}

/// Whose Epic account Heroic is signed in as, from legendary's `user.json`.
///
/// That file holds the account's tokens, so the bytes are cleared as soon as
/// the one field wanted has been taken out of them.
pub fn account(paths: &Paths) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Who {
        #[serde(rename = "displayName")]
        display_name: Option<String>,
    }
    let mut bytes = std::fs::read(paths.user()).ok()?;
    let who = serde_json::from_slice::<Who>(&bytes);
    bytes.fill(0);
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
    who.ok()?
        .display_name
        .filter(|name| !name.trim().is_empty())
}

/// The Proton or Wine Heroic will start games with, by name, where it is
/// actually on this disk — Heroic's own `validWine`: a Proton is its `proton`
/// script existing, a Wine is its binary and its `wineserver`.
pub fn proton(paths: &Paths) -> Option<String> {
    let text = std::fs::read_to_string(paths.config()).ok()?;
    let config: serde_json::Value = serde_json::from_str(&text).ok()?;
    let wine = &config["defaultSettings"]["wineVersion"];
    let name = wine["name"].as_str()?;
    let present = |key: &str| {
        wine[key]
            .as_str()
            .is_some_and(|at| !at.is_empty() && Path::new(at).exists())
    };
    let usable = match wine["type"].as_str()? {
        "wine" => present("bin") && present("wineserver"),
        _ => present("bin"),
    };
    usable.then(|| name.to_string())
}

fn on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

fn first_output(command: &mut Command) -> Option<String> {
    let out = command
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn first_line(command: &mut Command) -> Option<String> {
    first_output(command)?
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only flatpak's own failure to set the sandbox up is run again.
    #[test]
    fn only_a_sandbox_that_was_not_set_up_is_tried_again() {
        assert!(sandbox_not_set_up(
            b"error: Unable to update symbolic link /run/user/1003/.flatpak/com.heroicgameslauncher.hgl/dev-shm: File exists\n"
        ));
        assert!(!sandbox_not_set_up(
            b"[cli] ERROR: Login failed! Unable to check for EULAs.\n"
        ));
        assert!(!sandbox_not_set_up(b""));
    }

    /// The one argument that makes the account Heroic's rather than a copy
    /// nobody else sees, held down.
    #[test]
    fn legendary_is_pointed_at_heroics_own_configuration() {
        let paths = Paths::at(PathBuf::from("/home/somebody/.var/app/x/config/heroic"));
        let argv = legendary_argv(Kind::FlatpakSystem, &paths);
        assert_eq!(argv[0], "run");
        assert_eq!(argv[1], "--system");
        assert_eq!(argv[2], "--die-with-parent");
        assert!(
            argv[3].starts_with("--command=/app/bin/heroic/"),
            "{argv:?}"
        );
        assert!(argv[3].ends_with("/linux/legendary"), "{argv:?}");
        assert_eq!(
            argv[4],
            "--env=LEGENDARY_CONFIG_PATH=/home/somebody/.var/app/x/config/heroic/legendaryConfig/legendary"
        );
        assert_eq!(argv.last().unwrap(), FLATPAK_ID);
    }

    /// Heroic's own processes count; this helper's legendary runs, which are
    /// instances of the same flatpak, must not — or it would refuse to sign in
    /// because of itself.
    #[test]
    fn only_heroics_own_processes_count_as_heroic_running() {
        assert!(is_heroic(
            b"/app/bin/heroic/heroic\0--no-gui\0--no-sandbox\0"
        ));
        assert!(is_heroic(b"/app/bin/heroic/heroic\0--type=zygote\0"));
        // What a running Heroic's processes actually show, title rewritten.
        assert!(is_heroic(b"/app/bin/heroic/heroic --no-gui\0\0\0"));
        assert!(is_heroic(
            b"/app/bin/heroic/heroic --type=gpu-process --ozone-platform=wayland\0"
        ));
        assert!(!is_heroic(b"/app/bin/heroic/heroic-run\0"));
        assert!(!is_heroic(
            b"/usr/bin/python3\0/app/bin/heroic/resources/app.asar.unpacked/build/bin/x64/linux/legendary\0list\0"
        ));
        assert!(!is_heroic(b"bwrap\0--args\0/app/bin/heroic/heroic\0"));
        assert!(!is_heroic(b""));
    }

    #[test]
    fn the_account_name_is_read_and_nothing_else_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::at(dir.path().to_path_buf());
        assert_eq!(account(&paths), None);
        std::fs::create_dir_all(paths.legendary()).unwrap();
        std::fs::write(
            paths.user(),
            r#"{"access_token":"secret","displayName":"Somebody","refresh_token":"secret"}"#,
        )
        .unwrap();
        assert_eq!(account(&paths).as_deref(), Some("Somebody"));
        std::fs::write(paths.user(), r#"{"displayName":"  "}"#).unwrap();
        assert_eq!(account(&paths), None);
    }

    /// Heroic's own install folder and download settings, as it writes them.
    #[test]
    fn games_go_where_heroic_would_put_them() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::at(dir.path().to_path_buf());
        let home = Path::new("/home/somebody");
        assert_eq!(
            install_base(&paths, home),
            PathBuf::from("/home/somebody/Games/Heroic")
        );
        assert_eq!(max_workers(&paths), None);
        assert!(!no_https(&paths));
        std::fs::write(
            paths.config(),
            r#"{"defaultSettings": {"defaultInstallPath": "/mnt/games/Heroic", "maxWorkers": 4, "downloadNoHttps": true}, "version": "v0"}"#,
        )
        .unwrap();
        assert_eq!(
            install_base(&paths, home),
            PathBuf::from("/mnt/games/Heroic")
        );
        assert_eq!(max_workers(&paths), Some(4));
        assert!(no_https(&paths));
        std::fs::write(
            paths.config(),
            r#"{"defaultSettings": {"defaultInstallPath": "~/Spiele", "maxWorkers": 0}}"#,
        )
        .unwrap();
        assert_eq!(
            install_base(&paths, home),
            PathBuf::from("/home/somebody/Spiele")
        );
        assert_eq!(max_workers(&paths), None);
    }

    #[test]
    fn only_epics_own_kind_of_name_is_plain() {
        assert!(plain("051eaac0842c46d7a5a62858ad534d5a"));
        assert!(plain("Quail"));
        assert!(plain("some-app_1"));
        assert!(!plain(""));
        assert!(!plain("../x"));
        assert!(!plain("a b"));
        assert!(!plain("--platform"));
        assert!(plain("x-"));
    }

    /// Heroic's `validWine`: the name only counts where the files are there.
    /// "Default Wine - Not Found" is what a fresh Heroic in its sandbox has,
    /// and it must read as no Proton at all.
    #[test]
    fn a_proton_counts_only_where_it_is_on_the_disk() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::at(dir.path().to_path_buf());
        assert_eq!(proton(&paths), None);

        let write = |wine: serde_json::Value| {
            std::fs::write(
                paths.config(),
                serde_json::json!({"defaultSettings": {"wineVersion": wine}, "version": "v0"})
                    .to_string(),
            )
            .unwrap();
        };
        write(serde_json::json!({"bin": "", "name": "Default Wine - Not Found", "type": "wine"}));
        assert_eq!(proton(&paths), None);

        let at = paths.protons().join("Proton-CachyOS-latest");
        std::fs::create_dir_all(&at).unwrap();
        let bin = at.join("proton");
        write(serde_json::json!({"bin": bin, "name": "Proton-CachyOS-latest", "type": "proton"}));
        assert_eq!(proton(&paths), None);
        std::fs::write(&bin, "").unwrap();
        assert_eq!(proton(&paths).as_deref(), Some("Proton-CachyOS-latest"));
    }
}
