//! What this helper says, and the one thing the shell and it have to agree
//! about.
//!
//! The same contract as `lxb-retroarch`'s and `lxb-heroic`'s, for the same
//! reason: the two halves are built together and installed apart, so what the
//! pair on a given machine says to each other is one JSON object per line on
//! this binary's stdout, and what either assumes about the other is
//! [`PROTOCOL`]. A newer helper may add a field and an older shell ignores it;
//! a record that changes what it means raises the number, and a shell that
//! does not know the number puts the integration away.
//!
//! **Nothing here is a sentence for the screen.** Where something went wrong
//! the record says *which* thing, as a [`Trouble`], and the shell says it in
//! the person's own language from its catalogs. The `note` a record carries is
//! for the session log, in English, and is never drawn.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The revision of everything in this file.
pub const PROTOCOL: u32 = 1;

/// What `lxb-rpcs3 probe` answers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Probe {
    pub protocol: u32,
    /// The RPCS3 that would be used, or `None` for a machine with none.
    pub rpcs3: Option<Installation>,
    /// Whether `flatpak` is here, which is what decides whether the shell may
    /// offer to install RPCS3 at all.
    pub flatpak: bool,
    /// The PlayStation 3 system software that RPCS3 has, `4.93`, or `None`
    /// where it has none — no game boots without it.
    pub firmware: Option<String>,
    /// Where that RPCS3 keeps its configuration, and the console's drives
    /// unless it moved them.
    pub config: Option<String>,
}

/// One RPCS3, as found.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Installation {
    pub kind: Kind,
    /// What starts it, as an argv: RPCS3 with its own window, which is what
    /// the row's Open RPCS3 is.
    pub command: Vec<String>,
    /// What starts a game, as an argv the shell appends `--no-gui
    /// --fullscreen` and the game to — the same program, told not to stop
    /// first. The Flathub build asks "Do you wish to use this build anyway?"
    /// on every start, beside the game, with No (which quits the game) as its
    /// default and no way to stop asking; it asks because `FLATPAK_ID` is
    /// set, so a game is started with that one variable taken away inside the
    /// sandbox (`--command=env … -u FLATPAK_ID rpcs3`). The user chose the
    /// flatpak as the way RPCS3 arrives; a question over every boot is not
    /// part of that choice. Its own window, [`Installation::command`], still
    /// asks.
    pub play: Vec<String>,
    /// What it calls itself, where that is cheap to learn. For the log.
    pub version: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// The distribution's package, run as `rpcs3`.
    System,
    /// A flatpak in this user's own installation — what this helper installs.
    FlatpakUser,
    /// A flatpak somebody installed for the whole machine.
    FlatpakSystem,
    /// RPCS3's own AppImage.
    AppImage,
}

/// One line of `lxb-rpcs3 setup` — RPCS3 where there is none, then the
/// system software where RPCS3 has none — or of `lxb-rpcs3 add`, a package
/// being installed. The last line is always `done` or `failed`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Progress {
    pub protocol: u32,
    pub stage: Stage,
    /// 0 to 1, or `None` where nothing a bar could be drawn from is known yet.
    pub progress: Option<f32>,
    /// On `failed`, what went wrong.
    pub trouble: Option<Trouble>,
    /// For the log.
    pub note: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Stage {
    /// Making sure this user can see Flathub.
    Remote,
    /// RPCS3 itself coming down from Flathub.
    Installing,
    /// The PlayStation 3 system software coming down from Sony.
    Downloading,
    /// RPCS3 installing the system software.
    Firmware,
    /// A zipped package being taken out of its zip, where RPCS3 can read it.
    Unpacking,
    /// RPCS3 installing a package.
    Adding,
    Done,
    Failed,
}

/// What went wrong, as a thing the shell can say in the person's language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Trouble {
    /// No `flatpak` to install RPCS3 with.
    NoFlatpak,
    /// There is no RPCS3 to do this with.
    NoRpcs3,
    /// A server could not be reached.
    Offline,
    /// The disk filled up.
    NoSpace,
    /// flatpak refused to install RPCS3.
    Refused,
    /// RPCS3 did not install the system software.
    FirmwareRejected,
    /// The file is not a package this can install — a PSP or Vita package, a
    /// damaged one, a zip without one in it.
    NotAPackage,
    /// RPCS3 did not install the package.
    PackageRejected,
    /// Anything else. The note says what.
    Other,
}

/// What `lxb-rpcs3 scan` answers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Library {
    pub protocol: u32,
    /// The folder this was read from, as given, or `None` for a scan of the
    /// installed games alone.
    pub folder: Option<String>,
    pub games: Vec<Game>,
    /// Why the folder could not be read, where it could not. The shell says so
    /// on the row, because an unplugged drive must not read as "no games".
    pub unreadable: Option<String>,
}

/// One PlayStation 3 game, however it is kept.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Game {
    /// Stable for as long as the game is where it is: its path, or
    /// `hdd0/<folder>` for an installed one.
    pub id: String,
    /// `BLUS30359`, where the game says.
    pub serial: Option<String>,
    /// The name it gives itself.
    pub title: String,
    /// Its name in the languages it has one in, keyed as the shell's catalogs
    /// are named — the shell shows the one in its own language, as a PS3 did.
    pub titles: BTreeMap<String, String>,
    pub form: Form,
    /// The file or folder the game is.
    pub path: String,
    /// What RPCS3 is given to start it. `None` for a package that has to be
    /// installed first.
    pub boot: Option<String>,
    /// The game's own version, `01.02`.
    pub version: Option<String>,
    /// PARAM.SFO's category — `DG` disc, `HG` installed, `GD` game data.
    pub category: Option<String>,
    /// The game's pictures and sounds, as files the shell can open: its icon
    /// (ICON0, 320×176), its backdrop (PIC1, 1920×1080), the picture that
    /// stands over the backdrop (PIC0), its animated icon (ICON1.PAM) and its
    /// music (SND0.AT3). Read out of the game itself and kept in the shell's
    /// cache where the game is an image or a package; the files themselves
    /// where it is a folder.
    pub icon: Option<String>,
    pub backdrop: Option<String>,
    pub overlay: Option<String>,
    pub preview: Option<String>,
    pub music: Option<String>,
    /// Whether RPCS3 has what it needs to read the disc.
    pub key: Key,
    /// The game's trophy set, `NPWR00482_00`, where it has one.
    pub trophies: Option<String>,
    /// Bytes on the disk.
    pub size: u64,
    /// Bytes RPCS3 keeps for it that it can make again — what it compiled
    /// for the game, and the game's system cache. `lxb-rpcs3 clear-cache`.
    #[serde(default)]
    pub cache: u64,
    /// For a package: the updates to the same game downloaded beside it, in
    /// version order, which are installed after it — see `scan`'s
    /// `one_row_per_game`.
    #[serde(default)]
    pub updates: Vec<String>,
    /// For a package: the licence beside it, and whether that licence is
    /// already where RPCS3 looks for it.
    pub licence: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Form {
    /// A disc image, `.iso`.
    Disc,
    /// A game unpacked into a folder — `PS3_GAME/USRDIR/EBOOT.BIN`.
    Folder,
    /// A `.pkg`, or a zip with one in it, not installed yet.
    Package,
    /// A game RPCS3 has installed, under its `dev_hdd0/game`.
    Installed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Key {
    /// Not an encrypted disc.
    Unneeded,
    /// An encrypted disc whose key RPCS3 will find.
    Present,
    /// An encrypted disc with no key yet — `lxb-rpcs3 key` fetches it.
    Missing,
}

/// What `lxb-rpcs3 key` answers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyResult {
    pub protocol: u32,
    pub key: Key,
    /// Why there is still no key, where there is not.
    pub trouble: Option<Trouble>,
    /// For the log: which of Redump's keys it was.
    pub note: String,
}

/// What `lxb-rpcs3 remove` and `lxb-rpcs3 clear-cache` answer: whether what
/// was asked to go has gone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Removal {
    pub protocol: u32,
    pub removed: bool,
    /// Why it is still there, where it is.
    pub trouble: Option<Trouble>,
    /// Bytes given back to the disk.
    pub freed: u64,
    /// For the log.
    pub note: String,
}

/// What `lxb-rpcs3 mark` answers: the package's mark, as the drawing a
/// `share/lxb/glyphs/<name>.svg` would hold.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mark {
    pub protocol: u32,
    /// The file name it would have there, without `.svg` — the shell draws it
    /// as `lxb:<name>`.
    pub name: String,
    pub drawing: String,
}

/// What `lxb-rpcs3 permit` answers: whether RPCS3 can read the folder now.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Permission {
    pub protocol: u32,
    pub permitted: bool,
    pub note: String,
}
