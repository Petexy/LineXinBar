//! Update sources are independent of launcher categories. This crate is shared
//! by the unprivileged shell and the detached coordinator. Native tools retain
//! their locks, dependency solvers and trust policy; their ordinary
//! confirmations are answered by their own unattended flags, because the
//! review's Update now was the confirmation — see [`discovery::steps`].
pub mod authorization;
pub mod custom;
pub mod discovery;
pub mod firmware;
pub mod journal;
pub mod listing;
pub mod policy;
pub mod preflight;
pub mod process;
pub mod prompt;
pub mod protection;
pub mod service;
mod supervisor;

use serde::{Deserialize, Serialize};

pub const PROTOCOL: u32 = 3;
pub const INSTALL_ACTION: &str = "org.linexinbar.updates.install";

/// How wide the terminal a transaction runs on is, in characters.
///
/// One number shared by both ends of the socket: the coordinator opens the
/// PTY this wide, so every tool lays its tables and progress lines out to
/// it, and the shell's transcript frame is this many columns of the same
/// fixed-width face — so a line that fit the terminal fits the frame, and
/// what pacman lined up stays lined up on the screen. Eighty, because that
/// is the width every tool's output was written against.
pub const COLUMNS: usize = 80;

/// How many lines of what the tools said a job keeps, altogether.
///
/// Enough to be the whole of an ordinary run — a machine that has waited a
/// month for its updates prints a line a download, a line an upgrade and a
/// line a hook, some thousands — so that "Full output" is full. Bounded all
/// the same, because a snapshot carries it down the socket on every status
/// poll: a transcript this long is a few hundred kilobytes, which a local
/// socket hands over in a millisecond, and one ten times longer would not
/// be read by anybody. The oldest lines go first.
pub const TRANSCRIPT_LINES: usize = 6000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceId {
    System,
    Flatpak,
    Aur,
    Snap,
    Nix,
    Guix,
    AppImage,
    Firmware,
}

impl SourceId {
    /// The source as a noun, for a line that is about it rather than a row
    /// that acts on it: "Firmware: 1 device update", not "Update firmware:
    /// 1 device update".
    pub fn name(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Flatpak => "Flatpaks",
            Self::Aur => "AUR",
            Self::Snap => "Snaps",
            Self::Nix => "Nix profile",
            Self::Guix => "Guix profile",
            Self::AppImage => "AppImages",
            Self::Firmware => "Firmware",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::System => "Update the system",
            Self::Flatpak => "Update Flatpaks",
            Self::Aur => "Update AUR",
            Self::Snap => "Update Snaps",
            Self::Nix => "Update Nix packages",
            Self::Guix => "Update Guix packages",
            Self::AppImage => "Update AppImages",
            Self::Firmware => "Update firmware",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum System {
    Pacman,
    Apt,
    Dnf5,
    Dnf,
    ZypperLeap,
    ZypperRolling,
    RpmOstree,
    Bootc,
    Transactional,
    Nixos,
    Apk,
    Xbps,
    Portage,
    Eopkg,
    Urpmi,
    Slackpkg,
    Guix,
    Unknown,
}

impl System {
    pub fn name(self) -> &'static str {
        match self {
            Self::Pacman => "pacman",
            Self::Apt => "APT",
            Self::Dnf5 => "DNF5",
            Self::Dnf => "DNF",
            Self::ZypperLeap | Self::ZypperRolling => "Zypper",
            Self::RpmOstree => "rpm-ostree",
            Self::Bootc => "bootc",
            Self::Transactional => "transactional-update",
            Self::Nixos => "NixOS",
            Self::Apk => "APK",
            Self::Xbps => "XBPS",
            Self::Portage => "Portage",
            Self::Eopkg => "eopkg",
            Self::Urpmi => "urpmi",
            Self::Slackpkg => "slackpkg",
            Self::Guix => "Guix System",
            Self::Unknown => "Unknown system provider",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Provider {
    System(System),
    Custom(custom::Reviewed),
    Flatpak { installations: Vec<String> },
    Aur { helper: Option<String> },
    Snap,
    Nix,
    Guix,
    AppImage,
    Firmware,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub name: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub id: SourceId,
    pub provider: Provider,
    pub note: String,
    pub items: Vec<Item>,
    pub excluded: Vec<Item>,
    pub error: Option<String>,
    pub checked: Option<u64>,
    /// A cached package database is not evidence that the host is up to date.
    pub fresh: bool,
    /// Whether `items` is the list of what would change, so that its length
    /// is a number the page can put up. A provider whose preview is a
    /// deployment's status or a profile's inventory has items that are not
    /// updates, or none, and the page says it is ready rather than how many.
    /// See [`listing::Listing::Status`].
    #[serde(default)]
    pub listed: bool,
    pub executable: bool,
    /// Keep configured ownership in the review; reject a changed policy before
    /// executing rather than silently updating another flake or profile.
    #[serde(default)]
    pub policy: Option<policy::Policy>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    Idle,
    Checking,
    Reviewing,
    Running,
    Completed,
    Partial,
    Failed,
    Interrupted,
    Cancelled,
    Restarting,
    AwaitingVerification,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Restart {
    Normal,
    Dnf5Offline,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResultEntry {
    pub source: SourceId,
    pub note: String,
    pub success: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub protocol: u32,
    pub revision: u64,
    pub job: u64,
    pub phase: Phase,
    pub sources: Vec<Source>,
    pub selected: Vec<SourceId>,
    pub active: Option<SourceId>,
    pub output: Vec<String>,
    pub results: Vec<ResultEntry>,
    pub message: String,
    /// Raw PTY echo state; not proof of a password prompt. See prompt::password.
    pub secret: bool,
    pub child: Option<u32>,
    pub started: u64,
    #[serde(default)]
    pub restart: Option<Restart>,
    #[serde(default)]
    pub boot_id: String,
    #[serde(default)]
    pub notices: Vec<String>,
    #[serde(default)]
    pub protected: bool,
    #[serde(default)]
    pub authorized: bool,
    #[serde(default)]
    pub custom_result: Option<custom::Applied>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            protocol: PROTOCOL,
            revision: 0,
            job: 0,
            phase: Phase::Idle,
            sources: vec![],
            selected: vec![],
            active: None,
            output: vec![],
            results: vec![],
            message: "Not checked yet".into(),
            secret: false,
            child: None,
            started: 0,
            restart: None,
            boot_id: String::new(),
            notices: vec![],
            protected: false,
            authorized: false,
            custom_result: None,
        }
    }
}

impl Snapshot {
    pub fn busy(&self) -> bool {
        matches!(
            self.phase,
            Phase::Checking | Phase::Running | Phase::Restarting
        )
    }
}

// Input travels separately as a bounded raw line, never as a debug-printable
// request or a persisted input record. Native output may echo nonsecret answers.
#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    Status,
    Check { selected: Vec<SourceId> },
    Install { job: u64 },
    CancelCheck { job: u64 },
    Restart { job: u64 },
    History,
    Output { job: u64, offset: u64 },
    Events,
    Delivered { event: String },
    Acknowledge { event: String },
    Preferences,
    SetDailyCheck(bool),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub snapshot: Snapshot,
    pub error: Option<String>,
    pub history: Vec<Snapshot>,
    #[serde(default)]
    pub history_requested: bool,
    pub daily_check: bool,
    #[serde(default)]
    pub log: Option<journal::Chunk>,
    #[serde(default)]
    pub events: Vec<JobEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobEvent {
    pub id: String,
    pub job: u64,
    pub phase: Phase,
    pub restart: Option<Restart>,
    pub attention: bool,
    #[serde(default)]
    pub delivered: bool,
}

/// A short, stable name for a state directory: what its transient service
/// unit and its control socket are both called after, so that one state
/// directory has exactly one of each and two accounts' (or two test runs')
/// coordinators never answer each other's shell.
pub fn stamp(path: &std::path::Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hash);
    hash.finish()
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
