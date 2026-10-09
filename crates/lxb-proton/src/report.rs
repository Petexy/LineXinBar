//! What this helper says, and the one thing the shell and it have to agree
//! about.
//!
//! The same contract as `lxb-retroarch`'s, `lxb-heroic`'s and `lxb-rpcs3`'s,
//! for the same reason: the two halves are built together and installed apart,
//! so what the pair on a given machine says to each other is one JSON object
//! per line on this binary's stdout, and what either assumes about the other
//! is [`PROTOCOL`]. A newer helper may add a field and an older shell ignores
//! it; a record that changes what it means raises the number, and a shell that
//! does not know the number puts the integration away.
//!
//! **Nothing here is a sentence for the screen.** Where something went wrong
//! the record says *which* thing, as a [`Trouble`], and the shell says it in
//! the person's own language from its catalogs. The `note` a record carries is
//! for the session log, in English, and is never drawn.

use serde::{Deserialize, Serialize};

/// The revision of everything in this file.
pub const PROTOCOL: u32 = 1;

/// What `lxb-proton probe` answers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Probe {
    pub protocol: u32,
    /// The Proton a new prefix would be made with, or `None` on a machine
    /// that has neither an installed Valve build nor a managed one yet.
    ///
    /// `None` is not a failure: it is the state a machine is in before the
    /// first download, and the shell offers that download rather than saying
    /// the integration is broken.
    pub proton: Option<Tool>,
    /// Whether Valve's client is installed, which is the one thing that
    /// decides whether a game is offered a Compatibility choice at all.
    ///
    /// A directory a removed Steam left behind is not an installed Steam. See
    /// `steam::Steam::found`.
    pub steam: bool,
    /// Where the managed Proton and runtimes are kept — under
    /// `$XDG_DATA_HOME/lxb/proton`. Said so a bug report can name it.
    pub store: String,
    /// Every root the shell has made prefixes under, the one new prefixes go
    /// in first. More than one only where somebody has moved the folder.
    pub prefix_roots: Vec<String>,
}

/// One thing a Windows game can be run with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tool {
    /// What it calls itself — the directory Steam unpacked it into, or the
    /// release tag of the managed build.
    pub name: String,
    /// The `proton` script itself, which is what a launch runs.
    pub bin: String,
    /// Where it came from, which is what decides whether this helper may
    /// write to it.
    pub origin: Origin,
    /// Its own `version` file, `11.0-100` style, where it has one. For the
    /// log, and for telling two builds of the same name apart.
    pub version: Option<String>,
    /// The runtime its `toolmanifest.vdf` asks for by `require_tool_appid`,
    /// and whether this machine has it.
    ///
    /// A Proton whose runtime is missing is still listed. It is a download
    /// away from working, and a list that quietly left it out would be a
    /// Compatibility menu that disagreed with the disk.
    pub runtime: Option<Runtime>,
}

/// Where a Proton came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Origin {
    /// Valve's own, installed by Steam into a library's `steamapps/common`.
    /// Read where it lies and never written to: it is Valve's client's, and
    /// an update of ours that touched it would be writing into another
    /// program's installation.
    Valve,
    /// Somebody's own build, in a library's `compatibilitytools.d`. Found so
    /// that a machine with one is not told it has no Proton, and left alone.
    Custom,
    /// The one this helper fetched into its own store. The only Proton it
    /// ever writes to, and the only one Updates offers to update.
    Managed,
}

/// The container runtime a Proton asks for, resolved from its own manifest
/// rather than assumed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Runtime {
    /// The `require_tool_appid` the Proton's `toolmanifest.vdf` names.
    /// Carried as the number it is: it is what the answer is looked up from,
    /// and a shell reading a log wants to see the same number Valve uses.
    pub app_id: u32,
    /// What Valve calls that generation — `steamrt4`. `None` for an id this
    /// build has never heard of, which is how a future generation arrives.
    pub suite: Option<String>,
    /// Its `_v2-entry-point`, which is the outer half of a launch, where this
    /// machine has the runtime at all.
    pub entry_point: Option<String>,
    /// Which build of it is on the disk, out of the runtime's own
    /// `VERSION.txt` — `4.0.20260805.254769`.
    pub version: Option<String>,
    /// Where it came from, on the same terms as a Proton's.
    pub origin: Option<Origin>,
}

/// What `lxb-proton tools` answers: everything this machine could run a
/// Windows game with, and which one it would use.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tools {
    pub protocol: u32,
    /// Newest first, Valve's before anybody else's. The shell draws them in
    /// this order and never sorts them itself: what "newest" means is this
    /// helper's business — see `tools::newness`.
    pub tools: Vec<Tool>,
    /// The name of the one a new prefix would be made with.
    pub chosen: Option<String>,
    /// Whether Valve's client is installed, repeated here so a Compatibility
    /// menu can be drawn from this one record.
    pub steam: bool,
}

/// What `lxb-proton offer` answers: what a machine with no usable Proton
/// would have to fetch, named and measured, before anything is fetched.
///
/// Asked before the download so the panel can say how large it is, and
/// separately from [`Fetch`] so that "what would this cost" never starts it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Offer {
    pub protocol: u32,
    /// The Proton release that would be fetched, where one is needed.
    pub proton: Option<Download>,
    /// The runtime that would be fetched, where one is needed. Resolved from
    /// the Proton above, not assumed.
    pub runtime: Option<Download>,
    /// On failure, what went wrong.
    pub trouble: Option<Trouble>,
    /// For the log.
    pub note: String,
}

/// One thing that would be downloaded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Download {
    /// What it is called where it comes from — a release tag, or a runtime
    /// build string.
    ///
    /// `None` for a runtime on a machine that has no Proton yet: which
    /// generation is needed is a fact about the build that has not been
    /// fetched, and this would rather say "a runtime, and I cannot say
    /// which" than name the one that happens to be current. Both Protons
    /// this could fetch ask for the same generation today, which is exactly
    /// the coincidence not to build on.
    pub name: Option<String>,
    /// How many bytes, where the server says. `None` is honest: GitHub gives
    /// a size in its release list and Valve's directory index does not.
    pub bytes: Option<u64>,
    /// Whether this machine already has exactly this one, in which case the
    /// fetch is a no-op and the panel says so rather than offering a
    /// download of nothing.
    pub already: bool,
}

/// One line of `lxb-proton fetch`. The last line is always `done` or
/// `failed`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Progress {
    pub protocol: u32,
    pub stage: Stage,
    /// 0 to 1, or `None` where nothing a bar could be drawn from is known
    /// yet.
    pub progress: Option<f32>,
    /// On `failed`, what went wrong.
    pub trouble: Option<Trouble>,
    /// For the log.
    pub note: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Stage {
    /// Asking where the newest build is.
    Looking,
    /// The Proton itself coming down.
    Proton,
    /// The runtime it asks for coming down.
    Runtime,
    /// A download being checked against the digest published beside it, and
    /// a runtime being checked against its own manifest, before anything is
    /// moved into place.
    Checking,
    /// A checked download being moved into place.
    Settling,
    Done,
    Failed,
}

/// What went wrong, as a thing the shell can say in the person's language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Trouble {
    /// A server could not be reached.
    Offline,
    /// What came down did not match the digest published beside it, or the
    /// runtime did not match its own manifest. Never used, and never kept.
    Damaged,
    /// The disk filled up.
    NoSpace,
    /// A file could not be read or written.
    Files,
    /// There is no build for this machine's processor. The managed Proton is
    /// published for x86-64 and arm64 and nothing else.
    WrongMachine,
    /// A newer Proton asks for a runtime generation this build has never
    /// heard of, so there is nothing to fetch it from.
    UnknownRuntime,
    /// A game or an installer is still using the prefix, so it may not be
    /// removed.
    InUse,
    /// What this game plays is not there any more.
    ///
    /// Only ever a program run where the person keeps it — see
    /// `prefix::Kept::play` — which is the one thing a game points at that
    /// this shell does not own. They moved the folder, or deleted the
    /// download it was in.
    Gone,
    /// Anything else. The note says what.
    Other,
}

/// What `lxb-proton games` answers: the Windows games on this machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Games {
    pub protocol: u32,
    /// In the order the folders are searched, which puts the folder new
    /// games go in first. The shell sorts them for the screen; this order
    /// is about the disk.
    pub games: Vec<GameRow>,
}

/// One Windows game, as the shell draws it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameRow {
    /// The identifier. The shell knows this game as `windows:<id>` — never
    /// as a number, because a numeric identifier is read elsewhere in the
    /// shell as a Steam app id and this is not one.
    pub id: String,
    pub name: String,
    /// Whether it can be played, or is an install that was interrupted.
    pub state: GameState,
    /// The Proton the user chose for it, where they have chosen one.
    pub proton: Option<String>,
    /// Its own directory. For the log, for Uninstall's confirmation, and
    /// for Show in folder.
    pub at: String,
    /// What a press of Play runs: a path inside the game's own folder, or an
    /// absolute one for a program run where it lives — see
    /// [`GameRow::runs_in_place`]. `None` while nothing has been named. The
    /// menu that offers to change it ticks this one.
    #[serde(default)]
    pub play: Option<String>,
    /// Whether Proton has ever built the prefix in there.
    ///
    /// Beside `state` rather than folded into it, because the two together
    /// are what a person needs told. Unfinished with no prefix is an
    /// install that never got going; unfinished *with* one is an installer
    /// that ran and left nothing anybody can play, which is the one to
    /// offer **Run .exe inside of...** for. And a game imported as a folder
    /// is ready with no prefix at all until the first time it is played.
    #[serde(default)]
    pub prefix_built: bool,
    /// Whether what it plays is a program outside its own folder — one
    /// pressed in Files and run where the person keeps it, rather than
    /// anything this shell copied.
    ///
    /// The shell reads one thing off it: whether a launch ending in this
    /// prefix is worth asking `settled` about. Everything else about it is
    /// the helper's — see `install::settled`.
    #[serde(default)]
    pub runs_in_place: bool,
    /// The program's own drawing, written out beside it as a PNG — what the
    /// row wears instead of the mark every Windows game used to share. See
    /// [`crate::icon`]. `None` for a game with nothing named to play yet,
    /// and for a program that carries no icon.
    #[serde(default)]
    pub cover: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GameState {
    /// There is something to play.
    Ready,
    /// Ours, and not finished: either Proton never completed the prefix or
    /// nothing has been named to run. Shown, and recoverable through
    /// **Run .exe inside of...**.
    Unfinished,
}

/// What `lxb-proton start` answers: how to run one game. Nothing is
/// started here — the shell starts it, so the game gets the loading screen
/// and the display it was pressed on.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Start {
    pub protocol: u32,
    /// The whole command, program first.
    pub argv: Vec<String>,
    /// What to add to the environment. Nothing is to be removed.
    pub environment: Vec<(String, String)>,
    /// Where to run it — the executable's own folder, because `proton`
    /// changes directory for nobody and many Windows games read their data
    /// relative to it.
    pub directory: String,
    /// Which Proton this command runs under, so the log says.
    pub proton: Option<String>,
    pub trouble: Option<Trouble>,
    pub note: String,
}

/// What `lxb-proton remove` answers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Removal {
    pub protocol: u32,
    pub id: String,
    pub removed: bool,
    pub trouble: Option<Trouble>,
    pub note: String,
}

/// What `lxb-proton end` answers: what was still running in a game's Windows
/// when it was asked, and what had to be killed.
///
/// Counted rather than named, because the shell has nothing to say to a
/// person about a process — see the log for the rest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ended {
    pub protocol: u32,
    pub id: String,
    /// How many processes were still running in that game.
    pub found: u32,
    /// How many of them did not go when asked and were killed.
    pub killed: u32,
    /// How many are still there after all of it, which on any ordinary
    /// machine is none.
    pub left: u32,
    pub trouble: Option<Trouble>,
    pub note: String,
}

/// What `lxb-proton new` and `lxb-proton import` answer: the game that was
/// made, and how to run the executable that sets it up.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Made {
    pub protocol: u32,
    /// The game, so the shell has its identifier at once — the prefix is
    /// its own even before anything has been installed into it, and an
    /// install that is interrupted is recoverable because of that.
    pub game: Option<GameRow>,
    /// How to run the installer, for `new`. `None` for an import, which
    /// starts nothing.
    pub start: Option<Start>,
    pub trouble: Option<Trouble>,
    pub note: String,
}

/// What `lxb-proton found` answers: the executables inside a prefix that
/// could be the game, likeliest first.
///
/// A ranking rather than a choice. The shell confirms one with the person,
/// because an installer leaves uninstallers, crash handlers and
/// redistributables behind and a confident wrong answer is worse than an
/// ordered list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Found {
    pub protocol: u32,
    pub id: String,
    pub candidates: Vec<Candidate>,
}

/// One executable that could be the game.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    /// Where it is, inside the game's own directory.
    pub at: String,
    /// What it is called, without the extension — the name to offer for
    /// the game itself.
    pub name: String,
    pub bytes: u64,
}

/// What `lxb-proton mark` answers: the drawing this package carries for the
/// column, the row, and every game without a picture of its own.
///
/// Answered before anything on the machine is looked at, so a shell can get
/// the mark out of a helper it has only just found.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mark {
    pub protocol: u32,
    /// The file name it would have under `share/lxb/glyphs`, without
    /// `.svg` — the shell draws it as `lxb:<name>`.
    pub name: String,
    pub drawing: String,
}

/// What `lxb-proton icon` answers: whether that Windows program carries a
/// drawing of its own, and where it was put.
///
/// **The drawing does not cross the pipe.** It is asked for on behalf of one
/// row of the shell's file explorer, which wants a picture to decode rather
/// than a field to parse — so the shell names the file it wants it written
/// to and this says whether there was one. The same bargain
/// `ffmpegthumbnailer` makes, which is the other program the shell asks for
/// a picture of one of somebody's files.
///
/// Not [`Mark`], which carries its drawing whole. That one is a glyph this
/// package ships, it is asked for once while the shell starts, and it is
/// text; this is a picture out of a file on the disk, asked for a row at a
/// time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Icon {
    pub protocol: u32,
    /// Where the PNG was written, or `None` for a file that carries no
    /// drawing this can read.
    ///
    /// `None` is an answer and not a failure. A great many programs carry no
    /// icon at all, and a `.bat` is a text file with no resources in it —
    /// the shell draws the Windows mark for those and does not ask about
    /// that file again.
    pub png: Option<String>,
    pub note: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every word that crosses the pipe, spelt the way the shell reads it.
    ///
    /// The shell declares these enums again rather than sharing them — the
    /// two are installed apart — so nothing but a test can catch a rename on
    /// one side. See `crates/lxb-desktop/src/proton.rs`.
    #[test]
    fn the_words_are_spelt_the_way_the_shell_reads_them() {
        for (value, spelt) in [
            (Origin::Valve, "\"valve\""),
            (Origin::Custom, "\"custom\""),
            (Origin::Managed, "\"managed\""),
        ] {
            assert_eq!(serde_json::to_string(&value).unwrap(), spelt);
        }

        for (value, spelt) in [
            (Stage::Looking, "\"looking\""),
            (Stage::Proton, "\"proton\""),
            (Stage::Runtime, "\"runtime\""),
            (Stage::Checking, "\"checking\""),
            (Stage::Settling, "\"settling\""),
            (Stage::Done, "\"done\""),
            (Stage::Failed, "\"failed\""),
        ] {
            assert_eq!(serde_json::to_string(&value).unwrap(), spelt);
        }

        for (value, spelt) in [
            (Trouble::Offline, "\"offline\""),
            (Trouble::Damaged, "\"damaged\""),
            (Trouble::NoSpace, "\"no-space\""),
            (Trouble::Files, "\"files\""),
            (Trouble::WrongMachine, "\"wrong-machine\""),
            (Trouble::UnknownRuntime, "\"unknown-runtime\""),
            (Trouble::InUse, "\"in-use\""),
            (Trouble::Other, "\"other\""),
        ] {
            assert_eq!(serde_json::to_string(&value).unwrap(), spelt);
        }
    }

    /// A field the shell does not know about yet must not stop it reading the
    /// record: that is the whole of what "additive" means here, and it is
    /// cheap enough to pin.
    #[test]
    fn an_older_reader_survives_a_newer_field() {
        #[derive(Debug, Deserialize)]
        struct OlderProbe {
            protocol: u32,
            steam: bool,
        }
        let newer = serde_json::json!({
            "protocol": 1, "steam": true, "store": "/x",
            "prefix_roots": [], "proton": null, "something_new": 7
        });
        let older: OlderProbe = serde_json::from_value(newer).unwrap();
        assert_eq!(older.protocol, 1);
        assert!(older.steam);
    }
}
