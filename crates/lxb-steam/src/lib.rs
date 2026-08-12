//! Steam for a shell: one account, one library, and Valve's client kept out of
//! sight.
//!
//! Enough of Steam to sign a person in, list what they own, say which of it is
//! on the disk — and then, for the two things that actually move a game,
//! Valve's own client, run as a background process with no window.
//!
//! The split is deliberate and is drawn where it is for a reason. Signing in,
//! the licence list and the catalogue are a quiet Connection Manager session
//! this crate holds itself: they are a network round trip and a parse, and
//! doing them here is what lets the bar be built before any client has started.
//! Playing and installing are not like that. They want the Steam Linux
//! Runtime, the Proton a player chose, the prefix a game already has its saves
//! in, the overlay, the anti-cheat and a real `steamclient.so` to talk to —
//! and the only thing that gets all of those right is the client.
//!
//! ```ignore
//! let mut steam = Steam::start();
//! steam.sign_in_with_qr();
//! loop {
//!     for event in steam.take() {
//!         match event {
//!             Event::Challenge { code, .. } => draw(code),
//!             Event::SignedIn(account) => greet(account),
//!             Event::Library(games) => hang(games),
//!             _ => {}
//!         }
//!     }
//! }
//! ```
//!
//! ## Shape
//!
//! One worker thread, two channels, and a handle the shell holds — the same
//! shape as the walk over the user's own files, and for the same reason.
//! Everything Steam does is a network round trip taking anything from a
//! millisecond to thirty seconds, and none of it may happen on the thread that
//! draws. So the handle only ever puts an [`Ask`] on a queue and drains
//! whatever [`Event`]s have arrived; on an ordinary frame that is one failed
//! `try_recv` and nothing else.
//!
//! Anything that needs Valve's client gets a thread of its own on top of that,
//! because bringing a cold client up is the one thing here that can take a
//! minute and a half — see [`in_the_background`], and [`ClientReport`], which
//! is what the shell draws a loading screen from meanwhile.
//!
//! ## Where the halves live
//!
//! | | |
//! |---|---|
//! | [`auth`]     | signing in, both ways round, and the CM credential it grants |
//! | [`library`]  | what the account owns, and what of it is on this disk |
//! | [`art`]      | the cover and the picture behind a game, from Valve's cache or from Steam |
//! | [`client`]   | Valve's client as a background process: where it is, whether it is up, whether it has signed in |
//! | [`webui`]    | the calls this crate makes into the client's own interface, and why they are made there |
//! | [`session`]  | what survives a reboot, and what must never be written down |
//! | [`protobuf`] | the wire format Steam's services speak, written out by hand |
//! | [`vdf`]      | Valve's key-values, which is what the disk answers in |
//! | [`rsa`]      | encrypting the password under the account's own key |
//! | [`qr`]       | the code on the screen, as squares for the shell to draw |

mod base64;
mod cm;
mod password;
mod protobuf;
mod rsa;
mod session;
mod vdf;
mod web;

pub mod art;
pub mod auth;
pub mod client;
pub mod library;
pub mod qr;
pub mod webui;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub use auth::Confirmation;
pub use client::Doing;
pub use library::Game;
pub use password::Password;
pub use web::Failed;

/// How long between one look at Steam for the owned list and the next.
///
/// Long, because it changes when somebody buys something, and a shell that
/// asked more often would be asking Steam a question whose answer it already
/// has. Anything the user does themselves — signing in, installing, coming
/// back from the store — brings the next one forward.
const OWNED_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// And between one look at the disk and the next.
///
/// Short, because this is the half that changes while somebody is watching: a
/// download finishing is a row moving from the bottom of the column to the
/// top. It costs a directory listing per Steam library and nothing else — no
/// network, and no work proportional to the size of the library.
const INSTALLED_INTERVAL: Duration = Duration::from_secs(10);

/// Held by any test that moves an environment variable.
///
/// The environment belongs to the whole process and `cargo test` runs its
/// tests on threads of one. Two tests that both point `XDG_DATA_HOME` at their
/// own scratch directory will, once in a while, each read the other's — which
/// arrives as one of them failing for no reason anybody can reproduce. This is
/// what makes them take turns.
#[cfg(test)]
pub(crate) fn one_at_a_time_with_the_environment() -> std::sync::MutexGuard<'static, ()> {
    static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());
    // A test that failed while holding it has already reported why; the next
    // one wants the lock rather than a second failure about the lock.
    TURN.lock().unwrap_or_else(|held| held.into_inner())
}

/// The account that is signed in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// What the user signs in with, which is what the shell greets them by.
    pub name: String,
    pub steam_id: u64,
}

/// What the shell asks of Steam.
#[derive(Debug)]
pub enum Ask {
    /// Begin a sign-in to be confirmed by photographing a code.
    SignInWithQr,
    /// Begin one with an account name and a password.
    SignInWithPassword { account: String, password: Password },
    /// Hand over the Steam Guard code that was asked for.
    SubmitCode(String),
    /// Stop waiting for a sign-in that is under way.
    CancelSignIn,
    /// Give up the stored session.
    SignOut,
    /// Ask Steam for the library again, now, rather than at the next interval.
    Refresh,
    /// Have Valve's client fetch one game.
    Install { app_id: u32 },
    /// Stop fetching one, and take away what had arrived.
    StopInstalling { app_id: u32 },
    /// Have Valve's client take one game off the disk, keeping it in the
    /// library.
    Uninstall { app_id: u32 },
    /// Have Valve's client running and signed in, because something is about
    /// to need it. Does nothing if it already is.
    WakeClient,
}

/// What Valve's background client is doing, as the shell needs to know it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientReport {
    /// It is being started and signed in. The shell has something to wait on
    /// and something to say while it waits.
    Waking,
    /// It is up and signed in. Anything Steam-backed can go ahead.
    Ready,
    /// It cannot be, and this is what to tell the user. Never a silent
    /// failure: a press that needed the client and did not get it has to say
    /// so, because the alternative is a loading screen that never ends.
    Unavailable(String),
}

/// Why a game did not come down.
///
/// Two shapes rather than one string, because the shell answers them
/// differently and a caller that had to read the wording to tell them apart
/// would be a caller that gets it wrong the day the wording changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stopped {
    /// Something went wrong, and this is what to say about it.
    Failed(String),
    /// Nothing went wrong: Steam will not go on without the person, and what
    /// it is waiting for is here. An agreement to accept, most often.
    ///
    /// Answered by offering [`Doing::Install`], which hands the whole install
    /// to Steam's own window — the only place the question can be asked.
    Asks(String),
}

impl Stopped {
    /// What to put in front of somebody.
    pub fn said(&self) -> &str {
        match self {
            Stopped::Failed(why) => why,
            Stopped::Asks(why) => why,
        }
    }
}

/// What Steam says back.
#[derive(Debug, Clone)]
pub enum Event {
    /// How a download is going, read off the manifest Valve's client keeps for
    /// the game. `total` is zero for the short stretch at the start where the
    /// client has written the manifest but does not yet know the size.
    ///
    /// Arrives once when the press is taken and then at every look at the
    /// disk until it is done, so a row counting up is a row saying what the
    /// client is actually doing rather than what it was asked to do.
    Installing { app_id: u32, done: u64, total: u64 },
    /// It finished, and the game is at this path.
    Installed { app_id: u32, into: PathBuf },
    /// It did not, and this is why. Whatever had been written is gone again,
    /// unless it was written over a game that was already there.
    InstallFailed { app_id: u32, why: Stopped },
    /// Somebody stopped it. Nothing is left on the disk and nothing went
    /// wrong, so this is not a failure and there is nothing to tell anybody.
    InstallStopped { app_id: u32 },
    /// One game is being taken off the disk. The row has something to say
    /// while the client works through it.
    Uninstalling { app_id: u32 },
    /// It has gone: its manifest is no longer on the disk, which is the only
    /// thing that can say so.
    Uninstalled { app_id: u32 },
    /// It has not, and this is what to tell the user.
    UninstallFailed { app_id: u32, why: String },
    /// A code to photograph, and the URL it stands for.
    ///
    /// Arrives again whenever Steam rotates the code, which it does every
    /// twenty seconds or so: the panel redraws around the new one, and the old
    /// one stops working. That is Steam's rule and not this crate's.
    Challenge { code: qr::Code, url: String },
    /// A sign-in that cannot go further until the user types a code.
    CodeWanted(Confirmation),
    /// One that needs something done elsewhere — a press on the phone — and
    /// nothing typed here. The string is what to say while waiting.
    Waiting(String),
    /// It worked. Carries the account, and is also what a session restored
    /// from disk at startup arrives as.
    SignedIn(Account),
    /// It did not, and this is what to tell the user.
    SignInFailed(String),
    /// The account is signed in, but its catalogue could not be refreshed.
    /// The last good library and credential remain intact.
    LibraryUnavailable(String),
    /// There is no session: either there never was one, or the user signed
    /// out, or Steam stopped accepting the stored token.
    SignedOut,
    /// How Valve's background client is getting on. Arrives unasked whenever
    /// it changes, so a shell that is waiting on it has something to draw.
    Client(ClientReport),
    /// The library, whole. Always the whole of it rather than a change to it,
    /// for the reason the file shelves are delivered whole: the shell swaps
    /// one list for another in a few microseconds however long it is, and a
    /// list of edits would need both sides to agree about a list they cannot
    /// both see.
    Library(Vec<Game>),
}

/// The shell's handle on all of it.
pub struct Steam {
    asks: Option<Sender<WorkerMessage>>,
    events: Option<Receiver<Event>>,
    /// Where Valve's client is on this machine, if it is anywhere. Read once
    /// here rather than on the worker, because the shell asks it while
    /// building a menu and the answer does not change during a session.
    client_at: Option<client::Where>,
    /// The last delivered catalogue, retained so a Play request can use its
    /// PICS launch metadata without asking the worker on the render thread.
    games: Vec<Game>,
}

impl Steam {
    /// Start the worker, which restores whatever session is on the disk.
    pub fn start() -> Steam {
        let (asks, take_asks) = mpsc::channel();
        let (send_events, events) = mpsc::channel();
        let worker = asks.clone();
        std::thread::spawn(move || work(&take_asks, &worker, &send_events));
        Steam {
            asks: Some(asks),
            events: Some(events),
            client_at: client::Where::find(),
            games: Vec::new(),
        }
    }

    /// One that will never do anything: for a session with Steam turned off,
    /// and for tests.
    pub fn settled() -> Steam {
        Steam {
            asks: None,
            events: None,
            client_at: None,
            games: Vec::new(),
        }
    }

    /// Where Valve's client is, if it is anywhere. `None` is a machine on
    /// which nothing in this integration works, and the shell says so once
    /// rather than failing at every press.
    pub fn client_at(&self) -> Option<&client::Where> {
        self.client_at.as_ref()
    }

    /// The `steam:` URL for one thing to be done to one title, handed to the
    /// client. `None` on a machine with no client.
    pub fn tell(&self, app_id: u32, doing: Doing) -> Result<(), String> {
        let Some(where_it_is) = self.client_at.as_ref() else {
            return Err("There is no Steam client installed on this machine.".to_string());
        };
        client::tell(where_it_is, &doing.url(app_id)).map_err(|error| error.to_string())
    }

    /// Everything the worker has said since the last look.
    ///
    /// The whole of what the shell does with this on an ordinary frame, and on
    /// nearly every frame it is an empty vector and a single failed
    /// `try_recv`.
    pub fn take(&mut self) -> Vec<Event> {
        let mut ready = Vec::new();
        while let Some(events) = self.events.as_ref() {
            match events.try_recv() {
                Ok(event) => {
                    match &event {
                        Event::Library(games) => self.games.clone_from(games),
                        Event::SignedOut => self.games.clear(),
                        _ => {}
                    }
                    ready.push(event);
                }
                Err(TryRecvError::Empty) => break,
                // The worker only stops if it cannot deliver, which it cannot
                // do while this end is held.
                Err(TryRecvError::Disconnected) => {
                    self.events = None;
                    self.asks = None;
                }
            }
        }
        ready
    }

    pub fn sign_in_with_qr(&self) {
        self.ask(Ask::SignInWithQr);
    }

    /// Sign in with an account name and a password.
    ///
    /// The password arrives as [`Password`], which the shell's own field
    /// writes itself into and which cannot be read outside this crate. It is
    /// encrypted under the account's RSA key on the worker and overwritten
    /// there; nothing keeps it and nothing writes it down.
    pub fn sign_in_with_password(&self, account: String, password: Password) {
        self.ask(Ask::SignInWithPassword { account, password });
    }

    pub fn submit_code(&self, code: String) {
        self.ask(Ask::SubmitCode(code));
    }

    pub fn cancel_sign_in(&self) {
        self.ask(Ask::CancelSignIn);
    }

    pub fn sign_out(&self) {
        self.ask(Ask::SignOut);
    }

    /// Have Valve's client fetch one game.
    ///
    /// Answered by [`Event::Installing`] at once so the row can say something,
    /// then by one of those for every look at the disk while it comes down,
    /// and finally by [`Event::Installed`]. The progress is the client's own:
    /// it writes how far it has got into the game's manifest as it goes, which
    /// is where every other fact about an installed game is already read from.
    ///
    /// A game that will not install silently is answered by
    /// [`Event::InstallFailed`] carrying [`Stopped::Asks`] — see [`Doing`].
    ///
    /// Which build comes down is the client's decision and not this shell's.
    /// It is the same decision Steam makes anywhere else — this system, this
    /// account's licences, the depots the game is actually made of — and a
    /// shell that passed its own opinion in would be a second implementation
    /// of it that could only ever be wrong in ways Steam's is not.
    pub fn install(&self, app_id: u32) {
        self.ask(Ask::Install { app_id });
    }

    /// Stop fetching one game, and take away what had arrived.
    ///
    /// Answered by [`Event::InstallStopped`]. Nothing is left claiming the
    /// game is installed and nothing is left of the download either — there is
    /// no resuming one from here, so what had arrived is of no use to a later
    /// attempt and would only be a folder nobody could account for.
    pub fn stop_installing(&self, app_id: u32) {
        self.ask(Ask::StopInstalling { app_id });
    }

    /// Take one game off the disk, keeping it in the library.
    ///
    /// Answered by [`Event::Uninstalling`] at once and by
    /// [`Event::Uninstalled`] when the game's manifest has left the disk.
    ///
    /// Nothing is put on the screen by this: the client is told not to ask,
    /// because the shell has already asked in its own panel. A caller that has
    /// *not* asked must not call this — it is a game being deleted, and the
    /// press that starts it is the only chance anybody gets to say no.
    pub fn uninstall(&self, app_id: u32) {
        self.ask(Ask::Uninstall { app_id });
    }

    /// Ask for the library again now — after something was installed through
    /// Steam's own window, or after the user came back from it.
    pub fn refresh(&self) {
        self.ask(Ask::Refresh);
    }

    /// Have Valve's client running and signed in.
    ///
    /// Answered by [`Event::Client`], once when it starts trying and once when
    /// it knows. Asking twice while it is already waking does nothing, so a
    /// second press cannot start a second client.
    ///
    /// This is the one thing in this crate that can take a minute and a half:
    /// a cold client unpacks its own update, starts a browser and reaches the
    /// network before it will answer anything. It happens on a thread of its
    /// own so that the library, the catalogue and every other answer carry on
    /// arriving while it does.
    pub fn wake_client(&self) {
        self.ask(Ask::WakeClient);
    }

    fn ask(&self, ask: Ask) {
        if let Some(asks) = self.asks.as_ref() {
            let _ = asks.send(WorkerMessage::Ask(ask));
        }
    }
}

impl Drop for Steam {
    fn drop(&mut self) {
        if let Some(worker) = self.asks.as_ref() {
            let _ = worker.send(WorkerMessage::Shutdown);
        }
    }
}

pub(crate) enum WorkerMessage {
    Ask(Ask),
    Cm(cm::Event),
    /// A job that needed Valve's client has come back. Routed through the
    /// worker rather than straight to the shell because the worker is the one
    /// keeping [`Watching`], and a job that failed has to stop being watched
    /// for as well as be reported.
    Done(Finished),
    Shutdown,
}

/// How one background job ended.
pub(crate) enum Finished {
    Installing {
        app_id: u32,
        how: Result<(), Stopped>,
    },
    StoppedInstalling {
        app_id: u32,
        how: Result<(), String>,
    },
    Uninstalling {
        app_id: u32,
        how: Result<(), String>,
    },
}

/// The games this session has asked Valve's client to move on or off the disk,
/// and which are therefore worth looking at the disk about.
///
/// This is the whole of how a row knows what is happening to it. The client
/// does not report progress to anybody — it writes it into the game's own
/// manifest, the same file the library is already read from — so a press is
/// remembered here and every look at the disk turns into something to say
/// about it.
///
/// Only games this session asked for. A download somebody started in Steam
/// itself an hour ago is already on the bar as a game that is updating, said
/// in the library's own words, and does not need a second voice.
#[derive(Default)]
struct Watching {
    fetching: std::collections::BTreeSet<u32>,
    removing: std::collections::BTreeSet<u32>,
}

impl Watching {
    /// Say how far each of them has got, and forget the ones that have
    /// arrived or gone.
    fn report(
        &mut self,
        installed: &std::collections::BTreeMap<u32, library::Installed>,
        events: &Sender<Event>,
    ) {
        self.fetching.retain(|app_id| {
            // Nothing on the disk yet: the client has taken the request and
            // has not begun writing. The press already said as much.
            let Some(on_disk) = installed.get(app_id) else {
                return true;
            };
            if on_disk.updating {
                let _ = events.send(Event::Installing {
                    app_id: *app_id,
                    done: on_disk.downloaded,
                    total: on_disk.to_download,
                });
                return true;
            }
            if on_disk.playable {
                let _ = events.send(Event::Installed {
                    app_id: *app_id,
                    into: on_disk.path.clone(),
                });
                return false;
            }
            // On the disk, not moving, and not playable: a game part way
            // through being repaired or removed. Nothing true to say about it
            // yet, so nothing is said.
            true
        });

        self.removing.retain(|app_id| {
            if installed.contains_key(app_id) {
                return true;
            }
            let _ = events.send(Event::Uninstalled { app_id: *app_id });
            false
        });
    }
}

enum State {
    Out,
    Waiting {
        session: auth::Session,
        next: Instant,
    },
    Connecting {
        stored: session::Stored,
        generation: u64,
        /// True after a previous CM connection was already announced to the
        /// shell. A reconnect must not make the account disappear meanwhile.
        announced: bool,
        /// True for a sign-in panel the user is currently watching.
        failure_to_panel: bool,
        /// The last complete remote catalogue. A reconnect must not replace it
        /// with an empty answer while the new CM session warms up.
        owned: Vec<Game>,
        cancel: cm::Cancel,
    },
    Reconnecting {
        stored: session::Stored,
        next: Instant,
        announced: bool,
        failure_to_panel: bool,
        owned: Vec<Game>,
    },
    In {
        stored: session::Stored,
        account: Account,
        generation: u64,
        commands: tokio::sync::mpsc::UnboundedSender<cm::Command>,
        cancel: cm::Cancel,
        /// Last successful CM/PICS answer, before local manifests are folded in.
        owned: Vec<Game>,
        /// Avoid repeatedly interrupting the shell for one persistent service
        /// failure. An explicit Refresh permits one fresh report.
        library_failure_announced: bool,
        next_owned: Instant,
        next_installed: Instant,
    },
}

fn work(
    messages: &Receiver<WorkerMessage>,
    worker: &Sender<WorkerMessage>,
    events: &Sender<Event>,
) {
    let wire = web::Wire::new();
    let mut generation = 0_u64;
    let mut state = restore(worker, events, &mut generation);
    // One client at a time: a second press while the first is still bringing
    // Steam up must not start a second one.
    let waking = Arc::new(AtomicBool::new(false));
    let mut watching = Watching::default();

    loop {
        let deadline = match &state {
            State::Out | State::Connecting { .. } => None,
            State::Waiting { next, .. } | State::Reconnecting { next, .. } => Some(*next),
            State::In {
                next_owned,
                next_installed,
                ..
            } => Some((*next_owned).min(*next_installed)),
        };
        let wait = deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
            .unwrap_or(Duration::from_secs(3600));

        state = match messages.recv_timeout(wait) {
            Ok(WorkerMessage::Ask(ask)) => {
                answer(ask, state, &wire, worker, events, &waking, &mut watching)
            }
            Ok(WorkerMessage::Cm(event)) => answer_cm(event, state, events),
            Ok(WorkerMessage::Done(finished)) => came_back(finished, state, events, &mut watching),
            // A download does not stop with the shell: it belongs to Valve's
            // client now, which carries on with it whether this session is
            // running or not, and finishing it is what the user asked for.
            Ok(WorkerMessage::Shutdown) => return,
            Err(RecvTimeoutError::Timeout) => {
                advance(state, &wire, worker, events, &mut generation, &mut watching)
            }
            Err(RecvTimeoutError::Disconnected) => return,
        };
    }
}

fn restore(worker: &Sender<WorkerMessage>, events: &Sender<Event>, generation: &mut u64) -> State {
    let Some(stored) = session::Stored::load() else {
        let _ = events.send(Event::SignedOut);
        return State::Out;
    };
    tracing::info!(account = %stored.account, "restoring the stored Steam CM session");
    connect(stored, false, false, Vec::new(), worker, generation)
}

fn connect(
    stored: session::Stored,
    announced: bool,
    failure_to_panel: bool,
    owned: Vec<Game>,
    worker: &Sender<WorkerMessage>,
    generation: &mut u64,
) -> State {
    *generation = generation.wrapping_add(1).max(1);
    let cancel = cm::start(stored.clone(), *generation, worker.clone());
    State::Connecting {
        stored,
        generation: *generation,
        announced,
        failure_to_panel,
        owned,
        cancel,
    }
}

fn answer(
    ask: Ask,
    state: State,
    wire: &web::Wire,
    worker: &Sender<WorkerMessage>,
    events: &Sender<Event>,
    waking: &Arc<AtomicBool>,
    watching: &mut Watching,
) -> State {
    // A second authentication attempt must never replace a live or pending
    // account while leaving its stored credential behind. The shell UI already
    // prevents this; the library boundary enforces it too.
    if matches!(&ask, Ask::SignInWithQr | Ask::SignInWithPassword { .. })
        && !matches!(&state, State::Out)
    {
        return state;
    }
    match ask {
        Ask::SignInWithQr => match auth::begin_with_qr(wire) {
            Ok(session) => {
                offer(events, &session);
                State::Waiting {
                    next: Instant::now() + Duration::from_secs_f32(session.interval),
                    session,
                }
            }
            Err(err) => {
                tracing::warn!(%err, "could not begin a Steam sign-in by code");
                let _ = events.send(Event::SignInFailed(err.said()));
                State::Out
            }
        },
        Ask::SignInWithPassword { account, password } => {
            let guard_data = session::Stored::load().and_then(|stored| stored.guard_data);
            match auth::begin_with_password(wire, &account, &password, guard_data.as_deref()) {
                Ok(session) => {
                    offer(events, &session);
                    State::Waiting {
                        next: Instant::now() + Duration::from_secs_f32(session.interval),
                        session,
                    }
                }
                Err(err) => {
                    tracing::warn!(%err, "could not begin a Steam sign-in by password");
                    let _ = events.send(Event::SignInFailed(err.said()));
                    State::Out
                }
            }
        }
        Ask::SubmitCode(code) => {
            let State::Waiting { session, mut next } = state else {
                return state;
            };
            let Some(confirmation) = session.code_wanted().cloned() else {
                return State::Waiting { session, next };
            };
            if let Err(err) = auth::submit_code(wire, &session, code.trim(), &confirmation) {
                tracing::warn!(%err, "Steam refused the Steam Guard code");
                let _ = events.send(Event::SignInFailed(err.said()));
                let _ = events.send(Event::CodeWanted(confirmation));
            } else {
                next = Instant::now();
            }
            State::Waiting { session, next }
        }
        Ask::CancelSignIn => match state {
            State::Waiting { .. } => {
                let _ = events.send(Event::SignedOut);
                State::Out
            }
            State::Connecting {
                stored,
                failure_to_panel: true,
                ..
            }
            | State::Reconnecting {
                stored,
                failure_to_panel: true,
                ..
            } => {
                if let Err(error) = auth::revoke(wire, &stored.refresh_token, stored.steam_id) {
                    tracing::info!(%error, "Steam was not told about the cancelled sign-in");
                }
                let _ = events.send(Event::SignedOut);
                State::Out
            }
            other => other,
        },
        Ask::SignOut => {
            let stored = match &state {
                State::Connecting { stored, .. }
                | State::Reconnecting { stored, .. }
                | State::In { stored, .. } => Some(stored.clone()),
                _ => session::Stored::load(),
            };
            if let Some(stored) = stored {
                if let Err(error) = auth::revoke(wire, &stored.refresh_token, stored.steam_id) {
                    tracing::info!(%error, "Steam was not told about the sign-out");
                }
                // The client is signed out with the shell. Leaving it signed in
                // would hand the next person at this machine an account the
                // shell has just said nobody is signed in to.
                if let Some(options) = client::Options::found() {
                    let where_it_is = client::Where::find();
                    if client::state(where_it_is.as_ref(), &options).running() {
                        if let Some(where_it_is) = where_it_is {
                            let _ = client::stop(&where_it_is);
                        }
                    }
                    client::autologin::stop(&options.root, &options.home, &stored.account);
                }
            }
            session::Stored::forget();
            let _ = events.send(Event::SignedOut);
            let _ = events.send(Event::Library(Vec::new()));
            State::Out
        }
        Ask::Install { app_id } => {
            let State::In { stored, .. } = &state else {
                let _ = events.send(Event::InstallFailed {
                    app_id,
                    why: Stopped::Failed(
                        "Steam is not signed in, so it cannot fetch this game.".to_string(),
                    ),
                });
                return state;
            };
            // A second press on a game that is already coming down does
            // nothing at all, rather than asking the client twice.
            if !watching.fetching.insert(app_id) {
                return state;
            }
            // Something on screen at once: waking a cold client is most of a
            // minute, and a press that appeared to do nothing for that long is
            // a press somebody makes again.
            let _ = events.send(Event::Installing {
                app_id,
                done: 0,
                total: 0,
            });
            in_the_background(stored, worker, move |ready| Finished::Installing {
                app_id,
                how: match ready {
                    Err(why) => Err(Stopped::Failed(why)),
                    Ok(()) => match webui::install(app_id) {
                        Ok(()) => {
                            tracing::info!(app_id, "Valve's client is fetching this game");
                            Ok(())
                        }
                        Err(webui::Problem::Asks(what)) => {
                            tracing::info!(app_id, %what, "this game cannot be fetched silently");
                            Err(Stopped::Asks(what))
                        }
                        Err(problem) => Err(Stopped::Failed(problem.to_string())),
                    },
                },
            });
            state
        }
        Ask::StopInstalling { app_id } => {
            if let State::In { stored, .. } = &state {
                in_the_background(stored, worker, move |ready| Finished::StoppedInstalling {
                    app_id,
                    how: ready.and_then(|()| {
                        webui::stop_installing(app_id).map_err(|problem| problem.to_string())
                    }),
                });
            }
            state
        }
        Ask::Uninstall { app_id } => {
            let State::In { stored, .. } = &state else {
                let _ = events.send(Event::UninstallFailed {
                    app_id,
                    why: "Steam is not signed in, so it cannot remove this game.".to_string(),
                });
                return state;
            };
            if !watching.removing.insert(app_id) {
                return state;
            }
            let _ = events.send(Event::Uninstalling { app_id });
            in_the_background(stored, worker, move |ready| Finished::Uninstalling {
                app_id,
                how: ready
                    .and_then(|()| webui::uninstall(app_id).map_err(|problem| problem.to_string())),
            });
            state
        }
        Ask::WakeClient => {
            // Only for a session that is actually signed in: the client is
            // signed in with *this* session's credential, and there is no
            // credential until Steam has accepted one.
            let State::In { stored, .. } = &state else {
                let _ = events.send(Event::Client(ClientReport::Unavailable(
                    "Steam is not signed in, so its client cannot be started.".to_string(),
                )));
                return state;
            };
            wake_the_client(stored, waking, events);
            state
        }
        Ask::Refresh => {
            if let State::In {
                stored,
                account,
                generation,
                commands,
                cancel,
                owned,
                library_failure_announced: _,
                next_owned: _,
                next_installed: _,
            } = state
            {
                let _ = commands.send(cm::Command::Refresh);
                let next_owned = Instant::now() + OWNED_INTERVAL;
                let next_installed = Instant::now();
                State::In {
                    stored,
                    account,
                    generation,
                    commands,
                    cancel,
                    owned,
                    library_failure_announced: false,
                    next_owned,
                    next_installed,
                }
            } else {
                state
            }
        }
    }
}

/// Do something that needs Valve's client, on a thread, once it is up.
///
/// Every Steam-backed job in this crate has the same two halves — have the
/// client, then ask it — and the first half is the slow one. The closure is
/// handed whether the client came up, so that a job which cannot run says why
/// in its own words: a failed install is an install that failed, not a client
/// that would not start, however true the second is.
///
/// What it comes back with goes to the worker rather than to the shell. The
/// worker is what remembers which games are being watched for, and a job that
/// ended has to leave that list as well as be announced.
fn in_the_background(
    stored: &session::Stored,
    worker: &Sender<WorkerMessage>,
    work: impl FnOnce(Result<(), String>) -> Finished + Send + 'static,
) {
    let there = client::Options::found().zip(client::Where::find());
    let account = stored.account.clone();
    let refresh_token = stored.refresh_token.clone();
    let worker = worker.clone();
    std::thread::spawn(move || {
        let ready = match &there {
            // Everything that comes this way is made as a call into the
            // client's own interface rather than as a `steam:` URL, so this is
            // where that interface is worth opening — and the only place.
            Some((options, where_it_is)) => client::wake(
                where_it_is,
                options,
                &account,
                &refresh_token,
                client::Need::Context,
            ),
            None => Err("There is no Steam client installed on this machine.".to_string()),
        };
        let _ = worker.send(WorkerMessage::Done(work(ready)));
    });
}

/// What one finished job does to the worker.
///
/// The successful ones say nothing here on purpose. What they started is
/// something the client is now doing to the disk, and the disk is what says
/// how it goes — so all a success does is bring the next look at it forward,
/// from up to ten seconds away to now.
fn came_back(
    finished: Finished,
    state: State,
    events: &Sender<Event>,
    watching: &mut Watching,
) -> State {
    match finished {
        Finished::Installing { how: Ok(()), .. } => at_once(state),
        Finished::Installing {
            app_id,
            how: Err(why),
        } => {
            watching.fetching.remove(&app_id);
            tracing::warn!(app_id, why = %why.said(), "a game was not fetched");
            let _ = events.send(Event::InstallFailed { app_id, why });
            state
        }
        Finished::StoppedInstalling { app_id, how } => {
            watching.fetching.remove(&app_id);
            match how {
                Ok(()) => {
                    let _ = events.send(Event::InstallStopped { app_id });
                    at_once(state)
                }
                Err(why) => {
                    let _ = events.send(Event::InstallFailed {
                        app_id,
                        why: Stopped::Failed(why),
                    });
                    state
                }
            }
        }
        Finished::Uninstalling { how: Ok(()), .. } => at_once(state),
        Finished::Uninstalling {
            app_id,
            how: Err(why),
        } => {
            watching.removing.remove(&app_id);
            tracing::warn!(app_id, %why, "a game was not removed");
            let _ = events.send(Event::UninstallFailed { app_id, why });
            state
        }
    }
}

/// Look at the disk now rather than at the next interval.
///
/// Something has just been asked of it that changes what a row says, and
/// somebody is watching that row.
fn at_once(state: State) -> State {
    match state {
        State::In {
            stored,
            account,
            generation,
            commands,
            cancel,
            owned,
            library_failure_announced,
            next_owned,
            next_installed: _,
        } => State::In {
            stored,
            account,
            generation,
            commands,
            cancel,
            owned,
            library_failure_announced,
            next_owned,
            next_installed: Instant::now(),
        },
        other => other,
    }
}

/// Start Valve's client and sign it in, on a thread of its own.
///
/// On its own thread because it is the one thing here that takes a minute and
/// a half in the worst case, and the worker it would otherwise block is what
/// answers the library, the catalogue and every press. `waking` is what stops
/// two presses starting two of them; it is cleared when the thread is done,
/// whichever way it went.
fn wake_the_client(stored: &session::Stored, waking: &Arc<AtomicBool>, events: &Sender<Event>) {
    if waking.swap(true, Ordering::SeqCst) {
        return;
    }
    let Some(options) = client::Options::found() else {
        waking.store(false, Ordering::SeqCst);
        let _ = events.send(Event::Client(ClientReport::Unavailable(
            "There is no Steam client installed on this machine.".to_string(),
        )));
        return;
    };
    let Some(where_it_is) = client::Where::find() else {
        waking.store(false, Ordering::SeqCst);
        let _ = events.send(Event::Client(ClientReport::Unavailable(
            "There is no Steam client installed on this machine.".to_string(),
        )));
        return;
    };

    let _ = events.send(Event::Client(ClientReport::Waking));
    let account = stored.account.clone();
    let refresh_token = stored.refresh_token.clone();
    let events = events.clone();
    let waking = Arc::clone(waking);
    std::thread::spawn(move || {
        // Signed in is the whole of what a game press needs: the game itself
        // is asked for with a `steam:` URL over the client's pipe, so on a
        // client that can sign itself in nothing is ever exposed.
        let report = match client::wake(
            &where_it_is,
            &options,
            &account,
            &refresh_token,
            client::Need::SignedIn,
        ) {
            Ok(()) => ClientReport::Ready,
            Err(why) => {
                tracing::warn!(%why, "Valve's client could not be signed in");
                ClientReport::Unavailable(why)
            }
        };
        waking.store(false, Ordering::SeqCst);
        let _ = events.send(Event::Client(report));
    });
}

fn answer_cm(event: cm::Event, state: State, events: &Sender<Event>) -> State {
    match (event, state) {
        (
            cm::Event::Ready {
                generation,
                commands,
            },
            State::Connecting {
                stored,
                generation: expected,
                announced,
                owned,
                cancel,
                ..
            },
        ) if generation == expected => {
            let account = Account {
                name: stored.account.clone(),
                steam_id: stored.steam_id,
            };
            tracing::info!(account = %account.name, "signed in to Steam CM");
            if let Err(error) = stored.save() {
                tracing::warn!(%error, "the Steam session could not be written down");
            }
            if !announced {
                let _ = events.send(Event::SignedIn(account.clone()));
            }
            let _ = events.send(Event::Library(library::merge(
                owned.clone(),
                &library::installed(),
            )));
            State::In {
                stored,
                account,
                generation,
                commands,
                cancel,
                owned,
                library_failure_announced: false,
                next_owned: Instant::now() + OWNED_INTERVAL,
                next_installed: Instant::now() + INSTALLED_INTERVAL,
            }
        }
        (
            cm::Event::Library { generation, games },
            State::In {
                stored,
                account,
                generation: expected,
                commands,
                cancel,
                owned: _,
                library_failure_announced: _,
                next_owned: _,
                next_installed,
            },
        ) if generation == expected => {
            let owned = library::owned(games);
            tracing::info!(games = owned.len(), "read the Steam CM/PICS library");
            let next_owned = Instant::now() + OWNED_INTERVAL;
            let _ = events.send(Event::Library(library::merge(
                owned.clone(),
                &library::installed(),
            )));
            State::In {
                stored,
                account,
                generation,
                commands,
                cancel,
                owned,
                library_failure_announced: false,
                next_owned,
                next_installed,
            }
        }
        (
            cm::Event::LibraryFailed { generation, reason },
            State::In {
                stored,
                account,
                generation: expected,
                commands,
                cancel,
                owned,
                library_failure_announced,
                next_owned: _,
                next_installed,
            },
        ) if generation == expected => {
            // A catalogue failure is not an authentication failure. Keep the
            // account, token and last successful catalogue intact.
            tracing::warn!(%reason, "the Steam CM/PICS library could not be read");
            if owned.is_empty() && !library_failure_announced {
                let _ = events.send(Event::LibraryUnavailable(
                    "Steam accepted the account, but its library could not be read. LineXinBar will try again."
                        .to_string(),
                ));
            }
            let next_owned = Instant::now() + Duration::from_secs(60);
            let library_failure_announced = library_failure_announced || owned.is_empty();
            State::In {
                stored,
                account,
                generation,
                commands,
                cancel,
                owned,
                library_failure_announced,
                next_owned,
                next_installed,
            }
        }
        (
            cm::Event::Ended {
                generation,
                failure,
            },
            State::Connecting {
                stored,
                generation: expected,
                announced,
                failure_to_panel,
                owned,
                cancel: _,
            },
        ) if generation == expected => {
            ended(stored, announced, failure_to_panel, owned, failure, events)
        }
        (
            cm::Event::Ended {
                generation,
                failure,
            },
            State::In {
                stored,
                generation: expected,
                owned,
                ..
            },
        ) if generation == expected => ended(stored, true, false, owned, failure, events),
        (_, state) => state,
    }
}

fn ended(
    stored: session::Stored,
    announced: bool,
    failure_to_panel: bool,
    owned: Vec<Game>,
    failure: cm::Failure,
    events: &Sender<Event>,
) -> State {
    tracing::warn!(%failure, "the Steam CM session ended");
    if failure.permanently_rejected() {
        session::Stored::forget();
        if failure_to_panel && !announced {
            let _ = events.send(Event::SignInFailed(failure.said()));
        } else {
            let _ = events.send(Event::SignedOut);
            if announced {
                let _ = events.send(Event::Library(Vec::new()));
            }
        }
        State::Out
    } else {
        if failure_to_panel {
            let _ = events.send(Event::Waiting(failure.said()));
        }
        State::Reconnecting {
            stored,
            next: Instant::now() + Duration::from_secs(5),
            announced,
            failure_to_panel,
            owned,
        }
    }
}

fn advance(
    state: State,
    wire: &web::Wire,
    worker: &Sender<WorkerMessage>,
    events: &Sender<Event>,
    generation: &mut u64,
    watching: &mut Watching,
) -> State {
    match state {
        State::Out | State::Connecting { .. } => state,
        State::Reconnecting {
            stored,
            announced,
            failure_to_panel,
            owned,
            ..
        } => connect(
            stored,
            announced,
            failure_to_panel,
            owned,
            worker,
            generation,
        ),
        State::Waiting {
            mut session,
            next: _,
        } => match auth::poll(wire, &session) {
            Ok(auth::Polled::Waiting) => {
                let next = Instant::now() + Duration::from_secs_f32(session.interval);
                State::Waiting { session, next }
            }
            Ok(auth::Polled::Renewed(url)) => {
                session.challenge_url = Some(url.clone());
                let next = Instant::now() + Duration::from_secs_f32(session.interval);
                match qr::encode(&url) {
                    Some(code) => {
                        let _ = events.send(Event::Challenge { code, url });
                    }
                    None => {
                        let _ = events.send(Event::SignInFailed(
                            "Steam sent a sign-in code that could not be drawn.".to_string(),
                        ));
                    }
                }
                State::Waiting { session, next }
            }
            Ok(auth::Polled::Granted(granted)) => {
                tracing::info!(account = %granted.account, "Steam granted a CM credential");
                let stored = session::Stored::granted(
                    granted.account,
                    granted.steam_id,
                    granted.refresh_token,
                    granted.guard_data,
                );
                let _ = events.send(Event::Waiting(
                    "Steam approved the account. Connecting to its library.".to_string(),
                ));
                connect(stored, false, true, Vec::new(), worker, generation)
            }
            Err(err) => {
                tracing::warn!(%err, "a Steam sign-in ended");
                let _ = events.send(Event::SignInFailed(err.said()));
                State::Out
            }
        },
        State::In {
            stored,
            account,
            generation: current_generation,
            commands,
            cancel,
            owned,
            library_failure_announced,
            mut next_owned,
            mut next_installed,
        } => {
            let now = Instant::now();
            if now >= next_owned {
                let _ = commands.send(cm::Command::Refresh);
                next_owned = now + OWNED_INTERVAL;
            }
            if now >= next_installed {
                let installed = library::installed();
                // What this session asked for, first: a row that is counting
                // up has to be told before the column it sits in is replaced.
                watching.report(&installed, events);
                let _ = events.send(Event::Library(library::merge(owned.clone(), &installed)));
                next_installed = now + INSTALLED_INTERVAL;
            }
            State::In {
                stored,
                account,
                generation: current_generation,
                commands,
                cancel,
                owned,
                library_failure_announced,
                next_owned,
                next_installed,
            }
        }
    }
}

/// Say what a sign-in that has just begun is waiting for.
fn offer(events: &Sender<Event>, session: &auth::Session) {
    if let Some(url) = session.challenge_url.as_deref() {
        match qr::encode(url) {
            Some(code) => {
                let _ = events.send(Event::Challenge {
                    code,
                    url: url.to_string(),
                });
            }
            None => {
                let _ = events.send(Event::SignInFailed(
                    "Steam sent a sign-in code that could not be drawn.".to_string(),
                ));
            }
        }
        return;
    }
    match session.code_wanted() {
        Some(confirmation) => {
            let _ = events.send(Event::CodeWanted(confirmation.clone()));
        }
        None => {
            // Nothing to type: either a press on the phone, or an account with
            // no guard at all, and the poll is the whole of the wait.
            let waiting = session
                .confirmations
                .first()
                .map(Confirmation::asked)
                .unwrap_or_else(|| "Signing in.".to_string());
            let _ = events.send(Event::Waiting(waiting));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::*;

    fn on_disk(app_id: u32, updating: bool, playable: bool) -> library::Installed {
        library::Installed {
            app_id,
            name: "Invented".to_string(),
            size_on_disk: 40,
            path: PathBuf::from("/games/Invented"),
            playable,
            updating,
            downloaded: 30,
            to_download: 100,
            tool: false,
        }
    }

    fn disk(entries: Vec<library::Installed>) -> BTreeMap<u32, library::Installed> {
        entries
            .into_iter()
            .map(|entry| (entry.app_id, entry))
            .collect()
    }

    /// A download says how it is going, from the manifest Valve's client keeps
    /// for the game — and it says it again at every look, because that is the
    /// only account of it there is. The client reports progress to nobody.
    ///
    /// This is the half that was missing. One `Installing` was sent when the
    /// press was taken and nothing ever again, so a row read "Installing…"
    /// with no percentage for as long as the session lasted, whether the game
    /// had arrived an hour ago or had never started coming down at all.
    #[test]
    fn a_download_is_reported_from_the_disk_until_it_has_arrived() {
        let (send, taken) = mpsc::channel();
        let mut watching = Watching::default();
        watching.fetching.insert(7);

        watching.report(&disk(vec![on_disk(7, true, false)]), &send);
        assert!(
            matches!(
                taken.try_recv(),
                Ok(Event::Installing {
                    app_id: 7,
                    done: 30,
                    total: 100
                })
            ),
            "how far it has got has to reach the row"
        );
        assert!(watching.fetching.contains(&7), "it is not there yet");

        // And once it is on the disk and playable, it is said once and then
        // stopped being watched for: nothing else in the session can tell the
        // row to stop counting.
        watching.report(&disk(vec![on_disk(7, false, true)]), &send);
        assert!(matches!(
            taken.try_recv(),
            Ok(Event::Installed { app_id: 7, .. })
        ));
        assert!(watching.fetching.is_empty());
        watching.report(&disk(vec![on_disk(7, false, true)]), &send);
        assert!(
            taken.try_recv().is_err(),
            "a game that arrived is not announced twice"
        );
    }

    /// Nothing on the disk yet is not a failure. Waking a cold client is most
    /// of a minute, and the press has already put "Installing…" on the row —
    /// so the honest thing to say at this point is nothing.
    #[test]
    fn a_download_that_has_not_begun_writing_says_nothing() {
        let (send, taken) = mpsc::channel();
        let mut watching = Watching::default();
        watching.fetching.insert(7);

        watching.report(&disk(Vec::new()), &send);
        assert!(taken.try_recv().is_err());
        assert!(
            watching.fetching.contains(&7),
            "and it is still watched for"
        );
    }

    /// A removal is the same idea the other way up: the manifest leaving the
    /// disk is the only thing that says the game has gone, because the call
    /// that removes it answers before the client has done any of the work.
    #[test]
    fn a_removal_is_over_when_the_manifest_has_gone() {
        let (send, taken) = mpsc::channel();
        let mut watching = Watching::default();
        watching.removing.insert(7);

        watching.report(&disk(vec![on_disk(7, false, true)]), &send);
        assert!(taken.try_recv().is_err(), "it is still there");

        watching.report(&disk(Vec::new()), &send);
        assert!(matches!(
            taken.try_recv(),
            Ok(Event::Uninstalled { app_id: 7 })
        ));
        assert!(watching.removing.is_empty());
    }

    /// Only what this session asked for. A game somebody set downloading in
    /// Steam itself is already on the bar as one that is updating, in the
    /// library's own words, and does not want a second voice saying the same
    /// thing in different words.
    #[test]
    fn nothing_is_said_about_a_game_nobody_here_asked_for() {
        let (send, taken) = mpsc::channel();
        let mut watching = Watching::default();

        watching.report(&disk(vec![on_disk(7, true, false)]), &send);
        assert!(taken.try_recv().is_err());
    }

    /// A game part way through being repaired is on the disk, not moving, and
    /// not playable. There is nothing true to say about it, so nothing is
    /// said — and it stays watched for, because it is still on its way
    /// somewhere.
    #[test]
    fn a_game_that_is_neither_arriving_nor_arrived_is_left_alone() {
        let (send, taken) = mpsc::channel();
        let mut watching = Watching::default();
        watching.fetching.insert(7);

        watching.report(&disk(vec![on_disk(7, false, false)]), &send);
        assert!(taken.try_recv().is_err());
        assert!(watching.fetching.contains(&7));
    }

    /// The two things a press can end in are answered differently by the
    /// shell — one is a panel that says something went wrong, the other a
    /// panel that offers to open Steam — so they are two shapes and not one
    /// string somebody has to read the wording of.
    #[test]
    fn a_question_and_a_failure_are_not_the_same_answer() {
        assert_eq!(
            Stopped::Asks("an agreement to accept".to_string()).said(),
            "an agreement to accept"
        );
        assert_eq!(
            Stopped::Failed("the disk filled".to_string()).said(),
            "the disk filled"
        );
        assert_ne!(
            Stopped::Asks("x".to_string()),
            Stopped::Failed("x".to_string())
        );
    }
}
