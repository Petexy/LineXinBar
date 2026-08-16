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

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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
        if let Some(path) = on_path("steam") {
            return Some(Where::Native(path));
        }
        // The Flatpak, checked by its data directory rather than by asking
        // `flatpak list`, which is a process to start and half a second to
        // wait for on every session.
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let flatpak = home.map(|home| {
            home.join(".var")
                .join("app")
                .join("com.valvesoftware.Steam")
        });
        if flatpak.is_some_and(|path| path.is_dir()) && on_path("flatpak").is_some() {
            return Some(Where::Flatpak);
        }
        None
    }

    /// A command that runs the client with no arguments yet.
    fn command(&self) -> Command {
        match self {
            Where::Native(path) => Command::new(path),
            Where::Flatpak => {
                let mut command = Command::new("flatpak");
                command.arg("run").arg("com.valvesoftware.Steam");
                command
            }
        }
    }
}

/// What the background client is doing.
///
/// Deliberately four states and not a bag of booleans: every caller wants to
/// know one of these four things, and a caller that had to work out "up but
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
}

impl State {
    pub fn running(self) -> bool {
        matches!(self, State::Starting | State::SignedIn(_))
    }

    pub fn signed_in(self) -> bool {
        matches!(self, State::SignedIn(_))
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
    /// Bring up the client itself.
    ///
    /// Not about one title at all, which is why it ignores the id it is given.
    /// It is here rather than as a function of its own because it is the same
    /// journey as every other row of that menu.
    Open,
}

impl Doing {
    /// What the row that does this is called.
    pub fn label(self) -> &'static str {
        match self {
            Doing::Install => "Install with Steam",
            Doing::Verify => "Verify with Steam",
            Doing::Open => "Open Steam",
        }
    }

    /// The URL the client answers for it.
    pub fn url(self, app_id: u32) -> String {
        match self {
            Doing::Install => format!("steam://install/{app_id}"),
            Doing::Verify => format!("steam://validate/{app_id}"),
            // `open/main` rather than starting the program with no arguments,
            // so a client that is already running comes to the front instead
            // of a second process starting and immediately exiting.
            Doing::Open => "steam://open/main".to_string(),
        }
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
    /// Where this machine keeps them.
    pub fn found() -> Option<Options> {
        let home = std::env::var_os("HOME").map(PathBuf::from)?;
        Some(Options {
            root: crate::library::root()?,
            home: home.join(".steam"),
        })
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

/// Start the client, quietly, and do not wait for it.
///
/// The child is deliberately dropped: Valve's client daemonises itself within a
/// second or two and the process this starts is a launcher that exits, so there
/// is nothing here worth keeping to wait on. What the client is doing is read
/// from [`state`] instead, which works the same whether this session started it
/// or it was already running when the shell came up.
pub fn start(client: &Where, options: &Options) -> std::io::Result<()> {
    tracing::info!(root = %options.root.display(), "starting Valve's client in the background");
    client
        .command()
        .args(QUIETLY)
        // Its output belongs in its own logs, which is where it already goes.
        // Inherited pipes would fill and stall it the moment nothing read them.
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(drop)
}

/// Hand the running client one `steam:` URL — which is how it is asked to do
/// anything at all once it is up.
///
/// A second `steam` process started this way notices the first one through its
/// pipe, hands the URL over and exits, which is why this waits for it: the wait
/// is milliseconds and its exit status is the only sign that the client took
/// the request.
pub fn tell(client: &Where, url: &str) -> std::io::Result<()> {
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

/// Ask the client to shut down.
///
/// Only ever called for a client this session started, and only when the
/// session ends: a user who had Steam running before the shell came up wants it
/// still running after.
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
const UNTIL_IT_STOPS: Duration = Duration::from_secs(30);

/// How long to wait for a client that has just been started to come up far
/// enough to be spoken to. A cold client unpacks an update, starts a browser
/// and reaches the network before it will answer anything.
const UNTIL_IT_ANSWERS: Duration = Duration::from_secs(90);

/// And how long to wait, after it has been given a credential, for Steam to
/// agree. This is a round trip to Valve plus whatever the client does with the
/// answer; it is the wait a user is watching a loading screen through.
const UNTIL_IT_SIGNS_IN: Duration = Duration::from_secs(60);

/// What a caller needs of Valve's client.
///
/// The distinction earns its place because one of these needs the client's JS
/// context and the other does not, and that context is the whole of what is
/// worth being careful about here — see [`crate::webui::expose`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    /// Up and signed in. Every game press: starting a title is a `steam:` URL
    /// handed over the client's pipe, and touches no interface at all.
    SignedIn,
    /// Up, signed in, and answering on its JS context. Moving a game on or off
    /// the disk needs this, because those are made as calls into the client
    /// rather than as URLs — a URL for either of them raises a window.
    Context,
}

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
/// That is somebody's Steam being taken away, so it happens on evidence and
/// never on a guess — and it is the answer the alternative deserves, which is
/// a shell that says a game did not start while the game is running on a screen
/// nobody is looking at.
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
/// Blocking, and for as long as a minute and a half. Never call it on a thread
/// that draws — see [`crate::Steam::wake_client`], which is how the shell asks.
pub fn wake(
    client: &Where,
    options: &Options,
    account: &str,
    refresh_token: &str,
    need: Need,
) -> Result<(), String> {
    // Asked before anything else is, because it is the one thing a client can
    // be wrong about while looking perfectly right: up, signed in, meeting
    // every `need` there is, and attached to somebody else's display. See
    // `in_this_session`.
    let elsewhere = state(Some(client), options).running() && !in_this_session(options);

    if !elsewhere && met(need, client, options) {
        return Ok(());
    }

    let mut ours = false;
    let mut fresh = false;
    // Kept rather than returned on, so that a start which fails still reaches
    // the withdrawal below. Restarting a client that will not let go of its
    // pipe is the likeliest way for this to fail and it fails *after* the
    // marker is up, which without this would leave the next Steam somebody
    // starts for themselves exposing a debugging port nobody asked for.
    let mut up = Ok(());
    if elsewhere {
        // Started again rather than left alone, because there is nothing else
        // that would work: where a client draws is fixed when it starts, and
        // no URL handed to it afterwards can move it. The cost is real and
        // falls on the other session — its Steam goes, and any download with
        // it — and it is still the better half of the trade, because the other
        // half is a shell whose games silently open on a screen the person
        // pressing the button is not looking at.
        tracing::info!("Valve's client belongs to another session; starting it again in this one");
        ours = expose(options)?;
        fresh = true;
        up = restart(client, options);
    } else if !state(Some(client), options).running() {
        // Before it is started, not after: the client tests for this file as
        // it comes up and never looks again, so the order here is the whole of
        // why it works.
        ours = expose(options)?;
        fresh = true;
        up = start(client, options).map_err(|error| format!("Steam would not start: {error}"));
    }

    let woken = up.and_then(|()| {
        bring_up(
            client,
            options,
            account,
            refresh_token,
            need,
            fresh,
            &mut ours,
        )
    });

    // Whatever happened, the marker is spent: it is read as the client starts
    // and never again, so by now it has either done its work or is not going
    // to. Only what this call created is taken back — a marker somebody else
    // put there is theirs.
    if ours {
        crate::webui::withdraw(&options.root);
    }
    woken
}

/// Tell the client to expose its interface, saying whether the marker had to be
/// made, and putting Valve's own wording on a Steam directory that will not
/// take one.
fn expose(options: &Options) -> Result<bool, String> {
    crate::webui::expose(&options.root)
        .map_err(|error| format!("{}: {error}", crate::webui::Problem::NotExposed))
}

/// From a client that is running to one that is everything `need` asks for.
///
/// `fresh` says this call started it, which decides how long to wait — a cold
/// client has a browser to start and a network to reach before it will answer
/// anything, where one that was already up has had its chance.
fn bring_up(
    client: &Where,
    options: &Options,
    account: &str,
    refresh_token: &str,
    need: Need,
    fresh: bool,
    ours: &mut bool,
) -> Result<(), String> {
    // Most sessions end here. The client keeps its own credential and comes
    // back up signed in by itself, so all this does is watch it happen.
    //
    // Waiting is only worth it for what waiting can fix. Signing itself in is
    // one; opening its port is one *for a client of ours*, which was told to
    // before it was started. A client that was already running when this
    // session found it will never open a port it was never told to open, and
    // watching one for thirty seconds to establish that is thirty seconds of
    // somebody's loading screen.
    let worth_waiting = fresh || need == Need::SignedIn || crate::webui::reachable();
    if worth_waiting {
        let patience = if fresh {
            UNTIL_IT_ANSWERS
        } else {
            UNTIL_IT_SIGNS_ITSELF_IN
        };
        if settles(need, client, options, patience) {
            return Ok(());
        }
    }

    // It will not get there on its own, and everything past this point is said
    // through the interface. A client of ours is already exposing it; one that
    // was up before this session is not, and nothing short of starting it
    // again will change that.
    if !crate::webui::reachable() {
        *ours |= expose(options)?;
        tracing::info!("restarting Valve's client, which came up before it was told to expose it");
        restart(client, options)?;
        if settles(need, client, options, UNTIL_IT_ANSWERS) {
            return Ok(());
        }
    }

    sign_it_in(client, options, account, refresh_token)
}

/// Whether the client is already everything `need` asks for.
fn met(need: Need, client: &Where, options: &Options) -> bool {
    if !state(Some(client), options).signed_in() {
        return false;
    }
    match need {
        Need::SignedIn => true,
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
fn settles(need: Need, client: &Where, options: &Options, patience: Duration) -> bool {
    let deadline = Instant::now() + patience;
    while Instant::now() < deadline {
        if met(need, client, options) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    false
}

/// Hand a running, exposed client this session's credential, and wait for
/// Steam to agree.
///
/// Reached only by a client that would not sign itself in — the first time, or
/// after the token it kept has expired.
fn sign_it_in(
    client: &Where,
    options: &Options,
    account: &str,
    refresh_token: &str,
) -> Result<(), String> {
    // Wait for it to be able to answer at all. Its own JS context is the thing
    // that has to be there, and it is the last of the client to come up — so
    // waiting for that is waiting for all of it.
    let deadline = Instant::now() + UNTIL_IT_ANSWERS;
    loop {
        if state(Some(client), options).signed_in() && crate::webui::reachable() {
            return Ok(());
        }
        match crate::webui::sign_in(account, refresh_token) {
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
    let deadline = Instant::now() + UNTIL_IT_SIGNS_IN;
    while Instant::now() < deadline {
        if state(Some(client), options).signed_in() {
            tracing::info!(account, "Valve's client is signed in and out of sight");
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    Err("Steam took the credential but did not sign in with it.".to_string())
}

/// What the client is doing, read from the disk.
///
/// Cheap enough to poll: a small file, a directory entry, and the tail of a
/// log. Nothing here starts a process or touches the network.
pub fn state(client: Option<&Where>, options: &Options) -> State {
    if client.is_none() {
        return State::Absent;
    }
    if !running(&options.home) {
        return State::Stopped;
    }
    match logged_on_as(&options.root) {
        Some(account_id) => State::SignedIn(account_id),
        None => State::Starting,
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
fn running(home: &Path) -> bool {
    use std::os::unix::fs::OpenOptionsExt;

    // Without O_NONBLOCK this would block until a reader arrived, which on a
    // machine with no client running is for ever.
    std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(home.join("steam.pipe"))
        .is_ok()
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
const DRAWS_INTO: [&str; 2] = ["WAYLAND_DISPLAY", "DISPLAY"];

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
fn logged_on_as(root: &Path) -> Option<u32> {
    const TAIL: u64 = 64 * 1024;

    let path = root.join("logs").join("connection_log.txt");
    let mut file = std::fs::File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(length.saturating_sub(TAIL)))
        .ok()?;
    // Read bytes and not a string: the seek lands wherever it lands, which may
    // be the middle of a character, and a log that happens to hold one is not
    // a client that is signed out.
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;

    logged_on_in(&String::from_utf8_lossy(&bytes))
}

/// What the client writes as the first line of every run.
const NEW_RUN: &str = "Client version:";

/// The state a logged-on client stamps its lines with.
const LOGGED_ON: &str = "Logged On";

/// Who the client is acting for now, out of a stretch of its log.
fn logged_on_in(log: &str) -> Option<u32> {
    let this_run = match log.rfind(NEW_RUN) {
        Some(at) => &log[at..],
        // No start marker in the tail. Either the client has been up long
        // enough to write 64 KB since, in which case all of this is its own,
        // or there is no log to speak of — and both are answered by reading
        // what is here.
        None => log,
    };
    match this_run.lines().filter_map(stamped).next_back() {
        Some((LOGGED_ON, account)) if account != 0 => Some(account),
        _ => None,
    }
}

/// The state and account one log line is stamped with, for the lines that
/// carry both.
///
/// The state block is `[<state>, <n>, <n>]`, and the two numbers are what tell
/// it from the timestamp block that precedes it — a line stamped with an
/// account but no state says nothing about whether the client is signed in and
/// must not be read as though it did.
fn stamped(line: &str) -> Option<(&str, u32)> {
    let at = line.find("[U:1:")?;
    let account = line[at + "[U:1:".len()..]
        .split_once(']')
        .and_then(|(id, _)| id.parse::<u32>().ok())?;

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
    Some((state, account))
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
pub mod autologin {
    use std::path::Path;

    use crate::vdf::{self, Node};

    /// Where the account name goes in the client's Linux registry — its
    /// stand-in for the Windows registry key of the same name.
    const AUTO_LOGIN_USER: [&str; 6] = [
        "Registry",
        "HKCU",
        "Software",
        "Valve",
        "Steam",
        "AutoLoginUser",
    ];

    /// Stop the client signing itself in as this account.
    ///
    /// Never fails outwards: this runs while the user is signing out, the
    /// sign-out itself has already happened, and there is nothing useful to
    /// say to somebody about a file they have never heard of. Whatever could
    /// be cleared is cleared.
    pub fn stop(root: &Path, home: &Path, account: &str) {
        let registry = home.join("registry.vdf");
        if let Some(mut node) = read(&registry) {
            if node.set(&AUTO_LOGIN_USER, "") {
                write(&registry, &vdf::text(&node));
            }
        }

        // The account keeps its place in the client's own list — the user may
        // sign in as it from the client later, and emptying that list would be
        // throwing away something the shell was never asked about. It only
        // stops being the one that signs itself in.
        let users = root.join("config").join("loginusers.vdf");
        if let Some(mut node) = read(&users) {
            let mut touched = false;
            if let Some(block) = node.make(&["users"]) {
                for user in block.values_mut() {
                    if user
                        .string(&["AccountName"])
                        .is_some_and(|name| name.eq_ignore_ascii_case(account))
                    {
                        user.set(&["AllowAutoLogin"], "0");
                        user.set(&["MostRecent"], "0");
                        touched = true;
                    }
                }
            }
            if touched {
                write(&users, &vdf::text(&node));
            }
        }
        tracing::info!(account, "Valve's client will not sign itself back in");
    }

    fn read(path: &Path) -> Option<Node> {
        Some(vdf::parse(&std::fs::read_to_string(path).ok()?))
    }

    /// Beside and rename over, so a client reading the file while this happens
    /// sees either all of the old one or all of the new one.
    fn write(path: &Path, text: &str) {
        let scratch = path.with_extension("lxb-writing");
        if std::fs::write(&scratch, text).is_ok() {
            let _ = std::fs::rename(&scratch, path);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// After this, the client comes up asking rather than signing itself
        /// in — and somebody else's account is still in its list.
        #[test]
        fn it_clears_only_this_accounts_automatic_sign_in() {
            let root = std::env::temp_dir().join(format!("lxb-autologin-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("config")).unwrap();
            std::fs::write(
                root.join("registry.vdf"),
                "\"Registry\"\n{\n\t\"HKCU\"\n\t{\n\t\t\"Software\"\n\t\t{\n\t\t\t\"Valve\"\n\t\t\t{\n\t\t\t\t\"Steam\"\n\t\t\t\t{\n\t\t\t\t\t\"AutoLoginUser\"\t\t\"someone\"\n\t\t\t\t}\n\t\t\t}\n\t\t}\n\t}\n}\n",
            )
            .unwrap();
            std::fs::write(
                root.join("config").join("loginusers.vdf"),
                "\"users\"\n{\n\t\"1\"\n\t{\n\t\t\"AccountName\"\t\t\"someone\"\n\t\t\"AllowAutoLogin\"\t\t\"1\"\n\t}\n\t\"2\"\n\t{\n\t\t\"AccountName\"\t\t\"somebody\"\n\t\t\"AllowAutoLogin\"\t\t\"1\"\n\t}\n}\n",
            )
            .unwrap();

            stop(&root, &root, "someone");

            let registry = vdf::parse(&std::fs::read_to_string(root.join("registry.vdf")).unwrap());
            assert_eq!(registry.string(&AUTO_LOGIN_USER), Some(""));

            let users = vdf::parse(
                &std::fs::read_to_string(root.join("config").join("loginusers.vdf")).unwrap(),
            );
            assert_eq!(users.string(&["users", "1", "AllowAutoLogin"]), Some("0"));
            assert_eq!(
                users.string(&["users", "2", "AllowAutoLogin"]),
                Some("1"),
                "somebody else's account was changed"
            );

            let _ = std::fs::remove_dir_all(&root);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            Some(("Logged On", 7))
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
