//! What this helper says, and the one thing the shell and it have to agree
//! about.
//!
//! The same arrangement as `lxb-retroarch`'s `report.rs`, for the same reason:
//! the two halves are built together and installed apart, so everything they
//! say to each other is one JSON object per line on this binary's stdout, and
//! everything either assumes about the other is [`PROTOCOL`]. A newer helper
//! may add a field; one that changes what an existing field means raises the
//! number, and a shell that does not know it puts the integration away.
//!
//! ## Words, not sentences
//!
//! Nothing here is shown to anybody as it stands. Where something did not
//! happen the record carries a [`Reason`] — one word — and the shell turns it
//! into a sentence of its own, in the person's language and without the names
//! of files, daemons or error codes on it. The `note` beside it is English for
//! the session log, which is where "why" is answered for whoever looks.

use serde::{Deserialize, Serialize};

/// The revision of everything in this file.
pub const PROTOCOL: u32 = 1;

/// Why something did not happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Reason {
    /// There is no flatpak on this machine, so Heroic cannot be installed from
    /// here and the shell should not offer to.
    NoFlatpak,
    /// There is no Heroic, and what was asked needs one.
    NoHeroic,
    /// Heroic is open. It reads its account, its library and its settings when
    /// it starts and keeps them in memory, so anything changed behind its back
    /// is something it goes on not knowing — and its next launch of a game can
    /// fail on it. See `heroic::running`.
    HeroicRunning,
    /// Nobody is signed in to Epic in Heroic.
    SignedOut,
    /// A server could not be reached.
    Offline,
    /// flatpak would not install Heroic.
    FlatpakRefused,
    /// Heroic's own default Proton could not be put in place: no release for
    /// this machine, a download that did not match its checksum, or one that
    /// would not unpack.
    Proton,
    /// The code on the screen ran out before it was approved, as many times as
    /// this offers a fresh one.
    Expired,
    /// Epic wants something done in a browser first — accepting changed terms,
    /// usually — at the address carried beside this.
    ActionNeeded,
    /// Epic refused, for a reason it gave in a form not worth repeating.
    Declined,
    /// legendary, inside Heroic's sandbox, did not do what it was asked.
    Legendary,
    /// Heroic's own files could not be read or written.
    Files,
    /// The drive games go to has too little room left for this one.
    NoSpace,
    /// The account does not hold a game by that name.
    NotOwned,
}

/// What `lxb-heroic probe` answers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Probe {
    pub protocol: u32,
    /// The Heroic that would be used, or `None` for a machine with none.
    pub heroic: Option<Installation>,
    /// Whether flatpak is on this machine at all, which decides whether the
    /// shell may offer to install Heroic.
    pub flatpak: bool,
    /// Whose Epic account Heroic is signed in as.
    pub account: Option<String>,
    /// The Proton Heroic will start games with, where it is actually on the
    /// disk. `None` is a Heroic that would stop on its first launch to ask
    /// which one to use, in a window the shell is keeping off the screen — so
    /// the shell treats it as setup still to be done.
    pub proton: Option<String>,
    /// Whether a Heroic is running right now.
    pub running: bool,
    /// Where Heroic installs games — its own `defaultInstallPath`.
    #[serde(default)]
    pub base: Option<String>,
    /// Whether Heroic syncs saves with Epic around a launch — its global
    /// `autoSyncSaves`.
    #[serde(default)]
    pub cloud_saves: bool,
}

/// One Heroic, as found.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Installation {
    pub kind: Kind,
    /// What starts it, as an argv. The shell starts a game with this and
    /// `--no-gui --no-sandbox heroic://launch?appName=…&runner=legendary&gui=false`
    /// after it — Heroic's own shortcut, word for word — through its own
    /// machinery, so the game is watched and closed like every other
    /// application. The shell also clears `ELECTRON_RUN_AS_NODE`, which turns
    /// Heroic into plain Node where it has leaked into a session.
    pub command: Vec<String>,
    /// What it calls itself. For the log.
    pub version: Option<String>,
    /// Heroic's own configuration directory, as this machine spells it.
    pub config: String,
}

/// Which flatpak installation it is in.
///
/// Only flatpaks: the integration is built on Heroic's Flathub build, whose
/// bundled legendary and whose sandbox paths everything here is written
/// against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// In this user's own installation — what `install` puts there.
    FlatpakUser,
    /// Installed for the whole machine by somebody.
    FlatpakSystem,
}

/// One line of `lxb-heroic install` as it happens.
///
/// The last line is always `Done` or `Failed`, so the shell can tell the run
/// is over without watching the process as well.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Progress {
    pub protocol: u32,
    pub stage: Stage,
    /// How far along the stage is, 0 to 1, where there is a number to give.
    pub progress: Option<f32>,
    /// Why it stopped, on `Failed`.
    pub reason: Option<Reason>,
    /// English, for the log.
    pub note: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Stage {
    /// Making sure this user can install from Flathub.
    Remote,
    /// flatpak fetching Heroic and what it runs on.
    Installing,
    /// Fetching Heroic's own default Proton and making it Heroic's default.
    Proton,
    Done,
    Failed,
}

/// One line of `lxb-heroic sign-in`.
///
/// `Code` comes first and may come again: a code lasts ten minutes, and one
/// that runs out is replaced rather than ended on, a few times over, the way
/// the Steam panel follows a QR code that rotates. The run ends on `SignedIn`
/// or `Failed`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub enum SignIn {
    /// A code to approve. `url` carries it and is what the QR code encodes;
    /// `short_url` and `code` are the same thing for somebody typing it on a
    /// laptop instead.
    Code {
        protocol: u32,
        code: String,
        url: String,
        short_url: String,
        expires_in: u32,
    },
    /// Approved on the phone; Heroic is being signed in with it now.
    Approved {
        protocol: u32,
        name: Option<String>,
    },
    SignedIn {
        protocol: u32,
        name: String,
    },
    Failed {
        protocol: u32,
        reason: Reason,
        /// Where to go, for [`Reason::ActionNeeded`].
        url: Option<String>,
        /// English, for the log.
        note: String,
    },
}

/// What `lxb-heroic sign-out` answers: whose account Heroic holds now.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub protocol: u32,
    pub name: Option<String>,
    /// Why the account is not what was asked for, where it is not.
    pub reason: Option<Reason>,
}

/// What `lxb-heroic library` answers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Library {
    pub protocol: u32,
    pub account: Option<String>,
    /// Whether the list was asked of Epic in this run rather than read off
    /// the disk as the last refresh left it.
    pub refreshed: bool,
    /// Why a refresh that was asked for did not happen. The games are still
    /// the last good list: a failed refresh never empties a library.
    pub reason: Option<Reason>,
    /// Alphabetical by title. The shell orders them its own way.
    pub games: Vec<Game>,
}

/// One game in the account, by Heroic's own rules for what counts as one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Game {
    /// Epic's name for it, which is what every legendary verb and Heroic's
    /// launch address take.
    pub app_name: String,
    pub title: String,
    pub developer: Option<String>,
    /// The tall box art, which is the cover on the card.
    pub cover: Option<String>,
    /// The wide box art, which stands behind the display.
    pub hero: Option<String>,
    pub logo: Option<String>,
    /// Where the cover is on this disk, once `lxb-heroic art` has fetched it.
    #[serde(default)]
    pub cover_file: Option<String>,
    /// Where the backdrop is on this disk, once it has been fetched.
    #[serde(default)]
    pub hero_file: Option<String>,
    /// Where the logo is on this disk, once it has been fetched.
    #[serde(default)]
    pub logo_file: Option<String>,
    pub installed: Option<Installed>,
    /// Which other store actually runs it, where one does.
    pub store: Option<Store>,
    /// Whether Epic says it can be played without a connection.
    pub offline: bool,
    /// Whether Epic keeps its saves.
    pub cloud_saves: bool,
    /// As Heroic counts it, in minutes.
    pub played_minutes: u64,
    /// When it was last played, as Heroic wrote it down.
    pub last_played: Option<String>,
    /// How many achievements Epic defines for it, as legendary last wrote
    /// down: `Some(0)` for a game with none, `None` where it has not said.
    #[serde(default)]
    pub achievements: Option<u32>,
}

/// One line of `lxb-heroic art`: a game whose pictures changed, or — with
/// `done` set and no game — the end of the run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artwork {
    pub protocol: u32,
    pub app_name: String,
    pub cover: Option<String>,
    pub hero: Option<String>,
    #[serde(default)]
    pub logo: Option<String>,
    /// Which game of how many this is.
    pub at: u32,
    pub of: u32,
    pub done: bool,
}

/// What `lxb-heroic size <app>` answers: what installing it would take.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Size {
    pub protocol: u32,
    pub app_name: String,
    /// What comes down, in bytes. `None` where Epic was not asked, or would not
    /// say — and for a title another store downloads, whose size is that
    /// store's business.
    pub download: Option<u64>,
    /// What it takes on the disk once installed, in bytes.
    pub disk: Option<u64>,
    /// The folder games are installed into, Heroic's own setting, so the shell
    /// can say how much room is left there.
    pub base: Option<String>,
    pub store: Option<Store>,
    pub reason: Option<Reason>,
}

/// One line of `lxb-heroic get <app>` as it happens.
///
/// The last line is always `Done` or `Failed`, and `Done` is only said once
/// Heroic's own list of what is installed carries the game.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Download {
    pub protocol: u32,
    pub app_name: String,
    pub step: Step,
    /// How far along, 0 to 1, where there is a number to give.
    pub progress: Option<f32>,
    /// Bytes downloaded, counting what an earlier, stopped download left.
    pub downloaded: Option<u64>,
    /// Bytes to download in all.
    pub size: Option<u64>,
    pub reason: Option<Reason>,
    /// English, for the log.
    pub note: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Step {
    /// Asking Epic what the game is made of, and what is already here.
    Preparing,
    /// Checking the files of a game already here against Epic's manifest,
    /// before a repair fetches whatever is wrong.
    Checking,
    Downloading,
    Done,
    Failed,
}

/// One thing Heroic can run games with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tool {
    /// The folder's name, which is what Heroic calls it.
    pub name: String,
    /// Its `proton` or `wine`, as Heroic writes the path down.
    pub bin: String,
    pub kind: ToolKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolKind {
    Proton,
    Wine,
}

/// What `lxb-heroic tools` answers: what Heroic can run games with, which it
/// runs them with by default, and which games have one of their own — every
/// one at once, since that is a few hundred small files and a menu should not
/// wait on it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tools {
    pub protocol: u32,
    pub tools: Vec<Tool>,
    pub default: Option<String>,
    /// App name → the name of the tool it runs with, for games with their own.
    pub games: std::collections::BTreeMap<String, String>,
}

/// What `lxb-heroic use-tool` answers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSet {
    pub protocol: u32,
    /// The game it was asked for, or nothing for Heroic's default.
    pub game: Option<String>,
    /// What it now runs with, or nothing for Heroic's default.
    pub tool: Option<String>,
    pub reason: Option<Reason>,
    /// English, for the log.
    pub note: String,
}

/// What `lxb-heroic folder DIR` answers: where Heroic installs games now.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Folder {
    pub protocol: u32,
    pub base: String,
    /// Why it is not the folder asked for, where it is not.
    pub reason: Option<Reason>,
    /// English, for the log.
    pub note: String,
}

/// What `lxb-heroic saves on|off` answers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Saves {
    pub protocol: u32,
    /// Whether Heroic syncs saves now.
    pub on: bool,
    /// How many games' save folders were found and written down.
    pub found: u32,
    pub reason: Option<Reason>,
    /// English, for the log.
    pub note: String,
}

/// One line of `lxb-heroic achievements`: one game's achievements and how many
/// this account has unlocked — or, with `done` set and no game, the end.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Achievements {
    pub protocol: u32,
    pub app_name: String,
    pub total: u32,
    pub unlocked: u32,
    /// Epic's own experience points for them: this account's, and the game's
    /// whole.
    pub xp: u32,
    pub total_xp: u32,
    /// Unlocked first, then under way, then not started, then the hidden
    /// ones — legendary's own order.
    pub list: Vec<Trophy>,
    /// Why this game could not be asked about.
    pub reason: Option<Reason>,
    pub done: bool,
}

/// One achievement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trophy {
    /// Epic's own name for it, which does not change.
    pub name: String,
    pub title: String,
    pub description: String,
    pub unlocked: bool,
    /// When, as seconds since the epoch.
    pub unlocked_at: Option<u64>,
    /// Its icon on this disk.
    pub icon: Option<String>,
    pub xp: u32,
    /// How many players have it, as a percentage.
    pub rarity: Option<f32>,
    /// Whether Epic keeps it secret until it is unlocked.
    pub hidden: bool,
}

/// What `lxb-heroic remove <app>` answers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Removal {
    pub protocol: u32,
    pub app_name: String,
    /// Whether the game is off Heroic's list of what is installed now —
    /// including where it was never on it.
    pub removed: bool,
    pub reason: Option<Reason>,
    /// English, for the log.
    pub note: String,
}

/// A game on this disk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Installed {
    /// Where it is. `None` for a title another store installs, which Heroic
    /// marks installed before anything of the game is on the disk.
    pub path: Option<String>,
    /// In bytes; nought where it is not known.
    pub size: u64,
    pub version: Option<String>,
    /// Whether Epic has a newer build than this one — Heroic's own test: the
    /// version on the disk against the build Epic lists for its platform in
    /// legendary's `assets.json`, as the last refresh left it.
    #[serde(default)]
    pub update: bool,
}

/// The stores that sell games through Epic and run them themselves.
///
/// Heroic installs these by fetching the other store's installer, which the
/// first launch runs in the game's Wine prefix; the game itself then comes
/// down in that store's own window. See `EPIC-TO-DO.MD`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Store {
    Ea,
    Ubisoft,
    /// A third party Heroic does not know how to install.
    Other,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The words on the wire are the shell's to read, so their spelling is
    /// held down here rather than left to whatever serde derives.
    #[test]
    fn the_words_are_spelt_the_way_the_shell_reads_them() {
        assert_eq!(
            serde_json::to_string(&Reason::HeroicRunning).unwrap(),
            "\"heroic-running\""
        );
        assert_eq!(
            serde_json::to_string(&Kind::FlatpakSystem).unwrap(),
            "\"flatpak-system\""
        );
        let code = serde_json::to_value(SignIn::Code {
            protocol: PROTOCOL,
            code: "ABCDEFGH".into(),
            url: "https://www.epicgames.com/activate?userCode=ABCDEFGH".into(),
            short_url: "https://www.epicgames.com/activate".into(),
            expires_in: 600,
        })
        .unwrap();
        assert_eq!(code["event"], "code");
        assert_eq!(code["protocol"], PROTOCOL);
        let signed = serde_json::to_value(SignIn::SignedIn {
            protocol: PROTOCOL,
            name: "Somebody".into(),
        })
        .unwrap();
        assert_eq!(signed["event"], "signed-in");
        assert_eq!(
            serde_json::to_string(&Reason::NoSpace).unwrap(),
            "\"no-space\""
        );
        assert_eq!(
            serde_json::to_string(&Step::Downloading).unwrap(),
            "\"downloading\""
        );
    }
}
