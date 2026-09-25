//! Epic Games, as the shell holds it: one row, one column, and the account
//! Heroic Games Launcher is signed in to.
//!
//! Everything that touches the machine — whether Heroic is here, installing
//! it, signing it in, reading the library — is a separate program,
//! `lxb-heroic`, which ships in a package of its own, exactly as `lxb-retroarch`
//! does. **This module is the half that belongs to the shell**, and where that
//! program is not on the machine every method here answers as though the
//! integration did not exist. See `crates/lxb-heroic`, and `EPIC-TO-DO.MD` for
//! everything that was measured before any of this was written.
//!
//! ## The journey, as the row
//!
//! ```text
//!   press Epic Games ─► no Heroic ─► "Download it?" ─► Heroic and its Proton ─┐
//!                    ├─ no Proton ─► "one more download" ─────────────────────┤
//!                    ├─ signed out ─► a code for the phone ◄──────────────────┘
//!                    └─ signed in ──► the Epic Games column
//! ```
//!
//! One press, one panel, and which one is [`Heroic::press`] — asked on the
//! press, because every answer is a fact about a helper process and a disk.
//!
//! ## Why the shell starts the games itself
//!
//! A game row carries the command that starts it — Heroic's own shortcut,
//! `heroic://launch?…&gui=false` — and goes through the launcher every other
//! row on this bar goes through, so it gets the loading screen, the display it
//! was started on and the guide's Close. The helper never starts anything.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};

use serde::Deserialize;

use crate::apps::{self, Entry};
use crate::{dialog, icons, menu};

/// What the helper is called, and what the shell looks for on `PATH`.
pub const HELPER: &str = "lxb-heroic";

/// Every name Heroic's own windows and desktop entry go by: the flatpak's
/// application id, and the window class its entry declares.
pub const WINDOW_NAMES: &[&str] = &["com.heroicgameslauncher.hgl", "heroic"];

/// The protocol revision this shell was written against. See
/// `crates/lxb-heroic/src/report.rs`, which declares the same number.
pub const PROTOCOL: u32 = 1;

/// What the row and the column are called. A name rather than a sentence, so
/// it is not translated — the same as Steam's and RetroArch's.
pub const TITLE: &str = "Epic Games";

/// The mark the row, the column and every game without a cover wear. It
/// arrives with the package, under `share/lxb/glyphs`, like RetroArch's.
const MARK: &str = "lxb:epic";

/// The mark, or the shell's own pad where the package brought no drawing.
pub fn mark() -> &'static str {
    if icons::shaped(MARK) {
        MARK
    } else {
        icons::CATEGORY_GAMES
    }
}

/// Heroic's own program, inside its sandbox. What `/proc` shows for every
/// running Heroic — see [`is_running`].
const HEROIC_BINARY: &str = "/app/bin/heroic/heroic";

/// Whether any Heroic is running on this machine, read off `/proc` the way
/// the helper reads it.
///
/// Asked once, at the press on an Epic game, because it decides what the
/// process that press starts is: with no Heroic running it is Heroic itself,
/// there as long as the game is, and with one running it only hands the
/// request over and goes. See [`crate::launch::Launch::heroic_already_running`].
pub fn is_running() -> bool {
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

/// Whether a process's command line is one of Heroic's own. Chromium writes
/// its processes' titles over their arguments, so what `/proc` has is one
/// string joined with spaces — `/app/bin/heroic/heroic --no-gui` — as often as
/// the arguments one by one.
fn is_heroic(cmdline: &[u8]) -> bool {
    let first = cmdline.split(|b| *b == 0).next().unwrap_or_default();
    first == HEROIC_BINARY.as_bytes()
        || first
            .strip_prefix(HEROIC_BINARY.as_bytes())
            .is_some_and(|rest| rest.first() == Some(&b' '))
}

/// The X11 class every game Heroic starts puts on its windows: umu runs them
/// all as Steam's app 0. What a game's Resolution is sent under — see
/// `Shell::resolution_subject`.
pub const WINDOW_CLASS: &str = "steam_app_0";

/// The helper, where this machine has it — for starting the integration
/// again once it has been switched off and on.
pub fn helper() -> Option<PathBuf> {
    FOUND.lock().ok()?.clone()
}

/// Whether the Heroic running is the shell's own background one with nothing
/// to do — no window of its own open and no game of its running. Told to the
/// helper with every question (`LXB_HEROIC_IDLE`), because it is what lets
/// the helper change what Heroic knows behind its back: the shell starts that
/// Heroic again afterwards. See [`Heroic::keep`].
static IDLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The helper, as a command with that said where it is true.
fn helper_command(helper: &Path) -> Command {
    let mut command = Command::new(helper);
    if IDLE.load(std::sync::atomic::Ordering::Relaxed) {
        command.env("LXB_HEROIC_IDLE", "1");
    }
    command
}

/// Heroic's main process — the one its Chromium helpers hang off — where one
/// is running. What is closed when the shell starts its background Heroic
/// again.
fn heroic_main() -> Option<i32> {
    let listing = std::fs::read_dir("/proc").ok()?;
    listing.filter_map(Result::ok).find_map(|entry| {
        let pid: i32 = entry.file_name().to_str()?.parse().ok()?;
        let line = std::fs::read(entry.path().join("cmdline")).ok()?;
        let main = is_heroic(&line) && !String::from_utf8_lossy(&line).contains("--type=");
        main.then_some(pid)
    })
}

/// Whether a game Heroic started is running: Heroic puts `HEROIC_APP_NAME` in
/// the environment of everything it launches, legendary and the game with it.
fn a_game_of_heroics_is_running() -> bool {
    let Ok(listing) = std::fs::read_dir("/proc") else {
        return false;
    };
    listing.filter_map(Result::ok).any(|entry| {
        entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.bytes().all(|b| b.is_ascii_digit()))
            && std::fs::read(entry.path().join("environ")).is_ok_and(|environ| {
                environ
                    .split(|b| *b == 0)
                    .any(|pair| pair.starts_with(b"HEROIC_APP_NAME="))
            })
    })
}

/// Ask Heroic to close, the way a desktop asks: `SIGTERM` to its main
/// process, which Electron takes as quitting.
fn close_heroic(pid: i32) {
    // SAFETY: `kill` with a signal number and a pid is the whole of it.
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
}

/// Where the helper is, as it was last looked for — see [`look_again`].
static FOUND: Mutex<Option<PathBuf>> = Mutex::new(None);

/// The shell's hidden `--heroic-helper`, where it named one.
static NAMED: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Whether this machine has the package — which decides whether Settings >
/// Games has an Epic Games page, as [`crate::retroarch::offered`] decides
/// RetroArch's.
pub fn offered() -> bool {
    FOUND.lock().is_ok_and(|found| found.is_some())
}

/// What Settings > Games > Epic Games shows: Heroic's own answers, as the
/// last probe gave them. Kept here rather than asked for, because the
/// Settings tree is built by free functions and a probe is two `flatpak`
/// calls.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    /// Where Heroic installs games.
    pub base: Option<PathBuf>,
    /// The Proton it starts them with.
    pub proton: Option<String>,
    /// Everything it could start them with instead, by name — see the
    /// helper's `tools.rs`.
    pub tools: Vec<String>,
    /// Whether it syncs saves with Epic around a launch.
    pub cloud_saves: bool,
}

static FACTS: Mutex<Facts> = Mutex::new(Facts {
    base: None,
    proton: None,
    tools: Vec::new(),
    cloud_saves: false,
});

/// See [`Facts`].
pub fn facts() -> Facts {
    FACTS.lock().map(|facts| facts.clone()).unwrap_or_default()
}

/// Write down what the settings page shows. Whether that changed it.
pub(crate) fn note_facts(facts: Facts) -> bool {
    let Ok(mut held) = FACTS.lock() else {
        return false;
    };
    let changed = *held != facts;
    *held = facts;
    changed
}

/// Look for the helper: `named` first (the shell's hidden `--heroic-helper`),
/// then beside the shell's own executable, then `PATH` — the order
/// [`crate::retroarch::look_for_helper`] uses, and for its reason: a local
/// build must not pick up an older installed helper.
///
/// Called before the catalogue is built, because the answer decides whether
/// there is an Epic Games row at all — and then again every few seconds, see
/// [`look_again`].
pub fn look_for_helper(named: Option<PathBuf>) -> Option<PathBuf> {
    let _ = NAMED.set(named);
    let found = find_helper(true);
    match &found {
        Some(at) => tracing::info!(at = %at.display(), "the Epic Games integration is installed"),
        None => tracing::debug!("no Epic Games integration on this machine"),
    }
    if let Ok(mut held) = FOUND.lock() {
        *held = found.clone();
    }
    found
}

/// Look for the package again, and answer where it has come or gone since:
/// `Some` of where it now is — nothing, where it has gone — and `None` where
/// nothing has changed.
///
/// Because a package is installed and removed while a session runs, and
/// what the shell draws has to follow. Taken off, the Epic Games row, its
/// column and its page go, and Heroic is an application like any other again
/// — not a row wearing Epic's mark and offering to sign in through a helper
/// that is no longer there. Put on, the integration starts as a session
/// coming up starts it.
pub fn look_again() -> Option<Option<PathBuf>> {
    let now = find_helper(false);
    let mut held = FOUND.lock().ok()?;
    came_or_went(&mut held, now)
}

/// Write down where the helper is now, and answer where it has come or gone
/// — not where it has merely moved, which changes nothing anybody sees.
fn came_or_went(held: &mut Option<PathBuf>, now: Option<PathBuf>) -> Option<Option<PathBuf>> {
    let changed = held.is_some() != now.is_some();
    *held = now.clone();
    changed.then_some(now)
}

fn find_helper(say: bool) -> Option<PathBuf> {
    let shell = std::env::current_exe().ok();
    let named = NAMED.get().cloned().flatten();
    resolve_helper(named, shell.as_deref(), || on_path(HELPER), say)
}

fn resolve_helper(
    named: Option<PathBuf>,
    shell: Option<&Path>,
    on_path: impl FnOnce() -> Option<PathBuf>,
    say: bool,
) -> Option<PathBuf> {
    if let Some(named) = named {
        return if executable(&named) {
            Some(named)
        } else {
            if say {
                tracing::warn!(at = %named.display(), "there is no executable helper there");
            }
            None
        };
    }
    shell
        .and_then(Path::parent)
        .map(|dir| dir.join(HELPER))
        .filter(|at| executable(at))
        .or_else(on_path)
}

fn executable(at: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(at).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

fn on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|at| executable(at))
}

// --- what the helper says ---------------------------------------------------
//
// Written out here rather than shared with the helper's crate, on the terms
// the RetroArch integration gives: the two are installed apart, and what they
// share is a wire format declared on both sides.

/// Why something did not happen, as the helper words it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Reason {
    NoFlatpak,
    NoHeroic,
    HeroicRunning,
    SignedOut,
    Offline,
    FlatpakRefused,
    Proton,
    Expired,
    ActionNeeded,
    Declined,
    Legendary,
    Files,
    NoSpace,
    NotOwned,
    /// A word a newer helper added.
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize)]
struct Probe {
    protocol: u32,
    heroic: Option<Installation>,
    #[serde(default)]
    flatpak: bool,
    account: Option<String>,
    proton: Option<String>,
    #[serde(default)]
    running: bool,
    #[serde(default)]
    base: Option<String>,
    #[serde(default)]
    cloud_saves: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct Installation {
    command: Vec<String>,
    #[serde(default)]
    version: Option<String>,
    /// Heroic's own configuration folder, which holds the playtimes whose
    /// changing is a game of Heroic's ending — see [`Heroic::keep`].
    #[serde(default)]
    config: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct Progress {
    protocol: u32,
    stage: Stage,
    progress: Option<f32>,
    reason: Option<Reason>,
    #[serde(default)]
    note: String,
}

/// Where an install has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Stage {
    Remote,
    Installing,
    Proton,
    Done,
    Failed,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
enum SignInLine {
    Code {
        protocol: u32,
        code: String,
        url: String,
        short_url: String,
    },
    Approved {
        protocol: u32,
    },
    SignedIn {
        protocol: u32,
        name: String,
    },
    Failed {
        protocol: u32,
        reason: Reason,
        url: Option<String>,
        #[serde(default)]
        note: String,
    },
    #[serde(other)]
    Unknown,
}

impl SignInLine {
    fn usable(&self) -> bool {
        match self {
            SignInLine::Code { protocol, .. }
            | SignInLine::Approved { protocol }
            | SignInLine::SignedIn { protocol, .. }
            | SignInLine::Failed { protocol, .. } => *protocol == PROTOCOL,
            SignInLine::Unknown => false,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct Account {
    protocol: u32,
    name: Option<String>,
    reason: Option<Reason>,
}

#[derive(Debug, Clone, Deserialize)]
struct Library {
    protocol: u32,
    account: Option<String>,
    #[serde(default)]
    refreshed: bool,
    reason: Option<Reason>,
    #[serde(default)]
    games: Vec<Game>,
}

/// One game of the account, as the helper read it.
#[derive(Debug, Clone, Deserialize)]
struct Game {
    app_name: String,
    title: String,
    #[serde(default)]
    cover_file: Option<String>,
    #[serde(default)]
    hero_file: Option<String>,
    #[serde(default)]
    logo_file: Option<String>,
    installed: Option<Installed>,
    store: Option<Store>,
    #[serde(default)]
    played_minutes: u64,
    /// When Heroic last started it, as Heroic writes it down — an ISO date,
    /// so two of them order as they read.
    #[serde(default)]
    last_played: Option<String>,
    /// Whether Epic says the game runs with no connection
    /// (`CanRunOffline`). Most do; the rest are said to need one while this
    /// machine has none.
    #[serde(default)]
    offline: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct Installed {
    #[serde(default)]
    size: u64,
    /// Whether Epic has a newer build — see the helper's `library.rs`.
    #[serde(default)]
    update: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Store {
    Ea,
    Ubisoft,
    #[serde(other)]
    Other,
}

/// What installing one game would take, as the helper measured it.
#[derive(Debug, Clone, Deserialize)]
struct SizeLine {
    protocol: u32,
    app_name: String,
    download: Option<u64>,
    disk: Option<u64>,
    base: Option<String>,
    reason: Option<Reason>,
}

/// One line of a game coming down.
#[derive(Debug, Clone, Deserialize)]
struct DownloadLine {
    protocol: u32,
    app_name: String,
    step: Step,
    progress: Option<f32>,
    #[serde(default)]
    size: Option<u64>,
    reason: Option<Reason>,
    #[serde(default)]
    note: String,
}

/// Where a game's download has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Step {
    Preparing,
    /// A repair checking the files already here, before it fetches what is
    /// wrong.
    Checking,
    Downloading,
    Done,
    Failed,
    #[serde(other)]
    Other,
}

/// One thing Heroic can run games with, as `lxb-heroic tools` lists it.
#[derive(Debug, Clone, Deserialize)]
struct ToolLine {
    name: String,
}

/// What `lxb-heroic tools` answers: what Heroic can run games with, its
/// default, and every game that has one of its own.
#[derive(Debug, Clone, Deserialize)]
struct ToolsLine {
    protocol: u32,
    #[serde(default)]
    tools: Vec<ToolLine>,
    default: Option<String>,
    #[serde(default)]
    games: std::collections::BTreeMap<String, String>,
}

/// What `lxb-heroic use-tool` answers.
#[derive(Debug, Clone, Deserialize)]
struct ToolSetLine {
    protocol: u32,
    game: Option<String>,
    tool: Option<String>,
    reason: Option<Reason>,
    #[serde(default)]
    note: String,
}

/// What choosing the folder games go to came to.
#[derive(Debug, Clone, Deserialize)]
struct FolderLine {
    protocol: u32,
    base: String,
    reason: Option<Reason>,
    #[serde(default)]
    note: String,
}

/// What turning saves sync on or off came to.
#[derive(Debug, Clone, Deserialize)]
struct SavesLine {
    protocol: u32,
    on: bool,
    #[serde(default)]
    found: u32,
    reason: Option<Reason>,
    #[serde(default)]
    note: String,
}

/// What an uninstall came to.
#[derive(Debug, Clone, Deserialize)]
struct Removal {
    protocol: u32,
    app_name: String,
    removed: bool,
    reason: Option<Reason>,
    #[serde(default)]
    note: String,
}

/// One game's achievements, as the helper read them off Epic.
#[derive(Debug, Clone, Deserialize)]
struct AchievementsLine {
    protocol: u32,
    app_name: String,
    #[serde(default)]
    total: u32,
    #[serde(default)]
    unlocked: u32,
    #[serde(default)]
    list: Vec<TrophyLine>,
    reason: Option<Reason>,
    #[serde(default)]
    done: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct TrophyLine {
    name: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    unlocked: bool,
    unlocked_at: Option<u64>,
    icon: Option<String>,
    #[serde(default)]
    xp: u32,
    rarity: Option<f32>,
    #[serde(default)]
    hidden: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct Artwork {
    protocol: u32,
    app_name: String,
    cover: Option<String>,
    hero: Option<String>,
    #[serde(default)]
    logo: Option<String>,
    #[serde(default)]
    done: bool,
}

// --- the worker -------------------------------------------------------------

/// What the shell asks the helper for, one at a time on the worker.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Ask {
    Probe,
    Install,
    SignOut,
    Library {
        refresh: bool,
    },
    Art {
        only: Vec<String>,
        heroes: bool,
    },
    /// What installing one game would take.
    Size(String),
    /// Uninstall one game.
    Remove(String),
    /// Install games into this folder from now on.
    Folder(PathBuf),
    /// Keep saves in Epic's cloud, or not.
    Saves(bool),
    /// The achievements of these games, or of every game worth asking about.
    Achievements(Vec<String>),
    /// What Heroic can run games with, and which games have their own.
    Tools,
    /// Run a game with a tool, or with Heroic's default where there is no
    /// name; or, with no game, make the tool Heroic's default.
    UseTool {
        game: Option<String>,
        name: Option<String>,
    },
}

/// What comes back.
enum Heard {
    Probed(Probe),
    Installing(Progress),
    Account(Account),
    Library(Library),
    Pictured(Artwork),
    Sized(SizeLine),
    Removed(Removal),
    Chose(FolderLine),
    SavesSet(SavesLine),
    Achieved(AchievementsLine),
    Tools(ToolsLine),
    ToolSet(ToolSetLine),
    /// A line of a game coming down, and which download it belongs to: a
    /// download stopped and asked for again is a new one.
    Fetching(u64, DownloadLine),
    FetchEnded(u64),
    /// A line of a sign-in, and which sign-in it belongs to: one given up on
    /// and started again must not have its last words read as the new one's.
    SignIn(u64, SignInLine),
    SignInEnded(u64),
    /// The helper could not be run or could not be understood, for the log.
    Broken(String),
}

fn answer(helper: &Path, ask: &Ask, back: &Sender<Heard>) {
    let mut command = helper_command(helper);
    match ask {
        Ask::Probe => command.arg("probe"),
        Ask::Install => command.arg("install"),
        Ask::SignOut => command.arg("sign-out"),
        Ask::Library { refresh } => {
            command.arg("library");
            if *refresh {
                command.arg("--refresh");
            }
            &mut command
        }
        Ask::Art { only, heroes } => {
            command.arg("art");
            if *heroes {
                command.arg("--heroes");
            }
            for app in only {
                command.arg("--only").arg(app);
            }
            &mut command
        }
        Ask::Size(app) => command.arg("size").arg(app),
        Ask::Remove(app) => command.arg("remove").arg(app),
        Ask::Folder(dir) => command.arg("folder").arg(dir),
        Ask::Saves(on) => command.arg("saves").arg(if *on { "on" } else { "off" }),
        Ask::Achievements(only) => {
            command.arg("achievements");
            for app in only {
                command.arg("--only").arg(app);
            }
            &mut command
        }
        Ask::Tools => command.arg("tools"),
        Ask::UseTool { game, name } => {
            command.arg("use-tool");
            if let Some(game) = game {
                command.arg("--game").arg(game);
            }
            if let Some(name) = name {
                command.arg(name);
            }
            &mut command
        }
    };
    let mut child = match command.stdin(Stdio::null()).stdout(Stdio::piped()).spawn() {
        Ok(child) => child,
        Err(err) => {
            let _ = back.send(Heard::Broken(format!("{HELPER} could not be run: {err}")));
            return;
        }
    };
    let mut said = false;
    if let Some(out) = child.stdout.take() {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let heard = match ask {
                Ask::Probe => serde_json::from_str(line).map(Heard::Probed),
                Ask::Install => serde_json::from_str(line).map(Heard::Installing),
                Ask::SignOut => serde_json::from_str(line).map(Heard::Account),
                Ask::Library { .. } => serde_json::from_str(line).map(Heard::Library),
                Ask::Art { .. } => serde_json::from_str(line).map(Heard::Pictured),
                Ask::Size(_) => serde_json::from_str(line).map(Heard::Sized),
                Ask::Remove(_) => serde_json::from_str(line).map(Heard::Removed),
                Ask::Folder(_) => serde_json::from_str(line).map(Heard::Chose),
                Ask::Saves(_) => serde_json::from_str(line).map(Heard::SavesSet),
                Ask::Achievements(_) => serde_json::from_str(line).map(Heard::Achieved),
                Ask::Tools => serde_json::from_str(line).map(Heard::Tools),
                Ask::UseTool { .. } => serde_json::from_str(line).map(Heard::ToolSet),
            };
            match heard {
                Ok(heard) => {
                    said = true;
                    if back.send(heard).is_err() {
                        break;
                    }
                }
                Err(err) => {
                    tracing::warn!(%err, line, "the Epic Games helper said something unreadable")
                }
            }
        }
    }
    let status = child.wait();
    if !said {
        let _ = back.send(Heard::Broken(format!(
            "{HELPER} answered nothing to {ask:?} ({status:?})"
        )));
    }
}

/// A sign-in, on a thread of its own: it waits on a phone for up to half an
/// hour, and the worker has the library and the covers to get on with.
fn sign_in(helper: PathBuf, generation: u64, held: Arc<Mutex<Option<Child>>>, back: Sender<Heard>) {
    follow(&helper, &["sign-in"], &held, |line: SignInLine| {
        back.send(Heard::SignIn(generation, line)).is_ok()
    });
    let _ = back.send(Heard::SignInEnded(generation));
}

/// A game coming down, on a thread of its own, for the same reason: it takes
/// as long as the line takes.
fn fetch(
    helper: PathBuf,
    generation: u64,
    app_name: String,
    repairing: bool,
    held: Arc<Mutex<Option<Child>>>,
    back: Sender<Heard>,
) {
    let arguments: &[&str] = match repairing {
        true => &["get", &app_name, "--repair"],
        false => &["get", &app_name],
    };
    follow(&helper, arguments, &held, |line: DownloadLine| {
        back.send(Heard::Fetching(generation, line)).is_ok()
    });
    let _ = back.send(Heard::FetchEnded(generation));
}

/// Run the helper with `arguments`, the child held where the shell can end
/// it, and hand each line it says to `each` until that answers `false`.
fn follow<T: serde::de::DeserializeOwned>(
    helper: &Path,
    arguments: &[&str],
    held: &Mutex<Option<Child>>,
    mut each: impl FnMut(T) -> bool,
) {
    let spawned = helper_command(helper)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(err) => {
            tracing::warn!(%err, ?arguments, "the Epic Games helper could not be run");
            return;
        }
    };
    let out = child.stdout.take();
    if let Ok(mut slot) = held.lock() {
        *slot = Some(child);
    }
    if let Some(out) = out {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            match serde_json::from_str::<T>(line.trim()) {
                Ok(said) => {
                    if !each(said) {
                        break;
                    }
                }
                Err(err) => tracing::warn!(%err, ?arguments, "a line this shell cannot read"),
            }
        }
    }
    // Waited for here unless the shell took it first to end it.
    let left = held.lock().ok().and_then(|mut slot| slot.take());
    if let Some(mut child) = left {
        let _ = child.wait();
    }
}

// --- the shell's own state --------------------------------------------------

/// Epic Games as one field of the shell.
pub struct Heroic {
    /// `None` on a machine without the package.
    inner: Option<Inner>,
}

/// What the shell knows of the background Heroic "Start with the shell" and
/// "Leave Heroic running" keep, and of Heroic's playtimes.
#[derive(Default)]
struct Kept {
    /// When it was last looked at: `/proc` is walked every two seconds at
    /// most, and only while one of the two settings is on.
    looked: Option<std::time::Instant>,
    /// Whether it is to be started again once it has gone, to read what the
    /// shell has since changed behind it.
    restart: bool,
    /// When it was asked to close, while it is closing.
    closing: Option<std::time::Instant>,
    /// Whether the one "Start with the shell" asks for has been started.
    started: bool,
    /// Heroic's playtimes, and when they last changed — a game ending.
    playtimes: Option<PathBuf>,
    stamp: Option<std::time::SystemTime>,
}

struct Inner {
    /// Whether this machine has no way to the internet, as the shell last
    /// heard — see [`Heroic::set_online`].
    offline: bool,
    /// What the field at the head of the column narrows it to, as typed.
    search: String,
    /// The background Heroic the shell keeps — see [`Heroic::keep`].
    kept: Kept,
    /// The games that run with a tool of their own, by name — see
    /// [`Heroic::compatibility_rows`]. Nothing until Heroic has been asked.
    own_tools: Option<std::collections::BTreeMap<String, String>>,
    /// The order the column is listed in — Steam's eight, over Epic's facts.
    sort: lxb_steam::library::Sort,
    helper: PathBuf,
    ask: Sender<Ask>,
    back: Sender<Heard>,
    heard: Receiver<Heard>,
    found: Found,
    account: Option<String>,
    games: Vec<Game>,
    /// How many library questions are out. The row says it is looking while
    /// any is.
    reading: u32,
    installing: Option<Installing>,
    signing: Option<Signing>,
    signings: u64,
    /// Whether the covers have been asked for this session. Once: a game Epic
    /// has no picture for will not have one by the next scan.
    covers_asked: bool,
    heroes_asked: HashSet<String>,
    /// Each cover's own shape, measured once off the file's header — see
    /// [`apps::EpicGame::shape`]. Keyed by the file, so a cover fetched again
    /// is measured again.
    shapes: HashMap<PathBuf, Option<f32>>,
    /// Each game's achievements, as the helper last read them, for the
    /// Trophies column — see [`Heroic::trophy_rows`].
    trophies: std::collections::BTreeMap<String, AchievementsLine>,
    /// Whether they have been asked for this session.
    trophies_asked: bool,
    /// Games whose icons have been asked for this session, once their list
    /// was opened — see [`Heroic::want_trophy_icons`].
    icons_asked: HashSet<String>,
    /// Games started from the column since their achievements were last
    /// asked for, which is what is asked again once they have ended.
    played: std::collections::BTreeSet<String>,
    /// What installing each game would take, as far as it has been asked.
    sizes: HashMap<String, Measured>,
    sizing: HashSet<String>,
    /// The game coming down, and the ones waiting their turn behind it — one
    /// at a time, as Heroic's own queue does.
    fetch: Option<Fetch>,
    waiting: VecDeque<String>,
    /// Which of the games waiting, or coming down, are repairs rather than
    /// downloads — see [`Heroic::repair`].
    repairs: HashSet<String>,
    fetches: u64,
    /// Since when the queue has been held because Heroic is open — it keeps
    /// what is installed in memory, so nothing is installed behind its back
    /// (see the helper's `game.rs`). Heroic is asked after every
    /// [`HELD_ASKED`] until it has gone, and the queue then goes on by
    /// itself. `None` while the queue is free to move.
    held: Option<std::time::Instant>,
    /// When each game this session installed landed — see
    /// [`Heroic::landed_at`].
    landed: HashMap<String, std::time::Instant>,
    removing: HashSet<String>,
    broken: Option<String>,
}

/// How often a queue held for Heroic asks whether it has gone. A probe is two
/// `flatpak info` calls and a walk of `/proc`: nothing, every few seconds, and
/// only while a download is waiting on it.
const HELD_ASKED: std::time::Duration = std::time::Duration::from_secs(4);

/// What installing one game would take.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Measured {
    /// Bytes to download and bytes on the disk, where Epic said.
    download: Option<u64>,
    disk: Option<u64>,
    /// The folder games go to — Heroic's own setting.
    base: Option<PathBuf>,
    /// Whether Epic could not be reached to ask, which is the one failure
    /// the install question has anything to say about.
    unreachable: bool,
}

struct Fetch {
    generation: u64,
    app_name: String,
    child: Arc<Mutex<Option<Child>>>,
    so_far: SoFar,
    /// Why it stopped, where its last line said.
    failed: Option<Reason>,
}

/// How far a game's download has got.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SoFar {
    pub step: Step,
    pub progress: Option<f32>,
    /// Bytes to download in all, where known.
    pub size: Option<u64>,
    /// Whether this is an update of a game already here rather than the game
    /// arriving, which is what the row calls it.
    pub updating: bool,
    /// Whether it is a repair — every file checked, what is wrong fetched —
    /// which the row calls that. See [`Heroic::repair`].
    pub repairing: bool,
}

/// What is happening to one game of the column.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Doing {
    Nothing,
    /// Asked for, behind another game's download.
    Waiting,
    /// Asked for, and first in the queue, which is waiting for Heroic to
    /// close — see [`Inner::held`].
    Held,
    Fetching(SoFar),
    Removing,
}

/// A game's download or uninstall that has just ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ended {
    pub app_name: String,
    pub title: String,
    pub worked: bool,
    pub reason: Option<Reason>,
    /// Whether what ended was a repair rather than a download, which is what
    /// is said about it.
    pub repair: bool,
}

impl Inner {
    /// Start the next game in the queue, where nothing is coming down.
    fn next_fetch(&mut self) {
        if self.fetch.is_some() || self.held.is_some() {
            return;
        }
        let Some(app_name) = self.waiting.pop_front() else {
            return;
        };
        self.fetches += 1;
        let generation = self.fetches;
        let child = Arc::new(Mutex::new(None));
        let (helper, held, back, app) = (
            self.helper.clone(),
            child.clone(),
            self.back.clone(),
            app_name.clone(),
        );
        let repairing = self.repairs.contains(&app_name);
        let started = std::thread::Builder::new()
            .name("lxb-heroic-get".to_string())
            .spawn(move || fetch(helper, generation, app, repairing, held, back));
        if let Err(err) = started {
            tracing::error!(?err, "no thread for an Epic download");
            return;
        }
        tracing::info!(app = %app_name, "an Epic download began");
        let updating = self
            .games
            .iter()
            .any(|game| game.app_name == app_name && game.installed.is_some());
        self.fetch = Some(Fetch {
            generation,
            app_name,
            child,
            so_far: SoFar {
                step: Step::Preparing,
                progress: None,
                size: None,
                updating,
                repairing,
            },
            failed: None,
        });
    }

    /// End the download coming down. The helper going takes legendary with
    /// it — see the helper's `--die-with-parent` — and what arrived stays.
    fn end_fetch(&mut self) {
        let Some(fetch) = self.fetch.take() else {
            return;
        };
        let child = fetch.child.lock().ok().and_then(|mut slot| slot.take());
        if let Some(mut child) = child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn doing(&self, app_name: &str) -> Doing {
        if self.removing.contains(app_name) {
            return Doing::Removing;
        }
        if let Some(fetch) = self
            .fetch
            .as_ref()
            .filter(|fetch| fetch.app_name == app_name)
        {
            return Doing::Fetching(fetch.so_far);
        }
        if self.held.is_some() && self.waiting.front().is_some_and(|first| first == app_name) {
            return Doing::Held;
        }
        if self.waiting.iter().any(|waiting| waiting == app_name) {
            return Doing::Waiting;
        }
        Doing::Nothing
    }

    /// Measure every cover not measured yet. Only a file's header is read,
    /// and each file once a session.
    fn measure_covers(&mut self) {
        for game in &self.games {
            let Some(cover) = game.cover_file.as_deref().map(PathBuf::from) else {
                continue;
            };
            self.shapes
                .entry(cover)
                .or_insert_with_key(|at| crate::retroarch::measured(at));
        }
    }

    /// A game's title, or its app name where the library has not got it.
    fn title_of<'a>(&'a self, app_name: &'a str) -> &'a str {
        self.games
            .iter()
            .find(|game| game.app_name == app_name)
            .map_or(app_name, |game| game.title.as_str())
    }
}

enum Found {
    Asking,
    Absent { flatpak: bool },
    Here { command: Vec<String>, proton: bool },
}

/// An install as the shell is drawing it.
pub struct Installing {
    pub stage: Stage,
    pub progress: Option<f32>,
    ended: Option<(bool, Option<Reason>)>,
}

struct Signing {
    generation: u64,
    child: Arc<Mutex<Option<Child>>>,
    stage: Sign,
}

/// Where a sign-in has got to, which is what its panel draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sign {
    /// Waiting for Epic to issue a code.
    Asking,
    /// A code to approve.
    Code {
        code: String,
        address: String,
        qr: Option<lxb_steam::qr::Code>,
        /// Epic's page for approving it, the code already filled in — what
        /// the QR code carries, and what "Sign in on this screen" opens. `None`
        /// for an address that is not Epic's own.
        page: Option<String>,
    },
    /// Approved on the phone; Heroic is being signed in with it.
    Approved,
    Failed {
        reason: Reason,
        /// Where Epic wants the person to go first, for
        /// [`Reason::ActionNeeded`].
        qr: Option<lxb_steam::qr::Code>,
    },
}

/// What one turn of the loop changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Change {
    /// The row's line, the column's rows, or both.
    pub rows: bool,
    /// The sign-in panel wants redrawing.
    pub panel: bool,
    /// An install has just ended, whether it worked, and why not.
    pub installed: Option<(bool, Option<Reason>)>,
    /// A sign-in has just ended signed in, as this account.
    pub signed_in: Option<String>,
    /// A sign-out has just ended, and why it did not work where it did not.
    pub signed_out: Option<Option<Reason>>,
    /// What installing a game would take has just been measured.
    pub sized: Option<String>,
    /// A game's download has just ended.
    pub fetched: Option<Ended>,
    /// A game's uninstall has just ended.
    pub removed: Option<Ended>,
    /// What Settings > Games > Epic Games shows has changed.
    pub settings: bool,
    /// The folder games go to was asked to change, and why it did not where
    /// it did not.
    pub chose: Option<Option<Reason>>,
    /// Saves sync was asked to change, and why it did not where it did not.
    pub saves: Option<Option<Reason>>,
    /// What the Trophies column shows of Epic's has changed.
    pub trophies: bool,
    /// What Heroic can run games with was heard, so a menu listing it has
    /// something new to show.
    pub tools: bool,
    /// A tool was chosen, for a game or as the default: `None` inside where it
    /// was written, the reason where it was not.
    pub tool_set: Option<Option<Reason>>,
}

/// What pressing the Epic Games row means, asked when it is pressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Press {
    /// The helper has not answered yet; the press asks again.
    Waiting,
    /// Offer to install Heroic.
    Install,
    /// It cannot be done here, and the sentence saying so.
    Cannot(String),
    /// Heroic is here without a Proton to start games with.
    SetUp,
    SignIn,
    /// Step across to the column.
    Enter,
}

impl Heroic {
    /// A session without the integration.
    pub fn absent() -> Heroic {
        Heroic { inner: None }
    }

    /// Start the worker and ask what is on this machine. Nothing is asked on
    /// this thread: the probe runs `flatpak info` twice.
    pub fn start(helper: PathBuf) -> Heroic {
        let (ask, asked) = std::sync::mpsc::channel::<Ask>();
        let (back, heard) = std::sync::mpsc::channel::<Heard>();
        let worker_helper = helper.clone();
        let worker_back = back.clone();
        let started = std::thread::Builder::new()
            .name("lxb-heroic".to_string())
            .spawn(move || {
                for question in asked {
                    answer(&worker_helper, &question, &worker_back);
                }
            });
        if let Err(err) = started {
            tracing::error!(?err, "no thread for the Epic Games integration");
            return Heroic::absent();
        }
        let mut heroic = Heroic {
            inner: Some(Inner {
                helper,
                ask,
                back,
                heard,
                found: Found::Asking,
                account: None,
                games: Vec::new(),
                reading: 0,
                installing: None,
                signing: None,
                signings: 0,
                covers_asked: false,
                heroes_asked: HashSet::new(),
                shapes: HashMap::new(),
                trophies: std::collections::BTreeMap::new(),
                trophies_asked: false,
                icons_asked: HashSet::new(),
                played: std::collections::BTreeSet::new(),
                sizes: HashMap::new(),
                sizing: HashSet::new(),
                fetch: None,
                waiting: VecDeque::new(),
                repairs: HashSet::new(),
                fetches: 0,
                held: None,
                landed: HashMap::new(),
                removing: HashSet::new(),
                broken: None,
                offline: false,
                search: String::new(),
                kept: Kept::default(),
                own_tools: None,
                sort: crate::settings::epic_sort().unwrap_or_default(),
            }),
        };
        heroic.send(Ask::Probe);
        heroic
    }

    fn send(&mut self, ask: Ask) {
        let Some(inner) = self.inner.as_mut() else {
            return;
        };
        if matches!(ask, Ask::Library { .. }) {
            inner.reading += 1;
        }
        // Pictures on threads of their own, never in the worker's queue: a
        // library's covers take a while, and the backdrop of the game the
        // cursor has just reached must not wait behind them — nor must the
        // row's own answers. Measured, the first way round: a backdrop asked
        // for half a minute into the covers had still not been fetched. A
        // game's size likewise: it is what an open panel is waiting on.
        if matches!(ask, Ask::Art { .. } | Ask::Size(_) | Ask::Achievements(_)) {
            let (helper, back) = (inner.helper.clone(), inner.back.clone());
            let started = std::thread::Builder::new()
                .name("lxb-heroic-art".to_string())
                .spawn(move || answer(&helper, &ask, &back));
            if let Err(err) = started {
                tracing::warn!(?err, "no thread for Epic pictures");
            }
            return;
        }
        if inner.ask.send(ask).is_err() {
            inner.broken = Some("the Epic Games worker has stopped".to_string());
        }
    }

    /// Ask again what is on this machine.
    pub fn reprobe(&mut self) {
        self.send(Ask::Probe);
    }

    /// Read the library off the disk again, without asking Epic: after a game,
    /// whose playtime Heroic has just written.
    pub fn reread(&mut self) {
        if self.account().is_none() {
            return;
        }
        self.send(Ask::Library { refresh: false });
    }

    /// Start a Heroic in the background, with no window — what "Start with the
    /// shell" asks for as the session comes up, and what "Leave Heroic
    /// running" keeps. Only where Heroic is here, signed in and set up, and
    /// none is running already.
    ///
    /// Heroic's own shortcut with no game named: `heroic://launch?gui=false`
    /// is the one address that brings it up without its window, and naming
    /// nothing, it launches nothing. With `--no-gui` as well unless it is to
    /// be left running, since that is what makes Heroic close with the first
    /// game it is handed. Detached, because it is not an application of the
    /// shell's to watch: it outlives the press that wanted it.
    pub fn start_background(&mut self) {
        let Some(inner) = self.inner.as_mut() else {
            return;
        };
        let Found::Here {
            command,
            proton: true,
        } = &inner.found
        else {
            return;
        };
        if inner.account.is_none() || heroic_main().is_some() {
            return;
        }
        let mut argv = vec!["-f".to_string()];
        argv.extend(without_electron_as_node(command));
        if !crate::settings::epic_left_after_a_game() {
            argv.push("--no-gui".to_string());
        }
        argv.push("--no-sandbox".to_string());
        argv.push("heroic://launch?gui=false".to_string());
        let started = Command::new("setsid")
            .args(&argv)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match started {
            Ok(mut child) => {
                // `setsid -f` answers at once; what it started is on its own.
                let _ = child.wait();
                tracing::info!("Heroic was started in the background");
            }
            Err(err) => tracing::warn!(%err, "Heroic could not be started in the background"),
        }
    }

    /// Close the background Heroic, where there is one with nothing to do —
    /// the integration being switched off. One with a window, or a game, is
    /// somebody's.
    pub fn close_background(&mut self) {
        if !IDLE.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        if let Some(pid) = heroic_main() {
            tracing::info!(pid, "closing the background Heroic");
            close_heroic(pid);
        }
    }

    /// Look after the background Heroic, and notice a game of Heroic's
    /// ending. Asked every frame; does anything at all every two seconds at
    /// most. Answers whether a game has ended since it last looked.
    ///
    /// Only with "Start with the shell" or "Leave Heroic running" on is a
    /// Heroic with no window and no game the shell's business. Then it is
    /// the one the helper may change things behind — told so with each
    /// question — and after such a change it is closed and started again, so
    /// what it read when it started is true again; and a download held for
    /// Heroic to close goes on.
    ///
    /// A game ending is Heroic writing its playtimes, which it does whether it
    /// was started from here or from its own window, and whether or not it
    /// then closes.
    pub fn keep(&mut self, heroic_window_open: bool) -> bool {
        let Some(inner) = self.inner.as_mut() else {
            return false;
        };
        let now = std::time::Instant::now();
        if inner
            .kept
            .looked
            .is_some_and(|at| now.duration_since(at) < std::time::Duration::from_secs(2))
        {
            return false;
        }
        inner.kept.looked = Some(now);

        let stamp = inner
            .kept
            .playtimes
            .as_ref()
            .and_then(|at| std::fs::metadata(at).ok()?.modified().ok());
        let ended = inner.kept.stamp.is_some() && stamp.is_some() && stamp != inner.kept.stamp;
        if stamp.is_some() {
            inner.kept.stamp = stamp;
        }

        let wanted =
            crate::settings::epic_at_startup() || crate::settings::epic_left_after_a_game();
        if !wanted {
            IDLE.store(false, std::sync::atomic::Ordering::Relaxed);
            inner.kept.restart = false;
            return ended;
        }
        let main = heroic_main();
        let idle = main.is_some() && !heroic_window_open && !a_game_of_heroics_is_running();
        IDLE.store(idle, std::sync::atomic::Ordering::Relaxed);

        // Started again, once the one asked to close has gone.
        if let Some(asked) = inner.kept.closing {
            if main.is_none() {
                inner.kept.closing = None;
                inner.kept.restart = false;
                self.start_background();
            } else if now.duration_since(asked) > std::time::Duration::from_secs(15) {
                tracing::warn!("the background Heroic did not close; leaving it");
                inner.kept.closing = None;
                inner.kept.restart = false;
            }
            return ended;
        }
        if inner.kept.restart {
            match main {
                None => inner.kept.restart = false,
                Some(pid) if idle => {
                    tracing::info!(
                        pid,
                        "starting the background Heroic again, to read what changed"
                    );
                    close_heroic(pid);
                    inner.kept.closing = Some(now);
                }
                Some(_) => {}
            }
        }
        // A download held for Heroic to close need not wait for this one.
        if idle && inner.held.is_some() {
            tracing::info!(
                "Heroic is the shell's own, with nothing to do; the Epic downloads go on"
            );
            inner.held = None;
            inner.next_fetch();
        }
        ended
    }

    /// A game of Heroic's has ended: its playtime is new, and so may its
    /// achievements be.
    pub fn game_ended(&mut self) {
        if self.account().is_none() {
            return;
        }
        self.send(Ask::Library { refresh: false });
        let played: Vec<String> = match self.inner.as_mut() {
            Some(inner) => std::mem::take(&mut inner.played).into_iter().collect(),
            None => Vec::new(),
        };
        if !played.is_empty() {
            self.send(Ask::Achievements(played));
        }
    }

    /// A game of the column has been started. Its achievements are asked for
    /// again once it has ended — see [`Self::reread`].
    pub fn started(&mut self, app_name: &str) {
        if let Some(inner) = self.inner.as_mut() {
            inner.played.insert(app_name.to_string());
        }
    }

    /// Somebody has opened a game's achievements: fetch the icons it has not
    /// got, once a session. The pass over the whole library uses only the
    /// icons already on the disk — thousands of pictures, most never looked
    /// at — and the Steam half of the column fetches a game's the same way,
    /// when its list is opened.
    pub fn want_trophy_icons(&mut self, app_name: &str) {
        let Some(inner) = self.inner.as_mut() else {
            return;
        };
        let missing = inner
            .trophies
            .get(app_name)
            .is_some_and(|line| line.list.iter().any(|trophy| trophy.icon.is_none()));
        if !missing || !inner.icons_asked.insert(app_name.to_string()) {
            return;
        }
        self.send(Ask::Achievements(vec![app_name.to_string()]));
    }

    /// Ask Epic for the library again.
    pub fn refresh(&mut self) {
        if self.account().is_some() {
            self.send(Ask::Library { refresh: true });
        }
    }

    /// Install Heroic where there is none, and its Proton either way.
    pub fn install(&mut self) {
        let Some(inner) = self.inner.as_mut() else {
            return;
        };
        if inner.installing.is_some() {
            return;
        }
        inner.installing = Some(Installing {
            stage: Stage::Remote,
            progress: None,
            ended: None,
        });
        self.send(Ask::Install);
    }

    /// The install on screen, if one is happening.
    pub fn installing(&self) -> Option<&Installing> {
        self.inner.as_ref()?.installing.as_ref()
    }

    /// Forget an install that has been answered.
    pub fn installed(&mut self) {
        if let Some(inner) = self.inner.as_mut() {
            inner.installing = None;
        }
    }

    pub fn sign_out(&mut self) {
        self.send(Ask::SignOut);
    }

    /// Start a sign-in, giving up any before it.
    pub fn begin_sign_in(&mut self) {
        self.cancel_sign_in();
        let Some(inner) = self.inner.as_mut() else {
            return;
        };
        inner.signings += 1;
        let generation = inner.signings;
        let child = Arc::new(Mutex::new(None));
        let (helper, held, back) = (inner.helper.clone(), child.clone(), inner.back.clone());
        let started = std::thread::Builder::new()
            .name("lxb-heroic-sign-in".to_string())
            .spawn(move || sign_in(helper, generation, held, back));
        if let Err(err) = started {
            tracing::error!(?err, "no thread for an Epic sign-in");
            return;
        }
        inner.signing = Some(Signing {
            generation,
            child,
            stage: Sign::Asking,
        });
    }

    /// Give a sign-in up: its panel has gone. The helper is ended, which is
    /// all it takes — nothing is held until a code has been approved.
    pub fn cancel_sign_in(&mut self) {
        let Some(signing) = self.inner.as_mut().and_then(|inner| inner.signing.take()) else {
            return;
        };
        let child = signing.child.lock().ok().and_then(|mut slot| slot.take());
        if let Some(mut child) = child {
            let _ = child.kill();
            let _ = child.wait();
        }
        tracing::info!("an Epic sign-in was given up");
    }

    /// Where the sign-in on screen has got to.
    pub fn signing(&self) -> Option<&Sign> {
        Some(&self.inner.as_ref()?.signing.as_ref()?.stage)
    }

    /// Epic's page for approving the code on screen, where a sign-in is
    /// showing one — see [`Sign::Code`].
    pub fn sign_in_page(&self) -> Option<&str> {
        match self.signing()? {
            Sign::Code { page, .. } => page.as_deref(),
            _ => None,
        }
    }

    /// Whose account Heroic holds.
    pub fn account(&self) -> Option<&str> {
        self.inner.as_ref()?.account.as_deref()
    }

    /// Ask what installing a game would take, unless that is known or asked.
    /// A measurement that came back without a size — Epic not reachable — is
    /// asked again, so the next press may have it.
    pub fn measure(&mut self, app_name: &str) {
        let Some(inner) = self.inner.as_mut() else {
            return;
        };
        let known = inner
            .sizes
            .get(app_name)
            .is_some_and(|measured| measured.download.is_some());
        if known || !inner.sizing.insert(app_name.to_string()) {
            return;
        }
        self.send(Ask::Size(app_name.to_string()));
    }

    /// Install a game: now, or after the one coming down.
    pub fn get(&mut self, app_name: &str) {
        let Some(inner) = self.inner.as_mut() else {
            return;
        };
        let busy = inner
            .fetch
            .as_ref()
            .is_some_and(|fetch| fetch.app_name == app_name)
            || inner.waiting.iter().any(|waiting| waiting == app_name);
        if busy || !inner.games.iter().any(|game| game.app_name == app_name) {
            return;
        }
        inner.waiting.push_back(app_name.to_string());
        inner.next_fetch();
    }

    /// Check every file of an installed game against Epic's manifest and
    /// fetch whatever is wrong — Heroic's Verify and Repair — in the queue a
    /// download goes in, counted on the row the way one is.
    pub fn repair(&mut self, app_name: &str) {
        let Some(inner) = self.inner.as_mut() else {
            return;
        };
        let installed = inner.games.iter().any(|game| {
            game.app_name == app_name && game.installed.is_some() && game.store.is_none()
        });
        let busy = inner
            .fetch
            .as_ref()
            .is_some_and(|fetch| fetch.app_name == app_name)
            || inner.waiting.iter().any(|waiting| waiting == app_name);
        if busy || !installed {
            return;
        }
        inner.repairs.insert(app_name.to_string());
        inner.waiting.push_back(app_name.to_string());
        inner.next_fetch();
    }

    /// Stop a game coming down, or take it out of the queue. What has arrived
    /// stays on the disk: asking for the game again carries on from it.
    pub fn stop(&mut self, app_name: &str) {
        let Some(inner) = self.inner.as_mut() else {
            return;
        };
        inner.waiting.retain(|waiting| waiting != app_name);
        inner.repairs.remove(app_name);
        if inner.waiting.is_empty() {
            inner.held = None;
        }
        if inner
            .fetch
            .as_ref()
            .is_some_and(|fetch| fetch.app_name == app_name)
        {
            inner.end_fetch();
            tracing::info!(app_name, "an Epic download was stopped");
            inner.next_fetch();
        }
    }

    /// Keep saves in Epic's cloud, or not — Heroic's own setting.
    pub fn set_cloud_saves(&mut self, on: bool) {
        self.send(Ask::Saves(on));
    }

    /// Install games into `dir` from now on — Heroic's own setting.
    pub fn choose_folder(&mut self, dir: &Path) {
        self.send(Ask::Folder(dir.to_path_buf()));
    }

    /// Uninstall a game.
    pub fn remove(&mut self, app_name: &str) {
        let Some(inner) = self.inner.as_mut() else {
            return;
        };
        if !inner.removing.insert(app_name.to_string()) {
            return;
        }
        self.send(Ask::Remove(app_name.to_string()));
    }

    /// When a game this session installed landed on the disk. A Heroic that
    /// was already running then does not know it is there — it read what is
    /// installed when it started — and a Play handed to it would be answered
    /// with its own offer to install the game, in a window this shell keeps
    /// off the screen.
    pub fn landed_at(&self, app_name: &str) -> Option<std::time::Instant> {
        self.inner.as_ref()?.landed.get(app_name).copied()
    }

    /// What is happening to one game.
    pub fn doing(&self, app_name: &str) -> Doing {
        match self.inner.as_ref() {
            Some(inner) => inner.doing(app_name),
            None => Doing::Nothing,
        }
    }

    /// Ask for one game's backdrop, once a session.
    pub fn want_hero(&mut self, app_name: &str) {
        let Some(inner) = self.inner.as_mut() else {
            return;
        };
        if !inner.heroes_asked.insert(app_name.to_string()) {
            return;
        }
        self.send(Ask::Art {
            only: vec![app_name.to_string()],
            heroes: true,
        });
    }

    /// What starts Heroic's own window, where Heroic is here.
    pub fn open_command(&self) -> Option<Vec<String>> {
        match &self.inner.as_ref()?.found {
            Found::Here { command, .. } => Some(without_electron_as_node(command)),
            _ => None,
        }
    }

    /// Everything the helper said since the last frame.
    pub fn poll(&mut self) -> Change {
        let mut change = Change::default();
        let Some(inner) = self.inner.as_mut() else {
            return change;
        };
        let mut asks = Vec::new();
        let mut start_background = false;
        while let Ok(heard) = inner.heard.try_recv() {
            match heard {
                Heard::Probed(probe) if probe.protocol != PROTOCOL => {
                    tracing::warn!(
                        protocol = probe.protocol,
                        "an Epic Games helper of another protocol"
                    );
                    inner.broken = Some("protocol".to_string());
                    change.rows = true;
                }
                Heard::Probed(probe) => {
                    let had = inner.account.clone();
                    inner.found = match probe.heroic {
                        Some(found) => {
                            tracing::info!(version = ?found.version, proton = ?probe.proton, "Heroic is here");
                            inner.kept.playtimes = found
                                .config
                                .map(|config| PathBuf::from(config).join("store/timestamp.json"));
                            Found::Here {
                                command: found.command,
                                proton: probe.proton.is_some(),
                            }
                        }
                        None => Found::Absent {
                            flatpak: probe.flatpak,
                        },
                    };
                    inner.account = probe.account;
                    change.settings |= note_facts(Facts {
                        base: probe.base.map(PathBuf::from),
                        proton: probe.proton,
                        tools: facts().tools,
                        cloud_saves: probe.cloud_saves,
                    });
                    // Heroic has gone: the queue held for it goes on.
                    if inner.held.is_some() && !probe.running {
                        tracing::info!("Heroic has closed; the Epic downloads go on");
                        inner.held = None;
                        inner.next_fetch();
                    }
                    if inner.account.is_some() && (had != inner.account || inner.games.is_empty()) {
                        // Off the disk first, which is immediate, and then
                        // from Epic, which takes a few seconds and may not be
                        // reachable at all.
                        asks.push(Ask::Library { refresh: false });
                        asks.push(Ask::Library { refresh: true });
                    }
                    if inner.account.is_none() {
                        inner.games.clear();
                    }
                    // "Start with the shell", once a session, once there is a
                    // Heroic signed in and set up to start.
                    if crate::settings::epic_at_startup()
                        && !inner.kept.started
                        && inner.account.is_some()
                        && matches!(inner.found, Found::Here { proton: true, .. })
                    {
                        inner.kept.started = true;
                        start_background = true;
                    }
                    // What it can run games with, once there is a Heroic to
                    // ask about: the menus and the settings page list it.
                    if matches!(inner.found, Found::Here { .. }) && inner.own_tools.is_none() {
                        asks.push(Ask::Tools);
                    }
                    change.rows = true;
                }
                Heard::Tools(line) if line.protocol != PROTOCOL => {}
                Heard::Tools(line) => {
                    let mut facts = facts();
                    facts.tools = line.tools.into_iter().map(|tool| tool.name).collect();
                    facts.proton = line.default.or(facts.proton);
                    change.settings |= note_facts(facts);
                    inner.own_tools = Some(line.games);
                    change.tools = true;
                }
                Heard::ToolSet(line) if line.protocol != PROTOCOL => {}
                Heard::ToolSet(line) => {
                    tracing::info!(game = ?line.game, tool = ?line.tool, reason = ?line.reason, note = %line.note, "what an Epic game runs with was asked to change");
                    inner.kept.restart |= line.reason.is_none();
                    change.tool_set = Some(line.reason);
                    // Asked again rather than guessed at: what it now runs with is
                    // what the files say.
                    asks.push(Ask::Tools);
                }
                Heard::Installing(line) if line.protocol != PROTOCOL => {}
                Heard::Installing(line) => {
                    let installing = inner.installing.get_or_insert(Installing {
                        stage: line.stage,
                        progress: None,
                        ended: None,
                    });
                    installing.stage = line.stage;
                    installing.progress = line.progress;
                    match line.stage {
                        Stage::Done => installing.ended = Some((true, None)),
                        Stage::Failed => {
                            tracing::warn!(note = %line.note, reason = ?line.reason, "Heroic was not set up");
                            installing.ended = Some((false, line.reason));
                        }
                        _ => {}
                    }
                    if let Some(ended) = installing.ended {
                        change.installed = Some(ended);
                        asks.push(Ask::Probe);
                    }
                    change.rows = true;
                    change.panel = true;
                }
                Heard::Account(account) if account.protocol != PROTOCOL => {}
                Heard::Account(account) => {
                    inner.account = account.name;
                    if inner.account.is_none() {
                        inner.games.clear();
                    }
                    inner.kept.restart |= account.reason.is_none();
                    change.signed_out = Some(account.reason);
                    change.rows = true;
                }
                Heard::Library(library) if library.protocol != PROTOCOL => {
                    inner.reading = inner.reading.saturating_sub(1);
                }
                Heard::Library(library) => {
                    inner.reading = inner.reading.saturating_sub(1);
                    if let Some(reason) = library.reason {
                        tracing::info!(?reason, "the Epic library was not asked of Epic this time");
                    }
                    inner.account = library.account;
                    // A refresh that failed still carries the last good list;
                    // only a signed-out answer empties the column.
                    if inner.account.is_none() || !library.games.is_empty() || library.refreshed {
                        inner.games = library.games;
                    }
                    inner.measure_covers();
                    // The achievements, once a session and once the account's
                    // games are known: which games are worth asking about is
                    // a question about the library.
                    if inner.account.is_some() && !inner.trophies_asked && !inner.games.is_empty() {
                        inner.trophies_asked = true;
                        asks.push(Ask::Achievements(Vec::new()));
                    }
                    let uncovered = inner.games.iter().any(|game| game.cover_file.is_none());
                    if !inner.covers_asked && uncovered && inner.account.is_some() {
                        inner.covers_asked = true;
                        asks.push(Ask::Art {
                            only: Vec::new(),
                            heroes: false,
                        });
                    }
                    change.rows = true;
                }
                Heard::Pictured(art) if art.protocol != PROTOCOL || art.done => {}
                Heard::Pictured(art) => {
                    if let Some(game) = inner
                        .games
                        .iter_mut()
                        .find(|game| game.app_name == art.app_name)
                    {
                        game.cover_file = art.cover.or(game.cover_file.take());
                        game.hero_file = art.hero.or(game.hero_file.take());
                        game.logo_file = art.logo.or(game.logo_file.take());
                        change.rows = true;
                    }
                    inner.measure_covers();
                }
                Heard::Sized(line) => {
                    inner.sizing.remove(&line.app_name);
                    if line.protocol != PROTOCOL {
                        continue;
                    }
                    if let Some(reason) = line.reason {
                        tracing::info!(?reason, app = %line.app_name, "an Epic game's size was not measured");
                    }
                    inner.sizes.insert(
                        line.app_name.clone(),
                        Measured {
                            download: line.download,
                            disk: line.disk,
                            base: line.base.map(PathBuf::from),
                            unreachable: line.reason == Some(Reason::Offline),
                        },
                    );
                    change.sized = Some(line.app_name);
                }
                Heard::Achieved(line) if line.protocol != PROTOCOL || line.done => {}
                Heard::Achieved(line) => {
                    if let Some(reason) = line.reason {
                        tracing::info!(app = %line.app_name, ?reason, "an Epic game's achievements could not be read");
                    }
                    inner.trophies.insert(line.app_name.clone(), line);
                    change.trophies = true;
                }
                Heard::SavesSet(line) if line.protocol != PROTOCOL => {}
                Heard::SavesSet(line) => {
                    tracing::info!(on = line.on, found = line.found, reason = ?line.reason, note = %line.note, "Epic saves sync was asked to change");
                    let mut facts = facts();
                    facts.cloud_saves = line.on;
                    change.settings |= note_facts(facts);
                    inner.kept.restart |= line.reason.is_none();
                    change.saves = Some(line.reason);
                }
                Heard::Chose(line) if line.protocol != PROTOCOL => {}
                Heard::Chose(line) => {
                    tracing::info!(reason = ?line.reason, note = %line.note, "the Epic games folder was asked to change");
                    let mut facts = facts();
                    facts.base = Some(PathBuf::from(line.base));
                    change.settings |= note_facts(facts);
                    inner.kept.restart |= line.reason.is_none();
                    change.chose = Some(line.reason);
                    // What is installed may be measured against a new folder:
                    // the sizes asked so far carry the old one.
                    inner.sizes.clear();
                }
                Heard::Removed(line) => {
                    inner.removing.remove(&line.app_name);
                    if line.protocol != PROTOCOL {
                        continue;
                    }
                    tracing::info!(app = %line.app_name, removed = line.removed, reason = ?line.reason, note = %line.note, "an Epic game's uninstall ended");
                    inner.kept.restart |= line.removed;
                    change.removed = Some(Ended {
                        title: inner.title_of(&line.app_name).to_string(),
                        app_name: line.app_name,
                        worked: line.removed,
                        reason: line.reason,
                        repair: false,
                    });
                    asks.push(Ask::Library { refresh: false });
                    change.rows = true;
                }
                Heard::Fetching(generation, line) => {
                    let Some(fetch) = inner
                        .fetch
                        .as_mut()
                        .filter(|fetch| fetch.generation == generation)
                    else {
                        continue;
                    };
                    if line.protocol != PROTOCOL || line.app_name != fetch.app_name {
                        continue;
                    }
                    fetch.so_far = SoFar {
                        step: line.step,
                        progress: line.progress.or(fetch.so_far.progress),
                        size: line.size.or(fetch.so_far.size),
                        updating: fetch.so_far.updating,
                        repairing: fetch.so_far.repairing,
                    };
                    if line.step == Step::Failed {
                        tracing::warn!(app = %line.app_name, reason = ?line.reason, note = %line.note, "an Epic game was not installed");
                        fetch.failed = Some(line.reason.unwrap_or(Reason::Legendary));
                    }
                    change.rows = true;
                }
                Heard::FetchEnded(generation) => {
                    let ours = inner
                        .fetch
                        .as_ref()
                        .is_some_and(|fetch| fetch.generation == generation);
                    if !ours {
                        // One the shell stopped itself, already forgotten.
                        continue;
                    }
                    let Some(fetch) = inner.fetch.take() else {
                        continue;
                    };
                    let worked = fetch.so_far.step == Step::Done;
                    // Refused because Heroic is open: not a failure to report
                    // but a wait. Back to the head of the queue, and the queue
                    // held until Heroic has gone — somebody who pressed
                    // Install while a game was running has said what they
                    // want, and it happens when it can.
                    if fetch.failed == Some(Reason::HeroicRunning) {
                        tracing::info!(app = %fetch.app_name, "an Epic download waits for Heroic to close");
                        inner.waiting.push_front(fetch.app_name);
                        inner.held = Some(std::time::Instant::now());
                        change.rows = true;
                        continue;
                    }
                    // A helper that ended without a last word died, which is
                    // a download that did not happen.
                    let reason = (!worked).then(|| fetch.failed.unwrap_or(Reason::Legendary));
                    tracing::info!(app = %fetch.app_name, worked, ?reason, "an Epic download ended");
                    if worked {
                        inner
                            .landed
                            .insert(fetch.app_name.clone(), std::time::Instant::now());
                        inner.kept.restart = true;
                    }
                    inner.repairs.remove(&fetch.app_name);
                    change.fetched = Some(Ended {
                        title: inner.title_of(&fetch.app_name).to_string(),
                        app_name: fetch.app_name,
                        worked,
                        reason,
                        repair: fetch.so_far.repairing,
                    });
                    asks.push(Ask::Library { refresh: false });
                    inner.next_fetch();
                    change.rows = true;
                }
                Heard::SignIn(generation, line) => {
                    let Some(signing) = inner
                        .signing
                        .as_mut()
                        .filter(|s| s.generation == generation)
                    else {
                        continue;
                    };
                    if !line.usable() {
                        continue;
                    }
                    change.panel = true;
                    match line {
                        SignInLine::Code {
                            code,
                            url,
                            short_url,
                            ..
                        } => {
                            signing.stage = Sign::Code {
                                code,
                                address: short_url
                                    .trim_start_matches("https://")
                                    .trim_start_matches("www.")
                                    .to_string(),
                                qr: lxb_steam::qr::encode(&url),
                                page: epics_own_page(&url).then_some(url),
                            };
                        }
                        SignInLine::Approved { .. } => signing.stage = Sign::Approved,
                        SignInLine::SignedIn { name, .. } => {
                            tracing::info!("Heroic is signed in to Epic");
                            inner.signing = None;
                            inner.account = Some(name.clone());
                            inner.covers_asked = false;
                            inner.kept.restart = true;
                            change.signed_in = Some(name);
                            change.rows = true;
                            asks.push(Ask::Library { refresh: false });
                            asks.push(Ask::Library { refresh: true });
                        }
                        SignInLine::Failed {
                            reason, url, note, ..
                        } => {
                            tracing::warn!(?reason, note, "an Epic sign-in did not go through");
                            signing.stage = Sign::Failed {
                                reason,
                                qr: url.as_deref().and_then(lxb_steam::qr::encode),
                            };
                        }
                        SignInLine::Unknown => {}
                    }
                }
                Heard::SignInEnded(generation) => {
                    // A helper that ended without a last word — it could not
                    // be run, or died — is a sign-in that did not work.
                    if let Some(signing) = inner
                        .signing
                        .as_mut()
                        .filter(|s| s.generation == generation)
                        .filter(|s| !matches!(s.stage, Sign::Failed { .. }))
                    {
                        signing.stage = Sign::Failed {
                            reason: Reason::Legendary,
                            qr: None,
                        };
                        change.panel = true;
                    }
                }
                Heard::Broken(why) => {
                    tracing::warn!(%why, "the Epic Games helper did not answer");
                    if matches!(inner.found, Found::Asking) {
                        inner.broken = Some(why);
                        change.rows = true;
                    }
                }
            }
        }
        // A queue held for Heroic asks after it now and then, and goes on by
        // itself when it has gone.
        if let Some(inner) = self.inner.as_mut() {
            let due = inner
                .held
                .is_some_and(|since| since.elapsed() >= HELD_ASKED);
            if due {
                inner.held = Some(std::time::Instant::now());
                asks.push(Ask::Probe);
            }
        }
        for ask in asks {
            self.send(ask);
        }
        if start_background {
            self.start_background();
        }
        change
    }

    /// What pressing the row means now.
    pub fn press(&self) -> Press {
        let Some(inner) = self.inner.as_ref() else {
            return Press::Waiting;
        };
        if inner.broken.is_some() {
            return Press::Cannot(
                crate::i18n::text("shell-not-working-on-this-machine").to_string(),
            );
        }
        if inner.installing.is_some() {
            return Press::Waiting;
        }
        match &inner.found {
            Found::Asking => Press::Waiting,
            Found::Absent { flatpak: true } => Press::Install,
            Found::Absent { flatpak: false } => Press::Cannot(
                crate::i18n::text("shell-it-cannot-be-downloaded-on-this-machine").to_string(),
            ),
            Found::Here { proton: false, .. } => Press::SetUp,
            Found::Here { .. } if inner.account.is_none() => Press::SignIn,
            Found::Here { .. } => Press::Enter,
        }
    }

    /// The line under the row. `None` on a machine without the integration,
    /// which takes the row off the bar.
    pub fn note(&self) -> Option<String> {
        let inner = self.inner.as_ref()?;
        if let Some(installing) = &inner.installing {
            return Some(installing_sentence(installing));
        }
        if inner.broken.is_some() {
            return Some(crate::i18n::text("shell-not-working-on-this-machine").to_string());
        }
        // A game coming down is what the row is about while it does, the way
        // the setup was: the row is where somebody looks to see it moving.
        if let Some(fetch) = &inner.fetch {
            return Some(format!(
                "{} · {}",
                inner.title_of(&fetch.app_name),
                so_far_sentence(&fetch.so_far)
            ));
        }
        if let Some(first) = inner.waiting.front().filter(|_| inner.held.is_some()) {
            return Some(format!(
                "{} · {}",
                inner.title_of(first),
                crate::i18n::text("epic-waits-for-heroic")
            ));
        }
        Some(match &inner.found {
            Found::Asking => crate::i18n::text("epic-looking").to_string(),
            Found::Absent { flatpak: true } => {
                crate::i18n::text("shell-press-to-download-it").to_string()
            }
            Found::Absent { flatpak: false } => {
                crate::i18n::text("shell-it-cannot-be-downloaded-on-this-machine").to_string()
            }
            Found::Here { proton: false, .. } => {
                crate::i18n::text("epic-press-to-finish-setting-up").to_string()
            }
            Found::Here { .. } => match &inner.account {
                Some(name) if inner.offline => format!(
                    "{} · {}",
                    crate::message!("steam-signed-in", "name" => name.as_str()),
                    crate::i18n::text("epic-no-connection")
                ),
                Some(name) => crate::message!("steam-signed-in", "name" => name.as_str()),
                None => crate::i18n::text("epic-sign-in-to-play").to_string(),
            },
        })
    }

    /// The row's line as a bar, while a setup or a game's download counts up.
    pub fn arriving(&self) -> Option<f32> {
        let inner = self.inner.as_ref()?;
        match &inner.installing {
            Some(installing) => installing.progress,
            None => inner.fetch.as_ref()?.so_far.progress,
        }
    }

    /// The Epic Games column, in the shape the Steam column has: the field
    /// that searches it and the row that empties it, the Alphabetical index,
    /// and then the games in the order chosen under Sort. Empty — which takes
    /// the column away — for a machine where nobody is signed in.
    ///
    /// The index is built out of the library's own order, installed first and
    /// each half by name, whatever the column is listed in: what a letter is
    /// asked is still "which of these can I play". See
    /// [`crate::steam::Steam::alphabetical`], which this shares.
    pub fn rows(&self) -> Vec<Entry> {
        let Some(inner) = self.inner.as_ref() else {
            return Vec::new();
        };
        let Found::Here { command, .. } = &inner.found else {
            return Vec::new();
        };
        if inner.account.is_none() || inner.games.is_empty() {
            return Vec::new();
        }
        use lxb_steam::library::Sort;
        let mut library: Vec<&Game> = inner.games.iter().collect();
        library.sort_by(|a, b| compare(Sort::InstalledFirst, a, b));
        let total = library.len();
        let matched: Vec<&Game> = match lxb_steam::library::sought(&inner.search) {
            Some(needle) => library
                .into_iter()
                .filter(|game| lxb_steam::library::sort_key(&game.title).contains(&needle))
                .collect(),
            None => library,
        };
        let mut rows = Vec::with_capacity(matched.len() + 3);
        apps::head(
            &mut rows,
            apps::Searched::Epic,
            &inner.search,
            matched.len(),
            total,
        );
        if matched.is_empty() {
            return rows;
        }
        rows.push(crate::steam::Steam::index(
            matched
                .iter()
                .map(|game| (initial(&game.title), game_row(inner, command, game))),
            mark(),
        ));
        let mut listing = matched;
        if inner.sort != Sort::InstalledFirst {
            listing.sort_by(|a, b| compare(inner.sort, a, b));
        }
        rows.extend(
            listing
                .into_iter()
                .map(|game| game_row(inner, command, game)),
        );
        rows
    }

    /// Narrow the column to what is being typed. Whether that changed it.
    pub fn set_search(&mut self, query: &str) -> bool {
        let Some(inner) = self.inner.as_mut() else {
            return false;
        };
        if inner.search == query {
            return false;
        }
        inner.search = query.to_string();
        true
    }

    /// List the column in another order. Whether that changed it.
    pub fn set_sort(&mut self, sort: lxb_steam::library::Sort) -> bool {
        let Some(inner) = self.inner.as_mut() else {
            return false;
        };
        if inner.sort == sort {
            return false;
        }
        inner.sort = sort;
        true
    }

    /// The order the column is listed in.
    pub fn sort(&self) -> lxb_steam::library::Sort {
        self.inner
            .as_ref()
            .map_or_else(Default::default, |inner| inner.sort)
    }

    /// What this library can be sorted by — an order nothing can be sorted by
    /// is offered greyed, as Steam's Sort list does.
    pub fn orders(&self) -> lxb_steam::library::Orders {
        let games = self
            .inner
            .as_ref()
            .map_or(&[][..], |inner| &inner.games[..]);
        lxb_steam::library::Orders {
            sizes: games
                .iter()
                .any(|game| game.installed.as_ref().is_some_and(|here| here.size > 0)),
            playtimes: games.iter().any(|game| game.played_minutes > 0),
            played: games.iter().any(|game| game.last_played.is_some()),
        }
    }

    /// The sign-in panel, where one is up.
    pub fn panel(&self) -> Option<Panel> {
        let sign = self.signing()?;
        let heading = dialog::Line::Heading(TITLE.to_string());
        let cancel = menu::Entry::new(menu::Command::EpicCancel, crate::i18n::text("shell-cancel"));
        Some(match sign {
            Sign::Asking => Panel {
                lines: vec![
                    heading,
                    dialog::Line::Note(crate::i18n::text("epic-asking-for-a-code").to_string()),
                    dialog::Line::Waiting,
                    dialog::Line::Rule,
                ],
                buttons: vec![cancel],
            },
            Sign::Code {
                code,
                address,
                qr,
                page,
            } => Panel {
                lines: {
                    let mut lines = vec![
                        heading,
                        dialog::Line::Note(
                            crate::i18n::text("epic-scan-with-your-phone").to_string(),
                        ),
                    ];
                    if let Some(qr) = qr {
                        lines.push(dialog::Line::Qr(qr.clone()));
                    }
                    lines.push(dialog::Line::Note(
                        crate::message!("epic-or-enter-the-code", "address" => address.as_str()),
                    ));
                    lines.push(dialog::Line::Heading(code.clone()));
                    lines.push(dialog::Line::Note(
                        crate::i18n::text("epic-may-say-fortnite").to_string(),
                    ));
                    lines.push(dialog::Line::Rule);
                    lines
                },
                // The other way in, for somebody with no phone to hand and no
                // second device: Epic's own page, in a window on this screen,
                // with the same code filled in. The sign-in goes on waiting
                // for it exactly as it waits for the phone.
                buttons: match page {
                    Some(_) => vec![
                        menu::Entry::new(
                            menu::Command::EpicSignInHere,
                            crate::i18n::text("epic-sign-in-on-this-screen"),
                        ),
                        cancel,
                    ],
                    None => vec![cancel],
                },
            },
            Sign::Approved => Panel {
                lines: vec![
                    heading,
                    dialog::Line::Note(crate::i18n::text("epic-signing-in").to_string()),
                    dialog::Line::Waiting,
                ],
                buttons: Vec::new(),
            },
            Sign::Failed { reason, qr } => Panel {
                lines: {
                    let mut lines = vec![heading, dialog::Line::Note(reason_sentence(*reason))];
                    if let Some(qr) = qr {
                        lines.push(dialog::Line::Qr(qr.clone()));
                    }
                    lines.push(dialog::Line::Rule);
                    lines
                },
                buttons: vec![
                    menu::Entry::new(
                        menu::Command::EpicSignIn,
                        crate::i18n::text("shell-try-again"),
                    ),
                    menu::Entry::new(menu::Command::EpicCancel, crate::i18n::text("shell-close")),
                ],
            },
        })
    }

    /// The menu over the Epic Games row — the Steam row's, for Epic: what is
    /// asked of the account above the rule (fetch the library again, sign
    /// out), and below it the column's order, Heroic's own window — as the
    /// Steam row carries Open Steam — and the way out. Empty — no menu at
    /// all — while the helper has not answered.
    pub fn row_menu(&self) -> Vec<menu::Entry> {
        let mut rows = Vec::new();
        let press = self.press();
        match press {
            Press::Waiting | Press::Cannot(_) => return rows,
            Press::Install | Press::SetUp => rows.push(
                menu::Entry::new(
                    menu::Command::EpicOfferInstall,
                    crate::i18n::text("shell-install"),
                )
                .glyph(icons::LAUNCH),
            ),
            Press::SignIn => rows.push(
                menu::Entry::new(
                    menu::Command::EpicSignIn,
                    crate::i18n::text("shell-sign-in"),
                )
                .glyph(icons::LAUNCH),
            ),
            Press::Enter => {
                rows.push(
                    menu::Entry::new(
                        menu::Command::EpicRefresh,
                        crate::i18n::text("shell-refresh-the-library"),
                    )
                    .glyph(icons::REFRESH),
                );
                rows.push(
                    menu::Entry::new(
                        menu::Command::EpicSignOut,
                        crate::i18n::text("shell-sign-out"),
                    )
                    .glyph(icons::SIGN_OUT)
                    .grave(),
                );
            }
        }
        if press == Press::Enter && !self.rows().is_empty() {
            rows.push(
                menu::Entry::new(menu::Command::EpicSort, crate::i18n::text("shell-sort"))
                    .glyph(icons::SORT)
                    .group(1),
            );
        }
        if matches!(press, Press::SetUp | Press::SignIn | Press::Enter) {
            rows.push(
                menu::Entry::new(
                    menu::Command::EpicOpenHeroic,
                    crate::i18n::text("epic-open-heroic"),
                )
                .group(1),
            );
        }
        rows.push(
            menu::Entry::new(menu::Command::Dismiss, crate::i18n::text("shell-cancel")).group(1),
        );
        rows
    }
}

/// A shell that ends ends its sign-in: the helper would otherwise go on asking
/// Epic about a code for half an hour with nobody left to show it to. And its
/// download, which a shell that starts again could not see and would start a
/// second time beside; what arrived stays, and the next press carries on.
impl Drop for Heroic {
    fn drop(&mut self) {
        self.cancel_sign_in();
        if let Some(inner) = self.inner.as_mut() {
            inner.waiting.clear();
            inner.end_fetch();
        }
    }
}

/// The three questions a game of the column asks before anything is done to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Question {
    Install,
    Stop,
    Uninstall,
    /// An installed game with a newer build waiting: update it first, or play
    /// what is here.
    Update,
}

/// A question about one game, as it is on screen: which game it is about,
/// which question, and the buttons it was drawn with — so a size landing or a
/// download moving redraws it, and its answer knows which game it was about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asked {
    pub app_name: String,
    pub question: Question,
    pub buttons: Vec<menu::Command>,
}

/// A panel's worth of lines and buttons.
pub struct Panel {
    pub lines: Vec<dialog::Line>,
    /// Empty for a panel that is waiting on something and offers nothing.
    pub buttons: Vec<menu::Entry>,
}

impl Heroic {
    /// The menu over one game of the column — Steam's game menu, row for
    /// row, so the two libraries are driven the same way: what can be done
    /// to the game first (play or install it, check its files, choose what it
    /// runs with, take it off), then how many pixels it draws, then the
    /// column's order, then the way out. Heroic's own window is not here: it
    /// is the Epic Games row's, as Steam's is the Steam row's.
    ///
    /// Update now is the one row Steam has no twin of, because Steam updates
    /// its games itself and Heroic's shortcut starts one without looking.
    pub fn game_menu(&self, game: &apps::EpicGame) -> Vec<menu::Entry> {
        let mut rows = Vec::new();
        let here = game.start.is_some();
        let doing = self.doing(&game.app_name);
        // Epic's own files, which are what a check can check: not a game
        // another store downloads into Heroic's prefix.
        let epics_own = self.inner.as_ref().is_some_and(|inner| {
            inner
                .games
                .iter()
                .any(|known| known.app_name == game.app_name && known.store.is_none())
        });
        match doing {
            Doing::Nothing if here => {
                if self.has_update(&game.app_name) {
                    rows.push(menu::Entry::new(
                        menu::Command::EpicUpdate,
                        crate::i18n::text("shell-update-now"),
                    ));
                }
                rows.push(
                    menu::Entry::new(menu::Command::EpicPlay, crate::i18n::text("shell-play"))
                        .glyph(icons::LAUNCH),
                );
                if epics_own {
                    rows.push(menu::Entry::new(
                        menu::Command::EpicVerify,
                        crate::i18n::text("epic-verify-and-repair"),
                    ));
                }
            }
            Doing::Nothing => rows.push(
                menu::Entry::new(
                    menu::Command::EpicOfferGet,
                    crate::i18n::text("shell-install"),
                )
                .glyph(icons::LAUNCH),
            ),
            Doing::Waiting | Doing::Held | Doing::Fetching(_) => rows.push(menu::Entry::new(
                menu::Command::EpicOfferStop,
                crate::i18n::text("shell-stop-installing"),
            )),
            Doing::Removing => {}
        }
        // What it runs with is a setting on the game rather than something
        // done to it, so it is offered whether or not the game is here — as
        // Steam's own properties window offers it.
        rows.push(menu::Entry::new(
            menu::Command::EpicCompatibility,
            crate::i18n::text("label-compatibility"),
        ));
        if here && doing == Doing::Nothing {
            rows.push(
                menu::Entry::new(
                    menu::Command::EpicOfferRemove,
                    crate::i18n::text("shell-uninstall"),
                )
                .glyph(icons::UNINSTALL)
                .grave(),
            );
        }
        rows.push(
            menu::Entry::new(
                menu::Command::Resolution,
                crate::i18n::text("shell-resolution"),
            )
            .glyph(icons::SETTING_RESOLUTION),
        );
        rows.push(
            menu::Entry::new(menu::Command::EpicSort, crate::i18n::text("shell-sort"))
                .glyph(icons::SORT)
                .group(1),
        );
        rows.push(
            menu::Entry::new(menu::Command::Dismiss, crate::i18n::text("shell-cancel")).group(1),
        );
        rows
    }

    /// One of the questions about a game, as it stands now. `None` where the
    /// question no longer applies — the download it offered to stop has ended.
    pub fn question(&self, question: Question, app_name: &str) -> Option<Panel> {
        match question {
            Question::Install => self.install_panel(app_name),
            Question::Stop => self.stop_panel(app_name),
            Question::Uninstall => self.uninstall_panel(app_name),
            Question::Update => self.update_panel(app_name),
        }
    }

    /// Whether a game on this disk has a newer build waiting.
    /// Run a game with one of the tools Heroic lists, or with Heroic's default
    /// where there is no name; or, with no game, make one Heroic's default.
    /// The answer arrives as [`Change::tool_set`].
    pub fn use_tool(&mut self, game: Option<&str>, name: Option<&str>) {
        tracing::info!(?game, ?name, "an Epic game is to run with another tool");
        self.send(Ask::UseTool {
            game: game.map(str::to_string),
            name: name.map(str::to_string),
        });
    }

    /// The rows under a game's Compatibility: Heroic's default first — the
    /// choice that takes a game's own back, as "Steam's choice" is on a Steam
    /// game — then everything Heroic can run it with, the one in force
    /// ticked. A row saying so while Heroic has not been asked yet.
    pub fn compatibility_rows(&self, app_name: &str) -> Vec<menu::Entry> {
        let mut rows = Vec::new();
        let own = self
            .inner
            .as_ref()
            .and_then(|inner| inner.own_tools.as_ref());
        match own {
            None => rows.push(
                menu::Entry::new(
                    menu::Command::Dismiss,
                    crate::i18n::text("epic-asking-heroic"),
                )
                .reading(),
            ),
            Some(own) => {
                let chosen = own.get(app_name).map(String::as_str);
                let facts = facts();
                let default = match &facts.proton {
                    Some(tool) => {
                        crate::message!("epic-heroics-default-is", "tool" => tool.as_str())
                    }
                    None => crate::i18n::text("epic-heroics-default").to_string(),
                };
                rows.push(chosen_when(
                    menu::Entry::new(menu::Command::EpicRunWith(None), default),
                    chosen.is_none(),
                ));
                for tool in &facts.tools {
                    rows.push(chosen_when(
                        menu::Entry::new(
                            menu::Command::EpicRunWith(Some(crate::settings::intern(tool))),
                            tool.clone(),
                        ),
                        chosen == Some(tool.as_str()),
                    ));
                }
            }
        }
        rows.push(
            menu::Entry::new(menu::Command::Dismiss, crate::i18n::text("shell-cancel")).group(1),
        );
        rows
    }

    /// Tell the column whether this machine can reach the internet —
    /// NetworkManager's answer, `None` where it has none. Whether that
    /// changed anything the rows say.
    ///
    /// Only a plain "no" counts as offline: a machine with no NetworkManager
    /// is not a machine with no network, and a game refused or a row
    /// hedged on a guess would be worse than one that fails and says why.
    pub fn set_online(&mut self, online: Option<bool>) -> bool {
        let Some(inner) = self.inner.as_mut() else {
            return false;
        };
        let offline = online == Some(false);
        if inner.offline == offline {
            return false;
        }
        inner.offline = offline;
        tracing::info!(
            offline,
            "the Epic column heard whether the machine is online"
        );
        true
    }

    /// Whether this machine has no way to the internet, as the shell last
    /// heard. See [`Self::set_online`].
    pub fn offline(&self) -> bool {
        self.inner.as_ref().is_some_and(|inner| inner.offline)
    }

    /// Whether a press on a game asks about its update first: there is one,
    /// and it could come down. With no connection it could not, so the press
    /// plays what is here — Heroic starts a game offline by itself, and skips
    /// the check that would refuse an old one.
    pub fn offers_update(&self, app_name: &str) -> bool {
        self.has_update(app_name) && !self.offline()
    }

    pub fn has_update(&self, app_name: &str) -> bool {
        self.inner.as_ref().is_some_and(|inner| {
            inner.games.iter().any(|game| {
                game.app_name == app_name
                    && game
                        .installed
                        .as_ref()
                        .is_some_and(|installed| installed.update)
            })
        })
    }

    /// The question a press on a game with an update waiting asks. Heroic's
    /// own shortcut starts a game without looking — `--no-gui` skips the
    /// version check — so an update nobody is asked about is an update that
    /// never happens, and an online game that then will not let them in.
    fn update_panel(&self, app_name: &str) -> Option<Panel> {
        if !self.has_update(app_name) || self.doing(app_name) != Doing::Nothing {
            return None;
        }
        let inner = self.inner.as_ref()?;
        Some(Panel {
            lines: vec![
                dialog::Line::Heading(inner.title_of(app_name).to_string()),
                dialog::Line::Note(
                    crate::i18n::text("shell-this-game-has-an-update-waiting").to_string(),
                ),
                dialog::Line::Rule,
            ],
            buttons: vec![
                menu::Entry::new(
                    menu::Command::EpicGet,
                    crate::i18n::text("shell-update-now"),
                ),
                menu::Entry::new(menu::Command::EpicPlayNow, crate::i18n::text("shell-play")),
            ],
        })
    }

    /// The question a press on a game that is not here asks: whether to
    /// install it, with what it takes and whether there is room for it.
    ///
    /// Drawn before the size is known, waiting for it, and drawn again when
    /// it lands — see [`Change::sized`].
    pub fn install_panel(&self, app_name: &str) -> Option<Panel> {
        let inner = self.inner.as_ref()?;
        let game = inner.games.iter().find(|game| game.app_name == app_name)?;
        if game.installed.is_some() || inner.doing(app_name) != Doing::Nothing {
            return None;
        }
        let mut lines = vec![dialog::Line::Heading(game.title.clone())];
        let mut room_for_it = true;
        match game.store {
            Some(Store::Ubisoft) => {
                lines.push(dialog::Line::Note(
                    crate::i18n::text("epic-plays-through-ubisoft").to_string(),
                ));
                lines.push(dialog::Line::Note(
                    crate::i18n::text("epic-ubisoft-downloads-it").to_string(),
                ));
            }
            Some(Store::Ea) => {
                lines.push(dialog::Line::Note(
                    crate::i18n::text("epic-plays-through-ea").to_string(),
                ));
                lines.push(dialog::Line::Note(
                    crate::i18n::text("epic-ea-downloads-it").to_string(),
                ));
            }
            Some(Store::Other) => {
                lines.push(dialog::Line::Note(
                    crate::i18n::text("shell-it-cannot-be-downloaded-on-this-machine").to_string(),
                ));
                lines.push(dialog::Line::Rule);
                return Some(Panel {
                    lines,
                    buttons: vec![menu::Entry::new(
                        menu::Command::Dismiss,
                        crate::i18n::text("shell-close"),
                    )],
                });
            }
            None => match inner.sizes.get(app_name) {
                None => {
                    lines.push(dialog::Line::Note(
                        crate::i18n::text("shell-this-game-is-not-on-this-machine").to_string(),
                    ));
                    lines.push(dialog::Line::Waiting);
                }
                // Epic could not be asked, so neither can it be downloaded
                // from: said now, rather than after an Install that fails.
                Some(measured) if measured.unreachable => {
                    room_for_it = false;
                    lines.push(dialog::Line::Note(
                        crate::i18n::text("epic-could-not-be-reached").to_string(),
                    ));
                }
                Some(measured) => {
                    lines.push(dialog::Line::Note(
                        match (measured.download, measured.disk) {
                            (Some(download), Some(disk)) => crate::message!(
                                "epic-download-and-disk",
                                "download" => crate::steam::format_size(download),
                                "disk" => crate::steam::format_size(disk)
                            ),
                            _ => crate::i18n::text("shell-this-game-is-not-on-this-machine")
                                .to_string(),
                        },
                    ));
                    let room = measured.base.as_deref().and_then(room_at);
                    match (room, measured.disk) {
                        (Some(free), Some(disk)) if free < disk => {
                            room_for_it = false;
                            lines.push(dialog::Line::Note(crate::message!(
                                "epic-not-enough-space",
                                "size" => crate::steam::format_size(disk)
                            )));
                        }
                        (Some(free), _) => lines.push(dialog::Line::Note(crate::message!(
                            "steam-free-on-this-machine",
                            "free" => crate::steam::format_size(free)
                        ))),
                        (None, _) => {}
                    }
                }
            },
        }
        // Not a refusal — the queue takes it — but the difference between a
        // download that starts now and one that starts in an hour.
        if let Some(first) = inner
            .fetch
            .as_ref()
            .filter(|fetch| fetch.app_name != app_name)
        {
            lines.push(dialog::Line::Note(crate::message!(
                "steam-downloading-first",
                "name" => inner.title_of(&first.app_name)
            )));
        }
        lines.push(dialog::Line::Rule);
        let buttons = if room_for_it {
            vec![
                menu::Entry::new(menu::Command::EpicGet, crate::i18n::text("shell-install")),
                menu::Entry::new(menu::Command::Dismiss, crate::i18n::text("shell-not-now")),
            ]
        } else {
            vec![menu::Entry::new(
                menu::Command::Dismiss,
                crate::i18n::text("shell-close"),
            )]
        };
        Some(Panel { lines, buttons })
    }

    /// The question a press on a game coming down asks: whether to stop.
    pub fn stop_panel(&self, app_name: &str) -> Option<Panel> {
        let inner = self.inner.as_ref()?;
        let mut lines = vec![dialog::Line::Heading(inner.title_of(app_name).to_string())];
        match inner.doing(app_name) {
            Doing::Fetching(so_far) => {
                lines.push(dialog::Line::Note(so_far_sentence(&so_far)));
                lines.push(dialog::Line::Note(
                    crate::i18n::text("epic-stopping-keeps-what-arrived").to_string(),
                ));
            }
            Doing::Waiting => lines.push(dialog::Line::Note(
                crate::i18n::text("integration-waiting-to-download").to_string(),
            )),
            Doing::Held => lines.push(dialog::Line::Note(
                crate::i18n::text("epic-waits-for-heroic").to_string(),
            )),
            Doing::Nothing | Doing::Removing => return None,
        }
        lines.push(dialog::Line::Rule);
        Some(Panel {
            lines,
            buttons: vec![
                menu::Entry::new(
                    menu::Command::Dismiss,
                    crate::i18n::text("shell-keep-installing"),
                ),
                menu::Entry::new(
                    menu::Command::EpicStop,
                    crate::i18n::text("shell-stop-installing"),
                ),
            ],
        })
    }

    /// The question before a game is uninstalled, which is the one press here
    /// that cannot be taken back — so it is asked, and the harmless answer is
    /// drawn first and stood on.
    pub fn uninstall_panel(&self, app_name: &str) -> Option<Panel> {
        let inner = self.inner.as_ref()?;
        let game = inner.games.iter().find(|game| game.app_name == app_name)?;
        game.installed.as_ref()?;
        Some(Panel {
            lines: vec![
                dialog::Line::Heading(game.title.clone()),
                // What is being taken off, whatever the connection: its size and
                // its playtime rather than what a press would do.
                dialog::Line::Note(game_note(game, false)),
                dialog::Line::Note(crate::i18n::text("shell-it-stays-in-the-library").to_string()),
                dialog::Line::Rule,
            ],
            buttons: vec![
                menu::Entry::new(menu::Command::Dismiss, crate::i18n::text("shell-keep-it")),
                menu::Entry::new(
                    menu::Command::EpicRemove,
                    crate::i18n::text("shell-uninstall"),
                )
                .grave(),
            ],
        })
    }

    /// The Epic half of the Trophies column: every game Epic answered for
    /// that has achievements — played or not, here or not, as the Steam half
    /// lists what the account owns — with its own under it, in the shape the
    /// other two halves give theirs, so the column sorts and groups it with
    /// them.
    ///
    /// A game Epic could not be asked about, or one with none, is left out
    /// rather than listed as nothing: the column is a list of what somebody
    /// has to show for their playing.
    pub fn trophy_rows(&self) -> Vec<Entry> {
        use crate::trophies::{Key, Platform, Row};
        let Some(inner) = self.inner.as_ref() else {
            return Vec::new();
        };
        if inner.account.is_none() {
            return Vec::new();
        }
        let mut rows = Vec::new();
        for (app_name, line) in &inner.trophies {
            if line.total == 0 || line.list.is_empty() {
                continue;
            }
            let game = inner.games.iter().find(|game| &game.app_name == app_name);
            let unlocked = line.list.iter().filter(|trophy| trophy.unlocked).count();
            let locked = line.list.len() - unlocked;
            let achievements = line
                .list
                .iter()
                .map(|trophy| {
                    let secret = trophy.hidden && !trophy.unlocked;
                    let state = match (trophy.unlocked, trophy.unlocked_at) {
                        (true, Some(at)) => crate::trophies::unlocked_at(at)
                            .unwrap_or_else(|| crate::i18n::text("shell-unlocked").to_string()),
                        (true, None) => crate::i18n::text("shell-unlocked").to_string(),
                        (false, _) => crate::i18n::text("shell-locked").to_string(),
                    };
                    let description = if secret {
                        ""
                    } else {
                        trophy.description.as_str()
                    };
                    let mut about = vec![
                        (
                            crate::i18n::text("shell-description").to_string(),
                            description.to_string(),
                        ),
                        (crate::i18n::text("shell-status").to_string(), state.clone()),
                    ];
                    if let Some(rarity) = trophy.rarity {
                        about.push((
                            crate::i18n::text("epic-players-who-have-it").to_string(),
                            format!("{rarity:.1}%"),
                        ));
                    }
                    Entry::Trophy(Row {
                        key: Key::EpicAchievement(app_name.clone(), trophy.name.clone()),
                        facts: apps::Facts {
                            title: if secret || trophy.title.is_empty() {
                                crate::i18n::text("shell-hidden-achievement").to_string()
                            } else {
                                trophy.title.clone()
                            },
                            comment: crate::message!(
                                "epic-achievement-summary",
                                "description" => description,
                                "xp" => trophy.xp,
                                "state" => state
                            ),
                            icon: crate::icons::CATEGORY_TROPHIES.into(),
                            about: apps::About::Listed(about),
                        },
                        picture: trophy.icon.as_ref().map(PathBuf::from),
                        entries: None,
                        section: Some(if trophy.unlocked {
                            crate::message!("achievements-unlocked-count", "count" => unlocked)
                        } else {
                            crate::message!("achievements-locked-count", "count" => locked)
                        }),
                        shape: None,
                        installed: None,
                        platform: None,
                    })
                })
                .collect();
            let cover = game.and_then(|game| game.cover_file.as_deref());
            rows.push(Entry::Trophy(Row {
                key: Key::EpicGame(app_name.clone()),
                facts: apps::Facts {
                    title: game.map_or_else(|| app_name.clone(), |game| game.title.clone()),
                    comment: format!(
                        "{TITLE} · {}",
                        crate::message!(
                            "achievements-unlocked-of",
                            "unlocked" => line.unlocked,
                            "total" => line.total
                        )
                    ),
                    icon: crate::icons::CATEGORY_TROPHIES.into(),
                    about: apps::About::Listed(Vec::new()),
                },
                picture: cover.map(PathBuf::from),
                entries: Some(achievements),
                section: None,
                // Epic's own box, at its own shape — see `ui::cards_in`.
                shape: Some(
                    cover
                        .and_then(|at| inner.shapes.get(Path::new(at)).copied().flatten())
                        .unwrap_or(0.75),
                ),
                installed: Some(game.is_some_and(|game| game.installed.is_some())),
                platform: Some(Platform {
                    name: TITLE.to_string(),
                    mark: mark().to_string(),
                }),
            }));
        }
        rows
    }

    /// A game's title, where the library has it.
    pub fn title(&self, app_name: &str) -> Option<String> {
        self.inner
            .as_ref()?
            .games
            .iter()
            .find(|game| game.app_name == app_name)
            .map(|game| game.title.clone())
    }

    /// A game's cover on this disk, for the notice that it has arrived.
    pub fn cover_of(&self, app_name: &str) -> Option<&str> {
        self.inner
            .as_ref()?
            .games
            .iter()
            .find(|game| game.app_name == app_name)?
            .cover_file
            .as_deref()
    }

    /// The game coming down, for the card in the corner of the guide — the
    /// card Steam's downloads stand on, because it is the same fact about the
    /// machine.
    ///
    /// Only the game being fetched. One waiting its turn, or held for Heroic
    /// to close, is not arriving, and a card saying "Downloading" over it
    /// would be a card about the wrong game — Steam's queue is under the same
    /// rule. The bar is the row's own reading, so the two cannot disagree.
    pub fn downloading(&self) -> Option<crate::steam::Coming> {
        let inner = self.inner.as_ref()?;
        let fetch = inner.fetch.as_ref()?;
        let so_far = fetch.so_far;
        Some(crate::steam::Coming {
            whose: crate::steam::Whose::Epic {
                app_name: fetch.app_name.clone(),
                cover: self.cover_of(&fetch.app_name).map(PathBuf::from),
            },
            name: inner.title_of(&fetch.app_name).to_string(),
            // The card's words are Steam's: checking a copy that is here,
            // bytes into a copy that is here, or the game arriving.
            verb: match (so_far.step, so_far.repairing || so_far.updating) {
                (Step::Checking, _) => crate::steam::CHECKING,
                (_, true) => crate::steam::UPDATING,
                (_, false) => crate::steam::DOWNLOADING,
            },
            share: so_far.progress.map(|share| share.clamp(0.0, 1.0)),
            stuck: false,
            a_download: true,
        })
    }
}

/// How much room is left where games go: the folder itself, or the nearest
/// folder above it that is there — Heroic's folder is made by the first
/// install.
fn room_at(base: &Path) -> Option<u64> {
    let there = base.ancestors().find(|at| at.is_dir())?;
    Some(crate::storage::room(there)?.free)
}

/// A row wearing the tick where it is the one in force.
fn chosen_when(row: menu::Entry, chosen: bool) -> menu::Entry {
    match chosen {
        true => row.glyph(icons::CHOSEN),
        false => row,
    }
}

/// How far a game's download has got, in the Steam column's own words.
pub fn so_far_sentence(so_far: &SoFar) -> String {
    if so_far.repairing {
        let percent = so_far.progress.map(|share| format!("{:.0}", share * 100.0));
        return match (so_far.step, percent) {
            (Step::Downloading, Some(percent)) => {
                crate::message!("epic-repairing-percent", "percent" => percent)
            }
            (Step::Downloading, None) => crate::i18n::text("epic-repairing").to_string(),
            (Step::Checking, Some(percent)) => {
                crate::message!("epic-checking-percent", "percent" => percent)
            }
            _ => crate::i18n::text("integration-checking-files").to_string(),
        };
    }
    if so_far.updating {
        return match (so_far.step, so_far.progress, so_far.size) {
            (Step::Downloading, Some(share), Some(size)) if size > 0 => crate::message!(
                "epic-updating-percent-of",
                "percent" => format!("{:.0}", share * 100.0),
                "size" => crate::steam::format_size(size)
            ),
            (Step::Downloading, Some(share), _) => crate::message!(
                "epic-updating-percent",
                "percent" => format!("{:.0}", share * 100.0)
            ),
            _ => crate::i18n::text("epic-updating").to_string(),
        };
    }
    match (so_far.step, so_far.progress, so_far.size) {
        (Step::Downloading, Some(share), Some(size)) if size > 0 => crate::message!(
            "steam-installing-percent-of",
            "percent" => format!("{:.0}", share * 100.0),
            "size" => crate::steam::format_size(size)
        ),
        (Step::Downloading, Some(share), _) => crate::message!(
            "steam-installing-percent",
            "percent" => format!("{:.0}", share * 100.0)
        ),
        _ => crate::i18n::text("shell-installing").to_string(),
    }
}

/// Why a game did not install, or would not uninstall, in plain words, where
/// there is something the person can do about it. `None` for the rest: the
/// panel already says it did not happen, and the log says why.
pub fn game_failed_sentence(reason: Reason) -> Option<String> {
    let key = match reason {
        Reason::HeroicRunning => "epic-heroic-is-busy",
        Reason::Offline => "epic-could-not-be-reached",
        Reason::NoSpace => "epic-no-space",
        Reason::SignedOut => "epic-sign-in-to-play",
        _ => return None,
    };
    Some(crate::i18n::text(key).to_string())
}

/// What an install is doing, in a sentence.
pub fn installing_sentence(installing: &Installing) -> String {
    let doing = match installing.stage {
        Stage::Installing => crate::i18n::text("epic-downloading-heroic"),
        Stage::Proton => crate::i18n::text("epic-downloading-proton"),
        _ => crate::i18n::text("epic-getting-ready"),
    };
    match installing.progress {
        Some(done) if installing.stage != Stage::Done => {
            format!("{doing} — {}%", (done * 100.0).round() as u32)
        }
        _ => doing.to_string(),
    }
}

/// Why something did not happen, in plain words.
pub fn reason_sentence(reason: Reason) -> String {
    crate::i18n::text(match reason {
        Reason::HeroicRunning => "epic-close-heroic-first",
        Reason::Offline => "epic-could-not-be-reached",
        Reason::Expired => "epic-code-ran-out",
        Reason::ActionNeeded => "epic-confirm-first",
        _ => "epic-sign-in-did-not-work",
    })
    .to_string()
}

/// Why a setup did not finish, in plain words, where there is something the
/// person can do about it. `None` for the rest, whose panel already says the
/// download did not happen — the reason is in the log.
pub fn setup_failed_sentence(reason: Reason) -> Option<String> {
    let key = match reason {
        Reason::HeroicRunning => "epic-close-heroic-first",
        Reason::Offline => "epic-could-not-be-reached",
        Reason::NoFlatpak => "shell-it-cannot-be-downloaded-on-this-machine",
        _ => return None,
    };
    Some(crate::i18n::text(key).to_string())
}

/// Whether an address is one of Epic's own pages, over HTTPS — the only kind
/// the shell will open in a browser on the helper's say-so.
fn epics_own_page(url: &str) -> bool {
    ["https://www.epicgames.com/", "https://epicgames.com/"]
        .iter()
        .any(|own| url.starts_with(own))
        && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// A command without the variable that turns Electron into plain Node, which
/// leaks into a session from some terminals and editors — measured on
/// 2026-09-24, when it stopped Heroic dead with `bad option: --no-gui`.
fn without_electron_as_node(command: &[String]) -> Vec<String> {
    let mut argv: Vec<String> = [
        "env",
        "-u",
        "ELECTRON_RUN_AS_NODE",
        "-u",
        "ELECTRON_NO_ATTACH_CONSOLE",
    ]
    .iter()
    .map(|word| word.to_string())
    .collect();
    argv.extend(command.iter().cloned());
    argv
}

/// What starts one game: Heroic's own shortcut, word for word, with its
/// window kept away for this launch. `None` for a name that is not Epic's
/// plain letters and digits, which is never a real one.
fn launch_argv(command: &[String], app_name: &str, left: bool) -> Option<Vec<String>> {
    let plain = !app_name.is_empty()
        && app_name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if !plain {
        return None;
    }
    // Left running after the game, Heroic is started detached and without
    // `--no-gui` — which is what closes it with its game — so what the shell
    // watches is a courier that hands the game over and goes, as it is when
    // Heroic is already up. The game is then followed by its window, and its
    // ending by Heroic's playtimes. See `Heroic::keep`.
    let mut argv = Vec::new();
    if left {
        argv.push("setsid".to_string());
        argv.push("-f".to_string());
    }
    argv.extend(without_electron_as_node(command));
    if !left {
        argv.push("--no-gui".to_string());
    }
    argv.push("--no-sandbox".to_string());
    argv.push(format!(
        "heroic://launch?appName={app_name}&runner=legendary&gui=false"
    ));
    Some(argv)
}

/// One game, as a row of the column — the one place a game becomes a row, so
/// the column and the index cannot come to different conclusions about it.
fn game_row(inner: &Inner, command: &[String], game: &Game) -> Entry {
    let (note, progress) = match inner.doing(&game.app_name) {
        Doing::Nothing => (game_note(game, inner.offline), None),
        Doing::Waiting => (
            crate::i18n::text("integration-waiting-to-download").to_string(),
            None,
        ),
        Doing::Held => (crate::i18n::text("epic-waits-for-heroic").to_string(), None),
        Doing::Removing => (crate::i18n::text("shell-removing").to_string(), None),
        Doing::Fetching(so_far) => (
            so_far_sentence(&so_far),
            so_far.progress.map(|share| apps::Arriving {
                share: share.clamp(0.0, 1.0),
                stuck: false,
            }),
        ),
    };
    Entry::EpicGame(apps::EpicGame {
        app_name: game.app_name.clone(),
        name: game.title.clone(),
        note,
        progress,
        installed: game.installed.is_some(),
        start: game.installed.as_ref().and_then(|_| {
            launch_argv(
                command,
                &game.app_name,
                crate::settings::epic_left_after_a_game(),
            )
        }),
        cover: game.cover_file.as_ref().map(PathBuf::from),
        shape: game
            .cover_file
            .as_deref()
            .and_then(|at| inner.shapes.get(Path::new(at)).copied().flatten()),
        hero: game.hero_file.as_ref().map(PathBuf::from),
        logo: game.logo_file.as_ref().map(PathBuf::from),
    })
}

/// The heading a game is filed under in the index: the first letter of its
/// name as the search folds it, or `None` for anything else.
fn initial(title: &str) -> Option<char> {
    lxb_steam::library::sort_key(title)
        .chars()
        .next()
        .filter(char::is_ascii_alphabetic)
        .map(|first| first.to_ascii_uppercase())
}

/// Put `a` before `b` in one of Steam's orders, over what Heroic knows of a
/// game. The name settles every order and Epic's own name for the game
/// settles that, so two games never swap between one rebuild and the next.
///
/// A game with no size or no last-played date goes last whichever way the
/// list runs — nothing known is not the least of something.
fn compare(sort: lxb_steam::library::Sort, a: &Game, b: &Game) -> std::cmp::Ordering {
    use lxb_steam::library::Sort;
    use std::cmp::Ordering;
    let name = |game: &Game| lxb_steam::library::sort_key(&game.title);
    let then = || {
        name(a)
            .cmp(&name(b))
            .then_with(|| a.app_name.cmp(&b.app_name))
    };
    let size = |game: &Game| game.installed.as_ref().map_or(0, |here| here.size);
    let known = |a: u64, b: u64, largest_first: bool| match (a, b) {
        (0, 0) => Ordering::Equal,
        (0, _) => Ordering::Greater,
        (_, 0) => Ordering::Less,
        (a, b) if largest_first => b.cmp(&a),
        (a, b) => a.cmp(&b),
    };
    match sort {
        Sort::InstalledFirst => b
            .installed
            .is_some()
            .cmp(&a.installed.is_some())
            .then_with(then),
        Sort::NameAscending => then(),
        Sort::NameDescending => name(b)
            .cmp(&name(a))
            .then_with(|| a.app_name.cmp(&b.app_name)),
        Sort::RecentlyPlayedFirst => match (&a.last_played, &b.last_played) {
            (Some(a), Some(b)) => b.cmp(a),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        }
        .then_with(then),
        Sort::MostPlayedFirst => b.played_minutes.cmp(&a.played_minutes).then_with(then),
        Sort::LeastPlayedFirst => a.played_minutes.cmp(&b.played_minutes).then_with(then),
        Sort::LargestFirst => known(size(a), size(b), true).then_with(then),
        Sort::SmallestFirst => known(size(a), size(b), false).then_with(then),
    }
}

/// The line under a game: which store runs it, or whether it is here and how
/// much it has been played — in the Steam column's own words.
fn game_note(game: &Game, offline: bool) -> String {
    match (&game.installed, game.store) {
        (_, Some(Store::Ubisoft)) => crate::i18n::text("epic-plays-through-ubisoft").to_string(),
        (_, Some(Store::Ea)) => crate::i18n::text("epic-plays-through-ea").to_string(),
        (None, _) => crate::i18n::text("integration-not-installed").to_string(),
        // With no connection, what a press would come to is the one thing
        // worth saying: Heroic starts every game offline then, and one Epic
        // does not say runs that way may stop at its own sign-in. Hedged,
        // because many that do not say so run perfectly well.
        (Some(_), _) if offline && !game.offline => format!(
            "{} · {}",
            crate::i18n::text("integration-installed"),
            crate::i18n::text("epic-may-need-a-connection")
        ),
        (Some(installed), _) if installed.update => format!(
            "{} · {}",
            crate::i18n::text("integration-installed"),
            crate::i18n::text("epic-update-waiting")
        ),
        (Some(installed), _) => {
            let here = crate::i18n::text("integration-installed");
            match (installed.size, game.played_minutes) {
                (0, _) => here.to_string(),
                (size, 0) => format!("{here} · {}", crate::steam::format_size(size)),
                (size, minutes) => crate::message!(
                    "steam-installed-played",
                    "size" => crate::steam::format_size(size),
                    "played" => played(minutes)
                ),
            }
        }
    }
}

fn played(minutes: u64) -> String {
    if minutes < 60 {
        return crate::message!("steam-playtime-minutes", "minutes" => minutes);
    }
    let hours = minutes as f64 / 60.0;
    let number = if hours < 10.0 {
        format!("{hours:.1}")
    } else {
        format!("{hours:.0}")
    };
    crate::message!("steam-playtime-hours", "hours" => crate::i18n::decimal(number))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Heroic is known by its program, whichever way Chromium has written its
    /// command line — and a program merely named like it is not Heroic.
    #[test]
    fn heroic_is_known_by_its_own_program() {
        assert!(is_heroic(b"/app/bin/heroic/heroic\0--no-gui\0"));
        assert!(is_heroic(
            b"/app/bin/heroic/heroic --type=renderer --no-sandbox"
        ));
        assert!(is_heroic(b"/app/bin/heroic/heroic"));
        assert!(!is_heroic(b"/app/bin/heroic/heroic-helper\0"));
        assert!(!is_heroic(b"/usr/bin/bash\0-c\0/app/bin/heroic/heroic\0"));
        assert!(!is_heroic(b""));
    }

    /// A stand-in for `lxb-heroic`: answers `size`, `get` and `remove` the way
    /// the real one does, word for word, and `get Slow` by waiting to be ended.
    fn fake_helper(dir: &Path) -> PathBuf {
        let at = dir.join("lxb-heroic");
        let base = dir.join("Games");
        std::fs::create_dir_all(&base).unwrap();
        let script = format!(
            r#"#!/bin/sh
case "$1" in
  size)
    case "$2" in
      Huge) echo '{{"protocol":1,"app_name":"Huge","download":1000,"disk":18446744073709551000,"base":"{base}","store":null,"reason":null}}' ;;
      *) echo '{{"protocol":1,"app_name":"'"$2"'","download":137167567,"disk":377925421,"base":"{base}","store":null,"reason":null}}' ;;
    esac ;;
  probe)
    if [ -e "{dir}/heroic-open" ]; then open=true; else open=false; fi
    echo '{{"protocol":1,"heroic":{{"kind":"flatpak-system","command":["flatpak","run","--system","com.heroicgameslauncher.hgl"],"version":"v2.22.3","config":"/x"}},"flatpak":true,"account":"Somebody","proton":"Proton-CachyOS-latest","running":'"$open"'}}' ;;
  get)
    if [ "$2" = Slow ]; then exec sleep 30; fi
    if [ -e "{dir}/heroic-open" ]; then
      echo '{{"protocol":1,"app_name":"'"$2"'","step":"failed","progress":null,"downloaded":null,"size":null,"reason":"heroic-running","note":"Heroic is open"}}'
      exit 1
    fi
    echo '{{"protocol":1,"app_name":"'"$2"'","step":"preparing","progress":null,"downloaded":null,"size":null,"reason":null,"note":"asking Epic about it"}}'
    echo '{{"protocol":1,"app_name":"'"$2"'","step":"downloading","progress":0.5,"downloaded":68584113,"size":137167567,"reason":null,"note":"downloading"}}'
    if [ "$2" = Full ]; then
      echo '{{"protocol":1,"app_name":"Full","step":"failed","progress":null,"downloaded":null,"size":137167567,"reason":"no-space","note":"Not enough available disk space"}}'
      exit 1
    fi
    echo '{{"protocol":1,"app_name":"'"$2"'","step":"done","progress":1.0,"downloaded":137167567,"size":137167567,"reason":null,"note":"installed"}}' ;;
  remove)
    echo '{{"protocol":1,"app_name":"'"$2"'","removed":true,"reason":null,"note":"uninstalled"}}' ;;
  achievements)
    echo "$*" >> "{dir}/asked"
    echo '{{"protocol":1,"app_name":"","total":0,"unlocked":0,"xp":0,"total_xp":0,"list":[],"reason":null,"done":true}}' ;;
esac
"#,
            base = base.display(),
            dir = dir.display()
        );
        std::fs::write(&at, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&at, std::fs::Permissions::from_mode(0o755)).unwrap();
        at
    }

    fn game(app_name: &str, installed: bool, store: Option<Store>) -> Game {
        Game {
            app_name: app_name.to_string(),
            title: format!("The {app_name}"),
            cover_file: None,
            hero_file: None,
            logo_file: None,
            installed: installed.then_some(Installed {
                size: 377_925_421,
                update: false,
            }),
            store,
            played_minutes: 0,
            last_played: None,
            offline: false,
        }
    }

    /// A Heroic that is here, signed in and holding `games`, answered by the
    /// fake helper — without the probe `start` would send.
    fn signed_in(helper: PathBuf, games: Vec<Game>) -> Heroic {
        let (ask, asked) = std::sync::mpsc::channel::<Ask>();
        // The worker's queue is never read here; held open so a library
        // question sent to it is not taken for a worker that has stopped.
        Box::leak(Box::new(asked));
        let (back, heard) = std::sync::mpsc::channel::<Heard>();
        Heroic {
            inner: Some(Inner {
                helper,
                ask,
                back,
                heard,
                found: Found::Here {
                    command: vec!["flatpak".into(), "run".into(), FLATPAK.into()],
                    proton: true,
                },
                account: Some("Somebody".to_string()),
                games,
                reading: 0,
                installing: None,
                signing: None,
                signings: 0,
                covers_asked: true,
                heroes_asked: HashSet::new(),
                shapes: HashMap::new(),
                trophies: std::collections::BTreeMap::new(),
                trophies_asked: false,
                icons_asked: HashSet::new(),
                played: std::collections::BTreeSet::new(),
                sizes: HashMap::new(),
                sizing: HashSet::new(),
                fetch: None,
                waiting: VecDeque::new(),
                repairs: HashSet::new(),
                fetches: 0,
                held: None,
                landed: HashMap::new(),
                removing: HashSet::new(),
                broken: None,
                offline: false,
                search: String::new(),
                kept: Kept::default(),
                own_tools: None,
                sort: crate::settings::epic_sort().unwrap_or_default(),
            }),
        }
    }

    const FLATPAK: &str = "com.heroicgameslauncher.hgl";

    /// Poll until `done` says the change it is waiting for has come, for up
    /// to ten seconds.
    fn poll_until(heroic: &mut Heroic, mut done: impl FnMut(&Change) -> bool) -> Change {
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            let change = heroic.poll();
            if done(&change) {
                return change;
            }
            assert!(Instant::now() < until, "the fake helper never answered");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn commands(panel: &Panel) -> Vec<menu::Command> {
        panel.buttons.iter().map(|entry| entry.command).collect()
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lxb-heroic-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The press on a game that is not here: the question waits for the
    /// size, then says it — and a game there is no room for is not offered.
    #[test]
    fn the_install_question_says_what_it_takes_and_refuses_what_will_not_fit() {
        let dir = scratch("size");
        let mut heroic = signed_in(
            fake_helper(&dir),
            vec![game("Quail", false, None), game("Huge", false, None)],
        );
        let waiting = heroic.install_panel("Quail").unwrap();
        assert!(waiting.lines.contains(&dialog::Line::Waiting));
        assert_eq!(
            commands(&waiting),
            [menu::Command::EpicGet, menu::Command::Dismiss]
        );

        heroic.measure("Quail");
        heroic.measure("Quail");
        let change = poll_until(&mut heroic, |change| change.sized.is_some());
        assert_eq!(change.sized.as_deref(), Some("Quail"));
        let sized = heroic.install_panel("Quail").unwrap();
        assert!(!sized.lines.contains(&dialog::Line::Waiting));
        // The heading, the two sizes, the room left, the rule.
        assert_eq!(sized.lines.len(), 4, "{:?}", sized.lines.len());
        assert_eq!(
            commands(&sized),
            [menu::Command::EpicGet, menu::Command::Dismiss]
        );

        heroic.measure("Huge");
        poll_until(&mut heroic, |change| {
            change.sized.as_deref() == Some("Huge")
        });
        let full = heroic.install_panel("Huge").unwrap();
        assert_eq!(commands(&full), [menu::Command::Dismiss]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The other stores' titles never ask Epic for a size: the store that
    /// downloads them says it, in its own window, on the first play.
    #[test]
    fn another_stores_title_says_where_it_comes_down_and_needs_no_size() {
        let dir = scratch("elsewhere");
        let heroic = signed_in(
            fake_helper(&dir),
            vec![
                game("Albacore", false, Some(Store::Ubisoft)),
                game("Strange", false, Some(Store::Other)),
                game("Here", true, None),
            ],
        );
        let panel = heroic.install_panel("Albacore").unwrap();
        assert!(!panel.lines.contains(&dialog::Line::Waiting));
        assert_eq!(
            commands(&panel),
            [menu::Command::EpicGet, menu::Command::Dismiss]
        );
        let panel = heroic.install_panel("Strange").unwrap();
        assert_eq!(commands(&panel), [menu::Command::Dismiss]);
        assert!(heroic.install_panel("Here").is_none());
        assert!(heroic.install_panel("Nobody").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// One download at a time: the second waits its turn, stopping the first
    /// starts it, and the first's ending — the helper killed — is not taken
    /// for the second's.
    #[test]
    fn downloads_queue_and_stopping_one_starts_the_next() {
        let dir = scratch("queue");
        let mut heroic = signed_in(
            fake_helper(&dir),
            vec![game("Slow", false, None), game("Quail", false, None)],
        );
        heroic.get("Slow");
        heroic.get("Quail");
        heroic.get("Quail");
        heroic.get("Nobody");
        assert!(matches!(heroic.doing("Slow"), Doing::Fetching(_)));
        assert_eq!(heroic.doing("Quail"), Doing::Waiting);
        let menu: Vec<_> = heroic
            .game_menu(&apps::EpicGame {
                app_name: "Quail".into(),
                name: "The Quail".into(),
                note: String::new(),
                progress: None,
                installed: false,
                start: None,
                cover: None,
                shape: None,
                hero: None,
                logo: None,
            })
            .iter()
            .map(|entry| entry.command)
            .collect();
        assert_eq!(menu[0], menu::Command::EpicOfferStop);
        let stop = heroic.stop_panel("Quail").unwrap();
        assert_eq!(
            commands(&stop),
            [menu::Command::Dismiss, menu::Command::EpicStop]
        );

        heroic.stop("Slow");
        assert_eq!(heroic.doing("Slow"), Doing::Nothing);
        assert!(matches!(heroic.doing("Quail"), Doing::Fetching(_)));
        let change = poll_until(&mut heroic, |change| change.fetched.is_some());
        let ended = change.fetched.unwrap();
        assert_eq!(ended.app_name, "Quail");
        assert_eq!(ended.title, "The Quail");
        assert!(ended.worked);
        assert_eq!(ended.reason, None);
        assert_eq!(heroic.doing("Quail"), Doing::Nothing);
        // Nothing more is said about the one that was stopped.
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(heroic.poll().fetched, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_download_that_fails_says_why() {
        let dir = scratch("full");
        let mut heroic = signed_in(fake_helper(&dir), vec![game("Full", false, None)]);
        heroic.get("Full");
        let change = poll_until(&mut heroic, |change| change.fetched.is_some());
        let ended = change.fetched.unwrap();
        assert!(!ended.worked);
        assert_eq!(ended.reason, Some(Reason::NoSpace));
        assert!(game_failed_sentence(Reason::NoSpace).is_some());
        assert!(game_failed_sentence(Reason::Legendary).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Pressed while Heroic is open — a game running through it, most often —
    /// a download is not refused but held, at the head of the queue, and goes
    /// on by itself once Heroic has gone.
    #[test]
    fn a_download_asked_for_while_heroic_is_open_waits_for_it() {
        let dir = scratch("held");
        let helper = fake_helper(&dir);
        std::fs::write(dir.join("heroic-open"), "").unwrap();
        let mut heroic = signed_in(helper.clone(), vec![game("Quail", false, None)]);
        heroic.get("Quail");
        let until = Instant::now() + Duration::from_secs(10);
        while heroic.doing("Quail") != Doing::Held {
            let change = heroic.poll();
            assert_eq!(change.fetched, None, "a wait, not a failure");
            assert!(Instant::now() < until, "never held");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(heroic.note().unwrap().starts_with("The Quail · "));
        let back = heroic.inner.as_ref().unwrap().back.clone();

        // Still open: still held.
        answer(&helper, &Ask::Probe, &back);
        std::thread::sleep(Duration::from_millis(100));
        heroic.poll();
        assert_eq!(heroic.doing("Quail"), Doing::Held);

        // Closed: it goes on, and lands.
        std::fs::remove_file(dir.join("heroic-open")).unwrap();
        answer(&helper, &Ask::Probe, &back);
        let change = poll_until(&mut heroic, |change| change.fetched.is_some());
        assert!(change.fetched.unwrap().worked);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The row says what is happening to its game, and draws it as a bar
    /// while it comes down.
    #[test]
    fn a_row_says_what_is_happening_to_its_game() {
        let dir = scratch("rows");
        let mut heroic = signed_in(
            fake_helper(&dir),
            vec![
                game("Slow", false, None),
                game("Quail", false, None),
                game("Here", true, None),
            ],
        );
        heroic.get("Slow");
        heroic.get("Quail");
        if let Some(fetch) = heroic.inner.as_mut().unwrap().fetch.as_mut() {
            fetch.so_far = SoFar {
                step: Step::Downloading,
                progress: Some(0.25),
                size: Some(137_167_567),
                updating: false,
                repairing: false,
            };
        }
        heroic.remove("Here");
        let rows = heroic.rows();
        let row = |app: &str| {
            rows.iter()
                .find_map(|entry| entry.epic_game().filter(|game| game.app_name == app))
                .unwrap()
                .clone()
        };
        let slow = row("Slow");
        assert_eq!(slow.progress.map(|bar| bar.share), Some(0.25));
        assert!(slow.note.contains("25"), "{}", slow.note);
        assert_eq!(row("Quail").progress, None);
        assert_eq!(
            row("Quail").note,
            crate::i18n::text("integration-waiting-to-download")
        );
        assert_eq!(row("Here").note, crate::i18n::text("shell-removing"));
        // The row of the whole column says it too, as a bar.
        assert_eq!(heroic.arriving(), Some(0.25));
        assert!(heroic.note().unwrap().starts_with("The Slow · "));
        // And so does the card in the corner of the guide: the game being
        // fetched, never the one waiting behind it, with the row's reading.
        let coming = heroic.downloading().expect("a card for the download");
        assert_eq!(
            coming.whose,
            crate::steam::Whose::Epic {
                app_name: "Slow".into(),
                cover: None
            }
        );
        assert_eq!(
            coming.said(),
            crate::i18n::builtin("Downloading").to_string() + " The Slow"
        );
        assert_eq!(coming.share, Some(0.25));
        assert!(coming.a_download);
        assert_eq!(
            coming.steam_app_id(),
            None,
            "its finish is the helper's to say"
        );
        heroic.stop("Slow");
        heroic.stop("Quail");
        assert_eq!(heroic.downloading(), None, "nothing is coming down");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// With no connection an installed game Epic does not say runs offline
    /// is said to maybe need one, the row says the machine has none, and a
    /// press on a game with an update waiting plays it rather than asking —
    /// and all of it goes back the moment the connection does.
    #[test]
    fn with_no_connection_the_rows_say_so_and_an_update_is_not_offered() {
        let dir = scratch("offline");
        let mut old = game("Old", true, None);
        old.installed = Some(Installed {
            size: 377_925_421,
            update: true,
        });
        let mut free = game("Free", true, None);
        free.offline = true;
        let mut heroic = signed_in(
            fake_helper(&dir),
            vec![
                game("Needy", true, None),
                free,
                old,
                game("Away", false, None),
            ],
        );
        let note = |heroic: &Heroic, app: &str| {
            heroic
                .rows()
                .iter()
                .find_map(|entry| entry.epic_game().filter(|game| game.app_name == app))
                .unwrap()
                .note
                .clone()
        };
        let needs = crate::i18n::text("epic-may-need-a-connection");
        assert!(!note(&heroic, "Needy").contains(needs));
        assert!(heroic.offers_update("Old"));

        assert!(!heroic.set_online(Some(true)), "nothing changed");
        assert!(!heroic.set_online(None), "not known is not offline");
        assert!(heroic.set_online(Some(false)));
        assert!(
            note(&heroic, "Needy").contains(needs),
            "{}",
            note(&heroic, "Needy")
        );
        assert!(
            !note(&heroic, "Free").contains(needs),
            "Epic says it runs offline"
        );
        assert_eq!(
            note(&heroic, "Away"),
            crate::i18n::text("integration-not-installed")
        );
        assert!(heroic.has_update("Old"), "still there");
        assert!(!heroic.offers_update("Old"), "but the press plays");
        assert!(heroic
            .note()
            .unwrap()
            .ends_with(crate::i18n::text("epic-no-connection")));
        // What an uninstall would take off is said whatever the connection.
        let uninstall = heroic.uninstall_panel("Needy").unwrap();
        assert!(!uninstall
            .lines
            .iter()
            .any(|line| matches!(line, dialog::Line::Note(note) if note.contains(needs))));

        assert!(heroic.set_online(None));
        assert!(!note(&heroic, "Needy").contains(needs));
        assert!(heroic.offers_update("Old"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Epic not answering the size is the install question's answer: said
    /// straight away, with Close alone, rather than an Install that fails.
    #[test]
    fn an_install_question_epic_cannot_answer_says_so_and_offers_only_close() {
        let dir = scratch("unreachable");
        let mut heroic = signed_in(fake_helper(&dir), vec![game("Away", false, None)]);
        heroic.inner.as_mut().unwrap().sizes.insert(
            "Away".into(),
            Measured {
                download: None,
                disk: None,
                base: None,
                unreachable: true,
            },
        );
        let panel = heroic.install_panel("Away").unwrap();
        assert!(panel.lines.iter().any(|line| matches!(
            line,
            dialog::Line::Note(note) if note == crate::i18n::text("epic-could-not-be-reached")
        )));
        assert_eq!(commands(&panel), [menu::Command::Dismiss]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A game with a newer build waiting says so, asks on the press whether
    /// to update it first, offers it in its menu, and counts up as an update
    /// rather than as an install.
    #[test]
    fn an_update_waiting_is_said_asked_about_and_counted_as_one() {
        let dir = scratch("update");
        let mut old = game("Old", true, None);
        old.installed = Some(Installed {
            size: 377_925_421,
            update: true,
        });
        let mut heroic = signed_in(
            fake_helper(&dir),
            vec![old, game("Slow", false, None), game("Here", true, None)],
        );
        assert!(heroic.has_update("Old"));
        assert!(!heroic.has_update("Here"));
        assert!(!heroic.has_update("Slow"));
        let rows = heroic.rows();
        let note = |app: &str| {
            rows.iter()
                .find_map(|entry| entry.epic_game().filter(|game| game.app_name == app))
                .unwrap()
                .note
                .clone()
        };
        assert!(note("Old").ends_with(crate::i18n::text("epic-update-waiting")));
        let panel = heroic.question(Question::Update, "Old").unwrap();
        assert_eq!(
            commands(&panel),
            [menu::Command::EpicGet, menu::Command::EpicPlayNow]
        );
        assert!(heroic.question(Question::Update, "Here").is_none());
        let menu: Vec<_> = heroic
            .game_menu(&apps::EpicGame {
                app_name: "Old".into(),
                name: "The Old".into(),
                note: String::new(),
                progress: None,
                installed: true,
                start: Some(vec!["true".into()]),
                cover: None,
                shape: None,
                hero: None,
                logo: None,
            })
            .iter()
            .map(|entry| entry.command)
            .collect();
        // Update now first, then Steam's game menu row for row.
        assert_eq!(
            menu,
            [
                menu::Command::EpicUpdate,
                menu::Command::EpicPlay,
                menu::Command::EpicVerify,
                menu::Command::EpicCompatibility,
                menu::Command::EpicOfferRemove,
                menu::Command::Resolution,
                menu::Command::EpicSort,
                menu::Command::Dismiss,
            ]
        );

        // Held behind a download that never ends, so what it is called can
        // be read before it has run.
        heroic.get("Slow");
        heroic.get("Old");
        heroic.stop("Slow");
        let Doing::Fetching(so_far) = heroic.doing("Old") else {
            panic!("updating");
        };
        assert!(so_far.updating);
        assert_eq!(so_far_sentence(&so_far), crate::i18n::text("epic-updating"));
        heroic.stop("Old");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Epic's half of the Trophies column: a game with achievements, its own
    /// under it with the counts and the sections the other halves use, a
    /// hidden one kept hidden until it is unlocked — and a game with none, or
    /// one Epic could not be asked about, left out.
    #[test]
    fn epics_achievements_are_rows_of_the_trophies_column() {
        let dir = scratch("trophies");
        let mut heroic = signed_in(
            fake_helper(&dir),
            vec![game("Quail", true, None), game("Owl", false, None)],
        );
        let line = |app: &str, list: Vec<TrophyLine>| AchievementsLine {
            protocol: PROTOCOL,
            app_name: app.into(),
            total: list.len() as u32,
            unlocked: list.iter().filter(|t| t.unlocked).count() as u32,
            list,
            reason: None,
            done: false,
        };
        let trophy = |name: &str, unlocked: bool, hidden: bool| TrophyLine {
            name: name.into(),
            title: format!("The {name}"),
            description: format!("Do {name}"),
            unlocked,
            unlocked_at: unlocked.then_some(1_790_295_142),
            icon: None,
            xp: 50,
            rarity: Some(12.5),
            hidden,
        };
        let back = heroic.inner.as_ref().unwrap().back.clone();
        back.send(Heard::Achieved(line(
            "Quail",
            vec![
                trophy("a", true, false),
                trophy("b", false, false),
                trophy("c", false, true),
            ],
        )))
        .unwrap();
        back.send(Heard::Achieved(line("Owl", Vec::new()))).unwrap();
        assert!(heroic.poll().trophies);

        let rows = heroic.trophy_rows();
        assert_eq!(rows.len(), 1, "a game with none is left out");
        let Entry::Trophy(game) = &rows[0] else {
            panic!("a trophy row");
        };
        assert_eq!(game.key, crate::trophies::Key::EpicGame("Quail".into()));
        assert_eq!(game.facts.title, "The Quail");
        assert!(game.facts.comment.starts_with("Epic Games · "));
        assert_eq!(game.installed, Some(true));
        assert_eq!(game.platform.as_ref().unwrap().name, TITLE);
        let list = game.entries.as_ref().unwrap();
        assert_eq!(list.len(), 3);
        let title = |at: usize| match &list[at] {
            Entry::Trophy(row) => row.facts.title.clone(),
            _ => unreachable!(),
        };
        assert_eq!(title(0), "The a");
        assert_eq!(title(2), crate::i18n::text("shell-hidden-achievement"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A game's icons are asked for when its list is opened, once, and only
    /// where it is missing some; and after playing, only the games played are
    /// asked about again, never the whole library.
    #[test]
    fn icons_and_a_played_games_achievements_are_asked_for_by_name() {
        let dir = scratch("asked");
        let mut heroic = signed_in(
            fake_helper(&dir),
            vec![game("Quail", true, None), game("Owl", false, None)],
        );
        let trophy = |icon: Option<&str>| TrophyLine {
            name: "a".into(),
            title: "A".into(),
            description: String::new(),
            unlocked: false,
            unlocked_at: None,
            icon: icon.map(str::to_string),
            xp: 50,
            rarity: None,
            hidden: false,
        };
        let back = heroic.inner.as_ref().unwrap().back.clone();
        for (app, icon) in [("Quail", None), ("Owl", Some("/cache/Owl/achievements/a"))] {
            back.send(Heard::Achieved(AchievementsLine {
                protocol: PROTOCOL,
                app_name: app.into(),
                total: 1,
                unlocked: 0,
                list: vec![trophy(icon)],
                reason: None,
                done: false,
            }))
            .unwrap();
        }
        assert!(heroic.poll().trophies);
        let asked = || {
            std::fs::read_to_string(dir.join("asked"))
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        let settle = |heroic: &mut Heroic| {
            std::thread::sleep(Duration::from_millis(300));
            heroic.poll();
        };

        heroic.want_trophy_icons("Owl");
        heroic.want_trophy_icons("Quail");
        heroic.want_trophy_icons("Quail");
        settle(&mut heroic);
        assert_eq!(
            asked(),
            ["achievements --only Quail"],
            "once, and not for one with all its icons"
        );

        heroic.game_ended();
        settle(&mut heroic);
        assert_eq!(asked().len(), 1, "nothing played, nothing asked");
        heroic.started("Owl");
        // The process that started it going is not the game ending: with
        // Heroic already up, it goes at once.
        heroic.reread();
        settle(&mut heroic);
        assert_eq!(asked().len(), 1, "not when the launcher went");
        heroic.game_ended();
        settle(&mut heroic);
        assert_eq!(
            asked().last().map(String::as_str),
            Some("achievements --only Owl")
        );
        heroic.game_ended();
        settle(&mut heroic);
        assert_eq!(asked().len(), 2, "asked once for each time it was played");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A game of Heroic's ending is its playtimes changing, whatever started
    /// it: the first look only takes their measure, and a change is said once.
    #[test]
    fn a_game_ending_is_heroics_playtimes_changing() {
        let dir = scratch("playtimes");
        let playtimes = dir.join("timestamp.json");
        std::fs::write(&playtimes, "{}").unwrap();
        let mut heroic = signed_in(fake_helper(&dir), vec![game("Quail", true, None)]);
        heroic.inner.as_mut().unwrap().kept.playtimes = Some(playtimes.clone());
        let look = |heroic: &mut Heroic| {
            heroic.inner.as_mut().unwrap().kept.looked = None;
            heroic.keep(false)
        };
        assert!(
            !look(&mut heroic),
            "the first look is a measure, not an ending"
        );
        assert!(!look(&mut heroic));
        let later = std::time::SystemTime::now() + Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&playtimes)
            .unwrap()
            .set_modified(later)
            .unwrap();
        assert!(look(&mut heroic), "Heroic wrote them");
        assert!(!look(&mut heroic), "and said once");
        assert!(
            !heroic.keep(false),
            "nor looked at more than every two seconds"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The two menus are Steam's: the same rows in the same bands wearing the
    /// same marks — Play, Verify, Compatibility, Uninstall, Resolution, then
    /// Sort and Cancel under the rule — and Heroic's window only on the row,
    /// where Steam's own window is.
    #[test]
    fn the_menus_are_steams_menus() {
        let dir = scratch("menus");
        let heroic = signed_in(
            fake_helper(&dir),
            vec![game("Here", true, None), game("Away", false, None)],
        );
        let epic = |app: &str, here: bool| apps::EpicGame {
            app_name: app.into(),
            name: format!("The {app}"),
            note: String::new(),
            progress: None,
            installed: here,
            start: here.then(|| vec!["true".into()]),
            cover: None,
            shape: None,
            hero: None,
            logo: None,
        };
        let shape = |rows: Vec<menu::Entry>| -> Vec<(menu::Command, Option<&'static str>, u8)> {
            rows.iter()
                .map(|row| (row.command, row.glyph, row.group))
                .collect()
        };
        assert_eq!(
            shape(heroic.game_menu(&epic("Here", true))),
            [
                (menu::Command::EpicPlay, Some(icons::LAUNCH), 0),
                (menu::Command::EpicVerify, None, 0),
                (menu::Command::EpicCompatibility, None, 0),
                (menu::Command::EpicOfferRemove, Some(icons::UNINSTALL), 0),
                (
                    menu::Command::Resolution,
                    Some(icons::SETTING_RESOLUTION),
                    0
                ),
                (menu::Command::EpicSort, Some(icons::SORT), 1),
                (menu::Command::Dismiss, None, 1),
            ]
        );
        let away = shape(heroic.game_menu(&epic("Away", false)));
        assert_eq!(
            away[0],
            (menu::Command::EpicOfferGet, Some(icons::LAUNCH), 0)
        );
        assert!(!away
            .iter()
            .any(|row| row.0 == menu::Command::EpicOfferRemove));
        assert!(away
            .iter()
            .any(|row| row.0 == menu::Command::EpicCompatibility));
        for menu in [
            heroic.game_menu(&epic("Here", true)),
            heroic.game_menu(&epic("Away", false)),
        ] {
            assert!(!menu
                .iter()
                .any(|row| row.command == menu::Command::EpicOpenHeroic));
        }
        assert_eq!(
            shape(heroic.row_menu()),
            [
                (menu::Command::EpicRefresh, Some(icons::REFRESH), 0),
                (menu::Command::EpicSignOut, Some(icons::SIGN_OUT), 0),
                (menu::Command::EpicSort, Some(icons::SORT), 1),
                (menu::Command::EpicOpenHeroic, None, 1),
                (menu::Command::Dismiss, None, 1),
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The column is Steam's column: the field, the index, the games — and
    /// the field narrows, the index files by letter, and Sort orders.
    #[test]
    fn the_column_is_searched_indexed_and_sorted_like_steams() {
        let dir = scratch("column");
        let mut big = game("Big", true, None);
        big.title = "Anno".into();
        big.installed = Some(Installed {
            size: 9_000_000_000,
            update: false,
        });
        let mut played = game("Played", false, None);
        played.title = "Brill".into();
        played.played_minutes = 90;
        played.last_played = Some("2026-09-20T10:00:00.000Z".into());
        let mut small = game("Small", true, None);
        small.title = "Zork".into();
        let mut numbered = game("Numbered", false, None);
        numbered.title = "112 Operator".into();
        let mut heroic = signed_in(fake_helper(&dir), vec![big, played, small, numbered]);
        let titles = |rows: &[Entry]| -> Vec<String> {
            rows.iter()
                .filter_map(|entry| entry.epic_game().map(|game| game.name.clone()))
                .collect()
        };

        let rows = heroic.rows();
        assert!(matches!(&rows[0], Entry::Search(search) if search.of == apps::Searched::Epic));
        assert_eq!(
            apps::head_rows(&rows),
            2,
            "the field and the index stand over it"
        );
        assert_eq!(
            titles(&rows),
            ["Anno", "Zork", "112 Operator", "Brill"],
            "installed first"
        );
        let Entry::Folder(index) = &rows[1] else {
            panic!("the index");
        };
        let headings: Vec<usize> = index
            .entries
            .iter()
            .map(|letter| match letter {
                Entry::Folder(letter) => letter.entries.len(),
                _ => 0,
            })
            .collect();
        assert_eq!(headings, [1, 1, 1, 1], "#, A, B, Z");

        for (sort, expected) in [
            (
                lxb_steam::library::Sort::NameAscending,
                ["112 Operator", "Anno", "Brill", "Zork"],
            ),
            (
                lxb_steam::library::Sort::NameDescending,
                ["Zork", "Brill", "Anno", "112 Operator"],
            ),
            (
                lxb_steam::library::Sort::MostPlayedFirst,
                ["Brill", "112 Operator", "Anno", "Zork"],
            ),
            (
                lxb_steam::library::Sort::RecentlyPlayedFirst,
                ["Brill", "112 Operator", "Anno", "Zork"],
            ),
            (
                lxb_steam::library::Sort::LargestFirst,
                ["Anno", "Zork", "112 Operator", "Brill"],
            ),
        ] {
            assert!(heroic.set_sort(sort));
            assert_eq!(titles(&heroic.rows()), expected, "{sort:?}");
        }
        let orders = heroic.orders();
        assert!(orders.sizes && orders.playtimes && orders.played);

        assert!(heroic.set_search("ANN"));
        assert!(
            !heroic.set_search("ANN"),
            "the same search twice is no change"
        );
        let found = heroic.rows();
        assert_eq!(titles(&found), ["Anno"]);
        assert!(
            matches!(&found[1], Entry::Search(_)),
            "and the row that empties it"
        );
        assert!(heroic.set_search("nothing like it"));
        assert_eq!(titles(&heroic.rows()), Vec::<String>::new());
        assert!(!heroic.rows().is_empty(), "the field stays to be emptied");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Uninstalling is asked first, the harmless answer first, and only of a
    /// game that is here; the answer lands as a change.
    #[test]
    fn uninstalling_is_asked_first_and_answered() {
        let dir = scratch("remove");
        let mut heroic = signed_in(
            fake_helper(&dir),
            vec![game("Here", true, None), game("Away", false, None)],
        );
        let panel = heroic.uninstall_panel("Here").unwrap();
        assert_eq!(
            commands(&panel),
            [menu::Command::Dismiss, menu::Command::EpicRemove]
        );
        assert!(heroic.uninstall_panel("Away").is_none());
        heroic.remove("Here");
        heroic.remove("Here");
        assert_eq!(heroic.doing("Here"), Doing::Removing);
        // Through the worker, which this test has no thread for: answered
        // here the way the worker would.
        let back = heroic.inner.as_ref().unwrap().back.clone();
        answer(
            &heroic.inner.as_ref().unwrap().helper.clone(),
            &Ask::Remove("Here".into()),
            &back,
        );
        let change = poll_until(&mut heroic, |change| change.removed.is_some());
        let ended = change.removed.unwrap();
        assert!(ended.worked);
        assert_eq!(ended.title, "The Here");
        assert_eq!(heroic.doing("Here"), Doing::Nothing);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_local_helper_comes_before_path_but_not_before_the_flag() {
        let dir = std::env::temp_dir().join(format!("lxb-heroic-lookup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let shell = dir.join("lxb-desktop");
        let beside = dir.join(HELPER);
        std::fs::write(&beside, "#!/bin/sh\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&beside, std::fs::Permissions::from_mode(0o755)).unwrap();
        let elsewhere = PathBuf::from("/usr/bin/lxb-heroic");
        assert_eq!(
            resolve_helper(None, Some(&shell), || Some(elsewhere.clone()), false),
            Some(beside.clone())
        );
        assert_eq!(
            resolve_helper(Some(dir.join("nothing")), Some(&shell), || None, false),
            None
        );
        assert_eq!(
            resolve_helper(None, None, || Some(elsewhere.clone()), false),
            Some(elsewhere)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The package coming and going while the session runs is noticed, once
    /// each way; its merely being found somewhere else is not news.
    #[test]
    fn the_package_is_noticed_coming_and_going() {
        let mut held = None;
        let there = PathBuf::from("/usr/bin/lxb-heroic");
        assert_eq!(came_or_went(&mut held, None), None, "never there");
        assert_eq!(
            came_or_went(&mut held, Some(there.clone())),
            Some(Some(there.clone()))
        );
        assert_eq!(came_or_went(&mut held, Some(there.clone())), None);
        assert_eq!(
            came_or_went(&mut held, Some(PathBuf::from("/usr/local/bin/lxb-heroic"))),
            None,
            "moved, not gone"
        );
        assert_eq!(came_or_went(&mut held, None), Some(None), "taken off");
        assert_eq!(held, None);
    }

    /// Heroic's own shortcut, word for word, behind the one variable that
    /// stops it dead where it has leaked in.
    #[test]
    fn a_game_starts_the_way_heroics_own_shortcut_starts_it() {
        let command: Vec<String> = ["flatpak", "run", "--system", "com.heroicgameslauncher.hgl"]
            .iter()
            .map(|word| word.to_string())
            .collect();
        let argv = launch_argv(&command, "051eaac0842c46d7a5a62858ad534d5a", false).unwrap();
        assert_eq!(
            argv[..5],
            [
                "env",
                "-u",
                "ELECTRON_RUN_AS_NODE",
                "-u",
                "ELECTRON_NO_ATTACH_CONSOLE"
            ]
        );
        assert_eq!(argv[5..9], command[..]);
        assert_eq!(
            argv[9..],
            [
                "--no-gui",
                "--no-sandbox",
                "heroic://launch?appName=051eaac0842c46d7a5a62858ad534d5a&runner=legendary&gui=false"
            ]
        );
        assert_eq!(launch_argv(&command, "a&runner=x", false), None);

        // Left running after the game: detached, so the process the shell
        // watches is a courier, and without the flag that closes Heroic with
        // its game.
        let left = launch_argv(&command, "051eaac0842c46d7a5a62858ad534d5a", true).unwrap();
        assert_eq!(left[..2], ["setsid", "-f"]);
        assert!(!left.contains(&"--no-gui".to_string()));
        assert!(left.last().unwrap().ends_with("&gui=false"));
    }

    /// The helper's words, as it spells them: a record it may add a field to
    /// is still read, and a word it may add is not a line thrown away.
    #[test]
    fn what_the_helper_says_is_read_whatever_it_adds() {
        let line: SignInLine = serde_json::from_str(
            r#"{"event":"code","protocol":1,"code":"KDNNWTGW","url":"https://www.epicgames.com/activate?userCode=KDNNWTGW","short_url":"https://www.epicgames.com/activate","expires_in":600}"#,
        )
        .unwrap();
        assert!(line.usable());
        let line: SignInLine = serde_json::from_str(
            r#"{"event":"failed","protocol":1,"reason":"heroic-running","url":null,"note":"x"}"#,
        )
        .unwrap();
        assert!(matches!(
            line,
            SignInLine::Failed {
                reason: Reason::HeroicRunning,
                ..
            }
        ));
        let line: SignInLine =
            serde_json::from_str(r#"{"event":"something-new","protocol":1}"#).unwrap();
        assert!(!line.usable());
        assert!(epics_own_page(
            "https://www.epicgames.com/activate?userCode=KDNNWTGW"
        ));
        assert!(!epics_own_page("https://www.epicgames.com.evil/activate"));
        assert!(!epics_own_page("http://www.epicgames.com/activate"));
        assert!(!epics_own_page("https://www.epicgames.com/a b"));
        let reason: Reason = serde_json::from_str(r#""a-word-from-later""#).unwrap();
        assert_eq!(reason, Reason::Other);
        let library: Library = serde_json::from_str(
            r#"{"protocol":1,"account":"Somebody","refreshed":true,"reason":null,"games":[
                {"app_name":"Quail","title":"20XX","developer":"Batterystaple","cover":"https://c","hero":null,
                 "logo":null,"cover_file":"/c/Quail/cover.jpg","installed":{"path":"/g/20XX","size":1663600000,"version":"1"},
                 "store":null,"offline":false,"cloud_saves":true,"played_minutes":95,"last_played":null,"future":1}]}"#,
        )
        .unwrap();
        assert_eq!(
            library.games[0].installed.as_ref().unwrap().size,
            1663600000
        );
        assert_eq!(library.games[0].played_minutes, 95);
    }
}
