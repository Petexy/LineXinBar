//! RetroArch, as the shell holds it: one row, one column, and the folder
//! somebody keeps their games in.
//!
//! Everything that touches the machine — whether RetroArch is installed,
//! installing it, and reading the ROM folder — is a separate program,
//! `lxb-retroarch`, which ships in a package of its own. **This module is the
//! half that belongs to the shell**, and its first question is always the same
//! one: is that program on this machine at all? Where it is not, every method
//! here answers as though the integration did not exist, and the bar is exactly
//! what it was before this file was written.
//!
//! That is the whole reason for the split. A shell that carried a table of
//! forty consoles, a flatpak installer and a walk over somebody's collection
//! would carry them on every machine that will never emulate anything; a shell
//! that shells out to a program that may not be there carries a `PATH` lookup.
//! See `crates/lxb-retroarch`, and [`Record`] for the one thing the two halves
//! have to agree about.
//!
//! ## The journey, as the row
//!
//! ```text
//!   press RetroArch ─► not installed ─► "Install it?" ─► installing ─┐
//!                   │                                                │
//!                   ├──────────────── no ROM folder ◄────────────────┘
//!                   │                        │
//!                   │                 "where are they?" ─► the folder picker
//!                   │                                              │
//!                   └──────────────── the RetroArch column ◄───────┘
//! ```
//!
//! Each of those is one press and one panel, and which one it is, is
//! [`RetroArch::press`] — asked on the press rather than carried on the row,
//! because every one of those answers is a fact about a helper process and a
//! disk. See [`crate::apps::Emulation`].
//!
//! ## And the cores, which are not RetroArch
//!
//! A console is played by a *core*, which ships with neither RetroArch nor the
//! distribution — the Flathub build has none at all — so a folder full of games
//! and a fresh RetroArch is a column where nothing will start. The shell
//! fetches them, on the two occasions somebody has said so:
//!
//! ```text
//!   the folder is chosen ─► scanned ─► one core per console, without asking
//!   a game with no core   ─► "Get it and play?" ─► that core, then the game
//! ```
//!
//! The first is [`Shell::retroarch_scanned`]'s, and it is the only download in
//! this integration that nobody presses Yes to: choosing a folder *is* the yes.
//! The second is [`Shell::say_no_core`], for the console somebody added last
//! week. Both go through [`RetroArch::fetch`], and neither knows what a core is
//! — the names are a list from the helper's own table and the choosing is the
//! helper's. See `lxb-retroarch`'s `cores.rs`.
//!
//! ## Why the shell launches the games itself
//!
//! The helper answers with a command line and never runs one. A game started
//! from this bar has to be watched for its window, drawn a loading screen,
//! pinned to the display it was started on, and ended by the guide like
//! anything else — and none of that is reachable from a process the shell
//! forked and forgot about. So a ROM row carries its own argv and goes through
//! [`crate::model::Lattice::launch_selected`], which is where every other row
//! on this bar starts what it starts.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Mutex, OnceLock};

use serde::Deserialize;

use crate::apps::{self, Entry};
use crate::icons;
use crate::menu;

/// What the helper is called, and what the shell looks for on `PATH`.
pub const HELPER: &str = "lxb-retroarch";

/// Every name the emulator's own windows call themselves by.
///
/// Two, because the emulator arrives two ways. A distribution package ships a
/// desktop entry declaring `retroarch`, and the Flatpak — which is what this
/// shell installs — puts its own application id on the window instead. Neither
/// can be derived from the other, and a shell that knew only one of them would
/// have a setting that worked on half the machines it runs on.
///
/// Written here rather than taken from `lxb-retroarch`, on the terms everything
/// else this shell knows about that package is written here: a machine may have
/// either package without the other, so what the two agree about is declared on
/// both sides rather than shared.
pub const WINDOW_NAMES: &[&str] = &["retroarch", "org.libretro.RetroArch"];

/// The protocol revision this shell was written against.
///
/// The helper says which it is speaking in every record. A number *newer* than
/// this one means a package has been updated past the shell beside it, and the
/// integration puts itself away rather than acting on a record it half
/// understands — see [`Record::usable`]. An older one cannot happen without
/// somebody installing a package older than this shell, and is refused the same
/// way and for the same reason.
pub const PROTOCOL: u32 = 1;

/// The mark every row that came out of RetroArch wears, and the one over its
/// column.
///
/// A name rather than a drawing: it arrives with the package, under
/// `share/lxb/glyphs`, and a machine without it has none. See
/// [`icons::package_glyphs`].
///
/// **One name for both places**, which it has not always been. There were two
/// drawings and they were the same drawing — the same path data, four per cent
/// apart in scale, which is nothing anybody could see at any size the shell
/// draws a mark. Two names means two cells of the atlas and two files in the
/// package to keep in step, in exchange for a difference that was never on
/// screen. Where a head and a row really do want different air, that is what
/// the *quad* is for.
const MARK: &str = "lxb:retroarch";

/// The mark for a row, or the shell's own pad where the package brought no
/// drawing.
///
/// The fallback is not defensive. A package can be installed with its glyph
/// directory missing — a distribution that split the files differently, a
/// partial install, a `$XDG_DATA_DIRS` that does not name the prefix — and a
/// row with no mark at all would be a hole in the column. The pad says "a game"
/// and is honest about it.
pub fn mark() -> &'static str {
    if icons::shaped(MARK) {
        MARK
    } else {
        icons::CATEGORY_GAMES
    }
}

/// The mark for one console, or RetroArch's own where there is none to be had.
///
/// Asked of the atlas rather than taken on trust, exactly as [`mark`] is and
/// for the same reason: the drawings ship in a package and a package can be
/// installed with its data directory missing. A name nothing drew would go
/// through `ui::shaded` as a name the atlas does not know, which is the one
/// thing that must not happen — see the note on `ui::own_mark`.
fn console_mark(named: Option<&str>) -> String {
    named
        .filter(|name| icons::shaped(name))
        .map_or_else(|| mark().to_string(), str::to_owned)
}

/// Where the helper is, once it has been looked for.
static FOUND: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Look for the helper, once, and remember the answer.
///
/// `named` is what the session was started with — see the shell's
/// `--retroarch-helper`, which is how a helper that has not been installed
/// anywhere can be driven for development. Otherwise prefer a helper beside
/// the shell executable, then walk `PATH`. This keeps a local build paired
/// with its helper even when an older integration is installed system-wide.
///
/// Called once, before the catalogue is built, because the answer decides
/// whether there is a RetroArch row on the bar at all — and the row's mark has
/// to be in the atlas with the rest rather than being asked for on the first
/// frame that draws it.
pub fn look_for_helper(named: Option<PathBuf>) -> Option<&'static Path> {
    let found = FOUND.get_or_init(|| {
        resolve_helper(named, std::env::current_exe().ok().as_deref(), || {
            on_path(HELPER)
        })
    });
    match found {
        Some(at) => tracing::info!(at = %at.display(), "the RetroArch integration is installed"),
        None => tracing::debug!("no RetroArch integration on this machine"),
    }
    found.as_deref()
}

fn resolve_helper(
    named: Option<PathBuf>,
    shell: Option<&Path>,
    on_path: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    if let Some(named) = named {
        return if executable(&named) {
            Some(named)
        } else {
            tracing::warn!(at = %named.display(), "there is no executable helper there");
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

/// Where the integration is, for the one caller that has to run it itself.
///
/// [`offered`] is the question nearly everything asks; this is for the
/// RetroAchievements client, which spawns the helper rather than going through
/// the calls above it and so needs the path [`look_for_helper`] settled on.
pub fn helper() -> Option<&'static Path> {
    FOUND.get().and_then(|p| p.as_deref())
}

/// Whether this machine has the integration at all.
///
/// The one question every other part of the shell asks — the Settings page,
/// the catalogue, the press — and it is answered without touching the disk
/// after the first time.
pub fn offered() -> bool {
    FOUND.get().is_some_and(Option::is_some)
}

/// Where an executable of this name is, walking `PATH` as a shell does.
fn on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|at| executable(at))
}

// --- what the helper says ---------------------------------------------------

/// The one field every record carries, and the only thing the two halves of
/// this integration have to agree about.
///
/// Declared here rather than shared with the helper's crate, and that is the
/// point of the split rather than a duplication of it: `lxb-desktop` does not
/// depend on `lxb-retroarch` at build time, so a machine can have either
/// package without the other. What they share is a wire format, and it is
/// written down on both sides.
trait Record {
    fn protocol(&self) -> u32;

    /// Whether this shell may act on it.
    fn usable(&self) -> bool {
        self.protocol() == PROTOCOL
    }
}

/// What `lxb-retroarch probe` answers.
#[derive(Debug, Clone, Deserialize)]
struct Probe {
    protocol: u32,
    retroarch: Option<Installation>,
    flatpak: bool,
    /// Where RetroArch keeps its own configuration — the one directory this
    /// shell and a sandboxed emulator spell the same way, and therefore the
    /// only place the shell can leave it a file. See [`controllers`].
    #[serde(default)]
    config: Option<String>,
    /// Whether the helper on this machine can fetch a core at all, which is a
    /// fact about the *helper* — one written before the `cores` verb was does
    /// not answer, and the default is what stops the shell offering a download
    /// that would come back as an error. See [`Fetch`].
    #[serde(default)]
    fetches_cores: bool,
}

impl Record for Probe {
    fn protocol(&self) -> u32 {
        self.protocol
    }
}

/// One RetroArch, as found.
#[derive(Debug, Clone, Deserialize)]
pub struct Installation {
    /// What starts it, as an argv. A game's own command line is this with a
    /// core and a path on the end of it.
    pub command: Vec<String>,
    pub version: Option<String>,
}

/// One line of an install as it happens.
#[derive(Debug, Clone, Deserialize)]
struct Progress {
    protocol: u32,
    stage: Stage,
    progress: Option<f32>,
    note: String,
}

impl Record for Progress {
    fn protocol(&self) -> u32 {
        self.protocol
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Stage {
    Remote,
    Installing,
    /// Taking it off again. One command and no bar: flatpak deletes what it
    /// downloaded without saying how far along it is.
    Removing,
    Done,
    Failed,
}

/// What `lxb-retroarch permit` answers.
#[derive(Debug, Clone, Deserialize)]
struct Permission {
    protocol: u32,
    permitted: bool,
    note: String,
}

impl Record for Permission {
    fn protocol(&self) -> u32 {
        self.protocol
    }
}

/// One line of a core coming down.
#[derive(Debug, Clone, Deserialize)]
struct Fetch {
    protocol: u32,
    stage: Getting,
    core: String,
    progress: Option<f32>,
    at: u32,
    of: u32,
    note: String,
}

impl Record for Fetch {
    fn protocol(&self) -> u32 {
        self.protocol
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Getting {
    Looking,
    Downloading,
    Done,
    Failed,
}

/// What `lxb-retroarch scan` answers.
#[derive(Debug, Clone, Deserialize)]
struct Library {
    protocol: u32,
    roms: String,
    consoles: Vec<Console>,
    unreadable: Option<String>,
    /// Where RetroArch reads the files a core needs beside itself. Reported
    /// rather than worked out here: RetroArch's own configuration may point it
    /// anywhere, and a panel telling somebody to put a BIOS in the wrong folder
    /// is worse than one that names no folder at all.
    #[serde(default)]
    system: Option<String>,
}

/// One file a core cannot start without, as libretro describes it.
///
/// Both halves are libretro's own words and neither is composed here — see the
/// helper's `firmware.rs`, and the reason there is no table of BIOS names
/// anywhere in this project.
#[derive(Debug, Clone, Deserialize)]
pub struct Need {
    /// The sentence libretro writes about it, which names the file, the machine
    /// and very often the region: the one thing in this record a person could
    /// take to a search engine, and so the line a panel shows.
    pub note: String,
    /// Where it belongs, below RetroArch's system folder — `pcsx2/bios`. Not
    /// shown to anybody; it is where the shell puts the files somebody points
    /// it at. See [`Firmware`].
    pub path: String,
    /// Whether it is on this disk already.
    #[serde(default)]
    pub here: bool,
}

/// What one console is still waiting for, out of everything its cores declare.
///
/// Not simply everything that is not there. Several cores declare a whole shelf
/// of files: duckstation names all three PlayStation BIOS regions, o2em four
/// Videopac models, uae4arm six Kickstarts. Nobody owns all of those and nobody
/// has to — one of them is what a person's own games run on, and a shell that
/// counted the rest as missing would offer a folder chooser no folder on earth
/// could answer.
///
/// So which of them stand in for each other is read out of where they live.
/// What a core wants in one folder is one job, and having any of it is having
/// what that job needs; a second folder is a second job. duckstation's three
/// sit side by side and are three spellings of one — the machine's boot ROM.
/// pcsx2 declares its BIOS folder and, separately, a game database under
/// `pcsx2/resources`, which are two.
///
/// It is a reading of libretro's own layout rather than a table of consoles,
/// which is the rule this whole integration is held to: a table would be wrong
/// within a year and wrong immediately for the cores whose author changed their
/// mind.
///
/// **Nothing decides in advance whether a game will start.** This says what
/// there is to offer somebody, on a settings row and in the panel a game raises
/// *after failing to start*. Whether a console needs any of it is the
/// emulator's business, and the emulator answers by running.
pub fn unserved(wanted: &[Firmware]) -> Vec<&Firmware> {
    let job = |one: &Firmware| one.into.parent().map(Path::to_path_buf).unwrap_or_default();
    wanted
        .iter()
        .filter(|one| {
            !one.here
                && !wanted
                    .iter()
                    .any(|other| other.here && job(other) == job(one))
        })
        .collect()
}

/// The same, for one console, out of what the last scan found.
///
/// What the panel over a game that would not start reads: it is holding a
/// console's name and has to say what there is to go and look for.
pub fn wanting(console: &str) -> Vec<Firmware> {
    let held: Vec<Firmware> = firmware()
        .into_iter()
        .filter(|wanted| wanted.console == console)
        .collect();
    unserved(&held).into_iter().cloned().collect()
}

/// The same, read off the disk this moment rather than out of the last scan.
///
/// What the press that has just copied files needs. A scan takes a moment to
/// come back and the answer this shell has to give is immediate: did the folder
/// somebody pointed at actually set the console up, or is the question still
/// standing? Believing the old answer would mean saying "nothing was in there"
/// over a BIOS that had just been put down, and — the bug this exists for —
/// saying a console was ready because a folder of holiday photographs had been
/// copied into its firmware directory.
pub fn unserved_now(wanted: &[Firmware]) -> Vec<Firmware> {
    let fresh: Vec<Firmware> = wanted
        .iter()
        .map(|had| Firmware {
            here: on_disk(had),
            ..had.clone()
        })
        .collect();
    unserved(&fresh).into_iter().cloned().collect()
}

impl Record for Library {
    fn protocol(&self) -> u32 {
        self.protocol
    }
}

/// One console's folder, and the games in it.
#[derive(Debug, Clone, Deserialize)]
struct Console {
    title: String,
    /// The mark this console's column and its games wear, as the atlas knows
    /// it — `lxb:console-nes`. `None` for a folder the helper has never heard
    /// of, and for a helper too old to have drawings.
    #[serde(default)]
    glyph: Option<String>,
    core: Option<Core>,
    wanted: Vec<String>,
    /// What the installed core cannot start without and this machine has not
    /// got — a console's own boot ROM, nearly always.
    ///
    /// Nothing here is fetchable: these are the console maker's files, and the
    /// only lawful copy is one dumped from hardware the user owns. So unlike a
    /// missing core, which the row offers to download, this is *said* — and
    /// said before the press, because a core that cannot boot starts and exits
    /// with nothing on the screen, which from the sofa is indistinguishable
    /// from this shell being broken. See the helper's `firmware.rs`.
    #[serde(default)]
    needs: Vec<Need>,
    /// Whether that core is missing the folder of files it reads beside
    /// itself, which some emulators cannot draw a letter without.
    ///
    /// It means the same here as having no core: the game would start, and it
    /// would run with blank boxes where its menus are. So the row says what it
    /// says for a console with nothing to play it, and pressing a game fetches
    /// the missing half — the helper works out that the core itself is already
    /// there. Defaulted for a helper too old to say, which is the answer it
    /// would give.
    #[serde(default)]
    incomplete: bool,
    roms: Vec<Rom>,
    /// The shape this console's covers are, measured off them — see
    /// [`shelf_shape`], and [`crate::apps::Rom::shape`] for what it is for.
    ///
    /// Not in the record and never sent: it is a fact about files this shell
    /// put in its own cache, and the helper measuring them would be a second
    /// answer to a question the shell can answer by opening one of them.
    ///
    /// Worked out at the two moments it can change — a scan landing, and a
    /// cover arriving — rather than when the rows are built, because the rows
    /// are built from an event and reading eight files inside one is a thing to
    /// do once per event rather than once per console per event.
    #[serde(skip)]
    shape: Option<f32>,
}

/// The core a console resolved to.
///
/// Its `name` is never drawn on a row. A libretro core's name is a fact about
/// this machine's insides rather than about somebody's games, and it was taken
/// off the console's line for that reason. It is read here because the shell
/// files one thing under it: a console's missing BIOS belongs on the settings
/// page of the emulator that wants it, and those pages are keyed by this name.
#[derive(Debug, Clone, Deserialize)]
struct Core {
    path: String,
    name: String,
}

/// One line of a collection's pictures being fetched.
///
/// Two per game — one as it is asked about and one when it has been answered —
/// which is what lets covers arrive on the bar one at a time instead of all at
/// once when a hundred games have finished coming down.
#[derive(Debug, Clone, Deserialize)]
struct Artwork {
    protocol: u32,
    stage: Picturing,
    /// The game this line is about, by its own path, which is what the shell
    /// holds its row under. Empty on the lines that begin and end the run.
    rom: String,
    at: u32,
    of: u32,
    note: String,
    boxart: Option<String>,
    snap: Option<String>,
}

impl Record for Artwork {
    fn protocol(&self) -> u32 {
        self.protocol
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Picturing {
    Looking,
    Fetching,
    Done,
    Failed,
}

#[derive(Debug, Clone, Deserialize)]
struct Rom {
    title: String,
    path: String,
    /// Where the game's cover and a screenshot of it are on this disk, when the
    /// helper has fetched them — see its `art` module. Defaulted for a helper
    /// too old to say, which is the answer it would give.
    #[serde(default)]
    boxart: Option<String>,
    #[serde(default)]
    snap: Option<String>,
}

// --- the worker -------------------------------------------------------------

/// What the shell asks the helper for.
enum Ask {
    Probe,
    Install,
    /// Take it off again, with everything it kept — see the helper's
    /// `install::remove`. Sent only when somebody has said yes to a panel that
    /// spells out what goes.
    Uninstall,
    /// Let a sandboxed RetroArch read the folder the user has just chosen. Sent
    /// once, when they choose it — see [`RetroArch::permit`].
    Permit(PathBuf),
    Scan(PathBuf),
    /// Fetch a core for each console named — one item per console, holding that
    /// console's cores best first and separated by commas. See
    /// [`RetroArch::fetch`].
    Cores(Vec<String>),
    /// Ask every installed core what it can be set to. Goes stale when the
    /// probe finds RetroArch and again whenever a core is fetched, because a
    /// core that was not here a moment ago has settings nobody has asked about;
    /// sent when somebody reaches Settings — see
    /// [`RetroArch::settings_reached`].
    Options,
    /// Fetch the covers and screenshots of somebody's games from libretro's
    /// thumbnail collection.
    ///
    /// `only` narrows it to particular games by path — what the menu row over
    /// one game asks for — and `again` throws away what is known about which
    /// names libretro has, which is what the row under Settings asks for.
    Art {
        roms: PathBuf,
        only: Vec<String>,
        again: bool,
    },
}

impl Ask {
    /// Whether a question that came back with nothing at all means the
    /// integration is not working.
    ///
    /// True of every question but one. A probe that says nothing, a scan that
    /// says nothing, an install that says nothing — each is a helper that could
    /// not be run or could not be understood, and the row has to say so rather
    /// than sit there claiming a machine has no games on it.
    ///
    /// [`Ask::Options`] is the exception, and it is an exception because
    /// silence is one of its *answers*: a RetroArch with no cores installed
    /// yet has nothing to say about what any core can be set to, and so does
    /// one whose cores all refuse to load. Neither is a broken integration —
    /// the first is what every fresh install looks like — and reading them as
    /// one put "Not working on this machine" under a row that was working
    /// perfectly well.
    fn must_answer(&self) -> bool {
        !matches!(self, Ask::Options)
    }
}

/// What comes back.
enum Heard {
    Probed(Probe),
    Installing(Progress),
    Permitted(Permission),
    Scanned(Library),
    Fetching(Fetch),
    /// One core's settings, as that core declares them.
    Tunable(CoreOptions),
    /// One game's pictures, or a line about the run they are part of.
    Pictured(Artwork),
    /// The helper could not be run, or said something this shell cannot read.
    /// Which is not the same as it saying "no": a broken helper leaves the row
    /// saying so rather than offering an install that will not happen.
    Broken(String),
}

/// Run one question on the worker, and send back every line of the answer.
fn answer(helper: &Path, ask: &Ask, back: &Sender<Heard>) {
    let mut command = Command::new(helper);
    match ask {
        Ask::Probe => command.arg("probe"),
        Ask::Install => command.arg("install"),
        Ask::Uninstall => command.arg("uninstall"),
        Ask::Permit(at) => command.arg("permit").arg(at),
        Ask::Scan(at) => command.arg("scan").arg(at),
        Ask::Cores(wanted) => command.arg("cores").args(wanted),
        Ask::Options => command.arg("options"),
        Ask::Art { roms, only, again } => {
            command.arg("art").arg(roms);
            for path in only {
                command.arg("--only").arg(path);
            }
            if *again {
                command.arg("--again");
            }
            &mut command
        }
    };
    // The helper's own stderr is this session's log, which is where a flatpak
    // that refused to install says why.
    let mut child = match command.stdin(Stdio::null()).stdout(Stdio::piped()).spawn() {
        Ok(child) => child,
        Err(err) => {
            let _ = back.send(Heard::Broken(
                crate::message!("retroarch-helper-could-not-run", "helper" => HELPER, "error" => err.to_string()),
            ));
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
                Ask::Install | Ask::Uninstall => serde_json::from_str(line).map(Heard::Installing),
                Ask::Permit(_) => serde_json::from_str(line).map(Heard::Permitted),
                Ask::Scan(_) => serde_json::from_str(line).map(Heard::Scanned),
                Ask::Cores(_) => serde_json::from_str(line).map(Heard::Fetching),
                Ask::Options => serde_json::from_str(line).map(Heard::Tunable),
                Ask::Art { .. } => serde_json::from_str(line).map(Heard::Pictured),
            };
            match heard {
                Ok(heard) => {
                    said = true;
                    if back.send(heard).is_err() {
                        // The shell has gone. Let the helper finish rather than
                        // killing it: an install half done is worse than one
                        // nobody is watching.
                        break;
                    }
                }
                Err(err) => tracing::warn!(%err, line, "the helper said something unreadable"),
            }
        }
    }
    let status = child.wait();
    if !said && ask.must_answer() {
        let why = match status {
            Ok(status) => {
                crate::message!("retroarch-helper-answered-nothing", "helper" => HELPER, "status" => status.to_string())
            }
            Err(err) => {
                crate::message!("retroarch-helper-could-not-be-waited-for", "helper" => HELPER, "error" => err.to_string())
            }
        };
        let _ = back.send(Heard::Broken(why));
    }
}

// --- the shell's own state --------------------------------------------------

/// RetroArch as one field of the shell.
pub struct RetroArch {
    /// `None` on a machine without the package. Everything below it is then
    /// never read, and every method answers as though this file did not exist.
    inner: Option<Inner>,
}

struct Inner {
    ask: Sender<Ask>,
    heard: Receiver<Heard>,
    /// Whether there is a RetroArch, and what starts it.
    found: Found,
    /// The install on screen, if one is happening.
    installing: Option<Installing>,
    /// The cores coming down, if any are.
    fetching: Option<Fetching>,
    /// The pictures coming down, if any are.
    picturing: Option<Pictures>,
    /// The one game a fetch of pictures is about, by the name to say it under,
    /// and only while nothing has been found for it.
    ///
    /// What it is for is the press that finds nothing. A run over a whole
    /// collection needs no answer — the covers appearing are the answer, and a
    /// panel counting the games libretro has never heard of would be a panel
    /// about somebody's homebrew. A press on *one* game is a different thing:
    /// somebody asked a question about the row they are standing on, and a row
    /// that quietly stays as it was is a button nobody trusts twice.
    ///
    /// Cleared the moment a picture arrives for it, so what is left at the end
    /// of the run is exactly the case worth saying out loud.
    picturing_about: Option<String>,
    /// The folder whose pictures have already been asked about this session.
    ///
    /// What stops the shell asking again on every scan, which is every start-up
    /// and every folder change: a run that found nothing for a game found
    /// nothing because libretro has nothing, and asking a second time in the
    /// same session would be the same answer down the same wire. A new session
    /// asks once more, which is how a game that had no cover last month gets
    /// the one somebody has drawn since.
    pictured: Option<PathBuf>,
    /// Whether this machine's helper can fetch one at all — the probe's answer,
    /// and what decides whether a core is ever offered. See [`Probe`].
    fetches_cores: bool,
    /// RetroArch's own configuration directory, from the same answer.
    config: Option<PathBuf>,
    /// The last scan, and the folder it was of. The folder is carried because
    /// the setting can change while a scan of the old one is in flight, and a
    /// column built from the answer to a question nobody is asking any more
    /// would be somebody else's games.
    scanned: Option<PathBuf>,
    consoles: Vec<Console>,
    /// What the filesystem said about a folder that could not be read.
    unreadable: Option<String>,
    /// Where RetroArch reads a core's own files, as the last scan reported it.
    ///
    /// Nothing on the bar draws this yet. It is kept because the record carries
    /// it and the shell is the half that would have to name a folder if it ever
    /// had to — see the helper's `firmware.rs`, and the reason this integration
    /// chooses a different core instead of naming one.
    system: Option<PathBuf>,
    /// Whether a scan is in flight, so the row can say it is looking.
    reading: bool,
    /// The folder the last scan was *asked* about, which is not the same as
    /// the one it answered about — see [`Inner::scanned`]. What stops a helper
    /// that cannot answer from being asked again on every frame: the question
    /// is asked once per folder, and a folder is only asked about again when
    /// the user changes it or presses the row.
    asked: Option<PathBuf>,
    /// Whether the cores need asking what they can be set to.
    ///
    /// Set when a RetroArch is found and again whenever a core is fetched; the
    /// question itself is not sent until somebody reaches Settings — see
    /// [`RetroArch::settings_reached`], which is where the argument for that
    /// is. False both before there is anything to ask and after the answer has
    /// been asked for, so passing Settings a second time asks nothing.
    wants_options: bool,
    /// The helper could not be run, and what it said.
    broken: Option<String>,
}

/// Whether there is a RetroArch on this machine.
enum Found {
    /// The probe has not answered yet, which is the state every session starts
    /// in.
    Asking,
    Absent {
        /// Whether one could be installed, which is whether this machine has
        /// flatpak. A machine without it is told so rather than being offered
        /// a button that cannot work.
        flatpak: bool,
    },
    Here(Installation),
}

/// An install as the shell is drawing it.
pub struct Installing {
    pub progress: Option<f32>,
    pub note: String,
    /// Which way this one is going.
    ///
    /// One field rather than a second piece of state beside it, because the
    /// two are the same thing to everything that watches: a panel with a
    /// sentence and no buttons, and one line at the end saying how it went.
    /// What turns on it is the words.
    pub removing: bool,
    /// Set on the line that ends it, and taken by the shell on the next frame:
    /// finishing is an event, and the panel that was up has to be answered
    /// exactly once. See [`RetroArch::poll`].
    ended: Option<bool>,
}

/// Cores coming down, as the shell is drawing them.
pub struct Fetching {
    /// The one being fetched now.
    pub core: String,
    pub progress: Option<f32>,
    /// Which of them this is, counting from one, and how many there are. A
    /// panel says "2 of 5" from these, and says nothing where there is one.
    pub at: u32,
    pub of: u32,
    pub note: String,
    /// Set on the line that ends it, and taken by the shell on the next frame
    /// — the same one-shot [`Installing`] uses, and for the same reason.
    ended: Option<bool>,
}

impl Fetching {
    /// The one sentence a fetch says, wherever it is said.
    ///
    /// The row under Games and the panel over it both draw from this, so that
    /// the two cannot disagree about what is happening — the panel is what
    /// somebody is looking at, and the row is what is left when they dismiss
    /// it.
    ///
    /// A percentage where there is one thing to get, and a count where there
    /// are several: what a person setting a folder up wants to know is how much
    /// of this is left, and what somebody waiting on one game wants is how far
    /// down it is.
    ///
    /// It does not name the core. A core is the part of an emulator that is one
    /// particular console, which is a true and useless thing to tell somebody
    /// waiting to play a game — what they are owed is that something is being
    /// fetched and how much of it is left.
    pub fn sentence(&self) -> String {
        if self.core.is_empty() {
            return self.note.clone();
        }
        let getting = crate::i18n::text("shell-getting-your-games-ready");
        match (self.of, self.progress) {
            (of, _) if of > 1 => {
                crate::message!("retroarch-getting-progress", "getting" => getting, "at" => self.at, "of" => of)
            }
            (_, Some(done)) => format!("{getting} — {}%", (done * 100.0).round() as u32),
            _ => getting.to_string(),
        }
    }
}

/// Pictures coming down, as the shell is drawing them.
///
/// No panel stands over this and that is deliberate: what it changes is the
/// picture on a row somebody is already looking at, so the change *is* the
/// feedback — covers appearing one at a time down a column somebody is
/// scrolling. What a panel would add is a thing to dismiss. The row at the head
/// of the column says what is happening for anybody who wants to know; see
/// [`RetroArch::library_note`].
pub struct Pictures {
    /// Which game this is, counting from one, and how many there are.
    pub at: u32,
    pub of: u32,
}

impl Pictures {
    /// The one sentence a fetch of pictures says.
    ///
    /// It does not name the game. What is being fetched is artwork for a whole
    /// collection and the row saying a different title sixty times a minute
    /// would be a row nobody can read — what somebody is owed is that the
    /// pictures are coming and how far along they are.
    pub fn sentence(&self) -> String {
        let getting = crate::i18n::text("shell-getting-the-pictures");
        match self.of {
            0 => getting.to_string(),
            of => {
                crate::message!("retroarch-getting-progress", "getting" => getting, "at" => self.at.max(1).min(of), "of" => of)
            }
        }
    }
}

/// What one turn of the loop changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Change {
    /// The row's line, the column's rows, or both. The two are rebuilt
    /// together because they are rebuilt from the same state, and separating
    /// them would be two ways for one fact to be on screen.
    pub rows: bool,
    /// An install has just ended, and whether RetroArch is now here.
    pub installed: Option<bool>,
    /// A fetch of cores has just ended, and whether every one of them arrived.
    pub fetched: Option<bool>,
    /// A folder has just been read. Not the same as `rows`, which is set by
    /// every other answer as well: what waits on this is the work that can only
    /// be done once the games are known — fetching the cores they need, and
    /// starting the game somebody pressed before there was one.
    pub scanned: bool,
    /// The panel over an install or a fetch wants redrawing.
    pub panel: bool,
    /// A fetch of pictures that was about *one* game has ended with nothing
    /// found for it, and what that game is called.
    ///
    /// Only ever set for the press over a single row — see
    /// [`Inner::picturing_about`], which is where the reason a whole
    /// collection finding nothing says nothing is written down.
    pub unpictured: Option<String>,
    /// The covers that landed in this poll: the ROM's path, and where its box
    /// art now is.
    ///
    /// Carried out of the poll rather than left for somebody to read back off
    /// this module, because the Trophies column keeps a copy of this folder of
    /// its own and a cover is the one thing in it the column cannot work out
    /// for itself. Asking the achievement helper for it again is a network
    /// round trip per console; this is the string, already in hand. See
    /// [`crate::retroachievements::RetroAchievements::pictured`].
    ///
    /// Only box art, and only where one was found: a run that turned up a
    /// screenshot and no cover has changed nothing a row wears, and `None`
    /// pushed here would read as *the cover is gone* to anybody holding one.
    pub pictured: Vec<(String, String)>,
}

/// What pressing the RetroArch row means, asked at the moment it is pressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Press {
    /// Nothing yet: the helper has not answered. The press asks again rather
    /// than doing nothing visible, which is what the Steam row does with a
    /// library that has not arrived.
    Waiting,
    /// Offer to install it.
    Install,
    /// It cannot be installed here, and why.
    Cannot(String),
    /// Ask where the games are.
    Folder,
    /// Step across to the column.
    Enter,
}

impl RetroArch {
    /// A session without the integration.
    pub fn absent() -> RetroArch {
        RetroArch { inner: None }
    }

    /// Start the worker, and ask it what is on this machine.
    ///
    /// `helper` is where the program is. Nothing is asked on this thread: the
    /// probe runs `flatpak info` twice and walks `PATH`, and the first frame is
    /// several hundred milliseconds of GPU setup away — the same reason the
    /// Steam session and the media walk are started here.
    pub fn start(helper: PathBuf) -> RetroArch {
        let (ask, asked) = std::sync::mpsc::channel::<Ask>();
        let (back, heard) = std::sync::mpsc::channel::<Heard>();
        let started = std::thread::Builder::new()
            .name("lxb-retroarch".to_string())
            .spawn(move || {
                for question in asked {
                    answer(&helper, &question, &back);
                }
            });
        if let Err(err) = started {
            tracing::error!(?err, "no thread for the RetroArch integration");
            return RetroArch::absent();
        }

        let inner = Inner {
            ask,
            heard,
            found: Found::Asking,
            installing: None,
            fetching: None,
            picturing: None,
            picturing_about: None,
            pictured: None,
            fetches_cores: false,
            config: None,
            scanned: None,
            consoles: Vec::new(),
            unreadable: None,
            system: None,
            reading: false,
            asked: None,
            wants_options: false,
            broken: None,
        };
        let mut retroarch = RetroArch { inner: Some(inner) };
        retroarch.send(Ask::Probe);
        retroarch
    }

    fn send(&mut self, ask: Ask) {
        let Some(inner) = self.inner.as_mut() else {
            return;
        };
        if inner.ask.send(ask).is_err() {
            inner.broken = Some(
                crate::i18n::text("label-the-retroarch-integration-stopped-answering").to_string(),
            );
        }
    }

    /// Read the folder again, if there is one to read.
    ///
    /// Called when the setting changes and when an install finishes, and never
    /// on a timer: a ROM folder is not something that changes under a session
    /// the way a Steam library does, and a walk over a collection is a walk
    /// over a collection.
    pub fn rescan(&mut self) {
        let Some(at) = crate::settings::roms_folder() else {
            if let Some(inner) = self.inner.as_mut() {
                inner.consoles.clear();
                inner.scanned = None;
                inner.asked = None;
                inner.unreadable = None;
            }
            return;
        };
        if let Some(inner) = self.inner.as_mut() {
            inner.reading = true;
            inner.asked = Some(at.clone());
        }
        self.send(Ask::Scan(at));
    }

    /// Ask again what is on this machine.
    pub fn reprobe(&mut self) {
        self.send(Ask::Probe);
    }

    /// Somebody has arrived in Settings, so ask the cores what they can be set
    /// to — if nobody has since the last time the answer went stale.
    ///
    /// Asked here rather than at startup because this is the only screen the
    /// answer is for, and because asking is not free: every installed core is
    /// loaded into a process to be asked — see the helper's `options.rs` — and
    /// a machine with a dozen of them maps a dozen emulators at every login for
    /// a page most people open twice.
    ///
    /// Reaching Settings is a long way from a core's own page — down the
    /// column to Games, in, down to RetroArch, in, and down again — so the
    /// answer has several presses and their animations to arrive in, and it
    /// arrives the way a fetched core's answer already did: the column is built
    /// again and the rows are simply there.
    /// Answers whether it actually asked, which is the one caller that is not
    /// somebody arriving: the walk this shell makes for a game that will not
    /// start goes to a row on a core's own page, and that page does not exist
    /// until this has been answered. It waits for the answer where there is one
    /// coming and says so where there is not, rather than waiting for ever. See
    /// `Shell::open_bios_picker`.
    pub fn settings_reached(&mut self) -> bool {
        let wanted = self.inner.as_mut().is_some_and(|inner| {
            let wanted = inner.wants_options && matches!(inner.found, Found::Here(_));
            // Cleared whether or not it is sent below, because what stops it
            // being sent is there being no RetroArch to send it about.
            inner.wants_options = false;
            wanted
        });
        if wanted {
            self.send(Ask::Options);
        }
        wanted
    }

    /// Let the emulator read the folder that has just been chosen.
    ///
    /// Sent on the press that chooses it and at no other time: it changes one
    /// application's permissions, which is a thing to do once and on purpose.
    /// A flatpak sees this user's home directory and nothing else, so a
    /// collection on an external drive is otherwise one RetroArch starts and
    /// cannot open — and it says so in its own window, where nobody on a
    /// console is looking. Nothing at all happens for a distribution package,
    /// which is in no sandbox.
    pub fn permit(&mut self, at: &Path) {
        self.send(Ask::Permit(at.to_path_buf()));
    }

    /// Install it, and start saying so.
    pub fn install(&mut self) {
        if let Some(inner) = self.inner.as_mut() {
            inner.installing = Some(Installing {
                progress: None,
                note: crate::i18n::text("shell-installing-retroarch").to_string(),
                removing: false,
                ended: None,
            });
        }
        self.send(Ask::Install);
    }

    /// Take it off again, and start saying so.
    ///
    /// Sent only when somebody has said yes to a panel naming what goes with
    /// it. What that is, and what deliberately does not, is in the helper's
    /// `install::remove`.
    pub fn remove(&mut self) {
        if let Some(inner) = self.inner.as_mut() {
            inner.installing = Some(Installing {
                progress: None,
                note: crate::i18n::text("shell-removing-retroarch").to_string(),
                removing: true,
                ended: None,
            });
        }
        self.send(Ask::Uninstall);
    }

    /// Forget everything read off a RetroArch that is no longer there.
    ///
    /// The collection, the cores' settings, the firmware, the console marks and
    /// the folder that was scanned. Not a tidy-up: every one of these is
    /// something the bar and the Settings tree are built from, and a shell that
    /// left them standing would go on drawing a shelf of games nothing can play
    /// and a settings page for emulators that are not installed.
    pub fn forget(&mut self) {
        if let Some(inner) = self.inner.as_mut() {
            inner.consoles = Vec::new();
            inner.scanned = None;
            inner.asked = None;
            inner.pictured = None;
            inner.unreadable = None;
            inner.system = None;
            inner.config = None;
            inner.wants_options = false;
        }
        TUNABLES
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .clear();
        FIRMWARE
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .clear();
        CORE_MARKS
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .clear();
    }

    /// Fetch a core for each console in `wanted`.
    ///
    /// Each item is one console's cores, best first and comma-separated —
    /// [`RetroArch::missing_cores`] builds them, and the helper picks the first
    /// of each that libretro's build server actually publishes. Sent when
    /// somebody has said yes to it, and at no other time: it is a download from
    /// a third party, which is a thing to do on purpose.
    pub fn fetch(&mut self, wanted: Vec<String>) {
        if wanted.is_empty() {
            return;
        }
        let of = wanted.len() as u32;
        if let Some(inner) = self.inner.as_mut() {
            inner.fetching = Some(Fetching {
                core: String::new(),
                progress: None,
                at: 0,
                of,
                note: crate::i18n::text("shell-getting-ready").to_string(),
                ended: None,
            });
        }
        self.send(Ask::Cores(wanted));
    }

    /// Fetch the pictures for whatever in this folder has not got them.
    ///
    /// Once per folder per session, and only where something is actually
    /// missing: a collection whose covers are all on the disk asks nothing, and
    /// a game libretro has no picture of found nothing a moment ago and would
    /// find nothing again. A later session asks once more, which is how a game
    /// that had no cover last month gets the one somebody has drawn since.
    ///
    /// Answers whether it asked, which is what lets the caller say so.
    pub fn want_pictures(&mut self) -> bool {
        let Some(folder) = crate::settings::roms_folder() else {
            return false;
        };
        let ask = self.inner.as_ref().is_some_and(|inner| {
            matches!(inner.found, Found::Here(_))
                && inner.picturing.is_none()
                && inner.pictured.as_deref() != Some(folder.as_path())
                && inner
                    .consoles
                    .iter()
                    .flat_map(|console| &console.roms)
                    .any(|rom| rom.boxart.is_none() || rom.snap.is_none())
        });
        if !ask {
            return false;
        }
        if let Some(inner) = self.inner.as_mut() {
            inner.pictured = Some(folder.clone());
        }
        self.send(Ask::Art {
            roms: folder,
            only: Vec::new(),
            again: false,
        });
        true
    }

    /// Fetch pictures because somebody asked for them.
    ///
    /// `only` narrows it to particular games by path — what the menu row over
    /// one game asks for — and an empty list is the whole collection. `again`
    /// asks libretro which games exist afresh instead of believing the listing
    /// on this disk, which is what the row under Settings is for: a game that
    /// had no cover when the folder was set up may have one now, and the only
    /// thing between somebody and it is a listing a fortnight old.
    ///
    /// Unlike [`RetroArch::want_pictures`] this asks whatever it is told to,
    /// every time. It is a press: something has to happen.
    pub fn fetch_pictures(&mut self, only: Vec<String>, named: Option<String>, again: bool) {
        let Some(folder) = crate::settings::roms_folder() else {
            return;
        };
        tracing::info!(games = only.len(), again, "asking for the pictures");
        if let Some(inner) = self.inner.as_mut() {
            inner.pictured = Some(folder.clone());
            // Only where the run is about one game, which is the only case
            // whose failure is worth a panel.
            inner.picturing_about = named.filter(|_| only.len() == 1);
            // So the row says something on the very next frame rather than
            // when the helper's first line arrives, which is a whole listing
            // away.
            inner.picturing = Some(Pictures { at: 0, of: 0 });
        }
        self.send(Ask::Art {
            roms: folder,
            only,
            again,
        });
    }

    /// The pictures being fetched, if any are.
    pub fn picturing(&self) -> Option<&Pictures> {
        self.inner.as_ref()?.picturing.as_ref()
    }

    /// Where RetroArch keeps its own configuration, once the probe has said.
    pub fn config_dir(&self) -> Option<&Path> {
        self.inner.as_ref()?.config.as_deref()
    }

    /// The cores coming down, if any are.
    pub fn fetching(&self) -> Option<&Fetching> {
        self.inner.as_ref()?.fetching.as_ref()
    }

    /// The fetch is over and has been answered; put the panel's state away.
    pub fn fetched(&mut self) {
        if let Some(inner) = self.inner.as_mut() {
            inner.fetching = None;
        }
    }

    /// Whether a core can be fetched on this machine at all.
    ///
    /// False until the probe has answered, and false for ever on a machine
    /// whose helper is older than the verb. Everything that offers a download
    /// asks this first, because the alternative is a Yes that comes back as an
    /// error.
    pub fn offers_cores(&self) -> bool {
        self.inner
            .as_ref()
            .is_some_and(|inner| inner.fetches_cores && matches!(inner.found, Found::Here(_)))
    }

    /// The cores this collection needs and has not got: one item per console,
    /// each holding that console's cores best first.
    ///
    /// Empty for a folder where everything can already be played, which is the
    /// ordinary state of a machine that has been set up once — so the setup
    /// fetches what is missing and a later session asks for nothing.
    ///
    /// A console whose core is *there* but half installed is in this list too.
    /// Some emulators read a folder of files beside themselves and draw a game
    /// with no text at all without it, and a core in that state is not
    /// something anybody can play; the helper sees the core is already on the
    /// disk and fetches only the part that is missing.
    pub fn missing_cores(&self) -> Vec<String> {
        let Some(inner) = self.inner.as_ref() else {
            return Vec::new();
        };
        inner
            .consoles
            .iter()
            .filter(|console| {
                (console.core.is_none() || console.incomplete) && !console.wanted.is_empty()
            })
            .map(|console| console.wanted.join(","))
            .collect()
    }

    /// The install on screen, if one is happening.
    pub fn installing(&self) -> Option<&Installing> {
        self.inner.as_ref()?.installing.as_ref()
    }

    /// Take everything the worker has said since the last frame.
    pub fn poll(&mut self) -> Change {
        let mut change = Change::default();
        let Some(inner) = self.inner.as_mut() else {
            return change;
        };

        let mut ended = None;
        let mut got = None;
        let mut rescan = false;
        // Asked after the loop rather than inside it: `inner` is borrowed here
        // and sending is a method on the whole of `self`.
        let mut ask_options = false;
        while let Ok(heard) = inner.heard.try_recv() {
            match heard {
                Heard::Probed(probe) if !probe.usable() => {
                    inner.broken = Some(crate::message!(
                        "retroarch-helper-version-mismatch",
                        "theirs" => probe.protocol,
                        "ours" => PROTOCOL
                    ));
                    change.rows = true;
                }
                Heard::Probed(probe) => {
                    inner.broken = None;
                    inner.fetches_cores = probe.fetches_cores;
                    inner.config = probe.config.map(PathBuf::from);
                    inner.found = match probe.retroarch {
                        Some(installation) => {
                            tracing::info!(
                                version = installation.version.as_deref().unwrap_or("unknown"),
                                command = ?installation.command,
                                "RetroArch"
                            );
                            Found::Here(installation)
                        }
                        None => Found::Absent {
                            flatpak: probe.flatpak,
                        },
                    };
                    // Where RetroArch keeps its configuration, put where the
                    // Settings tree can read it: that tree is built by free
                    // functions taking nothing, and this is a fact about the
                    // machine rather than about any one screen.
                    *CONFIG_AT.lock().unwrap_or_else(|err| err.into_inner()) = inner.config.clone();
                    // And what every core on it can be set to, which is a
                    // question only the cores can answer — wanted from here,
                    // and asked when somebody goes looking for the answer.
                    inner.wants_options |= matches!(inner.found, Found::Here(_));
                    // What is on the disk decides what the games are startable
                    // with, so a probe that arrives after a scan has to rebuild
                    // the rows the scan built.
                    change.rows = true;
                }
                Heard::Tunable(core) => {
                    let mut held = TUNABLES.lock().unwrap_or_else(|err| err.into_inner());
                    held.retain(|had| had.core != core.core);
                    tracing::info!(
                        core = core.core,
                        settings = core.options.len(),
                        "a core said what it can be set to"
                    );
                    held.push(core);
                    // The Settings tree is built from this, and a page that
                    // was empty a moment ago now has rows on it.
                    change.rows = true;
                }
                Heard::Installing(line) if !line.usable() => {}
                Heard::Installing(line) => {
                    let stage = line.stage;
                    let installing = inner.installing.get_or_insert_with(|| Installing {
                        progress: None,
                        note: String::new(),
                        // The helper says which way it is going on its own
                        // first line, so a panel that arrives before the shell
                        // has set one up still knows.
                        removing: stage == Stage::Removing,
                        ended: None,
                    });
                    installing.note = line.note.clone();
                    installing.progress = line.progress.or(installing.progress);
                    installing.removing |= stage == Stage::Removing;
                    change.panel = true;
                    change.rows = true;
                    match stage {
                        Stage::Done => ended = Some(true),
                        Stage::Failed => {
                            let doing = if installing.removing {
                                "removed"
                            } else {
                                "installed"
                            };
                            tracing::warn!(why = %line.note, "RetroArch was not {doing}");
                            ended = Some(false);
                        }
                        Stage::Remote | Stage::Installing | Stage::Removing => {}
                    }
                }
                Heard::Permitted(permission) if !permission.usable() => {}
                Heard::Permitted(permission) => {
                    // Nothing on screen either way. What it decides is whether
                    // a game outside this user's home directory can be opened
                    // at all, and that is a question RetroArch answers in its
                    // own window a second after the press — the log is where
                    // somebody goes looking when it does.
                    if permission.permitted {
                        tracing::info!(note = %permission.note, "RetroArch may read the games");
                    } else {
                        tracing::warn!(
                            note = %permission.note,
                            "RetroArch may not be able to read the games"
                        );
                    }
                }
                Heard::Scanned(library) if !library.usable() => {}
                Heard::Fetching(line) if !line.usable() => {}
                Heard::Fetching(line) => {
                    let fetching = inner.fetching.get_or_insert_with(|| Fetching {
                        core: String::new(),
                        progress: None,
                        at: 0,
                        of: line.of,
                        note: String::new(),
                        ended: None,
                    });
                    fetching.core = line.core.clone();
                    fetching.progress = line.progress;
                    fetching.at = line.at;
                    fetching.of = line.of;
                    fetching.note = line.note.clone();
                    change.panel = true;
                    change.rows = true;
                    match line.stage {
                        Getting::Done => got = Some(true),
                        Getting::Failed => {
                            tracing::warn!(why = %line.note, "a core was not fetched");
                            got = Some(false);
                        }
                        Getting::Looking | Getting::Downloading => {}
                    }
                }
                Heard::Scanned(library) => {
                    inner.reading = false;
                    // The answer to a question nobody is asking any more: the
                    // folder was changed while this scan was in flight.
                    let asked = crate::settings::roms_folder();
                    if asked.as_deref() != Some(Path::new(&library.roms)) {
                        tracing::debug!(
                            was = %library.roms,
                            "a scan of a folder this shell is no longer set to"
                        );
                        continue;
                    }
                    inner.scanned = asked;
                    if let Some(why) = &library.unreadable {
                        tracing::warn!(%why, "the games folder could not be read");
                    }
                    inner.unreadable = library.unreadable;
                    inner.system = library.system.map(PathBuf::from);
                    inner.consoles = library.consoles;
                    // And what shape each console's shelf is drawn at, off the
                    // covers already in this shell's cache. Here rather than
                    // where the rows are built, so that a scan reads a handful
                    // of picture headers once instead of every rebuild reading
                    // them again.
                    measure_shelves(&mut inner.consoles);
                    // What a game cannot be started without, put where the
                    // Settings tree and the press that offers to fix it can
                    // both read it. Only the scan knows both halves: the
                    // console's own name, and RetroArch's system folder on this
                    // machine.
                    let wanted: Vec<Firmware> = match &inner.system {
                        Some(system) => inner
                            .consoles
                            .iter()
                            .flat_map(|console| {
                                // The core that wants it, which is what files
                                // the question: a console with none installed
                                // has no settings page to put the row on.
                                let core = console.core.as_ref().map(|core| core.name.clone());
                                console.needs.iter().filter_map(move |need| {
                                    Some(Firmware {
                                        console: console.title.clone(),
                                        core: core.clone()?,
                                        note: need.note.clone(),
                                        into: system.join(&need.path),
                                        here: need.here,
                                    })
                                })
                            })
                            .collect(),
                        None => Vec::new(),
                    };
                    for missing in wanted.iter().filter(|had| !had.here) {
                        tracing::info!(
                            console = %missing.console,
                            core = %missing.core,
                            note = %missing.note,
                            into = %missing.into.display(),
                            "a console is missing its firmware"
                        );
                    }
                    *FIRMWARE.lock().unwrap_or_else(|err| err.into_inner()) = wanted;
                    // And which console's mark each of those cores plays under,
                    // so that an emulator's settings page wears the drawing its
                    // shelf does. The same pass over the same consoles: only
                    // the scan holds both the core's name and the console's
                    // mark, and reading them apart is how the two lists start
                    // disagreeing.
                    *CORE_MARKS.lock().unwrap_or_else(|err| err.into_inner()) = inner
                        .consoles
                        .iter()
                        .filter_map(|console| {
                            Some((console.core.as_ref()?.name.clone(), console.glyph.clone()?))
                        })
                        .collect();
                    change.rows = true;
                    change.scanned = true;
                }
                Heard::Pictured(line) if !line.usable() => {}
                Heard::Pictured(line) => {
                    // Onto the row it is about, as it arrives. That is what
                    // makes a collection's covers appear one at a time down a
                    // column somebody is already scrolling, rather than all at
                    // once when the last of a hundred has come down.
                    if !line.rom.is_empty() && (line.boxart.is_some() || line.snap.is_some()) {
                        // Something was found, so there is nothing to say at
                        // the end of the run.
                        inner.picturing_about = None;
                        if let Some(boxart) = &line.boxart {
                            change.pictured.push((line.rom.clone(), boxart.clone()));
                        }
                        for console in &mut inner.consoles {
                            let mut landed = false;
                            for rom in &mut console.roms {
                                if rom.path == line.rom {
                                    rom.boxart = line.boxart.clone();
                                    rom.snap = line.snap.clone();
                                    landed = true;
                                }
                            }
                            // A console that has just got its first cover has
                            // just found out what shape its shelf is. Only the
                            // one the picture landed on: the others have not
                            // changed, and measuring all of them once per
                            // picture would be a hundred consoles' headers read
                            // for every cover of one.
                            if landed {
                                measure_shelves(std::slice::from_mut(console));
                            }
                        }
                    }
                    match line.stage {
                        Picturing::Done | Picturing::Failed => {
                            if line.stage == Picturing::Failed {
                                tracing::warn!(why = %line.note, "the pictures did not arrive");
                            } else {
                                tracing::info!(note = %line.note, "the pictures");
                            }
                            inner.picturing = None;
                            change.unpictured = inner.picturing_about.take();
                        }
                        Picturing::Looking | Picturing::Fetching => {
                            inner.picturing = Some(Pictures {
                                at: line.at,
                                of: line.of,
                            });
                        }
                    }
                    change.rows = true;
                }
                Heard::Broken(why) => {
                    tracing::warn!(%why, "the RetroArch integration");
                    inner.reading = false;
                    inner.broken = Some(why);
                    // An install that died without saying so is still an
                    // install that ended.
                    if inner.installing.is_some() {
                        ended = Some(false);
                    }
                    change.rows = true;
                }
            }
        }

        if let Some(worked) = got {
            if let Some(fetching) = inner.fetching.as_mut() {
                fetching.ended = Some(worked);
            }
            change.fetched = Some(worked);
            // Whatever it says it fetched, ask the disk: what a console can be
            // played with is a file being there, and the scan is what looks.
            inner.asked = None;
            // And the new core has to be asked what it can be set to, which is
            // the other half of it arriving. Wanted here as well as after the
            // probe because a core installed during a session was not on the
            // disk when that probe answered: without this its page under
            // Settings does not exist until the shell is started again, which
            // is a thing nobody would think to try.
            ask_options |= worked;
        }
        if let Some(worked) = ended {
            if let Some(installing) = inner.installing.as_mut() {
                installing.ended = Some(worked);
            }
            change.installed = Some(worked);
            // Whatever it says it did, ask the machine: what decides whether
            // there is a RetroArch here is `flatpak info`, not a line on a
            // pipe.
            rescan = true;
        }
        if rescan {
            self.reprobe();
        }
        if ask_options {
            if let Some(inner) = self.inner.as_mut() {
                inner.wants_options = true;
            }
        }
        // And the folder, once there is a RetroArch to read it with. Asked here
        // rather than at startup because the two answers arrive in that order:
        // what a game can be started with is a core belonging to an
        // installation, and until the probe has answered there is no
        // installation to ask about. Asked once per folder — see
        // [`Inner::asked`] — so a helper that cannot answer is not asked again
        // sixty times a second.
        let wanted = crate::settings::roms_folder();
        let ask = self.inner.as_ref().is_some_and(|inner| {
            matches!(inner.found, Found::Here(_)) && wanted.is_some() && inner.asked != wanted
        });
        if ask {
            self.rescan();
        }
        change
    }

    /// The install is over and has been answered; put the panel's state away.
    pub fn installed(&mut self) {
        if let Some(inner) = self.inner.as_mut() {
            inner.installing = None;
        }
    }

    /// What pressing the row means now.
    pub fn press(&self) -> Press {
        let Some(inner) = self.inner.as_ref() else {
            return Press::Waiting;
        };
        if let Some(why) = &inner.broken {
            return Press::Cannot(why.clone());
        }
        if inner.installing.is_some() || inner.fetching.is_some() {
            return Press::Waiting;
        }
        match &inner.found {
            Found::Asking => Press::Waiting,
            Found::Absent { flatpak: true } => Press::Install,
            Found::Absent { flatpak: false } => {
                Press::Cannot(crate::i18n::text("retroarch-no-flatpak").to_string())
            }
            Found::Here(_) if crate::settings::roms_folder().is_none() => Press::Folder,
            Found::Here(_) => Press::Enter,
        }
    }

    /// The line under the row, which is the whole of what it carries.
    ///
    /// `None` on a machine without the integration, which is what takes the row
    /// off the bar altogether — see [`crate::apps::offer_retroarch`].
    pub fn note(&self) -> Option<String> {
        let inner = self.inner.as_ref()?;
        if let Some(installing) = &inner.installing {
            return Some(match installing.progress {
                Some(done) => format!("{} — {}%", installing.note, (done * 100.0).round() as u32),
                None => installing.note.clone(),
            });
        }
        if let Some(fetching) = &inner.fetching {
            return Some(fetching.sentence());
        }
        if inner.broken.is_some() {
            return Some(crate::i18n::text("shell-not-working-on-this-machine").to_string());
        }
        Some(match &inner.found {
            Found::Asking => crate::i18n::text("shell-looking-for-retroarch").to_string(),
            Found::Absent { flatpak: true } => {
                crate::i18n::text("shell-press-to-download-it").to_string()
            }
            Found::Absent { flatpak: false } => {
                crate::i18n::text("shell-it-cannot-be-downloaded-on-this-machine").to_string()
            }
            Found::Here(_) => self.library_note(inner),
        })
    }

    /// How far the row's own line has got, as a share of one, for the bar
    /// drawn under it.
    ///
    /// Exactly the two states that put a percentage into [`RetroArch::note`]
    /// and no others: a row saying what is in a folder is not a row counting
    /// up, and a groove under one would be furniture.
    ///
    /// Several games are a count rather than a percentage in the words — see
    /// [`Fetching::sentence`] — and the bar follows the words rather than the
    /// file, because "3 of 12" and a bar a quarter full are the same sentence
    /// twice. The part-finished one is added in, so the bar moves between
    /// games instead of standing still through each of them.
    pub fn arriving(&self) -> Option<f32> {
        let inner = self.inner.as_ref()?;
        if let Some(installing) = &inner.installing {
            return installing.progress;
        }
        let fetching = inner.fetching.as_ref()?;
        if fetching.core.is_empty() {
            return None;
        }
        match fetching.of {
            0 => None,
            1 => fetching.progress,
            of => {
                let whole = f32::from(fetching.at.clamp(1, of) as u16 - 1);
                let part = fetching.progress.unwrap_or(0.0).clamp(0.0, 1.0);
                Some(((whole + part) / of as f32).clamp(0.0, 1.0))
            }
        }
    }

    /// What the row says once RetroArch is here, which is what is in the
    /// folder.
    fn library_note(&self, inner: &Inner) -> String {
        if crate::settings::roms_folder().is_none() {
            return crate::i18n::text("shell-choose-where-your-games-are").to_string();
        }
        if inner.unreadable.is_some() {
            // What the filesystem actually said is in the log — see
            // [`RetroArch::poll`]. A row is read by somebody standing in front
            // of a television, and "Permission denied (os error 13)" is a
            // sentence for whoever is reading the log afterwards.
            return crate::i18n::text("shell-that-folder-cannot-be-read").to_string();
        }
        if inner.reading && inner.consoles.is_empty() {
            return crate::i18n::text("shell-looking-through-your-games").to_string();
        }
        let consoles = inner.consoles.len();
        if consoles == 0 {
            return crate::i18n::text("shell-no-games-in-that-folder-yet").to_string();
        }
        // Ahead of the count, and only while it is happening: a collection
        // whose covers are coming down is a column changing under somebody's
        // eyes, and the row is the one place that says why. No panel stands
        // over it — see [`Pictures`].
        if let Some(picturing) = &inner.picturing {
            return picturing.sentence();
        }
        let games: usize = inner
            .consoles
            .iter()
            .map(|console| console.roms.len())
            .sum();
        crate::message!("games-on-consoles", "games" => games, "consoles" => consoles)
    }

    /// The RetroArch column: the consoles in somebody's folder, and — until
    /// there is one — the row that asks where the folder is.
    ///
    /// Empty for a machine where there is nothing to show — no package, or no
    /// RetroArch — which is what takes the column off the bar. Once RetroArch
    /// is installed the column always has something in it: a folder nobody has
    /// chosen yet is a column with one row in it, and that row is how it stops
    /// being one.
    ///
    /// And nothing else. This column is somebody's games, so the only rows in
    /// it are games and the machines they were written for: RetroArch's own
    /// interface is not one, and the question of where the folder is stops
    /// being one the moment it has been answered. Both were here, and both were
    /// a shell's furniture standing in a room that is not the shell's.
    pub fn rows(&self, picking: Option<(&Path, Piece)>) -> Vec<Entry> {
        let Some(inner) = self.inner.as_ref() else {
            return Vec::new();
        };
        if !matches!(inner.found, Found::Here(_)) {
            return Vec::new();
        }

        let mut rows = Vec::new();
        // For exactly as long as the question is open, which is what makes it
        // a question rather than a setting. A folder nobody has chosen, one
        // that cannot be read this morning, one with nothing in it yet: all
        // three are a column with no consoles in it, and all three are answered
        // by the same press. The moment there are consoles the row goes, and
        // the folder is a setting from then on — Settings > Games > RetroArch >
        // ROMs path, which is one row and always there. See
        // [`crate::settings::games`].
        //
        // A folder still being read counts as one with nothing in it, and that
        // is what keeps this column from ever being empty. An empty column is
        // taken off the bar, and taking it off while somebody is standing in it
        // carries them to the column beside it — which is precisely the moment
        // the answer arrives, since answering starts a scan and a scan takes
        // longer than the press. So for that moment the row that asked is what
        // is still standing there, and it is replaced by the games rather than
        // by nothing.
        if inner.consoles.is_empty() {
            rows.push(folder_row());
        }
        for console in &inner.consoles {
            rows.push(self.console_row(console, picking));
        }
        rows
    }

    /// One console, as a subcategory of games.
    ///
    /// The line under it is the count, and — where the console cannot be played
    /// yet — that something has to be downloaded first. Not *what*: the name of
    /// a libretro core is a fact about this machine's insides, and the person
    /// reading this row wants to know whether they can press it.
    fn console_row(&self, console: &Console, picking: Option<(&Path, Piece)>) -> Entry {
        let games = console.roms.len();
        // An emulator missing the folder it reads beside itself is not a
        // console somebody can play, so it reads as one waiting on a download
        // — which is exactly what it is waiting on.
        let ready = console.core.as_ref().filter(|_| !console.incomplete);
        let comment = match (ready, console.wanted.first()) {
            (Some(_), _) => crate::message!("count-games", "count" => games),
            (None, Some(_)) => crate::message!("games-need-download", "games" => games),
            (None, None) => crate::message!("games-no-emulator", "games" => games),
        };
        Entry::Folder(apps::Folder {
            title_message: None,
            comment_message: None,
            identity: None,
            title: console.title.clone(),
            comment: Some(comment),
            // The machine's own mark, which is the whole reason this column is
            // worth having as a column: nine consoles wearing one drawing is
            // nine rows told apart only by reading them.
            icon: Some(console_mark(console.glyph.as_deref())),
            entries: {
                // Once per shelf rather than once per row: what somebody has
                // chosen is one small list read off the disk, and asking for it
                // per game would be that list walked a hundred times to answer
                // a hundred rows that are nearly all "nothing".
                let chosen = chosen_for(&console.title);
                console
                    .roms
                    .iter()
                    .map(|rom| self.rom_row(console, rom, &chosen, picking))
                    .collect()
            },
            place: None,
            chosen: false,
            over_the_list: false,
            person: None,
            portrait: None,
        })
    }

    /// One game — or, while somebody is choosing a picture for it, the row that
    /// column of their files hangs off.
    ///
    /// The same row in the same place wearing the same name, and that is the
    /// whole of why the picker is opened this way. Choosing a picture for a
    /// game is a walk through somebody's own files, which is a thing this bar
    /// already does: a row with a place on it opens a column of what is there —
    /// see [`crate::files::Place`] — and the row it hangs off stays on screen
    /// to the left of the walk, as **Custom** does under Wallpaper.
    ///
    /// It is built here rather than pushed into the tree by the press, because
    /// the tree is rebuilt from this every time a cover lands: a row inserted
    /// from outside would be swept away by the next line of a collection coming
    /// down, with the user three folders into a walk that was hanging off it.
    fn rom_row(
        &self,
        console: &Console,
        rom: &Rom,
        chosen: &[Chosen],
        picking: Option<(&Path, Piece)>,
    ) -> Entry {
        let command = self.command();
        let controllers = self
            .inner
            .as_ref()
            .and_then(|inner| inner.config.as_deref())
            .map(controllers_file);
        let ready = console.core.as_ref().filter(|_| !console.incomplete);
        let pictures = pictures_for(chosen, rom);
        if let Some((_, piece)) = picking.filter(|(at, _)| *at == Path::new(&rom.path)) {
            return Entry::Folder(apps::Folder {
                title_message: None,
                comment_message: None,
                identity: None,
                title: rom.title.clone(),
                // What the walk is for, under the name of the game it is for,
                // so somebody several folders deep can still read which of the
                // two pictures they came to choose.
                comment: Some(piece.about().to_string()),
                icon: Some(console_mark(console.glyph.as_deref())),
                // Read on the press that opens it rather than now, exactly as
                // every other picker on this bar is: what is on somebody's disk
                // is a question about the moment they ask it.
                entries: Vec::new(),
                place: Some(crate::files::Place::Volumes(crate::files::Shows::Picture(
                    piece,
                ))),
                chosen: false,
                over_the_list: false,
                person: None,
                portrait: None,
            });
        }
        let start = match (ready, command) {
            (Some(core), Some(command)) => {
                let mut argv: Vec<String> = command.to_vec();
                // Read on top of RetroArch's own settings rather than instead
                // of them: everything the user has set in RetroArch's own
                // interface stands, and this adds the one thing only the shell
                // knows — which pad is in whose hands. Written moments before
                // this argv is run; see [`controllers`].
                if let Some(at) = controllers {
                    argv.push("--appendconfig".to_string());
                    argv.push(at.to_string_lossy().into_owned());
                }
                argv.push("-L".to_string());
                argv.push(core.path.clone());
                argv.push(rom.path.clone());
                Some(argv)
            }
            _ => None,
        };
        Entry::Rom(apps::Rom {
            name: rom.title.clone(),
            path: PathBuf::from(&rom.path),
            console: console.title.clone(),
            // Carried rather than looked up on the press, exactly as `start` is
            // and for the same reason: a row that cannot be started has to be
            // able to say what *would* start it, and the press that offers to
            // fetch that core is holding the row and nothing else.
            wanted: console.wanted.clone(),
            // The console it belongs to, which is what a row in a column of
            // covers has to say — or, where nothing can run it, the core that
            // would. Short, because a row's line is clipped at the width of
            // the column and a sentence would end in the middle of a word.
            //
            // It said "needs its BIOS" once, on any console whose core declared
            // a file this machine had not got. That was the shell deciding in
            // advance what an emulator would do with it, and it was wrong often
            // enough to matter: melonDS plays most Nintendo DS games with no
            // dump at all, and half the cores that name a boot ROM name three
            // regions of it. What a missing file stops is now found out by
            // starting the game — see `Shell::rom_would_not_start`.
            note: match (&start, console.wanted.first()) {
                (Some(_), _) => console.title.clone(),
                (None, Some(_)) => {
                    crate::message!("retroarch-console-needs-download", "console" => console.title.as_str())
                }
                (None, None) => {
                    crate::message!("retroarch-console-no-emulator", "console" => console.title.as_str())
                }
            },
            start,
            // The pictures: the ones somebody chose for this game, and
            // libretro's where they chose nothing — see [`pictures_for`].
            // Carried on the row for the reason everything else here is: the
            // column is drawn from these rows sixty times a second, and a cover
            // looked up per frame would be a `stat` per row per frame.
            boxart: pictures.cover,
            snap: pictures.background,
            // And whether either of them is theirs rather than libretro's,
            // which decides two things a path alone cannot say: what the menu
            // over this row offers, and whether the picture behind the display
            // is blurred on its way there.
            own_cover: pictures.own_cover,
            own_background: pictures.own_background,
            // And the shape its console's boxes are, so the card the cover
            // stands on is the shape of the cover rather than of a Steam
            // capsule. On every row of the shelf and not only the ones with a
            // picture: the column is laid out from the first row, and a shelf
            // where the rows with covers were one shape and the rows without
            // were another would be two columns interleaved.
            shape: console.shape,
            // And its console's mark, for the rows that have no cover. A game
            // nobody has drawn artwork for then says which machine it is for
            // instead of saying only that it came out of RetroArch — which the
            // column it is standing in has already said.
            glyph: console_mark(console.glyph.as_deref()),
        })
    }

    /// A picture somebody chose has landed, or has been taken away.
    ///
    /// Read again rather than told what changed: what has been chosen *is* the
    /// directory — there is no list of it anywhere — so the honest way to know
    /// is to look. It costs one `read_dir` of a directory that is nearly always
    /// empty, and it happens on a press.
    ///
    /// The shelves are measured again with it, because a console libretro has
    /// nothing for takes its shape from the pictures somebody put there by
    /// hand, and the first of those is the moment that shelf gets a shape at
    /// all. See [`shelf_shape`].
    pub fn pictures_chosen(&mut self) {
        reread_chosen_pictures();
        if let Some(inner) = self.inner.as_mut() {
            measure_shelves(&mut inner.consoles);
        }
    }

    /// Whether a row of the RetroArch column is one of the consoles the last
    /// scan found.
    ///
    /// What says a folder on the bar is a shelf of somebody's games rather than
    /// any of the other things a folder can be. Asked of the integration rather
    /// than worked out from the rows — a folder whose first row is a game — so
    /// that the answer comes from the same place the shelf was built from and
    /// the two cannot disagree.
    pub fn console(&self, title: &str) -> bool {
        self.consoles().any(|console| console.title == title)
    }

    /// Every game of one console, by the path that starts it.
    ///
    /// What a fetch of that console's artwork is asked about. Empty for a title
    /// no console answers to, which is the same answer as a console with no
    /// games in it — and a fetch asked about nothing is one that finds nothing.
    pub fn console_games(&self, console: &str) -> Vec<String> {
        self.consoles()
            .filter(|held| held.title == console)
            .flat_map(|held| held.roms.iter().map(|rom| rom.path.clone()))
            .collect()
    }

    /// The core that plays one console, under its libretro name — `ppsspp`.
    ///
    /// `None` for a console nothing on this machine can play, and for one whose
    /// emulator is missing the files it reads beside itself: both are a shelf
    /// waiting on a download, and neither has settings to be taken to.
    pub fn core_of(&self, console: &str) -> Option<&str> {
        self.consoles()
            .find(|held| held.title == console)
            .filter(|held| !held.incomplete)
            .and_then(|held| Some(held.core.as_ref()?.name.as_str()))
    }

    /// The consoles the last scan found, or none at all on a machine with no
    /// integration.
    fn consoles(&self) -> impl Iterator<Item = &Console> {
        self.inner.iter().flat_map(|inner| inner.consoles.iter())
    }

    /// What starts RetroArch, if it is here.
    pub fn command(&self) -> Option<&[String]> {
        match &self.inner.as_ref()?.found {
            Found::Here(installation) => Some(&installation.command),
            _ => None,
        }
    }
}

/// The row at the head of the RetroArch column that says which folder it is of,
/// and opens the picker.
///
/// The same row as the one under Settings > Games > RetroArch — built here and
/// used in both places, because it is one setting: two rows that opened the same
/// picker and said different things about it would be two settings on screen.
///
/// A free function rather than a method, and that is what makes the sharing
/// possible: what the row says is where the folder is, and where the folder is,
/// is written down — so the Settings page can build it without an integration
/// object to ask.
///
/// It stands *over* the column rather than in it, so the column opens on the
/// first console and a press of A out of habit does not open a picker nobody
/// asked for. The same rule the folder chooser a file is carried to keeps; see
/// [`crate::transfer`]. Under Settings there is no list for it to stand over
/// and the page turns that off again.
pub fn folder_row() -> Entry {
    let comment = match crate::settings::roms_folder() {
        Some(at) => crate::screenshot::abbreviated(&at),
        None => crate::i18n::text("shell-one-folder-per-console-e-g-psp-nes-etc").to_string(),
    };
    Entry::Folder(apps::Folder {
        title_message: Some("shell-games-folder"),
        comment_message: None,
        identity: None,
        title: crate::i18n::text("shell-games-folder").to_string(),
        comment: Some(comment),
        icon: Some(icons::FILE_FOLDER.to_string()),
        entries: Vec::new(),
        // Read on the press that opens it rather than now, exactly as the
        // wallpaper picker is: what is on somebody's disk is a question about
        // the moment they ask it.
        place: Some(crate::files::Place::Volumes(crate::files::Shows::Folders(
            crate::settings::Picking::RomsFolder,
        ))),
        chosen: false,
        over_the_list: true,
        person: None,
        portrait: None,
    })
}

// --- the pictures somebody chose themselves ---------------------------------

/// Which of a game's two pictures is meant.
///
/// The same two libretro publishes and the shell already draws — see this
/// module's note — because these are those: a picture somebody chose by hand
/// stands exactly where the fetched one would have, and nothing downstream of
/// the row knows the difference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Piece {
    /// The cover, which is what the row *is*.
    Cover,
    /// The picture that stands behind the whole display while the cursor is on
    /// the game.
    Background,
}

impl Piece {
    /// What the kept file is called after the game's own name.
    fn suffix(self) -> &'static str {
        match self {
            Piece::Cover => "cover",
            Piece::Background => "background",
        }
    }

    /// What it is called on the panel the picker is walked from.
    pub fn about(self) -> &'static str {
        match self {
            Piece::Cover => crate::i18n::text("shell-a-cover-for-this-game"),
            Piece::Background => crate::i18n::text("shell-a-background-for-this-game"),
        }
    }
}

/// Where the pictures somebody chose themselves are kept.
///
/// Under the data directory rather than the cache, and that is the whole of the
/// difference between these and the ones fetched from libretro. A cache is a
/// copy of something that can be had again: deleting
/// `lxb/retroarch-art` costs a download. This is a choice somebody made,
/// and there is nowhere on the internet to fetch it back from.
///
/// ```text
/// $XDG_DATA_HOME/lxb/game-art/
///     <console>/<the game's own file name>.cover.png
///     <console>/<the game's own file name>.background.jpg
/// ```
///
/// Named after the game's file rather than after a digest of its path, so that
/// somebody who opens this directory can see what is in it and take a picture
/// out again by deleting it. One console per directory because the same dump
/// can sit on two shelves — a game published for both a handheld and a console
/// keeps one name — and two shelves are two rows with two covers.
pub fn kept_in() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(at) = KEPT_AT
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .clone()
    {
        return Some(at);
    }
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|home| home.is_absolute())
                .map(|home| home.join(".local/share"))
        })?;
    Some(data.join("lxb").join("game-art"))
}

/// The directory one console's chosen pictures are kept in.
///
/// The console's name with anything that could be a path taken out of it. The
/// names come off a scan of somebody's own folders, and a shell that joined one
/// on to a directory unexamined would be one `..` away from writing wherever it
/// was pointed.
fn kept_for(console: &str) -> Option<PathBuf> {
    let safe: String = console
        .chars()
        .map(|letter| match letter {
            '/' | '\\' | '\0' => '-',
            other => other,
        })
        .collect();
    let safe = safe.trim().trim_matches('.').to_string();
    if safe.is_empty() {
        return None;
    }
    Some(kept_in()?.join(safe))
}

/// Keep `source` as one of `rom`'s own pictures, and answer with where it
/// landed.
///
/// Copied rather than pointed at, for the reason the wallpaper is copied — see
/// `crate::paper::keep`. The file somebody chose is one of their own, in a
/// folder they will tidy one day, and a cover that vanished when they sorted
/// out their photographs would be a choice the shell had quietly lost.
///
/// Written beside the destination and renamed on to it, so that the
/// destination's existence means a whole file: this directory *is* the record —
/// there is no list of what has been chosen anywhere — so a half-written file
/// would be a row wearing half a picture with nothing to say it.
///
/// Whatever was there before is replaced, which is what choosing a second
/// picture means. Under a different extension it would otherwise be two answers
/// to one question, so the others are taken away once this one is in place.
pub fn keep_picture(
    console: &str,
    rom: &Path,
    piece: Piece,
    source: &Path,
) -> std::io::Result<PathBuf> {
    let missing = |why: &str| std::io::Error::new(std::io::ErrorKind::NotFound, why.to_string());
    let directory = kept_for(console)
        .ok_or_else(|| missing(crate::i18n::text("label-there-is-nowhere-to-keep-it")))?;
    let game = rom
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            missing(crate::i18n::text(
                "label-that-game-has-no-name-to-keep-it-under",
            ))
        })?;
    let extension = source
        .extension()
        .and_then(|end| end.to_str())
        .unwrap_or("img");
    std::fs::create_dir_all(&directory)?;
    let destination = directory.join(format!("{game}.{}.{extension}", piece.suffix()));
    // Choosing the kept copy itself, which is a directory the picker can be
    // walked into like any other. Copying a file on to itself truncates it.
    if same_file(source, &destination) {
        return Ok(destination);
    }
    let partial = directory.join(format!(".{game}.{}.part", piece.suffix()));
    std::fs::copy(source, &partial).inspect_err(|_| {
        let _ = std::fs::remove_file(&partial);
    })?;
    std::fs::rename(&partial, &destination).inspect_err(|_| {
        let _ = std::fs::remove_file(&partial);
    })?;
    forget_other_pictures(&directory, game, piece, &destination);
    Ok(destination)
}

/// Whether two paths are the same file on the disk rather than two names that
/// look alike.
fn same_file(one: &Path, other: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let (Ok(one), Ok(other)) = (std::fs::metadata(one), std::fs::metadata(other)) else {
        return false;
    };
    one.dev() == other.dev() && one.ino() == other.ino()
}

/// Take away the earlier answers to this one question: the same game's same
/// picture under another extension.
///
/// Narrow on purpose. It matches one game's name and one of its two pictures
/// and nothing else, because this directory holds one console's chosen pictures
/// and a broader sweep would be a routine that could delete the cover of the
/// game beside it.
fn forget_other_pictures(directory: &Path, game: &str, piece: Piece, kept: &Path) {
    for at in pictures_of(directory, game, piece) {
        if at != kept {
            let _ = std::fs::remove_file(&at);
        }
    }
}

/// Every file in `directory` that is one game's one picture, whatever it is
/// called after that.
///
/// The matching is deliberately exact up to the extension: `Metroid.nes.cover.`
/// and one more component. A prefix match would take `Metroid.nes.cover.old.png`
/// with it, which is a file this never wrote and therefore somebody else's.
fn pictures_of(directory: &Path, game: &str, piece: Piece) -> Vec<PathBuf> {
    let stem = format!("{game}.{}.", piece.suffix());
    let Ok(held) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    held.flatten()
        .filter(|entry| entry.file_type().is_ok_and(|what| what.is_file()))
        .map(|entry| entry.path())
        .filter(|at| {
            at.file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_prefix(&stem))
                .is_some_and(|end| !end.is_empty() && !end.contains('.'))
        })
        .collect()
}

/// Take away the picture somebody chose for a game, so its row goes back to
/// whatever libretro published — or to the mark, where libretro has nothing.
///
/// Answers whether anything went. The file *is* the record, so deleting it is
/// the whole of forgetting the choice; there is no setting anywhere to put back.
///
/// It removes only the one picture of the one game it was asked about. A game
/// whose cover somebody chose and whose background they did not keeps the
/// background, because those are two separate answers to two separate
/// questions.
pub fn drop_picture(console: &str, rom: &Path, piece: Piece) -> bool {
    let Some(directory) = kept_for(console) else {
        return false;
    };
    let Some(game) = rom.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let mut gone = false;
    for at in pictures_of(&directory, game, piece) {
        match std::fs::remove_file(&at) {
            Ok(()) => {
                tracing::info!(at = %at.display(), "the picture they chose is forgotten");
                gone = true;
            }
            Err(err) => tracing::warn!(%err, at = %at.display(), "it could not be taken away"),
        }
    }
    gone
}

/// One picture somebody chose, as the disk has it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Chosen {
    console: String,
    /// The game's own file name, extension and all — which is what the kept
    /// file is named after and what a row can be matched by.
    game: String,
    piece: Piece,
    at: PathBuf,
}

/// What somebody has chosen for their own games, read off the disk.
///
/// `None` until it has been looked at, which is once a session and again
/// whenever a picture is chosen. Held rather than stat'ed per row because the
/// rows are rebuilt on every line of a collection's artwork coming down, and
/// two `stat`s per game per line is a hundred games' worth of disk for every
/// cover that lands. Nearly always empty, and an empty list costs a lock and a
/// glance.
static CHOSEN: Mutex<Option<Vec<Chosen>>> = Mutex::new(None);

/// Read the directory again, whatever was read before.
pub fn reread_chosen_pictures() {
    let held = read_chosen();
    if !held.is_empty() {
        tracing::info!(pictures = held.len(), "pictures chosen by hand");
    }
    *CHOSEN.lock().unwrap_or_else(|err| err.into_inner()) = Some(held);
}

/// What one console's games have been given, by the game's own file name.
///
/// Empty where nothing has been chosen at all, which is the ordinary answer and
/// the cheap one.
fn chosen_for(console: &str) -> Vec<Chosen> {
    let mut held = CHOSEN.lock().unwrap_or_else(|err| err.into_inner());
    let all = held.get_or_insert_with(read_chosen);
    all.iter()
        .filter(|one| one.console == console)
        .cloned()
        .collect()
}

/// Everything in the directory, as pictures of games.
///
/// Anything that cannot be read as one is left where it is rather than
/// mentioned: this is somebody's own directory, and a file in it that this did
/// not write is theirs.
fn read_chosen() -> Vec<Chosen> {
    let Some(directory) = kept_in() else {
        return Vec::new();
    };
    let Ok(consoles) = std::fs::read_dir(&directory) else {
        return Vec::new();
    };
    let mut held = Vec::new();
    for console in consoles.flatten() {
        let Some(name) = console.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Ok(pictures) = std::fs::read_dir(console.path()) else {
            continue;
        };
        for picture in pictures.flatten() {
            let at = picture.path();
            let Some((game, piece)) = at
                .file_name()
                .and_then(|file| file.to_str())
                .and_then(names)
            else {
                continue;
            };
            held.push(Chosen {
                console: name.clone(),
                game,
                piece,
                at,
            });
        }
    }
    held
}

/// The game and the picture a kept file's name says it is.
///
/// `Tekken 5 (USA).iso.cover.png` is the cover of `Tekken 5 (USA).iso`. Read
/// from the end, because the game's own name may hold as many dots as it likes
/// and the two on the end are the only ones this wrote.
fn names(file: &str) -> Option<(String, Piece)> {
    let (rest, _) = file.rsplit_once('.')?;
    for piece in [Piece::Cover, Piece::Background] {
        if let Some(game) = rest.strip_suffix(&format!(".{}", piece.suffix())) {
            if !game.is_empty() {
                return Some((game.to_string(), piece));
            }
        }
    }
    None
}

/// How many of a console's covers are measured before its shelf is given a
/// shape.
///
/// A handful rather than one, because one odd scan should not decide what a
/// whole column looks like, and a handful rather than all of them because
/// libretro's collection for a console is one photographer's work: every box on
/// a shelf is the same box. Eight is enough for the middle one to be the shape
/// the console's cases actually are, and cheap enough to do again each time a
/// cover lands.
const COVERS_SAMPLED: usize = 8;

/// The step a cover's shape is taken to.
///
/// Two of the same console's covers can differ by a pixel down one edge — 512
/// by 725 against 512 by 726 — and without this the shelf would be relaid out
/// around that pixel every time one of them arrived. A hundredth is finer than
/// anybody can see across a card and coarser than any scanner's rounding.
const SHAPE_STEP: f32 = 0.01;

/// The narrowest and widest shape a cover is allowed to make a card.
///
/// Not a judgement about artwork: it is what stops one corrupt or mislabelled
/// picture — a banner, a strip, a file that is not a picture of a box at all —
/// from turning a shelf into a row of slots. Wide enough for the widest case
/// anybody printed and narrow enough for the tallest.
const NARROWEST_SHELF: f32 = 0.4;
const WIDEST_SHELF: f32 = 2.5;

/// The shape a console's covers are: the width of one over its height.
///
/// Measured, and that is the point. A table of consoles and their box shapes
/// would be this shell asserting the dimensions of artwork it did not make, out
/// of date the moment libretro rescanned a collection, and wrong for every
/// console nobody had thought to put in it. The covers are already on this disk
/// and each one carries its own size in its first two dozen bytes.
///
/// The middle of what the first [`COVERS_SAMPLED`] of them measure — the middle
/// rather than the mean, so that one cover of the wrong thing moves the answer
/// by nothing at all — taken to [`SHAPE_STEP`] and held inside
/// [`NARROWEST_SHELF`]..[`WIDEST_SHELF`]. `None` for a console with no cover on
/// this disk yet, which is a shelf of marks and has no shape of its own to be
/// drawn at.
///
/// Always the *first* of them rather than a sample of all of them, so the answer
/// stops moving once the top of the shelf has its pictures: a collection coming
/// down settles its shape in the first few covers and is not relaid out again by
/// the hundredth.
///
/// Only the header of each file is read. `into_dimensions` stops at the point
/// where a picture says how big it is, so this is eight opens and eight short
/// reads rather than eight PNGs decoded.
pub fn shelf_shape(covers: impl Iterator<Item = impl AsRef<Path>>) -> Option<f32> {
    let mut shapes: Vec<f32> = covers.take(COVERS_SAMPLED).filter_map(measured).collect();
    if shapes.is_empty() {
        return None;
    }
    shapes.sort_by(f32::total_cmp);
    Some(shapes[shapes.len() / 2])
}

/// Measure every console's shelf off the pictures its rows will draw.
///
/// The two moments a shape can change both end here, and so does the third: a
/// scan landing, a cover arriving, and somebody choosing a picture by hand.
/// One function rather than the same line written three times, because a shape
/// worked out one way in one place and another way in another is a column that
/// changes size depending on what happened to it last.
fn measure_shelves(consoles: &mut [Console]) {
    for console in consoles {
        console.shape = shelf_shape(covers(console));
    }
}

/// The covers a console has on this disk, in the order its games are in.
///
/// The games with none are simply not in it, so a console where the ninth game
/// is the first with a cover is measured off that one rather than answering
/// "nothing to measure" — which is what taking the first [`COVERS_SAMPLED`]
/// *rows* would have done.
///
/// Whichever picture each row will actually draw, which is the chosen one where
/// somebody has chosen it. A shelf of a console libretro has never heard of has
/// no fetched cover at all and is still a shelf of covers if the user put them
/// there by hand, and it has to be drawn at their shape rather than at the one a
/// column of marks falls back to.
fn covers(console: &Console) -> impl Iterator<Item = PathBuf> + '_ {
    let chosen = chosen_for(&console.title);
    console
        .roms
        .iter()
        .filter_map(move |rom| pictures_for(&chosen, rom).cover)
}

/// The two pictures one game's row will draw, and whether each of them is the
/// user's own.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Shown {
    pub cover: Option<PathBuf>,
    pub background: Option<PathBuf>,
    /// Whether [`Shown::cover`] is a picture somebody chose rather than one
    /// libretro published.
    ///
    /// Two things read it. The menu over the game offers to take away a picture
    /// somebody chose and never one they did not, and the picture behind the
    /// display is drawn differently: libretro's is a console's screen, three
    /// hundred pixels tall, and is blurred on its way across a television,
    /// while a picture somebody chose is theirs at whatever size they chose it
    /// and is drawn as it is. See `Shell::sight_of`.
    pub own_cover: bool,
    pub own_background: bool,
}

/// The two pictures one game's row will draw: what somebody chose, and what was
/// fetched where they chose nothing.
///
/// Chosen first, and that is the whole of what choosing one means. libretro's
/// is what nearly every game gets and it stays where nobody has said otherwise;
/// a game whose cover somebody replaced by hand keeps theirs through every
/// fetch afterwards, because a run that put the published picture back would be
/// the shell overruling a choice on the user's behalf.
fn pictures_for(chosen: &[Chosen], rom: &Rom) -> Shown {
    let game = Path::new(&rom.path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let theirs = |piece: Piece| {
        chosen
            .iter()
            .find(|one| one.piece == piece && one.game == game)
            .map(|one| one.at.clone())
    };
    let (cover, background) = (theirs(Piece::Cover), theirs(Piece::Background));
    Shown {
        own_cover: cover.is_some(),
        own_background: background.is_some(),
        cover: cover.or_else(|| rom.boxart.as_deref().map(PathBuf::from)),
        background: background.or_else(|| rom.snap.as_deref().map(PathBuf::from)),
    }
}

/// What one picture on this disk measures, or `None` for anything that could
/// not be read as one.
fn measured(at: impl AsRef<Path>) -> Option<f32> {
    let file = image::ImageReader::open(at).ok()?;
    // Guessed from the bytes rather than trusted from the name. Every one of
    // these is called `.png` because that is what libretro calls them, and a
    // shelf should not lose its shape over one that turned out to be a JPEG.
    let (width, height) = file.with_guessed_format().ok()?.into_dimensions().ok()?;
    let shape = width as f32 / height.max(1) as f32;
    shape
        .is_finite()
        .then(|| ((shape / SHAPE_STEP).round() * SHAPE_STEP).clamp(NARROWEST_SHELF, WIDEST_SHELF))
}

/// The largest file worth treating as a BIOS, and the most of them to take.
///
/// Guards rather than limits anybody is expected to meet: a PlayStation 2 BIOS
/// is about four megabytes and a Saturn's is half of one. What these stop is
/// somebody pointing the picker at their home directory and this copying it
/// into RetroArch's system folder.
const BIGGEST: u64 = 64 * 1024 * 1024;
const MOST: usize = 64;

/// Whether a file could be a console's boot ROM.
///
/// Asked only where a declaration names a *folder* rather than a file, which in
/// the whole of libretro's collection is one core: pcsx2 reads every BIOS it
/// finds in `pcsx2/bios` and lets the user pick a region in its own menu, so
/// there is no name to match on and nothing else telling a dump from whatever
/// happens to be in the folder somebody pointed at. Everywhere else the
/// declaration names the file, and a name is a better test than any of this —
/// `neogeo.zip` really is a zip, and a rule about what a dump looks like would
/// refuse it.
///
/// It is deliberately a test for what a dump is *not*. A boot ROM is an opaque
/// blob and there is nothing positive to look for that would not be a table of
/// consoles, which this integration does not have anywhere. So what is refused
/// is what a blob demonstrably is not: an archive, a document, a picture, a
/// recording, a program, and text of any kind.
///
/// **The helper has its own copy of this, and the two have to agree.** It is
/// the half that decides whether a firmware folder counts as filled, this is
/// the half that fills it, and the two packages deliberately do not depend on
/// each other — a machine may have either without the other. A file this copies
/// and that one refuses is a folder that fills up and never counts as filled,
/// which is a console nobody can ever finish setting up. See
/// `lxb-retroarch/src/firmware.rs`.
fn could_be_firmware(at: &Path) -> bool {
    let Ok(about) = std::fs::metadata(at) else {
        return false;
    };
    if !about.is_file() || about.len() == 0 || about.len() > BIGGEST {
        return false;
    }
    let mut head = [0u8; 512];
    let Ok(read) = std::fs::File::open(at).and_then(|mut file| {
        use std::io::Read;
        file.read(&mut head)
    }) else {
        return false;
    };
    let head = &head[..read];
    !something_else(head) && !plain_text(head)
}

/// Whether what a file begins with says it is one of the kinds of thing a boot
/// ROM is not.
///
/// Signatures rather than file names, because a name is the one thing about a
/// file anybody can change and somebody's `bios.bin` is very often the zip they
/// downloaded it in. Nothing here is a guess about emulation: it is the list of
/// formats a person's folder actually holds.
fn something_else(head: &[u8]) -> bool {
    const SIGNATURES: &[&[u8]] = &[
        b"PK\x03\x04", // a zip, and every format built on one
        b"PK\x05\x06",
        b"PK\x07\x08",
        b"\x1f\x8b",           // gzip
        b"BZh",                // bzip2
        b"\xfd7zXZ\x00",       // xz
        b"7z\xbc\xaf\x27\x1c", // 7-zip
        b"Rar!",               // rar
        b"\x28\xb5\x2f\xfd",   // zstandard
        b"%PDF",               // a document
        b"\xd0\xcf\x11\xe0",   // an older one
        b"\x89PNG\r\n\x1a\n",  // a picture
        b"\xff\xd8\xff",
        b"GIF8",
        b"BM",
        b"RIFF", // a recording, a film, or a webp
        b"OggS",
        b"fLaC",
        b"ID3",
        b"\x1a\x45\xdf\xa3", // matroska
        b"\x7fELF",          // a program
        b"\xca\xfe\xba\xbe", // a java class
        b"SQLite format 3\x00",
    ];
    if SIGNATURES.iter().any(|magic| head.starts_with(magic)) {
        return true;
    }
    // The one that is not at the beginning: an mp4, a mov and a heic all carry
    // their kind four bytes in.
    head.len() >= 8 && &head[4..8] == b"ftyp"
}

/// Whether a file is text.
///
/// A boot ROM is machine code and tables, and both put a zero byte in the first
/// few hundred of them almost immediately; nothing written for a person to read
/// has one at all. So the test is that first, and then that every byte is one
/// somebody could have typed — which is what keeps a `.cue`, an `.m3u`, an
/// `.xml` and a folder of notes out of a console's firmware directory.
fn plain_text(head: &[u8]) -> bool {
    !head.contains(&0)
        && head
            .iter()
            .all(|byte| matches!(byte, 0x20..=0x7e | b'\t' | b'\n' | b'\r'))
}

/// Whether what one declaration asks for is on the disk *now*.
///
/// The same question [`Firmware::here`] answers, asked again rather than
/// believed. `here` is what the last scan found, and the press this is for has
/// just copied files: a shell that read the old answer would tell somebody
/// their console was still missing a BIOS they had put down a moment ago, or —
/// worse, and this is the bug it was written for — tell them a folder of
/// holiday photographs had set their PlayStation 2 up.
///
/// The reading is the helper's, kept in step with it deliberately: a file that
/// exists, or a folder holding something that could be a dump.
fn on_disk(wanted: &Firmware) -> bool {
    match std::fs::metadata(&wanted.into) {
        Ok(what) if what.is_dir() => std::fs::read_dir(&wanted.into)
            .map(|entries| {
                entries
                    .flatten()
                    .any(|entry| could_be_firmware(&entry.path()))
            })
            .unwrap_or(false),
        Ok(_) => true,
        Err(_) => false,
    }
}

/// Whether a path libretro declared names a file rather than a folder.
///
/// Nothing in the format says which it is: `firmware0_path` is `pcsx2/bios` for
/// pcsx2 and `dc/dc_boot.bin` for a Dreamcast core, and the only thing telling
/// them apart is that one of them has an extension. Which is enough — a BIOS
/// dump has a name like `scph39001.bin` and the folders these live in do not.
fn names_a_file(at: &Path) -> bool {
    at.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.contains('.'))
}

/// Copy the firmware out of the folder somebody pointed at, into the places the
/// cores are already looking.
///
/// A copy rather than a setting, because RetroArch has no setting for it: its
/// cores look for particular names below one system folder of its own, so
/// pointing an emulator at somebody's downloads directory is not a thing the
/// format allows. What can be done is to put the files where the core looks.
///
/// The files themselves are never fetched, inspected or renamed. A BIOS is the
/// console maker's and the only lawful copy is one dumped from hardware
/// somebody owns; this moves a file the user already had from one of their
/// folders to another, and does not care what is in it.
///
/// Answers how many files it put down, which is what the panel afterwards says.
pub fn place(from: &Path, wanted: &[Firmware]) -> usize {
    let Ok(entries) = std::fs::read_dir(from) else {
        return 0;
    };
    // The chosen folder itself and no deeper. Somebody pointing at the folder
    // their dumps are in has pointed at the files; walking below it would be
    // this deciding that some other folder of theirs was also meant.
    let here: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|at| {
            at.metadata()
                .is_ok_and(|about| about.is_file() && about.len() <= BIGGEST)
        })
        .take(MOST)
        .collect();

    let mut placed = 0;
    for missing in wanted {
        if names_a_file(&missing.into) {
            // One named file. Matched without regard to case, because a dump
            // is `SCPH39001.BIN` as often as `scph39001.bin` and a core that
            // wants one of those will read the other.
            let Some(name) = missing.into.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let found = here.iter().find(|at| {
                at.file_name()
                    .and_then(|had| had.to_str())
                    .is_some_and(|had| had.eq_ignore_ascii_case(name))
            });
            let (Some(found), Some(parent)) = (found, missing.into.parent()) else {
                continue;
            };
            if std::fs::create_dir_all(parent).is_ok()
                && std::fs::copy(found, &missing.into).is_ok()
            {
                placed += 1;
            }
        } else {
            // A whole folder, which is what pcsx2 declares: it reads every
            // BIOS it finds in there and lets the user pick a region in its own
            // menu, so all of them go in rather than this choosing one.
            if std::fs::create_dir_all(&missing.into).is_err() {
                continue;
            }
            for at in &here {
                // Everything that could be one, and nothing that could not.
                // The folder somebody points at is theirs and holds whatever
                // they keep in it; what goes into RetroArch's firmware
                // directory decides whether the console reads as set up, so a
                // shell that copied their notes and their cover scans in would
                // be answering its own question with its own noise. See
                // [`could_be_firmware`].
                if !could_be_firmware(at) {
                    continue;
                }
                let Some(name) = at.file_name() else { continue };
                if std::fs::copy(at, missing.into.join(name)).is_ok() {
                    placed += 1;
                }
            }
        }
    }
    placed
}

/// The row that asks where somebody's BIOS dumps are.
///
/// One emulator's, on that emulator's own settings page — Settings > Games >
/// RetroArch > LRPS2 > PlayStation 2 BIOS. Not on the RetroArch column beside
/// somebody's consoles, where it was first put: that column is a list of games
/// to play, and a job to do sitting in the middle of it is a job in the way. The
/// press that actually needs it is the one on a game that cannot start, and that
/// press raises a panel and walks the user here.
///
/// **There whether or not the file is already in place.** A dump put in the
/// wrong place, a second console's, a bad copy — choosing one is a thing
/// somebody has to be able to do twice, and a row that vanished the moment it
/// was answered could only be reached again by breaking the machine.
///
/// A copy and not a setting. RetroArch reads firmware out of one folder of its
/// own and its cores look for particular names inside it, so pointing an
/// emulator at somebody's downloads directory is not a thing the format allows;
/// what can be done is to put the files where the core is already looking.
pub fn bios_row(wanted: &[Firmware]) -> Option<Entry> {
    let first = wanted.first()?;
    let here = unserved(wanted).is_empty();
    // libretro's own sentence where it names a file, since it says the region
    // too. Where the declaration is a whole folder it reads `'pcsx2/bios'
    // folder`, which describes where this shell is about to put things and is
    // no use at all to somebody wondering what they are supposed to have.
    let comment = match (here, first.worth_saying()) {
        (true, _) => {
            crate::i18n::text("shell-added-choose-another-folder-to-replace-it").to_string()
        }
        (false, true) => first.note.clone(),
        (false, false) => {
            crate::i18n::text("shell-not-added-choose-the-folder-yours-is-in").to_string()
        }
    };
    Some(Entry::Folder(apps::Folder {
        title_message: None,
        comment_message: None,
        identity: None,
        title: bios_row_title(&first.console),
        comment: Some(comment),
        icon: Some(icons::FILE_FOLDER.to_string()),
        entries: Vec::new(),
        place: Some(crate::files::Place::Volumes(crate::files::Shows::Folders(
            crate::settings::Picking::Firmware,
        ))),
        chosen: false,
        over_the_list: true,
        person: None,
        portrait: None,
    }))
}

/// What one installed core can be set to, as the helper reports it.
///
/// The core's own declaration, asked of the core itself — see the helper's
/// `options.rs`. Nothing here is written down in this repository, and that is
/// the point: an emulator's settings belong to the emulator, and a copy of
/// them here would be wrong the first time somebody updated it.
#[derive(Debug, Clone, Deserialize)]
pub struct CoreOptions {
    /// Its libretro name — `ppsspp`.
    pub core: String,
    /// What it calls itself — `PPSSPP`. The name RetroArch files its chosen
    /// settings under, so the name this shell has to spell when writing them.
    pub display: String,
    #[serde(default)]
    pub categories: Vec<CoreCategory>,
    #[serde(default)]
    pub options: Vec<CoreSetting>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CoreCategory {
    pub key: String,
    pub title: String,
}

/// One setting a core declares.
///
/// The core's own sentence about what the setting *does* is in the record the
/// helper writes and is deliberately not read here. There is one line under a
/// row on this bar and it already says something more useful: what the setting
/// is set to now. A page of thirty explanations with no answers beside them is
/// a page somebody has to walk into thirty times to read.
#[derive(Debug, Clone, Deserialize)]
pub struct CoreSetting {
    pub key: String,
    pub title: String,
    pub category: Option<String>,
    pub default: Option<String>,
    pub values: Vec<CoreValue>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CoreValue {
    pub value: String,
    pub label: String,
}

/// What every installed core says it can be set to, and where RetroArch keeps
/// its configuration.
///
/// Two statics rather than fields on [`RetroArch`] because the Settings tree is
/// built by free functions that take nothing — see [`crate::settings`] — and
/// what they need is a fact about this machine rather than about any one
/// screen. Both are written once the helper answers and read on every rebuild.
/// One thing a console needs that nobody may fetch, and the folder on this
/// disk it belongs in.
///
/// The BIOS problem, made into something a press can act on. What libretro
/// declares is a path below RetroArch's system folder; what the shell can offer
/// is to put a file there. Both halves are needed to do anything but complain,
/// which is all this integration could do about a missing BIOS before.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Firmware {
    /// The console it stops — "PlayStation 2".
    pub console: String,
    /// The core that wants it, by its libretro name — `pcsx2`. What files the
    /// question under a core's own settings page: it is that emulator's BIOS,
    /// and a page belonging to another must not offer it.
    pub core: String,
    /// libretro's own sentence about the file.
    pub note: String,
    /// Where it goes, absolute.
    pub into: PathBuf,
    /// Whether it is there already.
    ///
    /// Reported either way, because the row that offers to go and find one is
    /// on the emulator's settings page and belongs there afterwards too: a
    /// dump put in the wrong place, or a second console's, is a thing somebody
    /// has to be able to do twice. What turns on this is what the row says and
    /// whether a game can start — not whether the row exists.
    pub here: bool,
}

impl Firmware {
    /// Whether libretro's sentence about this is worth showing somebody.
    ///
    /// It is where it names a file — "dc/dc_boot.bin (Dreamcast BIOS)" tells a
    /// person what to go and look for. It is not where the declaration is a
    /// whole folder: pcsx2's reads `'pcsx2/bios' folder`, which says only where
    /// this shell is about to put things and is no help at all to somebody
    /// wondering what they are supposed to have.
    pub fn worth_saying(&self) -> bool {
        names_a_file(&self.into)
    }
}

/// What this machine is missing, from the last scan.
///
/// Held here rather than reached for through the integration for the reason
/// [`TUNABLES`] is: the Settings tree is built by free functions taking
/// nothing, and this is a fact about the machine rather than about any screen.
static FIRMWARE: Mutex<Vec<Firmware>> = Mutex::new(Vec::new());

/// What one core declares, for that core's own settings page.
pub fn firmware_for(core: &str) -> Vec<Firmware> {
    firmware()
        .into_iter()
        .filter(|wanted| wanted.core == core)
        .collect()
}

/// What the row asking for one console's BIOS is called.
///
/// One function rather than two spellings, because two things look for it: the
/// page that puts it there, and the press that has to walk somebody to it from
/// a panel raised over a game several columns away.
pub fn bios_row_title(console: &str) -> String {
    format!("{console} BIOS")
}

/// Which console a firmware picker standing open is about, by the row it was
/// opened from.
///
/// The question a folder chooser answers has to travel with the walk — see
/// [`crate::files::Shows`] — and this is the half of it the walk cannot carry:
/// what the columns know is that a folder is being chosen for a BIOS, and what
/// the press at the end needs is *whose*. Read at the moment the picker opens,
/// which is the last moment the row is still under the cursor.
///
/// Matched against what the shell itself put on the row rather than by taking
/// the title apart, so a console called "Nintendo DS BIOS Edition" could not
/// answer for one called "Nintendo DS".
pub fn console_of_bios_row(title: &str) -> Option<String> {
    firmware()
        .into_iter()
        .map(|wanted| wanted.console)
        .find(|console| bios_row_title(console) == title)
}

/// Every required file the installed cores declare, present or not.
pub fn firmware() -> Vec<Firmware> {
    FIRMWARE
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .clone()
}

/// What one core's settings page is called, once the cores have answered.
///
/// The emulator's own name — `PPSSPP`, `melonDS` — because that is the name the
/// page wears, and the page is what a walk into Settings has to find. `None`
/// before the cores have been asked, which is every session until somebody
/// reaches Settings: the page does not exist then either, so the two answers
/// agree and the walk waits for both.
pub fn emulator_page(core: &str) -> Option<String> {
    tunables()
        .into_iter()
        .find(|held| held.core == core)
        .map(|held| held.display)
}

/// Which console's mark each installed core wears, from the last scan.
///
/// A core's settings page is a page about *a console* — melonDS is the Nintendo
/// DS page whatever its authors called the emulator — so it wears the mark the
/// shelf draws that console with. Taken from the scan rather than from a table
/// here, and that is the whole point: the shelf's rows and these rows then
/// cannot disagree, because there is one answer and both read it.
///
/// A core no console on this machine names is not in here, and falls back to
/// RetroArch's own mark. That is the honest answer rather than a gap: if no
/// shelf names it, there is no console mark it *would* match.
static CORE_MARKS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

/// The mark for one core's own settings page.
///
/// Goes through [`console_mark`] like every other console mark, so a drawing
/// that did not ship falls back the same way rather than reaching the atlas as
/// a name nothing drew.
pub fn mark_for_core(core: &str) -> String {
    let glyph = CORE_MARKS
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .iter()
        .find(|(named, _)| named == core)
        .map(|(_, glyph)| glyph.clone());
    console_mark(glyph.as_deref())
}

/// Serialises the tests that state this module's facts about the machine.
///
/// [`FIRMWARE`] and [`CORE_MARKS`] are module-wide and tests run in parallel,
/// so a test that states one and a test that reads it have to take turns — or
/// the first decides what the second sees, and the pair fails once in a while
/// on a machine that happened to schedule them together. The same shape as
/// `settings`' own `LOCK` and `theme::with_accent`.
///
/// Here rather than in either test module because the two that need it are in
/// two different ones.
#[cfg(test)]
pub static GLOBALS: Mutex<()> = Mutex::new(());

/// Say which console's mark a core wears, for a test that draws its page.
///
/// The settings tree is built by free functions that reach for this module's
/// facts, so a test of one of those pages has to be able to state the fact
/// first — the same shape as the scan writing it, and the reason `CORE_MARKS`
/// is not simply made public. Take [`GLOBALS`] first.
#[cfg(test)]
pub fn set_core_marks(marks: Vec<(String, String)>) {
    *CORE_MARKS.lock().unwrap_or_else(|err| err.into_inner()) = marks;
}

/// The same for what a console cannot start without. Take [`GLOBALS`] first.
/// Point the settings readers and writers at a scratch configuration.
#[cfg(test)]
pub fn set_config_at(at: Option<PathBuf>) {
    *CONFIG_AT.lock().unwrap_or_else(|err| err.into_inner()) = at;
}

/// Keep the pictures somebody chooses somewhere a test can look at, rather than
/// in the data directory of whoever is running the suite. Take [`GLOBALS`]
/// first.
#[cfg(test)]
static KEPT_AT: Mutex<Option<PathBuf>> = Mutex::new(None);

#[cfg(test)]
pub fn set_kept_at(at: Option<PathBuf>) {
    *KEPT_AT.lock().unwrap_or_else(|err| err.into_inner()) = at;
    reread_chosen_pictures();
}

#[cfg(test)]
pub fn set_firmware(wanted: Vec<Firmware>) {
    *FIRMWARE.lock().unwrap_or_else(|err| err.into_inner()) = wanted;
}

static TUNABLES: Mutex<Vec<CoreOptions>> = Mutex::new(Vec::new());
static CONFIG_AT: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Every installed core's settings, in the order the cores answered.
pub fn tunables() -> Vec<CoreOptions> {
    TUNABLES
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .clone()
}

/// Where RetroArch keeps its configuration, once the helper has said.
pub fn config_at() -> Option<PathBuf> {
    CONFIG_AT
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .clone()
}

/// The file RetroArch keeps one core's chosen settings in.
///
/// Its own directory named for the core, which is RetroArch's own layout: a
/// setting written anywhere else is a setting the emulator will not read.
fn core_options_file(config: &Path, named: &str) -> PathBuf {
    config
        .join("config")
        .join(named)
        .join(format!("{named}.opt"))
}

/// What that core's setting is set to now, or `None` where nobody has set it.
///
/// The caller falls back to the core's own default, which is what RetroArch
/// does with an absent line and is why an absent line is not worth writing.
pub fn core_option(named: &str, key: &str) -> Option<String> {
    let config = config_at()?;
    read_key(&core_options_file(&config, named), key)
}

/// Set one of a core's settings, in the file RetroArch reads it from.
pub fn set_core_option(named: &str, key: &str, value: &str) -> bool {
    let Some(config) = config_at() else {
        return false;
    };
    let at = core_options_file(&config, named);
    let done = write_key(&at, key, value);
    if done {
        tracing::info!(core = named, key, value, "a core setting was written");
    }
    done
}

/// What one of RetroArch's own settings is set to now.
pub fn setting(key: &str) -> Option<String> {
    let config = config_at()?;
    read_key(&config.join("retroarch.cfg"), key)
}

/// Set one of RetroArch's own settings.
///
/// Written into the emulator's own configuration, and deliberately: unlike the
/// controllers, which are a fact about the last few seconds and are appended
/// for one launch only, an aspect ratio is a thing somebody chose and expects
/// to still be chosen tomorrow — including when they open RetroArch by itself.
pub fn set_setting(key: &str, value: &str) -> bool {
    let Some(config) = config_at() else {
        return false;
    };
    let done = write_key(&config.join("retroarch.cfg"), key, value);
    if done {
        tracing::info!(key, value, "a RetroArch setting was written");
    }
    done
}

/// What this shell sets an emulator it has just met to, and only where nobody
/// has said otherwise.
///
/// One setting today: **what RetroArch draws with**. Its own default is
/// OpenGL, and on this bar that is the wrong answer — a PlayStation 2 drawn
/// through it comes out full of graphical faults, and the cores that can use
/// Vulkan are the ones whose consoles need the help. RetroArch chose OpenGL
/// years ago for machines that had nothing else; a machine running this shell
/// is drawing its own bar through Vulkan or it would not be here.
///
/// **Only where the line is absent.** A `retroarch.cfg` with no `video_driver`
/// in it is one nothing has ever chosen for — RetroArch writes every key it
/// has on the way out, so the moment somebody runs it once, or picks a driver
/// under Settings > Games > RetroArch > Video driver, the line is there and
/// this never looks again. That is what makes it a *default* rather than the
/// shell overruling people.
///
/// Answers what it set, for the log and for the test. Nothing at all is the
/// ordinary answer.
pub fn settle_defaults(vulkan: bool) -> Vec<(&'static str, &'static str)> {
    let mut set = Vec::new();
    // Never onto a machine that has not got it. The emulator would take the
    // line, fail to open a context and come back with nothing on the screen —
    // which is the failure this whole integration keeps being about.
    if vulkan && setting("video_driver").is_none() && set_setting("video_driver", "vulkan") {
        set.push(("video_driver", "vulkan"));
    }
    set
}

/// One `key = "value"` line out of a file of them.
fn read_key(at: &Path, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(at).ok()?;
    text.lines().find_map(|line| {
        let (said, value) = line.split_once('=')?;
        (said.trim() == key).then(|| value.trim().trim_matches('"').to_string())
    })
}

/// Put one `key = "value"` line into a file of them, leaving the rest alone.
///
/// The whole file is read and written back, because these are somebody's own
/// settings: `retroarch.cfg` is a hundred kilobytes of them, and a writer that
/// rewrote only what it understood would quietly drop everything it did not.
/// Written beside and renamed into place, so a file interrupted half way
/// through is the old one rather than half of each.
fn write_key(at: &Path, key: &str, value: &str) -> bool {
    let said = format!("{key} = \"{value}\"");
    let existing = std::fs::read_to_string(at).unwrap_or_default();
    let mut out = String::with_capacity(existing.len() + said.len() + 1);
    let mut replaced = false;
    for line in existing.lines() {
        match line.split_once('=') {
            Some((had, _)) if had.trim() == key => {
                if !replaced {
                    out.push_str(&said);
                    out.push('\n');
                    replaced = true;
                }
            }
            _ => {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    if !replaced {
        out.push_str(&said);
        out.push('\n');
    }

    if let Some(parent) = at.parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            tracing::warn!(at = %parent.display(), %err, "it could not be made");
            return false;
        }
    }
    let part = at.with_extension("lxb-part");
    if let Err(err) = std::fs::write(&part, &out) {
        tracing::warn!(at = %part.display(), %err, "a setting could not be written");
        return false;
    }
    if let Err(err) = std::fs::rename(&part, at) {
        let _ = std::fs::remove_file(&part);
        tracing::warn!(at = %at.display(), %err, "a setting could not be put in place");
        return false;
    }
    true
}

/// The rows of the menu raised over the RetroArch row on the bar.
///
/// Kept out of the live shell so what the row offers is testable without a
/// Wayland session, exactly as Steam's two menus are.
///
/// ## What is on it, and why it is not one list
///
/// The row means a different thing in each of the states its press answers —
/// see [`Press`] — and a menu is a list of things that can be done *now*. A
/// machine with no folder chosen has no collection to look through and no
/// cores to fetch for it; a machine still waiting on the helper does not yet
/// know what it has. Offering those rows greyed out would be a menu whose
/// every row is a refusal, which is the argument the Steam menu makes about a
/// library that is not there.
///
/// The last band is the same in every state that has a menu at all: RetroArch's
/// own interface, and the way out. It is below the rule because it is not about
/// this machine's collection — it is a different program, and the rows above
/// are things the shell does itself.
///
/// Empty for a state with nothing to offer, and an empty list raises no menu.
pub fn menu_rows(press: &Press, missing: usize, folder_set: bool) -> Vec<menu::Entry> {
    let mut rows = Vec::new();
    match press {
        // Nothing is known yet, and nothing can honestly be offered. The press
        // itself asks the helper again; a menu cannot.
        Press::Waiting => return rows,
        // It cannot be put on this machine, and every row here needs it.
        Press::Cannot(_) => return rows,
        Press::Install => {
            rows.push(
                menu::Entry::new(
                    menu::Command::InstallRetroArch,
                    crate::i18n::text("shell-download-retroarch"),
                )
                .glyph(icons::LAUNCH),
            );
            rows.push(
                menu::Entry::new(menu::Command::Dismiss, crate::i18n::text("shell-cancel"))
                    .group(1),
            );
            return rows;
        }
        Press::Folder | Press::Enter => {}
    }

    if folder_set {
        rows.push(
            menu::Entry::new(
                menu::Command::RetroArchRescan,
                crate::i18n::text("shell-look-for-new-games"),
            )
            .glyph(icons::REFRESH),
        );
    }
    // Only where there is something to fetch. A row that would answer "there
    // is nothing missing" is a row the user pressed for nothing, and the
    // count is what makes it worth pressing.
    if missing > 0 {
        let label = crate::message!("retroarch-get-missing-emulators", "count" => missing);
        rows.push(menu::Entry::new(menu::Command::RetroArchCores, label));
    }
    rows.push(
        menu::Entry::new(
            menu::Command::ChooseRomsFolder,
            crate::i18n::text("shell-games-folder"),
        )
        .glyph(icons::FILE_FOLDER),
    );
    rows.push(
        menu::Entry::new(
            menu::Command::RetroArchOpen,
            crate::i18n::text("shell-open-retroarch"),
        )
        .glyph(icons::LAUNCH)
        .group(1),
    );
    // Below the rule with RetroArch's own interface, because it is about the
    // program rather than about this machine's collection — and last of the
    // three, because it is the only row here that takes something away. What it
    // takes is spelt out in a panel before anything happens; see
    // `Shell::offer_to_remove_retroarch`.
    rows.push(
        menu::Entry::new(
            menu::Command::RemoveRetroArch,
            crate::i18n::text("shell-remove-retroarch"),
        )
        .glyph(icons::UNINSTALL)
        .group(1),
    );
    rows.push(menu::Entry::new(menu::Command::Dismiss, crate::i18n::text("shell-cancel")).group(1));
    rows
}

/// The rows of the menu raised over one console's row.
///
/// Beside [`menu_rows`] and for the same reason: what a menu offers is a
/// decision, and a decision is a thing to hold shut in a test rather than
/// something only a running session can be asked about.
///
/// Two rows and the way out, and both are about the shelf rather than about any
/// one game on it. `fetching` is whether a run over libretro is already going —
/// the same guard the game's own artwork row keeps, and for the same reason: a
/// second helper asking the same questions would be two processes writing into
/// one cache. `emulated` is whether anything on this machine plays the console.
///
/// Whether the emulator's settings *page* has been built yet is deliberately
/// not asked. The pages are built out of what each core answers when somebody
/// reaches Settings, so on a fresh session there are none — and a row that
/// needed one would be greyed until the user had already walked to where it was
/// going to take them. The press asks for the page and waits for it; see
/// `Shell::open_core_settings`.
pub fn console_menu_rows(fetching: bool, emulated: bool) -> Vec<menu::Entry> {
    let artwork = menu::Entry::new(
        menu::Command::RetroArchConsoleArt,
        crate::i18n::text("shell-get-the-artwork-again"),
    )
    .glyph(icons::REFRESH);
    let settings = menu::Entry::new(
        menu::Command::RetroArchCoreSettings,
        crate::i18n::text("shell-emulator-settings"),
    )
    .glyph(icons::CATEGORY_SETTINGS);
    vec![
        if fetching {
            artwork.disabled()
        } else {
            artwork
        },
        // Greyed rather than absent, unlike the rows of the RetroArch menu
        // above it. That list is a list of states — a machine with no folder
        // has nothing to look through — and this is one row that is true of
        // every console and answerable for most: a shelf whose emulator has not
        // been downloaded yet has settings the moment it has, and a row that
        // vanished and came back would be a menu that changed shape under
        // somebody's hand.
        if emulated {
            settings
        } else {
            settings.disabled()
        },
        menu::Entry::new(menu::Command::Dismiss, crate::i18n::text("shell-cancel")).group(1),
    ]
}

/// What the file naming the controllers is called.
///
/// In RetroArch's own configuration directory, which is where it has to be: a
/// flatpak sees this machine's disk through a sandbox, and the paths that mean
/// the same thing on both sides of it are the ones under there. Named for the
/// shell so that whoever finds it knows who wrote it.
const CONTROLLERS: &str = "lxb-controllers.cfg";

/// Where that file is, on a machine that has a RetroArch to read it.
pub fn controllers_file(config: &Path) -> PathBuf {
    config.join(CONTROLLERS)
}

/// Tell RetroArch which controller is which player and where its buttons are,
/// and write it down.
///
/// **Every time a game starts**, because it is only true at that moment: pads
/// are plugged in and taken away, Steam mirrors them as it pleases, and which
/// one somebody has picked up is a fact about the last few seconds.
///
/// ## Which pad is which player
///
/// One line per player — the *index* of a device in the order RetroArch will
/// enumerate them, which [`crate::pads`] works out. Not a reservation, which
/// is RetroArch's own way of saying this and is the wrong one here: a
/// reservation names a device by name or by ids, and the two controllers this
/// most needs to tell apart are a grabbed pad and the copy of it standing in
/// its place, which agree on both. And with no reservation anywhere in the
/// file, RetroArch leaves these indices exactly as they are written — its own
/// rule, and the reason this works at all.
///
/// ## Where its buttons are
///
/// RetroArch keeps a list of the pads it has heard of, and a pad that is not
/// on it is not configured *at all* — every button on it dead, whatever port
/// it is on. Steam's own virtual controllers are not on it, and this shell
/// puts one of those in front of a game every time somebody plays through
/// Steam Input, so a machine can very easily have four controllers on it and
/// nothing that works.
///
/// So the shell says where the buttons are. It knows, for pads on the *other*
/// list — SDL's, which GilRs carries and which has the pads RetroArch's is
/// missing — and the numbers it writes are the ones RetroArch counts in: a
/// button's place in the device's own list of buttons, an axis's place in its
/// list of axes with the hats left out, and a hat named as a hat. See
/// [`crate::pads::Pad::button`] and [`crate::pads::Pad::axis`].
///
/// A pad neither list knows gets its player line and nothing else, which
/// leaves RetroArch to guess exactly as it does today. A pad only *RetroArch*
/// knows gets the same, because these lines are read after its own answer and
/// would overrule it.
///
/// ## Why nothing here is kept
///
/// The file turns saving-on-exit off, and that is not incidental. RetroArch
/// writes its settings back over its configuration when it closes, and it
/// makes no distinction between a setting somebody chose and a line appended
/// on the way in — so without this, the pad order and the button numbers of
/// one launch would be written into that person's own settings and stand
/// there for every launch afterwards, including the ones this shell knows
/// nothing about. Save files, save states, playlists and each core's own
/// options are written elsewhere and are unaffected; what is given up is a
/// setting changed inside RetroArch during a game the shell started.
///
/// Returns whether anything could be written; a machine with no controller
/// writes an empty file rather than none, so that a stale one from an earlier
/// game cannot go on speaking for pads that have been unplugged since.
pub fn controllers(
    config: &Path,
    pads: &[crate::pads::Pad],
    order: &[usize],
    mappings: &[Option<crate::pads::Mapping>],
) -> bool {
    let out = controller_file(pads, order, mappings);
    let at = controllers_file(config);
    if let Some(parent) = at.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::write(&at, out) {
        Ok(()) => true,
        Err(err) => {
            tracing::warn!(at = %at.display(), %err, "the controllers could not be written");
            false
        }
    }
}

/// What goes in that file. Split from writing it so that what it says can be
/// read back without a disk.
fn controller_file(
    pads: &[crate::pads::Pad],
    order: &[usize],
    mappings: &[Option<crate::pads::Mapping>],
) -> String {
    let mut out = String::new();
    out.push_str("# Which controller is which player, and where its buttons\n");
    out.push_str("# are, written by LineXinBar when it started a game and\n");
    out.push_str("# rewritten every time it does: what is plugged in, and\n");
    out.push_str("# which of it somebody has picked up, are facts about that\n");
    out.push_str("# moment.\n");
    out.push_str("#\n");
    out.push_str("# Nothing here is a setting anybody chose, and nothing is\n");
    out.push_str("# lost by deleting it — the next game writes it again.\n");
    out.push_str("\n# Which is why none of it is kept: RetroArch writes its\n");
    out.push_str("# settings back when it closes, and these are not settings.\n");
    out.push_str("config_save_on_exit = \"false\"\n");
    // How many player ports there are to fill, which is how many controllers
    // were handed over and no more. Every one of them is named below, so this
    // is what shuts the door: a port past the end of this list is a port
    // RetroArch would fill by itself, out of everything on the machine — the
    // grabbed originals, and the controllers Steam Input invents, which
    // [`crate::pads::order`] has just gone to the trouble of leaving out.
    let handed = order.iter().filter(|at| pads.get(**at).is_some()).count();
    out.push_str("\n# And no ports past the ones named here, so that nothing\n");
    out.push_str("# else on this machine can be picked up to fill one.\n");
    out.push_str(&format!("input_max_users = \"{}\"\n", handed.max(1)));
    for (player, at) in order.iter().enumerate() {
        let Some(pad) = pads.get(*at) else { continue };
        // The name is a comment and nothing else: what RetroArch reads is the
        // number. It is here because a file of bare indices is unreadable by
        // the person most likely to be reading it, who is trying to work out
        // why their controller does nothing.
        out.push_str(&format!(
            "\n# player {}: {}\n",
            player + 1,
            comment(&pad.name)
        ));
        out.push_str(&format!(
            "input_player{}_joypad_index = \"{at}\"\n",
            player + 1
        ));
        if let Some(Some(mapping)) = mappings.get(*at) {
            out.push_str(&binds(player + 1, pad, mapping));
        }
    }
    out
}

/// What RetroArch calls each of a pad's controls.
///
/// Its own names are a SNES pad's: the button under the thumb is `b` and the
/// one to the right of it is `a`, which is neither what an Xbox pad says on it
/// nor what a PlayStation pad does. Naming them by *place* is the only way
/// across — see [`crate::pads::Control`] — and it is why this table is short
/// and boring rather than a list of exceptions.
const NAMED: [(crate::pads::Control, &str); 12] = {
    use crate::pads::Control::*;
    [
        (South, "b"),
        (East, "a"),
        (West, "y"),
        (North, "x"),
        (LeftBumper, "l"),
        (RightBumper, "r"),
        (LeftTrigger, "l2"),
        (RightTrigger, "r2"),
        (Select, "select"),
        (Start, "start"),
        (LeftStick, "l3"),
        (RightStick, "r3"),
    ]
};

/// The two ends of each stick, and what RetroArch calls them.
const STICKS: [(crate::pads::Control, &str); 4] = {
    use crate::pads::Control::*;
    [
        (LeftX, "l_x"),
        (LeftY, "l_y"),
        (RightX, "r_x"),
        (RightY, "r_y"),
    ]
};

/// The lines that say where one controller's controls are.
///
/// A control the pad does not have, or one whose number cannot be worked out,
/// is left out rather than guessed: an absent line leaves RetroArch to answer
/// for that button itself, and a wrong one puts somebody's jump on the button
/// beside the one they are pressing.
fn binds(player: usize, pad: &crate::pads::Pad, mapping: &crate::pads::Mapping) -> String {
    use crate::pads::At;

    let mut out = String::new();
    let mut line = |what: &str, how: &str, value: String| {
        out.push_str(&format!(
            "input_player{player}_{what}_{how} = \"{value}\"\n"
        ));
    };

    for (control, name) in NAMED {
        match mapping.at(control) {
            Some(At::Key(code)) => {
                if let Some(number) = pad.button(code) {
                    line(name, "btn", number.to_string());
                }
            }
            // A trigger is usually an axis rather than a button, and RetroArch
            // takes one under a different name. Which way round a given pad
            // reports it is the pad's business, not this shell's.
            Some(At::Axis(code)) => {
                if let Some(number) = pad.axis(code) {
                    line(name, "axis", format!("+{number}"));
                }
            }
            None => {}
        }
    }

    for (control, name) in STICKS {
        let Some(At::Axis(code)) = mapping.at(control) else {
            continue;
        };
        let Some(number) = pad.axis(code) else {
            continue;
        };
        // Which way is which: RetroArch's *plus* is right and down, which is
        // the direction the kernel's own numbers grow in, so the two agree and
        // neither end needs turning round.
        line(&format!("{name}_plus"), "axis", format!("+{number}"));
        line(&format!("{name}_minus"), "axis", format!("-{number}"));
    }

    out.push_str(&dpad(player, pad, mapping));
    out
}

/// The four lines that say where the D-pad is.
///
/// A D-pad is one of two things and never both: a *hat*, which is a pair of
/// absolute axes with three positions each, or four ordinary buttons. RetroArch
/// names a hat's four directions in a form of its own — `h0up` and its three
/// neighbours — so which of the two this pad is has to be answered before
/// anything can be written at all. The hat is looked for first because it is
/// what a pad that has both reports as, and what RetroArch's own list says for
/// every such pad.
fn dpad(player: usize, pad: &crate::pads::Pad, mapping: &crate::pads::Mapping) -> String {
    use crate::pads::{At, Control};

    let mut out = String::new();
    let axis = |control| match mapping.at(control) {
        Some(At::Axis(code)) => crate::pads::hat(code),
        _ => None,
    };
    if let (Some((across, true)), Some((down, false))) =
        (axis(Control::DPadX), axis(Control::DPadY))
    {
        if across == down {
            for (way, name) in [
                ("up", "up"),
                ("down", "down"),
                ("left", "left"),
                ("right", "right"),
            ] {
                out.push_str(&format!(
                    "input_player{player}_{name}_btn = \"h{across}{way}\"\n"
                ));
            }
            return out;
        }
    }

    for (control, name) in [
        (Control::DPadUp, "up"),
        (Control::DPadDown, "down"),
        (Control::DPadLeft, "left"),
        (Control::DPadRight, "right"),
    ] {
        let Some(At::Key(code)) = mapping.at(control) else {
            continue;
        };
        let Some(number) = pad.button(code) else {
            continue;
        };
        out.push_str(&format!("input_player{player}_{name}_btn = \"{number}\"\n"));
    }
    out
}

/// A device's name, made safe to put in a comment: one line, and no quotes to
/// end the string early on a parser reading the line above.
fn comment(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_control() && *c != '"')
        .take(64)
        .collect()
}

/// An argv as one command line, quoted so that a path with a space in it is
/// still one argument.
///
/// The shell's own launcher takes a command line rather than an argv — it is
/// what a `.desktop` file carries — so the one place an argv has to become one
/// is here, and it is single-quoted, which is the one quoting in `sh` with no
/// escapes inside it at all.
pub fn shell_command(argv: &[String]) -> String {
    argv.iter()
        .map(|word| format!("'{}'", word.replace('\'', r"'\''")))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
pub fn plural<'a>(count: usize, one: &'a str, many: &'a str) -> &'a str {
    if count == 1 {
        one
    } else {
        many
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pads::{At, Control, Mapping, Pad};

    #[test]
    fn local_helper_precedes_path_but_not_explicit_override() {
        use std::os::unix::fs::PermissionsExt;
        let dir =
            std::env::temp_dir().join(format!("lxb-helper-resolution-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sibling = dir.join(HELPER);
        let explicit = dir.join("custom-helper");
        for at in [&sibling, &explicit] {
            std::fs::write(at, "fixture").unwrap();
            std::fs::set_permissions(at, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let shell = dir.join("lxb-desktop");
        let fallback = || Some(PathBuf::from("/installed/lxb-retroarch"));
        assert_eq!(
            resolve_helper(None, Some(&shell), fallback),
            Some(sibling.clone())
        );
        assert_eq!(
            resolve_helper(Some(explicit.clone()), Some(&shell), fallback),
            Some(explicit)
        );
        assert_eq!(
            resolve_helper(Some(dir.join("missing")), Some(&shell), fallback),
            None
        );
        std::fs::set_permissions(&sibling, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(resolve_helper(None, Some(&shell), fallback), fallback());
        std::fs::remove_file(&sibling).unwrap();
        assert_eq!(resolve_helper(None, Some(&shell), fallback), fallback());
        assert_eq!(resolve_helper(None, None, || None), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The controller Steam Input puts in front of a game, as this machine
    /// really reports it: an Xbox pad's buttons and axes, and a hat for the
    /// D-pad.
    fn steam_pad() -> Pad {
        Pad {
            node: PathBuf::from("/dev/input/event262"),
            sysfs: "/devices/virtual/input/input339/event262".to_string(),
            name: "Microsoft X-Box 360 pad 1".to_string(),
            vendor: 0x28de,
            product: 0x11ff,
            keys: vec![
                0x130, 0x131, 0x133, 0x134, 0x136, 0x137, 0x13a, 0x13b, 0x13c, 0x13d, 0x13e,
            ],
            axes: vec![0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x10, 0x11],
        }
    }

    /// And what the mapping database says its controls are — captured from
    /// GilRs on this machine, for the pad above. Note which way round the two
    /// middle face buttons are: the *top* one is `0x134`, which is what makes
    /// this a pad of Microsoft's shape rather than Sony's.
    fn steam_mapping() -> Mapping {
        Mapping::new(vec![
            (Control::South, At::Key(0x130)),
            (Control::East, At::Key(0x131)),
            (Control::North, At::Key(0x134)),
            (Control::West, At::Key(0x133)),
            (Control::LeftBumper, At::Key(0x136)),
            (Control::RightBumper, At::Key(0x137)),
            (Control::LeftTrigger, At::Axis(0x02)),
            (Control::RightTrigger, At::Axis(0x05)),
            (Control::Select, At::Key(0x13a)),
            (Control::Start, At::Key(0x13b)),
            (Control::LeftStick, At::Key(0x13d)),
            (Control::RightStick, At::Key(0x13e)),
            (Control::LeftX, At::Axis(0x00)),
            (Control::LeftY, At::Axis(0x01)),
            (Control::RightX, At::Axis(0x03)),
            (Control::RightY, At::Axis(0x04)),
            (Control::DPadX, At::Axis(0x10)),
            (Control::DPadY, At::Axis(0x11)),
        ])
    }

    /// Every line of it is a line out of the profile RetroArch itself ships
    /// for a pad of this shape — `Microsoft X-Box 360 pad`, which is the one
    /// it will not use here, because the device Steam presents is not the
    /// device that profile is for. Getting the same answer from the other side
    /// is the whole point of writing them.
    #[test]
    fn a_pad_of_microsofts_shape_is_written_the_way_retroarch_writes_it() {
        let written = binds(1, &steam_pad(), &steam_mapping());
        assert_eq!(
            written,
            "\
input_player1_b_btn = \"0\"
input_player1_a_btn = \"1\"
input_player1_y_btn = \"2\"
input_player1_x_btn = \"3\"
input_player1_l_btn = \"4\"
input_player1_r_btn = \"5\"
input_player1_l2_axis = \"+2\"
input_player1_r2_axis = \"+5\"
input_player1_select_btn = \"6\"
input_player1_start_btn = \"7\"
input_player1_l3_btn = \"9\"
input_player1_r3_btn = \"10\"
input_player1_l_x_plus_axis = \"+0\"
input_player1_l_x_minus_axis = \"-0\"
input_player1_l_y_plus_axis = \"+1\"
input_player1_l_y_minus_axis = \"-1\"
input_player1_r_x_plus_axis = \"+3\"
input_player1_r_x_minus_axis = \"-3\"
input_player1_r_y_plus_axis = \"+4\"
input_player1_r_y_minus_axis = \"-4\"
input_player1_up_btn = \"h0up\"
input_player1_down_btn = \"h0down\"
input_player1_left_btn = \"h0left\"
input_player1_right_btn = \"h0right\"
"
        );
    }

    /// The guide button is never written, on any pad. It is the way out of
    /// whatever is in front, and an emulator that had a binding for it would
    /// answer the one press that is not an application's to answer.
    #[test]
    fn the_guide_button_is_not_handed_over() {
        let written = binds(1, &steam_pad(), &steam_mapping());
        assert!(!written.contains("menu_toggle"), "{written}");
        assert!(
            !written.contains("\"8\""),
            "the guide button's own number: {written}"
        );
    }

    /// Two middle face buttons the other way round — a pad of Sony's shape,
    /// where `0x133` is the top one. Same device, same numbers, and the two
    /// binds that name them swap, which is exactly the mistake that reading
    /// the codes without a database makes.
    #[test]
    fn a_pad_of_sonys_shape_swaps_the_two_it_should() {
        let mapping = Mapping::new(vec![
            (Control::South, At::Key(0x130)),
            (Control::East, At::Key(0x131)),
            (Control::North, At::Key(0x133)),
            (Control::West, At::Key(0x134)),
        ]);
        let written = binds(1, &steam_pad(), &mapping);
        assert!(
            written.contains("input_player1_x_btn = \"2\"\n"),
            "{written}"
        );
        assert!(
            written.contains("input_player1_y_btn = \"3\"\n"),
            "{written}"
        );
    }

    /// A D-pad that is four buttons rather than a hat, which is how several
    /// pads report one. The numbers are the buttons' own, and no hat is named.
    #[test]
    fn a_d_pad_of_buttons_is_written_as_buttons() {
        let mut pad = steam_pad();
        pad.keys.extend([0x220, 0x221, 0x222, 0x223]);
        pad.axes = vec![0x00, 0x01, 0x02, 0x03, 0x04, 0x05];
        let mapping = Mapping::new(vec![
            (Control::DPadUp, At::Key(0x220)),
            (Control::DPadDown, At::Key(0x221)),
            (Control::DPadLeft, At::Key(0x222)),
            (Control::DPadRight, At::Key(0x223)),
        ]);
        assert_eq!(
            dpad(1, &pad, &mapping),
            "\
input_player1_up_btn = \"11\"
input_player1_down_btn = \"12\"
input_player1_left_btn = \"13\"
input_player1_right_btn = \"14\"
"
        );
    }

    /// The file names every pad's player, says where the buttons are on the
    /// ones a database knows, and leaves the others to the emulator.
    #[test]
    fn the_file_says_which_pad_is_which_player() {
        let pads = vec![steam_pad(), steam_pad()];
        let mappings = vec![None, Some(steam_mapping())];
        let written = controller_file(&pads, &[1, 0], &mappings);
        assert!(
            written.contains("input_player1_joypad_index = \"1\"\n"),
            "{written}"
        );
        assert!(
            written.contains("input_player2_joypad_index = \"0\"\n"),
            "{written}"
        );
        assert!(
            written.contains("input_player1_b_btn = \"0\"\n"),
            "{written}"
        );
        assert!(
            !written.contains("input_player2_b_btn"),
            "a pad no database knows is left to the emulator: {written}"
        );
    }

    /// And none of it is kept. RetroArch writes its settings back over its own
    /// configuration when it closes and cannot tell a setting somebody chose
    /// from a line appended on the way in, so a pad order from one launch
    /// would otherwise stand for every launch afterwards.
    #[test]
    fn nothing_in_the_file_is_written_into_anybodys_settings() {
        let written = controller_file(&[steam_pad()], &[0], &[None]);
        assert!(
            written.contains("config_save_on_exit = \"false\"\n"),
            "{written}"
        );
    }

    /// An integration whose helper has already answered, with the games it
    /// found. The worker's own end of both channels is dropped: nothing here
    /// asks it anything.
    fn found(consoles: Vec<Console>) -> RetroArch {
        let (ask, _) = std::sync::mpsc::channel::<Ask>();
        let (_, heard) = std::sync::mpsc::channel::<Heard>();
        RetroArch {
            inner: Some(Inner {
                ask,
                heard,
                found: Found::Here(Installation {
                    command: vec!["retroarch".to_string()],
                    version: Some("1.22.2".to_string()),
                }),
                installing: None,
                fetching: None,
                picturing: None,
                picturing_about: None,
                pictured: None,
                fetches_cores: true,
                config: None,
                scanned: None,
                consoles,
                unreadable: None,
                reading: false,
                asked: None,
                wants_options: false,
                broken: None,
                system: None,
            }),
        }
    }

    fn console(title: &str, games: &[&str]) -> Console {
        Console {
            title: title.to_string(),
            glyph: Some("lxb:console-psp".to_string()),
            core: Some(Core {
                name: "mesen".to_string(),
                path: "/app/lib/libretro/ppsspp_libretro.so".to_string(),
            }),
            wanted: vec!["ppsspp".to_string()],
            incomplete: false,
            needs: Vec::new(),
            roms: games
                .iter()
                .map(|title| Rom {
                    title: (*title).to_string(),
                    path: format!("/home/x/ROMs/psp/{title}.iso"),
                    boxart: None,
                    snap: None,
                })
                .collect(),
            shape: None,
        }
    }

    /// A game whose pictures the helper has found carries them onto the row,
    /// where the column and the display behind it read them.
    #[test]
    fn a_game_with_pictures_carries_them_onto_its_row() {
        let mut console = console("PlayStation Portable", &["Tekken 6"]);
        console.roms[0].boxart = Some("/cache/box.png".to_string());
        console.roms[0].snap = Some("/cache/snap.png".to_string());
        let integration = found(vec![console]);
        let rows = integration.rows(None);
        let Entry::Folder(folder) = &rows[0] else {
            panic!("a console opens a column");
        };
        let Entry::Rom(rom) = &folder.entries[0] else {
            panic!("a console's rows are games");
        };
        assert_eq!(rom.boxart.as_deref(), Some(Path::new("/cache/box.png")));
        assert_eq!(rom.snap.as_deref(), Some(Path::new("/cache/snap.png")));
    }

    /// The menu over a console's row: the shelf, and the emulator that plays
    /// it.
    ///
    /// Nothing on it acts on a file. A console is a folder somebody sorted
    /// their games into, and a shell offering to rename or delete it from a row
    /// that stands for a machine would be offering to take a machine off the
    /// bar by deleting a directory.
    #[test]
    fn a_console_is_offered_its_shelf_and_its_emulator() {
        let rows = console_menu_rows(false, true);
        let labels: Vec<&str> = rows.iter().map(|row| row.label.as_str()).collect();
        assert_eq!(
            labels,
            ["Get the artwork again", "Emulator settings", "Cancel"]
        );
        assert!(rows.iter().all(|row| row.enabled));
        assert_eq!(
            rows.iter().map(|row| row.group).collect::<Vec<u8>>(),
            [0, 0, 1],
            "the rule falls between the shelf and the way out"
        );
        for row in &rows {
            assert!(
                !matches!(row.command, menu::Command::Delete | menu::Command::Rename),
                "a console is not a file"
            );
        }
    }

    /// And the two states either row can be greyed in, which are two quite
    /// different facts about the machine.
    #[test]
    fn a_console_says_which_of_its_two_rows_cannot_be_pressed() {
        // A run over libretro already going is one helper; a second would be
        // two processes writing into one cache.
        let fetching = console_menu_rows(true, true);
        assert_eq!(fetching[0].command, menu::Command::RetroArchConsoleArt);
        assert!(!fetching[0].enabled);
        assert!(fetching[1].enabled, "which says nothing about its settings");

        // And a console nothing on this machine plays has no emulator to be
        // set. The row stays where it is rather than going, so the menu does
        // not change shape when the download lands.
        let waiting = console_menu_rows(false, false);
        assert_eq!(waiting[1].command, menu::Command::RetroArchCoreSettings);
        assert!(!waiting[1].enabled);
        assert!(waiting[0].enabled, "its artwork can still be asked for");
    }

    /// A kept picture's name says which game it is for and which of the two
    /// pictures it is — read from the end, because a game's own name may hold
    /// as many dots as it likes.
    #[test]
    fn a_kept_pictures_name_says_what_it_is() {
        assert_eq!(
            names("Tekken 5 (USA).iso.cover.png"),
            Some(("Tekken 5 (USA).iso".to_string(), Piece::Cover))
        );
        assert_eq!(
            names("Metroid.nes.background.jpg"),
            Some(("Metroid.nes".to_string(), Piece::Background))
        );
        // A game whose own name ends the way one of these does. The two on the
        // end are the only ones this wrote, so the rest is the game.
        assert_eq!(
            names("Zelda.cover.png.cover.webp"),
            Some(("Zelda.cover.png".to_string(), Piece::Cover))
        );
        for other in [
            "notes.txt",
            "Tekken.iso.snapshot.png",
            ".cover.png",
            "cover.png",
        ] {
            assert_eq!(names(other), None, "{other} is not one of these");
        }
    }

    /// A picture somebody chose stands where libretro's would, and stays there
    /// through every fetch afterwards.
    ///
    /// The whole point of the row. libretro has a cover for nearly everything
    /// anybody owns, and the two cases it has none for are a game whose name is
    /// too far from the database's for a match and a game whose published cover
    /// is not the edition somebody has. A run that put the published picture
    /// back over theirs would be the shell overruling a choice on their behalf.
    #[test]
    fn a_picture_of_your_own_stands_where_libretros_would() {
        let _held = GLOBALS.lock().unwrap_or_else(|err| err.into_inner());
        let at = pictures("chosen");
        set_kept_at(Some(at.clone()));

        let mut console = console("PlayStation 2", &["Tekken 5 (USA)"]);
        console.roms[0].path = "/home/x/ROMs/ps2/Tekken 5 (USA).iso".to_string();
        console.roms[0].boxart = Some("/cache/libretro/tekken.png".to_string());
        console.roms[0].snap = Some("/cache/libretro/tekken-snap.png".to_string());

        // Before anything is chosen, both pictures are libretro's.
        let libretro = pictures_for(&chosen_for("PlayStation 2"), &console.roms[0]);
        assert_eq!(
            libretro.cover.as_deref(),
            Some(Path::new("/cache/libretro/tekken.png"))
        );
        assert_eq!(
            libretro.background.as_deref(),
            Some(Path::new("/cache/libretro/tekken-snap.png"))
        );

        // Somebody chooses their own cover for it.
        let theirs = cover(&at, "mine", 512, 700);
        let kept = keep_picture(
            "PlayStation 2",
            Path::new(&console.roms[0].path),
            Piece::Cover,
            Path::new(&theirs),
        )
        .expect("it is kept");
        reread_chosen_pictures();

        let now = pictures_for(&chosen_for("PlayStation 2"), &console.roms[0]);
        assert_eq!(
            now.cover.as_deref(),
            Some(kept.as_path()),
            "theirs, not libretro's"
        );
        assert_eq!(
            now.background.as_deref(),
            Some(Path::new("/cache/libretro/tekken-snap.png")),
            "and the picture they said nothing about is untouched"
        );

        set_kept_at(None);
        let _ = std::fs::remove_dir_all(&at);
    }

    /// Choosing a second picture replaces the first, whatever it was called.
    ///
    /// A PNG chosen over a JPEG would otherwise be two answers to one question
    /// sitting in one directory, and which of them the row wore would be
    /// whichever `read_dir` happened to hand back first.
    #[test]
    fn choosing_a_second_picture_replaces_the_first() {
        let _held = GLOBALS.lock().unwrap_or_else(|err| err.into_inner());
        let at = pictures("replaced");
        set_kept_at(Some(at.clone()));
        let game = Path::new("/home/x/ROMs/nes/Metroid.nes");

        let first = cover(&at, "first", 300, 400);
        keep_picture("NES", game, Piece::Cover, Path::new(&first)).expect("the first");
        let second = at.join("second.jpg");
        std::fs::copy(&first, &second).expect("a second picture");
        let kept = keep_picture("NES", game, Piece::Cover, &second).expect("the second");
        reread_chosen_pictures();

        let held = chosen_for("NES");
        assert_eq!(held.len(), 1, "one answer to one question: {held:?}");
        assert_eq!(held[0].at, kept);
        assert_eq!(held[0].game, "Metroid.nes");

        // And the game's *other* picture is not touched by either of them.
        keep_picture("NES", game, Piece::Background, Path::new(&first)).expect("a background");
        reread_chosen_pictures();
        let held = chosen_for("NES");
        assert_eq!(held.len(), 2, "the two are different questions: {held:?}");

        set_kept_at(None);
        let _ = std::fs::remove_dir_all(&at);
    }

    /// While a picture is being chosen for a game, that game's own row is what
    /// the walk hangs off — and every other row of the shelf is untouched.
    ///
    /// The whole of why the picker is opened this way. A row with a place on it
    /// opens a column of what is on the disk, one folder at a time, and the row
    /// it hangs off stays on screen to the left of the walk — which is what
    /// **Custom** does under Wallpaper. Built here rather than pushed into the
    /// tree by the press, because the tree is rebuilt from this every time one
    /// of a hundred covers lands: a row inserted from outside would be swept
    /// away with somebody three folders into a walk that was hanging off it.
    #[test]
    fn the_game_being_given_a_picture_is_the_row_the_walk_hangs_off() {
        let mut console = console("PlayStation 2", &["Tekken 5", "Tekken Tag"]);
        console.roms[0].path = "/roms/ps2/Tekken 5.iso".to_string();
        console.roms[1].path = "/roms/ps2/Tekken Tag.iso".to_string();
        let integration = found(vec![console]);

        // Nobody choosing: every row is a game.
        let shelf = |rows: &[Entry]| match &rows[0] {
            Entry::Folder(folder) => folder.entries.clone(),
            _ => panic!("a console opens a column"),
        };
        let quiet = shelf(&integration.rows(None));
        assert!(quiet.iter().all(|row| row.rom().is_some()));

        let asked = Path::new("/roms/ps2/Tekken 5.iso");
        let walking = shelf(&integration.rows(Some((asked, Piece::Background))));
        let Entry::Folder(row) = &walking[0] else {
            panic!("the game being given a picture opens a column");
        };
        assert_eq!(row.title, "Tekken 5", "the same row, wearing the same name");
        assert_eq!(row.comment.as_deref(), Some(Piece::Background.about()));
        assert_eq!(
            row.place
                .as_ref()
                .map(crate::files::Place::shows)
                .and_then(crate::files::Shows::piece),
            Some(Piece::Background),
            "and it opens the walk that is choosing that picture"
        );
        assert!(
            row.entries.is_empty(),
            "what is on somebody's disk is read on the press that opens it"
        );

        // And the game beside it is a game still: one row changes, and the
        // shelf a walk is hanging off goes on being a shelf.
        assert!(
            walking[1].rom().is_some(),
            "the game beside it is untouched"
        );
        assert_eq!(walking.len(), quiet.len(), "no row was added or taken away");
    }

    /// Taking a chosen picture away leaves the game's other one, and every
    /// other game, exactly as they were.
    ///
    /// The row then goes back to what libretro published, or to its console's
    /// mark where libretro has nothing — which is what it wore before anybody
    /// chose anything. The file *is* the record, so deleting it is the whole of
    /// forgetting the choice.
    #[test]
    fn taking_a_picture_away_leaves_everything_else_alone() {
        let _held = GLOBALS.lock().unwrap_or_else(|err| err.into_inner());
        let at = pictures("dropped");
        set_kept_at(Some(at.clone()));
        let mario = Path::new("/home/x/ROMs/nes/Metroid.nes");
        let other = Path::new("/home/x/ROMs/nes/Super Mario Bros.nes");
        let picture = cover(&at, "any", 300, 400);

        for (game, piece) in [
            (mario, Piece::Cover),
            (mario, Piece::Background),
            (other, Piece::Cover),
        ] {
            keep_picture("NES", game, piece, Path::new(&picture)).expect("kept");
        }
        reread_chosen_pictures();
        assert_eq!(chosen_for("NES").len(), 3);

        assert!(drop_picture("NES", mario, Piece::Cover));
        reread_chosen_pictures();
        let left = chosen_for("NES");
        assert_eq!(left.len(), 2, "one went and no more: {left:?}");
        assert!(
            left.iter()
                .any(|one| one.game == "Metroid.nes" && one.piece == Piece::Background),
            "its background is a separate answer to a separate question"
        );
        assert!(
            left.iter().any(|one| one.game == "Super Mario Bros.nes"),
            "and the game beside it was never asked about"
        );

        // Asked again, there is nothing left to take, and it says so rather
        // than claiming to have done something.
        assert!(!drop_picture("NES", mario, Piece::Cover));

        set_kept_at(None);
        let _ = std::fs::remove_dir_all(&at);
    }

    /// A shelf whose covers are all somebody's own is still a shelf of covers,
    /// and is drawn at their shape.
    ///
    /// The case libretro cannot answer at all: a console it has never published
    /// artwork for. Every row of it wears a picture somebody put there by hand,
    /// and a column of those drawn at the shape a column of *marks* falls back
    /// to would be the one shelf where choosing covers made things worse.
    #[test]
    fn a_shelf_of_chosen_covers_is_measured_off_them() {
        let _held = GLOBALS.lock().unwrap_or_else(|err| err.into_inner());
        let at = pictures("homebrew");
        set_kept_at(Some(at.clone()));

        let mut console = console("Homebrew Box", &["Something"]);
        console.roms[0].path = "/home/x/ROMs/hb/Something.bin".to_string();
        assert_eq!(
            shelf_shape(covers(&console)),
            None,
            "libretro has nothing for it"
        );

        let theirs = cover(&at, "wide", 512, 460);
        keep_picture(
            "Homebrew Box",
            Path::new(&console.roms[0].path),
            Piece::Cover,
            Path::new(&theirs),
        )
        .expect("it is kept");
        reread_chosen_pictures();

        let shape = shelf_shape(covers(&console)).expect("their own covers say what shape it is");
        assert!((shape - 1.11).abs() < 0.005, "{shape}");

        set_kept_at(None);
        let _ = std::fs::remove_dir_all(&at);
    }

    /// Somewhere to put pictures, and a picture of a given size in it.
    fn pictures(name: &str) -> PathBuf {
        let at = std::env::temp_dir().join(format!("lxb-covers-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&at);
        std::fs::create_dir_all(&at).expect("a scratch directory");
        at
    }

    fn cover(at: &Path, name: &str, width: u32, height: u32) -> String {
        let file = at.join(format!("{name}.png"));
        image::DynamicImage::new_rgba8(width, height)
            .save_with_format(&file, image::ImageFormat::Png)
            .expect("a picture");
        file.to_string_lossy().into_owned()
    }

    /// A shelf is drawn at the shape of the boxes that console came in, and
    /// that shape is measured off the covers rather than looked up.
    ///
    /// The sizes are libretro's own, off the covers this shell had already
    /// fetched when the shelves were found to be the wrong shape: a Nintendo DS
    /// case is wider than it is tall, and a UMD case is nearly twice as tall as
    /// it is wide. One card shape for both is a card the picture cannot fill on
    /// at least one of them, and what showed on screen was a square cover
    /// floating in a portrait button.
    #[test]
    fn a_shelf_is_drawn_at_the_shape_of_its_own_covers() {
        let at = pictures("shapes");
        for (console, width, height, want) in [
            ("ds", 512, 460, 1.11),
            ("wii", 512, 720, 0.71),
            ("psp", 512, 882, 0.58),
        ] {
            let cover = cover(&at, console, width, height);
            let shape = shelf_shape([cover.as_str()].into_iter()).expect("a measured shelf");
            assert!(
                (shape - want).abs() < 0.005,
                "{console} covers are {width}x{height}, so its shelf is {want} and not {shape}"
            );
        }
        let _ = std::fs::remove_dir_all(&at);
    }

    /// Two scans of the same console's boxes that differ by one pixel are the
    /// same shelf.
    ///
    /// Not a nicety. The covers are relaid out as they arrive, and a shape read
    /// to the last decimal would move every card in the column by a fraction of
    /// a pixel each time one landed — sixty times over a collection coming
    /// down, on rows somebody is scrolling. See [`SHAPE_STEP`].
    #[test]
    fn a_cover_a_pixel_taller_than_its_neighbour_does_not_move_the_shelf() {
        let at = pictures("pixel");
        let one = cover(&at, "tekken-5", 512, 725);
        let two = cover(&at, "tekken-tag", 512, 726);
        assert_eq!(
            shelf_shape([one.as_str()].into_iter()),
            shelf_shape([two.as_str()].into_iter()),
        );
        let _ = std::fs::remove_dir_all(&at);
    }

    /// One picture of the wrong thing does not decide what a shelf looks like.
    ///
    /// libretro's collections are somebody's work and a collection can hold a
    /// banner where a box should be. The middle of what was measured is the
    /// answer, so the odd one out moves it by nothing — where the mean would
    /// have pulled the whole column towards it.
    #[test]
    fn one_cover_of_the_wrong_thing_does_not_decide_the_shelf() {
        let at = pictures("odd");
        let mut covers = vec![cover(&at, "banner", 1600, 200)];
        for game in 0..4 {
            covers.push(cover(&at, &format!("game-{game}"), 512, 726));
        }
        let shape = shelf_shape(covers.iter().map(String::as_str)).expect("a measured shelf");
        assert!(
            (shape - 0.71).abs() < 0.005,
            "the boxes decide it, not {shape}"
        );
        let _ = std::fs::remove_dir_all(&at);
    }

    /// A shelf with nothing to measure has no shape of its own, and says so
    /// rather than answering with a number nobody measured.
    #[test]
    fn a_shelf_with_no_covers_has_no_shape_of_its_own() {
        let at = pictures("none");
        assert_eq!(shelf_shape(std::iter::empty::<&Path>()), None);
        let missing = at.join("never-fetched.png");
        assert_eq!(
            shelf_shape([missing.to_string_lossy().as_ref()].into_iter()),
            None,
            "a path with no picture at the end of it is not a shape"
        );
        let _ = std::fs::remove_dir_all(&at);
    }

    /// The shape goes onto every row of the shelf, including the games nobody
    /// has drawn a cover for.
    ///
    /// The column is laid out from the first row it can ask — see
    /// `ui::cards_in` — so a shelf where the games with covers were one shape
    /// and the games without were another would be two columns interleaved,
    /// with the answer depending on which game happened to be at the top.
    #[test]
    fn every_row_of_a_shelf_carries_the_consoles_shape() {
        let mut console = console("Nintendo DS", &["New Super Mario Bros.", "Homebrew Thing"]);
        console.roms[0].boxart = Some("/cache/ds/mario.png".to_string());
        console.shape = Some(1.11);
        let integration = found(vec![console]);
        let Entry::Folder(folder) = &integration.rows(None)[0] else {
            panic!("a console opens a column");
        };
        for entry in &folder.entries {
            let Entry::Rom(rom) = entry else {
                panic!("a console's rows are games");
            };
            assert_eq!(rom.shape, Some(1.11), "{} is a card of its own", rom.name);
        }
    }

    /// And a cover landing is what tells a shelf what shape it is.
    ///
    /// The whole of the wiring, at the moment somebody sees it: a console whose
    /// covers have never been fetched is a column of marks at the shape a
    /// column of covers has always been, and the first picture to come down is
    /// the console's boxes measuring themselves.
    #[test]
    fn the_first_cover_to_arrive_gives_the_shelf_its_shape() {
        let at = pictures("arriving");
        let (ask, _) = std::sync::mpsc::channel::<Ask>();
        let (back, heard) = std::sync::mpsc::channel::<Heard>();
        let mut console = console("Nintendo DS", &["New Super Mario Bros."]);
        console.roms[0].path = "/home/x/ROMs/nds/mario.nds".to_string();
        let mut retroarch = found(vec![console]);
        let inner = retroarch.inner.as_mut().expect("an installed RetroArch");
        inner.ask = ask;
        inner.heard = heard;

        let shape = |retroarch: &RetroArch| {
            let Entry::Folder(folder) = &retroarch.rows(None)[0] else {
                panic!("a console opens a column");
            };
            let Entry::Rom(rom) = &folder.entries[0] else {
                panic!("a console's rows are games");
            };
            rom.shape
        };
        assert_eq!(shape(&retroarch), None, "nothing has been measured yet");

        let boxart = cover(&at, "mario", 512, 460);
        back.send(Heard::Pictured(
            serde_json::from_str(&format!(
                r#"{{"protocol":{PROTOCOL},"stage":"fetching","console":"nds",
                    "rom":"/home/x/ROMs/nds/mario.nds","at":1,"of":1,
                    "note":"New Super Mario Bros.","boxart":{boxart:?},"snap":null}}"#
            ))
            .expect("the helper's own wire format"),
        ))
        .expect("the worker's channel");
        retroarch.poll();

        let measured = shape(&retroarch).expect("the cover says what shape the shelf is");
        assert!(
            (measured - 1.11).abs() < 0.005,
            "a Nintendo DS box is wider than it is tall: {measured}"
        );
        let _ = std::fs::remove_dir_all(&at);
    }

    /// And one whose pictures nobody has found carries neither, which is a row
    /// wearing the mark it always did and a display keeping the shell's own
    /// wallpaper.
    #[test]
    fn a_game_with_no_pictures_carries_none() {
        let integration = found(vec![console("PlayStation Portable", &["Homebrew Thing"])]);
        let Entry::Folder(folder) = &integration.rows(None)[0] else {
            panic!("a console opens a column");
        };
        let Entry::Rom(rom) = &folder.entries[0] else {
            panic!("a console's rows are games");
        };
        assert!(rom.boxart.is_none() && rom.snap.is_none());
    }

    /// A console wears its own machine's mark, and its games wear it too where
    /// they have no cover.
    ///
    /// Asked with a mark the shell **compiles in**, and that is the whole
    /// reason the name below is not a console's. The console marks arrive with
    /// the integration package, so a test that named one would be a question
    /// about the machine it ran on: the same assertion passed on a machine
    /// without the package installed and failed on one with it, which is not a
    /// test of anything. `icons::shaped` is the same answer everywhere for a
    /// built-in.
    #[test]
    fn a_console_and_its_games_wear_the_machines_own_mark() {
        // Not the mark the fallback would reach for, so that this can tell the
        // two branches apart wherever it runs. On a machine with no integration
        // package installed `mark()` is itself `CATEGORY_GAMES`, and a test
        // asking for that one would pass without proving anything.
        let known = icons::CATEGORY_SETTINGS;
        assert!(icons::shaped(known), "a mark the shell always has");
        assert_ne!(known, mark(), "and not the one it falls back to");

        let mut nintendo = console("Nintendo Entertainment System", &["Metroid"]);
        nintendo.glyph = Some(known.to_string());
        let integration = found(vec![nintendo]);
        let rows = integration.rows(None);
        let Entry::Folder(folder) = &rows[0] else {
            panic!("a console opens a column");
        };
        assert_eq!(folder.icon.as_deref(), Some(known));
        let Entry::Rom(rom) = &folder.entries[0] else {
            panic!("a console's rows are games");
        };
        assert_eq!(rom.glyph, known, "a game with no cover wears it too");
    }

    /// And the half worth holding shut: a name the atlas does not know must
    /// never reach a row.
    ///
    /// The shell shades a quad *by name*, so a name it cannot find comes out as
    /// a pale smear rather than as a mark — see the note on `ui::own_mark`.
    /// This is the case [`console_mark`] exists for: a package installed with
    /// its glyph directory missing, or one console in it that nobody drew.
    #[test]
    fn a_mark_the_atlas_does_not_know_never_reaches_a_row() {
        // No package ships this and none ever will, so the answer is the same
        // on every machine — which is the property the test above lost.
        let unknown = "lxb:console-no-such-machine";
        assert!(!icons::shaped(unknown), "nothing drew this");

        let mut invented = console("No Such Machine", &["Some Game"]);
        invented.glyph = Some(unknown.to_string());
        let integration = found(vec![invented]);
        let Entry::Folder(folder) = &integration.rows(None)[0] else {
            panic!("a console opens a column");
        };
        assert_eq!(folder.icon.as_deref(), Some(mark()));
        let Entry::Rom(rom) = &folder.entries[0] else {
            panic!("a console's rows are games");
        };
        assert_eq!(rom.glyph, mark());
        assert!(!rom.glyph.is_empty(), "a row always has something to wear");
    }

    /// A folder the helper has never heard of has no machine behind it to draw,
    /// and says so rather than borrowing another console's.
    #[test]
    fn a_console_nobody_knows_wears_the_emulators_own_mark() {
        let mut unknown = console("weird-box", &["Something"]);
        unknown.glyph = None;
        let integration = found(vec![unknown]);
        let Entry::Folder(folder) = &integration.rows(None)[0] else {
            panic!("a console opens a column");
        };
        assert_eq!(folder.icon.as_deref(), Some(mark()));
    }

    fn scratch(name: &str) -> PathBuf {
        let at = std::env::temp_dir().join(format!("lxb-bios-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&at);
        std::fs::create_dir_all(at.join("from")).expect("a scratch directory");
        at
    }

    /// A console whose firmware is a whole folder takes every dump in the one
    /// it is pointed at.
    ///
    /// Which is what the PlayStation 2 declares: `pcsx2/bios` and no file name,
    /// because the emulator reads every BIOS it finds in there and lets the
    /// user pick a region in its own menu. Choosing one for them would be this
    /// shell deciding which console somebody meant.
    ///
    /// Every *dump*, and not everything. There is no name to match on here, so
    /// this is the one place a folder chooser could empty somebody's downloads
    /// directory into RetroArch — and the files it put there would then be what
    /// said the console was set up. See [`could_be_firmware`].
    #[test]
    fn a_console_that_wants_a_folder_is_given_every_dump_in_it() {
        let at = scratch("folder");
        let from = at.join("from");
        std::fs::write(from.join("scph39001.bin"), [0x00, 0x78, 0x1a, 0x40]).expect("a dump");
        std::fs::write(from.join("SCPH70004.BIN"), [0xff; 64]).expect("another");
        // The two shapes of thing that are in a real folder beside them, and
        // neither is a boot ROM: a note somebody left themselves, and the
        // archive the dumps came down in.
        std::fs::write(from.join("notes.txt"), b"where these came from").expect("a note");
        std::fs::write(from.join("bios.zip"), b"PK\x03\x04\x00\x00").expect("an archive");

        let into = at.join("system/pcsx2/bios");
        let wanted = [Firmware {
            console: "PlayStation 2".to_string(),
            core: "acore".to_string(),
            note: "'pcsx2/bios' folder".to_string(),
            into: into.clone(),
            here: false,
        }];
        let placed = place(&from, &wanted);
        assert_eq!(placed, 2);
        assert!(into.join("scph39001.bin").is_file());
        assert!(into.join("SCPH70004.BIN").is_file());
        assert!(!into.join("notes.txt").exists(), "not somebody's note");
        assert!(!into.join("bios.zip").exists(), "and not the archive");
        assert!(
            unserved_now(&wanted).is_empty(),
            "and the console is set up"
        );
        let _ = std::fs::remove_dir_all(&at);
    }

    /// A folder with nothing in it that could be a dump leaves the console
    /// exactly as it was.
    ///
    /// The bug this guard was written for, and the half of it a count of copied
    /// files could not see. A declaration that names a whole folder has no name
    /// to match on, so pointing the chooser at a folder of photographs used to
    /// copy the photographs in — and a firmware folder with *something* in it
    /// read as answered. The row said "Added", the warning came off every game
    /// on that console, and the emulator started to a black screen with nothing
    /// anywhere saying why.
    #[test]
    fn a_folder_of_the_wrong_thing_leaves_the_console_wanting() {
        let at = scratch("wrong-thing");
        let from = at.join("from");
        std::fs::write(from.join("holiday.jpg"), b"\xff\xd8\xff\xe0....").expect("a photograph");
        std::fs::write(from.join("where.txt"), b"they are on the other disk").expect("a note");

        let into = at.join("system/pcsx2/bios");
        let wanted = [Firmware {
            console: "PlayStation 2".to_string(),
            core: "acore".to_string(),
            note: "'pcsx2/bios' folder".to_string(),
            into: into.clone(),
            here: false,
        }];
        assert_eq!(place(&from, &wanted), 0, "nothing in there is a dump");
        assert_eq!(
            unserved_now(&wanted).len(),
            1,
            "so the question is still standing"
        );
        let _ = std::fs::remove_dir_all(&at);
    }

    /// One of a console's regional BIOSes is the console's BIOS.
    ///
    /// duckstation declares all three PlayStation regions as required, o2em
    /// four Videopac models, uae4arm six Kickstarts. Nobody owns the set and
    /// nobody has to: a shell that insisted on it would leave "needs its BIOS"
    /// on a console that played perfectly well, and offer a folder chooser that
    /// no folder could ever answer — which is the failure a re-asking panel
    /// turns from a wrong line into a loop.
    #[test]
    fn one_of_a_consoles_regional_dumps_is_enough() {
        let at = scratch("regions");
        let system = at.join("system");
        std::fs::create_dir_all(&system).expect("the folder");
        std::fs::write(system.join("scph5502.bin"), [0u8; 512]).expect("the european one");
        let regions = |name: &str| Firmware {
            console: "PlayStation".to_string(),
            core: "acore".to_string(),
            note: format!("{name} (PS1 BIOS)"),
            into: system.join(name),
            here: false,
        };
        let wanted = [
            regions("scph5500.bin"),
            regions("scph5501.bin"),
            regions("scph5502.bin"),
        ];
        assert!(
            unserved_now(&wanted).is_empty(),
            "the one they have is the one their games run on"
        );

        // And a second job in a second folder is not answered by the first.
        // pcsx2 declares its BIOS folder and a game database under
        // `pcsx2/resources`, and a BIOS is not a database.
        let elsewhere = [
            regions("scph5502.bin"),
            Firmware {
                into: system.join("resources/GameIndex.yaml"),
                ..regions("GameIndex.yaml")
            },
        ];
        assert_eq!(unserved_now(&elsewhere).len(), 1, "the database is missing");
        let _ = std::fs::remove_dir_all(&at);
    }

    /// And a console that names one file gets that file, however it is spelt.
    ///
    /// A dump is `SCPH39001.BIN` as often as `scph39001.bin`, and a core that
    /// declares one of those reads the other. Nothing else in the folder goes
    /// anywhere: the declaration is a file name and this is not a chance to
    /// copy somebody's directory into RetroArch.
    #[test]
    fn a_console_that_names_a_file_gets_that_file_only() {
        let at = scratch("file");
        let from = at.join("from");
        std::fs::write(from.join("DC_BOOT.BIN"), b"dreamcast").expect("a dump");
        std::fs::write(from.join("something_else.bin"), b"not it").expect("another");

        let into = at.join("system/dc/dc_boot.bin");
        let placed = place(
            &from,
            &[Firmware {
                console: "Dreamcast".to_string(),
                core: "acore".to_string(),
                note: "dc/dc_boot.bin (Dreamcast BIOS)".to_string(),
                into: into.clone(),
                here: false,
            }],
        );
        assert_eq!(placed, 1);
        assert_eq!(std::fs::read(&into).expect("the file"), b"dreamcast");
        assert!(
            !into.with_file_name("something_else.bin").exists(),
            "only what was asked for"
        );
        let _ = std::fs::remove_dir_all(&at);
    }

    /// A folder with nothing in it that any console wants places nothing, and
    /// that is what the panel afterwards is for.
    #[test]
    fn a_folder_with_no_bios_in_it_places_nothing() {
        let at = scratch("empty");
        let from = at.join("from");
        std::fs::write(from.join("holiday.jpg"), b"not a bios").expect("a photograph");
        let placed = place(
            &from,
            &[Firmware {
                console: "Dreamcast".to_string(),
                core: "acore".to_string(),
                note: "dc/dc_boot.bin (Dreamcast BIOS)".to_string(),
                into: at.join("system/dc/dc_boot.bin"),
                here: false,
            }],
        );
        assert_eq!(placed, 0, "nothing in there is the file it named");
        let _ = std::fs::remove_dir_all(&at);
    }

    /// The row is on the page whenever the emulator declares a BIOS — before
    /// one is chosen and after.
    ///
    /// The "after" is the part worth holding shut. A row that vanished the
    /// moment it was answered could only be reached again by taking the file
    /// back off the disk, and choosing a BIOS is a thing people get wrong: a
    /// dump for the wrong region, a bad copy, the other console's.
    #[test]
    fn the_bios_row_is_there_before_and_after_it_is_answered() {
        assert!(
            bios_row(&[]).is_none(),
            "an emulator that declares no firmware has no such row"
        );

        let ps2 = |here: bool| Firmware {
            console: "PlayStation 2".to_string(),
            core: "pcsx2".to_string(),
            note: "'pcsx2/bios' folder".to_string(),
            into: PathBuf::from("/system/pcsx2/bios"),
            here,
        };

        let row = bios_row(&[ps2(false)]).expect("a row");
        assert_eq!(row.title(), "PlayStation 2 BIOS", "named for the console");
        assert_eq!(
            row.comment(),
            Some("Not added — choose the folder yours is in"),
            "libretro's line for a folder describes where this shell puts \
             things, which is no help to somebody wondering what they need"
        );
        assert!(
            crate::apps::opens_a_picker(&row),
            "and pressing it asks where one is"
        );

        let answered = bios_row(&[ps2(true)]).expect("still a row");
        assert_eq!(answered.title(), "PlayStation 2 BIOS");
        assert_eq!(
            answered.comment(),
            Some("Added — choose another folder to replace it")
        );
        assert!(crate::apps::opens_a_picker(&answered));
    }

    /// An emulator's settings page wears the mark of the console it plays,
    /// and it is the shelf's own answer rather than a second copy of it.
    ///
    /// Four emulators under one page all wearing RetroArch's own drawing are
    /// four rows told apart only by reading them, which is the thing the
    /// console marks exist to stop. What this pins is where the answer comes
    /// from: a table here would be a second statement of which core plays what,
    /// and the two would drift.
    #[test]
    fn a_cores_page_wears_the_mark_of_the_console_it_plays() {
        let _held = GLOBALS.lock().unwrap_or_else(|err| err.into_inner());
        *CORE_MARKS.lock().unwrap() = vec![
            ("melonds".to_string(), "lxb:console-nds".to_string()),
            ("pcsx2".to_string(), "lxb:console-ps2".to_string()),
        ];

        // The atlas in a unit test has no drawings in it at all, so every one
        // of these falls back — which is the fallback being tested. What is
        // being asserted is that the *lookup* found the right name to try and
        // that an unknown core reaches the fallback by the other road.
        let known = mark_for_core("melonds");
        let unknown = mark_for_core("nothing-plays-this");
        assert_eq!(
            unknown,
            mark(),
            "a core no console names wears RetroArch's own mark"
        );
        if crate::icons::shaped("lxb:console-nds") {
            assert_eq!(
                known, "lxb:console-nds",
                "and a known one wears its console"
            );
            assert_ne!(known, unknown, "which is not the frontend's drawing");
        } else {
            assert_eq!(
                known,
                mark(),
                "a drawing that did not ship falls back rather than reaching                  the atlas as a name nothing drew"
            );
        }

        CORE_MARKS.lock().unwrap().clear();
    }

    /// The row is filed under the emulator that wants it.
    #[test]
    fn one_emulators_bios_is_not_offered_on_anothers_page() {
        let _held = GLOBALS.lock().unwrap_or_else(|err| err.into_inner());
        *FIRMWARE.lock().unwrap() = vec![Firmware {
            console: "PlayStation 2".to_string(),
            core: "pcsx2".to_string(),
            note: "'pcsx2/bios' folder".to_string(),
            into: PathBuf::from("/system/pcsx2/bios"),
            here: false,
        }];
        assert_eq!(firmware_for("pcsx2").len(), 1);
        assert!(
            firmware_for("mesen").is_empty(),
            "a Mesen page must not offer to put a PlayStation 2 BIOS anywhere"
        );
        FIRMWARE.lock().unwrap().clear();
    }

    /// libretro's sentence is shown where it names a file and not where it
    /// names a folder.
    ///
    /// "dc/dc_boot.bin (Dreamcast BIOS)" tells somebody what to look for.
    /// pcsx2's reads `'pcsx2/bios' folder`, which says only where this shell is
    /// about to put things — no help at all to a person wondering what they are
    /// supposed to have, and the panel says the console's name already.
    #[test]
    fn only_a_sentence_naming_a_file_is_worth_showing() {
        let missing = |into: &str| Firmware {
            console: "x".to_string(),
            core: "acore".to_string(),
            note: "y".to_string(),
            into: PathBuf::from(into),
            here: false,
        };
        assert!(missing("/system/dc/dc_boot.bin").worth_saying());
        assert!(!missing("/system/pcsx2/bios").worth_saying());
    }

    /// Silence from the question about core settings is one of its answers, not
    /// a broken integration.
    ///
    /// A RetroArch with no cores installed yet has nothing to say about what
    /// any core can be set to — and that is what every fresh install looks
    /// like. Reading it as a failure put "Not working on this machine" under a
    /// row that was working perfectly well, which is what this holds shut.
    #[test]
    fn only_some_questions_have_to_be_answered() {
        assert!(!Ask::Options.must_answer());
    }

    /// A core that arrives during a session is asked what it can be set to —
    /// and asked when somebody goes to read the answer, not before.
    ///
    /// Two things at once, because they are two halves of one rule. The
    /// question used to be asked once, when the probe answered, and a core
    /// installed after that was one whose page under Settings did not exist
    /// until the shell was started again — which is not a thing anybody would
    /// think to try, and reads as the settings simply not being there. So a
    /// finished fetch makes the answer stale again.
    ///
    /// And it is a question with a price: every installed core is loaded into a
    /// process to be asked. That does not belong at a login, for a screen most
    /// people open twice. So nothing is sent until a bar is standing in
    /// Settings, and nothing is sent twice for the one staleness.
    #[test]
    fn a_core_that_has_just_arrived_is_asked_what_it_can_be_set_to() {
        let (ask, asked) = std::sync::mpsc::channel::<Ask>();
        let (back, heard) = std::sync::mpsc::channel::<Heard>();
        let mut retroarch = RetroArch {
            inner: Some(Inner {
                ask,
                heard,
                found: Found::Here(Installation {
                    command: vec!["retroarch".to_string()],
                    version: None,
                }),
                installing: None,
                fetching: None,
                picturing: None,
                picturing_about: None,
                pictured: None,
                fetches_cores: true,
                config: None,
                scanned: None,
                consoles: Vec::new(),
                unreadable: None,
                system: None,
                reading: false,
                asked: None,
                wants_options: false,
                broken: None,
            }),
        };

        back.send(Heard::Fetching(
            serde_json::from_str(
                r#"{"protocol":1,"stage":"done","core":"play","progress":1.0,"at":1,"of":1,"note":"Everything is ready"}"#,
            )
            .expect("the helper's own wire format"),
        ))
        .expect("the worker's channel");
        retroarch.poll();

        let before: Vec<Ask> = asked.try_iter().collect();
        assert!(
            !before
                .iter()
                .any(|question| matches!(question, Ask::Options)),
            "a fetch must not load every core before anybody has asked to see a \
             settings page, and it asked {} things",
            before.len()
        );

        // Somebody walks to Settings, which is the only screen the answer is
        // for.
        retroarch.settings_reached();
        let questions: Vec<Ask> = asked.try_iter().collect();
        assert!(
            questions
                .iter()
                .any(|question| matches!(question, Ask::Options)),
            "reaching Settings after a fetch has to ask the new core what it offers, and \
             asked {} things",
            questions.len()
        );

        // And walking past Settings again asks nothing: the trigger is looked
        // at on every frame the cursor is in there, and a question sent sixty
        // times a second would load every core on the machine sixty times a
        // second.
        retroarch.settings_reached();
        retroarch.settings_reached();
        let again: Vec<Ask> = asked.try_iter().collect();
        assert!(
            again.is_empty(),
            "nothing has gone stale since, so nothing should have been asked, and {} was",
            again.len()
        );
        assert!(Ask::Probe.must_answer());
        assert!(Ask::Install.must_answer());
        assert!(Ask::Scan(PathBuf::from("/roms")).must_answer());
        assert!(Ask::Cores(vec!["ppsspp".to_string()]).must_answer());
        assert!(Ask::Art {
            roms: PathBuf::from("/roms"),
            only: Vec::new(),
            again: false,
        }
        .must_answer());
    }

    /// The row says how far along a fetch of pictures is, in games rather than
    /// in percentages: what is coming down is a hundred small files, and the
    /// number somebody wants is how many of their games are left.
    #[test]
    fn the_row_counts_the_pictures_rather_than_the_bytes() {
        assert_eq!(
            Pictures { at: 12, of: 40 }.sentence(),
            "Getting the pictures — 12 of 40"
        );
        // Before the helper has said how many there are, which is the frame
        // between the press and its first line.
        assert_eq!(Pictures { at: 0, of: 0 }.sentence(), "Getting the pictures");
    }

    /// Writing one setting leaves every other line of the file exactly as it
    /// was.
    ///
    /// This writes into `retroarch.cfg`, which is a hundred kilobytes of
    /// somebody's own settings. A writer that kept only the lines it
    /// understood would throw the rest away, and nothing would say so until
    /// the next time they opened RetroArch and found it factory fresh.
    #[test]
    fn setting_one_line_keeps_every_other_one() {
        let dir = std::env::temp_dir().join(format!("lxb-cfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let at = dir.join("retroarch.cfg");

        let before = "# a comment nobody parses\n\
                      video_driver = \"gl\"\n\
                      aspect_ratio_index = \"22\"\n\
                      some_setting_this_shell_never_heard_of = \"kept\"\n";
        std::fs::write(&at, before).unwrap();

        assert!(write_key(&at, "aspect_ratio_index", "1"));
        let after = std::fs::read_to_string(&at).unwrap();
        assert_eq!(
            after,
            "# a comment nobody parses\n\
             video_driver = \"gl\"\n\
             aspect_ratio_index = \"1\"\n\
             some_setting_this_shell_never_heard_of = \"kept\"\n",
            "the line changed and nothing else did"
        );
        assert_eq!(read_key(&at, "aspect_ratio_index").as_deref(), Some("1"));
        assert_eq!(
            read_key(&at, "some_setting_this_shell_never_heard_of").as_deref(),
            Some("kept")
        );

        // A key the file has never held is added rather than refused.
        assert!(write_key(&at, "video_vsync", "false"));
        assert_eq!(read_key(&at, "video_vsync").as_deref(), Some("false"));
        assert_eq!(std::fs::read_to_string(&at).unwrap().lines().count(), 5);

        // And a file that is not there yet is one line long afterwards, which
        // is what a core nobody has ever set anything on starts as.
        let fresh = dir.join("PPSSPP.opt");
        assert!(write_key(&fresh, "ppsspp_internal_resolution", "960x544"));
        assert_eq!(
            std::fs::read_to_string(&fresh).unwrap(),
            "ppsspp_internal_resolution = \"960x544\"\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What this shell gives a RetroArch it has just met, and what it leaves
    /// alone.
    ///
    /// RetroArch's own default is OpenGL, and on this bar that is the wrong
    /// answer — a PlayStation 2 drawn through it comes out full of graphical
    /// faults. But it is only a *default*: an emulator whose configuration
    /// already names a driver has been chosen for, by somebody picking one
    /// under Settings or simply by RetroArch having been run once, and this
    /// must not overrule either.
    #[test]
    fn a_fresh_emulator_is_given_vulkan_and_a_chosen_one_is_left_alone() {
        let _held = GLOBALS.lock().unwrap_or_else(|err| err.into_inner());
        let dir = std::env::temp_dir().join(format!("lxb-defaults-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        set_config_at(Some(dir.clone()));
        let at = dir.join("retroarch.cfg");

        // The configuration a fresh flatpak writes: its own paths, and not one
        // word about what to draw with.
        let fresh = "assets_directory = \"/app/share/libretro/assets/\"\n\
                     config_save_on_exit = \"true\"\n";
        std::fs::write(&at, fresh).unwrap();
        assert_eq!(settle_defaults(true), vec![("video_driver", "vulkan")]);
        assert_eq!(read_key(&at, "video_driver").as_deref(), Some("vulkan"));
        assert_eq!(
            read_key(&at, "config_save_on_exit").as_deref(),
            Some("true"),
            "and the rest of the file is untouched"
        );

        // Asked again — a second session, a second probe — it says nothing,
        // because the line is there now.
        assert!(settle_defaults(true).is_empty());

        // A machine that chose OpenGL keeps OpenGL.
        std::fs::write(&at, "video_driver = \"gl\"\n").unwrap();
        assert!(settle_defaults(true).is_empty(), "somebody chose that");
        assert_eq!(read_key(&at, "video_driver").as_deref(), Some("gl"));

        // And a machine with no Vulkan is never told to use it, whatever its
        // configuration says: the emulator would take the line, fail to open a
        // context, and come back with nothing on the screen.
        std::fs::write(&at, fresh).unwrap();
        assert!(settle_defaults(false).is_empty(), "there is no Vulkan here");
        assert_eq!(read_key(&at, "video_driver"), None);

        set_config_at(None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Nothing is left behind when a write cannot be finished.
    #[test]
    fn a_write_that_cannot_land_leaves_no_half_file() {
        let dir = std::env::temp_dir().join(format!("lxb-cfg-part-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A directory where the file should be: the rename cannot land on it.
        let at = dir.join("retroarch.cfg");
        std::fs::create_dir(&at).unwrap();

        assert!(
            !write_key(&at, "video_vsync", "false"),
            "it must not claim to have worked"
        );
        assert!(
            !at.with_extension("lxb-part").exists(),
            "and it must not leave the half-written one behind"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn menu(press: Press, missing: usize, folder: bool) -> Vec<String> {
        menu_rows(&press, missing, folder)
            .into_iter()
            .map(|row| row.label)
            .collect()
    }

    /// A row that cannot say what it would do offers no menu at all.
    ///
    /// Both of these states are the shell not knowing something rather than
    /// the user being unable to do something, and every row this menu has
    /// needs the answer. A panel whose only row is Cancel is a panel that
    /// wasted the press that raised it.
    #[test]
    fn a_row_with_nothing_to_offer_raises_no_menu() {
        assert!(menu_rows(&Press::Waiting, 0, false).is_empty());
        assert!(menu_rows(&Press::Cannot("no flatpak".to_string()), 0, false).is_empty());
    }

    /// Before it is installed there is one thing to do and one way out.
    #[test]
    fn a_machine_without_it_is_offered_it() {
        assert_eq!(
            menu(Press::Install, 0, false),
            vec!["Download RetroArch", "Cancel"]
        );
    }

    /// With no folder chosen there is no collection to look through, so the
    /// row that would look through it again is not there.
    #[test]
    fn there_is_nothing_to_look_through_until_a_folder_is_chosen() {
        assert_eq!(
            menu(Press::Folder, 0, false),
            vec![
                "Games folder",
                "Open RetroArch",
                "Remove RetroArch",
                "Cancel"
            ]
        );
    }

    /// And with one, the whole list.
    #[test]
    fn a_collection_can_be_read_again_and_its_emulators_fetched() {
        assert_eq!(
            menu(Press::Enter, 0, true),
            vec![
                "Look for new games",
                "Games folder",
                "Open RetroArch",
                "Remove RetroArch",
                "Cancel"
            ]
        );
    }

    /// The count is in the row, because it is what makes it worth pressing —
    /// and it is not there at all when there is nothing to fetch.
    #[test]
    fn the_row_that_fetches_emulators_says_how_many() {
        assert!(!menu(Press::Enter, 0, true)
            .iter()
            .any(|row| row.contains("missing")));
        assert!(menu(Press::Enter, 1, true).contains(&"Get the missing emulator".to_string()));
        assert!(menu(Press::Enter, 4, true).contains(&"Get 4 missing emulators".to_string()));
    }

    /// RetroArch's own interface, taking it off the machine, and the way out
    /// are the band below the rule: not one of them is something done to this
    /// machine's collection.
    ///
    /// Removing it is last of the three, and that is the whole of what its
    /// position says: it is the only row on this menu that takes something
    /// away, so it does not sit where a hand falls.
    #[test]
    fn the_emulators_own_screens_are_below_the_rule() {
        let rows = menu_rows(&Press::Enter, 2, true);
        let below: Vec<&str> = rows
            .iter()
            .filter(|row| row.group == 1)
            .map(|row| row.label.as_str())
            .collect();
        assert_eq!(below, vec!["Open RetroArch", "Remove RetroArch", "Cancel"]);
        assert!(rows
            .iter()
            .any(|row| row.command == menu::Command::RetroArchOpen));
    }

    /// Everything read off a RetroArch goes when the RetroArch does.
    ///
    /// Not tidying up. The shelf of games, the emulators' settings pages, the
    /// firmware rows and the console marks are all built out of what the last
    /// scan and the last ask found — so a shell that kept them would go on
    /// drawing a column of games nothing can play, and settings pages for
    /// emulators that are no longer on the machine.
    #[test]
    fn removing_it_forgets_everything_that_was_read_off_it() {
        let _held = GLOBALS.lock().unwrap_or_else(|err| err.into_inner());
        let mut integration = found(vec![console("PlayStation Portable", &["Tekken 6"])]);
        set_firmware(vec![Firmware {
            console: "PlayStation 2".to_string(),
            core: "pcsx2".to_string(),
            note: "'pcsx2/bios' folder".to_string(),
            into: std::path::PathBuf::from("/system/pcsx2/bios"),
            here: false,
        }]);
        set_core_marks(vec![("ppsspp".to_string(), "lxb:console-psp".to_string())]);
        TUNABLES
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .push(CoreOptions {
                core: "ppsspp".to_string(),
                display: "PPSSPP".to_string(),
                categories: Vec::new(),
                options: Vec::new(),
            });
        let consoles = |integration: &RetroArch| {
            integration
                .rows(None)
                .iter()
                .filter(|row| row.title() != "Games folder")
                .count()
        };
        assert_eq!(consoles(&integration), 1, "a shelf to begin with");

        integration.forget();

        assert_eq!(consoles(&integration), 0, "no games left to draw");
        assert!(firmware().is_empty(), "and nothing to ask a BIOS about");
        assert!(tunables().is_empty(), "and no emulator has settings here");
        assert_eq!(
            mark_for_core("ppsspp"),
            mark(),
            "and no console's mark is remembered for a core that has gone"
        );
        set_firmware(Vec::new());
        set_core_marks(Vec::new());
    }

    /// There is nothing to remove until there is something installed.
    ///
    /// The row is on every state that has a menu at all *except* the one where
    /// the press is an offer to download it — a machine with no RetroArch
    /// offering to take RetroArch off is a row that could only ever fail.
    #[test]
    fn a_machine_without_it_is_not_offered_to_remove_it() {
        let removes = |press: Press| {
            menu_rows(&press, 0, false)
                .iter()
                .any(|row| row.command == menu::Command::RemoveRetroArch)
        };
        assert!(!removes(Press::Install), "there is nothing there yet");
        assert!(!removes(Press::Waiting), "and nothing is known yet");
        assert!(!removes(Press::Cannot("no flatpak".to_string())));
        assert!(removes(Press::Folder), "installed, with no folder chosen");
        assert!(removes(Press::Enter), "and installed with one");
    }

    /// The row that asks where the games are is a question, and a question
    /// stops being asked once it has been answered.
    ///
    /// It is what a column with no consoles in it is *made* of — nobody has
    /// chosen a folder, or the folder cannot be read this morning, or there is
    /// nothing in it yet — and the moment there are games it goes, because a
    /// column of somebody's games is not the place to keep a setting. Settings
    /// is, and the same row is there for as long as the package is.
    #[test]
    fn the_folder_row_is_asked_until_it_is_answered() {
        let empty = found(Vec::new());
        let rows = empty.rows(None);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title(), "Games folder");

        let full = found(vec![console("PlayStation Portable", &["Wipeout Pure"])]);
        let rows = full.rows(None);
        assert_eq!(
            rows.iter().map(Entry::title).collect::<Vec<_>>(),
            vec!["PlayStation Portable"],
            "the games, and nothing else"
        );

        // And a folder still being read is a folder with nothing in it yet, so
        // that this column is never empty: an empty one comes off the bar, and
        // it would come off it on the very press that answered the question.
        let mut reading = found(Vec::new());
        reading.inner.as_mut().expect("started").reading = true;
        assert_eq!(
            reading
                .rows(None)
                .iter()
                .map(Entry::title)
                .collect::<Vec<_>>(),
            vec!["Games folder"],
            "the column has to go on existing while the scan runs"
        );
    }

    /// And RetroArch itself is not one of somebody's games.
    ///
    /// Its own entry comes off the bar because the shell's row stands in for
    /// it — see [`crate::apps::hide_retroarch_client`] — and it does not come
    /// back at the foot of this column, where it was the one row that was not a
    /// game.
    #[test]
    fn retroarch_itself_is_not_a_row_of_this_column() {
        let full = found(vec![console("PlayStation Portable", &["Daxter"])]);
        assert!(
            full.rows(None).iter().all(|row| row.app().is_none()),
            "nothing in this column starts a program of its own"
        );
    }

    /// A path with a space, an apostrophe or a dollar in it is still one
    /// argument. Every ROM anybody owns is one of those.
    #[test]
    fn a_command_line_survives_a_name_somebody_actually_used() {
        let argv = vec![
            "flatpak".to_string(),
            "run".to_string(),
            "org.libretro.RetroArch".to_string(),
            "-L".to_string(),
            "/app/lib/libretro/ppsspp_libretro.so".to_string(),
            "/home/p/ROMs/psp/Ridge Racer 2 (Europe) $2.iso".to_string(),
        ];
        let line = shell_command(&argv);
        assert!(line.ends_with(r"'/home/p/ROMs/psp/Ridge Racer 2 (Europe) $2.iso'"));

        let awkward = vec!["/games/Tom's ROM.iso".to_string()];
        assert_eq!(shell_command(&awkward), r"'/games/Tom'\''s ROM.iso'");
    }

    /// A session without the package has no row, no column and nothing to
    /// press.
    #[test]
    fn a_machine_without_the_package_has_no_integration() {
        let absent = RetroArch::absent();
        assert_eq!(absent.note(), None);
        assert!(absent.rows(None).is_empty());
        assert_eq!(absent.press(), Press::Waiting);
    }

    /// A helper a version ahead of this shell is refused rather than half
    /// understood.
    #[test]
    fn a_record_from_another_version_is_not_acted_on() {
        let ahead = Probe {
            protocol: PROTOCOL + 1,
            retroarch: None,
            flatpak: true,
            config: None,
            fetches_cores: true,
        };
        assert!(!ahead.usable());
        let ours = Probe {
            protocol: PROTOCOL,
            retroarch: None,
            flatpak: true,
            config: None,
            fetches_cores: true,
        };
        assert!(ours.usable());
    }

    /// A helper written before cores could be fetched does not say the field,
    /// and a shell talking to one must not offer a download it cannot do.
    ///
    /// This is the whole of why the protocol number did not have to change: an
    /// answer that has grown a field is still the answer, and the field's
    /// absence is itself the fact the shell needs.
    #[test]
    fn an_older_helper_is_not_offered_as_one_that_fetches_cores() {
        let older: Probe = serde_json::from_str(
            r#"{"protocol":1,"retroarch":{"command":["retroarch"],"version":null},"flatpak":true}"#,
        )
        .expect("a helper that predates the verb");
        assert!(older.usable(), "it is still this protocol");
        assert!(!older.fetches_cores);

        let newer: Probe = serde_json::from_str(
            r#"{"protocol":1,"retroarch":null,"flatpak":true,"fetches_cores":true}"#,
        )
        .expect("a helper that has it");
        assert!(newer.fetches_cores);
    }

    /// What is asked for is one item per console that has nothing to play it
    /// with, holding that console's cores best first — and nothing at all for a
    /// collection that is already playable.
    #[test]
    fn only_the_consoles_with_no_core_are_asked_about() {
        let played = Console {
            title: "PlayStation Portable".to_string(),
            glyph: Some("lxb:console-psp".to_string()),
            core: Some(Core {
                name: "mesen".to_string(),
                path: "/app/lib/libretro/ppsspp_libretro.so".to_string(),
            }),
            wanted: vec!["ppsspp".to_string()],
            incomplete: false,
            needs: Vec::new(),
            roms: Vec::new(),
            shape: None,
        };
        let mut waiting = console("Nintendo Entertainment System", &["Metroid"]);
        waiting.core = None;
        waiting.wanted = vec!["mesen".to_string(), "nestopia".to_string()];

        let here = found(vec![played.clone(), waiting]);
        assert_eq!(here.missing_cores(), vec!["mesen,nestopia".to_string()]);

        let playable = found(vec![played.clone()]);
        assert!(playable.missing_cores().is_empty());

        // A core that is on the disk but has not got the folder it reads is
        // asked about too. Nobody can play it, and what it is waiting for is a
        // download — the same answer, for the same reason.
        let mut half = played;
        half.incomplete = true;
        let asked = found(vec![half]);
        assert_eq!(asked.missing_cores(), vec!["ppsspp".to_string()]);
    }

    /// A console is only waiting for the files it has none of.
    ///
    /// duckstation names all three PlayStation BIOS regions, o2em four Videopac
    /// models, uae4arm six Kickstarts; nobody owns the set and nobody has to,
    /// because one of them is what a person's own games run on. What reads this
    /// is the settings row that offers to go and find one, and the panel a game
    /// raises after failing to start — so a shell that counted the other two as
    /// missing would keep offering a chooser no folder could answer.
    #[test]
    fn a_console_is_only_waiting_for_what_it_has_none_of() {
        let at = scratch("regional-rows");
        let system = at.join("system");
        std::fs::create_dir_all(&system).expect("the folder");
        let region = |name: &str, here: bool| Firmware {
            console: "PlayStation".to_string(),
            core: "acore".to_string(),
            note: format!("{name} (PS1 BIOS)"),
            into: system.join(name),
            here,
        };
        let held = [
            region("scph5500.bin", false),
            region("scph5501.bin", false),
            region("scph5502.bin", true),
        ];
        assert!(
            unserved(&held).is_empty(),
            "the one they have is the one their games run on"
        );

        let none = [
            region("scph5500.bin", false),
            region("scph5501.bin", false),
            region("scph5502.bin", false),
        ];
        assert_eq!(unserved(&none).len(), 3, "and a panel would name all three");
        let _ = std::fs::remove_dir_all(&at);
    }

    /// A game's row never says a console will not start.
    ///
    /// It said "needs its BIOS" once, on any console whose core declared a file
    /// this machine had not got — which is the shell deciding in advance what
    /// an emulator will do. melonDS plays most Nintendo DS games with no dump
    /// at all. The row says which console it is, and the game says the rest by
    /// running.
    #[test]
    fn a_game_says_which_console_it_is_and_nothing_about_its_bios() {
        let region = |name: &str| Need {
            note: format!("{name} (PS1 BIOS)"),
            path: name.to_string(),
            here: false,
        };
        let mut console = console("PlayStation", &["Ridge Racer"]);
        console.needs = vec![region("scph5500.bin"), region("scph5501.bin")];
        let integration = found(vec![console]);
        let inner = integration.inner.as_ref().unwrap();
        let Entry::Rom(rom) =
            integration.rom_row(&inner.consoles[0], &inner.consoles[0].roms[0], &[], None)
        else {
            panic!("a game is a rom");
        };
        assert_eq!(rom.note, "PlayStation");
        assert!(rom.start.is_some(), "and the press starts it");
    }

    /// And it reads as one, on the row and on the press.
    ///
    /// The whole of the bug this was written for: PPSSPP without its own
    /// folder starts a game, draws every menu as an empty box, and says `Core
    /// system files are missing` along the bottom — so a row that offered to
    /// play it would be offering something that does not work.
    #[test]
    fn a_core_missing_its_system_files_is_not_offered_as_playable() {
        let mut console = console("PlayStation Portable", &["Tekken 6"]);
        console.core = Some(Core {
            path: "/app/lib/libretro/ppsspp_libretro.so".to_string(),
            name: "ppsspp".to_string(),
        });
        console.wanted = vec!["ppsspp".to_string()];

        let whole = found(vec![console.clone()]);
        let Entry::Folder(row) =
            whole.console_row(&whole.inner.as_ref().unwrap().consoles[0], None)
        else {
            panic!("a console is a folder");
        };
        assert_eq!(row.comment.as_deref(), Some("1 game"));

        console.incomplete = true;
        let half = found(vec![console]);
        let inner = half.inner.as_ref().unwrap();
        let Entry::Folder(row) = half.console_row(&inner.consoles[0], None) else {
            panic!("a console is a folder");
        };
        assert_eq!(row.comment.as_deref(), Some("1 game — needs a download"));

        // And the game itself cannot be started, so the press offers the
        // download rather than an emulator with no letters in it.
        let Entry::Rom(rom) =
            half.rom_row(&inner.consoles[0], &inner.consoles[0].roms[0], &[], None)
        else {
            panic!("a game is a rom");
        };
        assert!(rom.start.is_none(), "it must not offer to play");
        assert_eq!(rom.wanted, vec!["ppsspp".to_string()]);
    }

    /// The sentence a fetch says, in the two shapes it is said in.
    #[test]
    fn a_fetch_says_how_far_along_it_is() {
        let one = Fetching {
            core: "ppsspp".to_string(),
            progress: Some(0.42),
            at: 1,
            of: 1,
            note: String::new(),
            ended: None,
        };
        assert_eq!(one.sentence(), "Getting your games ready — 42%");

        let several = Fetching {
            core: "mesen".to_string(),
            progress: Some(0.42),
            at: 2,
            of: 5,
            note: String::new(),
            ended: None,
        };
        assert_eq!(several.sentence(), "Getting your games ready — 2 of 5");

        // The lines before any core has started, and the one that ends it,
        // carry their own words.
        let looking = Fetching {
            core: String::new(),
            progress: None,
            at: 0,
            of: 3,
            note: "Getting ready".to_string(),
            ended: None,
        };
        assert_eq!(looking.sentence(), "Getting ready");

        // And nothing anywhere in it names a core, which is the word this
        // integration stopped saying out loud.
        for said in [one.sentence(), several.sentence(), looking.sentence()] {
            assert!(!said.contains("core"), "{said}");
        }
    }

    /// The bar under the row follows the words above it, in both shapes.
    ///
    /// A row saying "2 of 5" beside a bar four tenths full is one sentence and
    /// one picture agreeing; the same bar drawn from the *file* being fetched
    /// would be the row saying two different things at once. The part-finished
    /// game is added in so the bar moves through each of them rather than
    /// standing still and then jumping.
    #[test]
    fn the_bar_under_the_row_counts_what_the_line_above_it_counts() {
        let with = |fetching: Option<Fetching>, installing: Option<Installing>| {
            let mut retroarch = found(Vec::new());
            let inner = retroarch.inner.as_mut().expect("it was found");
            inner.fetching = fetching;
            inner.installing = installing;
            retroarch.arriving()
        };
        let fetch = |at: u32, of: u32, progress: Option<f32>| Fetching {
            core: "ppsspp".to_string(),
            progress,
            at,
            of,
            note: String::new(),
            ended: None,
        };

        // One thing to get: the words are a percentage and so is the bar.
        assert_eq!(with(Some(fetch(1, 1, Some(0.42))), None), Some(0.42));

        // Several: the second of five, two fifths of the way into it.
        assert_eq!(with(Some(fetch(2, 5, Some(0.5))), None), Some(0.3));
        assert_eq!(with(Some(fetch(1, 5, None)), None), Some(0.0));
        assert_eq!(with(Some(fetch(5, 5, Some(1.0))), None), Some(1.0));

        // The lines before any core has started say their own words — see
        // [`Fetching::sentence`] — and draw no bar: there is nothing yet to be
        // a fraction of.
        let mut looking = fetch(0, 3, None);
        looking.core = String::new();
        assert_eq!(with(Some(looking), None), None);

        // An install of RetroArch itself, which is the other percentage the row
        // carries — and a row that is only saying what is in a folder, which is
        // not a row counting up at all.
        assert_eq!(
            with(
                None,
                Some(Installing {
                    progress: Some(0.6),
                    note: "Downloading".to_string(),
                    removing: false,
                    ended: None,
                })
            ),
            Some(0.6)
        );
        assert_eq!(with(None, None), None);
        assert_eq!(RetroArch::absent().arriving(), None);
    }

    /// The counts on the row read as English, which is one `if` and is wrong in
    /// every shell that has not written it.
    #[test]
    fn one_game_is_not_one_games() {
        assert_eq!(plural(1, "game", "games"), "game");
        assert_eq!(plural(0, "game", "games"), "games");
        assert_eq!(plural(12, "game", "games"), "games");
    }

    /// The controller the person who asked for this owns, exactly as
    /// `/proc/bus/input/devices` reports it on their machine.
    fn ultimate_2_wireless() -> Pad {
        Pad {
            node: PathBuf::from("/dev/input/event33"),
            sysfs: "/devices/pci0000:00/0000:00:08.3/usb7/7-1/input48/event33".to_string(),
            name: "8BitDo Ultimate 2 Wireless".to_string(),
            vendor: 0x2dc8,
            product: 0x310b,
            keys: vec![
                0x130, 0x131, 0x133, 0x134, 0x136, 0x137, 0x13a, 0x13b, 0x13c, 0x13d, 0x13e,
            ],
            axes: vec![0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x10, 0x11],
        }
    }

    /// A pad of Xbox's shape is read out of what it declares, with no database
    /// consulted and none needed.
    ///
    /// The two middle face buttons are the whole of why this is worth a test:
    /// `0x133` has to come out *west*, which is the opposite of the name the
    /// kernel gives that code.
    #[test]
    fn an_xbox_pad_is_read_from_the_codes_it_declares() {
        let found = crate::pads::xinput(&steam_pad()).expect("an Xbox pad was not recognised");
        assert_eq!(found.at(Control::West), Some(At::Key(0x133)));
        assert_eq!(found.at(Control::North), Some(At::Key(0x134)));
        // And the rest of it agrees, control for control, with what GilRs was
        // captured saying about the same pad. Asked one at a time because the
        // order two answers happen to be built in is not part of either.
        let captured = steam_mapping();
        for control in [
            Control::South,
            Control::East,
            Control::North,
            Control::West,
            Control::LeftBumper,
            Control::RightBumper,
            Control::LeftTrigger,
            Control::RightTrigger,
            Control::Select,
            Control::Start,
            Control::LeftStick,
            Control::RightStick,
            Control::LeftX,
            Control::LeftY,
            Control::RightX,
            Control::RightY,
            Control::DPadX,
            Control::DPadY,
        ] {
            assert_eq!(found.at(control), captured.at(control), "{control:?}");
        }
    }

    /// A controller of Sony's shape is left alone rather than guessed at.
    ///
    /// It declares very nearly the same codes and means the opposite by two of
    /// them, so taking it for an Xbox pad would put somebody's jump on the
    /// button beside the one they are pressing. The shoulder *buttons* under
    /// the triggers are what give it away — an Xbox pad has never had them.
    #[test]
    fn a_pad_of_sonys_shape_is_not_taken_for_an_xbox_one() {
        let mut sony = steam_pad();
        sony.name = "Sony Interactive Entertainment DualSense Wireless Controller".to_string();
        sony.keys = vec![
            0x130, 0x131, 0x133, 0x134, 0x136, 0x137, 0x138, 0x139, 0x13a, 0x13b, 0x13c, 0x13d,
            0x13e,
        ];
        assert_eq!(crate::pads::xinput(&sony), None);
    }

    /// A pad with no analogue triggers is not one either — those are an Xbox
    /// controller's oldest promise, and a pad without them is some other shape
    /// whose two middle buttons cannot be guessed from here.
    #[test]
    fn a_pad_without_analogue_triggers_is_not_taken_for_one() {
        let mut odd = steam_pad();
        odd.axes = vec![0x00, 0x01, 0x03, 0x04, 0x10, 0x11];
        assert_eq!(crate::pads::xinput(&odd), None);
    }

    /// The whole point, in one assertion: for a pad RetroArch already has a
    /// hand-written profile for, the shell writes RetroArch's own answer.
    ///
    /// Every number here is transcribed from
    /// `share/libretro/autoconfig/udev/8BitDo_Ultimate_2_Wireless_USB.cfg`,
    /// which somebody sat down and worked out on the hardware. If reading the
    /// pad's own declaration ever stops agreeing with that, this says so —
    /// and a disagreement would mean the shell had started overruling a
    /// correct answer with a wrong one, on every pad of this shape at once.
    ///
    /// `input_menu_toggle_btn = "8"` is in that file and deliberately not
    /// here: the guide button is the way out of whatever is in front and is
    /// never handed to an application.
    #[test]
    fn what_the_shell_writes_is_what_retroarch_worked_out_by_hand() {
        let pad = ultimate_2_wireless();
        let mapping = crate::pads::xinput(&pad).expect("the pad was not recognised");
        let written = binds(1, &pad, &mapping);

        let retroarch = "\
input_b_btn = \"0\"
input_a_btn = \"1\"
input_y_btn = \"2\"
input_x_btn = \"3\"
input_l_btn = \"4\"
input_r_btn = \"5\"
input_select_btn = \"6\"
input_start_btn = \"7\"
input_l3_btn = \"9\"
input_r3_btn = \"10\"
input_l2_axis = \"+2\"
input_r2_axis = \"+5\"
input_l_x_plus_axis = \"+0\"
input_l_x_minus_axis = \"-0\"
input_l_y_plus_axis = \"+1\"
input_l_y_minus_axis = \"-1\"
input_r_x_plus_axis = \"+3\"
input_r_x_minus_axis = \"-3\"
input_r_y_plus_axis = \"+4\"
input_r_y_minus_axis = \"-4\"
input_up_btn = \"h0up\"
input_down_btn = \"h0down\"
input_left_btn = \"h0left\"
input_right_btn = \"h0right\"
";
        // The same lines, said the way a file naming one player says them.
        let mut theirs: Vec<String> = retroarch
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| format!("input_player1_{}", line.trim_start_matches("input_")))
            .collect();
        let mut ours: Vec<String> = written.lines().map(str::to_string).collect();
        theirs.sort();
        ours.sort();
        assert_eq!(ours, theirs);
    }
}
