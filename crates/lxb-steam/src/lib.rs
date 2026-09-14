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
//! | [`friends`]  | who the account knows, where each of them is, and where their pictures are |
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

pub mod achievements;
pub mod art;
pub mod audit;
pub mod auth;
pub mod backend;
pub mod catalogue;
pub mod chat;
pub mod client;
pub mod friends;
pub mod library;
pub mod process;
pub mod qr;
pub mod setup;
pub mod turns;
pub mod webui;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub use auth::Confirmation;
pub use client::Doing;
pub use friends::{AvatarSize, Band, Person, Presence, Roster};
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

/// And how often while something is actually moving.
///
/// Ten seconds is the right rate for a question nobody is watching — has
/// anything changed on the disk — and the wrong rate for the one thing in this
/// shell somebody stands and watches. A percentage and a bar that step once
/// every ten seconds read as a download that has stopped, which is the state
/// this shell goes to trouble to say *separately* (see [`Event::InstallStuck`]),
/// so the two would be indistinguishable.
///
/// Two seconds because that is about the rate Valve's client rewrites the
/// manifest it is read from: asking faster would be reading the same numbers
/// again. It costs a directory listing and a few small files per Steam library,
/// and only while something is moving.
const WHILE_SOMETHING_MOVES: Duration = Duration::from_secs(2);

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
    AchievementProgress {
        app_ids: Vec<u32>,
        request: u64,
    },
    /// Read-only achievement progress for one game, correlated with the caller's request.
    Achievements {
        app_id: u32,
        request: u64,
    },
    /// Have Valve's client install itself on this machine, which it has never
    /// done.
    ///
    /// The first thing a session on a new machine asks for and the only one
    /// that can be asked while signed out — it is not a Steam request at all,
    /// it is half a gigabyte off Valve's download hosts and an unpack, and it
    /// has to finish before there is a client for anything else here to talk
    /// to. Answered by a run of [`Event::Setup`]s ending in
    /// [`setup::SetUp::Done`] or [`setup::SetUp::Failed`]; see [`setup`].
    ///
    /// Does nothing where Steam is already installed, and nothing where a setup
    /// this session started is still running. Both are ordinary: the second is
    /// somebody pressing the row again to see the panel they dismissed.
    SetUpTheClient,
    /// Begin a sign-in to be confirmed by photographing a code.
    SignInWithQr,
    /// Begin one with an account name and a password.
    SignInWithPassword {
        account: String,
        password: Password,
    },
    /// Hand over the Steam Guard code that was asked for.
    SubmitCode(String),
    /// Stop waiting for a sign-in that is under way.
    CancelSignIn,
    /// Give up the stored session.
    SignOut,
    /// Ask Steam for the library again, now, rather than at the next interval.
    Refresh,
    /// Have Valve's client fetch one game.
    Install {
        app_id: u32,
    },
    /// Stop fetching one, and take away what had arrived.
    StopInstalling {
        app_id: u32,
    },
    /// Have Valve's client take one game off the disk, keeping it in the
    /// library.
    Uninstall {
        app_id: u32,
    },
    /// Ask Valve's client which Steam Play compatibility tools one title — or
    /// every unverified title — may be run with, and which of them is forced.
    ///
    /// Answered by [`Event::Compatibility`], or by
    /// [`Event::CompatibilityUnavailable`] where the client could not be
    /// reached or would not say. Needs the client's own interface, which is
    /// why it is asked rather than read: the list is Valve's knowledge of what
    /// this account may use, not a list of what is on the disk.
    Compatibility(webui::Which),
    /// Have it run under this tool from now on, or under whatever Steam
    /// chooses. `None` is the second of those.
    ///
    /// Answered by a fresh [`Event::Compatibility`] read back out of the
    /// client, so what the shell draws is what Steam ended up with rather than
    /// what it was asked for.
    ForceCompatibility {
        which: webui::Which,
        tool: Option<String>,
    },
    /// Have Valve's client running and signed in, because something is about
    /// to need it. Does nothing if it already is.
    ///
    /// `take_over` is the answer to [`ClientReport::SomebodyElses`], and is
    /// false for every press that has not been through it: a client belonging
    /// to another session or another account is left alone and reported.
    ///
    /// `request` is the press this belongs to, and it comes back on the
    /// [`Ticket`] of every [`Event::Client`] answering it. **A wake takes most
    /// of two minutes and the press that asked for it is often over by then**,
    /// so an answer that named nobody was an answer the shell handed to
    /// whichever press happened to be waiting when it arrived. See [`Ticket`].
    WakeClient {
        take_over: bool,
        request: u64,
    },
    /// Hand Valve's client one `steam:` URL — install, verify, or simply come
    /// to the front. Off the shell's thread because starting a process is not
    /// something a render loop can afford to wait on; see [`Steam::tell`].
    Tell {
        app_id: u32,
        doing: Doing,
    },
    /// Watch a launch that has just been asked for, and say if it stops.
    ///
    /// One per game, and several at once: a session can have handed the client
    /// two games on two displays, and a watch that ended when the *other* one
    /// did is a question about a game somebody is still waiting for that
    /// nobody ever hears. See [`Steam::watch_this_launch`].
    ///
    /// `request` rides on the [`Asking`] the watcher sends, for the race the
    /// watcher cannot win on its own: it decides to speak, and the press it is
    /// speaking about ends before the message is read. A watch that has been
    /// stopped and replaced answers under the number it was started with, and
    /// the shell drops it.
    WatchLaunch {
        app_id: u32,
        request: u64,
    },
    /// Stop watching one launch, or every launch there is.
    ///
    /// `None` is for the one moment when every watch is somebody else's at
    /// once: the account signing out. Every other path out of a press names
    /// the game it was about, because the press beside it may still be
    /// running.
    StopWatchingLaunch {
        app_id: Option<u32>,
    },
    /// Answer a launch the client stopped, with what somebody chose.
    AnswerLaunch {
        action_id: u32,
        carry: webui::Carry,
    },
    /// Tell Valve's client to stop walking the launch it is in the middle of
    /// for one game.
    ///
    /// The answer to somebody who has stopped waiting: the client was going to
    /// open the game once it had finished whatever it decided to do first, and
    /// a game that opens over the screen a quarter of an hour after the loading
    /// screen was dismissed is not what the press meant any more.
    ///
    /// Says nothing back, on the terms [`Ask::CloseClient`] does. The launch
    /// the shell was watching is over either way — this is only what makes the
    /// client agree — and a client that had already finished, or that has no
    /// such action left, is not a failure anybody has to be told about.
    StopLaunch {
        app_id: u32,
    },
    /// Something about a one-to-one conversation: open one, send a message,
    /// or say this account is typing.
    ///
    /// Handed to the CM session as it stands — there is no second connection
    /// and no second sign-in, and nothing here ever reaches Valve's client. A
    /// request made while the CM is down is **dropped rather than queued**, and
    /// that is deliberate: a message the user was told had gone and which
    /// actually went five minutes later, to a friend who has since gone
    /// offline, is worse than one they were told did not go. The panel refuses
    /// to send while the connection is down — see
    /// [`chat::Refused::NotConnected`] — so this arm is only reached where
    /// there is a session to carry it, and the race between the two is answered
    /// by failing the send.
    ///
    /// Answered by [`Event::Chat`], correlated by generation, account,
    /// conversation and request. See [`chat`], where the whole of that rule is.
    Chat(chat::Wanted),
    /// Tell Steam the account is in this state, because somebody chose it.
    ///
    /// The one thing this crate ever writes about the account — see the head of
    /// [`friends`], where what a session announces and why is set out. Kept for
    /// the rest of the session and announced again after a reconnect, so a
    /// status set at the start screen survives the wifi dropping; not written
    /// to disk, so the next boot is a session nobody has spoken for.
    ///
    /// Says nothing back. Steam echoes the account's own persona state, which
    /// arrives as an ordinary [`Event::Friends`] a moment later; the panel is
    /// told sooner than that by the CM session itself, which puts the chosen
    /// status into the roster it has in hand.
    SetStatus(friends::Presence),
    /// Ask Valve's client to shut down, if it is one this session started.
    ///
    /// The counterpart of [`Ask::WakeClient`]. It used to say nothing back, on
    /// the reasoning that a client which had already gone, that belongs to
    /// somebody else, or that would not go is not a failure anybody has to be
    /// told about — the log carried all three, and nothing above had a decision
    /// to make from them.
    ///
    /// **Something above does now.** The shell gives a client that has been
    /// asked a grace period to go in, asks once more, and then says it would
    /// not shut down; and a client belonging to another session is never asked
    /// at all, so all of that was seventy seconds spent waiting for an answer
    /// nobody had put a question to. It reports [`Event::ClientClosing`],
    /// carrying `request` back on its [`Ticket`] so that one ask's answer
    /// cannot be read as another's.
    CloseClient {
        request: u64,
    },
}

/// What one asynchronous answer belongs to.
///
/// Every answer in this crate that crosses a thread and outlives the press that
/// asked for it carries one of these, and the whole of the rule is one
/// sentence: **an answer completes the request it names and no other.**
///
/// The failure it exists for is not hypothetical, and it is not a race that
/// needs an unlucky millisecond. Waking a cold client is most of two minutes.
/// In two minutes a loading screen runs out of patience, a display is
/// unplugged, a panel is dismissed, an account signs out and another signs in —
/// and the wake goes on running, because it is a thread most of the way through
/// starting a program and there is nothing to call it back with. It always
/// answers. Before this, that answer said only *what happened to the client*,
/// so the shell handed it to whichever press was waiting at the moment it
/// arrived. One account's abandoned wake reporting `Ready` started another
/// account's game.
///
/// Three numbers, and each closes a different way of being wrong:
///
/// * `request` — which press. Handed down by the caller and echoed back
///   untouched. This is the one the shell checks.
/// * `ground` — which account and which Steam, as [`Watching::generation`]
///   counts them. The worker checks this one, and it is what makes signing out
///   or a client changing underneath the session invalidate what is in flight.
/// * `account` — who it was for. Checked with `ground` rather than instead of
///   it: two checks of one fact, and neither is redundant, because a generation
///   is a count of movements and this is the thing that moved.
///
/// What is deliberately *not* here is the game and the display. Both are the
/// shell's, both are already written down against `request` in the press it is
/// waiting on, and a second copy on the wire would be a second copy to keep
/// true. The identity of the client *process* is not here either: this says
/// which request an answer belongs to, and whether the client that answered is
/// still the client a game is about to be asked of is a different question,
/// asked of the machine at the moment it matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Ticket {
    /// The request this answers, as whoever asked numbered it.
    pub request: u64,
    /// The account and the Steam it was asked under.
    pub ground: u64,
    /// The account it was asked for. Zero where none was signed in.
    pub account: u64,
}

/// Where the session stands **now**, readable from a thread that is minutes
/// deep in a wake.
///
/// [`Ticket::ground`] says what a job was started on; [`Watching::generation`]
/// says what the session is on. Until this existed the two could only be
/// compared in one place — `came_back`, on the worker, *after* the job had
/// finished — and that is late enough to keep a stale answer off the screen
/// and far too late to keep a stale **authority** from acting. By then the
/// game is installed, removed, stopped or reassigned, out of an account that
/// signed out a minute ago; ignoring the result prevents the wrong row from
/// moving and prevents nothing else.
///
/// So the worker publishes the two numbers here and every job carries a clone
/// and asks. Five times, and each one closes a gap the last cannot:
///
/// 1. **Before the client is woken** — the job may have queued behind another
///    press's wake for the whole of [`client::LONGEST_ORDINARY_WAKE`].
/// 2. **After the wake** — which is most of two minutes of somebody signing
///    out, unplugging a display, or swapping the Steam under the session.
/// 3. **On the far side of the wizard's queue**, where one install can hold
///    the machine's only install wizard for `UNTIL_THE_WIZARD_ANSWERS`.
/// 4. **In the last instant before the call that cannot be taken back.**
/// 5. Where the answer is applied, which is `came_back` and is the one that
///    was always there.
///
/// Three and four are asked inside [`webui`], because they are moments only
/// that module is in: see [`webui::Still`].
///
/// And the ground is not the whole of it. All five say the *session* has not
/// moved; none of them says which client is on the other end of the socket,
/// and the interface is a port on loopback that whoever came up last is
/// holding. So the client [`standing_on`] proved is carried to the socket
/// too, and the connection is bound to it before anything goes out — see
/// [`webui::Still::reaches_the_client_it_proved`], which is asked between
/// three and four.
#[derive(Clone, Default)]
pub(crate) struct Ground(Arc<std::sync::Mutex<(u64, u64)>>);

impl Ground {
    /// Publish where the session stands, from the worker.
    ///
    /// The same two values [`still_the_same_ground`] compares, read in the same
    /// place, so the question a thread asks is the question the worker would
    /// have asked and not an older one wearing the same name.
    fn moved_to(&self, generation: u64, account: u64) {
        *self.0.lock().unwrap_or_else(|held| held.into_inner()) = (generation, account);
    }

    /// Whether a job asked for on `ticket` is still standing on the ground it
    /// was asked for on.
    ///
    /// Both numbers, for the reason [`still_the_same_ground`] checks both: the
    /// generation is a count of movements and the account is the thing that
    /// moved, so a generation that somehow did not move cannot let one
    /// account's authority act on another's library.
    ///
    /// A `Mutex` rather than two atomics because the pair has to be read
    /// together. Read apart, a job could see the new generation beside the old
    /// account, and the answer to "has this moved" would depend on which half
    /// landed first. It is uncontended in every ordinary case — one store per
    /// worker turn, a handful of loads per press.
    fn still(&self, ticket: &Ticket) -> Result<(), String> {
        let (generation, account) = *self.0.lock().unwrap_or_else(|held| held.into_inner());
        if generation == ticket.ground && account == ticket.account {
            return Ok(());
        }
        Err(format!(
            "The Steam account this was asked for is no longer the one signed in \
             (was {}/{}, now {generation}/{account}).",
            ticket.ground, ticket.account
        ))
    }
}

/// Whether two roots are one directory under two names, resolved on the disk.
///
/// Two names that cannot both be resolved are two names: a root that has gone
/// is a Steam that has gone, which is exactly the change this exists to notice.
fn same_directory(a: Option<&std::path::Path>, b: Option<&std::path::Path>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        },
        _ => false,
    }
}

/// What one background job stands on, carried from the wake to the act.
///
/// The answer to the audit's sixth step and to its fifth at once: it holds the
/// ground the job was asked on, the Steam it was resolved against, and the
/// client that was **proved** — so the last question before an irreversible
/// call can be asked of the same client rather than of whatever is holding the
/// pipe by then. See [`Standing::about_to_act`].
pub(crate) struct Standing {
    ticket: Ticket,
    ground: Ground,
    client: client::Where,
    options: client::Options,
    proven: client::Proven,
}

impl Standing {
    /// Everything that has to still be true, asked in one call.
    ///
    /// Handed to [`webui`] as its [`webui::Still`] and asked again by hand
    /// wherever a flow acts outside that module. It says *why* out loud rather
    /// than only answering, because the ground having moved is the one refusal
    /// nobody is ever told about — the row it belonged to went with the
    /// account — and a log line is the whole of what is left of it.
    fn about_to_act(&self) -> Result<(), String> {
        if let Err(why) = self.ground.still(&self.ticket) {
            tracing::info!(%why, "not acting on Valve's client for an account that has gone");
            return Err(why);
        }
        self.proven
            .still_there(&self.client, &self.options)
            .map_err(|refusal| {
                let why = said_about(&refusal);
                tracing::info!(%why, "the client this was proved against is not the one there now");
                why
            })
    }

    /// What this job stands on, as [`webui`] takes it.
    ///
    /// The ground and the client in one value, so that the module which owns
    /// the socket can ask both at the moment only it is in: after the wizard
    /// has been waited for and the interface opened. See [`webui::Still`].
    fn still(&self) -> webui::Still<'_> {
        webui::Still::standing_on(self, self.proven, self.ticket.request)
    }

    /// The ground half alone.
    ///
    /// For the caller whose next act re-proves the client itself as part of
    /// doing its job — see [`client::deliver`], which will not hand a URL to a
    /// client it has not just checked. Asking [`Standing::about_to_act`] there
    /// would walk `/proc` twice in a row to answer the same question.
    fn ground_is_still_there(&self) -> Result<(), String> {
        self.ground.still(&self.ticket)
    }
}

impl webui::Ground for Standing {
    fn about_to_act(&self) -> Result<(), String> {
        Standing::about_to_act(self)
    }
}

/// How a job comes to have a client to act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HaveAClient {
    /// Start one and sign it in where there is not one already.
    ByWaking(client::Need),
    /// Prove the one that is running, and never start or move one.
    ///
    /// For the two calls made **into a launch that is already walking**. A
    /// client that would have to be started is a client with no launch in it,
    /// and a wake that decided it had to restart one to open its port would end
    /// the very game it was asked about. Nothing here is worth that: the answer
    /// to a launch that has no client left to hear it is silence.
    AsItStands,
}

/// The steps every title-specific action takes, in the one order they are safe
/// in.
///
/// **This is the shape finding 4 asked for**, and it is one function so that
/// there is one order rather than one per press: resolve which Steam, check the
/// ground, reach the client, prove whose it is and whose account it is on, and
/// check the ground again — after which what comes back can be asked all of it
/// over again at the moment of acting.
///
/// What it replaces, on the paths that used to take it, was an ownership check
/// that a stopped client passed: with no Steam running there was nothing to
/// inspect, so "is this ours" answered yes and the URL was handed to a client
/// started from cold, which signed itself into whichever account it last
/// remembered. An install pressed in one account's library was delivered to
/// another's before this session had proved anything at all.
fn standing_on(
    stored: &session::Stored,
    ticket: Ticket,
    ground: &Ground,
    have: HaveAClient,
) -> Result<Standing, client::Refusal> {
    // 1. Which Steam. From the client that is actually there rather than from
    //    a scan of the disk — see [`backend::Backend::chosen`].
    let Some((options, client)) =
        client::Where::find().and_then(|found| Some((client::Options::for_client(&found)?, found)))
    else {
        return Err(client::Refusal::Failed(
            "There is no Steam client installed on this machine.".to_string(),
        ));
    };
    // Before a client is started, because a job can sit behind another press's
    // wake — `client::ONE_AT_A_TIME` holds the whole of one, and now holds it
    // against every session on the machine rather than every thread of this one
    // — for as long as a cold client takes, and starting Steam for an account
    // that has signed out is the authority acting, not merely reporting.
    ground.still(&ticket).map_err(client::Refusal::Failed)?;
    // 2, 3 and 4 together, because they are one call: `wake` starts or reaches
    // the client and answers with what it proved about it.
    let proven = match have {
        HaveAClient::ByWaking(need) => client::wake(
            &client,
            &options,
            credential(stored),
            need,
            // Never with permission to take a client over. Moving somebody's
            // Steam is a question, and a background job has nobody to ask.
            client::Permission::AskFirst,
            ticket.request,
        )?,
        HaveAClient::AsItStands => client::prove(&client, &options, credential(stored))?,
    };
    // And after it, which is the first moment the wake's own minute and a half
    // can be accounted for.
    ground.still(&ticket).map_err(client::Refusal::Failed)?;
    Ok(Standing {
        ticket,
        ground: ground.clone(),
        client,
        options,
        proven,
    })
}

/// A launch Valve's client has given up on.
///
/// The other way a press ends without a game and without a word, and the one
/// nothing in this shell could see. A question stops the launch and stands
/// there — see [`Asking`] — while this **is over**: the client has decided, it
/// has said so in a window this shell holds off the screen, and no window of
/// the game is coming. Read off its own log; see
/// [`client::how_the_launch_went`], which carries the evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchRefused {
    /// Which [`Ask::WatchLaunch`] this came out of, exactly as [`Asking`]
    /// carries it and for the same reason.
    pub request: u64,
    pub app_id: u32,
    /// Valve's own name for what went wrong — `AppError_19`, which is "update
    /// required". **For the log.** It is an internal identifier, it is in
    /// English whatever language Steam is running in, and what the shell does
    /// about a refusal is decided from what the shell can see for itself.
    pub why: String,
}

/// What Valve's client is doing about a game, under a launch that has not
/// finished.
///
/// **Not a question, though the client stops on some of these like one.** A
/// launch walks a dozen tasks and several of them are minutes long: fetching
/// an update the press itself put in the queue, compiling a shader cache,
/// syncing saves, running an installer. Each finishes by itself and counts
/// itself while it does, and Steam stops on exactly one of them — the shader
/// cache — only to offer a way past. So they belong on the loading screen,
/// beside everything else a press waits for, rather than in a panel over it.
///
/// Reported from use twice, on 2026-09-04, both times as a Valve window that
/// came out from under this shell's loading screen. First *"Processing Vulkan
/// shaders (0%)"*, with `Skip` and `Cancel`, on a session driven with a
/// controller and no way to press either button. Then *"Starting game /
/// Counter-Strike 2 / LAUNCHING / Downloading content (19%)"*, after the shell
/// had already given up and said the game failed to start — a minute into an
/// update the same press had asked for. The first of those needed a button;
/// the second needed only to be **seen**, which is what this is for.
///
/// Sent as it changes rather than once, because a percentage that is said once
/// is a percentage that is wrong a second later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub request: u64,
    pub app_id: u32,
    /// Valve's own name for the task — `DownloadingDepots`,
    /// `ProcessingShaderCache`, `SynchronizingCloud`. `None` is a launch that
    /// has moved on to something with nothing to say, and is what takes the
    /// line off the loading screen.
    ///
    /// Passed on rather than interpreted. Which of these the shell has words
    /// for, and which it draws nothing for because they are over before
    /// anybody could read them, is the shell's own table — see
    /// `lxb_desktop::launch::words_for`. The names themselves are Valve's, are
    /// in English whatever language Steam is running in, and are read off
    /// [`webui::launching`] and out of the client's log alike.
    pub task: Option<String>,
    /// How far through that step, where the client's interface can be asked.
    /// `None` for a client that was already running when this session came up
    /// and exposes none — its log says *which* step, and only the interface
    /// says how far — and `None` for the steps that do not count themselves.
    pub how_far: Option<u32>,
    /// And which action to answer to be let past, for the one step that offers
    /// it. `None` is a wait with nothing to press: every step but the shader
    /// cache, and the shader cache on a client with no interface to answer.
    pub skip: Option<u32>,
}

/// A launch that has stopped and cannot go on until somebody answers.
///
/// Named for what it is rather than for what asked it: the shell's answer is
/// the same whichever question it is — let Valve's client be seen, so the
/// person who pressed the game can answer it — and the wording belongs to
/// Valve's own window, which is about to be on the screen saying it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asking {
    /// Which [`Ask::WatchLaunch`] this came out of, so a watcher that spoke
    /// about a launch the shell has since let go of can be dropped.
    pub request: u64,
    pub app_id: u32,
    /// The client's own name for the step it stopped on. For the log.
    pub task: String,
    /// The question in words, where this shell has a panel for it.
    ///
    /// `None` for one it has not — an agreement to read, a key to copy down, a
    /// choice between two saves that needs the date of each. Those are answered
    /// the older way, by letting Valve's own window be seen, because a panel
    /// that put an OK on a licence nobody read would be worse than a mouse.
    pub question: Option<webui::Question>,
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
    /// There is a client running and it is not this session's to move: it is
    /// drawing into another session, or it is signed in as another account.
    /// Nothing has been done to it.
    ///
    /// Separate from [`ClientReport::Unavailable`] because the answer is
    /// different. This is not a failure to report and dismiss; it is a choice
    /// to put in front of the person, whose other half is
    /// [`Steam::take_over_the_client`]. What it carries is what to say while
    /// asking — see [`client::NotOurs`], which is two lines because the panel
    /// that draws it gives a line to each and cuts the rest.
    SomebodyElses(client::NotOurs),
}

/// What the shell is told when Valve's client takes a `steam:` URL.
///
/// One fact beside the event, and it is the fact that decides how the window
/// is shown. A client that was up and signed in raises the window the request
/// is for and nothing else; a client the wake had to sign in first was sitting
/// on its own login screen, and it keeps that screen up for some seconds
/// *after* it reports logged on before it raises anything. Measured on
/// 2026-09-13, on a client this shell had just installed: credential taken at
/// 20:31:54, login window closed at 20:31:59, storefront mapped at 20:32:01.
///
/// A shell that gave sight the moment the request was taken put that login
/// screen on the display for five seconds, and then — reading a window of
/// Valve's going away as the person being done with Steam — hid the client
/// again two seconds before the storefront arrived. That was "Open Steam does
/// nothing after a first setup". So the shell is told, and on a client that has
/// just been signed in it waits for a window of the request's own before it
/// shows anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HandedOver {
    /// Whether the wake behind this hand-over signed the client in, rather
    /// than finding it signed in already. See [`client::Proven::just_signed_in`].
    pub after_signing_in: bool,
    /// And what was asked for, because which of the client's windows is the
    /// one the press is waiting on depends on it. See
    /// [`Doing::answered_by_the_storefront`].
    pub asked: Doing,
}

/// Why a request that would have raised a window of Steam's own was not made.
///
/// Its own type, and delivered on its own event, because the alternative was
/// inference. Everything the client said used to arrive as
/// [`Event::Client`], and the shell told a hand-over's failure from a launch's
/// by whether a press happened to be waiting at that moment — which is right
/// until the two overlap, and then a refused Open Steam takes down the loading
/// screen of a game that is starting perfectly well.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    /// It could not be delivered, and this is what to say.
    Failed(String),
    /// It was not attempted. The client that would have taken it is not this
    /// session's — another display's, or another account's — and doing it
    /// there is not what the press meant.
    SomebodyElses(client::NotOurs),
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refused::Failed(why) => f.write_str(why),
            Refused::SomebodyElses(why) => write!(f, "{why}"),
        }
    }
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
    /// Nothing went wrong here either: this version of Valve's client no longer
    /// answers the calls a silent install is made of.
    ///
    /// `SteamClient` is private and unpromised, and an update may rename any of
    /// it. The shape of that failure used to be a press that did nothing at all
    /// — the flow opened a wizard nobody was listening to and came back after
    /// its patience with Valve's own unhelpful wording — so it is now asked
    /// about before anything is driven, and answered the way the question above
    /// is: by offering the window that can still do it.
    NotFromHere(String),
}

impl Stopped {
    /// What to put in front of somebody.
    pub fn said(&self) -> &str {
        match self {
            Stopped::Failed(why) => why,
            Stopped::Asks(why) => why,
            Stopped::NotFromHere(why) => why,
        }
    }
}

/// How far away Steam is, as the shell has to draw it.
///
/// Its own answer, separate from whether anybody is signed in, because the two
/// used to be one and the one was wrong. A session that comes up with a stored
/// credential and no network *is signed in*: it has an account, it has a
/// library on the disk, and the games on that disk are exactly what somebody
/// wants at that moment. It was drawn as signed out — and a press on the row
/// then did nothing at all, because the worker knew perfectly well it was not
/// signed out and refused to begin a second sign-in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reach {
    /// Reaching Steam. The account and whatever library there is stand.
    Restoring,
    /// Steam is answering.
    Online,
    /// It is not, and this is what to say. Nothing has been lost: the account
    /// is still this session's, the credential is still good as far as anybody
    /// here knows, and the games on the disk still play.
    Offline(String),
}

impl Reach {
    /// Whether Steam is answering right now.
    pub fn online(&self) -> bool {
        matches!(self, Reach::Online)
    }

    /// What to say about it, or nothing while all is well.
    pub fn said(&self) -> Option<&str> {
        match self {
            Reach::Restoring => Some("Connecting to Steam"),
            Reach::Online => None,
            Reach::Offline(why) => Some(why),
        }
    }
}

/// What Steam says back.
#[derive(Debug, Clone)]
pub enum Event {
    AchievementProgress(achievements::ProgressHeard),
    Achievements(achievements::Heard),
    /// How a download is going, read off the manifest Valve's client keeps for
    /// the game. `total` is zero for the short stretch at the start where the
    /// client has written the manifest but does not yet know the size.
    ///
    /// Arrives once when the press is taken and then at every look at the
    /// disk until it is done, so a row counting up is a row saying what the
    /// client is actually doing rather than what it was asked to do.
    Installing {
        app_id: u32,
        /// What the manifest on the disk says has arrived, and of how much.
        ///
        /// Kept because it is the only source that survives a client whose
        /// interface is shut — and it is nearly always the size that is worth
        /// something here, not the count. See [`webui::Live`] for the
        /// measurement that says why.
        done: u64,
        total: u64,
        /// What Valve's client says about the download it is running now,
        /// where the shell can reach its interface and the client is running
        /// *this* game. `None` otherwise, which is an ordinary state rather
        /// than a failure.
        live: Option<webui::Live>,
    },
    /// It finished, and the game is at this path.
    Installed {
        app_id: u32,
        into: PathBuf,
    },
    /// It did not, and this is why. Whatever had been written is gone again,
    /// unless it was written over a game that was already there.
    InstallFailed {
        app_id: u32,
        why: Stopped,
    },
    /// Somebody stopped it. Nothing is left on the disk and nothing went
    /// wrong, so this is not a failure and there is nothing to tell anybody.
    InstallStopped {
        app_id: u32,
    },
    /// It has stopped moving without finishing and without failing: Steam
    /// paused it, or what arrived needs repairing before it can go on.
    ///
    /// Not a failure, and nothing to put on the screen. What it is for is
    /// ending the *counting*: the row stops saying how far a download has got
    /// and goes back to being described by the library, which now says "Download
    /// paused" or "Needs repairing" in its own words. Without it a row that
    /// stopped moving counted the same percentage for the rest of the session.
    InstallWaiting {
        app_id: u32,
    },
    /// It says it is running and has not written a byte for an hour.
    ///
    /// The one failure the manifest cannot describe. Every state that *stops*
    /// is a state Valve's client writes down — paused, needing repair, being
    /// removed — and the shell reads all of them off the disk. A client that
    /// goes on claiming to download while nothing arrives writes nothing at
    /// all, so the row counted the same percentage until the session was
    /// restarted.
    ///
    /// Not a failure and not an ending: nothing is cancelled, nothing is
    /// deleted, and the watch goes on. All it changes is what the row says,
    /// and a byte arriving takes it back. Sent once per stall rather than on
    /// every look.
    InstallStuck {
        app_id: u32,
        quiet_for: Duration,
    },
    /// One game is being taken off the disk. The row has something to say
    /// while the client works through it.
    Uninstalling {
        app_id: u32,
    },
    /// It has gone: its manifest is no longer on the disk, which is the only
    /// thing that can say so.
    Uninstalled {
        app_id: u32,
    },
    /// It has not, and this is what to tell the user.
    UninstallFailed {
        app_id: u32,
        why: String,
    },
    /// How Valve's client is getting on installing itself, and what became of
    /// it.
    ///
    /// Only ever about a setup this session asked for — see
    /// [`Ask::SetUpTheClient`] — and it arrives while the session is signed
    /// out, which nothing else here does. It is not addressed by a [`Ticket`]
    /// and does not need to be: there is one setup on a machine, ever, and
    /// nothing is waiting behind it.
    Setup(setup::SetUp),
    /// A code to photograph, and the URL it stands for.
    ///
    /// Arrives again whenever Steam rotates the code, which it does every
    /// twenty seconds or so: the panel redraws around the new one, and the old
    /// one stops working. That is Steam's rule and not this crate's.
    Challenge {
        code: qr::Code,
        url: String,
    },
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
    /// One thing Steam said about a conversation.
    ///
    /// Passed through with its correlation intact rather than unpacked here:
    /// what is stale is decided against the conversation store the shell holds,
    /// which is the only thing that knows which requests are still wanted. See
    /// [`chat::Conversations::heard`].
    Chat(chat::Heard),
    /// The account is signed in, but its catalogue could not be refreshed.
    /// The last good library and credential remain intact.
    LibraryUnavailable(String),
    /// There is no session: either there never was one, or the user signed
    /// out, or Steam stopped accepting the stored token.
    SignedOut,
    /// How Valve's background client is getting on. Arrives unasked whenever
    /// it changes, so a shell that is waiting on it has something to draw.
    ///
    /// Only ever about a wake this session asked for — see [`Ask::WakeClient`].
    /// A press that hands the client a URL is answered by
    /// [`Event::HandOverRefused`] and never by this.
    ///
    /// **One of these per [`Ask::WakeClient`], always, and addressed.** A wake
    /// that finds its ground gone still answers, under the number it was asked
    /// with, so that nothing above is left waiting on a request that will never
    /// come back — and a second press made while a wake is already running gets
    /// an answer of its own rather than the running wake's. See [`Ticket`].
    Client {
        ticket: Ticket,
        report: ClientReport,
    },
    /// What became of a client this session asked to shut down.
    ///
    /// One per [`Ask::CloseClient`], always. It is not a failure report — three
    /// of the four answers are ordinary — and it is not the answer to *did it
    /// go*, which only the machine can give afterwards. It is the answer to
    /// whether the client was even asked, which is what decides whether it is
    /// worth waiting to see. See [`client::Closing`].
    ClientClosing {
        ticket: Ticket,
        how: client::Closing,
    },
    /// How far away Steam is. Arrives whenever it changes, and once at
    /// startup for a session that has a stored account to restore.
    Reach(Reach),
    /// When the library on screen was last read from Steam, for a session that
    /// is showing one it remembered rather than one it just fetched.
    ///
    /// `None` once Steam has answered: a library that is current does not need
    /// dating, and a row that said "as of now" on every frame would be noise.
    LibraryAsOf(Option<std::time::SystemTime>),
    /// Who the account knows, and where each of them is.
    ///
    /// Arrives unasked, whenever Steam says somebody has moved — signed in,
    /// started a game, changed their name or their picture. The first one lands
    /// a second or two after the sign-in and carries the whole list; the ones
    /// after it carry the same list with one row different, because the whole
    /// of it is what is drawn.
    ///
    /// Nothing is sent for a session that is signed out, and nothing is
    /// remembered between sessions: a roster is a statement about this minute,
    /// and a remembered one would say a friend is in a game they left last
    /// week. See [`friends`].
    Friends(Roster),
    /// A `steam:` URL the shell asked to be handed over was not.
    ///
    /// Never about a game launch. Every one of these belongs to a press that
    /// promised a window of Steam's own — Open Steam, Install with Steam,
    /// Verify — and the whole of what the shell does with it is say so, because
    /// a promised window that never arrives with nothing said reads as a press
    /// that was ignored.
    HandOverRefused(Refused),
    /// And one that landed: Valve's client took the URL, and whatever it raises
    /// for it is about to appear.
    ///
    /// **The moment sight is given**, which is why this exists at all. Sight
    /// used to be given at the press, on the argument that what the wait was
    /// for was Steam's own window and it should be watched arriving. That holds
    /// where the client is up and is already this account's, which is nearly
    /// every press — and it is exactly wrong on the press that is not: a wake
    /// that has to sign the client in first spends those seconds with the
    /// client's *login screen* on the display, which is the one window of
    /// Valve's this shell exists to keep off it. Given here instead, the warm
    /// press is unchanged to the eye and the cold one shows nothing until there
    /// is something worth showing.
    ///
    /// Sent for every hand-over that succeeded, by either route, so the shell
    /// need not know which one a press took — only whether the client had to
    /// be signed in on the way, which changes what the shell does with the
    /// sight it gives. See [`HandedOver`].
    HandedOver(HandedOver),
    /// The launch that was asked for has stopped, and needs a person.
    ///
    /// Said once per launch, and only about the one this session asked to be
    /// watched. See [`Steam::watch_this_launch`].
    LaunchIsAsking(Asking),
    /// And a launch it has given up on, said once. See [`LaunchRefused`].
    LaunchWasRefused(LaunchRefused),
    /// And one it has stopped on to compile shaders, said as it moves. See
    /// [`Shaders`].
    LaunchIsWorkingOnIt(Step),
    /// Which compatibility tools Steam offers for one title — or for every
    /// unverified title — and which of them it is set to use.
    ///
    /// Arrives for a menu that asked, and again after one of its rows was
    /// pressed: a choice is read back rather than assumed, so a tool Steam
    /// declined to take does not leave a tick beside it.
    Compatibility {
        which: webui::Which,
        said: webui::Compatibility,
    },
    /// It could not be asked, and this is what to say on the panel that is
    /// waiting for the list. Never silent: the press promised a list.
    CompatibilityUnavailable {
        which: webui::Which,
        why: String,
    },
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
    /// What the client's own log says it has in hand, read forward from where
    /// the last look stopped. See [`client::Jobs`].
    jobs: client::Jobs,
    /// The number the next asynchronous request is given.
    ///
    /// One counter for all of them rather than one per kind, so that a number
    /// in the log names exactly one thing that was asked for. It never wraps in
    /// any session anybody will ever run: a session that asked for a wake every
    /// second would need six hundred billion years to reach the end of it.
    next_request: AtomicU64,
}

impl Steam {
    /// Start the worker, which restores whatever session is on the disk.
    pub fn start() -> Steam {
        // Before anything else, and it is about the run before this one: a
        // session that was killed between opening Valve's debugging marker and
        // taking it back leaves the file behind, and from then on every Steam
        // that user starts comes up exposing its whole interface on the
        // loopback interface. Nothing else will ever remove it, because every
        // later `expose` finds it already there and correctly decides it is not
        // its to touch. See [`webui::withdraw_what_was_left_behind`].
        webui::withdraw_what_was_left_behind();
        // And then say what the machine is actually doing, once, because until
        // now nothing did. A marker somebody else made is not this shell's to
        // remove and never will be — but a session that starts with Valve's
        // whole client interface open to every program the user runs should at
        // least be a line in the log rather than a thing nobody knew. The panel
        // says the same in plain words, and carries the button.
        if let Some(backend) = backend::Backend::chosen() {
            let exposure = webui::exposure(backend.root());
            if exposure.open || exposure.marker.is_some() {
                tracing::warn!(
                    open = exposure.open,
                    marker = ?exposure.marker,
                    "Valve's client interface is exposed on the loopback interface"
                );
            }
        }
        let (asks, take_asks) = mpsc::channel();
        let (send_events, events) = mpsc::channel();
        let worker = asks.clone();
        std::thread::spawn(move || work(&take_asks, &worker, &send_events));
        Steam {
            asks: Some(asks),
            events: Some(events),
            client_at: client::Where::find(),
            jobs: client::Jobs::default(),
            games: Vec::new(),
            next_request: AtomicU64::new(1),
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
            jobs: client::Jobs::default(),
            next_request: AtomicU64::new(1),
        }
    }

    /// Where Valve's client is, if it is anywhere. `None` is a machine on
    /// which nothing in this integration works, and the shell says so once
    /// rather than failing at every press.
    pub fn client_at(&self) -> Option<&client::Where> {
        self.client_at.as_ref()
    }

    /// Whether a Steam client is running on this machine right now.
    ///
    /// Asked because a manifest is a record of what Valve's client *was* doing
    /// and not of what is happening. The `StateFlags` stay exactly as they were
    /// left, so a client stopped in the middle of an update leaves a manifest
    /// that goes on saying "update started" for as long as nobody starts Steam
    /// again — and a shell that read it without asking this drew a row updating
    /// for the whole of a session in which nothing was being fetched at all.
    ///
    /// Any client, not this session's and not this account's: what is being
    /// asked is whether there is a Steam on this machine that could be doing
    /// the work, and one drawing into another session is still fetching into
    /// the same directories.
    ///
    /// Cheap enough for the interval the shell already rechecks the client on —
    /// a FIFO, a directory entry and the tail of a log. See [`client::state`].
    pub fn client_is_running(&self) -> bool {
        let Some(where_it_is) = self.client_at.as_ref() else {
            return false;
        };
        let Some(options) = client::Options::for_client(where_it_is) else {
            return false;
        };
        client::is_running(Some(where_it_is), &options)
    }

    /// Whether the Steam client running on this machine is signed in to
    /// anybody.
    ///
    /// Asked for one decision the shell makes about the client's windows: a
    /// window of a client that is signed in to nobody is never a question for
    /// the person at the screen. Such a client has exactly one thing to show
    /// — its own login screen — and that screen is the one window of Valve's
    /// this shell exists to keep off the display, because the shell has a
    /// sign-in of its own and hands the client the credential itself. See
    /// `Shell::sync_steam_questions`.
    ///
    /// Read from the tail of the client's connection log — see
    /// [`client::state`] — which is the run before's for the first second or
    /// two of a client's life. Fine for a question that is asked of a window
    /// that has stood for a minute, on the interval the shell already rechecks
    /// the client on.
    pub fn client_is_signed_in(&self) -> bool {
        let Some(where_it_is) = self.client_at.as_ref() else {
            return false;
        };
        let Some(options) = client::Options::for_client(where_it_is) else {
            return false;
        };
        client::state(Some(where_it_is), &options).signed_in()
    }

    /// Read the client's own account of what it has in hand, and say whether
    /// the answer moved.
    ///
    /// The third source about a game, and the one that answers where the other
    /// two are silent: a file check moves no bytes, so no manifest describes it
    /// and no job of this session's is watching it. See [`client::Jobs`], which
    /// is where the reading is done and why.
    ///
    /// Forgotten where no client is running, and that is the whole of the guard
    /// the caller needs: a log is a record of what the client *was* doing,
    /// exactly as a manifest is, and with nothing running there is nothing
    /// doing it. See [`crate::library::Game::update_outstanding`] and the rule
    /// it belongs to.
    pub fn look_at_what_valve_is_doing(&mut self) -> bool {
        let Some(where_it_is) = self.client_at.clone() else {
            return self.jobs.forget();
        };
        let Some(options) = client::Options::for_client(&where_it_is) else {
            return self.jobs.forget();
        };
        if !client::is_running(Some(&where_it_is), &options) {
            return self.jobs.forget();
        }
        self.jobs.look(&options.root)
    }

    /// And what that account says, as of the last look.
    pub fn what_valve_is_doing(&self) -> &client::Jobs {
        &self.jobs
    }

    /// Ask the machine again, and say whether the answer moved.
    ///
    /// The answer is read once at startup because the shell asks it while
    /// building a menu, and a `PATH` walk per row of a library is a `PATH`
    /// walk per row of a library. What that misses is somebody removing Steam
    /// while the session runs: every worker path finds out at once, because
    /// each looks the client up for itself, but the rows and the menus went on
    /// offering Play until the shell was restarted. The offers were answered
    /// honestly when pressed — nothing here was ever silent — and they were
    /// still offers for something that had gone.
    ///
    /// Never on a handle with no worker: [`Steam::settled`] is a session that
    /// has turned Steam off and the library fixtures behind `--debug-steam-library`,
    /// and neither may acquire a client from the machine it happens to run on.
    pub fn recheck_client(&mut self) -> bool {
        if self.asks.is_none() {
            return false;
        }
        let now = client::Where::find();
        if now == self.client_at {
            return false;
        }
        tracing::info!(before = ?self.client_at, after = ?now, "Valve's client has come or gone");
        self.client_at = now;
        true
    }

    /// Watch the launch that has just been asked for, and say if it stops to
    /// ask something.
    ///
    /// A launch is otherwise watched by waiting for the game's window, which
    /// answers the question "has it started" and nothing else. It cannot tell a
    /// game that is slow from one that will never come — and the difference is
    /// routine: Valve's client stops a launch on a save the cloud disagrees
    /// with, an agreement, a launch option, and asks in a window this shell is
    /// holding off the screen. Waited on to the end of its patience, that is a
    /// loading screen that runs out and a panel saying the game did not start.
    ///
    /// One watch per game, and as many at once as there are games in flight.
    /// It used to be one for the session, which was true of the press waiting
    /// on the client — that gate is one at a time — and false of the launches
    /// past it: the gate is released the moment the client is asked, so a
    /// second game can be pressed on the second screen while the first is
    /// still opening. Starting that second watch stopped the first, and ending
    /// either loading screen stopped both. What that costs is the failure this
    /// whole thing exists for, on the game that did not happen to be last: the
    /// client stops to ask about a cloud save, nobody is listening, and the
    /// loading screen waits out its patience and says the game did not start.
    ///
    /// Ended with [`Self::stop_watching_the_launch`], which every path out of a
    /// launch goes through, and which names the game it is about.
    ///
    /// Returns the number this watch was started under. What comes back
    /// carries it — see [`Asking::request`] — so a watcher that speaks about a
    /// launch this shell has already let go of can be told from the one it is
    /// listening for.
    pub fn watch_this_launch(&self, app_id: u32) -> u64 {
        let request = self.next_request();
        self.ask(Ask::WatchLaunch { app_id, request });
        request
    }

    /// Stop watching one game's launch.
    pub fn stop_watching_the_launch(&self, app_id: u32) {
        self.ask(Ask::StopWatchingLaunch {
            app_id: Some(app_id),
        });
    }

    /// And stop watching all of them, for the one thing that ends every launch
    /// at once: the account they were started under signing out.
    pub fn stop_watching_every_launch(&self) {
        self.ask(Ask::StopWatchingLaunch { app_id: None });
    }

    /// Carry one answer back to the launch Valve's client stopped.
    ///
    /// The whole point of [`Asking::question`]: the choice was made on this
    /// shell's own panel, which a thumbstick can reach, and this is what the
    /// client's own dialog would have called had somebody found a mouse.
    pub fn answer_the_launch(&self, action_id: u32, carry: webui::Carry) {
        self.ask(Ask::AnswerLaunch { action_id, carry });
    }

    /// Ask Valve's client for one thing to be done to one title, as a `steam:`
    /// URL.
    ///
    /// The refusal this returns is the one that can be answered *now*: there is
    /// no client on this machine, so there is nothing to ask and the press has
    /// to say so on the frame it was made. Everything else happens on the
    /// worker, because the alternative is starting a process from the render
    /// thread — and where there is no client running yet, the process that
    /// carries a `steam:` URL is the client, which does not exit until the user
    /// quits Steam. Waited for from the loop that draws, that is a shell frozen
    /// on its last frame with Steam audible behind it.
    ///
    /// A failure the worker sees arrives as [`ClientReport::Unavailable`].
    pub fn tell(&self, app_id: u32, doing: Doing) -> Result<(), String> {
        if self.client_at.is_none() {
            return Err("There is no Steam client installed on this machine.".to_string());
        }
        self.ask(Ask::Tell { app_id, doing });
        Ok(())
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

    /// Whether Valve's client has yet to install itself on this machine.
    ///
    /// Two directory tests, so it is a question the thread that draws may ask —
    /// and it is asked there, once, at the moment somebody presses the sign-in
    /// row, to decide which panel goes up. See [`setup`].
    pub fn client_needs_setting_up(&self) -> bool {
        match self.client_at.as_ref() {
            Some(client) => {
                client::Options::for_client(client).is_some_and(|options| setup::needed(&options))
            }
            // No Steam at all is not a Steam part-way through installing
            // itself, and the shell has its own words for a machine with no
            // client on it.
            None => false,
        }
    }

    /// Have it install itself, saying how that goes.
    ///
    /// Answered by [`Event::Setup`]: once per change while it runs, then
    /// exactly one [`setup::SetUp::Done`] or [`setup::SetUp::Failed`]. Safe to
    /// ask twice — see [`Ask::SetUpTheClient`].
    pub fn set_up_the_client(&self) {
        self.ask(Ask::SetUpTheClient);
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

    /// Ask what one title — or every unverified title — may be run under, and
    /// what it is set to run under now.
    ///
    /// Answered by [`Event::Compatibility`] or by
    /// [`Event::CompatibilityUnavailable`], and never by silence: this is
    /// asked out of a panel that is waiting for it.
    pub fn compatibility(&self, which: webui::Which) {
        self.ask(Ask::Compatibility(which));
    }

    /// Force it to run under one tool, or stop forcing one.
    ///
    /// Answered by a fresh [`Event::Compatibility`] — what Steam ended up with
    /// once it had been told — and by [`Event::CompatibilityUnavailable`]
    /// where the telling failed.
    pub fn force_compatibility(&self, which: webui::Which, tool: Option<String>) {
        self.ask(Ask::ForceCompatibility { which, tool });
    }

    /// Stop the launch Valve's client is walking for one game.
    ///
    /// For a press that has stopped waiting: the client is in the middle of
    /// something it decided to do before starting the game — an update, most
    /// often — and would open the game at the end of it, over whatever is on
    /// the screen by then. Says nothing back; see [`Ask::StopLaunch`].
    pub fn stop_launch(&self, app_id: u32) {
        self.ask(Ask::StopLaunch { app_id });
    }

    /// Ask for the library again now — after something was installed through
    /// Steam's own window, or after the user came back from it.
    pub fn refresh(&self) {
        self.ask(Ask::Refresh);
    }

    /// Fetch library achievement counts in a batch without loading every schema.
    pub fn achievement_progress(&self, app_ids: Vec<u32>, request: u64) {
        self.ask(Ask::AchievementProgress { app_ids, request });
    }

    /// Ask for this account's achievements without starting the game.
    pub fn achievements(&self, app_id: u32, request: u64) {
        self.ask(Ask::Achievements { app_id, request });
    }

    /// Ask something of a one-to-one conversation: its history, a message, or
    /// a typing notice.
    ///
    /// Answered by [`Event::Chat`] except for a typing notice, which nothing
    /// answers. See [`Ask::Chat`] for what happens to one asked while the CM
    /// is down.
    pub fn chat(&self, wanted: chat::Wanted) {
        self.ask(Ask::Chat(wanted));
    }

    /// Put the account into one of [`friends::CHOOSABLE`], because somebody
    /// asked for it on the friends panel.
    ///
    /// See [`Ask::SetStatus`]: it is remembered for the session and announced
    /// again on a reconnect, and it is the only thing this crate writes about
    /// the account.
    pub fn set_status(&self, status: friends::Presence) {
        self.ask(Ask::SetStatus(status));
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
    ///
    /// A client that is running and is not this session's — another display
    /// session's, or another account's — is left exactly as it is and reported
    /// as [`ClientReport::SomebodyElses`]. See [`Self::take_over_the_client`].
    /// Returns the number this wake was asked under. Every answer to it
    /// carries that number back on its [`Ticket`], and whoever is waiting is
    /// expected to check it: the answer to somebody else's abandoned wake is
    /// not the answer to this press.
    pub fn wake_client(&self) -> u64 {
        let request = self.next_request();
        self.ask(Ask::WakeClient {
            take_over: false,
            request,
        });
        request
    }

    /// The same, for somebody who has been asked and said yes.
    ///
    /// The only call in this crate that may end a Steam this session did not
    /// start. It is separate from [`Self::wake_client`] so that it cannot be
    /// reached without the question having been put: a client on another
    /// display session goes, and its downloads go with it, and a client signed
    /// in to another account is signed out of that account.
    pub fn take_over_the_client(&self) -> u64 {
        let request = self.next_request();
        self.ask(Ask::WakeClient {
            take_over: true,
            request,
        });
        request
    }

    /// Ask Valve's client to shut down, if it is one this session started.
    ///
    /// For the two moments the shell has finished with it: a game has ended and
    /// the user has asked for the client not to be left running, and the Steam
    /// integration being turned off under a session that had already started
    /// one. [`Event::ClientClosing`] says what became of it — see
    /// [`Ask::CloseClient`] for why that is worth saying.
    ///
    /// It is *not* called when the session ends. A Steam left running by a
    /// shell that has exited is a Steam the user can still reach from whatever
    /// comes up next, and ending it would be this shell taking something away
    /// on its way out of the door.
    pub fn close_the_client(&self) -> u64 {
        let request = self.next_request();
        self.ask(Ask::CloseClient { request });
        request
    }

    /// The number the next request is asked under.
    ///
    /// Handed out here rather than by whoever is waiting, so that one counter
    /// covers every kind of request and no two callers can invent the same
    /// number for two different things.
    fn next_request(&self) -> u64 {
        self.next_request.fetch_add(1, Ordering::Relaxed)
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
    /// One word from the thread installing Valve's client for the first time.
    ///
    /// Routed through the worker rather than straight to the shell for the
    /// same reason a finished job is: the worker is what holds the one-at-a-
    /// time flag, and a setup that has ended has to be *let go of* as well as
    /// be reported. See [`Ask::SetUpTheClient`].
    SetUp(setup::SetUp),
    Shutdown,
}

/// How one background job ended.
///
/// Every one of them carries the account generation it was started under. A job
/// is a thread that outlives the press — an install can be most of a minute of
/// waking a cold client before it so much as asks — and the account can change
/// underneath it. Without this, an install begun by one person and finished
/// after they signed out arrived as a row counting down in somebody else's
/// library. See [`Watching::generation`].
pub(crate) enum Finished {
    Installing {
        app_id: u32,
        generation: u64,
        how: Result<(), Stopped>,
    },
    StoppedInstalling {
        app_id: u32,
        generation: u64,
        how: Result<(), String>,
    },
    Uninstalling {
        app_id: u32,
        generation: u64,
        how: Result<(), String>,
    },
    /// What Steam offers for one title, or the failure to find out.
    ///
    /// Carries the generation like the rest, and for the same reason: this
    /// waits on a client that may take a minute to come up, and a list that
    /// arrives after somebody has signed out belongs to a library that is no
    /// longer on the screen.
    Compatibility {
        which: webui::Which,
        generation: u64,
        how: Result<webui::Compatibility, String>,
    },
    /// A wake has finished, whichever way it finished.
    ///
    /// Routed through the worker rather than straight to the shell for the
    /// reason every other job here is: the worker is the one holding
    /// [`Waking`], and a wake that ends has to let the next one start as well
    /// as be reported. Its ticket is checked against the ground *now* rather
    /// than against the ground it was asked on, which is the whole point of
    /// carrying one.
    Waking {
        ticket: Ticket,
        report: ClientReport,
    },
}

impl Finished {
    /// The account generation this job belonged to.
    fn generation(&self) -> u64 {
        match self {
            Finished::Installing { generation, .. }
            | Finished::StoppedInstalling { generation, .. }
            | Finished::Uninstalling { generation, .. }
            | Finished::Compatibility { generation, .. } => *generation,
            Finished::Waking { ticket, .. } => ticket.ground,
        }
    }
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
    fetching: std::collections::BTreeMap<u32, Moved>,
    removing: std::collections::BTreeSet<u32>,
    /// Which account and which Steam these belong to, as a number that only
    /// ever goes up.
    ///
    /// Not the CM generation, which moves on every reconnect: a network blip
    /// must not throw away a download somebody is watching. This moves when the
    /// *ground under the job* moves — the account it was started for, or the
    /// Steam whose manifests say how it is going. See
    /// [`Watching::nobody_is_signed_in_now`] and
    /// [`Watching::the_backend_is_now`].
    generation: u64,
    /// The Steam these were last looked for in, so a machine whose client
    /// changes mid-session is noticed.
    backend: Option<PathBuf>,
    /// The launches being watched, by the game each is about, and the flag
    /// each watcher thread reads to know it is still wanted.
    ///
    /// A map rather than one number, because there is one of these per game in
    /// flight and not one per session: see [`Ask::WatchLaunch`]. A game watched
    /// twice replaces its own entry — the older thread is told to stop as the
    /// newer starts — and a thread that gave up on its own leaves its flag
    /// clear, which is what [`Watching::forget_finished_watches`] prunes.
    launches: HashMap<u32, Arc<AtomicBool>>,
}

/// Notice a download that says it is moving and is not, and say so once.
///
/// Called on every look at the disk, for every game this session is watching.
/// What it compares is the figure in the manifest against the last one it saw:
/// a byte arriving restarts the clock, and an hour with none crossing it is a
/// download that has stopped without anything anywhere saying so.
///
/// **Only while there is still something to fetch.** The end of a download is
/// exactly the moment `BytesDownloaded` stops changing and stays put — the
/// client has everything and is unpacking it into place, which on a large game
/// and a slow disk is a long half hour of a manifest that does not move. That
/// is not a stall, it is the part of an install that is not a download, and a
/// row that called it stuck would be wrong on every large game anybody ever
/// installed.
fn say_if_it_has_stopped_moving(
    app_id: u32,
    moved: &mut Moved,
    done: u64,
    total: u64,
    events: &Sender<Event>,
) {
    if done > moved.done {
        if moved.told {
            tracing::info!(app_id, done, "this download is moving again");
        }
        moved.done = done;
        moved.since = Instant::now();
        moved.told = false;
        return;
    }
    // Everything is here and the client is putting it in place. Not a stall.
    if done >= total {
        moved.since = Instant::now();
        return;
    }
    let quiet_for = moved.since.elapsed();
    if moved.told || quiet_for < STUCK_AFTER {
        return;
    }
    moved.told = true;
    tracing::warn!(
        app_id,
        done,
        total,
        quiet = quiet_for.as_secs(),
        "this download says it is running and has not written a byte"
    );
    audit::went(
        format_args!("install {app_id}"),
        audit::How::Refused(format!(
            "nothing has arrived for {} minutes",
            quiet_for.as_secs() / 60
        )),
    );
    let _ = events.send(Event::InstallStuck { app_id, quiet_for });
}

/// How far one download had got when it was last seen to move, and when that
/// was.
///
/// The one state that is not on the disk. Every other thing the shell says
/// about a download is read out of the game's own manifest, which is what makes
/// it true whoever started it — but "it has not moved" is a fact about *time*,
/// and the manifest of a download that stopped an hour ago looks exactly like
/// the manifest of one that stopped a second ago.
struct Moved {
    /// The last figure the manifest gave, so a byte arriving is noticed.
    done: u64,
    /// When it last changed. Set when the press is taken, so a client that
    /// never begins at all is caught by the same clock as one that stops.
    since: Instant,
    /// Whether this has already been reported as stuck, so a download that
    /// stopped an hour ago says so once rather than every ten seconds.
    told: bool,
}

impl Moved {
    fn now() -> Moved {
        Moved {
            done: 0,
            since: Instant::now(),
            told: false,
        }
    }
}

/// How long a download may claim to be moving without moving.
///
/// This is the failure the manifest cannot describe. Every state that *stops*
/// is a state Steam writes down — paused, needing repair, uninstalling — and
/// the shell reads all of them. A client that goes on saying `UpdateRunning`
/// while nothing arrives writes nothing at all, so the row counts the same
/// percentage until somebody restarts the session.
///
/// An hour, and it is a deliberate trade rather than a safe upper bound. What
/// it must not catch is a slow connection, and an hour without a single byte is
/// not a slow connection; what it costs to be wrong is a row that says a
/// download has stopped moving when it is about to move, which the next look
/// takes back. Nothing is cancelled and nothing is deleted: this only changes
/// what the row says.
const STUCK_AFTER: Duration = Duration::from_secs(60 * 60);

impl Watching {
    /// Stop one launch's watcher, or every one of them.
    ///
    /// Setting the flag is the whole of it: the thread reads it before every
    /// poll and before it speaks, so a watch stopped here says nothing more
    /// however far into a round trip it already was. The entry goes with it,
    /// so a game watched again gets a flag of its own rather than the one a
    /// dead thread is still holding.
    fn stop_watching(&mut self, app_id: Option<u32>) {
        match app_id {
            Some(app_id) => {
                if let Some(flag) = self.launches.remove(&app_id) {
                    flag.store(false, Ordering::SeqCst);
                }
            }
            None => {
                for (_, flag) in std::mem::take(&mut self.launches) {
                    flag.store(false, Ordering::SeqCst);
                }
            }
        }
    }

    /// Drop the entries whose threads have already finished.
    ///
    /// A watcher clears its own flag on the way out — it gives up on its own
    /// after [`UNTIL_A_LAUNCH_IS_SOMEBODY_ELSES`] — so an entry reading false
    /// is a watch nobody is running. Swept when a new one is filed rather than
    /// on a timer, because that is the only moment this map grows.
    fn forget_finished_watches(&mut self) {
        self.launches.retain(|_, flag| flag.load(Ordering::SeqCst));
    }

    /// Forget everything and start a new generation.
    ///
    /// Called when the account goes, whichever way it goes: signed out on
    /// purpose, or a stored credential Steam has stopped accepting. What is
    /// being forgotten is only this session's *interest* in those jobs — the
    /// download itself belongs to Valve's client, which carries on with it —
    /// and the alternative is a row in the next person's library counting up
    /// towards a game they do not own.
    fn nobody_is_signed_in_now(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        // Every launch in flight was one this account asked for, and none of
        // them is this session's to hear about any more.
        self.stop_watching(None);
        if self.fetching.is_empty() && self.removing.is_empty() {
            return;
        }
        tracing::info!(
            fetching = self.fetching.len(),
            removing = self.removing.len(),
            "the Steam account has gone; these are no longer this session's to report on"
        );
        self.fetching.clear();
        self.removing.clear();
    }

    /// Which Steam the disk is being read through now.
    ///
    /// A download is watched by looking at one root's manifests, so a machine
    /// whose Steam changes underneath a job — a Flatpak installed beside a
    /// native client, a native client removed — leaves that job waiting on a
    /// file in a directory nothing looks at any more. It would be watched for
    /// silently, for ever, with a row counting nothing.
    ///
    /// The first look is not a change: there is nothing yet to have moved.
    ///
    /// **Nor is the same directory under a new name.** A native root is named
    /// by the first of [`library::native_roots`] that looks like one, and the
    /// first two of those are symbolic links the client makes for itself on
    /// its first start — so a machine where Steam is installing itself reads
    /// its root as `~/.local/share/Steam` until the client has run, and as
    /// `~/.steam/steam` from the moment it has. Same directory, and on
    /// 2026-09-13 a job that spanned that moment — the wake that was
    /// installing the client — was refused as standing on ground that had
    /// moved. The two names are resolved before they are called different.
    fn the_backend_is_now(&mut self, root: Option<&std::path::Path>) {
        let root = root.map(std::path::Path::to_path_buf);
        if self.backend == root {
            return;
        }
        if same_directory(self.backend.as_deref(), root.as_deref()) {
            // The new spelling is kept, so the next look is the cheap compare
            // above rather than two resolutions every ten seconds.
            self.backend = root;
            return;
        }
        let before = std::mem::replace(&mut self.backend, root);
        if before.is_none() {
            return;
        }
        tracing::info!(
            was = ?before,
            now = ?self.backend,
            "the Steam this session reads has changed underneath it"
        );
        self.nobody_is_signed_in_now();
    }

    /// Whether this session has a job of its own in flight.
    ///
    /// Half of what decides how often the disk is looked at; the other half is
    /// the disk itself, because a download this session did not start is still
    /// a row somebody is watching. See [`WHILE_SOMETHING_MOVES`].
    fn anything_is_moving(&self) -> bool {
        !self.fetching.is_empty() || !self.removing.is_empty()
    }

    /// Say how far each of them has got, and forget the ones that have
    /// arrived or gone.
    fn report(
        &mut self,
        installed: &std::collections::BTreeMap<u32, library::Installed>,
        live: Option<webui::Live>,
        events: &Sender<Event>,
    ) {
        self.fetching.retain(|app_id, moved| {
            // Nothing on the disk yet: the client has taken the request and
            // has not begun writing. The press already said as much.
            let Some(on_disk) = installed.get(app_id) else {
                // Still on the same clock, though. A client that takes the
                // request and never writes a manifest at all is the same
                // failure as one that stops halfway, and it is the one that
                // leaves the least behind.
                say_if_it_has_stopped_moving(*app_id, moved, 0, u64::MAX, events);
                return true;
            };
            use library::Standing;
            match on_disk.standing {
                Standing::Ready => {
                    let _ = events.send(Event::Installed {
                        app_id: *app_id,
                        into: on_disk.path.clone(),
                    });
                    false
                }
                // Stopped without finishing and without failing: Steam paused
                // it, or what arrived turned out to need repairing. This is the
                // third answer, and it used to be silence — the row went on
                // counting a download that had stopped moving, for the rest of
                // the session, because neither of those is an arrival and
                // neither is a failure and nothing else ever said so.
                //
                // Nothing is announced. The library itself now describes the
                // state in its own words, and the row saying "Download paused"
                // is a better answer than a panel about it.
                standing if standing.waiting_for_somebody() => {
                    tracing::info!(app_id, ?standing, "this download has stopped moving");
                    let _ = events.send(Event::InstallWaiting { app_id: *app_id });
                    false
                }
                _ => {
                    // The manifest's two byte counts belong to the last thing
                    // Steam did with the app unless it is doing one of the
                    // things they measure — see [`library::Standing::counting_bytes`].
                    // An install passes through states where they do not (Steam
                    // verifies what is already there on its way in), and a row
                    // that read them anyway would print the previous
                    // operation's percentage in the middle of this one.
                    let (done, total) = match on_disk.standing.counting_bytes() {
                        true => (on_disk.downloaded, on_disk.to_download),
                        false => (0, 0),
                    };
                    let _ = events.send(Event::Installing {
                        app_id: *app_id,
                        done,
                        total,
                        // Only where the client is talking about *this* game.
                        // There is one download at a time, and a row must not
                        // count somebody else's.
                        live: live.filter(|live| live.app_id == *app_id),
                    });
                    say_if_it_has_stopped_moving(
                        *app_id,
                        moved,
                        on_disk.downloaded,
                        on_disk.to_download,
                        events,
                    );
                    true
                }
            }
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

/// What the worker keeps across messages.
///
/// One record rather than four arguments threaded through everything, because
/// they are one thing: the bookkeeping this worker does on its own behalf,
/// which no caller outside it ever sees and which every step of it may touch.
struct Ledger {
    /// The games this session has asked Valve's client to move, and which
    /// account and which Steam they belong to.
    watching: Watching,
    /// One client at a time: a second press while the first is still bringing
    /// Steam up must not start a second one.
    waking: Waking,
    /// Where the session stands, as a job on a thread can read it.
    ///
    /// The same numbers as `watching.generation` and the state's own account,
    /// published after every turn of the worker's loop — see [`Ground`], which
    /// is why a job now finds out that its account has gone *before* it
    /// installs a game rather than after.
    ground: Ground,
    /// The CM generation the next connection carries, so an answer from a
    /// connection that has since been replaced can be told apart from one from
    /// the connection that is live.
    generation: u64,
    /// Where this session says the account stands, and where somebody has
    /// asked for it to be.
    ///
    /// Held here rather than in the CM session because it has to outlive one:
    /// every reconnect reads it and announces it again. See [`cm::Stands`].
    status: cm::Status,
    /// Whether the client that is running now has been handed the status it is
    /// owed.
    ///
    /// One hand-over per client run, and dropped the moment there is no client
    /// — see [`keep_the_status_in_step`], where the rule is. Deliberately not
    /// "whether the client was up last time this looked": a client that came up
    /// before this worker's first look would never have been watched arriving,
    /// and would never be told.
    told_the_client: u8,
    /// Whether Valve's client is being installed on this machine right now, by
    /// this session.
    ///
    /// One install at a time and no more, for the same reason there is one wake
    /// at a time: two of them are two launchers unpacking half a gigabyte into
    /// one directory. It is a plain flag rather than a [`Waking`] because
    /// nothing waits *behind* a first setup — the panel that asked for it is
    /// told how it goes, and a second press while it runs is somebody looking
    /// at the same panel again. See [`Ask::SetUpTheClient`].
    setting_up: bool,
}

/// The one wake that may be in flight, and everything waiting behind it.
///
/// One client at a time is the rule and it has not changed: a second press
/// while the first is still bringing Steam up must not start a second client.
/// What has changed is what a second press *gets*. It used to get nothing at
/// all — the ask was dropped on the floor — and the running wake's answer went
/// to whoever was waiting when it landed, which is how one account's abandoned
/// wake started another account's game.
///
/// So a request is never dropped and never answered on somebody else's behalf.
/// It either rides on the wake that is already running, when that wake was
/// asked on the same ground, or it waits for it and starts afterwards, when the
/// ground has moved under it. Either way exactly one [`Event::Client`] comes
/// back bearing its own number.
#[derive(Default)]
struct Waking {
    /// The wake a thread is running now: the ground it was started on, and
    /// every request riding on it.
    ///
    /// Several, because two presses a moment apart on the same account are two
    /// presses and both are owed an answer — the second must not be left
    /// waiting on a number nothing will ever say. Each gets its own copy of the
    /// same report.
    in_flight: Option<(Ticket, Vec<u64>)>,
    /// Requests asked while a wake belonging to ground that has since moved was
    /// still running, and which therefore cannot be started yet.
    ///
    /// A thread most of the way through signing a client in as one account
    /// cannot be called back, and starting a second beside it is two clients
    /// fighting over one machine. So these wait, and the wake that ends starts
    /// them. It is at most the length of however many presses somebody made
    /// during one wake.
    waiting_behind: Vec<(u64, bool)>,
}

fn work(
    messages: &Receiver<WorkerMessage>,
    worker: &Sender<WorkerMessage>,
    events: &Sender<Event>,
) {
    let wire = web::Wire::new();
    let mut ledger = Ledger {
        watching: Watching::default(),
        waking: Waking::default(),
        ground: Ground::default(),
        generation: 0,
        status: cm::Status::default(),
        told_the_client: 0,
        setting_up: false,
    };
    let mut state = restore(worker, events, &mut ledger.generation, &ledger.status);
    // Before the first message is taken, because the first message is a press
    // and a press is stamped with `watching.generation` the instant it arrives.
    // A job carrying a ticket nothing had published yet would ask about ground
    // that reads as (0, 0) and refuse itself.
    where_the_session_stands(&mut ledger, &state);

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
            Ok(WorkerMessage::Ask(ask)) => answer(ask, state, &wire, worker, events, &mut ledger),
            Ok(WorkerMessage::Cm(event)) => answer_cm(event, state, events, &mut ledger.watching),
            Ok(WorkerMessage::Done(finished)) => came_back(
                finished,
                state,
                worker,
                events,
                &mut ledger.watching,
                &mut ledger.waking,
            ),
            Ok(WorkerMessage::SetUp(word)) => {
                // The flag is let go of on either ending, and on neither is
                // there anything else to do here: nothing waits behind a
                // setup, and what it leaves behind — an installed client — is
                // read off the disk by whoever asks next.
                if !matches!(word, setup::SetUp::Working(_)) {
                    ledger.setting_up = false;
                }
                let _ = events.send(Event::Setup(word));
                state
            }
            // A download does not stop with the shell: it belongs to Valve's
            // client now, which carries on with it whether this session is
            // running or not, and finishing it is what the user asked for.
            Ok(WorkerMessage::Shutdown) => return,
            Err(RecvTimeoutError::Timeout) => advance(state, &wire, worker, events, &mut ledger),
            Err(RecvTimeoutError::Disconnected) => return,
        };
        // After every turn and not only after a sign-out, because the ground
        // moves in more ways than one and this is the single place all of them
        // pass through. One uncontended lock per message.
        where_the_session_stands(&mut ledger, &state);
    }
}

/// Publish the ground the worker is standing on, for the threads that cannot
/// see it.
///
/// Read from exactly the two values [`still_the_same_ground`] compares a
/// finished job against, so the check a thread makes while it is running and
/// the check the worker makes when it comes back are the same check asked at
/// two different times, rather than two rules that can drift apart.
fn where_the_session_stands(ledger: &mut Ledger, state: &State) {
    ledger.ground.moved_to(
        ledger.watching.generation,
        signed_in_as(state).unwrap_or_default(),
    );
}

fn restore(
    worker: &Sender<WorkerMessage>,
    events: &Sender<Event>,
    generation: &mut u64,
    status: &cm::Status,
) -> State {
    let Some(stored) = session::Stored::load() else {
        let _ = events.send(Event::SignedOut);
        return State::Out;
    };
    tracing::info!(account = %stored.account, "restoring the stored Steam CM session");

    // A status chosen at a previous run of this shell that no client has worn
    // yet. It is read before the connection is made because the connection
    // announces it — see [`cm::Stands`] — and it is read at all because the gap
    // it covers outlives the shell: choosing a status with Valve's client shut
    // down and then restarting the shell before ever starting the client used
    // to lose the choice, and the client came up wearing what it remembered.
    if let Some(owed) = session::Owed::load(stored.steam_id)
        .and_then(|owed| friends::Presence::from_number(owed.status))
    {
        tracing::info!(
            status = owed.said(),
            "a status chosen here has not reached Valve's client yet"
        );
        status
            .lock()
            .expect("the account's status is never poisoned")
            .owed = Some(owed);
    }

    // Said before Steam is reached for, and that is the whole change. This
    // session *is* signed in — it has an account and a credential — and the
    // library on the disk is playable whether or not Valve answers. Waiting for
    // CM before admitting any of it meant a machine with no network came up
    // looking signed out, with an empty column and a Sign in row that did
    // nothing when pressed: the worker knew it was not signed out and refused
    // to begin a second sign-in, so the press had no answer at all.
    let _ = events.send(Event::SignedIn(Account {
        name: stored.account.clone(),
        steam_id: stored.steam_id,
    }));
    let _ = events.send(Event::Reach(Reach::Restoring));

    // What the account owned when Steam was last asked, and what is on the
    // disk now. The second needs nothing but the disk; the first is a memory,
    // and is dated on the screen rather than passed off as current.
    let remembered = catalogue::restore(stored.steam_id);
    let owned = remembered
        .as_ref()
        .map(|restored| restored.games.clone())
        .unwrap_or_default();
    let _ = events.send(Event::LibraryAsOf(
        remembered.as_ref().map(|restored| restored.read_at),
    ));
    let _ = events.send(Event::Library(library::merge(
        owned.clone(),
        &installed_now().games,
    )));

    // `announced` and the remembered catalogue both travel into the connection,
    // so a CM that answers a moment later does not make the account disappear
    // and reappear, and does not replace a full column with an empty one while
    // it warms up.
    connect(stored, true, false, owned, worker, generation, status)
}

fn connect(
    stored: session::Stored,
    announced: bool,
    failure_to_panel: bool,
    owned: Vec<Game>,
    worker: &Sender<WorkerMessage>,
    generation: &mut u64,
    status: &cm::Status,
) -> State {
    *generation = generation.wrapping_add(1).max(1);
    let cancel = cm::start(
        stored.clone(),
        *generation,
        worker.clone(),
        Arc::clone(status),
    );
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
    ledger: &mut Ledger,
) -> State {
    let Ledger {
        watching,
        waking,
        ground,
        generation,
        status,
        told_the_client,
        setting_up,
    } = ledger;
    // A second authentication attempt must never replace a live or pending
    // account while leaving its stored credential behind. The shell UI already
    // prevents this; the library boundary enforces it too.
    if matches!(&ask, Ask::SignInWithQr | Ask::SignInWithPassword { .. })
        && !matches!(&state, State::Out)
    {
        return state;
    }
    match ask {
        Ask::SetUpTheClient => {
            set_the_client_up(setting_up, worker, events);
            state
        }
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
                //
                // Only a client this session is entitled to stop. Signing out
                // of the shell is not a reason to end a Steam that belongs to a
                // desktop session left running behind this one, and it used to
                // be: whatever answered the pipe was shut down, download and
                // all. See [`client::stop_if_ours`].
                //
                // **And it has to be gone before the files below are
                // written.** `-shutdown` returns when the client has been
                // asked, not when it has finished; what it does on its way out
                // is write its configuration back — the account list and the
                // credential cache among it — so a sign-out that edited them
                // while it was still going would be a sign-out the client
                // overwrote. See [`client::UNTIL_IT_STOPS`].
                if let Some(where_it_is) = client::Where::find() {
                    if let Some(options) = client::Options::for_client(&where_it_is) {
                        client::stop_if_ours(&where_it_is, &options);
                        let deadline = Instant::now() + client::UNTIL_IT_STOPS;
                        while client::is_running(Some(&where_it_is), &options) {
                            if Instant::now() >= deadline {
                                tracing::info!(
                                    "Valve's client is still running;                                      signing it out on the disk anyway"
                                );
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(250));
                        }
                    }
                }
                // Wherever a client on this machine keeps it, and whether or
                // not there is one to keep it: this is about the files, not
                // about the client, and the machine this matters most on is
                // the one Steam has been taken off. See
                // [`client::Options::every_layout`].
                for options in client::Options::every_layout() {
                    client::account::sign_out(&options.root, &options.home, &stored.account);
                }
            }
            session::Stored::forget();
            // Nothing secret in either, and still nobody's business once the
            // account they belong to has gone. The record of what was driven
            // through Valve's client goes with the account that drove it.
            catalogue::forget();
            audit::forget();
            // And then one line into the empty record, so that a machine
            // somebody signed out of says so rather than saying nothing. It is
            // the only line that survives the forgetting, and it holds nothing
            // about who signed out.
            audit::went("sign out", audit::How::Done);
            watching.nobody_is_signed_in_now();
            let _ = events.send(Event::SignedOut);
            let _ = events.send(Event::Library(Vec::new()));
            State::Out
        }
        Ask::Install { app_id } => {
            let State::In { stored, .. } = &state else {
                let _ = events.send(Event::InstallFailed {
                    app_id,
                    why: Stopped::Failed(format!(
                        "{}, so it cannot fetch this game.",
                        out_of_reach(&state)
                    )),
                });
                return state;
            };
            // And the other half of the same question, which this session's own
            // connection cannot answer: a client in Valve's Offline Mode
            // refuses every download there is, and says so itself — "Games
            // cannot be installed when Steam is in Offline Mode." Asked here
            // rather than found out at the end of a wizard, which is a row
            // saying "Installing…" with nothing coming down.
            if the_client_is_offline() {
                let _ = events.send(Event::InstallFailed {
                    app_id,
                    why: Stopped::Failed(
                        "Steam is in Offline Mode, so it cannot fetch this game.".to_string(),
                    ),
                });
                return state;
            }
            // A second press on a game that is already coming down does
            // nothing at all, rather than asking the client twice.
            if watching.fetching.contains_key(&app_id) {
                return state;
            }
            // The clock starts here rather than at the first manifest, so a
            // client that takes the request and never writes anything is
            // caught by the same rule as one that stops halfway.
            watching.fetching.insert(app_id, Moved::now());
            // Something on screen at once: waking a cold client is most of a
            // minute, and a press that appeared to do nothing for that long is
            // a press somebody makes again.
            let _ = events.send(Event::Installing {
                app_id,
                done: 0,
                total: 0,
                // Nothing to be live about yet: the client has not been asked.
                live: None,
            });
            audit::asked(format_args!("install {app_id}"));
            let generation = watching.generation;
            let ticket = a_job_about(&state, watching);
            in_the_background(stored, ticket, ground, worker, move |ready| {
                Finished::Installing {
                    app_id,
                    generation,
                    how: match ready {
                        Err(why) => Err(Stopped::Failed(why)),
                        Ok(standing) => match webui::install(app_id, standing.still()) {
                            Ok(()) => {
                                tracing::info!(app_id, "Valve's client is fetching this game");
                                Ok(())
                            }
                            Err(webui::Problem::Asks(what)) => {
                                tracing::info!(app_id, %what, "this game cannot be fetched silently");
                                Err(Stopped::Asks(what))
                            }
                            // Not a failure either, and answered the same way: the
                            // window that can still do it.
                            Err(problem @ webui::Problem::Renamed(_)) => {
                                tracing::warn!(app_id, %problem, "this Steam cannot be driven from here");
                                Err(Stopped::NotFromHere(problem.to_string()))
                            }
                            Err(problem) => Err(Stopped::Failed(problem.to_string())),
                        },
                    },
                }
            });
            state
        }
        Ask::StopInstalling { app_id } => {
            if let State::In { stored, .. } = &state {
                audit::asked(format_args!("stop installing {app_id}"));
                let generation = watching.generation;
                let ticket = a_job_about(&state, watching);
                in_the_background(stored, ticket, ground, worker, move |ready| {
                    Finished::StoppedInstalling {
                        app_id,
                        generation,
                        how: ready.and_then(|standing| {
                            webui::stop_installing(app_id, standing.still())
                                .map_err(|problem| problem.to_string())
                        }),
                    }
                });
            }
            state
        }
        Ask::Uninstall { app_id } => {
            let State::In { stored, .. } = &state else {
                let _ = events.send(Event::UninstallFailed {
                    app_id,
                    why: format!("{}, so it cannot remove this game.", out_of_reach(&state)),
                });
                return state;
            };
            if !watching.removing.insert(app_id) {
                return state;
            }
            let _ = events.send(Event::Uninstalling { app_id });
            audit::asked(format_args!("uninstall {app_id}"));
            let generation = watching.generation;
            let ticket = a_job_about(&state, watching);
            in_the_background(stored, ticket, ground, worker, move |ready| {
                Finished::Uninstalling {
                    app_id,
                    generation,
                    how: ready.and_then(|standing| {
                        webui::uninstall(app_id, standing.still())
                            .map_err(|problem| problem.to_string())
                    }),
                }
            });
            state
        }
        Ask::Compatibility(which) => {
            // The same test every press that names a title makes: there is no
            // library to ask about until Steam has accepted a credential, and
            // a session holding one it cannot reach still has games on the
            // disk and a client that will answer about them.
            let Some((stored, _)) = holding(&state) else {
                let _ = events.send(Event::CompatibilityUnavailable {
                    which,
                    why: format!("{}, so it cannot say what runs this.", out_of_reach(&state)),
                });
                return state;
            };
            let generation = watching.generation;
            let ticket = a_job_about(&state, watching);
            in_the_background(stored, ticket, ground, worker, move |ready| {
                Finished::Compatibility {
                    which,
                    generation,
                    how: ready.and_then(|standing| {
                        // A read, and it is asked all the same. Nothing here
                        // outlives being wrong — the list is drawn and forgotten
                        // — but every title-specific action taking the same
                        // sequence is the point of there being a sequence, and
                        // an exception is a thing somebody later copies.
                        standing.about_to_act()?;
                        webui::compatibility(which, standing.still())
                            .map_err(|problem| problem.to_string())
                    }),
                }
            });
            state
        }
        Ask::ForceCompatibility { which, tool } => {
            let Some((stored, _)) = holding(&state) else {
                let _ = events.send(Event::CompatibilityUnavailable {
                    which,
                    why: format!("{}, so it cannot change this.", out_of_reach(&state)),
                });
                return state;
            };
            let said = format!(
                "run {which} under {}",
                tool.as_deref().unwrap_or("whatever Steam chooses")
            );
            audit::asked(&said);
            let generation = watching.generation;
            let ticket = a_job_about(&state, watching);
            in_the_background(stored, ticket, ground, worker, move |ready| {
                // The standing is carried *past* the change rather than spent
                // on it, because the read-back below is a second call and has
                // to be a second call at the same client: a list read off a
                // replacement is a tick drawn beside a row nobody set.
                let told = ready.and_then(|standing| {
                    webui::force(which, tool.as_deref(), standing.still())
                        .map_err(|problem| problem.to_string())
                        .map(|()| standing)
                });
                // Closed here rather than where the answer is handled, because
                // this is the half that changed something: what follows is a
                // second look at the client, and a list that failed to come
                // back is not a choice that failed to be made.
                audit::went(
                    &said,
                    match &told {
                        Ok(_) => audit::How::Done,
                        Err(why) => audit::How::Failed(why.clone()),
                    },
                );
                Finished::Compatibility {
                    which,
                    generation,
                    // Read back rather than assumed. Telling the client and
                    // asking it are two calls on one open socket, and the
                    // second is what the tick beside the row is drawn from —
                    // so a choice Steam quietly did not take shows as the
                    // choice Steam actually has.
                    how: told.and_then(|standing| {
                        standing.about_to_act()?;
                        webui::compatibility(which, standing.still())
                            .map_err(|problem| problem.to_string())
                    }),
                }
            });
            state
        }
        Ask::WakeClient { take_over, request } => {
            take_a_wake(request, take_over, &state, waking, watching, worker, events);
            state
        }
        Ask::Tell { app_id, doing } => {
            // **What decides this is whether this session holds a credential,
            // and not what the URL is about.** It used to be the other way
            // round: everything naming a title went to a client this session
            // had woken and proved, and everything about the client itself —
            // Open Steam, Big Picture, the downloads list — went to whatever
            // client happened to be holding the pipe. The argument was that
            // Steam's own window is account-neutral, and it is; what it missed
            // is that the window a *signed-out* client raises is its own login
            // screen.
            //
            // So on 2026-09-04, on a machine where this shell had just
            // installed Steam and signed itself in by photographed code, Open
            // Steam produced Valve's sign-in window: account name, password,
            // and a QR code of Steam's own. The library was on the bar the
            // whole time. Nothing had ever handed the client the credential,
            // because the one path that does is the wake this row went around.
            let holding = holding(&state);
            match (route(holding.is_some(), doing.about_the_client()), holding) {
                // Signed in — and that now includes the client's own windows.
                // A session holding a credential has exactly one right answer
                // for "open Steam", which is *their* Steam, signed in; a wake
                // is the only thing that produces one, and on a client that is
                // already up and already theirs it costs microseconds.
                //
                // **Finding 4** is the other half of this and is unchanged:
                // with no client running there was nothing to inspect, so "is
                // this ours" answered yes, and `steam://install/<id>` started a
                // client from cold — which signed itself into whichever account
                // it last remembered and installed the game into *that*
                // library, before this session had proved anything at all.
                //
                // The need comes from `holding` rather than being fixed, so a
                // session that cannot reach Steam is given Valve's own Offline
                // Mode here as it is everywhere else. Never `Need::Context`: a
                // URL is carried over the pipe and needs no interface, and
                // opening one costs a client restart.
                (Route::AClientOfOurs, Some((stored, need))) => hand_over_to_a_proven_client(
                    stored,
                    a_job_about(&state, watching),
                    ground,
                    need,
                    doing,
                    doing.url(app_id),
                    events,
                ),
                // Nobody signed in, and Steam's own window asked for. This is
                // the case the old rule was written for and it is kept whole:
                // that window is where somebody signs into Steam *itself*,
                // this session has no credential to offer instead, and where
                // there is no client at all it starts one. It is the only row
                // on the bar that works with nobody signed in.
                (Route::WhoeverIsThere, _) => hand_over(None, doing, doing.url(app_id), events),
                // `Nobody`, and the unreachable pairing that says the two halves
                // agree: `AClientOfOurs` is only ever chosen for a session that
                // is holding one.
                (Route::Nobody, _) | (Route::AClientOfOurs, None) => {
                    let _ = events.send(Event::HandOverRefused(Refused::Failed(format!(
                        "{}, so it cannot be asked about a game.",
                        out_of_reach(&state)
                    ))));
                }
            }
            state
        }
        Ask::AnswerLaunch { action_id, carry } => {
            // Answering a launch starts — or ends — somebody's game, so it is
            // one of the actions that must know whose client it is talking to.
            let Some((stored, _)) = holding(&state) else {
                let _ = events.send(Event::HandOverRefused(Refused::Failed(format!(
                    "{}, so it cannot answer this.",
                    out_of_reach(&state)
                ))));
                return state;
            };
            audit::asked(format_args!("answer launch {action_id} with {carry:?}"));
            let ticket = a_job_about(&state, watching);
            let stored = stored.clone();
            let ground = ground.clone();
            let events = events.clone();
            std::thread::spawn(move || {
                // Proved, never woken. There is a launch walking in a client
                // that is already up; a wake that decided it had to restart one
                // to open its port would end the very game this is about. See
                // [`HaveAClient::AsItStands`].
                let standing = match standing_on(&stored, ticket, &ground, HaveAClient::AsItStands)
                {
                    Ok(standing) => standing,
                    Err(refusal) => {
                        tracing::info!(%refusal, "not answering a launch at a client that is not this session's");
                        let _ = events.send(Event::HandOverRefused(refused_by(refusal)));
                        return;
                    }
                };
                if let Err(problem) = webui::answer_the_launch(action_id, &carry, standing.still())
                {
                    tracing::warn!(%problem, "the launch could not be answered");
                    // The one thing worth saying: the panel has gone, the
                    // person believes they answered, and nothing happened. The
                    // client's own window is where it can still be answered.
                    let _ = events.send(Event::HandOverRefused(Refused::Failed(format!(
                        "Steam would not take that answer: {problem}"
                    ))));
                }
            });
            state
        }
        Ask::CloseClient { request } => {
            // On the worker's own thread rather than a new one: `-shutdown` is
            // a process spawned and waited on, and it returns as soon as the
            // client has been *asked*. Nothing after this depends on it, and
            // the one thing that must not happen — the shell's own thread
            // waiting on a process — cannot, because this is not that thread.
            //
            // Only ever a client this session started. See
            // [`client::stop_if_ours`], which is the same rule signing out is
            // under and for the same reason: a Steam belonging to a desktop
            // session left running behind this one is not this shell's to end,
            // download and all.
            //
            // And the answer goes back up, which it did not use to. A client
            // that belongs to another session is not asked and cannot be
            // talked round; the shell above waited two grace periods over one
            // and then reported that it would not shut down.
            let how = client::Where::find()
                .and_then(|where_it_is| {
                    let options = client::Options::for_client(&where_it_is)?;
                    Some(client::stop_if_ours(&where_it_is, &options))
                })
                .unwrap_or(client::Closing::Gone);
            if how == client::Closing::Asked {
                audit::went("close the client", audit::How::Done);
            }
            let _ = events.send(Event::ClientClosing {
                ticket: ticket_for(request, &state, watching),
                how,
            });
            state
        }
        Ask::StopLaunch { app_id } => {
            // Cancelling a launch is the other half of answering one, and takes
            // the same proof for the same reason.
            let Some((stored, _)) = holding(&state) else {
                return state;
            };
            let ticket = a_job_about(&state, watching);
            let stored = stored.clone();
            let ground = ground.clone();
            // On a thread, because it is two round trips into the client and
            // the worker has a library to go on reading. Nothing waits on it.
            std::thread::spawn(move || {
                // Proved and never woken, on [`HaveAClient::AsItStands`]'s terms: the
                // launch being stopped is inside a client that is already up.
                let standing = match standing_on(&stored, ticket, &ground, HaveAClient::AsItStands)
                {
                    Ok(standing) => standing,
                    Err(refusal) => {
                        tracing::info!(app_id, %refusal, "that launch is not this session's to stop");
                        return;
                    }
                };
                // Which action it is has to be asked for: the shell holds an id
                // only for a launch that stopped to ask something, and this one
                // did not stop — it is getting on with an update nobody wants
                // to wait out.
                let Ok(actions) = webui::launching() else {
                    return;
                };
                let Some(walking) = actions.into_iter().find(|one| one.app_id == app_id) else {
                    tracing::info!(app_id, "Valve's client has no launch left to stop");
                    return;
                };
                match webui::answer_the_launch(
                    walking.action_id,
                    &webui::Carry::Stop,
                    standing.still(),
                ) {
                    Ok(()) => tracing::info!(app_id, "the launch nobody is waiting for is off"),
                    Err(problem) => {
                        tracing::info!(app_id, %problem, "that launch could not be stopped")
                    }
                }
            });
            state
        }
        Ask::WatchLaunch { app_id, request } => {
            // A flag of this watch's own, and the one it replaces is cleared as
            // it is handed over: a game watched twice must not leave the first
            // thread still sending events about a press that has been answered.
            // Only *that* game's, though — another display's launch is another
            // watch and none of this one's business.
            watching.forget_finished_watches();
            watching.stop_watching(Some(app_id));
            let running = Arc::new(AtomicBool::new(true));
            watching.launches.insert(app_id, running.clone());
            watch_the_launch(app_id, request, running, events);
            state
        }
        Ask::StopWatchingLaunch { app_id } => {
            watching.stop_watching(app_id);
            state
        }
        Ask::AchievementProgress { app_ids, request } => {
            let sent = if let State::In { commands, .. } = &state {
                commands
                    .send(cm::Command::AchievementProgress { app_ids, request })
                    .is_ok()
            } else {
                false
            };
            if !sent {
                let _ = events.send(Event::AchievementProgress(achievements::ProgressHeard {
                    generation: *generation,
                    account: signed_in_as(&state).unwrap_or_default(),
                    request,
                    result: Err("Steam is offline.".into()),
                }));
            }
            state
        }
        Ask::Achievements { app_id, request } => {
            let sent = if let State::In { commands, .. } = &state {
                commands
                    .send(cm::Command::Achievements { app_id, request })
                    .is_ok()
            } else {
                false
            };
            if !sent {
                let account = signed_in_as(&state).unwrap_or_default();
                let events = events.clone();
                let generation = *generation;
                std::thread::spawn(move || {
                    let result = achievements::finish(
                        account,
                        app_id,
                        Err("Steam is offline. Reopen this game to retry.".into()),
                    );
                    let _ = events.send(Event::Achievements(achievements::Heard {
                        generation,
                        account,
                        app_id,
                        request,
                        result,
                    }));
                });
            }
            state
        }
        Ask::Chat(wanted) => {
            // Only where there is a connection to carry it. A request made
            // while the CM is down has nowhere to go and is not kept: see
            // [`Ask::Chat`], where the reason a message is never queued is
            // written down. The panel has already refused the press; this is
            // the race between the two, and losing it is a send that fails
            // rather than one that goes late.
            if let State::In { commands, .. } = &state {
                let _ = commands.send(cm::Command::Chat(wanted));
            } else if let chat::Wanted::Send { with, request, .. } = wanted {
                // A send is answered even so, because there is a line on the
                // screen waiting to hear about it. A history fetch and a typing
                // notice are not: neither has anything drawn that would be left
                // waiting.
                let account = signed_in_as(&state).unwrap_or_default();
                let _ = events.send(Event::Chat(chat::Heard {
                    generation: *generation,
                    account,
                    word: chat::Word::Sent {
                        with,
                        request,
                        said: Err(chat::Refused::NotConnected.said().to_string()),
                    },
                }));
            }
            state
        }
        Ask::SetStatus(chosen) => {
            // Written down first and sent second, and the order is the whole
            // of it: a status chosen while the CM is down has nowhere to go
            // now, and the next connection is the one that announces it. To the
            // disk as well as to memory, because the client it is owed to may
            // not be started until after the next reboot. See [`cm::Stands`]
            // and [`session::Owed`].
            {
                let mut stands = status
                    .lock()
                    .expect("the account's status is never poisoned");
                stands.owed = Some(chosen);
                stands.announced = Some(chosen);
                stands.offline_record = chosen
                    .is_away_from_it_all()
                    .then(|| signed_in_as(&state).and_then(status_on_this_machine))
                    .flatten();
            }
            if let Some(steam_id) = signed_in_as(&state) {
                session::Owed {
                    steam_id,
                    status: chosen.to_number(),
                }
                .save();
            }
            tracing::info!(status = chosen.said(), "the account's status was chosen");
            if let State::In { commands, .. } = &state {
                let _ = commands.send(cm::Command::Announce {
                    presence: chosen,
                    chosen: true,
                });
            }
            // And Valve's client, where one is running. This is the half a
            // `ClientChangeStatus` cannot reach: that message is about *this
            // session*, and the client beside it goes on saying whatever it
            // last said — so somebody with Steam open watched the shell's panel
            // change and their own window not, which is two answers to one
            // question. See [`tell_the_client_too`].
            //
            // Where there is no client to tell, nothing happens here and
            // nothing needs to: what is owed is written down, and
            // [`keep_the_status_in_step`] hands it to the next client that
            // comes up.
            *told_the_client = u8::from(
                signed_in_as(&state).is_some_and(|steam_id| tell_the_client_too(chosen, steam_id)),
            );
            state
        }
        Ask::Refresh => {
            // Offline, this is the retry. A session waiting out its five
            // seconds between attempts had nothing anybody could press: the
            // Refresh row did nothing at all, because there was no CM to ask,
            // and the only way to try again was to wait. Bringing the next
            // attempt forward to now is the whole of what a retry is.
            if let State::Reconnecting {
                stored,
                announced,
                failure_to_panel,
                owned,
                next: _,
            } = state
            {
                tracing::info!("asked to try Steam again now");
                let _ = events.send(Event::Reach(Reach::Restoring));
                return connect(
                    stored,
                    announced,
                    failure_to_panel,
                    owned,
                    worker,
                    generation,
                    status,
                );
            }
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

/// Have Valve's client install itself, on a thread, and report the whole of it.
///
/// The odd one out among the jobs in this crate, and it is odd in exactly the
/// ways that matter. It needs no account — it is asked for by somebody who has
/// not signed in yet and could not, since there is nothing here to sign in to —
/// so it carries no [`Ticket`] and takes no [`Ground`]; and it is the only job
/// that reports *while it runs* rather than once at the end, because it runs
/// for minutes and a panel is watching it.
///
/// Refuses itself twice over, quietly, and both refusals are ordinary rather
/// than failures. A machine whose Steam is already installed has nothing to
/// install; a session already installing one is a session where somebody has
/// pressed the row a second time to look at the panel again, and the answer to
/// that is the panel, not a second launcher.
fn set_the_client_up(
    setting_up: &mut bool,
    worker: &Sender<WorkerMessage>,
    events: &Sender<Event>,
) {
    if *setting_up {
        tracing::info!("Valve's client is already being set up by this session");
        return;
    }
    let Some(client) = client::Where::find() else {
        let _ = events.send(Event::Setup(setup::SetUp::Failed(
            "Steam is not installed on this machine.".to_string(),
        )));
        return;
    };
    let Some(options) = client::Options::for_client(&client) else {
        let _ = events.send(Event::Setup(setup::SetUp::Failed(
            "There is no home directory for Steam to install itself into.".to_string(),
        )));
        return;
    };
    // Already done. Said as `Done` rather than as nothing at all: whoever asked
    // has a panel up waiting to hear, and the honest answer to "set Steam up"
    // on a machine where Steam is set up is that it is.
    if !setup::needed(&options) {
        let _ = events.send(Event::Setup(setup::SetUp::Done));
        return;
    }

    *setting_up = true;
    let worker = worker.clone();
    std::thread::spawn(move || {
        setup::run(&client, &options, |word| {
            let _ = worker.send(WorkerMessage::SetUp(word));
        });
    });
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
    ticket: Ticket,
    ground: &Ground,
    worker: &Sender<WorkerMessage>,
    work: impl FnOnce(Result<Standing, String>) -> Finished + Send + 'static,
) {
    let stored = stored.clone();
    let ground = ground.clone();
    let worker = worker.clone();
    std::thread::spawn(move || {
        // Everything that comes this way is made as a call into the client's
        // own interface rather than as a `steam:` URL, so this is where that
        // interface is worth opening — and the only place.
        //
        // What it hands the closure is no longer "did a client come up" but
        // *which* client came up and what was proved about it. The difference
        // is the whole of finding 5: a job that only knew a client had come up
        // could check nothing between that moment and the call it then made,
        // and the call is the half that cannot be taken back.
        let ready = standing_on(
            &stored,
            ticket,
            &ground,
            HaveAClient::ByWaking(client::Need::Context),
        )
        .map_err(|refusal| said_about(&refusal));
        let _ = worker.send(WorkerMessage::Done(work(ready)));
    });
}

/// The credential one stored session is, as the client module takes it.
fn credential(stored: &session::Stored) -> client::Credential<'_> {
    client::Credential {
        account: &stored.account,
        steam_id: stored.steam_id,
        refresh_token: &stored.refresh_token,
    }
}

/// Which permission a press carries.
fn permission(take_over: bool) -> client::Permission {
    match take_over {
        true => client::Permission::MayTakeOver,
        false => client::Permission::AskFirst,
    }
}

/// What a refusal says on a row that cannot ask a question.
///
/// A background job — an install, a removal — is reported on the row it was
/// pressed on, in one line, with the panel's other line already spoken for. So
/// what it gets is the half that says what is in the way; the half that says
/// what moving it would cost belongs to the panel that actually offers to move
/// it, which is the game press.
fn said_about(refusal: &client::Refusal) -> String {
    match refusal {
        client::Refusal::Failed(why) => why.clone(),
        client::Refusal::NotOurs(why) => why.what.clone(),
    }
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
    worker: &Sender<WorkerMessage>,
    events: &Sender<Event>,
    watching: &mut Watching,
    waking: &mut Waking,
) -> State {
    // A wake is answered before the check below rather than by it, because a
    // wake that came back on ground that has moved still has two things to do
    // that a stale install does not: let go of the one-at-a-time hold, and
    // start whatever was waiting behind it. It is also owed an answer either
    // way — see [`Waking`].
    if let Finished::Waking { ticket, report } = finished {
        return a_wake_landed(ticket, report, state, worker, events, watching, waking);
    }
    // A job whose account has gone says nothing to anybody. It was somebody's
    // install and they are not signed in any more; the client carries on with
    // whatever it was doing, and this session has no row left to report it on.
    // See [`Finished`].
    if finished.generation() != watching.generation {
        tracing::info!(
            was = finished.generation(),
            now = watching.generation,
            "a Steam job came back after its account had gone"
        );
        return state;
    }
    match finished {
        // Answered above, before the ground was checked: a wake has a hold to
        // let go of and a queue to start whichever way it went.
        Finished::Waking { .. } => state,
        Finished::Installing {
            app_id,
            how: Ok(()),
            ..
        } => {
            audit::went(format_args!("install {app_id}"), audit::How::Done);
            at_once(state)
        }
        Finished::Installing {
            app_id,
            how: Err(why),
            ..
        } => {
            watching.fetching.remove(&app_id);
            tracing::warn!(app_id, why = %why.said(), "a game was not fetched");
            // Refused rather than failed for the two that are not failures:
            // Steam waiting on the person, and a client this shell may no
            // longer drive. Both end in Steam's own window, which is a press
            // going somewhere rather than a press going wrong.
            audit::went(
                format_args!("install {app_id}"),
                match &why {
                    Stopped::Failed(said) => audit::How::Failed(said.clone()),
                    Stopped::Asks(said) | Stopped::NotFromHere(said) => {
                        audit::How::Refused(said.clone())
                    }
                },
            );
            let _ = events.send(Event::InstallFailed { app_id, why });
            state
        }
        Finished::StoppedInstalling { app_id, how, .. } => {
            watching.fetching.remove(&app_id);
            match how {
                Ok(()) => {
                    audit::went(format_args!("stop installing {app_id}"), audit::How::Done);
                    let _ = events.send(Event::InstallStopped { app_id });
                    at_once(state)
                }
                Err(why) => {
                    audit::went(
                        format_args!("stop installing {app_id}"),
                        audit::How::Failed(why.clone()),
                    );
                    let _ = events.send(Event::InstallFailed {
                        app_id,
                        why: Stopped::Failed(why),
                    });
                    state
                }
            }
        }
        Finished::Uninstalling {
            app_id,
            how: Ok(()),
            ..
        } => {
            audit::went(format_args!("uninstall {app_id}"), audit::How::Done);
            at_once(state)
        }
        Finished::Compatibility { which, how, .. } => {
            match how {
                Ok(said) => {
                    tracing::info!(
                        %which,
                        tools = said.tools.len(),
                        forced = said.forced.as_deref().unwrap_or("nothing"),
                        "Steam said what this runs under"
                    );
                    let _ = events.send(Event::Compatibility { which, said });
                }
                Err(why) => {
                    tracing::warn!(%which, %why, "Steam would not say what this runs under");
                    let _ = events.send(Event::CompatibilityUnavailable { which, why });
                }
            }
            state
        }
        Finished::Uninstalling {
            app_id,
            how: Err(why),
            ..
        } => {
            watching.removing.remove(&app_id);
            tracing::warn!(app_id, %why, "a game was not removed");
            audit::went(
                format_args!("uninstall {app_id}"),
                audit::How::Failed(why.clone()),
            );
            let _ = events.send(Event::UninstallFailed { app_id, why });
            state
        }
    }
}

/// Everything installed, read from the Steam this session is driving.
///
/// One line, and it exists so that no caller can accidentally ask the *machine*
/// instead of the client. A machine can have two Steams; asking it which games
/// are installed has two answers, and the wrong one is invisible. See
/// [`backend::Backend`].
fn installed_now() -> OnTheDisk {
    match backend::Backend::chosen() {
        Some(backend) => {
            backend::say_which_steam(&backend);
            OnTheDisk {
                root: Some(backend.root().to_path_buf()),
                games: library::installed_for(&backend),
            }
        }
        // No Steam on this machine at all, and nothing one left behind. Not a
        // failure: the account's own catalogue still lists, and every row in it
        // says plainly that there is nothing here to play it with.
        None => OnTheDisk::default(),
    }
}

/// What one look at the disk found, and which Steam it was looked at through.
///
/// The root travels with the games because a job in flight belongs to the Steam
/// it was started against: its progress is read out of that root's manifests,
/// and if the machine's Steam changes underneath it — a Flatpak installed, a
/// native client removed — the manifest it is waiting for is in a directory
/// nothing looks at any more. See [`Watching::the_backend_is_now`].
#[derive(Default)]
struct OnTheDisk {
    root: Option<PathBuf>,
    games: std::collections::BTreeMap<u32, library::Installed>,
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

/// Hand Valve's client one `steam:` URL, on a thread of its own.
///
/// **Only the URLs that are about the client itself** — Open Steam, Big
/// Picture, the downloads list. Everything that names a title goes through
/// [`hand_over_to_a_proven_client`] instead, which wakes a client of this
/// account rather than delivering to whatever is there. The split is finding
/// 4's: a stopped client reads as nobody's here, on purpose, because that is
/// how Steam gets opened on a machine where it has never run — and read that
/// way for a title's URL it is how an install reaches whichever account a
/// cold client remembered.
///
/// A thread even here, where the work is a fraction of a second in the ordinary
/// case: the case that is not ordinary is a machine with no client running,
/// where the process carrying the URL becomes the client and lives for as long
/// as Steam is open. [`client::open`] is what refuses to wait on that one — and
/// the thread is what keeps even the courier's half-second off the worker,
/// which is what answers the library and every other press meanwhile.
///
/// Nothing is reported on success. What the press was for is a window of
/// Steam's own, and the sign that it worked is that window.
fn hand_over(stored: Option<session::Stored>, asked: Doing, url: String, events: &Sender<Event>) {
    let events = events.clone();
    std::thread::spawn(move || {
        // The URL itself, which is what was actually handed over and is the
        // one thing a bug report about a press that went nowhere needs. It
        // names a game and an action and nothing about anybody.
        let doing = format!("hand over {url}");
        audit::asked(&doing);
        let Some(where_it_is) = client::Where::find() else {
            audit::went(
                &doing,
                audit::How::Failed("there is no Steam client on this machine".to_string()),
            );
            let _ = events.send(Event::HandOverRefused(Refused::Failed(
                "There is no Steam client installed on this machine.".to_string(),
            )));
            return;
        };
        // Its directories are how a running client is told from none, and a
        // machine that has never started one has nothing in them yet. That is
        // not a failure — see [`client::open`], which finds nothing running
        // and then starts one with the URL already in hand, rather than
        // waiting on a courier that may not be one.
        let options = client::Options::for_client(&where_it_is);

        // Whose client it is, asked here too. A `steam:` URL goes to whichever
        // client is holding the pipe, and this path used to ask nothing at all
        // about that — so Open Steam could quietly raise a window on the
        // desktop session left running behind this one, and its two
        // title-naming neighbours could act on another household account's
        // library, while every other press in this crate refused to.
        if let Some(options) = options.as_ref() {
            if let Some(why) = not_ours(&where_it_is, options, stored.as_ref()) {
                tracing::info!(%url, %why, "not handing this to a client that is not ours");
                audit::went(&doing, audit::How::Refused(why.to_string()));
                let _ = events.send(Event::HandOverRefused(Refused::SomebodyElses(why)));
                return;
            }
        }

        match client::open(&where_it_is, options.as_ref(), &url) {
            Ok(()) => {
                audit::went(&doing, audit::How::Done);
                // Nothing here signs anybody in: this is the route for a
                // session with no credential, handing the URL to whatever
                // client is there. Its login screen, if that is what comes
                // up, is the window the row is for.
                let _ = events.send(Event::HandedOver(HandedOver {
                    after_signing_in: false,
                    asked,
                }));
            }
            Err(why) => {
                tracing::warn!(%url, %why, "Valve's client would not take that");
                audit::went(&doing, audit::How::Failed(why.to_string()));
                let _ = events.send(Event::HandOverRefused(Refused::Failed(why.to_string())));
            }
        }
    });
}

/// Wake a client, prove it, and hand *that* client one `steam:` URL about a
/// title.
///
/// The title half of [`Ask::Tell`], and the whole of finding 4's fix. Every
/// difference from [`hand_over`] is deliberate:
///
/// * It **wakes** rather than delivering to whatever is there. The press means
///   "do this to the library on the screen", and the only way to make that true
///   with no client running is to start one and sign it into this account —
///   which is what every other title action already did.
/// * It **proves** the client it woke, and hands over with [`client::deliver`]
///   rather than [`client::open`], so a client that died in between is a press
///   that says so instead of a fresh Steam started blind with a title's URL in
///   its hand.
/// * It checks the ground either side of the wake, because a wake is most of
///   two minutes and an account can sign out inside one.
///
/// What it costs is the honest cost of the press: "Install with Steam" on a
/// machine with no client running now takes as long as starting Steam takes,
/// where before it returned in milliseconds and was sometimes wrong about whose
/// library it had just installed into.
fn hand_over_to_a_proven_client(
    stored: &session::Stored,
    ticket: Ticket,
    ground: &Ground,
    need: client::Need,
    asked: Doing,
    url: String,
    events: &Sender<Event>,
) {
    let stored = stored.clone();
    let ground = ground.clone();
    let events = events.clone();
    std::thread::spawn(move || {
        // The URL itself, which is what was actually handed over and is the one
        // thing a bug report about a press that went nowhere needs. It names a
        // game and an action and nothing about anybody.
        let doing = format!("hand over {url}");
        audit::asked(&doing);
        let refuse = |refusal: client::Refusal| {
            audit::went(
                &doing,
                match &refusal {
                    client::Refusal::NotOurs(why) => audit::How::Refused(why.to_string()),
                    client::Refusal::Failed(why) => audit::How::Failed(why.clone()),
                },
            );
            let _ = events.send(Event::HandOverRefused(refused_by(refusal)));
        };
        let standing = match standing_on(&stored, ticket, &ground, HaveAClient::ByWaking(need)) {
            Ok(standing) => standing,
            Err(refusal) => {
                tracing::info!(%url, %refusal, "not handing this to a client that is not ours");
                return refuse(refusal);
            }
        };
        // The last look before it goes: the ground here, and the client inside
        // `deliver`, which will not hand a URL to one it has not just checked.
        let handed = standing
            .ground_is_still_there()
            .map_err(client::Refusal::Failed)
            .and_then(|()| {
                client::deliver(&standing.client, &standing.options, &standing.proven, &url)
            });
        match handed {
            Ok(()) => {
                audit::went(&doing, audit::How::Done);
                let _ = events.send(Event::HandedOver(HandedOver {
                    after_signing_in: standing.proven.just_signed_in,
                    asked,
                }));
            }
            Err(refusal) => {
                tracing::warn!(%url, %refusal, "Valve's client would not take that");
                refuse(refusal)
            }
        }
    });
}

/// The same refusal, said the way the shell answers it.
///
/// The two arms are not two wordings of one thing: a failure is reported and
/// dismissed, and a client that belongs to somebody else is a choice to put in
/// front of the person. See [`Refused`].
fn refused_by(refusal: client::Refusal) -> Refused {
    match refusal {
        client::Refusal::NotOurs(why) => Refused::SomebodyElses(why),
        client::Refusal::Failed(why) => Refused::Failed(why),
    }
}

/// Why a running client is not this session's to speak to, or `None`.
///
/// The same two questions [`client::wake`] asks, asked of the one path that
/// hands the client a URL rather than bringing it up: [`hand_over`], which is
/// now only ever about Steam's own window. A client that is not running is
/// nobody's and answers `None`, and [`client::open`] then starts one here, in
/// this session.
///
/// **That last sentence is why nothing about a title may use this.** "Not
/// running" is a true answer to "whose is it" and a useless one to "whose
/// library is about to be changed": the client this session then starts signs
/// itself into whichever account it last remembered. Titles go through
/// [`client::prove`], which answers that a stopped client proves nothing.
///
/// `stored` is `None` for a session with nobody signed in, which can still ask
/// the client to show itself. Only the account half of this goes with it: which
/// session a client is drawing into is a fact about this session, not about an
/// account, and a window raised on the desktop next door is just as wrong
/// either way.
fn not_ours(
    where_it_is: &client::Where,
    options: &client::Options,
    stored: Option<&session::Stored>,
) -> Option<client::NotOurs> {
    if !client::is_running(Some(where_it_is), options) {
        return None;
    }
    if !client::in_this_session(options) {
        return Some(client::NotOurs {
            what: "Steam is running in another session.".to_string(),
            cost: "This would happen there, not here.".to_string(),
        });
    }
    let stored = stored?;
    // [`client::state_now`], because this is the account half and the log it is
    // read from is appended across runs — see [`client::state_now`]. The panel
    // this feeds is the one that asks somebody to sign another account out, and
    // asking that about a client which has not signed in to anything is the
    // same wrong sentence [`client::wake`] used to refuse presses with.
    let state = client::state_now(Some(where_it_is), options);
    (state.signed_in() && !state.signed_in_as(credential(stored))).then(|| client::NotOurs {
        what: "Steam is signed in to another account.".to_string(),
        cost: "This would happen to that account.".to_string(),
    })
}

/// Start Valve's client and sign it in, on a thread of its own.
///
/// On its own thread because it is the one thing here that takes a minute and
/// a half in the worst case, and the worker it would otherwise block is what
/// answers the library, the catalogue and every press. `waking` is what stops
/// two presses starting two of them; it is cleared when the thread is done,
/// whichever way it went.
/// Why this session cannot ask Steam for something, said the way it is true.
///
/// Three sentences and not one, because they are three different facts and the
/// difference is what somebody would do about each. "Not signed in" was said of
/// all three, and on a machine with no network it was said of a session that
/// was signed in, holding a credential Steam had accepted, with the account's
/// name on the screen.
fn out_of_reach(state: &State) -> &'static str {
    match state {
        // Never said: every caller has already matched this out.
        State::In { .. } => "Steam is busy",
        State::Connecting { .. } => "Steam is still being reached",
        State::Reconnecting { .. } => "Steam cannot be reached from this machine",
        State::Out | State::Waiting { .. } => "Steam is not signed in",
    }
}

/// Take back an Offline Mode this session asked for, now that Steam answers.
///
/// Only where there is no client running. The mode is read as a client comes up
/// and never again, so changing the field under one that is already going does
/// nothing until it is restarted — and restarting Valve's client is how you end
/// somebody's game.
fn take_back_any_offline_mode(account: &str) {
    let Some(where_it_is) = client::Where::find() else {
        return;
    };
    let Some(options) = client::Options::for_client(&where_it_is) else {
        return;
    };
    if !client::is_running(Some(&where_it_is), &options) {
        client::offline::give_back(&options, account);
    }
}

/// Whether Valve's client is in its own Offline Mode.
///
/// Which is a different question from whether *this session* can reach Steam,
/// and both have to be asked: the client keeps the mode until somebody turns it
/// off, so a shell with a live connection and a full library can be sitting
/// beside a client that will not download a byte. See [`client::offline`].
fn the_client_is_offline() -> bool {
    let Some(where_it_is) = client::Where::find() else {
        return false;
    };
    let Some(options) = client::Options::for_client(&where_it_is) else {
        return false;
    };
    client::state_now(Some(&where_it_is), &options).offline()
}

/// How often the client is asked what a launch it was given is doing.
///
/// The same two seconds a download's row is polled on, and for the same reason:
/// this runs while somebody is watching a loading screen, and a late answer is
/// worth nothing because the next one is already due.
const WHILE_A_LAUNCH_WAITS: Duration = Duration::from_secs(2);

/// How long a launch is watched before the watcher gives up on its own.
///
/// The shell ends the watch itself on every path out of a press — see
/// [`Steam::stop_watching_the_launch`] — and this is what stops a thread
/// outliving a shell that somehow did not. Comfortably past the loading
/// screen's own patience, so it is never what ends a watch that matters.
const UNTIL_A_LAUNCH_IS_SOMEBODY_ELSES: Duration = Duration::from_secs(300);

/// Watch one launch, and say once if Valve's client stops to ask something.
///
/// On a thread of its own, like every other job in here that talks to the
/// client: this is a websocket round trip every two seconds and the worker has
/// a disk to read beside it.
///
/// Said **once**. A question stands until somebody answers it, so the client
/// goes on reporting it every time it is asked, and a shell told twice would
/// give sight back to a client it had already given it to.
fn watch_the_launch(app_id: u32, request: u64, running: Arc<AtomicBool>, events: &Sender<Event>) {
    let events = events.clone();
    std::thread::spawn(move || {
        let began = Instant::now();
        let mut said = false;
        // The last thing said about the step in hand, so that a task which has
        // not changed and a percentage which has not moved are not sent twice a
        // second for the whole of a download.
        let mut step: Option<Step> = None;
        // Where the client's own log is, and how much of it was written before
        // this press. Taken here, on the way in, because the press has just
        // gone out: what is written after this mark is this launch's, and the
        // failure of a press made a minute ago is not.
        let console = client::Where::find()
            .and_then(|where_it_is| client::Options::for_client(&where_it_is))
            .map(|options| {
                let from = client::launches_so_far(&options.root);
                (options.root, from)
            });
        while running.load(Ordering::SeqCst) && began.elapsed() < UNTIL_A_LAUNCH_IS_SOMEBODY_ELSES {
            std::thread::sleep(WHILE_A_LAUNCH_WAITS);
            if !running.load(Ordering::SeqCst) {
                return;
            }
            // What became of it, out of the client's own log. First, because it
            // costs a read of a few kilobytes where the question below costs a
            // websocket round trip — and because it is the only place a launch
            // the client gave up on is written down at all.
            let standing = console
                .as_ref()
                .and_then(|(root, from)| client::how_the_launch_went(root, app_id, *from));
            match &standing {
                Some(client::LaunchStanding::Refused(why)) => {
                    let why = why.clone();
                    tracing::warn!(app_id, %why, "Valve's client gave up on this launch");
                    audit::went(
                        format_args!("launch {app_id}"),
                        audit::How::Refused(format!("the client would not start it ({why})")),
                    );
                    let _ = events.send(Event::LaunchWasRefused(LaunchRefused {
                        request,
                        app_id,
                        why,
                    }));
                    break;
                }
                // The client's half of the launch is done and there is nothing
                // left for this thread to say: what remains is the game's
                // window, which the loading screen watches for itself.
                Some(client::LaunchStanding::Started) => break,
                _ => {}
            }
            // A client that cannot be reached is the ordinary case for a
            // session that never had to open its interface, and it is not
            // worth a word: this explains a wait that is going wrong, and a
            // shell that cannot ask simply waits as it always did. It is *not*
            // the end of the matter any more, because the log above answers
            // half of this without it.
            let walking = webui::launching()
                .unwrap_or_default()
                .into_iter()
                .find(|one| one.app_id == app_id);

            // Which step of the walk it is on, which either half can see. The
            // interface is preferred where there is one: it is the live task
            // rather than the last one written down, and it is the only half
            // that counts. The log answers on a client that exposes nothing,
            // and its `Waiting` is the same step said a moment later — see
            // [`client::LaunchStanding`].
            let task = walking
                .as_ref()
                .map(|one| one.task.clone())
                .or_else(|| match &standing {
                    Some(client::LaunchStanding::Working(task))
                    | Some(client::LaunchStanding::Waiting(task)) => Some(task.clone()),
                    _ => None,
                });
            // Shaders being compiled, which is the one step Steam stops on to
            // offer a way past. Not a question — see [`Step`] — so this never
            // reaches the panel below, and it is the only step that carries
            // something to press.
            let compiling = matches!(
                &standing,
                Some(client::LaunchStanding::Waiting(task)) if task == webui::PROCESSING_SHADERS
            ) || walking.as_ref().is_some_and(|one| {
                one.waiting_for_a_person && one.task == webui::PROCESSING_SHADERS
            });
            let now = Step {
                request,
                app_id,
                task,
                how_far: walking.as_ref().and_then(webui::Launching::how_far),
                skip: match compiling {
                    true => walking.as_ref().map(|one| one.action_id),
                    false => None,
                },
            };
            if step.as_ref() != Some(&now) {
                tracing::info!(
                    app_id,
                    task = ?now.task,
                    how_far = ?now.how_far,
                    can_skip = now.skip.is_some(),
                    "what Valve's client is doing about this launch"
                );
                let _ = events.send(Event::LaunchIsWorkingOnIt(now.clone()));
                step = Some(now);
            }
            if compiling {
                continue;
            }
            let Some(stopped) = walking.filter(|one| one.waiting_for_a_person) else {
                continue;
            };
            if said {
                continue;
            }
            said = true;
            // Asked once, here, rather than on every poll: it is a second round
            // trip and it only ever matters at this moment.
            let question = webui::the_question(&stopped).unwrap_or_default();
            tracing::info!(
                app_id,
                task = %stopped.task,
                details = %stopped.details,
                ours = question.is_some(),
                "Valve's client has stopped this launch to ask something"
            );
            audit::went(
                format_args!("launch {app_id}"),
                audit::How::Refused(format!(
                    "the client is waiting on a person ({})",
                    stopped.task
                )),
            );
            let _ = events.send(Event::LaunchIsAsking(Asking {
                // The watch number is the worker's and stops this thread; this
                // is the shell's, and is what tells it that the launch this is
                // about is the launch it is still waiting on. The two cannot be
                // one number: a watcher decides to speak, and the press it is
                // speaking about can end between that decision and the message
                // being read.
                request,
                app_id,
                task: stopped.task,
                question,
            }));
        }
        // Cleared on the way out, whichever way it went: the map on the worker
        // reads this to know which of its entries belong to threads that have
        // already finished. See [`Watching::forget_finished_watches`].
        running.store(false, Ordering::SeqCst);
    });
}

/// A wake has finished. Answer everybody riding on it, and start the next one.
///
/// The rule this exists to keep is that **every wake request is answered
/// exactly once, under its own number**. What comes back from the thread is one
/// report about one client; what goes out is one event per request that was
/// waiting on it, each carrying that request's own ticket, so nothing above has
/// to guess whose answer it is holding.
///
/// A wake whose ground has moved is answered too, and answered with the truth:
/// what it signed in is not this session's account any more. Silence would be
/// worse than a refusal — a press waiting on a number nothing will ever say is
/// a loading screen that ends only when its patience does.
fn a_wake_landed(
    ticket: Ticket,
    report: ClientReport,
    state: State,
    worker: &Sender<WorkerMessage>,
    events: &Sender<Event>,
    watching: &mut Watching,
    waking: &mut Waking,
) -> State {
    let riders = match waking.in_flight.take() {
        Some((flying, riders)) if flying.request == ticket.request => riders,
        // Not the wake this worker is holding. Nothing starts a wake but
        // [`take_a_wake`], which records it before the thread begins, so this
        // is unreachable — said as a line in the log rather than a panic,
        // because there is nothing about a word from Steam worth ending a
        // session over.
        other => {
            tracing::warn!(
                request = ticket.request,
                held = ?other.as_ref().map(|(flying, _)| flying.request),
                "a wake came back that this worker was not holding"
            );
            waking.in_flight = other;
            return state;
        }
    };
    let report = match still_the_same_ground(&ticket, &state, watching) {
        true => report,
        false => {
            tracing::info!(
                was = ticket.ground,
                now = watching.generation,
                "Valve's client came up for an account that has since gone"
            );
            ClientReport::Unavailable(
                "The Steam account signed out while its client was starting.".to_string(),
            )
        }
    };
    // One per request, each under its own number. Two presses a moment apart
    // are two presses, and the second is owed an answer of its own rather than
    // a share of the first's.
    for request in riders {
        let _ = events.send(Event::Client {
            ticket: Ticket { request, ..ticket },
            report: report.clone(),
        });
    }
    // And whatever could not be started while that was running. Taken all at
    // once: the first goes to the client, and the rest ride on it or queue
    // again behind it, which is exactly what [`take_a_wake`] is for.
    for (request, take_over) in std::mem::take(&mut waking.waiting_behind) {
        take_a_wake(request, take_over, &state, waking, watching, worker, events);
    }
    state
}

/// Which client a `steam:` URL is delivered to.
///
/// The whole of the rule in one place, because the whole of a real failure was
/// one leg of it. See [`Ask::Tell`], where it is used and where the incident is
/// written down.
#[derive(Debug, PartialEq, Eq)]
enum Route {
    /// To a client this session has woken and signed in as its own account.
    AClientOfOurs,
    /// To whichever client is holding the pipe, starting one where there is
    /// none. Only ever for Steam's own window, asked for by a session with no
    /// credential to offer instead.
    WhoeverIsThere,
    /// To nobody: there is no account to ask this on behalf of.
    Nobody,
}

/// The rule itself, as a table of the two things that decide it.
fn route(signed_in: bool, about_the_client: bool) -> Route {
    match (signed_in, about_the_client) {
        (true, _) => Route::AClientOfOurs,
        (false, true) => Route::WhoeverIsThere,
        (false, false) => Route::Nobody,
    }
}

/// The credential this session is holding, and how far a client woken with it
/// may go without Steam.
///
/// Four of the five states carry a stored session; only [`State::Out`] and a
/// sign-in still being typed carry none. What separates them for this purpose
/// is not whether Steam has answered — it is whether Steam has been *given the
/// chance to* and failed:
///
/// * `In` and `Connecting` — a session that has reached Steam, or is reaching
///   for it now, and has no evidence it cannot. Ordinary wake.
/// * `Reconnecting` — a session that tried and could not. This is the only one
///   allowed to put Valve's client into Offline Mode, and the distinction is
///   the whole reason this is a function: a boot that was going to succeed
///   spends a second or two in `Connecting`, and a press made in that second
///   must not leave somebody's client coming up offline afterwards.
fn holding(state: &State) -> Option<(&session::Stored, client::Need)> {
    match state {
        State::In { stored, .. } | State::Connecting { stored, .. } => {
            Some((stored, client::Need::SignedIn))
        }
        State::Reconnecting { stored, .. } => Some((stored, client::Need::Offline)),
        State::Out | State::Waiting { .. } => None,
    }
}

/// The ticket one request made now belongs to.
///
/// The ground is read off the worker at the moment of asking rather than
/// passed in, so that there is one answer to "which account and which Steam is
/// this" and every request is stamped from it.
fn ticket_for(request: u64, state: &State, watching: &Watching) -> Ticket {
    Ticket {
        request,
        ground: watching.generation,
        account: signed_in_as(state).unwrap_or_default(),
    }
}

/// The ticket a background job about one title carries.
///
/// **No request number, and that is not an oversight.** These jobs are named by
/// the game they are about and answered on its row; there is no press waiting
/// on a number, which is the whole of what [`Ticket::request`] is for. What
/// they want of a ticket is its other two halves — which account and which
/// Steam — because those are what [`Ground`] is asked about while they run.
fn a_job_about(state: &State, watching: &Watching) -> Ticket {
    ticket_for(0, state, watching)
}

/// Whether an answer belongs to the session as it stands now.
///
/// Two numbers and both have to hold. The generation moves when the ground
/// under a job moves — the account signed out, the Steam on the machine
/// changed — and the account is the thing that moved, checked in its own right
/// so that a generation which somehow did not move cannot let one account's
/// answer through to another's.
fn still_the_same_ground(ticket: &Ticket, state: &State, watching: &Watching) -> bool {
    ticket.ground == watching.generation
        && ticket.account == signed_in_as(state).unwrap_or_default()
}

/// Take one wake request: start it, ride it on the one already running, or
/// queue it behind a wake whose ground has gone.
///
/// **Every request that reaches here is answered exactly once**, which is what
/// the shell above is entitled to assume — a press waiting on a number that
/// nothing will ever say is a loading screen that only ends when its patience
/// does.
fn take_a_wake(
    request: u64,
    take_over: bool,
    state: &State,
    waking: &mut Waking,
    watching: &Watching,
    worker: &Sender<WorkerMessage>,
    events: &Sender<Event>,
) {
    let ticket = ticket_for(request, state, watching);
    if let Some((flying, riders)) = waking.in_flight.as_mut() {
        // The same account and the same Steam: what is already coming up is
        // exactly what this press wants, so it rides on it and is answered
        // when it lands, under its own number.
        if flying.ground == ticket.ground && flying.account == ticket.account {
            riders.push(request);
            let _ = events.send(Event::Client {
                ticket,
                report: ClientReport::Waking,
            });
            return;
        }
        // The ground has moved. The thread that is running is signing a client
        // in as somebody who is no longer here and cannot be called back, and a
        // second client started beside it is two of them fighting over one
        // machine — so this waits for it. See [`Waking::waiting_behind`].
        tracing::info!(
            request,
            was = flying.ground,
            now = ticket.ground,
            "this wake waits for the one running for an account that has gone"
        );
        waking.waiting_behind.push((request, take_over));
        let _ = events.send(Event::Client {
            ticket,
            report: ClientReport::Waking,
        });
        return;
    }
    // Only for a session that is actually signed in: the client is signed in
    // with *this* session's credential, and there is no credential until Steam
    // has accepted one.
    //
    // Accepted one, and not *is accepting* one. This used to ask for
    // `State::In`, which is the state a session reaches by getting an answer
    // out of Steam — so on a machine with no network every game press in the
    // shell came back "Steam is not signed in, so its client cannot be
    // started", said of a session that was signed in, holding a credential
    // Steam had accepted, looking at a column of games that were on the disk
    // and playable. See [`holding`].
    let Some((stored, need)) = holding(state) else {
        let _ = events.send(Event::Client {
            ticket,
            report: ClientReport::Unavailable(
                "Steam is not signed in, so its client cannot be started.".to_string(),
            ),
        });
        return;
    };
    wake_the_client(
        stored,
        need,
        ticket,
        waking,
        worker,
        events,
        permission(take_over),
    );
}

fn wake_the_client(
    stored: &session::Stored,
    need: client::Need,
    ticket: Ticket,
    waking: &mut Waking,
    worker: &Sender<WorkerMessage>,
    events: &Sender<Event>,
    permission: client::Permission,
) {
    let refuse = |why: &str| {
        let _ = events.send(Event::Client {
            ticket,
            report: ClientReport::Unavailable(why.to_string()),
        });
    };
    let Some(where_it_is) = client::Where::find() else {
        refuse("There is no Steam client installed on this machine.");
        return;
    };
    // Asked of the client that was found rather than of the disk, so that a
    // client which has never been run is started a first time instead of being
    // reported missing. See [`client::Options::for_client`].
    let Some(options) = client::Options::for_client(&where_it_is) else {
        refuse("Steam's directories could not be found on this machine.");
        return;
    };

    waking.in_flight = Some((ticket, vec![ticket.request]));
    let _ = events.send(Event::Client {
        ticket,
        report: ClientReport::Waking,
    });
    audit::asked(format_args!("wake {where_it_is:?}"));
    let stored = stored.clone();
    let worker = worker.clone();
    std::thread::spawn(move || {
        // Signed in is the whole of what a game press needs: the game itself
        // is asked for with a `steam:` URL over the client's pipe, so on a
        // client that can sign itself in nothing is ever exposed. Where this
        // session cannot reach Steam either, `need` is [`client::Need::Offline`]
        // and the same wake ends in Valve's own Offline Mode instead.
        let report = match client::wake(
            &where_it_is,
            &options,
            credential(&stored),
            need,
            permission,
            ticket.request,
        ) {
            // What it proved is not carried up. The press that is waiting on
            // this hands `steam://rungameid/…` over from the shell's own
            // thread, minutes later in the worst case, and it proves the client
            // for itself in that moment — see `Steam::play`, which is where
            // being a minute out of date would matter. Carrying a proof that
            // far would be carrying a promise nobody could keep.
            Ok(_proven) => ClientReport::Ready,
            Err(client::Refusal::NotOurs(why)) => {
                tracing::info!(%why, "Valve's client is not this session's to move");
                ClientReport::SomebodyElses(why)
            }
            Err(client::Refusal::Failed(why)) => {
                tracing::warn!(%why, "Valve's client could not be signed in");
                ClientReport::Unavailable(why)
            }
        };
        audit::went(
            "wake the client",
            match &report {
                ClientReport::Ready => audit::How::Done,
                ClientReport::SomebodyElses(why) => audit::How::Refused(why.to_string()),
                ClientReport::Unavailable(why) => audit::How::Failed(why.clone()),
                // Not reachable: `Waking` is sent before the thread starts.
                ClientReport::Waking => audit::How::Done,
            },
        );
        // To the worker rather than to the shell: it is the worker that holds
        // the wake, that knows whether the ground it was asked on is still
        // there, and that has whatever was queued behind this to start.
        let _ = worker.send(WorkerMessage::Done(Finished::Waking { ticket, report }));
    });
}

fn answer_cm(
    event: cm::Event,
    state: State,
    events: &Sender<Event>,
    watching: &mut Watching,
) -> State {
    // The roster, first and without taking the state apart. It changes nothing
    // about the session — an account is not more or less signed in for a friend
    // having started a game — so the arm that would rebuild `State::In` field
    // by field to put it back unchanged is the wrong shape for it. The
    // generation is still checked: a roster from a connection this session has
    // already given up on is a list of where people were.
    if let cm::Event::Friends { generation, roster } = event {
        if matches!(&state, State::In { generation: expected, .. } if *expected == generation) {
            let _ = events.send(Event::Friends(roster));
        }
        return state;
    }
    // And conversations, on exactly the same terms and for the same reason: a
    // message arriving changes nothing about the session, so the arm that would
    // take `State::In` apart and put it back unchanged is the wrong shape for
    // it.
    //
    // The generation is checked *here as well as* in the shell's own store —
    // which does it again, against what it is still waiting for. Two checks of
    // one number, and neither is redundant: this one drops packets belonging to
    // a connection the worker has abandoned, and the shell's drops answers to
    // requests it has stopped wanting. See [`chat::Conversations::heard`].
    if let cm::Event::AchievementProgress(heard) = event {
        if matches!(&state, State::In { generation, .. } if *generation == heard.generation) {
            let _ = events.send(Event::AchievementProgress(heard));
        }
        return state;
    }
    if let cm::Event::Achievements(heard) = event {
        if matches!(&state, State::In { generation, .. } if *generation == heard.generation) {
            let _ = events.send(Event::Achievements(heard));
        }
        return state;
    }
    if let cm::Event::Chat(heard) = event {
        if matches!(&state, State::In { generation: expected, .. } if *expected == heard.generation)
        {
            let _ = events.send(Event::Chat(heard));
        }
        return state;
    }
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
            // The account id's last four digits and nothing else. It is enough
            // to tell one household account from another in a record somebody
            // is reading beside their own screen, and it is not an identifier:
            // the name is not here and neither is the id.
            audit::went(
                format_args!("sign in (account …{})", account.steam_id % 10_000),
                audit::How::Done,
            );
            if let Err(error) = stored.save() {
                tracing::warn!(%error, "the Steam session could not be written down");
            }
            if !announced {
                let _ = events.send(Event::SignedIn(account.clone()));
            }
            // Steam answers, so an Offline Mode this session asked for has
            // done its work. Here rather than only on the next press: a
            // machine that was offline last week and is not now must not start
            // Valve's client offline for somebody who never opens a game, and
            // the field is read as the client comes up, so this is the one
            // moment it is worth writing. It takes back nothing it did not ask
            // for — see [`client::offline`].
            take_back_any_offline_mode(&stored.account);
            let _ = events.send(Event::Reach(Reach::Online));
            let _ = events.send(Event::Library(library::merge(
                owned.clone(),
                &installed_now().games,
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
            // Written down before it is announced, so a session that is closed
            // in the next second still has it next time. See [`catalogue`].
            catalogue::keep(stored.steam_id, &owned);
            let next_owned = Instant::now() + OWNED_INTERVAL;
            let _ = events.send(Event::Reach(Reach::Online));
            // Not a memory any more: what is on the screen is what Steam just
            // said, and a row dating it would be dating the present.
            let _ = events.send(Event::LibraryAsOf(None));
            let _ = events.send(Event::Library(library::merge(
                owned.clone(),
                &installed_now().games,
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
        ) if generation == expected => ended(
            stored,
            announced,
            failure_to_panel,
            owned,
            failure,
            events,
            watching,
        ),
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
        ) if generation == expected => ended(stored, true, false, owned, failure, events, watching),
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
    watching: &mut Watching,
) -> State {
    tracing::warn!(%failure, "the Steam CM session ended");
    if failure.permanently_rejected() {
        session::Stored::forget();
        // The account has gone, and it did not go by anybody's choice. Whatever
        // was in flight for it stops being this session's to report on, exactly
        // as it would after a deliberate sign-out. See [`Watching::generation`].
        watching.nobody_is_signed_in_now();
        catalogue::forget();
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
        // Not a sign-out and not a failure: the account stands, the credential
        // stands, and the games on the disk still play. Said out loud so the
        // shell can draw the difference rather than showing a signed-in session
        // that mysteriously has no library.
        let _ = events.send(Event::Reach(Reach::Offline(failure.said())));
        State::Reconnecting {
            stored,
            next: Instant::now() + Duration::from_secs(5),
            announced,
            failure_to_panel,
            owned,
        }
    }
}

/// Tell Valve's client the account's status, where one is running.
///
/// **Only where one already is.** Nothing here starts a client: somebody
/// choosing a status has not asked for Steam to be launched, and a press that
/// took a minute and put a window on the screen would be the shell answering a
/// question nobody put. Answers whether there was a client to tell, which is
/// what [`a_client_still_has_to_be_told`] waits on.
///
/// It is a `steam:` URL rather than a call into the client's own interface, and
/// that is the point. The interface is only listening when the client was
/// started with the debugging marker in place — which the ordinary session
/// deliberately does not do, and which a client somebody started for themselves
/// never has — so a call would work for the one client in ten this shell
/// happens to have exposed. A URL is handed to any client at all, by the same
/// courier that hands over `steam://rungameid`.
///
/// Measured on client build 1785799196: `steam://friends/status/online`,
/// `.../away`, `.../invisible` and `.../offline` each moved the client's own
/// `GetPersonaState`, read back through its interface either side of the URL.
/// The four verbs this shell does not offer — busy, snooze, trade, play —
/// were accepted and did nothing, which is why [`friends::Presence::url_verb`]
/// answers `None` for them rather than sending a URL that reports success.
///
/// Says nothing back beyond that. A client that would not take the URL has left
/// a line in the log, and the status is set on this session either way; a panel
/// that refused to show a chosen status because Steam's own window disagreed
/// would be worse than the disagreement.
fn tell_the_client_too(status: friends::Presence, steam_id: u64) -> bool {
    let Some(verb) = status.url_verb() else {
        return false;
    };
    let Some((options, where_it_is)) =
        client::Where::find().and_then(|found| Some((client::Options::for_client(&found)?, found)))
    else {
        return false;
    };
    if !client_has_account(client::state_now(Some(&where_it_is), &options), steam_id) {
        return false;
    }
    let url = format!("steam://friends/status/{verb}");
    // Serialized on the Steam worker. A courier normally returns in
    // milliseconds, and preserving order matters more than making these rare
    // presses concurrent: two detached couriers could deliver an older choice
    // after the newer one.
    match client::open(&where_it_is, Some(&options), &url) {
        Ok(()) => {
            tracing::info!(url, "told Valve's client the account's status");
            true
        }
        Err(error) => {
            tracing::warn!(%error, url, "Valve's client would not take the status");
            false
        }
    }
}

fn client_has_account(state: client::State, steam_id: u64) -> bool {
    let account_id = steam_id as u32;
    matches!(
        state,
        client::State::SignedIn(id) | client::State::Offline(id) if id == account_id
    )
}

/// How many times one client run is handed a status it is not wearing.
///
/// More than once because a URL handed to a client is posted rather than
/// answered — [`client::open`] reports that Steam took it, not that the friends
/// menu moved — and bounded because a client that has refused three of them is
/// not going to take a fourth, and a shell that went on asking would be handing
/// Valve's client a URL every ten seconds for the rest of the session.
///
/// It is only ever reached by a status the client's own record cannot confirm:
/// the ordinary case is one hand-over, the record agreeing four seconds later,
/// and nothing owed after that.
const TIMES_A_CLIENT_IS_TOLD: u8 = 3;

/// What Valve's client on this machine has the account's status set to.
///
/// `None` where there is no client, or none that has ever been signed in to
/// this account: a machine whose Steam has never been opened has no status to
/// mirror, and this session falls back to [`friends::AS_QUIETLY_AS_IT_CAN`].
pub(crate) fn status_on_this_machine(steam_id: u64) -> Option<friends::Presence> {
    let found = client::Where::find()?;
    let options = client::Options::for_client(&found)?;
    client::recorded_status(&options, steam_id as u32)
}

/// Whose account this session is signed in to, where it is signed in to one.
fn signed_in_as(state: &State) -> Option<u64> {
    match state {
        State::Out | State::Waiting { .. } => None,
        State::Connecting { stored, .. } | State::Reconnecting { stored, .. } => {
            Some(stored.steam_id)
        }
        State::In { stored, .. } => Some(stored.steam_id),
    }
}

/// What is to be done about a status no client on this machine has worn yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Owing {
    /// Nothing is owed, or there is no client signed in to owe it to.
    Nothing,
    /// The client's own record agrees with it. It landed, and is forgotten.
    Landed,
    /// Hand it to the client that is running.
    HandOver,
    /// Owed still, and this client run has been told as often as it will be.
    Told,
}

/// The rule, kept where it can be read in one place and tested without a Steam.
///
/// **A status is owed until a client's own record agrees with it**, and not
/// until it has been handed over: handing it over is posting a URL, which says
/// that Steam took it and nothing about whether the friends menu moved. See
/// [`client::recorded_status`].
///
/// A client that is not signed in is owed nothing *yet* — not because it does
/// not need telling, but because there is nobody to tell. Its counter is
/// dropped elsewhere, so the next client to come up is told from scratch: one
/// that has just started is wearing whatever it last remembered rather than
/// anything this session said.
fn what_a_client_is_owed(
    owed: Option<friends::Presence>,
    recorded: Option<friends::Presence>,
    signed_in: bool,
    told: u8,
) -> Owing {
    let Some(owed) = owed else {
        return Owing::Nothing;
    };
    if !signed_in {
        return Owing::Nothing;
    }
    if recorded == Some(owed) {
        return Owing::Landed;
    }
    // Once for a status nothing will ever confirm, because going on asking
    // would be asking for an agreement that is never written down. See
    // [`friends::Presence::is_written_down`].
    let times = if owed.is_written_down() {
        TIMES_A_CLIENT_IS_TOLD
    } else {
        1
    };
    if told < times {
        Owing::HandOver
    } else {
        Owing::Told
    }
}

/// Where this machine stands on Steam, in the order the three answers count.
///
/// A status chosen here that no client has worn yet is the machine's status —
/// it is the most recent thing anybody said, and it is about to be the client's
/// too. Failing that, whatever Valve's client is set to. Failing that, there is
/// no Steam on this machine to mirror and this session speaks only for itself.
fn where_this_machine_stands(
    owed: Option<friends::Presence>,
    recorded: Option<friends::Presence>,
) -> friends::Presence {
    owed.or(recorded).unwrap_or(friends::AS_QUIETLY_AS_IT_CAN)
}

fn client_choice_supersedes_offline(
    owed: Option<friends::Presence>,
    told: u8,
    before: Option<friends::Presence>,
    now: Option<friends::Presence>,
) -> bool {
    owed == Some(friends::Presence::Offline)
        && told > 0
        && before.is_some()
        && now.is_some()
        && before != now
}

/// Keep the two halves of the account's status saying the same thing.
///
/// **The status is a property of the machine, not of this connection**, and
/// this is the whole of what follows from that. There are two places it can be
/// written — the shell's panel and Valve's client's friends menu — and the
/// user's complaint was that they disagreed: *"The Steam status should always
/// mirror what Shell sets in the Friends Menu and right now it is not the case
/// when Steam is started later."*
///
/// Run on every tick of the worker, above the state machine and outside it,
/// because none of it is about this worker's connection to Steam: a client
/// coming up, or somebody changing their status in its window, happens in any
/// of those states.
///
/// Three things, in order:
///
/// 1. **A status chosen here is owed to a client until one wears it.** Handed
///    over the moment a client is signed in, and *kept* until the client's own
///    record agrees — see [`client::recorded_status`], which is the only
///    readback there is on a client nobody started with a debugging marker.
///    Then it is forgotten, from memory and from the disk both.
/// 2. **Once nothing is owed, this session follows the client.** Somebody who
///    changes their status in Valve's own window has changed the machine's
///    status, and a panel still showing what the shell last said would be the
///    same disagreement pointing the other way.
/// 3. **What it announces is what it shows.** The status this session tells
///    Steam is the one the panel draws, so the two cannot come apart; and
///    because it is the machine's status rather than an opinion of the shell's,
///    it is announced with `persona_set_by_user` false unless a person really
///    did choose it here.
///
/// A machine with no client, or one whose Steam has never been opened, has
/// nothing to read and nothing to tell, and falls through all of it to
/// [`friends::AS_QUIETLY_AS_IT_CAN`] — which is where this shell started and is
/// still the right answer when there is no other.
fn keep_the_status_in_step(state: &State, status: &cm::Status, told: &mut u8, steam_id: u64) {
    // Signed in rather than merely running, and [`client::state_now`] rather
    // than [`client::state`], because a client that is still coming up has a
    // connection log whose tail belongs to the run before it.
    let client =
        client::Where::find().and_then(|found| Some((client::Options::for_client(&found)?, found)));
    let signed_in = client.as_ref().is_some_and(|(options, found)| {
        client_has_account(client::state_now(Some(found), options), steam_id)
    });
    let recorded = signed_in
        .then(|| {
            client
                .as_ref()
                .and_then(|(options, _)| client::recorded_status(options, steam_id as u32))
        })
        .flatten();

    // A client that is not there cannot have been told, which is what makes the
    // *next* one get the status — and it has to, because a client that has just
    // started is wearing what it last remembered rather than anything this
    // session said.
    if !signed_in {
        *told = 0;
    }

    let mut stands = status
        .lock()
        .expect("the account's status is never poisoned");
    // Offline has no value in ePersonaState, so it cannot agree in the usual
    // way. Remember the value it displaced; a later *different* recorded
    // value is somebody using Valve's own UI and releases the debt instead of
    // the shell forcing Offline for ever.
    if client_choice_supersedes_offline(stands.owed, *told, stands.offline_record, recorded) {
        tracing::info!("Valve's client changed after Offline was handed over");
        stands.owed = None;
        stands.offline_record = None;
        session::Owed::forget();
        *told = 0;
    } else if stands.owed == Some(friends::Presence::Offline) {
        match (stands.offline_record, recorded) {
            (None, current) if *told == 0 => stands.offline_record = current,
            _ => {}
        }
    } else if stands.owed != Some(friends::Presence::Offline) {
        stands.offline_record = None;
    }

    match what_a_client_is_owed(stands.owed, recorded, signed_in, *told) {
        Owing::Nothing | Owing::Told => {}
        Owing::Landed => {
            tracing::info!(
                status = stands.owed.map(friends::Presence::said),
                "Valve's client is wearing the status chosen here"
            );
            stands.owed = None;
            stands.offline_record = None;
            session::Owed::forget();
        }
        Owing::HandOver => {
            if let Some(owed) = stands.owed {
                tracing::info!(
                    status = owed.said(),
                    "Valve's client is not wearing the status chosen here, so it is told"
                );
                *told = told.saturating_add(u8::from(tell_the_client_too(owed, steam_id)));
            }
        }
    }

    // And what this session says about the account, which is what the panel
    // shows. Sent only where it has changed: this runs every ten seconds, and
    // the ordinary answer is that nothing has.
    let wanted = where_this_machine_stands(stands.owed, recorded);
    if stands.announced == Some(wanted) {
        return;
    }
    let chosen = stands.owed.is_some();
    if let State::In { commands, .. } = state {
        if commands
            .send(cm::Command::Announce {
                presence: wanted,
                chosen,
            })
            .is_ok()
        {
            tracing::info!(
                status = wanted.said(),
                chosen,
                "this session now stands where the machine's Steam does"
            );
            stands.announced = Some(wanted);
        }
    }
}

fn advance(
    state: State,
    wire: &web::Wire,
    worker: &Sender<WorkerMessage>,
    events: &Sender<Event>,
    ledger: &mut Ledger,
) -> State {
    let Ledger {
        watching,
        generation,
        status,
        told_the_client,
        ..
    } = ledger;
    // Before the state machine and outside it, because it is not about this
    // worker's connection to Steam at all: it is about the other half of the
    // integration, which moves in any of these states. See
    // [`keep_the_status_in_step`].
    if let Some(steam_id) = signed_in_as(&state) {
        keep_the_status_in_step(&state, status, told_the_client, steam_id);
    }
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
            status,
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
                connect(stored, false, true, Vec::new(), worker, generation, status)
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
                let on_the_disk = installed_now();
                // Before anything is said about a row: a job whose Steam has
                // gone out from under it can never finish, and is not this
                // session's to report on any more.
                watching.the_backend_is_now(on_the_disk.root.as_deref());
                let installed = on_the_disk.games;
                // What this session asked for, first: a row that is counting
                // up has to be told before the column it sits in is replaced.
                // What Valve's client says about the download it is running,
                // asked only where this session has a job that could be it.
                // The manifest is not a progress source — see [`webui::Live`],
                // which carries the measurement — so this is where a moving
                // percentage and a rate come from. Hopefully rather than
                // depended on: a client whose interface is shut answers
                // nothing, and the row falls back to the disk.
                let live = match watching.anything_is_moving() {
                    true => webui::downloading().ok().flatten(),
                    false => None,
                };
                watching.report(&installed, live, events);
                // Asked of the disk as well as of this session's own jobs. A
                // download somebody started in Steam's own window has a row
                // here counting up exactly like one started from the bar, and
                // it would otherwise be the one that crawled.
                let moving = watching.anything_is_moving()
                    || installed.values().any(|game| game.standing.moving());
                let _ = events.send(Event::Library(library::merge(owned.clone(), &installed)));
                next_installed = now
                    + match moving {
                        true => WHILE_SOMETHING_MOVES,
                        false => INSTALLED_INTERVAL,
                    };
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

    /// Where a `steam:` URL goes, and the leg of it that was wrong.
    ///
    /// Until 2026-09-04 the client's own URLs — Open Steam, Big Picture, the
    /// downloads list — went to whichever client held the pipe *whatever* this
    /// session was holding, on the argument that Steam's own window is
    /// account-neutral. It is; its login screen is not. On a machine where this
    /// shell had just installed Steam and signed itself in by photographed
    /// code, with the library on the bar, Open Steam produced Valve's sign-in
    /// window — because the only thing that ever hands the client a credential
    /// is the wake this row went around.
    #[test]
    fn a_session_holding_a_credential_opens_its_own_steam() {
        // The leg that was wrong. Signed in, and about the client: their Steam,
        // signed in, and nothing else will do.
        assert_eq!(route(true, true), Route::AClientOfOurs);
        // The leg that was already right, and stays right for the same reason.
        assert_eq!(route(true, false), Route::AClientOfOurs);

        // Signed out, and Steam's own window asked for: whatever is there, and
        // one started where there is nothing. This is the only row on the bar
        // that works with nobody signed in — it is where somebody signs in to
        // Steam itself — and it must keep working.
        assert_eq!(route(false, true), Route::WhoeverIsThere);
        // Signed out, and a title named: there is no library to install out of
        // or verify against until Steam has accepted a credential.
        assert_eq!(route(false, false), Route::Nobody);
    }

    /// [`came_back`] without the two the wake arm needs and no other job does.
    ///
    /// A worker channel nothing sends on and a hold nothing is in flight
    /// through: every job below is an install, a removal or a compatibility
    /// list, and none of them touches either.
    fn came_back_for_a_test(
        finished: Finished,
        state: State,
        events: &Sender<Event>,
        watching: &mut Watching,
    ) -> State {
        let (worker, _held) = mpsc::channel();
        came_back(
            finished,
            state,
            &worker,
            events,
            watching,
            &mut Waking::default(),
        )
    }

    fn on_disk(app_id: u32, standing: library::Standing) -> library::Installed {
        library::Installed {
            app_id,
            name: "Invented".to_string(),
            size_on_disk: 40,
            path: PathBuf::from("/games/Invented"),
            standing,
            downloaded: 30,
            to_download: 100,
            update_outstanding: false,
            update_required: false,
            scheduled_for: 0,
            last_result: 0,
            fully_installed: standing.on_the_disk(),
            tool: false,
        }
    }

    fn disk(entries: Vec<library::Installed>) -> BTreeMap<u32, library::Installed> {
        entries
            .into_iter()
            .map(|entry| (entry.app_id, entry))
            .collect()
    }

    /// A job that comes back after its account has gone says nothing to
    /// anybody, and a job of the account that is signed in now still does.
    ///
    /// A wake in flight, as [`take_a_wake`] would have recorded it.
    ///
    /// Built rather than started, because starting one asks the machine what
    /// Steam it has and every test here is about what happens *after* that.
    fn in_flight(request: u64, ground: u64) -> (Ticket, Vec<u64>) {
        let ticket = Ticket {
            request,
            ground,
            account: 0,
        };
        (ticket, vec![request])
    }

    /// A job's authority is checked **while it runs**, and not only when it
    /// comes back.
    ///
    /// The generation was always compared in `came_back`, which is after the
    /// game has been installed, removed, stopped or reassigned: it kept a stale
    /// answer off the screen and did nothing at all about stale *authority*
    /// acting. This is the number a thread can read for itself, four steps
    /// earlier. See [`Ground`].
    #[test]
    fn the_ground_a_job_was_asked_on_can_be_checked_before_it_acts() {
        let ground = Ground::default();
        ground.moved_to(3, 7);
        let ticket = Ticket {
            request: 0,
            ground: 3,
            account: 7,
        };
        assert!(ground.still(&ticket).is_ok());

        // Signed out: the generation moves and the account goes with it.
        ground.moved_to(4, 0);
        let why = ground.still(&ticket).expect_err("the ground moved");
        assert!(why.contains("no longer the one signed in"), "{why}");

        // And an account that moved without the generation moving is caught by
        // the other half — two checks of one fact, for the reason
        // [`still_the_same_ground`] makes both.
        ground.moved_to(3, 9);
        assert!(ground.still(&ticket).is_err());
    }

    /// What the worker publishes is what a finished job is checked against.
    ///
    /// The invariant that makes [`Ground`] safe to add at all: a job asking
    /// while it runs and the worker asking when it comes back must be asking
    /// one question, or a job could be let through by one and dropped by the
    /// other and nobody would be able to say which was right.
    #[test]
    fn a_running_job_and_a_finished_one_are_checked_against_the_same_ground() {
        let mut ledger = Ledger {
            watching: Watching::default(),
            waking: Waking::default(),
            ground: Ground::default(),
            generation: 0,
            status: cm::Status::default(),
            told_the_client: 0,
            setting_up: false,
        };
        let state = State::Out;
        ledger.watching.generation = 5;
        where_the_session_stands(&mut ledger, &state);

        let ticket = a_job_about(&state, &ledger.watching);
        assert!(still_the_same_ground(&ticket, &state, &ledger.watching));
        assert!(ledger.ground.still(&ticket).is_ok(), "the two disagreed");

        // A sign-out moves the ground. Both halves have to refuse the same
        // ticket, and refuse it for the same reason.
        ledger.watching.nobody_is_signed_in_now();
        where_the_session_stands(&mut ledger, &state);
        assert!(!still_the_same_ground(&ticket, &state, &ledger.watching));
        assert!(ledger.ground.still(&ticket).is_err(), "the two disagreed");
    }

    /// A job whose account has gone stops **before** it touches Valve's client.
    ///
    /// The order inside [`Standing::about_to_act`], pinned: the ground is the
    /// cheap half and the one that is true whatever the machine is doing, so it
    /// is asked first. The client here is deliberately nowhere — if the two
    /// were the other way round this would answer about a Steam that is not
    /// installed, which is a true sentence about the wrong thing.
    #[test]
    fn a_job_on_ground_that_has_moved_never_reaches_the_client() {
        let ground = Ground::default();
        ground.moved_to(1, 1);
        let standing = Standing {
            ticket: Ticket {
                request: 0,
                ground: 2,
                account: 1,
            },
            ground,
            client: client::Where::Native(PathBuf::from("/nonexistent/steam")),
            options: client::Options {
                root: PathBuf::from("/nonexistent/root"),
                home: PathBuf::from("/nonexistent/home"),
            },
            proven: client::Proven {
                pid: None,
                account: 1,
                just_signed_in: false,
            },
        };
        let why = standing.about_to_act().expect_err("the ground moved");
        assert!(why.contains("no longer the one signed in"), "{why}");
    }

    /// Every wake request is answered, under its own number, exactly once.
    ///
    /// The rule the shell above is entitled to assume. A second press made
    /// while a client is already coming up used to be dropped on the floor —
    /// the ask went nowhere — and the running wake's answer went to whoever
    /// happened to be waiting when it landed. Now it rides on the wake that is
    /// running, because that is exactly the client it wants, and gets an answer
    /// bearing its own number.
    /// Two games in flight are two watches, and ending one leaves the other
    /// running.
    ///
    /// The failure this is here for is the one the whole watch exists to stop,
    /// on whichever game did not happen to be last. There was one watch for the
    /// session, so starting the second screen's stopped the first screen's, and
    /// ending *either* screen's loading screen stopped both. Valve's client
    /// would then stop the surviving launch on a cloud save, or an agreement,
    /// or a launch option — behind a window this shell is holding off the
    /// screen — and nobody would hear it. What the person sees is a loading
    /// screen that waits out its whole patience and then says the game did not
    /// start.
    #[test]
    fn one_launch_ending_does_not_stop_another_launchs_watch() {
        let mut watching = Watching::default();
        let first = Arc::new(AtomicBool::new(true));
        let second = Arc::new(AtomicBool::new(true));
        watching.launches.insert(367520, first.clone());
        watching.launches.insert(1145360, second.clone());

        watching.stop_watching(Some(367520));
        assert!(!first.load(Ordering::SeqCst), "this launch is over");
        assert!(second.load(Ordering::SeqCst), "the other launch is not");
        assert_eq!(watching.launches.len(), 1);

        // And the one thing that does end every launch at once: the account
        // they were all started under signing out.
        watching.stop_watching(None);
        assert!(!second.load(Ordering::SeqCst));
        assert!(watching.launches.is_empty());
    }

    /// A game watched twice is watched once: the older thread is told to stop
    /// as the newer takes its place.
    ///
    /// A launch can stop twice — on a cloud save, and then on something else —
    /// so the shell arms the watch again after every answer, and two threads
    /// polling the same game would report the second question twice.
    #[test]
    fn watching_one_game_again_replaces_its_own_watch() {
        let mut watching = Watching::default();
        let older = Arc::new(AtomicBool::new(true));
        watching.launches.insert(367520, older.clone());

        watching.stop_watching(Some(367520));
        let newer = Arc::new(AtomicBool::new(true));
        watching.launches.insert(367520, newer.clone());

        assert!(!older.load(Ordering::SeqCst));
        assert!(newer.load(Ordering::SeqCst));
        assert_eq!(watching.launches.len(), 1);
    }

    /// A watcher that gave up on its own leaves nothing behind in the map.
    #[test]
    fn a_finished_watch_is_swept_out_of_the_map() {
        let mut watching = Watching::default();
        watching
            .launches
            .insert(367520, Arc::new(AtomicBool::new(false)));
        let running = Arc::new(AtomicBool::new(true));
        watching.launches.insert(1145360, running);

        watching.forget_finished_watches();
        assert_eq!(
            watching.launches.keys().copied().collect::<Vec<_>>(),
            [1145360]
        );
    }

    #[test]
    fn a_second_press_rides_on_the_wake_that_is_running_and_is_answered() {
        let (send, taken) = mpsc::channel();
        let (worker, _held) = mpsc::channel();
        let mut watching = Watching::default();
        let mut waking = Waking {
            in_flight: Some(in_flight(1, watching.generation)),
            ..Waking::default()
        };

        take_a_wake(
            2,
            false,
            &State::Out,
            &mut waking,
            &watching,
            &worker,
            &send,
        );
        assert_eq!(
            waking.in_flight.as_ref().map(|(_, riders)| riders.clone()),
            Some(vec![1, 2]),
            "the second press was dropped and would have taken the first's answer"
        );
        assert!(matches!(
            taken.try_recv(),
            Ok(Event::Client {
                ticket: Ticket { request: 2, .. },
                report: ClientReport::Waking
            })
        ));

        // And the one report about one client becomes one answer each.
        let (flying, _) = waking.in_flight.clone().expect("the wake that is running");
        a_wake_landed(
            flying,
            ClientReport::Ready,
            State::Out,
            &worker,
            &send,
            &mut watching,
            &mut waking,
        );
        let mut answered = Vec::new();
        while let Ok(Event::Client { ticket, report }) = taken.try_recv() {
            assert_eq!(report, ClientReport::Ready);
            answered.push(ticket.request);
        }
        assert_eq!(answered, vec![1, 2]);
        assert!(waking.in_flight.is_none(), "the hold was never let go of");
    }

    /// **The failure the envelope exists for**, from the worker's side.
    ///
    /// One account presses a game and a wake starts. That account signs out. The
    /// wake is most of two minutes into signing a client in and cannot be called
    /// back, so it lands anyway — and what it says is `Ready`, about a client
    /// signed in as somebody who is no longer here. Reported as it stands, that
    /// is the next account's press being answered by the last account's wake.
    ///
    /// Refused rather than dropped, and that half matters as much: a press
    /// waiting on a number nothing will ever say is a loading screen that ends
    /// only when its patience does.
    #[test]
    fn a_wake_whose_account_has_gone_is_refused_rather_than_reported_ready() {
        let (send, taken) = mpsc::channel();
        let (worker, _held) = mpsc::channel();
        let mut watching = Watching::default();
        let mut waking = Waking::default();
        let (flying, _) = in_flight(1, watching.generation);
        waking.in_flight = Some((flying, vec![1]));

        watching.nobody_is_signed_in_now();

        a_wake_landed(
            flying,
            ClientReport::Ready,
            State::Out,
            &worker,
            &send,
            &mut watching,
            &mut waking,
        );
        match taken.try_recv() {
            Ok(Event::Client { ticket, report }) => {
                assert_eq!(ticket.request, 1, "the answer named the wrong request");
                assert!(
                    matches!(report, ClientReport::Unavailable(_)),
                    "a client woken for an account that has gone was reported ready"
                );
            }
            other => panic!("the wake was never answered at all: {other:?}"),
        }
    }

    /// A wake asked on ground that has moved waits for the one running, and is
    /// started the moment it lands.
    ///
    /// Two clients started side by side is two of them fighting over one
    /// machine, and a thread already signing one in as another account cannot
    /// be called back — so the only honest answer is to wait. What must not
    /// happen is the request being forgotten in the meantime.
    #[test]
    fn a_wake_asked_on_new_ground_waits_and_is_still_answered() {
        let (send, taken) = mpsc::channel();
        let (worker, _held) = mpsc::channel();
        let mut watching = Watching::default();
        let mut waking = Waking::default();
        let (flying, _) = in_flight(1, watching.generation);
        waking.in_flight = Some((flying, vec![1]));

        // The account underneath the running wake goes, and somebody presses.
        watching.nobody_is_signed_in_now();
        take_a_wake(
            2,
            false,
            &State::Out,
            &mut waking,
            &watching,
            &worker,
            &send,
        );
        assert_eq!(
            waking.waiting_behind,
            vec![(2, false)],
            "the press was neither started nor queued"
        );
        assert_eq!(
            waking.in_flight.as_ref().map(|(_, riders)| riders.clone()),
            Some(vec![1]),
            "it rode on a wake belonging to an account that has gone"
        );

        a_wake_landed(
            flying,
            ClientReport::Ready,
            State::Out,
            &worker,
            &send,
            &mut watching,
            &mut waking,
        );
        assert!(
            waking.waiting_behind.is_empty(),
            "the queued press was never taken up"
        );

        // Both are answered. The second is refused here only because this test
        // has nobody signed in — what it is asserting is that it was answered
        // at all, under its own number.
        let mut answered = Vec::new();
        while let Ok(event) = taken.try_recv() {
            if let Event::Client { ticket, .. } = event {
                answered.push(ticket.request);
            }
        }
        assert!(answered.contains(&1), "the wake that landed said nothing");
        assert!(
            answered.contains(&2),
            "the press behind it was left waiting on an answer that never comes"
        );
    }

    /// An install is a thread that outlives the press by however long waking a
    /// cold client takes, which is most of a minute; somebody can sign out
    /// inside that, and somebody else can sign in. Without the generation, the
    /// answer arrived in whoever's library was on the screen by then — a row
    /// counting up towards a game that account does not own, or a failure
    /// panel about a press nobody there made.
    #[test]
    fn a_job_that_outlived_its_account_is_not_reported() {
        let (send, taken) = mpsc::channel();
        let mut watching = Watching::default();
        watching.fetching.insert(7, Moved::now());
        let began = watching.generation;

        watching.nobody_is_signed_in_now();
        assert!(
            watching.fetching.is_empty(),
            "the row is still being watched for in somebody else's library"
        );
        assert_ne!(watching.generation, began);

        // The install this account never asked for, coming back.
        came_back_for_a_test(
            Finished::Installing {
                app_id: 7,
                generation: began,
                how: Err(Stopped::Failed("no".to_string())),
            },
            State::Out,
            &send,
            &mut watching,
        );
        assert!(
            taken.try_recv().is_err(),
            "a press from a signed-out account reached the screen"
        );

        // And one belonging to the account that is here now is reported as it
        // always was — the generation must not simply silence everything. The
        // Steam moving under a job does the same thing, for the same reason:
        // see `a_job_whose_steam_moved_is_not_reported_either`.
        watching.fetching.insert(9, Moved::now());
        came_back_for_a_test(
            Finished::Installing {
                app_id: 9,
                generation: watching.generation,
                how: Err(Stopped::Failed("disk full".to_string())),
            },
            State::Out,
            &send,
            &mut watching,
        );
        assert!(matches!(
            taken.try_recv(),
            Ok(Event::InstallFailed { app_id: 9, .. })
        ));
        assert!(watching.fetching.is_empty());
    }

    /// Every kind of job there is, coming back after the account underneath it
    /// changed, and none of them says anything.
    ///
    /// [`a_job_that_outlived_its_account_is_not_reported`] covers an install.
    /// It is not the only thing that outlives a press: stopping one and
    /// removing a game are threads of their own, each of them a wake of a cold
    /// client and then a wizard, and each of them can land in a library
    /// belonging to somebody else. The drop is one early return in
    /// [`came_back`] and this is what holds every arm of it to that.
    ///
    /// An **account switch** rather than a sign-out, because that is the case
    /// where being wrong is worst: signing out leaves a bar with nothing on it,
    /// and switching leaves somebody looking at their own library with another
    /// person's game finishing on a row in it. Two moves of the generation, so
    /// a job started before the first is stale by two rather than by one.
    #[test]
    fn no_kind_of_job_survives_the_account_changing_under_it() {
        let _turn = one_at_a_time_with_the_environment();
        // The audit log is written from `came_back`, and a test has no business
        // appending to the record of what somebody's own session did to Steam.
        let state = std::env::temp_dir().join(format!("lxb-switch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        // SAFETY: the environment guard above is what makes this test the only
        // one touching this for its duration.
        let was = std::env::var_os("XDG_STATE_HOME");
        unsafe { std::env::set_var("XDG_STATE_HOME", &state) };

        let (send, taken) = mpsc::channel();
        let mut watching = Watching::default();
        watching.fetching.insert(11, Moved::now());
        watching.fetching.insert(12, Moved::now());
        watching.removing.insert(13);
        let theirs = watching.generation;

        // Signed out, and somebody else signed in behind them.
        watching.nobody_is_signed_in_now();
        watching.nobody_is_signed_in_now();
        assert!(watching.fetching.is_empty() && watching.removing.is_empty());
        assert_ne!(watching.generation, theirs);

        // Everything the four flows can come back as, all of it theirs.
        let stale = [
            Finished::Installing {
                app_id: 11,
                generation: theirs,
                how: Ok(()),
            },
            Finished::Installing {
                app_id: 11,
                generation: theirs,
                how: Err(Stopped::Failed("no room".to_string())),
            },
            Finished::StoppedInstalling {
                app_id: 12,
                generation: theirs,
                how: Ok(()),
            },
            Finished::StoppedInstalling {
                app_id: 12,
                generation: theirs,
                how: Err("the wizard would not".to_string()),
            },
            Finished::Uninstalling {
                app_id: 13,
                generation: theirs,
                how: Ok(()),
            },
            Finished::Uninstalling {
                app_id: 13,
                generation: theirs,
                how: Err("still running".to_string()),
            },
            // A list of compatibility tools is somebody's menu, and a menu
            // belonging to an account that has gone must not open over the
            // library that replaced it.
            Finished::Compatibility {
                which: webui::Which::Game(14),
                generation: theirs,
                how: Ok(webui::Compatibility::default()),
            },
            Finished::Compatibility {
                which: webui::Which::OtherTitles,
                generation: theirs,
                how: Err("Steam would not say".to_string()),
            },
        ];
        for job in stale {
            came_back_for_a_test(job, State::Out, &send, &mut watching);
        }
        assert!(
            taken.try_recv().is_err(),
            "somebody else's finished job reached this library"
        );

        // And the same, belonging to the account that is here now, are
        // reported exactly as they always were. The generation must silence the
        // stale ones and nothing else.
        watching.fetching.insert(21, Moved::now());
        watching.removing.insert(22);
        let now = watching.generation;
        came_back_for_a_test(
            Finished::StoppedInstalling {
                app_id: 21,
                generation: now,
                how: Ok(()),
            },
            State::Out,
            &send,
            &mut watching,
        );
        assert!(matches!(
            taken.try_recv(),
            Ok(Event::InstallStopped { app_id: 21 })
        ));
        assert!(watching.fetching.is_empty(), "the row is still counting");

        came_back_for_a_test(
            Finished::Uninstalling {
                app_id: 22,
                generation: now,
                how: Err("still running".to_string()),
            },
            State::Out,
            &send,
            &mut watching,
        );
        assert!(matches!(
            taken.try_recv(),
            Ok(Event::UninstallFailed { app_id: 22, .. })
        ));
        assert!(watching.removing.is_empty(), "the row is still removing");

        came_back_for_a_test(
            Finished::Compatibility {
                which: webui::Which::Game(23),
                generation: now,
                how: Ok(webui::Compatibility {
                    tools: vec![webui::Tool {
                        name: "proton_experimental".to_string(),
                        display: "Proton Experimental".to_string(),
                    }],
                    forced: Some("proton_experimental".to_string()),
                }),
            },
            State::Out,
            &send,
            &mut watching,
        );
        assert!(matches!(
            taken.try_recv(),
            Ok(Event::Compatibility {
                which: webui::Which::Game(23),
                ..
            })
        ));

        // SAFETY: as above.
        unsafe {
            match was {
                Some(was) => std::env::set_var("XDG_STATE_HOME", was),
                None => std::env::remove_var("XDG_STATE_HOME"),
            }
        }
        let _ = std::fs::remove_dir_all(&state);
    }

    /// Put an environment variable back the way it was found.
    ///
    /// # Safety
    ///
    /// Only from a test holding [`one_at_a_time_with_the_environment`].
    unsafe fn put_back(name: &str, was: Option<std::ffi::OsString>) {
        // SAFETY: the caller's guard is what makes this the only thread
        // touching the environment for its duration.
        unsafe {
            match was {
                Some(was) => std::env::set_var(name, was),
                None => std::env::remove_var(name),
            }
        }
    }

    /// The network going is not the account going, and the account going is not
    /// the network going.
    ///
    /// One function decides both — [`ended`] — and the two answers could hardly
    /// be further apart: one keeps the credential, the catalogue and every job
    /// in flight and tries again in five seconds; the other deletes the
    /// credential and the remembered library and ends the session. Getting them
    /// the wrong way round means either a train tunnel signing somebody out of
    /// their console, or a refused token retried for ever behind a row that
    /// says "Connecting to Steam".
    ///
    /// The **jobs surviving** is the part with no other test to its name. It is
    /// written into `Watching::generation`'s own doc — a blip must not throw
    /// away a download somebody is watching — and until now nothing held it
    /// there.
    /// A status is owed to Valve's client until the client's own record says it
    /// is wearing it — not until it has been handed one.
    ///
    /// The distinction is the whole of the user's second report. Handing a
    /// status over is posting a URL; it says Steam took it, and nothing about
    /// whether the friends menu moved. A rule that stopped at "it was handed
    /// over" is a rule that gives up on the one client that did not take it.
    /// Several statuses chosen in a second: the last one is what is owed, and
    /// the counter goes back to zero for it.
    ///
    /// **The ordering is structural rather than defended by a lock.** Every
    /// choice is one `Ask` on one worker thread, carried out in the order it
    /// was taken — the write to [`cm::Stands`], the write to disk, the
    /// announcement to this session, and the URL handed to Valve's client are
    /// all inside the same arm of [`answer`] — so a later choice cannot begin
    /// until the earlier one has finished. This test states what that buys:
    /// whatever order the presses came in, what is owed at the end is the last
    /// of them.
    ///
    /// The URL courier being on that same thread is the substance. Two detached
    /// threads would each hand Valve's client a URL, and the one that started
    /// first is not the one that arrives first: somebody who chose Away and
    /// then Online could end up Away, in a window they are looking at, with
    /// nothing on the screen to say why.
    #[test]
    fn the_last_status_chosen_is_the_one_that_is_owed() {
        use friends::Presence;

        let stands = cm::Status::default();
        let chosen = [
            Presence::Online,
            Presence::Away,
            Presence::Invisible,
            Presence::Online,
            Presence::Offline,
        ];
        let mut told: u8 = 2;
        for status in chosen {
            let mut held = stands.lock().expect("never poisoned");
            held.owed = Some(status);
            held.announced = Some(status);
            drop(held);
            // A fresh choice is owed to the client from scratch, whatever it
            // had been told about the one before.
            told = 0;
        }
        let held = stands.lock().expect("never poisoned");
        assert_eq!(held.owed, Some(Presence::Offline), "an older choice won");
        assert_eq!(held.announced, Some(Presence::Offline));
        // And the last of them is what a client is handed.
        assert_eq!(
            what_a_client_is_owed(held.owed, Some(Presence::Online), true, told),
            Owing::HandOver
        );
        // Offline is handed over once and never again, because Valve's client
        // does not write it down and the agreement would never come.
        assert_eq!(
            what_a_client_is_owed(held.owed, Some(Presence::Online), true, 1),
            Owing::Told
        );
    }

    #[test]
    fn a_status_is_owed_until_the_client_is_wearing_it() {
        use friends::Presence;

        // Nothing chosen: nothing is owed, whatever a client is doing.
        assert_eq!(
            what_a_client_is_owed(None, Some(Presence::Online), true, 0),
            Owing::Nothing
        );

        // Chosen with no client running — the case both reports are about.
        // There is nobody to tell, and the counter left alone at zero is what
        // makes the *next* client to come up get told.
        assert_eq!(
            what_a_client_is_owed(Some(Presence::Invisible), None, false, 0),
            Owing::Nothing
        );

        // It comes up wearing what it remembered, which is not this. So it is
        // told, and goes on being told while its record disagrees — up to the
        // point where a client that has refused three is not going to take a
        // fourth.
        for told in 0..TIMES_A_CLIENT_IS_TOLD {
            assert_eq!(
                what_a_client_is_owed(
                    Some(Presence::Invisible),
                    Some(Presence::Online),
                    true,
                    told
                ),
                Owing::HandOver,
                "still disagreeing after {told} of them"
            );
        }
        assert_eq!(
            what_a_client_is_owed(
                Some(Presence::Invisible),
                Some(Presence::Online),
                true,
                TIMES_A_CLIENT_IS_TOLD
            ),
            Owing::Told
        );

        // And when its record agrees, it landed: nothing is owed any more, and
        // from then on this session follows the client rather than the other
        // way about.
        assert_eq!(
            what_a_client_is_owed(
                Some(Presence::Invisible),
                Some(Presence::Invisible),
                true,
                1
            ),
            Owing::Landed
        );

        // Offline is the one a client never writes down, so it is handed over
        // once and then let be. Going on would be handing Valve's client a URL
        // every ten seconds waiting for a record that is never written.
        assert_eq!(
            what_a_client_is_owed(Some(Presence::Offline), Some(Presence::Online), true, 0),
            Owing::HandOver
        );
        assert_eq!(
            what_a_client_is_owed(Some(Presence::Offline), Some(Presence::Online), true, 1),
            Owing::Told
        );
    }

    #[test]
    fn a_status_courier_only_targets_the_same_account() {
        let us = 76561198000000001u64;
        let our_account = us as u32;
        assert!(client_has_account(client::State::SignedIn(our_account), us));
        assert!(client_has_account(client::State::Offline(our_account), us));
        assert!(!client_has_account(
            client::State::SignedIn(our_account.wrapping_add(1)),
            us
        ));
        assert!(!client_has_account(client::State::Starting, us));
        assert!(!client_has_account(client::State::Stopped, us));
    }

    #[test]
    fn a_new_client_choice_releases_an_offline_debt() {
        use friends::Presence;

        assert!(client_choice_supersedes_offline(
            Some(Presence::Offline),
            1,
            Some(Presence::Online),
            Some(Presence::Away),
        ));
        assert!(!client_choice_supersedes_offline(
            Some(Presence::Offline),
            1,
            Some(Presence::Online),
            Some(Presence::Online),
        ));
        assert!(!client_choice_supersedes_offline(
            Some(Presence::Offline),
            0,
            Some(Presence::Online),
            Some(Presence::Away),
        ));
        assert!(!client_choice_supersedes_offline(
            Some(Presence::Away),
            1,
            Some(Presence::Online),
            Some(Presence::Away),
        ));
    }

    /// What this session announces, which is also what the panel draws.
    ///
    /// The screenshot behind the third report was a panel reading Invisible
    /// beside Valve's own window reading Online, on a session where nobody had
    /// chosen anything: the shell was announcing a status of its own invention
    /// and showing it. What it announces now is what the machine is already set
    /// to, and the fallback is reached only where there is no Steam here to
    /// read.
    #[test]
    fn this_session_stands_where_the_machine_does() {
        use friends::Presence;

        // Nobody has chosen here: the machine's own status, as Valve's client
        // keeps it. Not Invisible, which is what disagreed with the screenshot.
        assert_eq!(
            where_this_machine_stands(None, Some(Presence::Online)),
            Presence::Online
        );

        // Somebody has: theirs, because it is the most recent thing said and
        // the client is about to be wearing it too.
        assert_eq!(
            where_this_machine_stands(Some(Presence::Away), Some(Presence::Online)),
            Presence::Away
        );

        // And a machine whose Steam has never been opened has nothing to
        // mirror, so this session speaks only for itself.
        assert_eq!(
            where_this_machine_stands(None, None),
            friends::AS_QUIETLY_AS_IT_CAN
        );
    }

    #[test]
    fn a_blip_keeps_everything_and_a_refused_credential_keeps_nothing() {
        let _turn = one_at_a_time_with_the_environment();
        let home = std::env::temp_dir().join(format!("lxb-reconnect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let was_home = std::env::var_os("HOME");
        let was_data = std::env::var_os("XDG_DATA_HOME");
        let was_state = std::env::var_os("XDG_STATE_HOME");
        // SAFETY: under the guard above. A home of its own because this test
        // deletes a stored credential, and the one on this machine is real.
        unsafe {
            std::env::set_var("HOME", &home);
            std::env::set_var("XDG_DATA_HOME", home.join("data"));
            std::env::set_var("XDG_STATE_HOME", home.join("state"));
        }

        let steam_id = 76561198042371721;
        let stored = || {
            session::Stored::granted(
                "someone".to_string(),
                steam_id,
                "not a real token".to_string(),
                None,
            )
        };
        let owned = || vec![Game::invented(504230, "Celeste".to_string(), false)];
        let put_it_all_back = || {
            stored().save().unwrap();
            catalogue::keep(steam_id, &owned());
        };

        // A blip. Somebody is fetching one game and removing another.
        put_it_all_back();
        let (send, taken) = mpsc::channel();
        let mut watching = Watching::default();
        watching.fetching.insert(7, Moved::now());
        watching.removing.insert(8);
        let before = watching.generation;
        let blip = cm::Failure::Temporary("no route to host".to_string());
        let said = blip.said();
        let state = ended(stored(), true, false, owned(), blip, &send, &mut watching);

        match state {
            State::Reconnecting { next, owned, .. } => {
                let in_a_moment = next.saturating_duration_since(Instant::now());
                assert!(
                    in_a_moment > Duration::ZERO && in_a_moment <= Duration::from_secs(5),
                    "a retry that is not coming: {in_a_moment:?}"
                );
                assert_eq!(owned.len(), 1, "the library was dropped on the way past");
            }
            _ => panic!("a blip ended the session"),
        }
        let heard: Vec<Event> = taken.try_iter().collect();
        assert!(
            matches!(heard.as_slice(), [Event::Reach(Reach::Offline(why))] if *why == said),
            "a blip said something other than which it was: {heard:?}"
        );
        assert_eq!(watching.generation, before, "a blip moved the generation");
        assert!(
            watching.fetching.contains_key(&7) && watching.removing.contains(&8),
            "a blip threw away what somebody was watching"
        );
        assert!(
            session::Stored::load().is_some(),
            "a blip deleted the token"
        );
        assert!(
            catalogue::restore(steam_id).is_some(),
            "a blip deleted the remembered library"
        );

        // And a credential Steam will not have again. Result 5 is
        // `InvalidPassword`, which is what an expired authorization comes back
        // as; see `Failure::permanently_rejected`.
        let (send, taken) = mpsc::channel();
        let state = ended(
            stored(),
            true,
            false,
            owned(),
            cm::Failure::Rejected(5),
            &send,
            &mut watching,
        );
        assert!(matches!(state, State::Out));
        let heard: Vec<Event> = taken.try_iter().collect();
        assert!(
            matches!(
                heard.as_slice(),
                [Event::SignedOut, Event::Library(games)] if games.is_empty()
            ),
            "a refused credential did not empty the bar: {heard:?}"
        );
        assert_ne!(watching.generation, before, "the jobs are still somebody's");
        assert!(watching.fetching.is_empty() && watching.removing.is_empty());
        assert!(session::Stored::load().is_none(), "the token was kept");
        assert!(
            catalogue::restore(steam_id).is_none(),
            "the last account's library is still on the disk"
        );

        // The same refusal while somebody is watching a sign-in panel is a
        // sentence on that panel instead — they are standing there waiting for
        // it, and an empty bar with no explanation is not an answer.
        put_it_all_back();
        let (send, taken) = mpsc::channel();
        let state = ended(
            stored(),
            false,
            true,
            owned(),
            cm::Failure::Rejected(5),
            &send,
            &mut watching,
        );
        assert!(matches!(state, State::Out));
        let heard: Vec<Event> = taken.try_iter().collect();
        assert!(
            matches!(heard.as_slice(), [Event::SignInFailed(_)]),
            "the panel was left with nothing on it: {heard:?}"
        );

        // SAFETY: as above, and still under the same guard.
        unsafe {
            put_back("HOME", was_home);
            put_back("XDG_DATA_HOME", was_data);
            put_back("XDG_STATE_HOME", was_state);
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A session that comes up with a stored account says so before Steam is
    /// reached for, and shows the library it remembered.
    ///
    /// This is the offline failure. Everything was waited for: the account was
    /// announced only when CM answered, so a machine with no network came up
    /// looking *signed out* — an empty Steam column, and a "Sign in to Steam"
    /// row that did nothing at all when pressed, because the worker knew
    /// perfectly well it was not signed out and refused to begin a second
    /// sign-in. The one moment somebody wants the games already on their disk
    /// was the one moment the shell could not list them.
    fn a_stored_session() -> session::Stored {
        session::Stored {
            account: "someone".to_string(),
            steam_id: 76561198042371721,
            refresh_token: "not a real token".to_string(),
            guard_data: None,
            machine_id: "not a real machine".to_string(),
        }
    }

    /// A session with no network is signed in, and its client may be started.
    ///
    /// This is the whole of what made offline play impossible: every press went
    /// through a match that only accepted `State::In`, which is the state a
    /// session reaches by being *answered* by Steam. So a machine with no
    /// network came up with the account's name on the screen, a column of games
    /// that were on the disk, and a Play that answered "Steam is not signed in".
    #[test]
    fn a_session_that_cannot_reach_steam_may_still_start_the_client() {
        let reconnecting = State::Reconnecting {
            stored: a_stored_session(),
            next: Instant::now(),
            announced: true,
            failure_to_panel: false,
            owned: Vec::new(),
        };
        let (stored, need) = holding(&reconnecting).expect("a session holding a credential");
        assert_eq!(stored.account, "someone");
        // And it is the one wake that may put Valve's client into Offline Mode,
        // because it is the one that has evidence there is nothing to reach.
        assert_eq!(need, client::Need::Offline);
    }

    /// A session still reaching for Steam has no such evidence.
    ///
    /// The distinction is not pedantry: a boot that was going to succeed spends
    /// a second or two connecting, and a press made in that second must not
    /// leave somebody's Steam coming up offline for the rest of the week.
    #[test]
    fn a_session_still_connecting_does_not_take_the_client_offline() {
        let connecting = State::Connecting {
            stored: a_stored_session(),
            generation: 1,
            announced: false,
            failure_to_panel: false,
            owned: Vec::new(),
            cancel: cm::Cancel::cancelling_nothing(),
        };
        assert_eq!(
            holding(&connecting).map(|(_, need)| need),
            Some(client::Need::SignedIn)
        );
        // And a session with nobody signed in holds nothing at all, which is
        // still the one refusal that was always right.
        assert!(holding(&State::Out).is_none());
        assert_eq!(out_of_reach(&State::Out), "Steam is not signed in");
        assert_eq!(
            out_of_reach(&connecting),
            "Steam is still being reached",
            "and it is not the same sentence as having failed"
        );
    }

    #[test]
    fn a_stored_account_is_announced_before_steam_is_reached_for() {
        let _turn = one_at_a_time_with_the_environment();
        let home = std::env::temp_dir().join(format!("lxb-restore-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let was_home = std::env::var_os("HOME");
        let was_data = std::env::var_os("XDG_DATA_HOME");
        let was_cache = std::env::var_os("XDG_CACHE_HOME");
        // SAFETY: the environment guard above is what makes this test the only
        // one touching these for its duration.
        //
        // `HOME` among them, and deliberately: the library half of what this
        // announces is read off the real disk, and this machine has Steam on
        // it. Without a home of its own the test would assert against whatever
        // its author happens to have installed — see the project's note on
        // tests that read the machine.
        unsafe {
            std::env::set_var("HOME", &home);
            std::env::set_var("XDG_DATA_HOME", home.join("data"));
            std::env::set_var("XDG_CACHE_HOME", home.join("cache"));
        }

        session::Stored::granted(
            "someone".to_string(),
            76561198042371721,
            "not a real token".to_string(),
            None,
        )
        .save()
        .unwrap();
        catalogue::keep(
            76561198042371721,
            &[Game::invented(504230, "Celeste".to_string(), false)],
        );

        let (send_events, events) = mpsc::channel();
        let (worker, _asks) = mpsc::channel();
        let mut generation = 0;
        let state = restore(
            &worker,
            &send_events,
            &mut generation,
            &cm::Status::default(),
        );

        // It is connecting — nothing has answered yet, and that is the point.
        assert!(matches!(state, State::Connecting { .. }));

        let said: Vec<Event> = events.try_iter().collect();
        assert!(
            matches!(said.first(), Some(Event::SignedIn(account)) if account.name == "someone"),
            "the account was not announced until Steam answered: {said:?}"
        );
        assert!(
            said.iter()
                .any(|event| matches!(event, Event::Reach(Reach::Restoring))),
            "nothing said Steam was still being reached for"
        );
        assert!(
            said.iter()
                .any(|event| matches!(event, Event::LibraryAsOf(Some(_)))),
            "the library on screen is a memory and is not dated"
        );
        let library = said
            .iter()
            .find_map(|event| match event {
                Event::Library(games) => Some(games),
                _ => None,
            })
            .expect("a library before Steam answered");
        assert_eq!(library.len(), 1);
        assert_eq!(library[0].name, "Celeste");

        // And nothing said the session was signed out, which is what the shell
        // drew from the silence.
        assert!(!said.iter().any(|event| matches!(event, Event::SignedOut)));

        session::Stored::forget();
        catalogue::forget();
        // SAFETY: as above, and still under the same guard.
        //
        // All three of them, not just `HOME`. A test that leaves `XDG_DATA_HOME`
        // pointing into a directory it is about to delete leaves every later
        // test in the process reading a home that is not there — and only the
        // ones that take the environment guard would ever notice.
        unsafe {
            put_back("HOME", was_home);
            put_back("XDG_DATA_HOME", was_data);
            put_back("XDG_CACHE_HOME", was_cache);
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    /// The Steam changing underneath a session ends this session's interest in
    /// whatever it had in flight, exactly as an account change does.
    ///
    /// A download is watched by reading one root's manifests. Install a Flatpak
    /// beside a native client, or take a native client off, and the file the
    /// job is waiting for is in a directory nothing looks at any more — so the
    /// row counts nothing, for ever, and no event ever ends it.
    #[test]
    fn a_job_whose_steam_moved_is_not_reported_either() {
        let mut watching = Watching::default();
        watching.fetching.insert(7, Moved::now());

        // The first look is not a change: there is nothing yet to have moved,
        // and clearing here would throw away a press made moments earlier.
        let native = PathBuf::from("/home/someone/.local/share/Steam");
        let began = watching.generation;
        watching.the_backend_is_now(Some(&native));
        assert_eq!(watching.generation, began);
        assert!(watching.fetching.contains_key(&7));

        // Asked again with the same answer, which is every ten seconds for the
        // life of an ordinary session.
        watching.the_backend_is_now(Some(&native));
        assert_eq!(watching.generation, began);
        assert!(watching.fetching.contains_key(&7));

        // The same directory under the name the client's own symbolic link
        // gives it, which is what a first install turns the root into the
        // moment the client has run, is not a move. On the disk, because the
        // question is about the disk.
        let scratch =
            std::env::temp_dir().join(format!("lxb-backend-moved-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        let unpacked = scratch.join(".local/share/Steam");
        std::fs::create_dir_all(&unpacked).unwrap();
        std::fs::create_dir_all(scratch.join(".steam")).unwrap();
        let linked = scratch.join(".steam/steam");
        std::os::unix::fs::symlink(&unpacked, &linked).unwrap();
        let mut here = Watching::default();
        here.fetching.insert(7, Moved::now());
        here.the_backend_is_now(Some(&unpacked));
        let stood = here.generation;
        here.the_backend_is_now(Some(&linked));
        assert_eq!(
            here.generation, stood,
            "one directory under two names is one Steam"
        );
        assert!(here.fetching.contains_key(&7));
        let _ = std::fs::remove_dir_all(&scratch);

        // And then the ground moves.
        let flatpak =
            PathBuf::from("/home/someone/.var/app/com.valvesoftware.Steam/.local/share/Steam");
        watching.the_backend_is_now(Some(&flatpak));
        assert_ne!(watching.generation, began);
        assert!(
            watching.fetching.is_empty(),
            "a row is still counting a download in a Steam nothing reads any more"
        );
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
        watching.fetching.insert(7, Moved::now());

        watching.report(
            &disk(vec![on_disk(7, library::Standing::Downloading)]),
            None,
            &send,
        );
        assert!(
            matches!(
                taken.try_recv(),
                Ok(Event::Installing {
                    app_id: 7,
                    done: 30,
                    total: 100,
                    live: None,
                })
            ),
            "how far it has got has to reach the row"
        );
        assert!(watching.fetching.contains_key(&7), "it is not there yet");

        // And once it is on the disk and playable, it is said once and then
        // stopped being watched for: nothing else in the session can tell the
        // row to stop counting.
        watching.report(
            &disk(vec![on_disk(7, library::Standing::Ready)]),
            None,
            &send,
        );
        assert!(matches!(
            taken.try_recv(),
            Ok(Event::Installed { app_id: 7, .. })
        ));
        assert!(watching.fetching.is_empty());
        watching.report(
            &disk(vec![on_disk(7, library::Standing::Ready)]),
            None,
            &send,
        );
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
        watching.fetching.insert(7, Moved::now());

        watching.report(&disk(Vec::new()), None, &send);
        assert!(taken.try_recv().is_err());
        assert!(
            watching.fetching.contains_key(&7),
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

        watching.report(
            &disk(vec![on_disk(7, library::Standing::Ready)]),
            None,
            &send,
        );
        assert!(taken.try_recv().is_err(), "it is still there");

        watching.report(&disk(Vec::new()), None, &send);
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

        watching.report(
            &disk(vec![on_disk(7, library::Standing::Downloading)]),
            None,
            &send,
        );
        assert!(taken.try_recv().is_err());
    }

    /// A download that stops moving without finishing stops being counted.
    ///
    /// The third answer, and it used to be silence. Steam pauses a download, or
    /// what arrived turns out to need repairing: neither is an arrival and
    /// neither is a failure, so nothing was ever said and the row went on
    /// counting the same percentage for the rest of the session. Nothing is
    /// announced now either — the library says "Download paused" or "Needs
    /// repairing" in its own words, which is a better answer than a panel —
    /// but the counting ends.
    #[test]
    fn a_download_that_stops_moving_stops_being_counted() {
        for standing in [library::Standing::Paused, library::Standing::Broken] {
            let (send, taken) = mpsc::channel();
            let mut watching = Watching::default();
            watching.fetching.insert(7, Moved::now());

            watching.report(&disk(vec![on_disk(7, standing)]), None, &send);
            assert!(
                matches!(taken.try_recv(), Ok(Event::InstallWaiting { app_id: 7 })),
                "{standing:?} left the row counting a download that had stopped"
            );
            assert!(
                taken.try_recv().is_err(),
                "and nothing else is said about it"
            );
            assert!(watching.fetching.is_empty());
        }
    }

    /// The shell keeps hold of an install through the states it opens in, and
    /// never counts a percentage off the previous operation's numbers.
    ///
    /// Both halves of one bug. Valve's client opens a fresh install with no
    /// working bit set at all — `Update Required,` then
    /// `Update Required,Update Queued,` — which the shell read as a pause, and
    /// a pause is [`library::Standing::waiting_for_somebody`], so it let go of
    /// the install two seconds after being asked to make it. Everything the row
    /// said afterwards came off the disk with nobody watching.
    ///
    /// And on the states that are not a download the manifest's two byte counts
    /// are whatever the last operation left — an install passes through a
    /// verify on its way in — so they are not sent at all rather than sent as a
    /// percentage of something else.
    #[test]
    fn an_install_is_still_watched_through_the_states_it_opens_in() {
        let (send, taken) = mpsc::channel();
        let mut watching = Watching::default();
        watching.fetching.insert(7, Moved::now());

        // Queued: asked for, nothing arrived. Counted, and counted honestly.
        let mut queued = on_disk(7, library::Standing::Queued);
        queued.downloaded = 0;
        queued.to_download = 656_543_488;
        watching.report(&disk(vec![queued]), None, &send);
        assert!(
            matches!(
                taken.try_recv(),
                Ok(Event::Installing {
                    app_id: 7,
                    done: 0,
                    total: 656_543_488,
                    live: None,
                })
            ),
            "a queued install is not a paused one"
        );
        assert!(
            watching.fetching.contains_key(&7),
            "and it is still watched"
        );

        // The verify Steam runs on its way in. The two counts belong to
        // whatever it last did, so nothing is claimed about how far along this
        // is — and the watch is kept, because a verify finishes on its own.
        let mut checking = on_disk(7, library::Standing::Validating);
        checking.downloaded = 116_304;
        checking.to_download = 116_304;
        watching.report(&disk(vec![checking]), None, &send);
        assert!(
            matches!(
                taken.try_recv(),
                Ok(Event::Installing {
                    app_id: 7,
                    done: 0,
                    total: 0,
                    live: None,
                })
            ),
            "the last operation's bytes are not this one's percentage"
        );
        assert!(watching.fetching.contains_key(&7));

        // And the bytes are the bytes once they are actually this download's.
        let mut coming = on_disk(7, library::Standing::Downloading);
        coming.downloaded = 60_226_480;
        coming.to_download = 656_543_488;
        watching.report(&disk(vec![coming]), None, &send);
        assert!(matches!(
            taken.try_recv(),
            Ok(Event::Installing {
                app_id: 7,
                done: 60_226_480,
                total: 656_543_488,
                live: None,
            })
        ));
    }

    /// What the client says about a download reaches the row it is about, and
    /// no other.
    ///
    /// There is one download at a time in Steam, and the overview names which.
    /// A row counting somebody else's percentage would be worse than a row
    /// counting nothing.
    #[test]
    fn the_clients_own_count_reaches_only_the_game_it_is_about() {
        let live = |app_id: u32| {
            Some(webui::Live {
                app_id,
                percent: 47,
                bytes_per_second: 61_937_624,
                seconds_left: Some(5),
                moving: true,
            })
        };
        let counted = |about: u32| {
            let (send, taken) = mpsc::channel();
            let mut watching = Watching::default();
            watching.fetching.insert(7, Moved::now());
            let mut coming = on_disk(7, library::Standing::Downloading);
            coming.downloaded = 0;
            coming.to_download = 656_543_488;
            watching.report(&disk(vec![coming]), live(about), &send);
            match taken.try_recv() {
                Ok(Event::Installing { live, .. }) => live,
                other => panic!("{other:?} is not an install"),
            }
        };

        assert_eq!(counted(7), live(7), "the game being watched");
        assert_eq!(counted(945_360), None, "somebody else's download");
    }

    /// A download that says it is running while nothing arrives.
    ///
    /// The one failure the manifest cannot describe, and the one this shell
    /// could not see. Every state that *stops* is written down by Valve's
    /// client — paused, needing repair, being removed — and all of them are
    /// read off the disk. A client that goes on claiming to download while
    /// nothing comes writes nothing at all, so the row counted the same
    /// percentage until the session was restarted.
    #[test]
    fn a_download_that_says_it_is_running_and_is_not_says_so() {
        let (send, taken) = mpsc::channel();
        let mut watching = Watching::default();
        watching.fetching.insert(7, Moved::now());
        let downloading = |done: u64| {
            let mut about = on_disk(7, library::Standing::Downloading);
            about.downloaded = done;
            about.to_download = 100;
            disk(vec![about])
        };

        // An hour and a byte ago. Nothing has arrived since, which is the whole
        // of what is wrong with it.
        watching.report(&downloading(30), None, &send);
        assert!(matches!(taken.try_recv(), Ok(Event::Installing { .. })));
        assert!(taken.try_recv().is_err(), "nothing is wrong yet");

        let long_ago = Instant::now() - STUCK_AFTER - Duration::from_secs(1);
        watching.fetching.get_mut(&7).unwrap().since = long_ago;
        watching.report(&downloading(30), None, &send);
        assert!(matches!(taken.try_recv(), Ok(Event::Installing { .. })));
        assert!(
            matches!(taken.try_recv(), Ok(Event::InstallStuck { app_id: 7, .. })),
            "a download that has written nothing for an hour said nothing about it"
        );
        // And it is still watched: nothing is cancelled and nothing is
        // deleted, because nothing here knows that it will not come back.
        assert!(watching.fetching.contains_key(&7));

        // Said once, not on every look. This is asked every ten seconds.
        watching.report(&downloading(30), None, &send);
        assert!(matches!(taken.try_recv(), Ok(Event::Installing { .. })));
        assert!(taken.try_recv().is_err(), "it said so twice");

        // And a byte arriving takes it back, clock and all.
        watching.report(&downloading(31), None, &send);
        assert!(matches!(taken.try_recv(), Ok(Event::Installing { .. })));
        assert!(!watching.fetching[&7].told);
        assert!(watching.fetching[&7].since > long_ago);
    }

    /// What must not be called stuck: the end of every large install.
    ///
    /// `BytesDownloaded` stops changing the moment the client has everything
    /// and starts unpacking it into place, which on a large game and a slow
    /// disk is a long half hour of a manifest that does not move. A rule that
    /// read that as a stall would be wrong on every big game anybody installed.
    #[test]
    fn a_download_that_has_everything_is_not_stuck_while_it_is_unpacked() {
        let (send, taken) = mpsc::channel();
        let mut watching = Watching::default();
        watching.fetching.insert(7, Moved::now());
        watching.fetching.get_mut(&7).unwrap().since =
            Instant::now() - STUCK_AFTER - Duration::from_secs(1);

        let mut about = on_disk(7, library::Standing::Downloading);
        about.downloaded = 100;
        about.to_download = 100;
        watching.report(&disk(vec![about]), None, &send);

        assert!(matches!(taken.try_recv(), Ok(Event::Installing { .. })));
        assert!(
            taken.try_recv().is_err(),
            "an install being unpacked was called a stalled download"
        );
        assert!(!watching.fetching[&7].told);
    }

    /// One being taken off the disk is still on its way somewhere, and is still
    /// counted: it is moving, and the row has something true to say.
    #[test]
    fn a_removal_in_flight_is_still_watched() {
        let (send, taken) = mpsc::channel();
        let mut watching = Watching::default();
        watching.fetching.insert(7, Moved::now());

        watching.report(
            &disk(vec![on_disk(7, library::Standing::Uninstalling)]),
            None,
            &send,
        );
        assert!(matches!(taken.try_recv(), Ok(Event::Installing { .. })));
        assert!(watching.fetching.contains_key(&7));
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
