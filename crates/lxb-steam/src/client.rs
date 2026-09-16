//! Valve's client, kept as a background process.
//!
//! This shell used to do without it: it started games itself, answered their
//! Steamworks calls with its own library, and fetched content out of Valve's
//! depots by hand. That worked, and it broke on every game that did anything
//! unusual — a loader that opens `libsteam_api.so` by name, a Windows game
//! whose prefix Steam had already made, an anti-cheat that wants the real
//! client's pipe. The client is the only thing that gets all of them right, so
//! now the client does it and this module is what keeps the client out of
//! sight.
//!
//! ## Out of sight
//!
//! Three things have to be true, or the user sees Steam and the point is lost:
//!
//! * **It never opens a window of its own.** `-silent` is what says so;
//!   [`Options`] carries the rest of what a background client is told.
//! * **It never asks for anything.** A client that has to ask who is signing in
//!   puts up its own login window, so it is handed a credential instead — see
//!   [`wake`], which starts it and then signs it in through the same interface
//!   its own login screen uses.
//! * **It is started only when there is Steam work to do**, and it is the shell
//!   that decides when that is. Nothing here starts anything on its own.
//! * **The windows it opens anyway never reach the screen.** `-silent` governs
//!   how the client starts, not what it does afterwards, and afterwards it puts
//!   up a "starting game" dialog and raises its storefront behind it. The
//!   compositor is asked to keep those off the screen by name — see
//!   [`WINDOW_NAMES`].
//!
//! ## Knowing what it is doing
//!
//! There is no API to ask. What there is, is the pipe the client listens on and
//! the log it keeps of its own connection — and between them they answer two of
//! the three questions the shell has: is it up, and is it signed in. See
//! [`State`], which is read from the disk in about a millisecond and is
//! therefore something the worker can poll.
//!
//! The third is **whose it is**, and neither of those can answer it. There is
//! one pipe in one home directory and every session on the machine shares it,
//! so a client belonging to a desktop the user left running answers exactly as
//! this session's own would — and then starts its games on that desktop. It is
//! read out of the client's own environment instead; see [`in_this_session`],
//! which is asked once per [`wake`] rather than polled.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// What the shell wants done to every process this module starts, or nothing
/// where it has not said.
///
/// Valve's client is an application the shell starts, and it was the one
/// application not being handed the environment the shell hands the others:
/// this module builds the command, so the shell's own launcher never saw it.
/// The one thing that costs, measured on this machine, is the guide button.
/// The shell takes a controller away from `/dev/input` and gives it back with
/// that button missing, and then tells applications to leave the pad's *raw*
/// node alone, because a driver reading `hidraw` walks straight around the
/// swap — and Steam, reading the raw node, went on seeing the button and
/// raising its overlay over the guide the button had just opened.
///
/// A hook rather than a table of variables, because what it answers changes
/// while the session runs: pads are plugged in and unplugged, and the list is
/// whatever the guard is holding at the moment the client is started.
static CONFINEMENT: Mutex<Option<fn(&mut Command)>> = Mutex::new(None);

/// Say how every Steam process started from here is to be confined.
///
/// Set once, by the shell, before anything is started.
pub fn confine_children_with(hook: fn(&mut Command)) {
    if let Ok(mut confinement) = CONFINEMENT.lock() {
        *confinement = Some(hook);
    }
}

/// What Valve's client calls its own windows.
///
/// `-silent` keeps the client from *opening* with a window, and it is honoured;
/// what it does not cover is the windows the client puts up later of its own
/// accord — the "starting game" dialog it shows while a title loads, and the
/// storefront it raises behind it. Those are the ones the shell asks the
/// compositor to keep off the screen, by name, through
/// `lxb_shell_v1.keep_out_of_sight`. This is the one place the names live.
///
/// They are `WM_CLASS` values rather than Wayland app ids: the client is an X11
/// program and reaches the session through Xwayland. Read off a running client,
/// its window carries `WM_CLASS = "steamwebhelper", "steam"` — an instance and
/// a class, in that order. Both are named here on purpose. The compositor keys
/// on the class, which is the second, and naming the instance as well means the
/// hiding does not quietly stop working if a window of the client's turns out
/// to carry the two the other way round.
///
/// Naming one too many costs nothing — a name no window carries hides no
/// window — where missing one costs the whole point of this module.
pub const WINDOW_NAMES: [&str; 2] = ["steam", "steamwebhelper"];

/// What the client titles its own main window — the storefront and library,
/// the one window it has whenever it is running with a UI at all.
///
/// Read off a live client, where it is exactly this. It is the client's own
/// name rather than a sentence about what the window is showing, which is the
/// only reason it can be matched on: everything else Valve puts in a title bar
/// is prose, and prose ships in every language Steam is translated into.
///
/// Used to tell the client *working* from the client *asking*. See
/// `lxb_shell_v1.unseen_window`, which is what the shell reads it against.
pub const STOREFRONT: &str = "Steam";

/// What the client names its notification toasts — the small windows it pops
/// at the corner of the desktop for a friend coming online, a controller
/// found, a download finished.
///
/// Read off a live client on 2026-09-13: `notificationtoasts_1_desktop`, then
/// `notificationtoasts_10000_desktop` and upwards, one window per toast, each
/// 283×70 at the bottom right of a desktop the client assumes is there. An
/// identifier rather than a sentence, like the `steam_app_<id>` class a game
/// is started under — in no language, and never shown to anybody — which is
/// what makes it safe to read.
///
/// None of them is ever a window somebody asked for. The one that mattered
/// arrived seven seconds after a fresh client was signed in, while its login
/// screen was still up: read as "a window of the request's own", it had the
/// compositor show the client — login screen and all — for the quarter of a
/// second before that screen closed. See [`is_a_toast`].
pub const TOAST_WINDOW_PREFIX: &str = "notificationtoasts_";

/// Whether a window of the client's is one of its notification toasts, by
/// title. See [`TOAST_WINDOW_PREFIX`].
pub fn is_a_toast(title: &str) -> bool {
    title
        .trim()
        .to_ascii_lowercase()
        .starts_with(TOAST_WINDOW_PREFIX)
}

/// Where Valve's client is on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Where {
    /// A `steam` on the path, or the launcher script beside the client.
    Native(PathBuf),
    /// The Flatpak, which is reached through `flatpak run`.
    Flatpak,
}

impl Where {
    /// Find it. `None` on a machine with no Valve client, which is a machine
    /// where nothing in this integration works and the shell says so once,
    /// plainly, rather than failing at every press.
    pub fn find() -> Option<Where> {
        Where::all().into_iter().next()
    }

    /// Every client on this machine, in the order [`Where::find`] prefers them.
    ///
    /// One entry is the ordinary machine and the whole of what [`Where::find`]
    /// needs. Two is a machine where this shell has picked one Steam and the
    /// user may have meant the other, and there is no honest way to guess which
    /// — a `steam` on `PATH` is as likely to be the one somebody uses as a
    /// Flatpak they installed last week. So the second answer exists to be
    /// *said* rather than to be chosen between; see
    /// [`crate::backend::say_which_steam`].
    pub fn all() -> Vec<Where> {
        let mut found = Vec::new();
        if let Some(path) = on_path("steam") {
            found.push(Where::Native(path));
        }
        if flatpak_deployed() && on_path("flatpak").is_some() {
            found.push(Where::Flatpak);
        }
        found
    }

    /// A command that runs the client with no arguments yet.
    ///
    /// Every process this module starts is built here, which is why the
    /// shell's confinement is applied here and nowhere else: the client, the
    /// courier that hands it a URL, and the one that starts it with a URL
    /// already in hand are all the same command with different arguments, and
    /// a game the client launches inherits whatever the client was given. See
    /// [`CONFINEMENT`].
    pub(crate) fn command(&self) -> Command {
        let mut command = match self {
            Where::Native(path) => Command::new(path),
            Where::Flatpak => {
                let mut command = Command::new("flatpak");
                command.arg("run").arg("com.valvesoftware.Steam");
                command
            }
        };
        if let Some(confine) = CONFINEMENT.lock().ok().and_then(|hook| *hook) {
            confine(&mut command);
        }
        command
    }
}

/// What the background client is doing.
///
/// Deliberately five states and not a bag of booleans: every caller wants to
/// know one of these five things, and a caller that had to work out "up but
/// not signed in" from two flags is a caller that will one day get it wrong in
/// the direction of starting a game that cannot start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// There is no Valve client on this machine.
    Absent,
    /// There is one, and it is not running.
    Stopped,
    /// It is running and has not signed in yet — starting up, updating itself,
    /// or reaching for the network. This is the state a launch waits in.
    Starting,
    /// It is running and signed in, as this account id. Everything works.
    SignedIn(u32),
    /// It is running and logged on for this account **without Steam's
    /// servers** — Valve's own Offline Mode.
    ///
    /// A separate state and not a flag on [`State::SignedIn`], because the two
    /// are read from different places and are true of different things. An
    /// offline client plays every game that is on the disk and fully up to
    /// date, and can do nothing at all that needs Steam: no install, no
    /// removal, no verify, no library. See [`offline`], which is where the
    /// mode is read and asked for.
    Offline(u32),
}

impl State {
    pub fn running(self) -> bool {
        !matches!(self, State::Absent | State::Stopped)
    }

    pub fn signed_in(self) -> bool {
        matches!(self, State::SignedIn(_) | State::Offline(_))
    }

    /// Whether it is signed in **as this account**, which is what every caller
    /// in this crate actually wants of it.
    ///
    /// [`State::signed_in`] answers a question nobody here is asking. One
    /// machine has one pipe and one client, and the person at the desk may have
    /// two accounts, or a household may have four; a client signed in to the
    /// wrong one of them passes every other test in this module and then
    /// installs into, verifies and plays out of somebody else's library.
    ///
    /// Offline counts, and that is the whole of what makes a game start with
    /// no network. A client in Valve's Offline Mode is logged on for exactly
    /// one account and holds exactly that account's library; the question this
    /// answers is whose games it would open, and the answer is the same either
    /// way.
    pub fn signed_in_as(self, who: Credential<'_>) -> bool {
        matches!(self, State::SignedIn(id) | State::Offline(id) if id == who.account_id())
    }

    /// Whether what it is signed in to is this machine rather than Steam.
    ///
    /// Asked by everything that needs Valve's servers, so that it can say so
    /// rather than fail somewhere further down: an install driven at an offline
    /// client is a wizard that opens and cannot fetch anything.
    pub fn offline(self) -> bool {
        matches!(self, State::Offline(_))
    }
}

/// What the shell can hand to Valve's client as a `steam:` URL.
///
/// Every one of these ends in a window of the client's own, which is what they
/// have in common and why they are one type: the shell gives sight back before
/// handing any of them over. The things that must *not* raise a window —
/// playing, installing, removing — are not here and are deliberately separate.
/// See [`crate::Steam::play`], [`crate::Steam::install`] and
/// [`crate::Steam::uninstall`], each of which is answered by the shell's own
/// screen instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Doing {
    /// Hand the whole install over to Steam's own window.
    ///
    /// The one thing a silent install cannot do is answer a question meant for
    /// a person — an agreement to accept, a product key to type — and roughly
    /// two games in five have one. So this is what the shell offers when its
    /// own install comes back with [`crate::webui::Problem::Asks`]: the game
    /// still gets installed, and the question is asked in the only place that
    /// can ask it.
    Install,
    /// Check what is on the disk against what should be, and repair it.
    Verify,
    /// Open Steam's own downloads list.
    ///
    /// The deliberate hand-off. A download that Steam has paused, queued behind
    /// another, or stopped over a full disk is a download this shell has no way
    /// to resume: pausing and resuming are the client's own list, and a second
    /// implementation of it here would be a second opinion about somebody's
    /// bandwidth. So the shell says plainly what is happening — see
    /// [`crate::library::Standing`] — and offers the one place it can be
    /// changed, rather than offering a button that does nothing.
    Downloads,
    /// Bring the client up in Big Picture, its own console screen.
    ///
    /// Not about one title at all, which is why it ignores the id it is given.
    /// It is here rather than as a function of its own because it is the same
    /// journey as every other row of that menu.
    ///
    /// The plain one of the two, and the one simply called "Open Steam",
    /// because it is the one that belongs on this screen: a shell driven from
    /// a pad across the room has just handed over to another program, and Big
    /// Picture is the only face of the client that can be driven the same way.
    /// The desktop client is still a row away — see [`Doing::Open`].
    BigPicture,
    /// Bring up the client's desktop window.
    ///
    /// Offered beside [`Doing::BigPicture`] rather than instead of it: the
    /// storefront's small print, the console tabs of the settings, and every
    /// dialog Big Picture has no screen for are in this window and nowhere
    /// else, so a shell that could only raise the console one would be a shell
    /// with a part of Steam it cannot reach.
    Open,
}

impl Doing {
    /// What the row that does this is called.
    pub fn label(self) -> &'static str {
        match self {
            Doing::Install => "Install with Steam",
            Doing::Verify => "Verify with Steam",
            Doing::Downloads => "Open Downloads in Steam",
            Doing::BigPicture => "Open Steam",
            Doing::Open => "Open Steam (Client)",
        }
    }

    /// The URL the client answers for it.
    pub fn url(self, app_id: u32) -> String {
        match self {
            Doing::Install => format!("steam://install/{app_id}"),
            Doing::Verify => format!("steam://validate/{app_id}"),
            // About the list rather than about one title, so the id is ignored
            // exactly as Big Picture's is.
            Doing::Downloads => "steam://open/downloads".to_string(),
            // Big Picture is a mode of the running client rather than a
            // separate program, so this is the whole of how it is entered —
            // and it starts the client first where there is not one running,
            // exactly as the rest of these do.
            Doing::BigPicture => "steam://open/bigpicture".to_string(),
            // `open/main` rather than starting the program with no arguments,
            // so a client that is already running comes to the front instead
            // of a second process starting and immediately exiting.
            Doing::Open => "steam://open/main".to_string(),
        }
    }

    /// What the shell calls the thing this opens, while it is opening.
    ///
    /// The name on the loading screen a press puts up — see
    /// `Shell::begin_valves_client_splash` — and it is the row's own
    /// vocabulary rather than Valve's window titles, so that what somebody
    /// pressed and what they are then looking at are called the same thing.
    /// "Open Steam" opens *Steam*; "Open Steam (Client)" opens the thing this
    /// shell has always called Steam (Client); "Open Downloads in Steam"
    /// opens Steam Downloads.
    ///
    /// The two that are about a title open a window of Steam's about that
    /// title — a wizard, a file check — and neither has a name of its own to
    /// be given. What is opening is Steam.
    pub fn opening(self) -> &'static str {
        match self {
            Doing::BigPicture | Doing::Install | Doing::Verify => "Steam",
            Doing::Open => "Steam (Client)",
            Doing::Downloads => "Steam Downloads",
        }
    }

    /// Whether this is about the client itself rather than about one title, and
    /// so needs nothing selected to be pressed.
    pub fn about_the_client(self) -> bool {
        matches!(self, Doing::BigPicture | Doing::Open | Doing::Downloads)
    }

    /// Whether the window this raises is the client's main one — the
    /// storefront, see [`STOREFRONT`] — rather than a window of its own.
    ///
    /// Two of them: `open/main` *is* that window, and `open/downloads` is a
    /// page of it. On a client whose storefront is already mapped, either
    /// brings that window forward and maps nothing new, so it is the window
    /// the press is waiting on. The rest each raise a window of their own —
    /// Big Picture's console screen, the install wizard, the file check — and
    /// a storefront that happens to be mapped already is not what any of them
    /// was for. Read off a live client on 2026-09-13: Big Picture asked for
    /// from over the storefront had that storefront counted as its arrival,
    /// 230 ms before the console screen mapped.
    pub fn answered_by_the_storefront(self) -> bool {
        matches!(self, Doing::Open | Doing::Downloads)
    }
}

/// The two directories the client keeps itself in.
///
/// Both, because they are not the same and neither can be derived from the
/// other without following a symbolic link that may not be there: `root` is
/// where the client is installed and holds `config`, `steamapps` and `logs`,
/// while `home` is the small `~/.steam` beside it that holds the pid, the pipe
/// and, on Linux, the registry. Passed rather than looked up so a test can put
/// the whole of this somewhere harmless.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub root: PathBuf,
    pub home: PathBuf,
}

impl Options {
    /// Where *this* client keeps them.
    ///
    /// Asked of a [`Where`] rather than of the machine, because the two kinds
    /// of client keep them in two different places and a machine may have the
    /// leavings of both. The Flatpak runs with its home remapped into
    /// `~/.var/app/com.valvesoftware.Steam`, so its pipe and its registry are
    /// in there with its library; the native client uses the real home. Asking
    /// the disk which of them exists — which is what this used to do — pairs
    /// whichever it finds first with whichever client was found, and on a
    /// machine that has had both that is one client's log read for another
    /// client's state.
    ///
    /// `None` only where there is no home directory to build them out of.
    /// Where a client has been installed and never run there is nothing on the
    /// disk yet, and this answers with the places it *will* keep them, so the
    /// shell can start it a first time rather than reporting it missing. See
    /// [`wake`], which creates the root it is about to write the marker into.
    pub fn for_client(client: &Where) -> Option<Options> {
        let home = std::env::var_os("HOME").map(PathBuf::from)?;
        Some(Options::in_home(client, &home))
    }

    /// Both layouts, whether or not either client is installed.
    ///
    /// For the one job that is about what a client left on the disk rather
    /// than about a client: clearing the automatic sign-in when somebody signs
    /// out. That has to work on a machine whose Steam has been *removed* —
    /// otherwise a reinstall months later comes up signed in to an account the
    /// shell has since said nobody is signed in to — and it has to reach the
    /// Flatpak's registry as well as the native one, which asking for a single
    /// layout could not: whichever was asked for, the other went untouched.
    pub fn every_layout() -> Vec<Options> {
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
            return Vec::new();
        };
        Options::every_layout_in(&home)
    }

    /// The same, below a given home directory. The native client is named with
    /// an empty path because there is no client here to name: what is being
    /// asked for is a layout, and [`Options::in_home`] reads only which of the
    /// two it is.
    pub(crate) fn every_layout_in(home: &Path) -> Vec<Options> {
        vec![
            Options::in_home(&Where::Native(PathBuf::new()), home),
            Options::in_home(&Where::Flatpak, home),
        ]
    }

    /// The same, below a given home directory, so a test can put the whole of
    /// it somewhere harmless.
    pub(crate) fn in_home(client: &Where, home: &Path) -> Options {
        match client {
            // The two symbolic links the client maintains come first, since
            // those follow a Steam that has been moved, then the directory it
            // unpacks into — which is also where a client that has never run
            // will put itself, and so is the fallback.
            Where::Native(_) => Options {
                root: crate::library::native_roots(home)
                    .into_iter()
                    .find(|path| crate::library::looks_like_a_root(path))
                    .unwrap_or_else(|| crate::library::unpacks_into(home)),
                home: home.join(".steam"),
            },
            Where::Flatpak => {
                let sandbox = crate::library::flatpak_home(home);
                Options {
                    root: crate::library::unpacks_into(&sandbox),
                    home: sandbox.join(".steam"),
                }
            }
        }
    }
}

/// The whole of what this shell says to Valve's client on the command line.
///
/// Every one of these is here to keep it out of the way rather than to make it
/// faster, and each costs exactly the feature it names:
///
/// * `-silent` — come up with no window at all. Without it the client opens its
///   library over whatever the user was looking at.
/// * `-nofriendsui` — do not bring up the friends list, which is a second
///   window and the one most likely to appear on its own when somebody comes
///   online.
/// * `-noverifyfiles` — do not re-check its own installation on the way up. It
///   is minutes of disk on a cold start, it is not why the client is being
///   started, and the client does it again on its own schedule anyway.
/// * `-nocrashdialog` — a client that has fallen over must not put a dialog in
///   front of a game. The shell notices it is gone by asking, and says so
///   itself.
const QUIETLY: [&str; 4] = [
    "-silent",
    "-nofriendsui",
    "-noverifyfiles",
    "-nocrashdialog",
];

/// The same, less the one word that makes a *first* start fatal.
///
/// **`-noverifyfiles` is not "skip a check" on a machine where Steam has never
/// run; it is "skip the install".** Measured on this machine on 2026-09-04, on
/// a home directory with no `~/.local/share/Steam` in it. Valve's launcher
/// unpacks its bootstrap, and the bootstrap's *verification* step is what
/// notices there is no client here and fetches one. Told not to verify, it
/// writes three lines and dies:
///
/// ```text
/// Verifying installation...
/// Verification skipped
/// Verification complete
/// dlmopen steamui.so failed: steamui.so: cannot open shared object file
/// Fatal error: Failed to load steamui.so
/// ```
///
/// — in under a second, leaving a `.crash` file and a Steam directory holding
/// nothing but the bootstrap. The identical command without the flag downloads
/// 496 MB and installs in about forty seconds. So the flag stays for every
/// ordinary start, where it saves minutes of disk, and is dropped for the one
/// start that is an installation. See [`crate::setup`].
const QUIETLY_FIRST_RUN: [&str; 3] = ["-silent", "-nofriendsui", "-nocrashdialog"];

/// What to say to a client that is about to be started, given what is on the
/// disk where it keeps itself.
///
/// One function so that the two lists cannot be chosen between in two places:
/// [`start`] is not the only caller any more — [`crate::setup`] starts the
/// first-ever client itself, because it keeps the child in order to hear it
/// fail.
pub(crate) fn how_to_start(options: &Options) -> &'static [&'static str] {
    match crate::library::looks_like_a_root(&options.root) {
        true => &QUIETLY,
        false => &QUIETLY_FIRST_RUN,
    }
}

/// Start the client, quietly, and do not wait for it.
///
/// The child is deliberately dropped: Valve's client daemonises itself within a
/// second or two and the process this starts is a launcher that exits, so there
/// is nothing here worth keeping to wait on. What the client is doing is read
/// from [`state`] instead, which works the same whether this session started it
/// or it was already running when the shell came up.
pub fn start(client: &Where, options: &Options) -> std::io::Result<()> {
    let words = how_to_start(options);
    tracing::info!(
        root = %options.root.display(),
        first_run = words.len() == QUIETLY_FIRST_RUN.len(),
        "starting Valve's client in the background"
    );
    client
        .command()
        .args(words)
        // Its output belongs in its own logs, which is where it already goes.
        // Inherited pipes would fill and stall it the moment nothing read them.
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(drop)
}

/// Ask the client for one `steam:` URL, starting one where there is none
/// running — which is what every caller actually wants, and is not the same
/// command twice.
///
/// The difference is what the process does, and it is the difference between a
/// press that works and a shell that stops. With a client already up, `steam
/// <url>` is a courier: it finds the running one through the pipe, hands the
/// URL over and exits in milliseconds, and its exit status is the only sign the
/// request was taken — so [`tell`] waits for it. With no client up, that same
/// command *is* the client: `/usr/bin/steam` execs the real thing, which then
/// runs until the user quits Steam. Waiting on that is waiting for the whole
/// session, and a caller that did it from a shell's event loop would freeze the
/// screen for as long as Steam was open.
///
/// So the wait is only ever spent on a courier. Where there is no client, this
/// starts one with the URL already in hand — no `-silent`, because every URL
/// that comes through here is a window somebody asked to see — and does not
/// wait for it at all, exactly as [`start`] does not.
///
/// `options` is how the two are told apart, and `None` — a machine whose client
/// directories cannot be found — is treated as no client running. The cost of
/// being wrong in that direction is a request that is delivered and not waited
/// for; the other way round is the freeze.
pub fn open(client: &Where, options: Option<&Options>, url: &str) -> std::io::Result<()> {
    let running = options.is_some_and(|options| is_running(Some(client), options));
    if running {
        return tell(client, url);
    }
    tracing::info!(url, "no client to hand this to; starting one with it");
    client
        .command()
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(drop)
}

/// Hand the *running* client one `steam:` URL, and wait for the handover.
///
/// Only ever called with a client already up — see [`open`], which is what
/// decides that and is what every caller outside this module uses. The wait is
/// milliseconds and its exit status is the only sign that the client took the
/// request.
fn tell(client: &Where, url: &str) -> std::io::Result<()> {
    tracing::debug!(url, "handing Valve's client a request");
    let status = client
        .command()
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "Steam would not take {url}: {status}"
        )))
    }
}

/// What became of a client that was asked to shut down.
///
/// [`stop_if_ours`] used to answer `bool`, and the shell above it threw the
/// answer away — which was right while nothing up there had a decision to make
/// from it. It has one now: a client that was **never asked** is a client that
/// is never going, and the shell spent seventy seconds waiting out two grace
/// periods to discover that and then said the client *would not shut down*,
/// which was not true of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Closing {
    /// It was asked, and `steam -shutdown` took the message.
    ///
    /// Not that it went: that request returns when the client has been *asked*,
    /// and the client has its own work to finish first. Whether it went is
    /// [`running`], asked afterwards.
    Asked,
    /// There was nothing to ask — no client running, or no Steam on this
    /// machine at all.
    Gone,
    /// It belongs to another session on this machine, so it was not asked.
    ///
    /// **The one that will not change by being asked again.** Whose client it
    /// is cannot turn into this session's while it goes on running, so a shell
    /// waiting to see whether this one goes is waiting for something that
    /// cannot happen.
    NotOurs,
    /// It is this session's, and the request itself did not go through.
    ///
    /// Rare and worth telling from the one above: the client is ours to stop
    /// and the shell could not manage it — a `steam` that is no longer on the
    /// disk under a client still running from it, most likely.
    Refused,
}

/// Ask the client to shut down **if it is this session's**, saying what became
/// of it.
///
/// [`stop`] documents an assumption it cannot check — that the client being
/// stopped is one this session is entitled to stop — and signing out used to
/// call it on whatever was running. On a machine with a desktop session left
/// open behind this one, that was somebody's Steam shut down, with their
/// download in it, because somebody else signed out of the shell.
///
/// The same evidence [`wake`] refuses on, and read the same way round:
/// [`in_this_session`] answers `true` when it cannot find out, so the
/// destructive half only ever happens on evidence.
pub fn stop_if_ours(client: &Where, options: &Options) -> Closing {
    if !is_running(Some(client), options) {
        return Closing::Gone;
    }
    if !in_this_session(options) {
        tracing::info!(
            "leaving Valve's client running: it belongs to another session on this machine"
        );
        return Closing::NotOurs;
    }
    if let Err(error) = stop(client) {
        // Not "would not shut down", which is what the shell says about a
        // client that was asked and stayed. This is the asking itself failing.
        tracing::info!(%error, "Valve's client could not be asked to shut down");
        return Closing::Refused;
    }
    Closing::Asked
}

/// Ask the client to shut down.
///
/// Only ever called for a client this session started, and only when the
/// session ends: a user who had Steam running before the shell came up wants it
/// still running after. [`stop_if_ours`] is what checks that; call it rather
/// than this unless the caller has already established whose client it is.
pub fn stop(client: &Where) -> std::io::Result<()> {
    tracing::info!("asking Valve's client to shut down");
    client
        .command()
        .arg("-shutdown")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(drop)
}

/// Stop the client and start it again, for the one case that needs it: it was
/// running before it had been told to expose its JS context.
///
/// The wait in the middle is the point. `-shutdown` returns as soon as the
/// running client has been *asked*, not when it has gone, and a client started
/// while the old one still holds the pipe is a second client that hands its
/// arguments over and exits — which would look exactly like a restart that did
/// nothing.
fn restart(client: &Where, options: &Options) -> Result<(), String> {
    stop(client).map_err(|error| format!("Steam would not shut down: {error}"))?;

    let deadline = Instant::now() + UNTIL_IT_STOPS;
    while running(&options.home) {
        if Instant::now() >= deadline {
            return Err("Steam did not shut down, so it could not be started again.".to_string());
        }
        std::thread::sleep(Duration::from_millis(250));
    }

    start(client, options).map_err(|error| format!("Steam would not start: {error}"))
}

/// How long to give the client to sign itself in before concluding that it
/// cannot, and handing it a credential through an interface that has to be
/// opened for the purpose.
///
/// Generous against a cold start on a slow disk — it took five seconds here —
/// because the cost of being wrong is asymmetric. Too short only means the
/// interface is opened and the client restarted when waiting would have done,
/// which is slower and briefly exposes something; too long means a first-ever
/// sign-in sits there for no reason. Neither breaks, and the first is the one
/// worth avoiding.
const UNTIL_IT_SIGNS_ITSELF_IN: Duration = Duration::from_secs(30);

/// How long to wait for a client that has been asked to shut down to let go of
/// its pipe. It has its own work to finish first — writing its configuration
/// back, closing its connection — and this is generous rather than tight
/// because the alternative to waiting is starting a second client that does
/// nothing.
///
/// Public because the shell watches the same client go and must not be quicker
/// to give up on it than the crate that asked it: see
/// `steam::UNTIL_THE_CLIENT_GOES`, which is read off this.
pub const UNTIL_IT_STOPS: Duration = Duration::from_secs(30);

/// How long to wait for a client that has just been started to come up far
/// enough to be spoken to. A cold client unpacks an update, starts a browser
/// and reaches the network before it will answer anything.
const UNTIL_IT_ANSWERS: Duration = Duration::from_secs(90);

/// And how long to wait, after it has been given a credential, for Steam to
/// agree. This is a round trip to Valve plus whatever the client does with the
/// answer; it is the wait a user is watching a loading screen through.
const UNTIL_IT_SIGNS_IN: Duration = Duration::from_secs(60);

/// How long an ordinary cold [`wake`] can honestly take, for whoever is drawing
/// the loading screen over it.
///
/// A cold client comes up, is watched until it answers, and is then handed a
/// credential and watched until Steam agrees. That is the two numbers above and
/// it is the path nearly every first press of a session takes.
///
/// Deliberately not the worst case. A client that has to be stopped, started,
/// found still not exposing its port, stopped and started again can outlast
/// three of these, and a shell that waited that long before saying anything
/// would be a shell with a spinner on it for six minutes. What this is is the
/// number a splash may not give up *before*, published from here so that the
/// two cannot drift apart: the wait and the patience for it were separately
/// chosen constants in separate crates, and the splash's was the shorter — so
/// a cold client reliably outlived the loading screen watching for it.
pub const LONGEST_ORDINARY_WAKE: Duration =
    Duration::from_secs(UNTIL_IT_ANSWERS.as_secs() + UNTIL_IT_SIGNS_IN.as_secs());

/// What a caller needs of Valve's client.
///
/// The distinction earns its place because one of these needs the client's JS
/// context and the other does not, and that context is the whole of what is
/// worth being careful about here — see [`crate::webui::expose`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    /// Up and signed in. Every game press made by a session that has reached
    /// Steam: starting a title is a `steam:` URL handed over the client's pipe,
    /// and touches no interface at all.
    SignedIn,
    /// The same, for a session that cannot reach Steam itself.
    ///
    /// Everything [`Need::SignedIn`] accepts, and one thing more: where the
    /// client will not log on either, it is asked for Valve's own Offline Mode
    /// rather than watched until the patience runs out. That is the whole of
    /// what makes a game start on a machine with no network — see [`offline`].
    ///
    /// A need and not a second function, because it is the same wake: the
    /// client may be up already, may be signed in already, may belong to
    /// somebody else. Only the last step differs.
    Offline,
    /// Up, signed in, and answering on its JS context. Moving a game on or off
    /// the disk needs this, because those are made as calls into the client
    /// rather than as URLs — a URL for either of them raises a window.
    Context,
}

/// Who the shell is signed in as, and what signs a client in as them.
///
/// The three travel together because separating them is what let the two
/// questions be confused. "Is a client signed in" and "is a client signed in
/// as the person whose library is on the screen" are different states, and the
/// second is the one every press here actually depends on: a client already
/// signed in to somebody else's account passes every other test in this module
/// and then installs, verifies or plays out of *their* library.
#[derive(Debug, Clone, Copy)]
pub struct Credential<'a> {
    /// The account name Steam knows them by, which is what its own login
    /// interface is handed.
    pub account: &'a str,
    /// The whole SteamID of that account.
    pub steam_id: u64,
    pub refresh_token: &'a str,
}

impl Credential<'_> {
    /// The account id, which is the low half of the SteamID and the only form
    /// a running client ever says out loud — see [`logged_on_in`], which reads
    /// it out of `[U:1:<account>]` in the client's own connection log.
    pub fn account_id(&self) -> u32 {
        self.steam_id as u32
    }
}

/// How far a caller may go with a client that is not this session's to move.
///
/// The default is [`Permission::AskFirst`], and it is the default because the
/// alternative is destructive and silent. A client belonging to another display
/// session is somebody's Steam with somebody's download in it; a client signed
/// in to another account is somebody's library. Both used to be taken over
/// without anybody being asked — see [`wake`], which is where the two are
/// found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    /// Refuse a client that is not ours and say so, so the shell can put the
    /// choice in front of the person who pressed the button.
    AskFirst,
    /// The person has been asked and said yes: stop that client and start it
    /// again here, signed in as this session's account.
    MayTakeOver,
}

/// A client that is running and is not this session's: what is in the way, and
/// what getting past it would cost.
///
/// Two short lines rather than one sentence, and that is a fact about the
/// screen rather than about style. A panel gives one line to each note and cuts
/// what will not fit, so a sentence long enough to say both halves arrives with
/// its end missing — and here the end is the half that says whose download is
/// about to stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotOurs {
    /// Which client is in the way.
    pub what: String,
    /// And what moving it would cost, said to whoever is about to decide.
    pub cost: String,
}

impl NotOurs {
    fn new(what: &str, cost: &str) -> NotOurs {
        NotOurs {
            what: what.to_string(),
            cost: cost.to_string(),
        }
    }
}

impl std::fmt::Display for NotOurs {
    /// For a log line, where one line is what there is.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.what, self.cost)
    }
}

/// Why a client could not be brought up.
///
/// Two shapes rather than one string for the reason [`crate::Stopped`] has two:
/// the shell answers them differently. A failure is something to report; a
/// client that belongs to somebody else is a question to ask.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Something went wrong, and this is what to say about it.
    Failed(String),
    /// Nothing went wrong. There is a client running that this session must
    /// not take over on its own: it is drawing into another session, or it is
    /// signed in as another account. Answered by asking, and then by
    /// [`Permission::MayTakeOver`].
    NotOurs(NotOurs),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::Failed(why) => f.write_str(why),
            Refusal::NotOurs(refusal) => write!(f, "{refusal}"),
        }
    }
}

impl From<String> for Refusal {
    fn from(why: String) -> Refusal {
        Refusal::Failed(why)
    }
}

/// One client, proved to be this session's and this account's, at one moment.
///
/// Every question this module asks about whose client it is — is one running,
/// is it drawing into this session, is it signed in to this account — is a
/// reading of the machine as it stands, and the machine does not stand still.
/// A wake is most of two minutes; the flow behind it takes a mutex, opens a
/// socket and drives an install wizard. Somewhere in that gap Valve's client
/// can die and be replaced by one that signed itself into whichever account it
/// last remembered, and every check made before the gap is then a check about
/// a process that is no longer there.
///
/// So the checks answer with *what they proved about*, and the answer is
/// carried to the moment of acting and asked again — see [`Proven::still_there`],
/// which every irreversible step is preceded by.
///
/// The process is named by the pid holding the pipe rather than by anything
/// stronger, and a pid can in principle be reused. It cannot be reused inside
/// the window this covers: what separates a proof from the act it guards is
/// milliseconds, or the length of one wizard flow, and reuse needs the whole of
/// the machine's pid space to turn over in that time. The failure actually seen
/// is a client that was killed and started again, and that takes a new pid
/// every time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Proven {
    /// The process holding `steam.pipe` open for reading when the proof was
    /// made, where it could be found.
    ///
    /// `None` is the same "could not find out" [`in_this_session`] answers with
    /// — a client running as another user, a `/proc` entry that went while it
    /// was being read — and two of them compare equal, so a shell that cannot
    /// see into the client is no worse off than it was before this existed. It
    /// is not weaker than that either: the account half below is read from the
    /// client's own log and does not depend on finding the process at all.
    pub pid: Option<u32>,
    /// The account id it was signed in — or logged on offline — as.
    pub account: u32,
    /// Whether the wake that made this proof handed the client its credential,
    /// rather than finding it signed in already.
    ///
    /// Carried because the window a request raises on such a client is not
    /// the first window there is. A client that had to be signed in was
    /// sitting on its own login screen, and it keeps that screen up for some
    /// seconds *after* it reports logged on — measured on 2026-09-13: logged on
    /// at 20:31:54, login window closed at 20:31:59, storefront mapped at
    /// 20:32:01. A shell that gave sight the moment the request was taken put
    /// Valve's login screen on the display for five seconds and then, reading
    /// its closing as the user being done, hid the storefront that came two
    /// seconds later. So the shell is told, and waits for a window of the
    /// request's own. See `Event::HandedOver`.
    pub just_signed_in: bool,
}

impl Proven {
    /// Ask both questions again, of the client as it stands this instant.
    ///
    /// **This is step five**, and it is a separate call rather than something
    /// folded into the wake because the two are minutes apart. What it must
    /// catch is not one race but three of them: the client stopped, the client
    /// was replaced by one somebody else started, and the client signed itself
    /// into another account while this session was queueing behind a wizard.
    ///
    /// Unsure refuses here, which is the opposite of the rule
    /// [`in_this_session`] is under, and deliberately: that one decides whether
    /// to *stop* somebody's Steam, where being wrong costs them a download, and
    /// this one decides whether to install into, remove from or play out of a
    /// library, where being wrong costs somebody else's. The whole cost of
    /// refusing is a press that says so and can be made again.
    pub fn still_there(&self, client: &Where, options: &Options) -> Result<(), Refusal> {
        let now = proven_for(client, options, self.account)?;
        if now.pid != self.pid {
            tracing::info!(
                was = ?self.pid,
                now = ?now.pid,
                "Valve's client was replaced between the check and the request"
            );
            return Err(Refusal::Failed(
                "Steam was restarted while this was being asked for.".to_string(),
            ));
        }
        Ok(())
    }
}

/// Prove that the client running **now** is this session's and this account's.
///
/// The ownership contract asked as one question, in one place, so that every
/// path which acts on a title asks it the same way and in the same order:
/// running at all, then which session it draws into, then which account it is
/// acting for. Answering with a [`Proven`] rather than with a `bool` is what
/// lets the answer be checked again later against the same client rather than
/// against whatever is holding the pipe by then.
///
/// A client that is **not running is not proof of anything**, and this is where
/// that differs from [`crate::not_ours`], which reads a stopped client as
/// nobody's and lets a `steam:` URL start one. That is right for opening
/// Steam's own window and wrong for everything else: a client started with a
/// title's URL already in hand signs itself into whichever account it
/// remembered, and does the thing to that account's library before this session
/// has proved anything at all. So there is nothing to prove here, and the
/// caller's answer is to wake a client first — see [`wake`], which answers with
/// one of these.
pub fn prove(client: &Where, options: &Options, who: Credential<'_>) -> Result<Proven, Refusal> {
    proven_for(client, options, who.account_id())
}

/// The same, asked with the account id alone.
///
/// For the caller that has no credential to hand and needs none: the shell's
/// own launch, which delivers `steam://rungameid/…` from the thread that draws
/// and holds the account's number rather than the token that would sign one in.
/// Proving *whose* client it is only ever needs the number, because the number
/// is the whole of what a client says out loud — see [`logged_on_in`].
pub fn proven_for(client: &Where, options: &Options, account: u32) -> Result<Proven, Refusal> {
    if !is_running(Some(client), options) {
        return Err(Refusal::Failed(
            "Steam is not running, so there was nothing to ask.".to_string(),
        ));
    }
    if !in_this_session(options) {
        return Err(Refusal::NotOurs(NotOurs::new(
            "Steam is running in another session.",
            "This would happen there, not here.",
        )));
    }
    // [`state_now`] and never [`state`]: the tail of the connection log belongs
    // to the run before for the first second or two of a client's life, and
    // this is exactly the moment that is true — something has just started a
    // client, or one has just been replaced.
    match state_now(Some(client), options) {
        State::SignedIn(id) | State::Offline(id) if id == account => Ok(Proven {
            pid: holder_of(&options.home.join("steam.pipe")),
            account,
            just_signed_in: false,
        }),
        State::SignedIn(_) | State::Offline(_) => Err(Refusal::NotOurs(NotOurs::new(
            "Steam is signed in to another account.",
            "This would happen to that account.",
        ))),
        // Up and has not said who it is yet, which is not the same as nobody's
        // and must not be read as this session's. A client three seconds into
        // starting is about to be signed in to *something*, and what it is
        // about to be signed in to is the account it remembered.
        State::Starting => Err(Refusal::Failed(
            "Steam is still starting and has not signed in yet.".to_string(),
        )),
        // Neither is reachable past `is_running` above, and both are said as
        // arms rather than as a panic: there is nothing about a Steam that has
        // gone in the last microsecond worth ending a session over.
        State::Absent | State::Stopped => Err(Refusal::Failed(
            "Steam stopped before it could be asked.".to_string(),
        )),
    }
}

/// Hand one `steam:` URL to a client that has just been proved, and to no
/// other — starting none.
///
/// The difference from [`open`] is the whole of it. `open` starts a client
/// where there is none, with the URL already in its hand, which is what makes
/// "Open Steam" work on a machine where Steam has never run; handed a title's
/// URL it is also what delivers an install to whichever account that fresh
/// client remembered. This one refuses instead, because by the time anything
/// gets here a client has already been woken and proved, and a client that has
/// gone since is a press to make again rather than a fresh Steam to start
/// blind.
pub fn deliver(
    client: &Where,
    options: &Options,
    proven: &Proven,
    url: &str,
) -> Result<(), Refusal> {
    proven.still_there(client, options)?;
    tell(client, url).map_err(|error| Refusal::Failed(error.to_string()))
}

/// Prove the client that is running and hand it one `steam:` URL, in one pass.
///
/// For the caller that has no proof carried from anywhere to check against, and
/// cannot afford to make two: the shell's own launch, which runs the courier
/// from the thread that draws. [`deliver`] would walk `/proc` twice over — once
/// to make a proof and once to check it against itself — to answer one
/// question.
///
/// It is the same question either way, and asking it here is the point.
/// `steam://rungameid/…` used to go out through [`open`], which starts a client
/// where there is none: a wake that succeeded and a client that died in the
/// seconds after it meant a game delivered to a Steam started from cold, signed
/// into whichever account it remembered, with no check made of either half.
///
/// Answers with what it proved, so the caller's log can name the client that
/// took the game.
pub fn prove_and_deliver(
    client: &Where,
    options: &Options,
    account: u32,
    url: &str,
) -> Result<Proven, Refusal> {
    let proven = proven_for(client, options, account)?;
    tell(client, url).map_err(|error| Refusal::Failed(error.to_string()))?;
    Ok(proven)
}

/// One client, one caller at a time.
///
/// Everything [`wake`] does to Valve's client is global to the machine: it
/// makes and takes away one marker file, it stops and starts one process, and
/// it hands one credential to one login interface. Two callers doing that at
/// once — an install and a launch pressed seconds apart, which is an ordinary
/// thing to do — interleave into a client being started while it is being
/// stopped, and a marker withdrawn out from under a client that has not read
/// it yet.
///
/// The worker's own `waking` flag never covered this. It guards the explicit
/// launch wake and nothing else, so every background job went straight past it.
/// Holding this for the whole of a wake is what makes the second caller wait
/// for the first and then, nearly always, find the client already up and
/// return in microseconds.
///
/// **It is a lock on the machine and not on this program**, and it had to
/// become one. A `static Mutex` here excluded the threads of one process and
/// nothing else, while every sentence above is about the *user's* one client —
/// so a second session, or one of this crate's own `probe-*` examples run
/// against a live shell, went through all of it beside the first. See
/// [`crate::turns`].
const ONE_AT_A_TIME: crate::turns::What = crate::turns::What::TheClient;

/// The whole of what "Steam is available in the background" means, in one call:
/// start it if it is not running, wait for it to sign in, and — if it cannot do
/// that on its own — give it this session's credential through
/// [`crate::webui`]. A client of this session's that is already up and meets
/// `need` returns at once, so this is what every Steam-backed press asks for
/// and only the first one pays for.
///
/// ## Whose client it is
///
/// Every other question here is about what the client is *doing*; this one is
/// about which session it belongs to, and it is asked first because a client
/// on another display passes every other test there is. It is up, it is signed
/// in, it takes the URL and it starts the game — onto the desktop the user
/// left running behind this one. See [`in_this_session`].
///
/// A client that is not this session's is stopped and started again here, which
/// is the only thing that moves it: where it draws is fixed when it starts.
/// That is somebody's Steam being taken away, so it happens on evidence, never
/// on a guess — and, since it is somebody's, never without being asked. See
/// [`Permission`]: the ordinary call refuses with [`Refusal::NotOurs`] and the
/// shell puts the choice on the screen, and only the second call, made because
/// the person said yes, moves it.
///
/// **Whose account it is** is the same question one step in. A client signed in
/// to another household account answers its pipe, exposes its context and takes
/// every URL — and installs into, verifies and plays out of a library that is
/// not the one on the screen. It is found the same way and refused the same
/// way; see [`met`], which compares the account id in the client's own log with
/// the account this session holds a credential for.
///
/// ## What this exposes, and when
///
/// **A client this session starts is started ready to be spoken to**, whatever
/// it was started for. The marker goes up first, the client reads it as it
/// comes up, and it comes straight back down again — so the port is open for
/// the life of a client this shell started and is keeping out of sight, and a
/// Steam somebody starts for themselves next week comes up exposing nothing.
///
/// It was not always so, and the reason it is now is worth writing down. The
/// interface used to be opened only when something reached for it, which meant
/// the first install of every session found a client that was already up and
/// exposing nothing — and the only way to change that is to stop the client and
/// start it again, because the marker is read once on the way up. That restart
/// is what broke installing. Caught on a real machine: the client was asked to
/// shut down, did not let go of its pipe within [`UNTIL_IT_STOPS`], and the
/// press failed; the user pressed the row a second time, and only then — with
/// the client now stopped, so it could be started fresh — did it work.
///
/// Starting it right the first time costs one file that exists for a second.
/// Stopping somebody's Steam to open a port costs them whatever it was doing.
///
/// A client that was *already* running when this session found it is still the
/// old story: it is somebody else's, or it is one this session started before
/// its credential was wanted, and if the interface is genuinely needed there is
/// nothing for it but to start it again.
///
/// Blocking, and for as long as a minute and a half — and now, where another
/// session of this user's is already inside one, for as long as *its* wake
/// takes as well. Never call it on a thread that draws — see
/// [`crate::Steam::wake_client`], which is how the shell asks.
///
/// `request` is the number the shell gave whatever asked for this. Nothing here
/// decides on it; it goes into the lock's lease, so that a session waiting for
/// this one can say in its log which press it is waiting behind, and so that the
/// wait can be put beside a line in the audit log. See [`crate::turns::Behalf`].
pub fn wake(
    client: &Where,
    options: &Options,
    who: Credential<'_>,
    need: Need,
    permission: Permission,
    request: u64,
) -> Result<Proven, Refusal> {
    // One caller at a time, for the whole of it, across every process this user
    // is running. See [`ONE_AT_A_TIME`]: two presses seconds apart used to run
    // every step of this concurrently, and two *sessions* still did after that
    // was fixed within one.
    let _turn = crate::turns::take(
        ONE_AT_A_TIME,
        crate::turns::Behalf {
            backend: Some(options.root.clone()),
            request: Some(request),
        },
    );

    // Asked before anything else is, because it is the one thing a client can
    // be wrong about while looking perfectly right: up, signed in, meeting
    // every `need` there is, and attached to somebody else's display. See
    // `in_this_session`.
    let elsewhere = is_running(Some(client), options) && !in_this_session(options);

    // And the other way a running client is not this session's to use: it is
    // signed in, and to somebody else. Every test below this line would pass —
    // it answers its pipe, it exposes its context, it takes a `steam:` URL —
    // and what it would then do is install into, verify and play out of an
    // account whose library is not the one on the screen.
    // [`state_now`], not [`state`]: the account it names is read out of a log
    // that is appended across runs, so a client which is *starting* still
    // carries the run before's account in it — and that account is routinely
    // somebody else's, because the ordinary reason a client is starting is that
    // the last one was killed. Read raw, a press landing in those two seconds
    // was refused with "Steam is signed in to another account", about a client
    // that had not signed in to anything.
    let running = state_now(Some(client), options);
    let another_account = running.signed_in() && !running.signed_in_as(who);

    // Whether this client has ever been run. Read before anything here touches
    // the disk, because the first thing this does to it is make the root.
    //
    // It is not "does the directory exist": a first start that ran out of
    // patience leaves the empty directory this made behind, and a second press
    // is still the first run. What ends it is Valve's own furniture arriving.
    let first_run = !crate::library::looks_like_a_root(&options.root);

    let not_ours = elsewhere || another_account;
    if !not_ours && met(need, client, options, who) {
        // Asked here too, and that is the point of it being a function. The
        // guide button is meant to be settled on *every* wake, and this is the
        // path most wakes take — a client already up and already ours — so a
        // bare `return` here was the one way to reach a running client without
        // ever having asked it. See [`ask_about_the_guide_button`].
        ask_about_the_guide_button();
        // Proved on the way out, even here. This is the path most wakes take
        // and it is the one that used to answer `Ok(())` — a word about a
        // client, naming no client — so the press behind it had nothing to
        // check against when its own moment came. See [`Proven`].
        return prove(client, options, who);
    }

    // A client that is not ours is not taken over on a guess. Which of the two
    // it is decides what there is to say about it, and both are questions for
    // the person who pressed the button rather than decisions for this module.
    if not_ours && permission == Permission::AskFirst {
        let why = if elsewhere {
            NotOurs::new(
                "Steam is running in another session.",
                "Moving it here ends its downloads.",
            )
        } else {
            NotOurs::new(
                "Steam is signed in to another account.",
                "Moving it signs that account out.",
            )
        };
        tracing::info!(
            elsewhere,
            another_account,
            "Valve's client is not this session's to use"
        );
        return Err(Refusal::NotOurs(why));
    }

    // Two things this session's own reach decides, and both are decided here
    // rather than in [`bring_up`], because the client reads its own list as it
    // comes up and never looks again — after it is started it is too late for
    // either of them.
    //
    // A session that cannot reach Steam asks for Valve's Offline Mode. Not as a
    // last resort after the ordinary patience has run out: the wait it would be
    // spent on is a client reaching for the same network this session has
    // already failed to reach, and at the end of it the client has to be
    // started again anyway. See [`offline`].
    let want_it_offline = need == Need::Offline
        && !running.signed_in_as(who)
        && offline::ask_for(options, who.account);
    // And a session that *has* reached Steam takes back an Offline Mode it
    // asked for itself, so the next client comes up on Steam. Only where there
    // is no client running: the field is read on the way up, so changing it
    // under a client that is already going would do nothing until it was
    // restarted, and restarting one is how you end somebody's game.
    if need != Need::Offline && !running.running() {
        offline::give_back(options, who.account);
    }

    let mut ours = false;
    // Whether this call started a client, which decides how long the one that
    // comes up is given. Dating its log is not this variable's job any more —
    // see [`the_log_is_this_runs`], which asks the running client itself.
    let mut started_one = false;
    // Kept rather than returned on, so that a start which fails still reaches
    // the withdrawal below. Restarting a client that will not let go of its
    // pipe is the likeliest way for this to fail and it fails *after* the
    // marker is up, which without this would leave the next Steam somebody
    // starts for themselves exposing a debugging port nobody asked for.
    let mut up = Ok(());
    if want_it_offline && is_running(Some(client), options) {
        // A client that is up and cannot log on. Nothing else here would move
        // it: the mode is read once, and this one was started before it was
        // asked for.
        tracing::info!(
            "restarting Valve's client, which came up before Offline Mode was asked for"
        );
        started_one = true;
        up = restart(client, options);
    } else if elsewhere {
        // Started again rather than left alone, because there is nothing else
        // that would work: where a client draws is fixed when it starts, and
        // no URL handed to it afterwards can move it. The cost is real and
        // falls on the other session — its Steam goes, and any download with
        // it — and it is still the better half of the trade, because the other
        // half is a shell whose games silently open on a screen the person
        // pressing the button is not looking at.
        tracing::info!("Valve's client belongs to another session; starting it again in this one");
        ours = expose(options)?;
        started_one = true;
        up = restart(client, options);
    } else if !is_running(Some(client), options) {
        // Before it is started, not after: the client tests for this file as
        // it comes up and never looks again, so the order here is the whole of
        // why it works.
        ours = expose(options)?;
        started_one = true;
        up = start(client, options).map_err(|error| format!("Steam would not start: {error}"));
    }

    let woken: Result<bool, String> = up.and_then(|()| {
        bring_up(client, options, who, need, started_one, &mut ours)
            // A client being run for the first time is not a client that failed.
            // What it does with its first start is fetch and unpack Valve's
            // bootstrap, which is minutes of somebody's connection and none of it
            // visible from here, so it reliably outlasts the patience above. It
            // goes on doing it — it was started detached and nothing here stops it
            // — so what this says is what is true and what to do about it, rather
            // than the symptom, which is that a client that does not exist yet did
            // not sign in.
            .map_err(|why| {
                if first_run {
                    tracing::info!(%why, "Valve's client is still installing itself");
                    "Steam is setting itself up on this machine, which it does once. \
                 It will be ready in a few minutes."
                        .to_string()
                } else {
                    why
                }
            })
    });

    if woken.is_ok() {
        ask_about_the_guide_button();
    }

    // Whatever happened, the marker is spent: it is read as the client starts
    // and never again, so by now it has either done its work or is not going
    // to. Only what this call created is taken back — a marker somebody else
    // put there is theirs.
    if ours {
        crate::webui::withdraw(&options.root);
    }
    // What came up, named. `met` has already established the account inside
    // `bring_up`, and this asks the session half again as well — a client this
    // call restarted is a new process, and which session it draws into is
    // fixed as it starts rather than by what it was started for.
    //
    // And whether this call is what signed it in, carried on the proof: the
    // press behind it decides how to show the client by it. See
    // [`Proven::just_signed_in`].
    woken.map_err(Refusal::Failed).and_then(|just_signed_in| {
        prove(client, options, who).map(|proven| Proven {
            just_signed_in,
            ..proven
        })
    })
}

/// Ask Valve's client to leave the guide button to this shell, and find out
/// what it has left in its place.
///
/// The guide button is this shell's, and Valve's client answers it too unless
/// it is asked not to. See [`crate::webui::leave_the_guide_button_alone`],
/// which explains why this is the only pad on the machine that needs asking
/// rather than taking.
///
/// Best effort, and never the reason a press fails. Somebody who wanted to
/// start a game has started one; a Big Picture opening behind it is worth a
/// line in the log and nothing more. It is said on every wake rather than once,
/// because the client keeps this setting in memory and a client that was
/// restarted — or reinstalled, or is somebody's second machine — starts out
/// answering the button again.
///
/// A closed port refuses at once, so the wake that finds a signed-in client and
/// returns in microseconds still pays only a refused connection for this.
fn ask_about_the_guide_button() {
    match crate::webui::leave_the_guide_button_alone() {
        // Only once the first question has been answered, and that ordering is
        // not tidiness. Both are one expression over the same loopback socket
        // with the same patience, and this runs inside the wake a loading
        // screen is watching: a client whose interface accepts connections but
        // does not answer them would otherwise be waited out *twice* before the
        // press it was for got anywhere. One question already establishes
        // whether there is anybody there.
        Ok(()) => the_overlay_is_still_where_the_shell_thinks_it_is(),
        Err(why) => tracing::warn!(
            %why,
            "could not ask Valve's client to leave the guide button alone; \
             it may open Big Picture when the guide is pressed"
        ),
    }
}

/// Say so where Valve's overlay is not where a shell can reach it.
///
/// Taking the guide button leaves a hole: on every other machine shaped like a
/// console that button is what raises Steam's overlay. A shell fills it with a
/// chord, and a chord can only send a keystroke — the overlay lives inside the
/// game rather than in the client, so there is nothing to *ask*. Which
/// keystroke is a setting, and one the client keeps in memory and writes to no
/// file until it is changed, so this is the one moment it can be read: the
/// interface is open, and it is open because a game is being started.
///
/// Only a client that has moved it, or switched the overlay off, says anything.
/// The ordinary case is silent, because the ordinary case is Valve's default
/// and the shell's chord sending exactly that.
///
/// This does not *fix* anything and is not meant to. It is the difference
/// between a chord that does nothing and a chord that does nothing for a reason
/// somebody can read.
fn the_overlay_is_still_where_the_shell_thinks_it_is() {
    let Ok(key) = crate::webui::overlay_key() else {
        return;
    };
    if key.is_shift_tab() && key.enabled {
        return;
    }
    tracing::warn!(
        overlay = %key,
        "Valve's overlay is not on the keystroke this shell's controller chord sends; \
         the guide button held with Select will do nothing in a game"
    );
}

/// Tell the client to expose its interface, saying whether the marker had to be
/// made, and putting Valve's own wording on a Steam directory that will not
/// take one.
///
/// The directory is made where it is not there yet, which is the whole of what
/// a first start needs of this shell. A client installed and never run has no
/// root — it unpacks itself into one on its first start — and the marker has
/// to be down *before* that start, because the client reads it as it comes up
/// and never looks again. Making it is safe in both directions: this is the
/// path the client would have made itself, and a Steam that then unpacks into
/// it finds one empty directory and its own marker.
pub(crate) fn expose(options: &Options) -> Result<bool, String> {
    if !options.root.is_dir() {
        std::fs::create_dir_all(&options.root).map_err(|error| {
            format!(
                "Steam's directory could not be made at {}: {error}",
                options.root.display()
            )
        })?;
        tracing::info!(
            root = %options.root.display(),
            "made the directory Valve's client will unpack itself into"
        );
    }
    crate::webui::expose(&options.root)
        .map_err(|error| format!("{}: {error}", crate::webui::Problem::NotExposed))
}

/// From a client that is running to one that is everything `need` asks for.
///
/// `started_one` says whether this call started it, which decides how long to
/// wait: a cold client has a browser to start and a network to reach before it
/// will answer anything, where one that was already up has had its chance.
///
/// It used to carry the *moment* as well, for dating the client's own log
/// against — see [`the_log_is_this_runs`], which now dates it against the
/// running client instead and so needs nothing from up here.
///
/// Answers with whether it had to hand the client a credential — `false` for a
/// client that signed itself in, or was signed in already — because the caller
/// tells the shell, and the shell shows a client it has just signed in
/// differently. See [`Proven::just_signed_in`].
fn bring_up(
    client: &Where,
    options: &Options,
    who: Credential<'_>,
    need: Need,
    started_one: bool,
    ours: &mut bool,
) -> Result<bool, String> {
    // Most sessions end here. The client keeps its own credential and comes
    // back up signed in by itself, so all this does is watch it happen.
    //
    // Waiting is only worth it for what waiting can fix. Signing itself in is
    // one; opening its port is one *for a client of ours*, which was told to
    // before it was started. A client that was already running when this
    // session found it will never open a port it was never told to open, and
    // watching one for thirty seconds to establish that is thirty seconds of
    // somebody's loading screen.
    //
    // And one thing waiting can never fix: a client signed in to somebody
    // else. It will not become ours by being watched — it has to be told,
    // which is what everything below this does — so waiting on it is thirty
    // seconds of a loading screen spent establishing what is already known.
    let signed_in_to_somebody_else = {
        let now = state_now(Some(client), options);
        now.signed_in() && !now.signed_in_as(who)
    };
    // And the third thing waiting cannot fix, which is what a first login on a
    // new machine *is*: a client that has never signed anybody in. What the
    // wait below is for is a client coming back up on the credential it kept,
    // and one with no account in its registry kept none — so the whole
    // patience, ninety seconds of it on a client this call started, is spent
    // establishing something the registry answers in a syscall. It is then
    // handed the credential and signs in within five. See
    // [`account::could_sign_itself_in`].
    let could_sign_itself_in = account::could_sign_itself_in(&options.home);
    let worth_waiting = !signed_in_to_somebody_else
        && could_sign_itself_in
        && (started_one || need != Need::Context || crate::webui::reachable());
    if worth_waiting {
        let patience = match started_one {
            true => UNTIL_IT_ANSWERS,
            false => UNTIL_IT_SIGNS_ITSELF_IN,
        };
        if settles(need, client, options, who, patience) {
            return Ok(false);
        }
    }

    if need == Need::Offline {
        // Everything below this line ends in `SetLoginToken`, which is a
        // credential handed to *Steam* — and this path was taken because Steam
        // cannot be reached. So there is nothing left to try, and the honest
        // answer is the one Valve gives for the same case: Offline Mode is not
        // available to an account that has never signed in on this machine.
        return Err("Steam could not be reached, and its client would not start in Offline                     Mode. Offline Mode needs an account that has signed in on this machine                     while it had a connection."
            .to_string());
    }

    // It will not get there on its own, and everything past this point is said
    // through the interface. A client of ours is already exposing it; one that
    // was up before this session is not, and nothing short of starting it
    // again will change that.
    //
    // **Never one this call started**, and that guard is what makes skipping
    // the wait above safe. A client started here was started one line after
    // [`expose`], so its port is going to open — it may simply not have got
    // there yet, a second or two in. The wait used to cover that gap by
    // accident; without it, a client that cannot sign itself in reached this
    // line while it was still starting and was restarted on the spot, which is
    // a cold start paid twice. What it actually needs is patience for the
    // context, and [`sign_it_in`] has its own.
    if !started_one && !crate::webui::reachable() {
        *ours |= expose(options)?;
        tracing::info!("restarting Valve's client, which came up before it was told to expose it");
        restart(client, options)?;
        if settles(need, client, options, who, UNTIL_IT_ANSWERS) {
            return Ok(false);
        }
    }

    sign_it_in(client, options, who).map(|()| true)
}

/// Whether the client is already everything `need` asks for, **for this
/// account**.
///
/// The account half is not decoration. `State::SignedIn` carries whose it is
/// and this used to throw it away, which made "somebody is signed in" the whole
/// of what a press had to establish before it installed a game or started one.
fn met(need: Need, client: &Where, options: &Options, who: Credential<'_>) -> bool {
    // The same answer [`state_now`] gives, asked in the cheaper order. A state
    // that is not this account's is a no whatever the log's age turns out to
    // be, and finding out its age is a walk of `/proc` — so it is asked only of
    // a log that claims what is being waited for. This runs twice a second for
    // as long as a minute and a half.
    if !state(Some(client), options).signed_in_as(who) {
        return false;
    }
    // And then whose log it is, because the tail of it may be the run before's
    // — which is a wake that answers in half a second and a game that opens ten
    // seconds later. See [`the_log_is_this_runs`].
    if !the_log_is_this_runs(options) {
        return false;
    }
    match need {
        Need::SignedIn | Need::Offline => true,
        Need::Context => crate::webui::reachable(),
    }
}

/// Wait for the client to become everything `need` asks for by itself.
///
/// Everything, and not just the signing-in half. Both halves are things a
/// client of this session's does on its own as it starts — it signs itself in
/// from the credential it kept, and it opens its port because it was told to
/// before it was started — and they do not finish in a fixed order. Asking for
/// one and then testing the other once was how a client that had opened its
/// port a second later got taken for one that never would, and answered with a
/// restart it did not need.
fn settles(
    need: Need,
    client: &Where,
    options: &Options,
    who: Credential<'_>,
    patience: Duration,
) -> bool {
    let deadline = Instant::now() + patience;
    while Instant::now() < deadline {
        if met(need, client, options, who) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    false
}

/// Whether the tail of the connection log was written by the client that is
/// running **now**.
///
/// **It was not, for the first second or two of a client's life, and reading it
/// anyway is a wake that answers before the client has signed in.**
/// `connection_log.txt` is appended across runs and never truncated — six
/// hundred runs of it on this machine — so until the client that is coming up
/// writes its own `Client version:` line, the tail still belongs to the run
/// before. [`last_stamp`] therefore reads *that* run's account, and
/// [`logged_on_in`] answers with it.
///
/// [`RUN_ENDED`] already closes half of this: a client that shut down cleanly
/// writes `Log session ended` as its last line, and the stamp is refused. **A
/// client that was killed writes no such line** — and being killed is the
/// ordinary end of a client this shell started, because it is started inside
/// the session and dies with it.
///
/// Measured on this machine on 2026-09-02, sampling five times a second: a
/// client was killed at 21:27:33 with `[Logged On, 4, 7]` as its last line, and
/// on the next start the pipe was held at 21:28:11.4 while the log's last run
/// marker still read 21:27:31 — **2.1 seconds** in which `state` answered
/// `SignedIn` for a client that had not reached Steam. Seen in the wild first:
/// a press whose wake reported ready in 501 ms, and the game opened ten seconds
/// later because Valve's client had queued the request rather than refused it.
///
/// **Dated against the running client itself, and it used to be dated against
/// the moment the wake started one.** That was cheaper — one `stat`, no walk —
/// and it left the whole question unanswered for every client this session did
/// *not* start, because there is no such moment for one of those. Which is
/// precisely the client the account half is about: one somebody else started,
/// whose log tail may be a third run's. A press landing in the first seconds of
/// a client's life was refused with *"Steam is signed in to another account"*,
/// which is alarming, wrong, and about a client that had not signed in to
/// anything yet.
///
/// **Unsure counts as this run's**, as everywhere else here: a log or a process
/// that cannot be looked at is answered by reading what the state says, which
/// is what happened before any of this existed.
fn the_log_is_this_runs(options: &Options) -> bool {
    written_since(&options.root, the_running_client_started(options))
}

/// Whether the connection log has been written to since a moment, with the
/// comparison kept apart from the two readings so it can be tested without a
/// client or a `/proc`.
fn written_since(root: &Path, started: Option<std::time::SystemTime>) -> bool {
    let Some(started) = started else {
        return true;
    };
    let written = std::fs::metadata(root.join("logs").join(CONNECTION_LOG))
        .and_then(|it| it.modified())
        .ok();
    match written {
        Some(written) => written >= started,
        None => true,
    }
}

/// When the process holding the pipe started.
///
/// From `/proc/<pid>/stat`'s start time **and `/proc/uptime`**, rather than
/// from `/proc/stat`'s `btime`, which is the obvious route and is a second
/// wrong: `btime` is whole seconds and a machine does not boot on a second
/// boundary. Measured here — `btime` put a process 0.87 s before its true
/// start, where the age below put it 0.3 ms after. A second of error is the
/// wrong size for a question about two seconds.
///
/// **A clock that steps, and what is left of it.** The route this replaced held
/// a wall-clock moment captured when the wake started, so a clock stepping
/// *backwards* — the ordinary shape of one, an NTP correction as a session
/// comes up — put every later log line before it and the wake spent its whole
/// patience refusing a perfectly good log. Here the moment is recomputed at
/// each look out of an age, so a backwards step carries it down with the log
/// and the answer does not move.
///
/// A step *forwards* still costs, and there is no fixing it from this side: the
/// log line carries the clock as it read when the line was written, and nothing
/// on the disk says what the clock was then. It costs one line rather than a
/// wake, though — the client writes its next one within seconds, that one
/// carries the new clock, and the guard opens.
///
/// The walk in [`holder_of`] is what this costs — 24 ms of reading every
/// process's descriptors — which is why it is asked once per press, on the
/// worker, and only where the log claims a sign-in worth checking. See
/// [`state_now`].
fn the_running_client_started(options: &Options) -> Option<std::time::SystemTime> {
    let pid = holder_of(&options.home.join("steam.pipe"))?;
    let age = age_of(Path::new("/proc"), pid)?;
    std::time::SystemTime::now().checked_sub(age)
}

/// How long that process has been alive, below a directory standing in for
/// `/proc` so a test can build one.
///
/// The start time is the twenty-second field of `stat`, in clock ticks since
/// the machine booted, and the parse starts after the **last** `)`: the second
/// field is a program name in brackets and a program may be called `a) b (c`.
fn age_of(proc: &Path, pid: u32) -> Option<Duration> {
    let stat = std::fs::read_to_string(proc.join(pid.to_string()).join("stat")).ok()?;
    let ticks: f64 = stat[stat.rfind(')')? + 1..]
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()?;
    let uptime = std::fs::read_to_string(proc.join("uptime")).ok()?;
    let uptime: f64 = uptime.split_whitespace().next()?.parse().ok()?;
    // Hundredths on every Linux this runs on, and asked rather than assumed.
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    let seconds = uptime - ticks / (hz as f64).max(1.0);
    Duration::try_from_secs_f64(seconds.max(0.0)).ok()
}

/// [`state`], with the account half thrown away where the log carrying it
/// belongs to the run before.
///
/// The honest answer for a client that is up and has not yet said who it is, is
/// [`State::Starting`] — which is what this returns rather than a `SignedIn`
/// naming somebody who is not signed in here.
///
/// Everything that acts on *whose* client it is goes through this. Everything
/// that asks only whether one is running goes through [`is_running`], which
/// costs one syscall. `state` itself is left for the rare caller that wants the
/// file read exactly as it stands.
pub fn state_now(client: Option<&Where>, options: &Options) -> State {
    let state = state(client, options);
    // The walk is paid only here, and only where there is something to be
    // wrong about: a client that is not running, or one whose log says nothing
    // about an account, has nothing for this to take away.
    if !state.signed_in() || the_log_is_this_runs(options) {
        return state;
    }
    tracing::debug!(
        ?state,
        "Valve's client has not written its own first line yet; that account is the run before's"
    );
    State::Starting
}

/// Hand a running, exposed client this session's credential, and wait for
/// Steam to agree.
///
/// Reached only by a client that would not sign itself in — the first time, or
/// after the token it kept has expired.
fn sign_it_in(client: &Where, options: &Options, who: Credential<'_>) -> Result<(), String> {
    // Wait for it to be able to answer at all. Its own JS context is the thing
    // that has to be there, and it is the last of the client to come up — so
    // waiting for that is waiting for all of it.
    let deadline = Instant::now() + UNTIL_IT_ANSWERS;
    loop {
        if met(Need::Context, client, options, who) {
            return Ok(());
        }
        match crate::webui::sign_in(who.account, who.refresh_token) {
            Ok(()) => break,
            // Not up yet, or up and not finished building itself. A context
            // that is listed is not the same as one that has `SteamClient` in
            // it, and reaching for a method that is not there yet throws —
            // which arrives here as a refusal and is not one. Everything else
            // is a real answer and stops, because trying it again for a
            // minute would only say the same thing later.
            Err(
                crate::webui::Problem::NotExposed
                | crate::webui::Problem::NoContext
                | crate::webui::Problem::Unreachable(_)
                | crate::webui::Problem::NotReady(_),
            ) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_secs(2));
            }
            Err(problem) => return Err(problem.to_string()),
        }
    }

    // It took the credential. Whether Steam agrees is a separate question, and
    // the client's own log is what answers it.
    // As *this* account, and not merely signed in. A client that was already
    // signed in to somebody else takes the credential and goes on saying it is
    // signed in for as long as it takes to swap, and a wait that asked only
    // whether somebody was signed in would come back at once with the wrong
    // answer and hand the next press their library.
    let deadline = Instant::now() + UNTIL_IT_SIGNS_IN;
    while Instant::now() < deadline {
        if met(Need::SignedIn, client, options, who) {
            tracing::info!(
                account = who.account,
                "Valve's client is signed in and out of sight"
            );
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    Err("Steam took the credential but did not sign in to this account with it.".to_string())
}

/// Whether a client is running at all, without asking whose it is.
///
/// **The cheap half of [`state`], split out because most callers want only
/// this.** One `open` on a FIFO and nothing else: no log to read, no account to
/// parse. `state(…).running()` is exactly this and was what the shell asked on
/// every frame — sixty-four kilobytes of somebody's connection log read and
/// scanned backwards, per tick, on the thread that draws, to answer a question
/// a single syscall answers. See [`crate::Steam::client_is_running`], which is
/// where that was being paid.
///
/// Splitting it is also what lets the *expensive* half afford to be exact.
/// Asked once per press rather than once per frame, the account question can
/// pay for the walk that dates the log — see [`state_now`].
pub fn is_running(client: Option<&Where>, options: &Options) -> bool {
    client.is_some() && running(&options.home)
}

/// What the client is doing, read from the disk.
///
/// Cheap enough to poll: a small file, a directory entry, and the tail of a
/// log. Nothing here starts a process or touches the network.
///
/// **The account half of this can be the run before's** — the log is appended
/// across runs — so anything acting on *whose* client it is wants [`state_now`]
/// rather than this, and anything asking only whether one is running wants
/// [`is_running`].
pub fn state(client: Option<&Where>, options: &Options) -> State {
    if client.is_none() {
        return State::Absent;
    }
    if !running(&options.home) {
        return State::Stopped;
    }
    let log = tail_of_the_connection_log(&options.root).unwrap_or_default();
    if let Some(account_id) = logged_on_in(&log) {
        return State::SignedIn(account_id);
    }
    // Not on Steam, which used to be the end of it. A client in Valve's own
    // Offline Mode never reaches the state above and never will: it logs on to
    // this machine alone, writes `Logged Off` in that log for the rest of its
    // run, and plays every game on the disk perfectly. Read off this machine on
    // 2026-09-02 — a client started with the mode on was logged on for its
    // account within a second, and `steam://rungameid/…` started a game — while
    // this function answered `Starting` for as long as it was up.
    //
    // Two facts and not one, and both are needed. The log says which account
    // the client that is *running* is acting for; the client's own list says
    // whether that account asked for Offline Mode. Either alone is a client
    // this shell would hand somebody else's library.
    match (acting_for_in(&log), offline::account_wanting_it(options)) {
        (Some(acting), Some(offline)) if acting == offline => State::Offline(acting),
        _ => State::Starting,
    }
}

/// Whether a client is alive and listening.
///
/// Its own pipe, and not its own pid file. The pid file looks like the obvious
/// answer and is a trap: *every* invocation of `steam` writes its own pid into
/// it, including `steam -shutdown` and the one-shot `steam steam://…` that
/// hands a URL over and exits. Both of those are called from this very module,
/// so a shell that trusted the pid file would ask "is the client up?", see its
/// own half-second helper process, and say yes.
///
/// The pipe cannot be wrong about it. `~/.steam/steam.pipe` is a FIFO that a
/// running client holds open for reading and nothing else does; opening it for
/// writing without blocking succeeds when somebody is listening and fails with
/// `ENXIO` when nobody is. It is what Valve's own launcher script asks, it
/// costs one syscall, and it is true the instant the client goes away.
pub(crate) fn running(home: &Path) -> bool {
    use std::os::unix::fs::OpenOptionsExt;

    // Without O_NONBLOCK this would block until a reader arrived, which on a
    // machine with no client running is for ever.
    std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(home.join("steam.pipe"))
        .is_ok()
}

/// The status Valve's client keeps for this account on this machine.
///
/// **The one place a status on this machine is written down, and it is Valve's
/// own.** The client records the state its friends menu is set to in that
/// account's user config — `FriendStoreLocalPrefs_<account>`, a small JSON
/// document escaped into a VDF string — and comes back up wearing it. So it is
/// two things at once: what a client here *would* announce, which is what the
/// shell's panel has to be showing to be telling the truth, and the readback
/// for a status handed over as a URL, which is otherwise unanswerable without
/// the debugging marker no user-started client has.
///
/// Measured on this machine on 2026-09-03, on a client started cold with no
/// marker anywhere. The field read `1` on the way up; a
/// `steam://friends/status/invisible` was handed over four seconds after the
/// client signed in, and the field read `7` four seconds after that. Away
/// moved it to `3`, and Online back to `1`.
///
/// **Offline is the exception, and is not recorded.** The same measurement sent
/// `steam://friends/status/offline` and the field did not move off `3` — going
/// offline is the client leaving the friends network rather than a state it
/// remembers being in. Which is the useful way round: a shell that adopts this
/// can never be talked into going offline, and a client that comes back up
/// after one comes back up online.
pub fn recorded_status(options: &Options, account_id: u32) -> Option<crate::friends::Presence> {
    let config = options
        .root
        .join("userdata")
        .join(account_id.to_string())
        .join("config")
        .join("localconfig.vdf");
    let raw = std::fs::read_to_string(config).ok()?;
    let key = format!("FriendStoreLocalPrefs_{account_id}");
    persona_state_in(raw.lines().find(|line| line.contains(&key))?)
}

/// The number out of one of those lines.
///
/// Its own function so the parse can be tested without a client's config on the
/// disk. The value is JSON that has been escaped to live inside a VDF string,
/// so what is on the line is `\"ePersonaState\":7` — backslashes and all — and
/// unescaping the whole of it to reach one digit would be a JSON parser and a
/// VDF parser for a field that is a single number.
fn persona_state_in(line: &str) -> Option<crate::friends::Presence> {
    let at = line.find("ePersonaState")?;
    let digits: String = line[at..]
        .chars()
        .skip_while(|character| !character.is_ascii_digit())
        .take_while(char::is_ascii_digit)
        .collect();
    crate::friends::Presence::from_number(digits.parse().ok()?)
}

/// What decides which session a process draws into.
///
/// Both, and not either on its own. Valve's client reaches the screen through
/// Xwayland and its games may reach it either way — a Proton title uses Wine's
/// X11 driver, a native one is as likely to be Wayland — so a client that
/// matched on one of these and not the other would be a client whose games can
/// still come up somewhere nobody is looking.
///
/// They are also the only two variables read out of another process here. The
/// environment of Valve's client holds rather more than this, none of which is
/// any of the shell's business, so [`displays_in`] keeps these and drops the
/// rest as it parses rather than copying a stranger's environment about.
pub(crate) const DRAWS_INTO: [&str; 2] = ["WAYLAND_DISPLAY", "DISPLAY"];

/// Whether the client that is running belongs to *this* session.
///
/// The question exists because `~/.steam/steam.pipe` is one path in one home
/// directory and every session on the machine shares it. [`running`] is
/// therefore true of a client on any of them, and a shell that stopped there
/// hands `steam://rungameid/…` to whichever client happens to be listening. It
/// is not refused and nothing fails: Steam starts the game perfectly, onto the
/// display *that* client is attached to, and this session waits out its
/// patience for a window that was never coming here.
///
/// Which is the one failure in this module that leaves no trace anywhere. The
/// game's own logs show a clean launch, Steam's show a clean handover, and the
/// only thing that is wrong is the screen it went to — so it reads to the user
/// as a game that did not start, and to anybody reading the logs afterwards as
/// a game that did.
///
/// **Unsure counts as ours.** Every `None` here means the shell could not find
/// out — no process holding the pipe, an environment it may not read — and the
/// two ways of being wrong are not equal. Treating a client of ours as foreign
/// stops somebody's Steam and takes their download with it; treating a foreign
/// one as ours costs a loading screen. So the destructive answer is only ever
/// given on evidence.
///
/// Public because it is the one thing about a misdelivered launch that can be
/// checked from outside — `probe-client` reports it — and because there is no
/// other way to find out afterwards: everything either side of it looks right.
pub fn in_this_session(options: &Options) -> bool {
    let Some(pid) = holder_of(&options.home.join("steam.pipe")) else {
        return true;
    };
    let Ok(environ) = std::fs::read(format!("/proc/{pid}/environ")) else {
        return true;
    };
    let theirs = displays_in(&environ);
    let ours: Vec<Option<String>> = DRAWS_INTO
        .iter()
        .map(|name| std::env::var(name).ok())
        .collect();
    if theirs == ours {
        return true;
    }
    tracing::info!(
        pid,
        ?theirs,
        ?ours,
        "Valve's client is drawing into another session"
    );
    false
}

/// The process listening on that pipe, if it can be found.
///
/// Found by walking `/proc` rather than by reading the pid file, for the reason
/// [`running`] gives: the pid file names whichever `steam` ran last, which is
/// routinely one of this module's own one-shot helpers.
///
/// Only a process holding it **open for reading** counts, which is what makes
/// this the client and not a passer-by. A FIFO has writers as well, and this
/// module produces them — [`running`] opens it for writing on every poll, and
/// so does every `steam steam://…` that hands a URL over — so a scan that took
/// the first process it found holding the pipe would sooner or later find one
/// of ours, read our own environment out of it, and conclude that whatever is
/// running is ours by definition.
fn holder_of(pipe: &Path) -> Option<u32> {
    let pipe = std::fs::canonicalize(pipe).ok()?;
    for process in std::fs::read_dir("/proc").ok()?.flatten() {
        let Some(pid) = process
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        // Not readable for a process belonging to another user, which is a
        // process this shell has no business asking about anyway.
        let Ok(open) = std::fs::read_dir(process.path().join("fd")) else {
            continue;
        };
        for handle in open.flatten() {
            // A descriptor can close between the two calls, so neither the
            // link nor the flags being unreadable means anything but "not this
            // one".
            if std::fs::read_link(handle.path()).is_ok_and(|target| target == pipe)
                && opened_for_reading(&process.path(), &handle.file_name())
            {
                return Some(pid);
            }
        }
    }
    None
}

/// Whether one open descriptor of one process was opened for reading.
///
/// `/proc/<pid>/fdinfo/<fd>` carries the flags the descriptor was opened with,
/// in octal, which is the only place the read/write half of it is written down
/// — the symbolic link in `fd/` names the file and says nothing about how it
/// is held.
fn opened_for_reading(process: &Path, handle: &std::ffi::OsStr) -> bool {
    let Ok(info) = std::fs::read_to_string(process.join("fdinfo").join(handle)) else {
        return false;
    };
    info.lines()
        .filter_map(|line| line.strip_prefix("flags:"))
        .filter_map(|flags| u32::from_str_radix(flags.trim(), 8).ok())
        .any(|flags| flags & (libc::O_ACCMODE as u32) == (libc::O_RDONLY as u32))
}

/// [`DRAWS_INTO`], read out of the `NUL`-separated block `/proc/<pid>/environ`
/// is, in that order, and nothing else out of it.
///
/// An absent variable stays absent rather than becoming empty: a session with
/// no Xwayland sets no `DISPLAY` at all, and it must not compare equal to one
/// whose `DISPLAY` is the empty string.
fn displays_in(environ: &[u8]) -> Vec<Option<String>> {
    let named = |wanted: &str| {
        environ
            .split(|byte| *byte == 0)
            .filter_map(|entry| std::str::from_utf8(entry).ok())
            .filter_map(|entry| entry.split_once('='))
            .find(|(name, _)| *name == wanted)
            .map(|(_, value)| value.to_string())
    };
    DRAWS_INTO.iter().map(|name| named(name)).collect()
}

/// The account the client is signed in as, if it is signed in.
///
/// There is no file that says this and no interface to ask, so it is read from
/// the client's own connection log. Every line it writes carries two things:
/// the state it is in and the Steam account it is acting for, in that order —
///
/// ```text
/// [2026-08-12 12:41:24] [Logged Off, 4, 0] [U:1:82105993] LogOn() called…
/// [2026-08-12 12:41:25] [Logged On, 4, 7]  [U:1:82105993] RecvMsgClientLogOnResponse() : processing complete
/// ```
///
/// — and **only the first of those says whether it is signed in**. This used to
/// read the account stamp alone, and the two lines above are why that was
/// wrong: a client two seconds into starting has already written the account it
/// is *about* to log on as, so the shell called it signed in while it was still
/// connecting. Every press then went to a client that could not answer it. Read
/// off a real client, the id appears a full second before the logon completes,
/// and on a cold one considerably more.
///
/// The log is also appended to across runs, so the stamp left by a client that
/// was shut down an hour ago is still the last one in the file. That is why
/// only the current run is looked at: `Client version:` is the first line the
/// client writes as it comes up, so anything before the last one belongs to a
/// client that is no longer there.
///
/// Only the tail is read. The file runs to megabytes over a few weeks and this
/// is polled while somebody is watching a loading screen.
fn tail_of_the_connection_log(root: &Path) -> Option<String> {
    tail_of(&root.join("logs").join(CONNECTION_LOG))
}

/// The log the client stamps with what its connection is doing and whose it is.
const CONNECTION_LOG: &str = "connection_log.txt";

/// What the client says it is in the middle of, for one game.
///
/// The phases are Valve's own words, read off this machine: `Reconfiguring`,
/// `Preallocating`, `Downloading,Staging`, `Verifying Installed`,
/// `Verifying Staged`, `Staging`, `Committing`, `Running Script`, and any of
/// them with `Stopping` appended — two thousand of them in one machine's logs.
///
/// Two, and not nine, because the shell has words for two. What a row can say
/// is [`crate::library::Standing`], and everything below is one of its states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InHand {
    /// `Verifying Installed`: reading back what is on the disk and checking it
    /// against what should be there.
    ///
    /// Its own answer because it is the one phase that is long, silent, and
    /// **not** an update — it moves no byte, writes nothing to the manifest,
    /// and on a large game runs for minutes. A row that called it "Updating"
    /// would be making the mistake `library::standing_from` already refuses to
    /// make: a check on a 70 GB game is not a download.
    ///
    /// `Verifying Staged` is deliberately *not* this. That one checks what has
    /// just arrived, so it happens inside a download the manifest is already
    /// describing, and the row has nothing to learn from it.
    Checking,
    /// Any other part of a job.
    Working,
}

/// Which of the client's three job tracks a line belongs to.
///
/// Not one list, because the two questions this answers want different halves
/// of it. A shader cache must never reach a row — the game is on the disk and
/// plays perfectly while Steam fetches one — and it must equally not be
/// interrupted, because one of them ran for **sixty-four minutes** on this
/// machine. Measured over both of this machine's content logs, 2026-09-02:
///
/// ```text
///            jobs   median      p90       max     total
/// App         290       6s     108s     2916s      5.3h
/// Workshop    353       1s       1s       41s      0.1h
/// Shader      439       3s      14s     3832s      1.8h
/// ```
///
/// So shaders are seconds nearly always, which is what makes counting them for
/// the shutdown cost nothing, and hours occasionally, which is what makes
/// counting them worth it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Track {
    /// The game's own files.
    App,
    /// Content somebody subscribed to for it.
    Workshop,
    /// A cache of compiled shaders. Steam's own housekeeping, fetched while the
    /// game is being played and rebuilt whenever it likes.
    Shaders,
}

impl Track {
    /// Whether this is the game itself, which is what a row may speak about.
    fn is_the_game(self) -> bool {
        matches!(self, Track::App | Track::Workshop)
    }

    /// The words the client writes in front of the phase.
    fn said(self) -> &'static str {
        match self {
            Track::App => " App update changed : ",
            Track::Workshop => " Workshop update changed : ",
            Track::Shaders => " Shader update changed : ",
        }
    }
}

/// Every track, for the walk that decides which one a line is.
const TRACKS: [Track; 3] = [Track::App, Track::Workshop, Track::Shaders];

/// What Valve's client has in hand, kept up to date from its own log.
///
/// **The manifests do not say.** `appmanifest_<id>.acf` picks up a working bit
/// when bytes move and at no other time, and there are whole operations that
/// move none: measured on this machine on 2026-09-02, a file check driven
/// through `steam://validate` ran for **thirty-six seconds** on a 15 GB game
/// with the client's own log reading
/// `App update changed : Running Update,Verifying Installed,` throughout, while
/// the file on the disk said `StateFlags 4` and was never rewritten — a watcher
/// sampling it twice a second saw no change at all. A shell that asks only the
/// manifests therefore believes nothing is happening: it draws a row saying
/// "Installed" over a game Steam has in hand, answers a press on it with a
/// launch the client will queue behind the check, and shuts the client down in
/// the middle of the very work it asked for.
///
/// So this is the third source about a game, beside the manifest and beside
/// whatever this session started itself. It needs no account and no interface,
/// and answers for work begun in Steam's own window as readily as for work
/// begun here.
///
/// **Only the current run of the client counts.** [`NEW_RUN`] is written as the
/// first line of every run, and a client killed in the middle of an update
/// never writes the line that ends it — so without that boundary a session that
/// lost its Steam once would go on believing that update was in flight for
/// ever, and nothing would ever close a client again. See [`Jobs::fold`].
///
/// **Stateful, because the log is a stream and not a state.** The first cut of
/// this read the last 64 KB every time and folded what it found, which is right
/// until a job outlives its own line: the client writes `App update changed`
/// only when the job moves between phases, and it writes a great deal else in
/// between — cache connections, schedulers, the other two tracks — so a long
/// job's last line is pushed out of the window and the job reads as finished.
/// Here it is read the way it is written: forward, once, from where the last
/// look stopped.
#[derive(Debug, Default)]
pub struct Jobs {
    /// How far into the log this has read. Bytes, and always the end of a whole
    /// line — a client writing while this reads leaves a half-written last one,
    /// and folding half a line and then skipping the rest of it would lose the
    /// only word that mattered.
    read_to: u64,
    /// And *which file* that offset is into: the device and inode the last look
    /// read from.
    ///
    /// A position alone is not a place. The client rotates this log at four
    /// megabytes by renaming it and opening a new one at the same path, so the
    /// path stops meaning the file the offset was measured against — and the
    /// only cut of this that asked whether the file had gone backwards missed
    /// every rotation the new file had already grown past the old offset by.
    /// It then went on reading from that offset into a file it had never seen
    /// the start of, which is to say it skipped whatever was written before it
    /// and folded a stream it had cut in half.
    file: Option<(u64, u64)>,
    /// And what everything up to there said. Keyed by the track as well as the
    /// app, because the three run at once: a game whose shader cache finishes
    /// downloading has not finished updating.
    doing: BTreeMap<(Track, u32), InHand>,
}

impl Jobs {
    /// Read whatever the client has written since the last look, and say
    /// whether what it has in hand changed.
    pub fn look(&mut self, root: &Path) -> bool {
        let before = self.doing.clone();
        self.read(&root.join("logs").join(CONTENT_LOG));
        self.doing != before
    }

    /// Forget all of it, for a client that is no longer running.
    ///
    /// A log is a record of what the client *was* doing, exactly as a manifest
    /// is — see [`crate::library::Game::update_outstanding`] — and with nothing
    /// running there is nothing doing any of it. The offset goes too, so the
    /// next client is read from its own first line.
    pub fn forget(&mut self) -> bool {
        let had = !self.doing.is_empty() || self.read_to != 0;
        self.read_to = 0;
        self.file = None;
        self.doing.clear();
        had
    }

    /// Whether the client has anything at all in hand, of any kind.
    ///
    /// The question the two rules that close a client ask, and they ask it of
    /// all three tracks: a shader cache is none of a row's business and is
    /// still not something to shut a client down under. See [`Track`].
    pub fn anything_in_hand(&self) -> bool {
        !self.doing.is_empty()
    }

    /// And what it is doing to one game's own content, in the two words a row
    /// has for it — or nothing, which is what a shader cache always is here.
    pub fn to_the_game(&self, app_id: u32) -> Option<InHand> {
        let mut found = None;
        for (&(track, id), &phase) in &self.doing {
            if id != app_id || !track.is_the_game() {
                continue;
            }
            // A check is the more particular word, so it wins where the two
            // tracks disagree.
            if phase == InHand::Checking {
                return Some(InHand::Checking);
            }
            found = Some(phase);
        }
        found
    }

    /// Whether the client is fetching this game's **shader cache**, which is
    /// the one thing it does to a game that [`Self::to_the_game`] will not
    /// speak about.
    ///
    /// Kept out of that answer on purpose and still kept: the game is on the
    /// disk and plays perfectly while Steam fetches one, so a row saying
    /// "Updating" over it would be describing housekeeping nobody asked for.
    /// See [`Track`].
    ///
    /// What this is for is the one moment when it is the answer to a question
    /// somebody is actually asking. Read off this machine on 2026-09-03: a
    /// press on Counter-Strike 2 woke the client, and what the client did with
    /// it was fetch **five gigabytes of shader cache** — `Shader update
    /// changed : Running Update,Downloading,Staging,` against
    /// `update started : download 0/5191457952` — while the game's own update
    /// waited behind it. Every other source was silent, so the guide had
    /// nothing in the corner and the loading screen had nothing to say.
    pub fn fetching_shaders_for(&self, app_id: u32) -> bool {
        self.doing.contains_key(&(Track::Shaders, app_id))
    }

    /// Everything in hand, for a probe that has to show its working.
    pub fn each(&self) -> impl Iterator<Item = (Track, u32, InHand)> + '_ {
        self.doing
            .iter()
            .map(|(&(track, id), &phase)| (track, id, phase))
    }

    /// Read whatever is there to read, wherever this look finds itself.
    ///
    /// Three situations, and the whole of the difficulty is telling them
    /// apart. A steady look reads what has arrived since the last one. A
    /// **first** look has to find the start of the run it is joining, which may
    /// be anywhere behind it. And a **rotation** moves the file the offset was
    /// measured against out from under both of them.
    ///
    /// Nothing here throws away what the client has in hand. A job is ended by
    /// the client saying so — see [`Jobs::fold`] — or by a new run of the
    /// client, or by there being no client at all, which is [`Jobs::forget`]
    /// and is the caller's to say. A log rolling over is none of those: the
    /// work goes on across it, and the client writes nothing to say so because
    /// nothing about the work has changed.
    fn read(&mut self, path: &Path) {
        let Ok(file) = std::fs::File::open(path) else {
            // No log at that path this instant. That is not the client
            // dropping what it had: rotation renames the file and opens
            // another, and for the moment in between there is nothing here to
            // open. What is lost is the position, so the next look reads the
            // file it finds from the beginning; what the client has in hand is
            // not this reader's to forget. A client that is actually gone is
            // answered by the caller — see [`Jobs::forget`].
            self.read_to = 0;
            self.file = None;
            return;
        };
        let Ok(meta) = file.metadata() else {
            return;
        };
        let identity = (meta.dev(), meta.ino());
        let length = meta.len();

        let from = match self.file {
            // The same file as last time, and the ordinary case.
            Some(seen) if seen == identity => {
                if length == self.read_to {
                    return;
                }
                // Shorter than it was, with the file itself unchanged: the log
                // has been emptied in place rather than moved aside. What was
                // read is gone and what is here is to be read from its start.
                match length < self.read_to {
                    true => 0,
                    false => self.read_to,
                }
            }
            // A different file at the same path: rotated. Whatever the client
            // wrote to the old one after the last look went with it, and it is
            // still on the disk under another name — so that is read first,
            // and then this one from its start.
            Some(seen) => {
                self.catch_up_with_the_rotated(path, seen);
                0
            }
            // The first look of this run, which is the one that has to find
            // where the run began. See [`Jobs::attach`].
            None => {
                self.attach(path, file, identity);
                return;
            }
        };
        // Both recorded before the read, so that a look that finds nothing
        // whole to fold — a log the client has opened and not yet written a
        // line into — still leaves this reader pointing where it is reading
        // from rather than at the file before it.
        self.file = Some(identity);
        self.read_to = from;
        let Some((bytes, whole)) = whole_lines_from(file, from) else {
            return;
        };
        self.read_to = from + whole as u64;
        self.fold(&String::from_utf8_lossy(&bytes[..whole]));
    }

    /// Join a client that is already running, from the start of the run it is
    /// in the middle of.
    ///
    /// **The whole of this run, and not a window on the end of it.** The first
    /// cut read back a megabyte, which is a length and not a boundary, and it
    /// is wrong in both directions: a job whose last phase line is further back
    /// than that — a check on a large game, an update sitting in one phase
    /// while the client fills the log with cache connections — is missing from
    /// the state this attaches with, so the shell believes a game Steam has in
    /// hand is idle; and a run marker further back than that is a window that
    /// may open in the middle of a *previous* run, whose unfinished jobs are
    /// then folded in as though they were this client's.
    ///
    /// So the run is found rather than guessed at: [`NEW_RUN`] is the first
    /// line of every run, and the last one in the file is where this client's
    /// own account of itself begins. Where there is none, the run began before
    /// the log was rotated, and the rest of it is in the file that was moved
    /// aside — which is read first, exactly as far back as its own last run
    /// marker.
    ///
    /// Once per run of the client. A whole log is four megabytes at the
    /// outside, because that is where the client rotates it.
    fn attach(&mut self, path: &Path, file: std::fs::File, identity: (u64, u64)) {
        let Some((bytes, whole)) = whole_lines_from(file, 0) else {
            return;
        };
        // The identity is the open file's own, so what is remembered is the
        // file these bytes came out of and not whatever is at the path by the
        // time the fold is over.
        self.read_to = whole as u64;
        self.file = Some(identity);

        match this_run_in(&bytes[..whole]) {
            // The run began in this file, so nothing before that mark is this
            // client's.
            Some(at) => self.fold(&String::from_utf8_lossy(&bytes[at..whole])),
            // It did not, so the beginning of it is in the log the client moved
            // aside. Read that one first and this one after it, in the order
            // the client wrote them.
            None => {
                if let Some(before) = std::fs::File::open(rotated(path))
                    .ok()
                    .and_then(|file| whole_lines_from(file, 0))
                {
                    let (bytes, whole) = before;
                    let at = this_run_in(&bytes[..whole]).unwrap_or_default();
                    self.fold(&String::from_utf8_lossy(&bytes[at..whole]));
                }
                self.fold(&String::from_utf8_lossy(&bytes[..whole]));
            }
        }
    }

    /// Read the end of the log that has just been rotated away, which is the
    /// stretch this reader never saw.
    ///
    /// Between one look and the next the client goes on writing, and if the
    /// rotation falls in that gap those lines are in the file it moved aside
    /// rather than in the one at the path. They are exactly the lines that say
    /// a job ended, so without this a job that finished across a rotation is
    /// remembered as in hand until the client is closed.
    ///
    /// Only where the file that was moved aside is provably the one this was
    /// reading: the same device and inode the offset was measured against. Any
    /// other file at that name is a rotation this reader missed entirely, and
    /// an offset into it would be an offset into somebody else's stream.
    fn catch_up_with_the_rotated(&mut self, path: &Path, seen: (u64, u64)) {
        let Ok(file) = std::fs::File::open(rotated(path)) else {
            return;
        };
        let Ok(meta) = file.metadata() else {
            return;
        };
        if (meta.dev(), meta.ino()) != seen || meta.len() <= self.read_to {
            return;
        }
        let Some((bytes, whole)) = whole_lines_from(file, self.read_to) else {
            return;
        };
        self.fold(&String::from_utf8_lossy(&bytes[..whole]));
    }

    fn fold(&mut self, text: &str) {
        for line in text.lines() {
            // A run of the client's own begins here, and nothing the run before
            // it left unfinished belongs to this one. **A client killed in the
            // middle of an update never writes the line that ends it**, so
            // without this a session that lost its Steam once would believe
            // that update was in flight for ever and nothing would ever close a
            // client again.
            if line.contains(NEW_RUN) {
                self.doing.clear();
                continue;
            }
            let Some((track, app_id, what)) = job_in(line) else {
                continue;
            };
            // Each line is the whole state of that job, so the last one wins
            // and `None` is how the client says it has finished.
            match what == NOTHING_IN_HAND {
                true => self.doing.remove(&(track, app_id)),
                false => self.doing.insert((track, app_id), phase_of(what)),
            };
        }
    }
}

/// Everything from `from` to the end of the file, cut back to the last whole
/// line, and how much of it that is.
///
/// Whole lines only, because a client writing while this reads leaves a
/// half-written last one — see [`Jobs::read_to`] — and bytes rather than a
/// string, because the seek lands wherever it is asked to and that may be the
/// middle of a character.
fn whole_lines_from(mut file: std::fs::File, from: u64) -> Option<(Vec<u8>, usize)> {
    file.seek(SeekFrom::Start(from)).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    let whole = bytes.iter().rposition(|byte| *byte == b'\n')? + 1;
    Some((bytes, whole))
}

/// Where the run of the client that is still going began, in a stretch of its
/// log, or nothing where the whole stretch belongs to runs before it.
///
/// The offset of the start of the last [`NEW_RUN`] line: what follows it is
/// this client's own account of itself, and what precedes it was written by a
/// client that is no longer there.
fn this_run_in(bytes: &[u8]) -> Option<usize> {
    let marker = NEW_RUN.as_bytes();
    let at = bytes
        .windows(marker.len())
        .rposition(|window| window == marker)?;
    // Back to the start of the line it is on, so the fold begins on a line
    // boundary rather than in the middle of one.
    Some(match bytes[..at].iter().rposition(|byte| *byte == b'\n') {
        Some(end) => end + 1,
        None => 0,
    })
}

/// The name the client moves this log aside to when it fills up.
fn rotated(path: &Path) -> PathBuf {
    path.with_file_name(CONTENT_LOG_BEFORE)
}

/// The log the client writes what it is doing to each game into.
const CONTENT_LOG: &str = "content_log.txt";

/// And what it renames that to at four megabytes, keeping one.
///
/// Read for two stretches that are in it rather than in the log at the live
/// name, and both of them are a run of the client that outlived its own log:
/// the lines written between the last look and the rotation, and — for a shell
/// that starts while a client is already running — the beginning of that
/// client's run. See [`Jobs::catch_up_with_the_rotated`] and [`Jobs::attach`].
const CONTENT_LOG_BEFORE: &str = "content_log.previous.txt";

/// Which of the two a job's phase is. See [`InHand`].
fn phase_of(what: &str) -> InHand {
    match what.contains(CHECKING_THE_DISK) {
        true => InHand::Checking,
        false => InHand::Working,
    }
}

/// The phase that reads back what is already on the disk. Matched whole, so
/// that `Verifying Staged` — which checks what has just arrived, inside a
/// download the manifest is already describing — is not taken for it.
const CHECKING_THE_DISK: &str = "Verifying Installed";

/// The track, the app id and the phase out of one of the client's job lines.
///
/// `[2026-09-02 21:03:40] AppID 1391110 App update changed : Running Update,`
fn job_in(line: &str) -> Option<(Track, u32, &str)> {
    let (track, (before, what)) = TRACKS
        .iter()
        .find_map(|track| Some((*track, line.split_once(track.said())?)))?;
    let app_id = before.rsplit_once(APP_ID)?.1.trim().parse().ok()?;
    Some((track, app_id, what.trim()))
}

const APP_ID: &str = "AppID ";

/// What the client writes when that job is over.
const NOTHING_IN_HAND: &str = "None";

/// The last 64 KB of a log the client is writing, or nothing where there is no
/// such file.
fn tail_of(path: &Path) -> Option<String> {
    const TAIL: u64 = 64 * 1024;

    let mut file = std::fs::File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(length.saturating_sub(TAIL)))
        .ok()?;
    // Read bytes and not a string: the seek lands wherever it lands, which may
    // be the middle of a character, and a log that happens to hold one is not
    // a client that is signed out.
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;

    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// What the client's own log says is standing between a press and a game.
///
/// A launch walks a dozen tasks — `UpdatingAppInfo`, `CheckShaderDepotManifest`,
/// `DownloadingDepots`, `SynchronizingCloud`, `CreatingProcess` — and this is
/// the last one it wrote, with the two ends of the walk called out as their
/// own answers.
///
/// **The walk itself is the news, and reading it as silence cost a launch.**
/// The first cut of this answered nothing at all for a launch that was still
/// being walked, on the reasoning that a launch in flight is the ordinary case
/// and the loading screen was already waiting for it. Reported from use on
/// 2026-09-04, with a screenshot of Valve's own launch window saying
/// *"Downloading content (19%)"*: a press on Counter-Strike 2 reached
/// `DownloadingDepots` one second in and stayed there while Steam fetched the
/// update, and the loading screen — which had nothing on this disk to read,
/// the game's manifest saying only that the copy was being repaired — ran out
/// its minute and said the game had not started, over a client that was one
/// fifth of the way through starting it.
///
/// So a task in flight is an answer: **the client is working on this press**,
/// it says on what, and a screen that can see that has no business giving up
/// on it. See [`LaunchStanding::Working`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchStanding {
    /// `LaunchApp failed with <error>`: the client has given up on it, and no
    /// window is coming.
    ///
    /// **Reported off this machine on 2026-09-03, twice in an hour.** A press
    /// on Counter-Strike 2 walked as far as `DownloadingDepots` and then:
    ///
    /// ```text
    /// [2026-09-03 23:47:02] GameAction [AppID 730, ActionID 1] : LaunchApp failed with AppError_19 with ""
    /// [2026-09-03 23:47:02] GameAction [AppID 730, ActionID 1] : LaunchApp changed task to Failed with ""
    /// ```
    ///
    /// `AppError_19` is "Update required", and Valve's client says so in a
    /// modal — which this shell holds off the screen, so nothing anywhere said
    /// it. Twenty-one seconds into the first press and twenty into the second,
    /// against a loading screen that waits sixty and then says the game did not
    /// start. Steam went on to fetch the update the launch had asked it to
    /// schedule, finished it, and started nothing: the launch it belonged to
    /// had been over for eighteen seconds.
    ///
    /// The string is Valve's own name for the error and is **for the log**. It
    /// is an internal identifier, it is in English whatever language Steam is
    /// running in, and nothing here reads it as an enumeration — what to do
    /// about a refusal is decided from what the shell can see for itself.
    Refused(String),
    /// `changed task to Completed`: the client has done its half, and what is
    /// left is the window.
    Started,
    /// `waiting for user response to <task>`: it has stopped on that step, and
    /// the client is showing a window about it that this shell is holding off
    /// the screen.
    ///
    /// The same fact `bWaitingForUI` carries in
    /// [`crate::webui::launching`], out of a file rather than out of the
    /// client's interface — so it is answerable on a client that was already
    /// running when this session came up, which exposes no interface at all.
    Waiting(String),
    /// `changed task to <task>`, with nothing after it: the client is on that
    /// step of the walk and has not left it.
    ///
    /// Not a stop and not an end — it is the ordinary way a launch spends its
    /// time, and the reason it is worth reporting is that some of these steps
    /// are *minutes* long. `DownloadingDepots` is the whole of an update the
    /// press asked for; `ProcessingShaderCache` is a shader cache; and Valve's
    /// own launch window puts a line and a percentage on the screen for each of
    /// them while this shell had a spinner and a clock running out.
    ///
    /// The string is Valve's own name for the task. Unlike
    /// [`LaunchStanding::Refused`]'s it **is** read as an enumeration — by the
    /// shell, which has its own words for the steps worth naming and says
    /// nothing for the rest.
    Working(String),
}

/// What the client's own log says became of the launch of `app_id`, out of
/// everything it has written since `from`.
///
/// **The log rather than the interface.** `GetActiveGameActions` describes a
/// launch that is still walking — see [`crate::webui::launching`] — and a
/// launch that has failed is one the client is no longer walking. This is
/// written down whatever happens, needs no websocket, and is the only place a
/// refusal is recorded at all.
///
/// `from` is where the log stood when the press was made, so that a failure
/// from an earlier press of the same game is not read as this one's. See
/// [`launches_so_far`].
pub fn how_the_launch_went(root: &Path, app_id: u32, from: u64) -> Option<LaunchStanding> {
    let mut file = std::fs::File::open(root.join("logs").join(CONSOLE_LOG)).ok()?;
    // A log that has gone backwards has been rolled over under this, so what
    // this launch wrote is wherever it is: read what is there rather than
    // seeking past the end of it.
    let from = match file.metadata().ok()?.len() < from {
        true => 0,
        false => from,
    };
    file.seek(SeekFrom::Start(from)).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    what_became_of_the_launch(&String::from_utf8_lossy(&bytes), app_id)
}

/// How much the client has written into that log so far, or nothing where
/// there is none to read.
///
/// Taken once, as a press goes out, so that everything read afterwards belongs
/// to it.
pub fn launches_so_far(root: &Path) -> u64 {
    std::fs::metadata(root.join("logs").join(CONSOLE_LOG))
        .map(|meta| meta.len())
        .unwrap_or_default()
}

/// The same reading, out of a stretch of log, so the shapes a real client
/// writes can be checked without one.
///
/// Backwards, because what is wanted is the last word about this game and the
/// client writes a good deal about others in between.
fn what_became_of_the_launch(log: &str, app_id: u32) -> Option<LaunchStanding> {
    let mut failed = false;
    for line in log.lines().rev() {
        let Some(said) = a_launch_step(line, app_id) else {
            continue;
        };
        if let Some(why) = said.strip_prefix(FAILED_WITH) {
            return Some(LaunchStanding::Refused(up_to_the_details(why)));
        }
        if let Some(task) = said.strip_prefix(WAITING_FOR) {
            // Unless something after it says the client went on, which is what
            // the walk backwards has already looked at, this launch is stopped
            // here. The task's details follow it in quotation marks, without
            // the `with` the other lines put in front of them, so the name is
            // taken as the first word rather than by cutting at that.
            return task
                .split_whitespace()
                .next()
                .map(|task| LaunchStanding::Waiting(task.to_string()));
        }
        let Some(task) = said.strip_prefix(CHANGED_TASK) else {
            // `continues with user response …`, which is a launch that was
            // stopped and is not any more.
            continue;
        };
        return match up_to_the_details(task).as_str() {
            COMPLETED => Some(LaunchStanding::Started),
            // The line that says *why* is the one before it, so this is not an
            // answer yet — but it is the answer if nothing else turns up.
            FAILED => {
                failed = true;
                continue;
            }
            // A step of the walk, which means the client is still walking it
            // — unless a `Failed` further down was waiting on a reason and
            // this is the step it failed on rather than the reason.
            step => match failed {
                true => Some(LaunchStanding::Refused(String::new())),
                false => Some(LaunchStanding::Working(step.to_string())),
            },
        };
    }
    failed.then(|| LaunchStanding::Refused(String::new()))
}

/// What one of the client's launch lines says about `app_id`, or nothing for
/// every other line in the log.
///
/// ```text
/// [2026-09-03 23:47:02] GameAction [AppID 730, ActionID 1] : LaunchApp failed with AppError_19 with ""
/// ```
///
/// `LaunchApp` and nothing else: the same list carries the client's installs
/// and removals, and an install that failed is not a game that will not start.
/// The action number is deliberately not read — it counts from one again on
/// every run of the client, so it names nothing on its own, and `from` is what
/// tells one press from another.
fn a_launch_step(line: &str, app_id: u32) -> Option<&str> {
    let (id, rest) = line.split_once(GAME_ACTION)?.1.split_once(',')?;
    (id.trim().parse::<u32>().ok()? == app_id).then_some(())?;
    Some(rest.split_once(A_LAUNCH)?.1.trim())
}

/// The client puts the task's own details after it, in quotation marks, and
/// they are empty on every line this reads. Cut rather than assumed empty.
fn up_to_the_details(said: &str) -> String {
    said.split(DETAILS)
        .next()
        .unwrap_or(said)
        .trim()
        .to_string()
}

const GAME_ACTION: &str = "GameAction [AppID ";
const A_LAUNCH: &str = "] : LaunchApp ";
const FAILED_WITH: &str = "failed with ";
const CHANGED_TASK: &str = "changed task to ";
/// What the client writes when it has stopped on a step and put a window up
/// about it. The task's own name follows, and then its details in quotation
/// marks — `waiting for user response to ProcessingShaderCache ""`.
const WAITING_FOR: &str = "waiting for user response to ";
const DETAILS: &str = " with \"";
/// The client's own words for the two ends of a launch.
const COMPLETED: &str = "Completed";
const FAILED: &str = "Failed";

/// The log the client walks each launch through, a line to a step.
const CONSOLE_LOG: &str = "console_log.txt";

/// What the client writes as the first line of every run.
const NEW_RUN: &str = "Client version:";

/// The state a logged-on client stamps its lines with.
const LOGGED_ON: &str = "Logged On";

/// What the client writes as the last line of every run it closes cleanly.
///
/// The other half of [`NEW_RUN`], and it is needed for the same reason. A
/// client that has just been started has not written its own first line yet,
/// so for a second or so the tail of this log still belongs to the client
/// before it — and that one's account stamp is still the last one in the file.
/// Caught on this machine on 2026-09-02: a cold start was called ready one
/// second in, off the *previous* client's Offline Mode.
const RUN_ENDED: &str = "Log session ended";

/// The last line of the client's current run that says both what its
/// connection is doing and whose it is.
fn last_stamp(log: &str) -> Option<(&str, u32, &str)> {
    let this_run = match log.rfind(NEW_RUN) {
        Some(at) => &log[at..],
        // No start marker in the tail. Either the client has been up long
        // enough to write 64 KB since, in which case all of this is its own,
        // or there is no log to speak of — and both are answered by reading
        // what is here.
        None => log,
    };
    this_run.lines().filter_map(stamped).next_back()
}

/// Who the client is signed in to *Steam* as now, out of a stretch of its log.
fn logged_on_in(log: &str) -> Option<u32> {
    match last_stamp(log) {
        Some((LOGGED_ON, account, said)) if account != 0 && !said.contains(RUN_ENDED) => {
            Some(account)
        }
        _ => None,
    }
}

/// Who the client that is running is acting for, whether or not it has reached
/// Steam.
///
/// The weaker question, and the only one an offline client answers. It stamps
/// every line with its account from the moment it knows which one it is —
/// `CCMInterface::SetSteamID( [U:1:82105993] )`, a second into a cold start —
/// and then stays `Logged Off` for the rest of its run. On its own this says
/// nothing about whether the client is usable, which is why [`state`] only
/// reads it beside the client's own list. Zero is the client before it knows,
/// and is not an account.
fn acting_for_in(log: &str) -> Option<u32> {
    let (_, account, said) = last_stamp(log)?;
    if said.contains(RUN_ENDED) {
        // The run this stamp belongs to is over, so it says nothing about the
        // client that is up now. See [`RUN_ENDED`].
        return None;
    }
    (account != 0).then_some(account)
}

/// The state and account one log line is stamped with, for the lines that
/// carry both.
///
/// The state block is `[<state>, <n>, <n>]`, and the two numbers are what tell
/// it from the timestamp block that precedes it — a line stamped with an
/// account but no state says nothing about whether the client is signed in and
/// must not be read as though it did.
fn stamped(line: &str) -> Option<(&str, u32, &str)> {
    let at = line.find("[U:1:")?;
    let (account, said) = line[at + "[U:1:".len()..]
        .split_once(']')
        .and_then(|(id, said)| Some((id.parse::<u32>().ok()?, said)))?;

    let before = &line[..at];
    let open = before.rfind('[')?;
    let block = &before[open + 1..before[open..].find(']')? + open];
    let mut fields = block.split(',');
    let state = fields.next()?.trim();
    let counts = (
        fields.next()?.trim().parse::<u32>(),
        fields.next()?.trim().parse::<u32>(),
    );
    if fields.next().is_some() || counts.0.is_err() || counts.1.is_err() {
        return None;
    }
    Some((state, account, said))
}

/// Valve's Flatpak, which is the name of everything it puts on the disk:
/// its deployment, its exported launcher, and the home directory it runs with.
pub(crate) const FLATPAK_APP: &str = "com.valvesoftware.Steam";

/// Whether Valve's Flatpak is *deployed*, rather than merely remembered.
///
/// This used to ask whether `~/.var/app/com.valvesoftware.Steam` was a
/// directory, and that is the one question about a Flatpak whose answer
/// outlives the application. `flatpak uninstall` keeps the application's data
/// unless it is asked for `--delete-data`, on purpose — somebody who removes a
/// launcher does not thereby mean to throw away their saves — so a machine
/// that had the Flatpak and removed it looks exactly like one that has it. The
/// shell then offered every row that needs a client, and each ran
/// `flatpak run` against nothing: `spawn` succeeds, Flatpak fails a moment
/// later where nobody is reading, and the press ended in three minutes of
/// loading screen and a sentence about a debugging port.
///
/// What is asked instead is whether a commit is deployed, which is the file
/// Flatpak itself removes on uninstall. Still no process started: `flatpak
/// list` is half a second, and this is four `stat` calls on the session's way
/// up.
fn flatpak_deployed() -> bool {
    deployed_in(&flatpak_roots(), FLATPAK_APP)
}

/// Whether any of these installations has a commit of this application
/// deployed. Split out so a test can point it at a scratch directory rather
/// than at whatever this machine happens to have installed.
fn deployed_in(roots: &[PathBuf], app: &str) -> bool {
    roots.iter().any(|root| {
        root.join("app")
            .join(app)
            .join("current")
            .join("active")
            .exists()
    })
}

/// Every installation Flatpak would look in, in its own order of preference.
///
/// The user's and the system's, and then any the administrator has defined —
/// a custom installation only exists by being named in `installations.d`, so
/// reading that file is what makes this complete rather than merely usual. A
/// Steam deployed into one of those is a Steam `flatpak run` will find, and so
/// is one this must not call missing.
fn flatpak_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    // The same three places Flatpak itself would take the user installation
    // from, in the same order. `XDG_DATA_HOME` matters here: a session that
    // sets it moves the user installation with it, and looking only below
    // `~/.local/share` would call a Steam that is installed missing.
    let user = std::env::var_os("FLATPAK_USER_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|data| data.join("flatpak"))
        })
        .or_else(|| {
            std::env::var_os("HOME").map(|home| {
                PathBuf::from(home)
                    .join(".local")
                    .join("share")
                    .join("flatpak")
            })
        });
    roots.extend(user);
    roots.push(PathBuf::from("/var/lib/flatpak"));

    let Ok(entries) = std::fs::read_dir("/etc/flatpak/installations.d") else {
        return roots;
    };
    for entry in entries.flatten() {
        if entry.path().extension().is_none_or(|end| end != "conf") {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(entry.path()) {
            roots.extend(installations_in(&text));
        }
    }
    roots
}

/// The installations one `installations.d` file defines.
///
/// A small INI of `[Installation "name"]` sections, of which one key matters.
/// Read by hand rather than parsed: a key this misreads costs one installation
/// almost nobody has, and a dependency costs everybody.
fn installations_in(text: &str) -> Vec<PathBuf> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("Path="))
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Look one program up on `PATH`, the way a shell would.
fn on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(program))
        .find(|candidate| {
            // Executable by somebody: the bit is what makes it a program
            // rather than a file that happens to share the name.
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(candidate)
                .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
        })
}

/// Keeping the client from signing itself back in.
///
/// Signing it *in* is not here: it is one call into the client's own interface
/// — see [`crate::webui`] — and the client then writes the credential down
/// itself, in its own private form, exactly as it would have if somebody had
/// typed a password into it.
///
/// Signing it *out* has no such call. The one the client's own menu uses is
/// `SignOutAndRestart`, and the second half of that name is the problem: it
/// brings the client back up on its login screen, which is the one thing this
/// shell must never put in front of somebody. So the client is stopped, and the
/// two plain files that would otherwise sign it straight back in are cleared.
///
/// What this deliberately does not touch is the credential itself. It is in a
/// format Valve has not published, this shell did not write it, and a guess at
/// where it lives is a guess that corrupts somebody's Steam configuration. What
/// it costs is narrow and worth saying plainly: after signing out of the shell,
/// a user who starts Valve's client *by hand* may find it still signed in, and
/// signs out from inside it as they always would.
/// One writer at a time at `config/loginusers.vdf`, across every session this
/// user is running.
///
/// **Two modules below write that file**, and both do it by reading all of it,
/// changing a field and putting all of it back: [`offline`] sets
/// `WantsOfflineMode`, and [`account::sign_out`] clears `AllowAutoLogin` and
/// `MostRecent`. Two of those at once — two sessions, or a session and one of
/// this crate's `probe-*` examples — read the same bytes and each put back a
/// copy without the other's change in it. The second to finish wins, silently,
/// and what is lost is either somebody's Offline Mode or the sign-out that was
/// supposed to stop a client signing itself back in.
///
/// It is only ever half a guard, and there is no honest way to make it a whole
/// one: Valve's client writes this file too and has never heard of this lock.
/// What it makes impossible is two LineXinBar sessions losing each other's
/// edit. See [`crate::turns::What::TheAccountList`].
fn the_account_list(root: &Path) -> crate::turns::Turn {
    crate::turns::take(
        crate::turns::What::TheAccountList,
        crate::turns::Behalf {
            backend: Some(root.to_path_buf()),
            request: None,
        },
    )
}

/// Beside and rename over, so a client reading one of these files while this
/// happens sees either all of the old one or all of the new one.
///
/// One copy for both modules below, which had one each and identical. The
/// scratch name carries this process's pid: under [`the_account_list`] no two
/// sessions are here at once anyway, but a fixed name would be one file two
/// programs write into — and the machine where that matters is exactly the one
/// where the lock could not be made. It also means a scratch file left behind
/// by a session killed mid-write is visibly whose, rather than a permanent
/// squatter on the only name.
fn beside_and_rename_over(path: &Path, text: &str) {
    let scratch = path.with_extension(format!("lxb-writing-{}", std::process::id()));
    if std::fs::write(&scratch, text).is_ok() {
        let _ = std::fs::rename(&scratch, path);
    }
}

pub mod account {
    use std::path::Path;

    use super::beside_and_rename_over as write;
    use crate::vdf::{self, Node};

    /// Where the account name goes in the client's Linux registry — its
    /// stand-in for the Windows registry key of the same name.
    pub(super) const AUTO_LOGIN_USER: [&str; 6] = [
        "Registry",
        "HKCU",
        "Software",
        "Valve",
        "Steam",
        "AutoLoginUser",
    ];

    /// Whether this client would sign *itself* in, given the chance.
    ///
    /// The same field [`stop`] clears, read rather than written, and it is read
    /// for the one thing worth knowing about a client that is up and signed in
    /// to nobody: whether waiting is going to change that. A client with an
    /// account here keeps a credential for it and logs itself back on unaided,
    /// usually within a couple of seconds, and waiting is by far the cheapest
    /// way to a signed-in client. A client with this empty has nobody to be —
    /// it has never signed anybody in, or this shell signed it out — and no
    /// amount of waiting will make it somebody.
    ///
    /// **This is the difference between a first login that takes five seconds
    /// and one that takes thirty-five.** A machine where this shell has just
    /// installed Steam has exactly this state. Without this,
    /// [`super::bring_up`] spent its whole `UNTIL_IT_SIGNS_ITSELF_IN` patience
    /// watching a client that was never going to, before handing it the
    /// credential that worked at once.
    ///
    /// **Absent is "nobody", the same as empty.** The two are two different
    /// clients: a client this shell signed out has the field and it is empty
    /// (`AutoLoginUser ""`, measured 2026-09-04), and a client that has never
    /// signed anybody in has no field at all — Valve's launcher writes it at
    /// the first logon and not before, measured on a fresh install on
    /// 2026-09-13. Reading the missing field as "could" cost that install
    /// exactly the thirty seconds this exists to save, on the first press
    /// after the shell's own sign-in.
    ///
    /// A registry that cannot be read at all still answers `true`, which is
    /// the cautious way round: it says nothing about the client either way,
    /// where a registry with no account in it says the one thing that matters.
    pub(super) fn could_sign_itself_in(home: &Path) -> bool {
        let Some(node) = read(&home.join("registry.vdf")) else {
            return true;
        };
        node.string(&AUTO_LOGIN_USER)
            .is_some_and(|account| !account.trim().is_empty())
    }

    /// Where the client keeps the credentials it signs itself back in with.
    ///
    /// One entry per account it remembers, under a key derived from the
    /// account's name — see [`cached_under`].
    const CONNECT_CACHE: [&str; 5] = [
        "MachineUserConfigStore",
        "Software",
        "Valve",
        "Steam",
        "ConnectCache",
    ];

    /// The flags in the client's own account list that say it may sign this
    /// account in again without being asked for anything.
    ///
    /// Four names for what is really two facts, because Valve's client has
    /// spelled them differently across its own versions — the build measured
    /// on 2026-09-13 writes `AutoLogin` and `RememberPassword`, and older ones
    /// write `AllowAutoLogin` and `MostRecent`. **Only the ones already in the
    /// account's block are written**, so a client is never given a key its own
    /// build does not read, and a spelling nobody has thought of yet costs
    /// nothing: the credential itself is taken away below, and the registry's
    /// `AutoLoginUser` above, and either alone stops an unattended sign-in.
    const REMEMBERED: [&str; 4] = [
        "AutoLogin",
        "AllowAutoLogin",
        "RememberPassword",
        "MostRecent",
    ];

    /// Sign Valve's client out of this account.
    ///
    /// **The whole sign-out, as somebody pressing a row called "Sign out"
    /// means it**, and the user asked for exactly that on 2026-09-13: what
    /// the shell forgets is its own half, and a client left holding the
    /// account's credential is a Steam the next person at the machine opens
    /// and is already signed in to. Three things, of which only the first was
    /// here before:
    ///
    /// 1. the registry's `AutoLoginUser`, so nothing signs itself in on the
    ///    way up;
    /// 2. the flags in the client's own account list that say this account may
    ///    be signed in again unasked — see [`REMEMBERED`];
    /// 3. and the credential the client kept for it, which is the one that
    ///    actually lets somebody in. Without this the client comes up on its
    ///    login screen with the account listed, and one click signs in.
    ///
    /// The account **keeps its place in that list**, which is what Valve's own
    /// sign-out does too: the name stays on the login screen and a password
    /// gets somebody back in. Emptying the list would be throwing away
    /// something the shell was never asked about, and on a household machine
    /// it is somebody else's row as often as it is this one's.
    ///
    /// Never fails outwards: this runs while the user is signing out, the
    /// sign-out itself has already happened, and there is nothing useful to
    /// say to somebody about a file they have never heard of. Whatever could
    /// be cleared is cleared.
    ///
    /// **Call it with no client running.** All three files are the client's
    /// own and it writes them back as it exits; editing them under a client
    /// that is still shutting down is an edit it overwrites. See
    /// [`Ask::SignOut`](crate::Ask), which waits for the pipe to go first.
    pub fn sign_out(root: &Path, home: &Path, account: &str) {
        // Held over all three files. One of them is the account list, which
        // [`super::offline`] writes too — see [`super::the_account_list`].
        let _turn = super::the_account_list(root);
        // Whether there was anything here to clear. Asked because this is run
        // once per layout a client could have used — see
        // [`crate::client::Options::every_layout`] — and a line saying the
        // client has been signed out, said of a directory where no client has
        // ever been, is a line that would send somebody looking in the wrong
        // place.
        let mut cleared = false;
        let registry = home.join("registry.vdf");
        if let Some(mut node) = read(&registry) {
            if node.set(&AUTO_LOGIN_USER, "") {
                write(&registry, &vdf::text(&node));
                cleared = true;
            }
        }

        let users = root.join("config").join("loginusers.vdf");
        if let Some(mut node) = read(&users) {
            let mut touched = false;
            if let Some(block) = node.make(&["users"]) {
                for user in block.values_mut() {
                    if user
                        .string(&["AccountName"])
                        .is_some_and(|name| name.eq_ignore_ascii_case(account))
                    {
                        for flag in REMEMBERED {
                            // Only what the client itself put there. See
                            // [`REMEMBERED`].
                            if user.string(&[flag]).is_some() {
                                user.set(&[flag], "0");
                                touched = true;
                            }
                        }
                    }
                }
            }
            if touched {
                write(&users, &vdf::text(&node));
                cleared = true;
            }
        }

        let store = root.join("local.vdf");
        if let Some(mut node) = read(&store) {
            if forget_the_credential(&mut node, account) {
                write(&store, &vdf::text(&node));
                cleared = true;
            }
        }

        if cleared {
            tracing::info!(
                root = %root.display(),
                account,
                "Valve's client has been signed out of this account"
            );
        }
    }

    /// Take this account's stored credential out of the client's cache, and
    /// say whether one was there.
    ///
    /// Only this account's. Every other entry belongs to somebody else who
    /// signs in on this machine, and a sign-out that emptied the cache would
    /// sign the rest of a household out with them.
    fn forget_the_credential(node: &mut Node, account: &str) -> bool {
        // Asked before it is made: `make` builds every step of a path that is
        // not there, and a machine whose client has never cached anything must
        // not gain an empty cache from being signed out of.
        if node.get(&CONNECT_CACHE).is_none() {
            return false;
        }
        let Some(cache) = node.make(&CONNECT_CACHE) else {
            return false;
        };
        let under = cached_under(account);
        let before = cache.len();
        cache.retain(|key, _| !key.to_ascii_lowercase().starts_with(&under));
        cache.len() != before
    }

    /// The key one account's credential is filed under in [`CONNECT_CACHE`],
    /// as far as the key is knowable: the CRC-32 of the account name, in lower
    /// case hexadecimal.
    ///
    /// Read off a live client on 2026-09-13 — account `…1999`, cache key
    /// `48e613161`, `crc32` of the name `48e61316` — so the whole key is that
    /// with one more character after it, which is the persistence scheme the
    /// entry was written under. The scheme digit is deliberately **not**
    /// derived: what is returned is the prefix, and every entry that begins
    /// with it is this account's whatever scheme wrote it.
    ///
    /// A key that turns out to be built some other way on some other build
    /// costs nothing here — nothing is removed, and the flags and the registry
    /// have already said the client may not sign itself in. It is the extra
    /// mile rather than the whole road.
    fn cached_under(account: &str) -> String {
        format!("{:x}", crc32(account.as_bytes()))
    }

    /// CRC-32, the ordinary IEEE one, computed rather than taken as a
    /// dependency.
    ///
    /// Eight lines against a crate, for the reason this module parses VDF
    /// itself: it is one short loop, it has not changed since 1975, and a test
    /// pins it to the standard check value.
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = !0u32;
        for byte in bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                let carry = crc & 1;
                crc >>= 1;
                if carry != 0 {
                    crc ^= 0xEDB8_8320;
                }
            }
        }
        !crc
    }

    fn read(path: &Path) -> Option<Node> {
        Some(vdf::parse(&std::fs::read_to_string(path).ok()?))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// After this the client comes up asking rather than signing itself
        /// in, it has nothing left to sign in *with*, and somebody else's
        /// account is untouched in both files.
        #[test]
        fn it_signs_only_this_account_out() {
            let root = std::env::temp_dir().join(format!("lxb-account-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("config")).unwrap();
            std::fs::write(
                root.join("registry.vdf"),
                "\"Registry\"\n{\n\t\"HKCU\"\n\t{\n\t\t\"Software\"\n\t\t{\n\t\t\t\"Valve\"\n\t\t\t{\n\t\t\t\t\"Steam\"\n\t\t\t\t{\n\t\t\t\t\t\"AutoLoginUser\"\t\t\"someone\"\n\t\t\t\t}\n\t\t\t}\n\t\t}\n\t}\n}\n",
            )
            .unwrap();
            // Two accounts, and the two spellings Valve's own builds use: the
            // one signing out carries the newer pair, the other the older.
            std::fs::write(
                root.join("config").join("loginusers.vdf"),
                "\"users\"\n{\n\t\"1\"\n\t{\n\t\t\"AccountName\"\t\t\"someone\"\n\t\t\"AutoLogin\"\t\t\"1\"\n\t\t\"RememberPassword\"\t\t\"1\"\n\t}\n\t\"2\"\n\t{\n\t\t\"AccountName\"\t\t\"somebody\"\n\t\t\"AllowAutoLogin\"\t\t\"1\"\n\t\t\"RememberPassword\"\t\t\"1\"\n\t}\n}\n",
            )
            .unwrap();
            let mine = cached_under("someone");
            let theirs = cached_under("somebody");
            std::fs::write(
                root.join("local.vdf"),
                format!(
                    "\"MachineUserConfigStore\"\n{{\n\t\"Software\"\n\t{{\n\t\t\"Valve\"\n\t\t{{\n\t\t\t\"Steam\"\n\t\t\t{{\n\t\t\t\t\"ConnectCache\"\n\t\t\t\t{{\n\t\t\t\t\t\"{mine}1\"\t\t\"abcdef\"\n\t\t\t\t\t\"{theirs}1\"\t\t\"123456\"\n\t\t\t\t}}\n\t\t\t}}\n\t\t}}\n\t}}\n}}\n"
                ),
            )
            .unwrap();

            sign_out(&root, &root, "someone");

            let registry = vdf::parse(&std::fs::read_to_string(root.join("registry.vdf")).unwrap());
            assert_eq!(registry.string(&AUTO_LOGIN_USER), Some(""));

            let users = vdf::parse(
                &std::fs::read_to_string(root.join("config").join("loginusers.vdf")).unwrap(),
            );
            assert_eq!(users.string(&["users", "1", "AutoLogin"]), Some("0"));
            assert_eq!(users.string(&["users", "1", "RememberPassword"]), Some("0"));
            // And nothing the client's own build does not read was invented
            // for it.
            assert_eq!(users.string(&["users", "1", "AllowAutoLogin"]), None);
            assert_eq!(
                users.string(&["users", "2", "AllowAutoLogin"]),
                Some("1"),
                "somebody else's account was changed"
            );
            assert_eq!(
                users.string(&["users", "2", "RememberPassword"]),
                Some("1"),
                "somebody else's account was changed"
            );

            let store = vdf::parse(&std::fs::read_to_string(root.join("local.vdf")).unwrap());
            let mut left: Vec<&String> = store
                .block(&CONNECT_CACHE)
                .map(|(key, _)| key)
                .collect::<Vec<_>>();
            left.sort();
            assert_eq!(
                left,
                vec![&format!("{theirs}1")],
                "the wrong credentials were taken away"
            );

            let _ = std::fs::remove_dir_all(&root);
        }

        /// A machine whose client has never cached anything is left alone
        /// rather than given an empty cache to explain.
        #[test]
        fn a_client_with_nothing_cached_gains_nothing() {
            let mut node = vdf::parse("\"MachineUserConfigStore\"\n{\n}\n");
            assert!(!forget_the_credential(&mut node, "someone"));
            assert_eq!(vdf::text(&node), "\"MachineUserConfigStore\"\n{\n}\n");
        }

        /// The check value every CRC-32 implementation is measured against.
        #[test]
        fn the_checksum_is_the_ordinary_one() {
            assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
            assert_eq!(crc32(b""), 0);
            // And the key is the name's checksum in lower-case hexadecimal,
            // which is the form the client files it under.
            assert_eq!(cached_under("123456789"), "cbf43926");
        }
    }
}

/// Valve's own Offline Mode: reading it, and asking for it.
///
/// ## What the mode is
///
/// A client in Offline Mode logs on to *this machine* rather than to Steam. It
/// never opens a connection, it says `Logged Off` in its connection log for the
/// whole of its run, and it plays every game on the disk that is fully up to
/// date. Valve's own wording for what it costs is plain: "Many features, such
/// as Friends and Family Sharing, will not be available while offline. Only
/// games that are fully up-to-date will be available."
///
/// ## Where it lives
///
/// In the client's own list of accounts, `config/loginusers.vdf`, as
/// `WantsOfflineMode` on the entry for one account. Measured on this machine on
/// 2026-09-02: choosing "Go Offline" in Valve's own menu turned that field from
/// `0` to `1`, and a client started afterwards — with the network up and
/// working — came up logged on for that account without so much as trying to
/// connect, in about a second, with no window and no dialog. So the field is
/// not a note of what happened; it is what decides how the client comes up, and
/// writing it is the whole of how a shell with no terminal enters the mode.
///
/// The two obvious alternatives were tried first and are worse. `steam://
/// goonline` and `steam://gooffline` are real verbs in the client's own URL
/// table and are **silent no-ops** on this build — measured: the URL was
/// accepted, and nothing happened, ever. `SteamClient.User.StartOffline` is a
/// real call, and reaching it means opening the client's debugging port and
/// restarting a client that came up without it, which is a minute of somebody's
/// loading screen to write a field this shell can write in a millisecond.
///
/// ## What this shell will and will not do with it
///
/// It turns the mode **on** when it cannot reach Steam and somebody has pressed
/// a game, because on a console there is nothing else the press can mean. It
/// turns it **off** only where it turned it on: the marker in [`ours`] is what
/// separates a mode this shell asked for from one the user chose in Valve's own
/// menu, and dragging somebody back online because their network came back
/// would be the shell overruling a choice it never made.
pub mod offline {
    use std::path::{Path, PathBuf};

    use super::beside_and_rename_over as write;
    use super::Options;
    use crate::vdf;

    /// The field, on one account's entry in the client's own list.
    const WANTS_OFFLINE_MODE: &str = "WantsOfflineMode";

    /// Which account, if any, would have the client come up offline.
    ///
    /// Read as the client reads it: the registry says which account signs
    /// itself in, and that account's entry says whether it wants the mode. An
    /// entry for somebody else with the field set is not this client's business
    /// — a household with two accounts must not have one of them decide how the
    /// other comes up.
    ///
    /// Answers the **account id**, which is the low half of the SteamID64 the
    /// entry is filed under, because that is the form the client's own log
    /// stamps its lines with and the form [`super::state`] has to compare
    /// against.
    pub fn account_wanting_it(options: &Options) -> Option<u32> {
        let signs_itself_in = auto_login_user(&options.home)?;
        let users = read(&list(&options.root))?;
        let steam_id = users.block(&["users"]).find_map(|(steam_id, user)| {
            let is_the_one = user
                .string(&["AccountName"])
                .is_some_and(|name| name.eq_ignore_ascii_case(&signs_itself_in));
            let wants_it = user.number(&[WANTS_OFFLINE_MODE]) == Some(1);
            (is_the_one && wants_it).then(|| steam_id.parse::<u64>().ok())?
        })?;
        Some(steam_id as u32)
    }

    /// Whether this account's entry already asks for the mode.
    pub fn wanted(options: &Options, account: &str) -> bool {
        read(&list(&options.root)).is_some_and(|users| {
            users.block(&["users"]).any(|(_, user)| {
                user.string(&["AccountName"])
                    .is_some_and(|name| name.eq_ignore_ascii_case(account))
                    && user.number(&[WANTS_OFFLINE_MODE]) == Some(1)
            })
        })
    }

    /// Ask for it, so that the next client to start comes up offline.
    ///
    /// Says whether the file had to be changed, which is what tells a client
    /// that has to be started again from one that was going to come up right
    /// anyway.
    pub fn ask_for(options: &Options, account: &str) -> bool {
        // Held over the read, the write and the marker below, because [`set`]
        // is a read-modify-write of the whole account list and the marker in
        // [`ours`] must not be able to disagree with the field it records. See
        // [`super::the_account_list`].
        let _turn = super::the_account_list(&options.root);
        if set(options, account, true) {
            ours::remember(account);
            tracing::info!(
                account,
                "asked Valve's client for its own Offline Mode, having no way to reach Steam"
            );
            return true;
        }
        false
    }

    /// Take it back, for a mode this shell asked for and no longer needs.
    ///
    /// Deliberately silent about a mode somebody chose themselves: see the
    /// module's own note, and [`ours`].
    pub fn give_back(options: &Options, account: &str) -> bool {
        let _turn = super::the_account_list(&options.root);
        if !ours::was_it(account) {
            return false;
        }
        ours::forget();
        if set(options, account, false) {
            tracing::info!(
                account,
                "took Valve's client back out of the Offline Mode this session asked for"
            );
            return true;
        }
        false
    }

    /// Write the field, and say whether anything moved.
    ///
    /// Called only under [`super::the_account_list`], which is what makes the
    /// read and the write below one step rather than two.
    fn set(options: &Options, account: &str, wanted: bool) -> bool {
        let path = list(&options.root);
        let Some(mut users) = read(&path) else {
            return false;
        };
        let value = if wanted { "1" } else { "0" };
        let mut moved = false;
        if let Some(block) = users.make(&["users"]) {
            for user in block.values_mut() {
                let theirs = user
                    .string(&["AccountName"])
                    .is_some_and(|name| name.eq_ignore_ascii_case(account));
                if theirs && user.string(&[WANTS_OFFLINE_MODE]) != Some(value) {
                    user.set(&[WANTS_OFFLINE_MODE], value);
                    moved = true;
                }
            }
        }
        if moved {
            write(&path, &vdf::text(&users));
        }
        moved
    }

    fn list(root: &Path) -> PathBuf {
        root.join("config").join("loginusers.vdf")
    }

    /// The account the client signs itself in as, out of its Linux registry.
    fn auto_login_user(home: &Path) -> Option<String> {
        let registry = read(&home.join("registry.vdf"))?;
        let name = registry.string(&super::account::AUTO_LOGIN_USER)?;
        (!name.is_empty()).then(|| name.to_string())
    }

    fn read(path: &Path) -> Option<vdf::Node> {
        Some(vdf::parse(&std::fs::read_to_string(path).ok()?))
    }

    /// Whether the Offline Mode that is on is one this shell asked for.
    ///
    /// A file of this shell's own rather than a field of Valve's, because the
    /// question is about this shell and Valve's client has no opinion on who
    /// pressed what. It holds the account name, so that signing in as somebody
    /// else does not inherit a decision made about a different library, and it
    /// lives in the cache directory because losing it costs one Offline Mode
    /// left on until somebody says otherwise — which is exactly what Valve's
    /// own client does anyway.
    pub mod ours {
        use std::path::PathBuf;

        pub(super) fn remember(account: &str) {
            let Some(path) = marker() else { return };
            if let Some(folder) = path.parent() {
                let _ = std::fs::create_dir_all(folder);
            }
            let _ = std::fs::write(path, account);
        }

        pub(super) fn forget() {
            if let Some(path) = marker() {
                let _ = std::fs::remove_file(path);
            }
        }

        /// Whether this shell asked for the mode that is on, for this account.
        pub fn was_it(account: &str) -> bool {
            marker()
                .and_then(|path| std::fs::read_to_string(path).ok())
                .is_some_and(|remembered| remembered.trim().eq_ignore_ascii_case(account))
        }

        fn marker() -> Option<PathBuf> {
            let cache = std::env::var_os("XDG_CACHE_HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .or_else(|| {
                    let home = std::env::var_os("HOME").map(PathBuf::from)?;
                    Some(home.join(".cache"))
                })?;
            Some(cache.join("lxb").join("steam-offline-was-ours"))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// A layout with one account in the client's list and one name in its
        /// registry — the shape read off this machine on 2026-09-02.
        ///
        /// A layout of its own per call, and the counter is what makes it so:
        /// the name and the mode were the whole of the path, so the two tests
        /// that both ask for `("someone", "0")` shared one directory and ran in
        /// parallel in it. One of them writes the field; the other asserts it is
        /// not written — so the suite failed about once in a while, in the test
        /// that had done nothing wrong.
        fn a_client(name: &str, offline: &str) -> (std::path::PathBuf, Options) {
            static NTH: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let root = std::env::temp_dir().join(format!(
                "lxb-offline-{name}-{offline}-{}-{}",
                std::process::id(),
                NTH.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("config")).unwrap();
            std::fs::write(
                root.join("registry.vdf"),
                format!(
                    "\"Registry\"\n{{\n\t\"HKCU\"\n\t{{\n\t\t\"Software\"\n\t\t{{\n\t\t\t\"Valve\"\n\t\t\t{{\n\t\t\t\t\"Steam\"\n\t\t\t\t{{\n\t\t\t\t\t\"AutoLoginUser\"\t\t\"{name}\"\n\t\t\t\t}}\n\t\t\t}}\n\t\t}}\n\t}}\n}}\n"
                ),
            )
            .unwrap();
            std::fs::write(
                root.join("config").join("loginusers.vdf"),
                format!(
                    "\"users\"\n{{\n\t\"76561198042371721\"\n\t{{\n\t\t\"AccountName\"\t\t\"{name}\"\n\t\t\"WantsOfflineMode\"\t\t\"{offline}\"\n\t}}\n\t\"76561198000000002\"\n\t{{\n\t\t\"AccountName\"\t\t\"somebody\"\n\t\t\"WantsOfflineMode\"\t\t\"1\"\n\t}}\n}}\n"
                ),
            )
            .unwrap();
            let options = Options {
                root: root.clone(),
                home: root.clone(),
            };
            (root, options)
        }

        /// The account id is the low half of the SteamID64 the entry is filed
        /// under, because that is the form the client stamps its log with.
        #[test]
        fn the_account_that_would_come_up_offline_is_the_one_that_signs_itself_in() {
            let (_root, options) = a_client("someone", "1");
            assert_eq!(account_wanting_it(&options), Some(82105993));
        }

        /// Somebody else's entry with the field set decides nothing. A
        /// household with two accounts must not have one of them say how the
        /// other comes up — and the fixture's second account has it on.
        #[test]
        fn another_accounts_offline_mode_is_not_this_clients() {
            let (_root, options) = a_client("someone", "0");
            assert_eq!(account_wanting_it(&options), None);
            assert!(!wanted(&options, "someone"));
            assert!(wanted(&options, "somebody"), "the fixture's other account");
        }

        /// Asked for, and then taken back — but only by the session that asked.
        #[test]
        fn offline_mode_is_only_taken_back_where_this_shell_asked_for_it() {
            let _turn = crate::one_at_a_time_with_the_environment();
            let (root, options) = a_client("someone", "0");
            let cache = root.join("cache");
            let was_cache = std::env::var_os("XDG_CACHE_HOME");
            let was_state = std::env::var_os("XDG_STATE_HOME");
            // SAFETY: under the environment mutex, which every test that moves
            // one of these holds.
            unsafe { std::env::set_var("XDG_CACHE_HOME", &cache) };
            // The state directory too, because writing the field takes a lock
            // that lives there — see [`the_list`]. Without this the lock would
            // go wherever the test that ran before this one left the variable
            // pointing, which on a fresh process is the real one belonging to
            // whoever is running the suite.
            unsafe { std::env::set_var("XDG_STATE_HOME", root.join("state")) };
            ours::forget();

            assert!(ask_for(&options, "someone"), "the field had to be written");
            assert!(wanted(&options, "someone"));
            assert_eq!(account_wanting_it(&options), Some(82105993));
            // Twice is not twice: the field is already right, so nothing moves.
            assert!(!ask_for(&options, "someone"));

            // A mode somebody chose in Valve's own menu is theirs, and the
            // marker is the whole of how the two are told apart.
            ours::forget();
            assert!(!give_back(&options, "someone"), "not this shell's to undo");
            assert!(wanted(&options, "someone"), "and so it is still on");

            ours::remember("someone");
            assert!(give_back(&options, "someone"));
            assert!(!wanted(&options, "someone"));
            // And the marker goes with it, so the next mode starts unclaimed.
            assert!(!ours::was_it("someone"));

            // Both put back. One left pointing into a directory this test is
            // about to remove is read by every later test in the process.
            // SAFETY: still under the environment mutex taken at the top.
            unsafe {
                match was_cache {
                    Some(was) => std::env::set_var("XDG_CACHE_HOME", was),
                    None => std::env::remove_var("XDG_CACHE_HOME"),
                }
                match was_state {
                    Some(was) => std::env::set_var("XDG_STATE_HOME", was),
                    None => std::env::remove_var("XDG_STATE_HOME"),
                }
            }
        }

        /// Signing in as somebody else must not inherit a decision made about
        /// a different library.
        #[test]
        fn the_marker_is_about_one_account() {
            let _turn = crate::one_at_a_time_with_the_environment();
            let cache =
                std::env::temp_dir().join(format!("lxb-offline-who-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&cache);
            // SAFETY: as above.
            unsafe { std::env::set_var("XDG_CACHE_HOME", &cache) };
            ours::remember("someone");
            assert!(ours::was_it("SOMEONE"), "account names are not case");
            assert!(!ours::was_it("somebody"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every row that opens one of Valve's windows says what it is opening,
    /// and says it in the words the row that was pressed used.
    #[test]
    fn each_request_names_what_it_opens() {
        assert_eq!(Doing::BigPicture.opening(), "Steam");
        assert_eq!(Doing::Open.opening(), "Steam (Client)");
        assert_eq!(Doing::Downloads.opening(), "Steam Downloads");
        // And the two about a title open a window of Steam's about it.
        assert_eq!(Doing::Install.opening(), "Steam");
        assert_eq!(Doing::Verify.opening(), "Steam");
        // The row's label and what it opens are not the same sentence: one is
        // an instruction and the other is a name.
        for doing in [
            Doing::BigPicture,
            Doing::Open,
            Doing::Downloads,
            Doing::Install,
            Doing::Verify,
        ] {
            assert_ne!(doing.label(), doing.opening());
        }
    }

    /// Which requests the storefront is the answer to, which is what decides
    /// whether a storefront already mapped counts as the window a press was
    /// waiting for.
    #[test]
    fn only_the_two_requests_the_storefront_answers_say_so() {
        assert!(Doing::Open.answered_by_the_storefront());
        assert!(Doing::Downloads.answered_by_the_storefront());
        // Big Picture has a window of its own, and a storefront standing in
        // front of it is what the press was pressed to get away from.
        assert!(!Doing::BigPicture.answered_by_the_storefront());
        assert!(!Doing::Install.answered_by_the_storefront());
        assert!(!Doing::Verify.answered_by_the_storefront());
    }

    /// The toasts are told by the name the client gives them, and nothing else
    /// of the client's is.
    #[test]
    fn a_toast_is_known_by_its_name() {
        assert!(is_a_toast("notificationtoasts_1_desktop"));
        assert!(is_a_toast(" NotificationToasts_10003_desktop "));
        assert!(!is_a_toast(STOREFRONT));
        assert!(!is_a_toast("Sign in to Steam"));
        assert!(!is_a_toast(""));
    }

    /// The connection log is appended across runs, so until the client that is
    /// coming up writes its own first line the tail of it is the run before's.
    ///
    /// Measured on this machine: a client killed at 21:27:33 with `Logged On`
    /// as its last line, and the next one holding the pipe at 21:28:11.4 with
    /// the log's last run marker still reading 21:27:31. Two seconds in which
    /// the account of a dead client is the answer to "is this one signed in".
    #[test]
    fn a_log_that_has_not_moved_since_the_client_started_is_the_run_befores() {
        let root = std::env::temp_dir().join(format!(
            "lxb-log-age-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(root.join("logs")).expect("a scratch directory");
        let log = root.join("logs").join(CONNECTION_LOG);

        // A machine with no client holding the pipe has no such moment, and
        // unsure counts as this run's — which is what happened before any of
        // this existed.
        assert!(written_since(&root, None));
        // Nor is it asked of a log that cannot be looked at.
        assert!(written_since(&root, Some(std::time::SystemTime::now())));

        std::fs::write(&log, "[2026-09-02 21:27:31] Client version: 1788291500\n")
            .expect("writable");
        let after_it_was_written = std::time::SystemTime::now();
        assert!(
            written_since(&root, Some(std::time::UNIX_EPOCH)),
            "a log written since the client started is that client's"
        );
        assert!(
            !written_since(&root, Some(after_it_was_written)),
            "a client started after the last line of this log did not write it"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A client that is up and has not written its own first line yet is
    /// **starting**, not signed in to whoever ran last.
    ///
    /// Driven against a real FIFO and the real `/proc`: this test process holds
    /// the pipe, so it *is* the running client as far as [`holder_of`] can
    /// tell, and the log is stamped at the epoch — long before this process
    /// started. [`state`] reads it and names an account. [`state_now`] asks how
    /// old the log is against the process holding the pipe, and answers
    /// [`State::Starting`], which is what is true of it.
    ///
    /// The wrong answer was not merely late. `wake` refused the press outright
    /// — *"Steam is signed in to another account. Moving it signs that account
    /// out."* — about a client that had signed in to nothing, and the ordinary
    /// way to reach it is the ordinary way a client ends: killed with the
    /// session that started it, leaving `Logged On` as the last line of a log
    /// the next run appends to.
    #[test]
    fn an_account_from_before_the_running_client_started_is_not_this_clients() {
        use std::os::unix::ffi::OsStrExt;

        let scratch = std::env::temp_dir().join(format!(
            "lxb-stale-account-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let options = Options {
            root: scratch.join("root"),
            home: scratch.join("home"),
        };
        std::fs::create_dir_all(options.root.join("logs")).expect("a scratch directory");
        std::fs::create_dir_all(&options.home).expect("a scratch directory");

        let pipe = options.home.join("steam.pipe");
        let name = std::ffi::CString::new(pipe.as_os_str().as_bytes()).expect("a path");
        assert_eq!(
            unsafe { libc::mkfifo(name.as_ptr(), 0o600) },
            0,
            "a scratch FIFO"
        );
        // Read-only and non-blocking, which is exactly what a client holds it
        // as and what `holder_of` looks for — so this process is now the client.
        let held = unsafe { libc::open(name.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK) };
        assert!(held >= 0, "the FIFO could be held");

        let log = options.root.join("logs").join(CONNECTION_LOG);
        std::fs::write(
            &log,
            "[2026-09-02 21:27:31] Client version: 1788291500\n\
             [2026-09-02 21:27:33] [Logged On, 4, 7] [U:1:82105993] processing complete\n",
        )
        .expect("writable");
        let client = Where::Native(std::path::PathBuf::from("/nonexistent/steam"));
        let us = Credential {
            account: "somebody",
            refresh_token: "not a real token",
            steam_id: 0,
        };

        // As it stands on the disk, which is the run before's.
        assert_eq!(state(Some(&client), &options), State::SignedIn(82105993));

        // Stamped before this process — the one holding the pipe — was born.
        std::fs::File::options()
            .write(true)
            .open(&log)
            .and_then(|file| {
                file.set_times(std::fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH))
            })
            .expect("the log could be dated");
        assert!(!the_log_is_this_runs(&options));
        assert_eq!(
            state_now(Some(&client), &options),
            State::Starting,
            "a client that has not said who it is was taken for the run before's account"
        );
        // Which is the whole of the bug: neither signed in as us nor signed in
        // to somebody else, so nothing refuses the press and nothing calls it
        // ready either.
        assert!(!state_now(Some(&client), &options).signed_in());
        assert!(!state_now(Some(&client), &options).signed_in_as(us));

        // And once this run has written a line of its own, it is this run's.
        std::fs::write(
            &log,
            "[2026-09-02 21:28:12] Client version: 1788291500\n\
             [2026-09-02 21:28:14] [Logged On, 4, 7] [U:1:82105993] processing complete\n",
        )
        .expect("writable");
        assert!(the_log_is_this_runs(&options));
        assert_eq!(
            state_now(Some(&client), &options),
            State::SignedIn(82105993)
        );

        unsafe { libc::close(held) };
        // And with nobody holding the pipe there is no client to date it
        // against — and no client at all, which `is_running` says for one
        // syscall and without opening the log.
        assert!(!is_running(Some(&client), &options));
        assert_eq!(state_now(Some(&client), &options), State::Stopped);

        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// A client is proved by whose session and whose account it is, and a proof
    /// is only good for the client it was made about.
    ///
    /// The two halves of finding 4, on a scratch Steam this process is itself
    /// the client of — a FIFO held open for reading is exactly what
    /// [`holder_of`] looks for, and this process's own environment is what
    /// [`in_this_session`] compares against, so both come out true without a
    /// Steam anywhere near the machine.
    ///
    /// What each assertion is for:
    ///
    /// * A stopped client proves **nothing**, where the check it replaces read
    ///   one as nobody's and let a title's URL start one from cold.
    /// * Another account's client is refused rather than driven.
    /// * A client that has not said who it is yet is refused too — it is about
    ///   to be signed in to whatever it remembered.
    /// * And a proof of one process does not carry to another wearing its
    ///   place, which is the whole of [`Proven::still_there`].
    #[test]
    fn a_proof_names_one_client_and_does_not_carry_to_another() {
        use std::os::unix::ffi::OsStrExt;

        let scratch = std::env::temp_dir().join(format!(
            "lxb-proof-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&scratch);
        let options = Options {
            root: scratch.join("root"),
            home: scratch.join("home"),
        };
        std::fs::create_dir_all(options.root.join("logs")).expect("a scratch directory");
        std::fs::create_dir_all(&options.home).expect("a scratch directory");
        let client = Where::Native(std::path::PathBuf::from("/nonexistent/steam"));
        let ours = Credential {
            account: "somebody",
            refresh_token: "not a real token",
            steam_id: 82_105_993,
        };
        let theirs = Credential {
            steam_id: 4_242_424,
            ..ours
        };

        // Nothing running yet. **This is the case the old check passed**: with
        // no client to inspect it answered "nobody's", and the URL went to a
        // Steam started from cold.
        assert!(matches!(
            prove(&client, &options, ours),
            Err(Refusal::Failed(_))
        ));

        let pipe = options.home.join("steam.pipe");
        let name = std::ffi::CString::new(pipe.as_os_str().as_bytes()).expect("a path");
        assert_eq!(
            unsafe { libc::mkfifo(name.as_ptr(), 0o600) },
            0,
            "a scratch FIFO"
        );
        let held = unsafe { libc::open(name.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK) };
        assert!(held >= 0, "the FIFO could be held");

        // Up, and it has not said who it is. Not this session's to use, and not
        // somebody else's to refuse either: it is about to become one of them.
        std::fs::write(
            options.root.join("logs").join(CONNECTION_LOG),
            "[2026-09-03 19:02:11] Client version: 1788291500\n",
        )
        .expect("writable");
        assert_eq!(state_now(Some(&client), &options), State::Starting);
        assert!(matches!(
            prove(&client, &options, ours),
            Err(Refusal::Failed(_))
        ));

        // Signed in, and to us.
        std::fs::write(
            options.root.join("logs").join(CONNECTION_LOG),
            "[2026-09-03 19:02:11] Client version: 1788291500\n\
             [2026-09-03 19:02:13] [Logged On, 4, 7] [U:1:82105993] processing complete\n",
        )
        .expect("writable");
        let proven = prove(&client, &options, ours).expect("this session's client");
        assert_eq!(proven.account, 82_105_993);
        assert_eq!(
            proven.pid,
            Some(std::process::id()),
            "the process holding the pipe is the one named"
        );
        assert!(proven.still_there(&client, &options).is_ok());

        // The same client, and not ours: the household account beside it.
        assert!(matches!(
            prove(&client, &options, theirs),
            Err(Refusal::NotOurs(_))
        ));

        // A proof of some other process does not hold here, which is what
        // catches a client that was killed and started again between the wake
        // and the request.
        let somebody_else = Proven {
            pid: Some(std::process::id().wrapping_add(1)),
            account: 82_105_993,
            just_signed_in: false,
        };
        assert!(matches!(
            somebody_else.still_there(&client, &options),
            Err(Refusal::Failed(_))
        ));

        // And once it has gone there is nothing to prove and nothing to
        // deliver to. `deliver` never starts one, which is what separates it
        // from `open`.
        unsafe { libc::close(held) };
        assert!(matches!(
            proven.still_there(&client, &options),
            Err(Refusal::Failed(_))
        ));
        assert!(matches!(
            deliver(&client, &options, &proven, "steam://rungameid/440"),
            Err(Refusal::Failed(_))
        ));

        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// A process's age comes off `/proc/uptime` and its own start time, and the
    /// name in the second field is not to be trusted to be one word.
    ///
    /// **`/proc/stat`'s `btime` is the route not taken**, and this is why: it
    /// is whole seconds, and measured on this machine it put a process 0.87 s
    /// before its true start where this route put it 0.3 ms after. A second of
    /// error is the wrong size for a question about two.
    #[test]
    fn a_process_is_dated_by_its_age_and_not_by_the_hour_the_machine_booted() {
        let proc = std::env::temp_dir().join(format!(
            "lxb-proc-age-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(proc.join("41")).expect("a scratch directory");
        std::fs::write(proc.join("uptime"), "1000.00 4000.00\n").expect("writable");

        // Twenty-two fields, of which the twenty-second is the start time. The
        // name is deliberately a bracket and a space, which is what a program
        // called `a) b (c` puts in there — the parse has to start at the last
        // `)` and not the first.
        let ticks = 40_000; // 400 s at 100 Hz, so 600 s old.
        let stat =
            format!("41 (a) b (c) S 1 41 41 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 {ticks} 0 0 0 0 0\n");
        std::fs::write(proc.join("41").join("stat"), stat).expect("writable");

        let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64;
        let age = age_of(&proc, 41).expect("an age");
        assert!(
            (age.as_secs_f64() - (1000.0 - 40_000.0 / hz)).abs() < 0.01,
            "age was {age:?}"
        );

        // A process younger than the machine is not a negative age, and one
        // whose files are unreadable is no answer rather than a wrong one.
        std::fs::write(proc.join("uptime"), "1.00 4.00\n").expect("writable");
        assert_eq!(age_of(&proc, 41), Some(Duration::ZERO));
        assert_eq!(age_of(&proc, 42), None);

        let _ = std::fs::remove_dir_all(&proc);
    }

    /// What the client's own log says it has in hand, which is the one place
    /// a file check is written down at all.
    ///
    /// Every line here is off this machine on 2026-09-02, and the trace is the
    /// whole finding: Steam ran a check on Dispatch for **thirty-six seconds**
    /// with `appmanifest_2592160.acf` reading `StateFlags 4` throughout — a
    /// watcher sampling the file twice a second never saw it rewritten — so
    /// nothing in a manifest, and nothing in a job of this session's, said the
    /// client was busy. The shell shut it down under the check it had itself
    /// asked for.
    #[test]
    fn a_check_that_no_manifest_describes_is_in_the_clients_own_log() {
        let root = std::env::temp_dir().join(format!(
            "lxb-content-log-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(root.join("logs")).expect("a scratch directory");
        let log = root.join("logs").join(CONTENT_LOG);
        // Written whole each time, which is what the client never does — the
        // appending half is [`a_job_is_read_forward_and_not_out_of_a_window`].
        let write = |body: &str| {
            std::fs::write(&log, body).expect("writable");
            let mut jobs = Jobs::default();
            jobs.look(&root);
            jobs
        };

        assert!(!Jobs::default().anything_in_hand(), "no log is no work");
        assert!(!write("nothing to see\n").anything_in_hand());

        let started = "\
[2026-09-02 21:03:12] Client version: 1788291500
[2026-09-02 20:50:49] AppID 2592160 scheduler update : Priority First, not played for 445474 seconds
[2026-09-02 20:50:49] AppID 2592160 state changed : Fully Installed,Update Queued,
[2026-09-02 20:50:49] AppID 2592160 state changed : Fully Installed,Update Queued,Update Running,
[2026-09-02 20:50:49] AppID 2592160 App update changed : Running Update,
[2026-09-02 20:50:49] AppID 2592160 App update changed : Running Update,Reconfiguring,
[2026-09-02 20:50:49] AppID 2592160 App update changed : Running Update,
[2026-09-02 20:50:49] AppID 2592160 App update changed : Running Update,Verifying Installed,
";
        let doing = write(started);
        assert!(doing.anything_in_hand());
        assert_eq!(
            doing.to_the_game(2592160),
            Some(InHand::Checking),
            "a check the manifests say nothing at all about, named as a check"
        );

        // And the line that ends it. `scheduler finished` follows, but `None`
        // is what the client says about the job itself and is enough.
        let ended = format!(
            "{started}\
[2026-09-02 20:51:25] AppID 2592160 App update changed : Running Update,
[2026-09-02 20:51:25] AppID 2592160 App update changed : None
[2026-09-02 20:51:25] AppID 2592160 state changed : Fully Installed,
[2026-09-02 20:51:25] AppID 2592160 scheduler finished : removed from schedule (result No Error, state 0xc)
"
        );
        assert!(!write(&ended).anything_in_hand());

        // **Only this run of the client.** A client killed in the middle of an
        // update never writes the line that ends it, so without the start
        // marker a session that had lost its Steam once would believe that
        // update was in flight for the rest of its life — and nothing would
        // ever close a client again.
        let orphaned = "\
[2026-09-02 18:00:00] Client version: 1788291500
[2026-09-02 18:00:01] AppID 108600 App update changed : Running Update,Verifying Installed,
[2026-09-02 21:03:12] Client version: 1788291500
[2026-09-02 21:03:12] Loaded Steam library folders configuration: /home/x/steamapps/libraryfolders.vdf
";
        assert!(
            !write(orphaned).anything_in_hand(),
            "work the client before this one was killed in the middle of"
        );

        // **The three tracks are three jobs, and they run at once.** A shader
        // cache must never reach a row — the game is on the disk and plays
        // perfectly while Steam fetches one — and must equally not be
        // interrupted, because one of them ran for sixty-four minutes here.
        let mixed = "\
[2026-09-02 21:03:12] Client version: 1788291500
[2026-09-02 21:03:16] AppID 241100 Workshop update changed : Running Update,Staging,
[2026-09-02 21:03:16] AppID 3812600 Shader update changed : Running Update,Downloading,Staging,
";
        let doing = write(mixed);
        assert!(
            doing.anything_in_hand(),
            "so a client is not shut down under it"
        );
        assert_eq!(doing.to_the_game(241100), Some(InHand::Working));
        assert_eq!(
            doing.to_the_game(3812600),
            None,
            "and a shader cache is never a word on a row"
        );
        // It is still asked about, by the one thing that has a use for it: a
        // press standing there while Steam fetches five gigabytes of them.
        assert!(doing.fetching_shaders_for(3812600));
        assert!(!doing.fetching_shaders_for(241100), "that one is Workshop");

        // A shader job of one game's ending says nothing about the update of
        // the same game, which is why the two are not one entry.
        let both = "\
[2026-09-02 21:03:12] Client version: 1788291500
[2026-09-02 21:03:16] AppID 108600 App update changed : Running Update,Verifying Installed,
[2026-09-02 21:03:16] AppID 108600 Shader update changed : Running Update,Downloading,Staging,
[2026-09-02 21:03:18] AppID 108600 Shader update changed : None
";
        let both = write(both);
        assert_eq!(both.to_the_game(108600), Some(InHand::Checking));
        assert!(
            !both.fetching_shaders_for(108600),
            "and the shader half of it said it had finished"
        );

        // `Verifying Staged` is the other check, and is not this one: it reads
        // back what has just arrived, inside a download the manifest is already
        // describing.
        let staged = "\
[2026-09-02 21:03:12] Client version: 1788291500
[2026-09-02 21:03:16] AppID 108600 App update changed : Running Update,Verifying Staged,
";
        assert_eq!(write(staged).to_the_game(108600), Some(InHand::Working));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A job outlives its own line, so the log is read forward rather than
    /// looked at through a window.
    ///
    /// The client writes `App update changed` only when a job moves between
    /// phases, and it writes a great deal else in between — cache connections,
    /// schedulers, the other two tracks. So the first cut of this, which folded
    /// the last 64 KB every time, lost a long job the moment its line was
    /// pushed out: the check went on running and the shell believed it had
    /// finished.
    #[test]
    fn a_job_is_read_forward_and_not_out_of_a_window() {
        let root = std::env::temp_dir().join(format!(
            "lxb-content-stream-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(root.join("logs")).expect("a scratch directory");
        let log = root.join("logs").join(CONTENT_LOG);
        let append = |body: &str| {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log)
                .expect("writable");
            file.write_all(body.as_bytes()).expect("written");
        };

        let mut jobs = Jobs::default();
        append(
            "[2026-09-02 21:03:12] Client version: 1788291500\n\
             [2026-09-02 21:03:16] AppID 730 App update changed : Running Update,Verifying Installed,\n",
        );
        assert!(jobs.look(&root), "the answer moved");
        assert_eq!(jobs.to_the_game(730), Some(InHand::Checking));
        assert!(!jobs.look(&root), "and a look at nothing new moves nothing");

        // Two hundred kilobytes of everything else, which is a quarter of an
        // hour of a busy client and three times the window the first cut used.
        for _ in 0..2000 {
            append("[2026-09-02 21:04:00] HTTPS (SteamCache,494) - cache12-waw1.steamcontent.com: Closing connection, and some more of the same to fill the line out to a hundred characters\n");
        }
        assert!(!jobs.look(&root), "none of that is about a job");
        assert_eq!(
            jobs.to_the_game(730),
            Some(InHand::Checking),
            "the check is still running and its own line is long gone"
        );

        append("[2026-09-02 21:19:00] AppID 730 App update changed : None\n");
        assert!(jobs.look(&root));
        assert_eq!(jobs.to_the_game(730), None);

        // A line the client is still writing is not folded until it is whole,
        // or the only word that mattered would be skipped with the rest of it.
        append("[2026-09-02 21:20:00] AppID 730 App update changed : Running Upd");
        assert!(!jobs.look(&root), "half a line says nothing yet");
        append("ate,Verifying Installed,\n");
        assert!(jobs.look(&root));
        assert_eq!(jobs.to_the_game(730), Some(InHand::Checking));

        // And the log being emptied under it is a file to be read from its
        // start again, not one that has gone backwards. **What the client has
        // in hand survives it.** The check is still running: no log said it had
        // ended, and a log being shorter than it was is a fact about the log.
        std::fs::write(
            &log,
            "[2026-09-02 21:30:00] AppID 108600 App update changed : Running Update,\n",
        )
        .expect("writable");
        assert!(jobs.look(&root));
        assert_eq!(
            jobs.to_the_game(730),
            Some(InHand::Checking),
            "nothing said the check had ended"
        );
        assert_eq!(jobs.to_the_game(108600), Some(InHand::Working));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A job outlives the log it was written into, so a rotation is not the end
    /// of it.
    ///
    /// The client renames this log at four megabytes and opens another at the
    /// same path, and a download fills it fast: measured on this machine on
    /// 2026-09-02, the rotated log's last nine minutes were sixty-four
    /// kilobytes of failed-allocation lines, and the update running across the
    /// boundary had its last phase line in one file and the line that ended it
    /// in the next.
    ///
    /// Two things had to be true for that to be read correctly, and neither was.
    /// The reader has to notice the file is a different file — the offset alone
    /// says nothing, and a new log that has already grown past the old offset
    /// looks like an ordinary append. And it must not throw away what the
    /// client has in hand when it notices, because nothing about the work
    /// changed: the client says a job is over by saying so.
    #[test]
    fn a_job_survives_the_log_being_rotated_under_it() {
        let root = std::env::temp_dir().join(format!(
            "lxb-content-rotate-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(root.join("logs")).expect("a scratch directory");
        let log = root.join("logs").join(CONTENT_LOG);
        let append = |body: &str| {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log)
                .expect("writable");
            file.write_all(body.as_bytes()).expect("written");
        };

        let mut jobs = Jobs::default();
        append(
            "[2026-09-02 18:00:00] Client version: 1788291500\n\
             [2026-09-02 18:00:01] AppID 108600 App update changed : Running Update,Downloading,Staging,\n",
        );
        assert!(jobs.look(&root));
        assert_eq!(jobs.to_the_game(108600), Some(InHand::Working));

        // What the client wrote between that look and the rotation, which this
        // reader never saw at the live name: a second job began, and it is in
        // the file that is about to be moved aside.
        append(
            "[2026-09-02 18:34:21] AppID 241100 Workshop update changed : Running Update,Staging,\n",
        );
        std::fs::rename(&log, root.join("logs").join(CONTENT_LOG_BEFORE)).expect("renamable");
        // And the log the client opens in its place, longer than the old offset
        // so that nothing about the length says anything happened. The update
        // goes on across it and says nothing, because nothing about it changed.
        let mut fresh = String::new();
        for _ in 0..40 {
            fresh.push_str("[2026-09-02 18:34:22] HTTPS (SteamCache,494) - cache12-waw1.steamcontent.com: Closing connection, and rather more of the same to fill the line out\n");
        }
        std::fs::write(&log, &fresh).expect("writable");
        assert!(
            std::fs::metadata(&log).expect("there").len() > 200,
            "a new log that has already grown past the old offset"
        );

        assert!(jobs.look(&root));
        assert_eq!(
            jobs.to_the_game(108600),
            Some(InHand::Working),
            "the update is still running and its log has been rolled over"
        );
        assert_eq!(
            jobs.to_the_game(241100),
            Some(InHand::Working),
            "and the job that began in the stretch this reader had not got to yet"
        );

        // The client is what ends a job, and it is read from the new file the
        // same as from the old one.
        append("[2026-09-02 18:34:22] AppID 108600 App update changed : None\n");
        assert!(jobs.look(&root));
        assert_eq!(jobs.to_the_game(108600), None);
        assert_eq!(jobs.to_the_game(241100), Some(InHand::Working));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A shell that starts while a client is already running joins that
    /// client's run at the beginning of it.
    ///
    /// The log is not a state, so where the reading starts decides what is
    /// known — and the first cut started a megabyte back from the end, which is
    /// a length and not a boundary. It is wrong in both directions. A client
    /// that has been up a while writes far more than that (this machine's
    /// rotated log holds ten days above its last megabyte), so a job whose last
    /// phase line is older than the window is missing, and the shell believes a
    /// game Steam has in hand is idle: the row goes back to "Installed", a
    /// press on it is answered as though it would start, and the client is shut
    /// down under the work. And a window that reaches back past the run marker
    /// into a *previous* run picks up jobs a killed client never finished.
    #[test]
    fn a_client_already_running_is_joined_at_the_start_of_its_run() {
        let root = std::env::temp_dir().join(format!(
            "lxb-content-attach-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(root.join("logs")).expect("a scratch directory");
        let log = root.join("logs").join(CONTENT_LOG);

        // A megabyte and a fifth of everything else, which is what a busy
        // client writes over a long download and is more than the window the
        // first cut of this looked through.
        let mut noise = String::new();
        while noise.len() < 1024 * 1024 + 200 * 1024 {
            noise.push_str("[2026-09-02 21:04:00] HTTPS (SteamCache,494) - cache12-waw1.steamcontent.com: Closing connection, and some more of the same to fill the line out to a hundred characters\n");
        }

        let running = format!(
            "[2026-09-02 17:00:00] Client version: 1788291500\n\
             [2026-09-02 17:00:01] AppID 480 App update changed : Running Update,Verifying Installed,\n\
             [2026-09-02 18:00:00] Client version: 1788291500\n\
             [2026-09-02 18:00:01] AppID 730 App update changed : Running Update,Verifying Installed,\n\
             {noise}"
        );
        std::fs::write(&log, &running).expect("writable");
        let mut jobs = Jobs::default();
        jobs.look(&root);
        assert_eq!(
            jobs.to_the_game(730),
            Some(InHand::Checking),
            "a check that began before the last megabyte is still a check"
        );
        assert_eq!(
            jobs.to_the_game(480),
            None,
            "and work the client before this one was killed in the middle of is not this one's"
        );

        // The steady state carries on from there: the client ends the job and
        // the next look reads only what arrived.
        {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&log)
                .expect("writable");
            file.write_all(b"[2026-09-02 21:19:00] AppID 730 App update changed : None\n")
                .expect("written");
        }
        assert!(jobs.look(&root));
        assert_eq!(jobs.to_the_game(730), None);

        // And a run that began before its own log was rotated: the start of it
        // is in the file the client moved aside, and there is no other way to
        // know what this client has in hand.
        std::fs::rename(&log, root.join("logs").join(CONTENT_LOG_BEFORE)).expect("renamable");
        std::fs::write(
            &log,
            "[2026-09-02 21:30:00] HTTPS (SteamCache,494) - cache12-waw1.steamcontent.com: Closing connection\n",
        )
        .expect("writable");
        let mut joining = Jobs::default();
        joining.look(&root);
        assert_eq!(
            joining.to_the_game(730),
            None,
            "the client said that one was over before the log rolled over"
        );

        std::fs::write(
            root.join("logs").join(CONTENT_LOG_BEFORE),
            format!(
                "[2026-09-02 18:00:00] Client version: 1788291500\n\
                 [2026-09-02 18:00:01] AppID 730 App update changed : Running Update,Downloading,Staging,\n\
                 {noise}"
            ),
        )
        .expect("writable");
        let mut joining = Jobs::default();
        joining.look(&root);
        assert_eq!(
            joining.to_the_game(730),
            Some(InHand::Working),
            "the run began in the log that was moved aside, and the update is still running"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A launch Valve's client gave up on says so in its own log, twenty
    /// seconds in, and nowhere else at all.
    ///
    /// Every line here is off this machine on 2026-09-03. The press on
    /// Counter-Strike 2 walked to `DownloadingDepots` and failed with
    /// `AppError_19` — "Update required" — which the client says in a modal
    /// this shell holds off the screen. The loading screen waited its full
    /// minute and said the game had not started; Steam then finished the very
    /// update the launch had asked it to schedule, and started nothing.
    #[test]
    fn a_launch_the_client_gave_up_on_is_in_its_own_log() {
        let walking = "\
[2026-09-03 23:46:41] ExecuteSteamURL: \"steam://rungameid/730\"
[2026-09-03 23:46:41] GameAction [AppID 730, ActionID 1] : LaunchApp changed task to UpdatingAppInfo with \"\"
[2026-09-03 23:46:42] GameAction [AppID 730, ActionID 1] : LaunchApp changed task to CheckShaderDepotManifest with \"\"
[2026-09-03 23:46:43] IPC function call IClientUser::GetAssociatedSiteName took too long: 71 msec
[2026-09-03 23:46:43] GameAction [AppID 730, ActionID 1] : LaunchApp changed task to DownloadingDepots with \"\"
";
        assert_eq!(
            what_became_of_the_launch(walking, 730),
            Some(LaunchStanding::Working("DownloadingDepots".to_string())),
            "a launch that is still being walked says which step it is on"
        );

        let refused = format!(
            "{walking}\
[2026-09-03 23:47:02] GameAction [AppID 730, ActionID 1] : LaunchApp failed with AppError_19 with \"\"
[2026-09-03 23:47:02] GameAction [AppID 730, ActionID 1] : LaunchApp changed task to Failed with \"\"
[2026-09-03 23:47:20] IPC function call IClientUGC::GetAppItemsStatus took too long: 62 msec
"
        );
        assert_eq!(
            what_became_of_the_launch(&refused, 730),
            Some(LaunchStanding::Refused("AppError_19".to_string()))
        );
        assert_eq!(
            what_became_of_the_launch(&refused, 504230),
            None,
            "and it says nothing about anybody else's game"
        );

        // The whole walk of a launch that worked, off the same log an hour
        // earlier, questions and all. `Completed` is the client's half done.
        let started = "\
[2026-09-03 22:27:57] GameAction [AppID 504230, ActionID 1] : LaunchApp changed task to CheckShaderDepotManifest with \"\"
[2026-09-03 22:27:58] GameAction [AppID 504230, ActionID 1] : LaunchApp changed task to ShowInterstitials with \"\"
[2026-09-03 22:27:58] GameAction [AppID 504230, ActionID 1] : LaunchApp waiting for user response to ShowInterstitials \"\"
[2026-09-03 22:27:58] GameAction [AppID 504230, ActionID 1] : LaunchApp continues with user response \"ShowInterstitials\"
[2026-09-03 22:27:59] GameAction [AppID 504230, ActionID 1] : LaunchApp changed task to CreatingProcess with \"\"
[2026-09-03 22:27:59] GameAction [AppID 504230, ActionID 1] : LaunchApp changed task to WaitingGameWindow with \"\"
[2026-09-03 22:27:59] GameAction [AppID 504230, ActionID 1] : LaunchApp changed task to Completed with \"\"
";
        assert_eq!(
            what_became_of_the_launch(started, 504230),
            Some(LaunchStanding::Started)
        );

        // A press that failed and was made again: the last word wins, and the
        // press before it is not this one's news. Both are in the file, which
        // is why the reading starts where the press did — see
        // [`launches_so_far`].
        let again = format!("{refused}{started}");
        assert_eq!(
            what_became_of_the_launch(&again, 730),
            Some(LaunchStanding::Refused("AppError_19".to_string())),
            "the other game's launch says nothing about this one"
        );

        // And the client stopping at `Failed` with the reason rolled out of
        // the stretch being read is still a refusal.
        let cut_off = "\
[2026-09-03 23:47:02] GameAction [AppID 730, ActionID 1] : LaunchApp changed task to Failed with \"\"
";
        assert_eq!(
            what_became_of_the_launch(cut_off, 730),
            Some(LaunchStanding::Refused(String::new()))
        );

        assert_eq!(what_became_of_the_launch("", 730), None);
        assert_eq!(what_became_of_the_launch("nothing to see\n", 730), None);
    }

    /// A launch that has stopped on a step says so in the same log, which is
    /// how a client that exposes no interface can still be seen doing it.
    ///
    /// Off this machine at 00:10 on 2026-09-04, and it is the whole of what the
    /// shell had to go on: Valve's own "Processing Vulkan shaders (0%)" dialog
    /// was up behind the loading screen, on a session driven with a controller,
    /// and the client this session did not start exposes nothing to ask.
    #[test]
    fn a_launch_that_has_stopped_on_a_step_says_which() {
        let compiling = "\
[2026-09-04 00:10:17] GameAction [AppID 730, ActionID 1] : LaunchApp changed task to SynchronizingControllerConfig with \"\"
[2026-09-04 00:10:17] GameAction [AppID 730, ActionID 1] : LaunchApp changed task to ProcessingShaderCache with \"\"
[2026-09-04 00:10:17] GameAction [AppID 730, ActionID 1] : LaunchApp waiting for user response to ProcessingShaderCache \"\"
";
        assert_eq!(
            what_became_of_the_launch(compiling, 730),
            Some(LaunchStanding::Waiting("ProcessingShaderCache".to_string())),
            "the task's name, without the details that follow it"
        );

        // And the client going on from it — because somebody pressed Skip, or
        // because it finished — is a launch that is walking again.
        let went_on = format!(
            "{compiling}\
[2026-09-04 00:10:31] GameAction [AppID 730, ActionID 1] : LaunchApp continues with user response \"SkipShaders\"
[2026-09-04 00:10:31] GameAction [AppID 730, ActionID 1] : LaunchApp changed task to CreatingProcess with \"\"
"
        );
        assert_eq!(
            what_became_of_the_launch(&went_on, 730),
            Some(LaunchStanding::Working("CreatingProcess".to_string()))
        );

        // The interstitial the same launch stopped on a moment earlier, which
        // this shell has no panel for and which the client answers itself.
        let interstitial = "\
[2026-09-04 00:10:17] GameAction [AppID 730, ActionID 1] : LaunchApp waiting for user response to ShowInterstitials \"\"
";
        assert_eq!(
            what_became_of_the_launch(interstitial, 730),
            Some(LaunchStanding::Waiting("ShowInterstitials".to_string()))
        );
    }

    /// A launch that is fetching the game before it starts it says so on every
    /// poll, for as long as it takes.
    ///
    /// **Reported from use on 2026-09-04, with a screenshot** of Valve's own
    /// launch window — *"Starting game / Counter-Strike 2 / LAUNCHING /
    /// Downloading content (19%)"* — which came out from under this shell's
    /// loading screen after the shell had already said the game failed to
    /// start. These are the lines the client had written by then, verbatim:
    /// two of them, one second after the press, and then an hour of silence
    /// because a task that does not change is not written down again.
    ///
    /// Nothing else on the machine answered. The game's own manifest read
    /// `StateFlags 1158` — installed, update required, files corrupt, update
    /// started — which is [`crate::library::Standing::Broken`], not one of the
    /// standings the loading screen watches, and its byte counters were all
    /// nought. So the only account of that press anywhere was this one.
    #[test]
    fn a_launch_fetching_the_game_first_says_so_until_it_stops() {
        let fetching = "\
[2026-09-04 00:54:39] ExecuteSteamURL: \"steam://rungameid/730\"
[2026-09-04 00:54:39] GameAction [AppID 730, ActionID 1] : LaunchApp changed task to UpdatingAppInfo with \"\"
[2026-09-04 00:54:39] GameAction [AppID 730, ActionID 1] : LaunchApp changed task to CheckShaderDepotManifest with \"\"
[2026-09-04 00:54:40] GameAction [AppID 730, ActionID 1] : LaunchApp changed task to DownloadingDepots with \"\"
[2026-09-04 00:54:40] GameAction [AppID 730, ActionID 1] : LaunchApp changed task to DownloadingDepots with \"\"
";
        assert_eq!(
            what_became_of_the_launch(fetching, 730),
            Some(LaunchStanding::Working("DownloadingDepots".to_string())),
        );

        // A minute later, with the whole of the client's ordinary noise written
        // over it and the launch still on the same step. This is the moment the
        // loading screen gave up.
        let a_minute_on = format!(
            "{fetching}\
[2026-09-04 00:55:41] HTTPS (SteamCache,493) - cache11-waw1.steamcontent.com: Connection has been idle for '61' seconds, closing
[2026-09-04 00:55:42] Timeout calling process '/mnt/GamesSSD/SteamLibrary/steamapps/common/SteamLinuxRuntime_4'/_v2-entry-point --verb=run
"
        );
        assert_eq!(
            what_became_of_the_launch(&a_minute_on, 730),
            Some(LaunchStanding::Working("DownloadingDepots".to_string())),
            "a step nothing has written over is the step the client is still on"
        );

        // And the two ends of the walk still win, from the same stretch: a step
        // in flight is the last word only while it is the last word.
        let started = format!(
            "{fetching}\
[2026-09-04 00:56:10] GameAction [AppID 730, ActionID 1] : LaunchApp changed task to CreatingProcess with \"\"
[2026-09-04 00:56:10] GameAction [AppID 730, ActionID 1] : LaunchApp changed task to Completed with \"\"
"
        );
        assert_eq!(
            what_became_of_the_launch(&started, 730),
            Some(LaunchStanding::Started)
        );
        let refused = format!(
            "{fetching}\
[2026-09-04 00:56:10] GameAction [AppID 730, ActionID 1] : LaunchApp failed with AppError_19 with \"\"
[2026-09-04 00:56:10] GameAction [AppID 730, ActionID 1] : LaunchApp changed task to Failed with \"\"
"
        );
        assert_eq!(
            what_became_of_the_launch(&refused, 730),
            Some(LaunchStanding::Refused("AppError_19".to_string()))
        );
    }

    /// Every line the client writes carries a state and an account, and only
    /// the state says whether it is signed in.
    ///
    /// Lines taken from a real client's log. The second of them is the whole
    /// reason this reads the state at all: two seconds into starting, the
    /// client has already written the account it is *about* to log on as, and
    /// reading the stamp alone called that signed in.
    #[test]
    fn only_the_logged_on_state_means_signed_in() {
        assert_eq!(logged_on_in(""), None);
        assert_eq!(logged_on_in("nothing to see"), None);

        let starting = "\
[2026-08-12 12:41:22] Client version: 1785799196
[2026-08-12 12:41:22] [Logged Off, 0, 0] [U:1:0] CCMInterface::SetSteamID( [U:1:0] )
[2026-08-12 12:41:24] [Logged Off, 0, 0] [U:1:82105993] CCMInterface::SetSteamID( [U:1:82105993] )
[2026-08-12 12:41:24] [Logged Off, 4, 0] [U:1:82105993] LogOn() called; not connected yet";
        assert_eq!(
            logged_on_in(starting),
            None,
            "it knows the account it is about to be; it is not it yet"
        );

        let on = format!(
            "{starting}
[2026-08-12 12:41:25] [Logging On, 4, 7] [U:1:82105993] RecvMsgClientLogOnResponse() : 'OK'
[2026-08-12 12:41:25] [Logged On, 4, 7] [U:1:82105993] RecvMsgClientLogOnResponse() : processing complete
[2026-08-12 12:41:25] CClientJobGetClientUpdateHosts: cached version not expired"
        );
        assert_eq!(logged_on_in(&on), Some(82105993));

        // And one that has gone is nobody, though its account is still the
        // last one stamped anywhere in the file.
        let off = format!(
            "{on}
[2026-08-12 12:45:01] [Logged On, 4, 7] [U:1:82105993] LogOff()
[2026-08-12 12:45:03] [Logged Off, 0, 0] [U:1:82105993] Log session ended"
        );
        assert_eq!(logged_on_in(&off), None);
    }

    /// The log is appended to across runs, so a client that was shut down an
    /// hour ago has left the last word in the file. Only the current run
    /// counts, and `Client version:` is where it starts.
    #[test]
    fn a_previous_run_does_not_speak_for_this_one() {
        let log = "\
[2026-08-12 12:32:53] [Logged On, 4, 7] [U:1:82105993] RecvMsgClientLogOnResponse() : processing complete
[2026-08-12 12:35:19] [Logging Off, 4, 7] [U:1:82105993] AsyncDisconnect( bDontWaitOnTCPShutdown: false )

[2026-08-12 12:39:24] Client version: 1785799196
[2026-08-12 12:39:24] Connectivity test: Starting test";
        assert_eq!(
            logged_on_in(log),
            None,
            "the new client has not signed in yet"
        );

        // A run that was killed rather than shut down leaves `Logged On` as
        // its last word, which is exactly the case the marker has to cut.
        let killed = "\
[2026-08-12 12:32:53] [Logged On, 4, 7] [U:1:82105993] RecvMsgClientLogOnResponse() : processing complete

[2026-08-12 12:39:24] Client version: 1785799196
[2026-08-12 12:39:24] [Logged Off, 0, 0] [U:1:0] CCMInterface::SetSteamID( [U:1:0] )";
        assert_eq!(logged_on_in(killed), None);
    }

    /// A line stamped with an account but no state block says nothing about
    /// whether the client is signed in, and must not be read as though it
    /// did — the timestamp is bracketed too, and taking it for a state would
    /// make every such line a sign-out.
    #[test]
    fn a_line_without_a_state_is_not_read_as_one() {
        assert_eq!(stamped("[2026-08-12 12:41:25] plain [U:1:7] hello"), None);
        assert_eq!(
            stamped("[2026-08-12 12:41:25] [Logged On, 4, 7] [U:1:7] hello"),
            Some(("Logged On", 7, " hello"))
        );
        // So a state line still speaks for the client when a stateless one
        // follows it, which is what the client's log actually looks like.
        assert_eq!(
            logged_on_in(
                "[2026-08-12 12:41:25] [Logged On, 4, 7] [U:1:7] on\n[2026-08-12 12:41:25] later [U:1:7] noise"
            ),
            Some(7)
        );
    }

    /// The four states are one question each, and the two that mean "it is
    /// there" agree about it.
    #[test]
    fn what_the_states_mean() {
        assert!(!State::Absent.running() && !State::Absent.signed_in());
        assert!(!State::Stopped.running() && !State::Stopped.signed_in());
        assert!(State::Starting.running() && !State::Starting.signed_in());
        assert!(State::SignedIn(1).running() && State::SignedIn(1).signed_in());
    }

    /// The two rows that open the client open two different things, and the
    /// plain name belongs to the one a pad can drive.
    /// The one number in Valve's client's config that says where the account
    /// stands on this machine.
    ///
    /// The line is taken verbatim off this machine on 2026-09-03, escaping and
    /// all: the value is a JSON document that has been escaped to live inside a
    /// VDF string, which is why this reads a digit out of it rather than
    /// parsing either format.
    #[test]
    fn the_clients_own_record_of_where_the_account_stands() {
        let line = r#"		"FriendStoreLocalPrefs_82105993"		"{\"ePersonaState\":7,\"strNonFriendsAllowedToMsg\":\"\"}""#;
        assert_eq!(
            persona_state_in(line),
            Some(crate::friends::Presence::Invisible)
        );
        assert_eq!(
            persona_state_in(&line.replace(":7", ":1")),
            Some(crate::friends::Presence::Online)
        );
        assert_eq!(
            persona_state_in(&line.replace(":7", ":3")),
            Some(crate::friends::Presence::Away)
        );

        // A line with no such field, and a number that is not a state. Both
        // have to be nothing rather than Offline: this decides what the shell
        // announces, and a config it could not read must not put somebody
        // offline. See `Presence::from_number`.
        assert_eq!(persona_state_in(r#"		"PersonaName"		"Petexon""#), None);
        assert_eq!(persona_state_in(&line.replace(":7", ":9")), None);
    }

    /// Whether a client will sign itself in, and what a first-run one says.
    ///
    /// Two registries with nobody in them, and they are not the same file. The
    /// one this shell signs a client out of has the field and it is empty,
    /// copied out of this developer's `~/.steam/registry.vdf` on 2026-09-04.
    /// The one Valve's launcher writes on a machine where Steam has never
    /// signed anybody in has no field at all — the launcher writes it at the
    /// first logon — copied out of a fresh install on 2026-09-13.
    #[test]
    fn a_client_that_has_never_signed_anybody_in_will_not_sign_itself_in() {
        let home = std::env::temp_dir().join(format!("lxb-account-read-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();

        let registry = |account: &str| {
            format!(
                "\"Registry\"\n{{\n\t\"HKCU\"\n\t{{\n\t\t\"Software\"\n\t\t{{\n\t\t\t\"Valve\"\n\t\t\t{{\n\t\t\t\t\"Steam\"\n\t\t\t\t{{\n\t\t\t\t\t\"AutoLoginUser\"\t\t\"{account}\"\n\t\t\t\t}}\n\t\t\t}}\n\t\t}}\n\t}}\n}}\n"
            )
        };
        // What the launcher leaves before anybody has logged on: the client's
        // pid and its language, and no account field at all.
        let never_signed_in = "\"Registry\"\n{\n\t\"HKLM\"\n\t{\n\t\t\"Software\"\n\t\t{\n\t\t\t\"Valve\"\n\t\t\t{\n\t\t\t\t\"Steam\"\n\t\t\t\t{\n\t\t\t\t\t\"SteamPID\"\t\t\"318776\"\n\t\t\t\t\t\"ClientLauncherType\"\t\t\"0\"\n\t\t\t\t}\n\t\t\t}\n\t\t}\n\t}\n\t\"HKCU\"\n\t{\n\t\t\"Software\"\n\t\t{\n\t\t\t\"Valve\"\n\t\t\t{\n\t\t\t\t\"Steam\"\n\t\t\t\t{\n\t\t\t\t\t\"language\"\t\t\"english\"\n\t\t\t\t}\n\t\t\t}\n\t\t}\n\t}\n}\n";

        // Nothing on the disk at all: the cautious answer, which costs only the
        // wait it would otherwise have saved.
        assert!(account::could_sign_itself_in(&home));

        // A client this shell signed out. This is the whole point of the
        // function: waiting for this one to sign itself in is thirty seconds
        // spent on something that cannot happen.
        std::fs::write(home.join("registry.vdf"), registry("")).unwrap();
        assert!(!account::could_sign_itself_in(&home));

        // And a client that has never signed anybody in, which is the one the
        // shell has just installed. It has nobody to be either, and reading
        // its missing field as "could" cost the first press after a first
        // setup the whole thirty seconds.
        std::fs::write(home.join("registry.vdf"), never_signed_in).unwrap();
        assert!(!account::could_sign_itself_in(&home));

        // And an ordinary machine, where waiting is by far the cheapest way to
        // a signed-in client and must go on happening.
        std::fs::write(home.join("registry.vdf"), registry("someone")).unwrap();
        assert!(account::could_sign_itself_in(&home));

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn open_steam_is_the_console_one() {
        assert_eq!(Doing::BigPicture.label(), "Open Steam");
        assert_eq!(
            Doing::BigPicture.url(0),
            "steam://open/bigpicture",
            "the plainly named row raises the desktop client instead"
        );
        assert_eq!(Doing::Open.label(), "Open Steam (Client)");
        assert_eq!(Doing::Open.url(0), "steam://open/main");

        // Both ignore the id they are given, which is what lets the row be
        // pressed with nothing selected; the ones about a title do not.
        for doing in [Doing::BigPicture, Doing::Open] {
            assert!(doing.about_the_client());
            assert_eq!(doing.url(730), doing.url(0));
        }
        for doing in [Doing::Install, Doing::Verify] {
            assert!(!doing.about_the_client());
            assert!(doing.url(730).ends_with("/730"));
        }
    }

    /// A request made with no client running must not be waited on.
    ///
    /// The whole of the freeze, measured. `steam <url>` is two different
    /// programs depending on what is already running: a courier that exits in
    /// milliseconds, or — with nothing to hand the URL to — the client itself,
    /// which exits when the user quits Steam. Waiting on the second one from
    /// the shell's own thread is a screen stopped on its last frame, its music
    /// still playing, with Steam audible and never shown; that is what this
    /// stands on. The stand-in client here behaves like the real one: it does
    /// not exit.
    #[test]
    fn a_request_with_no_client_running_is_not_waited_on() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!("lxb-open-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let script = root.join("steam");
        std::fs::write(&script, "#!/bin/sh\nexec sleep 10\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        // No `steam.pipe` in this home, so nothing is listening and this is a
        // machine with no client up — the state the freeze happened in.
        let options = Options {
            root: root.clone(),
            home: root.clone(),
        };
        assert_eq!(
            state(Some(&Where::Native(script.clone())), &options),
            State::Stopped
        );

        let began = Instant::now();
        open(
            &Where::Native(script),
            Some(&options),
            "steam://open/bigpicture",
        )
        .expect("it should have started one");
        assert!(
            began.elapsed() < Duration::from_secs(3),
            "it waited {:?} for a client that was never going to exit",
            began.elapsed()
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// With no client on the machine, nothing about the disk can change the
    /// answer: there is nothing to be running.
    #[test]
    fn no_client_means_absent_whatever_is_on_the_disk() {
        let nowhere = Options {
            root: PathBuf::from("/nonexistent"),
            home: PathBuf::from("/nonexistent"),
        };
        assert_eq!(state(None, &nowhere), State::Absent);
        // And with a client but nothing running, it is stopped rather than
        // signed in — the direction that matters, because the other way round
        // is a game started against a client that is not there.
        let client = Where::Native(PathBuf::from("/usr/bin/steam"));
        assert_eq!(state(Some(&client), &nowhere), State::Stopped);
    }

    /// The comparison that decides whether the running client is this
    /// session's, on the environment blocks that produced the bug.
    ///
    /// The first is a client Plasma started, read out of `/proc` on the machine
    /// where a game was pressed in the shell, launched perfectly, and opened on
    /// the other desktop. The second is what the compositor gives everything it
    /// starts. Nothing else about them differs enough to matter and no log
    /// anywhere records the difference, which is why it is checked here.
    #[test]
    fn a_client_on_another_display_is_not_this_sessions() {
        let plasma =
            b"LANG=en_GB.UTF-8\0WAYLAND_DISPLAY=wayland-0\0DISPLAY=:1\0XDG_CURRENT_DESKTOP=KDE\0";
        let shell = b"LANG=en_GB.UTF-8\0WAYLAND_DISPLAY=lxb-0\0DISPLAY=:2\0XDG_CURRENT_DESKTOP=LineXinBar\0";

        assert_ne!(displays_in(plasma), displays_in(shell));
        assert_eq!(displays_in(shell), displays_in(shell));

        // Only the two that decide where a window goes are read out at all;
        // the rest of a stranger's environment is nobody's business here.
        assert_eq!(
            displays_in(plasma),
            vec![Some("wayland-0".to_string()), Some(":1".to_string())]
        );
    }

    /// A client signed in to somebody else is not a client this session may
    /// use, however signed in it is.
    ///
    /// The account id is the low half of a SteamID, which is the only form a
    /// running client ever says out loud: it stamps its connection log with
    /// `[U:1:<account>]` and never writes the whole number anywhere. So the
    /// comparison has to be made in that half, and the shell holds the other
    /// form — which is exactly the sort of mismatch that gets written as
    /// "signed in, near enough" and then hands somebody another household
    /// member's library.
    ///
    /// The numbers are a real pair: the account id is the one in the log lines
    /// [`only_the_logged_on_state_means_signed_in`] is built from, and the
    /// SteamID is that account's.
    #[test]
    fn signed_in_is_not_the_same_as_signed_in_as_us() {
        const ACCOUNT: u32 = 82105993;
        const STEAM_ID: u64 = 76561198042371721;

        let us = Credential {
            account: "someone",
            steam_id: STEAM_ID,
            refresh_token: "not a real token",
        };
        assert_eq!(us.account_id(), ACCOUNT, "the halves do not line up");

        assert!(State::SignedIn(ACCOUNT).signed_in_as(us));
        assert!(!State::SignedIn(ACCOUNT + 1).signed_in_as(us));
        assert!(!State::Starting.signed_in_as(us));
        assert!(!State::Stopped.signed_in_as(us));
        assert!(!State::Absent.signed_in_as(us));

        // And the two questions really are different: the household's other
        // account is signed in, by every measure but the one that matters.
        let theirs = State::SignedIn(ACCOUNT + 1);
        assert!(theirs.signed_in(), "the test is not testing anything");
        assert!(!theirs.signed_in_as(us));

        // Offline Mode is signed in, and to exactly one account. It is what a
        // game press needs and what somebody else's client still is not.
        assert!(State::Offline(ACCOUNT).signed_in_as(us));
        assert!(State::Offline(ACCOUNT).running());
        assert!(State::Offline(ACCOUNT).offline());
        assert!(!State::Offline(ACCOUNT + 1).signed_in_as(us));
        assert!(!State::SignedIn(ACCOUNT).offline(), "on Steam, not offline");
    }

    /// A client in Valve's Offline Mode never reaches `Logged On`, and the
    /// stamp it does leave is the only thing in that log saying whose it is.
    ///
    /// The lines are a real client's, copied from this machine on 2026-09-02
    /// out of a cold start made with Offline Mode on.
    #[test]
    fn an_offline_client_says_whose_it_is_without_ever_logging_on() {
        const OFFLINE_RUN: &str = "\
[2026-09-02 00:30:54] Client version: 1785799196
[2026-09-02 00:30:54] [Logged Off, 0, 0] [U:1:0] CCMInterface::SetSteamID( [U:1:0] )
[2026-09-02 00:30:55] [Logged Off, 0, 0] [U:1:82105993] CCMInterface::SetSteamID( [U:1:82105993] )
[2026-09-02 00:30:55] [Logged Off, 0, 0] [U:1:82105993] LogOff()
[2026-09-02 00:30:56] IPv6 UDP connectivity test (ipv6check-udp.steamserver.net) - TIMEOUT";

        assert_eq!(logged_on_in(OFFLINE_RUN), None, "it is not on Steam");
        assert_eq!(acting_for_in(OFFLINE_RUN), Some(82105993));

        // Before it knows which account it is. Zero is the client itself, not
        // somebody with account id nought, and reading it as an account would
        // make a client that had only just started look like one that was ready.
        const JUST_STARTED: &str = "\
[2026-09-02 00:30:54] Client version: 1785799196
[2026-09-02 00:30:54] [Logged Off, 0, 0] [U:1:0] CCMInterface::SetSteamID( [U:1:0] )";
        assert_eq!(acting_for_in(JUST_STARTED), None);

        // And the run before this one says nothing about this one: the log is
        // appended to across restarts, so an account that was on Steam an hour
        // ago is still the last `Logged On` in the file.
        let two_runs = format!(
            "[2026-09-01 22:00:00] Client version: 1785799196\n\
             [2026-09-01 22:00:01] [Logged On, 4, 7] [U:1:99] hello\n\
             {OFFLINE_RUN}"
        );
        assert_eq!(logged_on_in(&two_runs), None);
        assert_eq!(acting_for_in(&two_runs), Some(82105993));

        // And the run that is over says nothing about the client that has just
        // been started in its place. Measured, not reasoned about: a cold start
        // made while writing this was called ready one second in, off the
        // previous client's Offline Mode, because that client's last stamp was
        // still the last one in the file. These are the lines it read.
        let just_gone = format!(
            "{OFFLINE_RUN}\n\
             [2026-09-02 00:30:21] [Logged Off, 0, 0] [U:1:82105993] Log session ended"
        );
        assert_eq!(
            acting_for_in(&just_gone),
            None,
            "a client that has closed its log is not a client that is up"
        );
    }

    /// A scratch directory of this test's own, removed first so a run that
    /// died halfway leaves nothing behind for the next one to read.
    fn scratch(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("lxb-client-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("a scratch directory");
        path
    }

    /// The bug this is here for: `flatpak uninstall` keeps the application's
    /// data, so the directory the shell used to look for outlives the
    /// application by design. A machine that had Steam and removed it looked
    /// exactly like one that has it, and every row that needs a client was
    /// offered against a `flatpak run` that could only fail — after three
    /// minutes of loading screen, because nothing reads the exit status of a
    /// process that spawned successfully.
    ///
    /// What is asked now is what Flatpak itself removes.
    #[test]
    fn a_removed_flatpak_is_not_a_deployed_one() {
        let scratch = scratch("flatpak");
        let installation = scratch.join("var-lib-flatpak");
        let leftovers = scratch
            .join("home")
            .join(".var")
            .join("app")
            .join(FLATPAK_APP);
        // Somebody's saves and configuration, which is the whole reason
        // Flatpak leaves them: the application is gone and these are not.
        std::fs::create_dir_all(&leftovers).expect("a scratch directory");
        assert!(leftovers.is_dir(), "the data outlives the application");
        assert!(!deployed_in(
            std::slice::from_ref(&installation),
            FLATPAK_APP
        ));

        // And with a commit deployed, which is what an installed one has.
        std::fs::create_dir_all(installation.join("app").join(FLATPAK_APP).join("current"))
            .expect("a scratch directory");
        std::fs::write(
            installation
                .join("app")
                .join(FLATPAK_APP)
                .join("current")
                .join("active"),
            b"",
        )
        .expect("a scratch file");
        assert!(deployed_in(&[installation], FLATPAK_APP));
    }

    /// Every installation Flatpak would look in is looked in, because a Steam
    /// deployed into one of them is a Steam `flatpak run` finds — and so is one
    /// this must not call missing.
    #[test]
    fn a_flatpak_in_any_installation_counts() {
        let scratch = scratch("installations");
        let elsewhere = scratch.join("srv").join("flatpak");
        std::fs::create_dir_all(elsewhere.join("app").join(FLATPAK_APP).join("current"))
            .expect("a scratch directory");
        std::fs::write(
            elsewhere
                .join("app")
                .join(FLATPAK_APP)
                .join("current")
                .join("active"),
            b"",
        )
        .expect("a scratch file");

        assert!(!deployed_in(
            &[scratch.join("var-lib-flatpak")],
            FLATPAK_APP
        ));
        assert!(deployed_in(
            &[scratch.join("var-lib-flatpak"), elsewhere.clone()],
            FLATPAK_APP
        ));

        // Which is where the definitions come from.
        assert_eq!(
            installations_in(&format!(
                "[Installation \"extra\"]\nPath={}\nDisplayName=Extra\n",
                elsewhere.display()
            )),
            vec![elsewhere]
        );
        assert!(installations_in("[Installation \"broken\"]\nPath=\n").is_empty());
        assert!(installations_in("").is_empty());
    }

    /// The two clients keep their directories in two different places, and a
    /// machine may hold the leavings of both. Asking the disk which of them
    /// exists — which is what this used to do — pairs whichever is found first
    /// with whichever client was found, and for the Flatpak that meant looking
    /// for its pipe in the real home, where it never is.
    #[test]
    fn each_client_is_asked_for_its_own_directories() {
        let scratch = scratch("directories");
        let native = Where::Native(PathBuf::from("/usr/bin/steam"));

        // Nothing on the disk at all: the answer is where each *will* put
        // itself, so a client that has never been run can be started rather
        // than reported missing.
        let fresh = Options::in_home(&native, &scratch);
        assert_eq!(
            fresh.root,
            scratch.join(".local").join("share").join("Steam")
        );
        assert_eq!(fresh.home, scratch.join(".steam"));
        assert!(!crate::library::looks_like_a_root(&fresh.root));

        let sandbox = scratch.join(".var").join("app").join(FLATPAK_APP);
        let flatpak = Options::in_home(&Where::Flatpak, &scratch);
        assert_eq!(
            flatpak.root,
            sandbox.join(".local").join("share").join("Steam")
        );
        assert_eq!(flatpak.home, sandbox.join(".steam"));

        // With a Flatpak's library on the disk and no native one, the native
        // client still gets the native paths: the two answers never cross.
        std::fs::create_dir_all(flatpak.root.join("steamapps")).expect("a scratch directory");
        assert_eq!(Options::in_home(&native, &scratch).root, fresh.root);
        assert!(crate::library::looks_like_a_root(&flatpak.root));

        // And the link the native client maintains wins over the directory it
        // unpacks into, since that link follows a Steam that has been moved.
        let moved = scratch.join(".steam").join("steam");
        std::fs::create_dir_all(moved.join("steamapps")).expect("a scratch directory");
        assert_eq!(Options::in_home(&native, &scratch).root, moved);
    }

    /// A client that has never been run has no directory to be told anything
    /// in, and the marker has to be down *before* it starts — the client reads
    /// it as it comes up and never looks again. So the first start makes the
    /// directory the client would have made itself, which is the whole of what
    /// this shell does about a first run.
    ///
    /// Before this, a machine with Steam installed and never started reported
    /// that Steam was not installed, and every path that could have started it
    /// was the path that said so.
    #[test]
    fn a_first_start_makes_the_directory_it_needs() {
        // Held because `expose` writes a note beside the marker saying whose it
        // is, and where that note goes is `$XDG_STATE_HOME` — which the tests
        // in `webui` point at scratch directories of their own. Without this
        // the two write into each other: measured on this machine, about one
        // run of `cargo test -p lxb-steam` in twelve failed in one of those
        // tests, never in this one, which is what makes it worth writing down
        // here rather than there.
        let _turn = crate::one_at_a_time_with_the_environment();
        let scratch = scratch("first-run");
        let options = Options::in_home(&Where::Native(PathBuf::from("/usr/bin/steam")), &scratch);
        assert!(!options.root.exists(), "nothing has been run here");

        assert_eq!(expose(&options), Ok(true), "the marker had to be made");
        assert!(options.root.is_dir(), "and the directory to put it in");
        assert!(crate::webui::available(&options.root));

        // Made, and no more than made: an empty directory is not a Steam, so a
        // second press is still the first run and still starts the client.
        assert!(!crate::library::looks_like_a_root(&options.root));

        // And it is given back, so a Steam somebody starts for themselves is
        // not left exposing a debugging port this shell asked for.
        crate::webui::withdraw(&options.root);
        assert!(!crate::webui::available(&options.root));
        assert_eq!(expose(&options), Ok(true), "and can be made again");
    }

    /// Signing out has to reach both, because it is about what is left on the
    /// disk rather than about a client that is running: the registry that
    /// would sign the client straight back in outlives the client, so a
    /// machine whose Steam has been removed — and reinstalled a year later —
    /// must not come up signed in to an account this shell has since said
    /// nobody is signed in to.
    ///
    /// It reached one of them before, whichever the disk happened to answer
    /// with, which for a Flatpak user was the native registry it does not use.
    #[test]
    fn signing_out_clears_the_automatic_sign_in_of_both_layouts() {
        // Clearing the automatic sign-in writes the account list, which is
        // taken a turn at — and a turn lives under `XDG_STATE_HOME`. Without
        // both of these the lock would go wherever the test running beside this
        // one had just pointed that variable, which on a fresh process is the
        // real state directory belonging to whoever is running the suite.
        let _environment = crate::one_at_a_time_with_the_environment();
        let scratch = scratch("layouts");
        let was_state = std::env::var_os("XDG_STATE_HOME");
        // SAFETY: under the environment mutex, which every test that moves one
        // of these holds.
        unsafe { std::env::set_var("XDG_STATE_HOME", scratch.join("state")) };
        let layouts = Options::every_layout_in(&scratch);
        let homes: Vec<&PathBuf> = layouts.iter().map(|options| &options.home).collect();

        assert_eq!(homes.len(), 2, "a native client's and the Flatpak's");
        assert!(homes.contains(&&scratch.join(".steam")));
        assert!(homes.contains(
            &&scratch
                .join(".var")
                .join("app")
                .join(FLATPAK_APP)
                .join(".steam")
        ));

        // Every one of them is cleared, and clearing a layout no client has
        // ever used writes nothing and says nothing.
        for options in &layouts {
            account::sign_out(&options.root, &options.home, "someone");
            assert!(!options.home.join("registry.vdf").exists());
        }

        // SAFETY: still under the environment mutex taken at the top.
        unsafe {
            match was_state {
                Some(was) => std::env::set_var("XDG_STATE_HOME", was),
                None => std::env::remove_var("XDG_STATE_HOME"),
            }
        }
    }

    /// A session with no Xwayland sets no `DISPLAY`, and that is not the same
    /// thing as one whose `DISPLAY` is empty — the direction that matters,
    /// because treating them as equal adopts a client drawing elsewhere and
    /// treating an absent one as empty stops a Steam that was fine.
    #[test]
    fn an_absent_display_is_not_an_empty_one() {
        assert_eq!(
            displays_in(b"WAYLAND_DISPLAY=lxb-0\0"),
            vec![Some("lxb-0".to_string()), None]
        );
        assert_ne!(
            displays_in(b"WAYLAND_DISPLAY=lxb-0\0"),
            displays_in(b"WAYLAND_DISPLAY=lxb-0\0DISPLAY=\0")
        );
    }

    /// Nothing running holds the pipe of a home directory that does not exist,
    /// so there is nobody to be foreign — and the shell says so rather than
    /// guessing, because the guess in the other direction stops somebody's
    /// Steam.
    #[test]
    fn an_unfindable_client_is_left_alone() {
        let nowhere = Options {
            root: PathBuf::from("/nonexistent"),
            home: PathBuf::from("/nonexistent"),
        };
        assert_eq!(holder_of(&nowhere.home.join("steam.pipe")), None);
        assert!(in_this_session(&nowhere));
    }
}
