//! Steam, as the shell holds it: one account, one panel, one column.
//!
//! The client itself is a crate of its own — see `lxb-steam` — because none of
//! what it does may happen on the thread that draws. This is the half that
//! belongs to the shell: what is on screen while somebody signs in, what has
//! been typed into it, and the rows the library turns into.
//!
//! ## The sign-in, as a panel
//!
//! Signing in is a conversation, and the panel is one question at a time. What
//! makes it a state machine rather than a screen is that Steam decides which
//! questions there are: an account with a phone authenticator is confirmed by
//! pressing something on the phone, one without is confirmed by a code from an
//! email, and a sign-in by photographed code skips the whole of it. So the
//! shell asks the client what it is waiting for and draws that, and the stages
//! below are the complete list of things it can be waiting for.
//!
//! ```text
//!                    ┌──────────► Qr ──────────┐
//!   press Steam ─► Choosing                    ├──► Waiting ──► signed in
//!                    └─► Account ─► Password ──┘        │
//!                                                       ▼
//!                                                     Code ──► Waiting
//! ```
//!
//! Every stage can be cancelled, and any of them can end in [`Stage::Failed`],
//! which is the only stage that offers to start again — everything else offers
//! to give up, because a panel that could be dismissed *into* another attempt
//! is a panel a user cannot leave.
//!
//! ## Why the password is the shell's own type
//!
//! [`crate::secret::Secret`] is what collects it, exactly as it does for the
//! uninstall panel and for polkit, and it hands itself over the one way it
//! knows how: by writing itself down a sink. The sink here is
//! [`lxb_steam::Password`], which is the client crate's own overwritten
//! buffer. So the password goes field → sink → RSA, is never a `String`, is
//! never in the layout, and both halves of the journey are held by a type
//! whose whole job is to overwrite itself afterwards.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use lxb_steam::{Confirmation, Doing, Event, Game, Stopped};

use crate::dialog;
use crate::menu;
use crate::secret::Secret;

/// How often the machine is asked whether Valve's client is still installed.
///
/// Ten seconds because that is the rate the library already refreshes at, and
/// because the thing being watched for is somebody at a terminal removing a
/// package. Nothing waits on this: every press looks the client up for itself.
const AFTER_THE_CLIENT: Duration = Duration::from_secs(10);

/// And how often while one has been asked to shut down and has not gone.
///
/// Ten seconds is the rate of a question nobody is waiting on. This one is
/// waited on: `-shutdown` returns when the client has been *asked*, and until
/// the next look the shell cannot tell a client that went from one that
/// declined — so the half of the session that decides what to do about it was
/// deciding on evidence up to ten seconds old, and would ask a client that had
/// already gone to go again. See [`crate::what_becomes_of_the_client`].
const WHILE_THE_CLIENT_GOES: Duration = Duration::from_secs(1);

/// And how long that is worth doing for.
///
/// Longer than [`lxb_steam::client::UNTIL_IT_STOPS`], which is the crate's own
/// figure for how long a client may take to let go of its pipe: past that it is
/// not a client on its way out, it is a client that said no, and the answer to
/// that is not to keep looking. Read off that constant plus a few seconds of
/// slack, so that changing the wait cannot leave this behind.
pub const UNTIL_THE_CLIENT_GOES: Duration =
    Duration::from_secs(lxb_steam::client::UNTIL_IT_STOPS.as_secs() + 5);

/// How often the client's own log is asked what it has in hand.
///
/// A file read, on a question decided sixty times a second for the length of a
/// countdown — so it is cached, and this is how stale the answer may be. One
/// second against the five that decision waits, and against the thirty-six a
/// check took here.
const WHAT_VALVE_IS_DOING: Duration = Duration::from_secs(1);

/// The keystroke Valve's overlay comes up on, as Linux key codes: Shift, then
/// Tab.
///
/// Read off a live client rather than assumed. `overlay_key` in the client's
/// own settings store reads
/// `{ key_code: 65289, shift_key: true, display_name: "Shift+Tab" }` on Steam
/// stable build 1785799196 with the setting never touched — 65289 is `XK_Tab` —
/// and `gamescope_guide_hotkey`, which is the same question asked for a machine
/// that *is* shaped like a console, is the same two keys again.
///
/// Sent as key **codes** because that is what `lxb_shell_v1.keyboard_key`
/// carries, and here the two cannot disagree: Tab and Shift are in the same
/// place on every layout xkb has, which is a large part of why Valve could pick
/// them. A keysym would have to be turned back into a code against a keymap the
/// compositor is holding and the shell is not.
///
/// **A user who has moved Steam's overlay elsewhere is not followed.** The
/// setting lives in the running client's memory and is written to no file until
/// it is changed, so reading it means the client's debugging interface open —
/// which this shell holds for seconds during a sign-in and never while a game
/// is running. `cargo run -p lxb-steam --example probe-client -- --context`
/// prints what the client is actually set to, which is how the default above
/// was established and how a machine where it is wrong can be told.
pub const OVERLAY_CHORD: [u32; 2] = [KEY_LEFTSHIFT, KEY_TAB];

/// `KEY_TAB` and `KEY_LEFTSHIFT` from the kernel's `input-event-codes.h`, which
/// is the same numbering `wl_keyboard` reports and
/// [`crate::controller::KEY_UP`] and its three neighbours are written in.
const KEY_TAB: u32 = 15;
const KEY_LEFTSHIFT: u32 = 42;

/// Steam as one field of the shell.
/// The account an invented session is signed in as, and the first of the
/// people it knows.
///
/// Numbers no real account can have: Steam's own individual accounts start at
/// `76561197960265728`, so nothing here can collide with a real conversation
/// even if a fixture and a live session were somehow in one process.
const INVENTED_ACCOUNT: u64 = 1;
const INVENTED_FRIEND: u64 = 1_000;

/// What an invented conversation says.
///
/// Deliberately dull and deliberately about nothing: a fixture that read like a
/// real conversation is one somebody screenshots and publishes by accident.
const INVENTED_LINES: [&str; 4] = [
    "this is an invented message and was never sent to anybody",
    "it is here so the panel has something to draw",
    "a longer one, so that the column has a message in it that wraps over more than a single line and the bubble has to grow to hold it",
    "ok",
];

pub struct Steam {
    client: lxb_steam::Steam,
    /// Who is signed in, if anybody. The shell's echo of what the worker last
    /// said, because the Games row is drawn from it on every rebuild.
    account: Option<String>,
    /// And that account's number, for the diagnostics panel and nothing else.
    ///
    /// Never shown whole. What the panel prints is its last four digits, which
    /// is enough to tell one household account from another on a screen
    /// somebody is looking at beside their own — and is not an identifier. See
    /// [`Steam::diagnostics`].
    steam_id: Option<u64>,
    /// The library as it stands, in the order it arrives — installed first,
    /// each half by name. What order it goes on the *bar* in is `sort`, and
    /// [`Steam::rows`] is where the two meet.
    games: Vec<Game>,
    trophies: crate::trophies::Trophies,
    /// What order the user asked for the column in, which is what the settings
    /// file remembered from last time until they ask for another.
    ///
    /// Held beside the library rather than applied to it, so that the order
    /// somebody chose and the order Steam sent stay two separate things: the
    /// library is replaced wholesale every time a download finishes, and a list
    /// re-sorted in place would have to be re-sorted again on arrival — or
    /// would compare unequal to the one that just came in and rebuild the
    /// column for no reason. See the equality check in [`Steam::apply`].
    sort: lxb_steam::library::Sort,
    /// What is typed into the field at the head of the column, as the user
    /// typed it. Empty for a library nobody has searched, which is the state
    /// every session starts in.
    ///
    /// Held here rather than read back off the row it is drawn on, for the
    /// reason the shelves' queries are: the column is rebuilt from the library
    /// whenever a download moves a byte, and a field that read itself off the
    /// bar would lose whatever had been typed since. Not written down between
    /// sessions either, unlike [`Steam::sort`] — an order is how somebody wants
    /// their library listed and a search is a question they are in the middle
    /// of asking, and a shell that opened on a library narrowed to four games
    /// by yesterday's question would look like a shell that had lost the rest.
    search: String,
    /// The sign-in on screen, if one is.
    signing_in: Option<Stage>,
    /// Where Valve's client has got to installing itself, while this session
    /// is installing one.
    ///
    /// Held apart from `signing_in` on purpose, and that is the whole of what
    /// makes the setup something a user can walk away from: the panel is one
    /// view of this, and taking the panel down leaves the install exactly where
    /// it was. Pressing the row again builds the panel back out of it. `None`
    /// whenever no setup is running, which is every session on every machine
    /// that has had Steam started once.
    setting_up: Option<lxb_steam::setup::Step>,
    /// Whether the panel is what the setup is being watched by.
    ///
    /// `false` once somebody has pressed Back on it, and it is what decides who
    /// hears about the ending: a panel that is still up moves on to the sign-in
    /// questions by itself, and one that was dismissed is answered by the
    /// notification instead. See [`Changed::steam_is_ready`].
    setup_is_watched: bool,

    /// The games being fetched, and how far each has got. Kept here rather
    /// than on the game rows because the library is replaced wholesale
    /// whenever it is refreshed, and a download outlives several of those.
    fetching: BTreeMap<u32, Fetching>,
    /// And the ones being taken off the disk, for the same reason. Removing a
    /// game is quick but it is not instant, and a row that said nothing while
    /// it happened would be a row somebody pressed again.
    removing: BTreeSet<u32>,
    /// When the machine was last asked whether Valve's client is still there.
    ///
    /// Asked on an interval rather than per row: it is a `PATH` walk, the
    /// library is rebuilt whole, and what this is watching for — somebody
    /// removing Steam from under a running session — is not something that
    /// happens between two frames. See [`lxb_steam::Steam::recheck_client`].
    asked_after_the_client: Instant,
    /// When a client was last asked to shut down, while it is still there.
    ///
    /// The one stretch of a session where the interval above is too slow to be
    /// useful: something is waiting on the answer. See
    /// [`WHILE_THE_CLIENT_GOES`] and [`Self::close_the_client`].
    watching_the_client_go: Option<Instant>,
    /// What the worker said became of the last client asked to shut down.
    ///
    /// `None` from the moment one is asked until the answer arrives, which is
    /// the ordinary state and not a missing answer: the request crosses a
    /// thread. See [`Self::how_the_close_went`].
    how_the_close_went: Option<lxb_steam::client::Closing>,
    /// Which ask that answer would belong to.
    ///
    /// A client is asked to shut down, given a grace period, and asked again —
    /// so there are two asks in flight over one client, and the first one's
    /// answer arriving after the second was made is the shell deciding what
    /// became of a client from a report about a different question. `None` once
    /// the answer has been taken, and once the account underneath it has gone.
    closing: Option<u64>,
    /// The launches being watched, by the game each is about, and the number
    /// the watch for it was started under.
    ///
    /// The watcher is a thread that polls Valve's client every two seconds and
    /// then speaks; the press it is speaking about can end in between. Held so
    /// that a watcher belonging to a launch this shell has let go of cannot put
    /// a question on the screen about a game nobody is starting.
    ///
    /// A map rather than one number, because two displays can each have a game
    /// on the way. It used to be one, and the two failures that came of it were
    /// the same failure from both ends: starting the second screen's watch
    /// dropped the first screen's, so a question about the first game was
    /// thrown away as somebody else's; and ending *either* screen's loading
    /// screen stopped the watch for both, so a question about the game still
    /// loading was never asked for at all.
    watched_launches: BTreeMap<u32, u64>,
    /// What Valve's client's own log says it has in hand, and when that was
    /// last read. See [`Steam::valve_has_nothing_in_hand`].
    valve_is_doing: BTreeMap<u32, lxb_steam::client::InHand>,
    /// And the shader caches it is fetching, which the map above leaves out
    /// for the reason it is right to leave them out of a row: the game is on
    /// the disk and plays while Steam fetches one. Kept because there is one
    /// place it is the answer — a press that is waiting while Steam fetches
    /// five gigabytes of them. See [`Steam::what_a_press_is_waiting_on`].
    valve_is_fetching_shaders: BTreeSet<u32>,
    /// And whether it has anything at all in hand, which is a wider question
    /// than the two above: a shader cache is none of a row's business and is
    /// still not something to shut a client down under. See
    /// [`lxb_steam::client::Track`].
    valve_is_busy: bool,
    looked_at_valves_log: Instant,
    /// How far away Steam is, as the row and the menu have to draw it.
    ///
    /// Its own answer, separate from whether anybody is signed in. A session
    /// that comes up with a stored credential and no network is signed in, has
    /// a library on the disk, and can play every game on it — and used to be
    /// drawn as signed out, with an empty column and a Sign in row that did
    /// nothing when pressed.
    reach: lxb_steam::Reach,
    /// When the library on screen was last read from Steam, for a column that
    /// is a memory rather than an answer. `None` once Steam has answered.
    library_as_of: Option<std::time::SystemTime>,
    /// What Steam last said about which compatibility tool runs one title — or
    /// every unverified title.
    ///
    /// Asked once per title and kept for the session, because the answer is
    /// two round trips into a client that may have to be started first, and a
    /// menu that fetched it on every open would be a menu that waits. What can
    /// go stale in it is the *list*, which changes only when somebody installs
    /// a compatibility tool; what cannot is the tick, because every choice made
    /// here is read back out of the client and written down again.
    compat: BTreeMap<lxb_steam::webui::Which, Compat>,
    /// And every way each game can be started, for the games somebody has
    /// opened that row of the menu on.
    ///
    /// Held beside the tools for the same reason and on the same terms: the
    /// list is Valve's client's own answer, there is nowhere else to get it,
    /// and a press may have to wait for the client to come up. Kept per game
    /// rather than for the library, because it is asked about one game at a
    /// time and most games are never asked about at all.
    ways: BTreeMap<u32, Ways>,
    /// Which of those a press is in the middle of *changing*, so that a
    /// refusal can be told from a list that would not arrive.
    ///
    /// Both come back as the same event — the worker reads the answer back out
    /// of the client either way — and they want opposite treatment. A list that
    /// could not be fetched is written on the panel that is waiting for it. A
    /// choice the client would not take has no panel waiting: the menu was
    /// answered and put away on the press, so with nothing here it would be a
    /// press that silently did nothing.
    forcing: std::collections::BTreeSet<lxb_steam::webui::Which>,
    /// Which library each game pressed this session was sent to, where one was
    /// named.
    ///
    /// Kept for the one press that comes back and has to go on where the last
    /// one left off: an agreement accepted on the shell's panel opens the
    /// wizard again, and a game whose library was chosen a moment before must
    /// not lose that answer on the way through. See [`Steam::accept_and_install`].
    places: BTreeMap<u32, lxb_steam::webui::Place>,
    /// The game this session is moving into another library, while it is.
    /// See [`Relocation`].
    moving: Option<Relocation>,
    /// Who the account knows, and where each of them is, as Steam last said.
    ///
    /// Empty for a session with nobody signed in, and emptied again when
    /// somebody signs out: a roster is a statement about this minute and about
    /// one account, and leaving one standing would be showing the next person
    /// somebody else's friends. Nothing about it is written down between
    /// sessions for the same reason — see [`lxb_steam::friends`].
    roster: lxb_steam::Roster,
    /// What has been said in every one-to-one conversation this session has
    /// looked at.
    ///
    /// Beside the roster rather than in it, and for the reason everything else
    /// here is beside it: the roster is *replaced wholesale* whenever anybody
    /// changes their mind about anything, and a conversation folded into it
    /// would be thrown away every time a friend started a game. Held in memory
    /// only — see [`lxb_steam::chat`], where the reason nothing is written to
    /// the disk is argued.
    conversations: lxb_steam::chat::Conversations,
    /// Whether there is a Steam client running on this machine, as of the last
    /// look.
    ///
    /// The question the column used to draw without asking. Every "Updating",
    /// "Downloading" and "Checking files" on it is read out of a manifest, and
    /// a manifest is what Valve's client *was* doing: with no client running,
    /// none of it is happening and none of it will until one is started. A row
    /// that said "Updating" for a whole session in which nothing was fetched is
    /// what this is here to stop. See [`lxb_steam::Steam::client_is_running`].
    client_running: bool,
    /// Whether that client is signed in to anybody, as of the same look.
    ///
    /// For one decision and no other: whether a window of the client's that
    /// has stood hidden for a minute is a question somebody has to answer. A
    /// client signed in to nobody has exactly one window to show — its own
    /// login screen — and that is the one window of Valve's this shell exists
    /// to keep off the display. It was let through, on 2026-09-13, fifteen
    /// milliseconds after the shell's own sign-in panel came down. See
    /// `Shell::sync_steam_questions`.
    client_signed_in: bool,
    /// Whether one is on its way up because this session asked for it.
    ///
    /// Only ever about a wake this session asked for, which is exactly the one
    /// worth saying: somebody pressed Start Steam over a game whose update was
    /// waiting, and a minute of a cold client with nothing on the screen about
    /// it is a press that appears to have done nothing.
    client_waking: bool,
    /// Whether this session drives Valve's client itself.
    ///
    /// False in a session started with `--no-steam` and in one showing an
    /// invented library, which is exactly the difference that decides whether
    /// the client's windows are the shell's to hide: a session that never
    /// starts the client has no business hiding one somebody started for
    /// themselves. See [`Steam::driving`].
    driving: bool,
}

/// A path as the diagnostics panel prints it: `~/…` for anything under the
/// user's own home.
///
/// Not decoration. The panel gives a value one line and cuts what runs past it,
/// and every Steam directory worth naming is inside the home — so a panel that
/// printed the whole path spent its width on the part that is the same for
/// every machine and cut off the part that is not.
fn under_home(path: &std::path::Path) -> String {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    said_under(path, home.as_deref())
}

/// The same, against a given home, so a test can say which one without owning
/// this process's environment for the length of it.
fn said_under(path: &std::path::Path, home: Option<&std::path::Path>) -> String {
    match home
        .filter(|home| home.is_absolute())
        .and_then(|home| path.strip_prefix(home).ok())
    {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// About as much as one of the panel's values can print before the drawing
/// cuts it.
///
/// A rule of thumb rather than a measurement — what actually fits depends on
/// the label beside it and the characters in it — and it is here so that a
/// value written twice as long as this cannot be added without somebody
/// noticing. See the test below it.
#[cfg(test)]
const FITS_ON_A_LINE: usize = 34;

/// How many lines of the record the diagnostics panel shows.
///
/// A panel is read at a glance and this one already has seven values above the
/// record. Six is the last handful of things this session did, which is what
/// somebody is looking for; the rest of it is in the file, which is what a bug
/// report carries. See [`lxb_steam::audit`].
const LATELY_SHOWN: usize = 6;

/// A press on a Steam game, waiting for Valve's client to be ready for it.
///
/// Starting a game is a round trip — the client may have to be woken and
/// signed in first — so the press is answered immediately with the same splash
/// every other launch gets, and this is what the shell has to remember until
/// the client is ready: which game was pressed, and everything about the press
/// that was read off the bar at the time.
#[derive(Debug, Clone, PartialEq)]
pub struct Awaiting {
    /// The wake this press asked for, as [`lxb_steam::Steam::wake_client`]
    /// numbered it. What comes back from Valve's client names it, and an
    /// answer naming anything else is not this press's — see [`Gate`].
    pub request: u64,
    pub app_id: u32,
    pub name: String,
    /// So a failure can be explained on the display it was pressed from, out
    /// of the tile it was pressed on, once the splash has been taken away.
    ///
    /// Named rather than numbered. Waking a cold client is most of two minutes
    /// and a display can be unplugged inside it, which renumbers every panel
    /// after it — and a press that came back holding an index would explain
    /// itself on whichever screen had moved into that slot. See
    /// [`crate::Display`].
    pub display: crate::Display,
    pub from: [f32; 4],
    /// When the press was, for the log: the interesting number is how long a
    /// cold client keeps somebody waiting.
    pub asked: std::time::Instant,
    /// The invitation this press is accepting, where it is accepting one.
    ///
    /// Carried the whole way through the wake rather than looked up again at
    /// the end of it, and for the same reason the name is: waking a cold client
    /// is most of two minutes, and what the conversation holds by then is not
    /// necessarily what was pressed. It decides the URL — see [`Steam::join`]
    /// — and nothing else about the launch differs.
    pub joining: Option<lxb_steam::chat::Invite>,
}

/// What a word from Valve's client means, given what the shell is waiting for.
#[derive(Debug)]
pub enum Answer {
    /// A press is waiting on it. Here is what that press was about.
    Waiting(Awaiting),
    /// No game is waiting on this one, and this is what it was for. Whether
    /// anything goes on the screen depends on which — see [`NoGame`].
    NoGame(NoGame),
    /// A wake this shell asked for and then forgot about. Worth a panel and a
    /// line in the log, because it is a bug in this shell rather than a thing
    /// that happened to the user.
    Unexpected,
}

/// A wake with no game waiting on it, and what it was asked for.
///
/// Three of them, because the shell wakes Valve's client for three different
/// reasons and only one of those is a game. What tells them apart is what a
/// failure owes the user, and it used to be told by two flags a long way from
/// each other — one of which was cleared in three places and had to be right in
/// all three.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoGame {
    /// A press that was ended knowingly — its loading screen ran out of
    /// patience, its display was unplugged, its panel was dismissed. Nothing to
    /// say: the shell has already told the user, and saying it again over a
    /// start screen they have gone back to is answering one question twice.
    ThePressEnded,
    /// A client this shell warmed up before anybody pressed anything, so that a
    /// game pressed later would start sooner. Nothing is on screen about it and
    /// nothing goes on screen about it failing: the session simply has no
    /// client ready, which is where every session that has not asked for one
    /// already stands.
    NobodyAsked,
    /// The Start Steam row, pressed over a game whose update was waiting.
    /// Nothing is starting but Steam itself and there is no loading screen —
    /// and the press promised that Steam would start, so a failure is said out
    /// loud. It used to reach the screen by being unrecognised, which is the
    /// same arm a wake this shell had lost track of came out of.
    SteamWasPromised,
}

/// Whether a press is waiting on Valve's client, and what a late answer means.
///
/// One small state machine rather than two fields and the rules that used to
/// hold between them, because every bug this has had was a rule that held in
/// one place and not in another.
///
/// **A wake always answers, and the press that asked for it may be over by
/// then.** Waking a cold client is most of two minutes; a loading screen runs
/// out of patience before that, a display can be unplugged, a panel can be
/// dismissed. Every one of those ends the press and none of them ends the
/// wake — so the answer arrives at a shell where nobody is waiting, and what it
/// must not do is start the game. That is a game opening over a start screen,
/// with no loading screen and no dip, minutes after the shell said it had not
/// started.
///
/// And it must not swallow *every* answer that finds nobody waiting either, or
/// a wake the shell asked for and lost track of would be silent. So the two are
/// told apart: [`Answer::NoGame`] is a wake with no game waiting on it and the
/// reason there is none, and [`Answer::Unexpected`] is this shell losing something.
///
/// **Every answer names the wake it belongs to**, which is the half this used
/// to do by timing. A wake that had been given up on left one anonymous mark
/// behind, and a press made afterwards cleared it — so the abandoned wake's
/// `Ready`, arriving a minute later, was read as the answer to the new press
/// and started a game the client had never been asked for. Under a different
/// account, if somebody had signed out and back in inside that minute. Now
/// every wake is asked under a number, every answer carries it back, and a
/// number is spent by exactly one answer: see [`lxb_steam::Ticket`].
///
/// The other half is the gate itself. While a press is waiting, another press
/// does nothing at all — one game starts at a time — and a gate that was never
/// let go of would swallow every press for the rest of the session, which is
/// exactly what it did before [`Gate::gave_up`] existed.
#[derive(Debug, Default)]
pub struct Gate {
    waiting: Option<Awaiting>,
    /// Wakes that are still out there with nobody waiting on them, and why
    /// each has nobody.
    ///
    /// Bounded without a cap, because the worker answers **every** wake exactly
    /// once — one whose account signed out underneath it is answered with the
    /// truth rather than dropped — so every number put in here is taken out
    /// again by the answer it is waiting for.
    loose: std::collections::BTreeMap<u64, NoGame>,
}

impl Gate {
    /// What is being waited for, if anything.
    pub fn waiting(&self) -> Option<&Awaiting> {
        self.waiting.as_ref()
    }

    /// Whether a press is already waiting on the client.
    pub fn busy(&self) -> bool {
        self.waiting.is_some()
    }

    /// Whether a wake is still out there at all — one being waited on, or one
    /// whose press has been given up on and which is still going to answer.
    ///
    /// The question the two halves that close a client have to ask, and
    /// [`Self::busy`] is not it. A press given up on early — Back off the
    /// loading screen, or its patience running out — leaves a client on its way
    /// up with nobody waiting, and a shell that read that as "no client is
    /// running, nothing to decide" threw the question away and never closed the
    /// client that arrived a minute later. Always ends: a wake always answers,
    /// and the answer spends the number.
    pub fn a_wake_is_still_out_there(&self) -> bool {
        self.waiting.is_some() || !self.loose.is_empty()
    }

    /// Take a press, waiting on the wake it has just asked for.
    ///
    /// It does **not** clear what was already out there, which is the whole of
    /// the change: an older wake is still coming, it still answers under its own
    /// number, and it is not this press's answer. Letting a new press stand in
    /// as the answer to an abandoned one is exactly how an account that had
    /// signed out started the next account's game.
    pub fn press(&mut self, press: Awaiting) {
        self.waiting = Some(press);
    }

    /// Note a wake asked for on nobody's behalf, so a game pressed later starts
    /// sooner.
    ///
    /// Held here rather than as a flag beside the gate, because it is the same
    /// question every other wake asks — whose answer is this — and a second
    /// place to ask it is a second place to get it wrong.
    ///
    /// It follows that a background wake now counts towards
    /// [`Self::a_wake_is_still_out_there`], which the flag did not. That is the
    /// right answer to the question that method asks: a client warmed up at
    /// startup is a client on its way up, and the halves that decide whether to
    /// shut one down should not be deciding about it while it arrives. It ends
    /// when the wake answers, which is a minute at the outside.
    pub fn nobody_asked(&mut self, request: u64) {
        self.loose.insert(request, NoGame::NobodyAsked);
    }

    /// Note a wake somebody pressed a row for, where the row promised Steam and
    /// not a game. Its failure is said out loud; its success starts nothing.
    pub fn steam_was_promised(&mut self, request: u64) {
        self.loose.insert(request, NoGame::SteamWasPromised);
    }

    /// Stop waiting, because the press has ended.
    ///
    /// Every terminal path goes through here, which is the point of its being
    /// one function: the loading screen running out of patience, the display it
    /// was on being unplugged, and the panel it turned into being dismissed all
    /// leave the same two things behind — a gate that swallows every later
    /// press, and a wake still running whose answer would be acted on as though
    /// somebody were still waiting for it.
    ///
    /// The wake itself cannot be called back — it is a thread inside the
    /// worker, most of the way through starting a program — so what is recorded
    /// instead is that its answer belongs to nobody.
    pub fn gave_up(&mut self) -> Option<Awaiting> {
        let waiting = self.waiting.take()?;
        self.loose.insert(waiting.request, NoGame::ThePressEnded);
        Some(waiting)
    }

    /// Valve's client has answered one wake. Who, if anyone, it is for.
    ///
    /// Answered by number and by nothing else. A press waiting on a *different*
    /// wake is left exactly where it is: its own answer is still coming, and
    /// handing it somebody else's is the failure this whole envelope exists to
    /// stop.
    pub fn answered(&mut self, request: u64) -> Answer {
        if self
            .waiting
            .as_ref()
            .is_some_and(|it| it.request == request)
        {
            let waiting = self.waiting.take().expect("just matched");
            return Answer::Waiting(waiting);
        }
        if let Some(why) = self.loose.remove(&request) {
            return Answer::NoGame(why);
        }
        Answer::Unexpected
    }

    /// Let go of everything, because the ground under it has gone.
    ///
    /// Signing out, and the Steam integration being turned off. Neither leaves
    /// a wake worth answering: the press that asked for it has been told, and
    /// what the client does next is about an account this session no longer
    /// has. Returns whatever press was waiting, for whoever has something to
    /// say about it.
    pub fn everything_is_off(&mut self) -> Option<Awaiting> {
        self.loose.clear();
        self.waiting.take()
    }
}

/// How long ago something was, in the words a row has space for.
///
/// Coarse on purpose. The question this answers is "is what I am looking at
/// this morning's or last week's", and a column that counted seconds would be
/// a column redrawing itself every second to say nothing new.
fn ago(when: std::time::SystemTime) -> String {
    let Ok(since) = when.elapsed() else {
        // A clock that has gone backwards — a machine that has just picked up
        // the time from the network, which is every first boot. Not worth a
        // sentence; it is a library from this session either way.
        return crate::i18n::text("shell-just-now").to_string();
    };
    let minutes = since.as_secs() / 60;
    match minutes {
        0 => crate::i18n::text("shell-just-now").to_string(),
        1 => crate::i18n::text("shell-a-minute-ago").to_string(),
        2..=59 => crate::message!("time-minutes-ago", "count" => minutes),
        60..=119 => crate::i18n::text("shell-an-hour-ago").to_string(),
        120..=1439 => crate::message!("time-hours-ago", "count" => minutes / 60),
        1440..=2879 => crate::i18n::text("time-yesterday").to_string(),
        _ => crate::message!("time-days-ago", "count" => minutes / 1440),
    }
}

/// What the shell knows about a download before it is agreed to.
///
/// Everything here is read off this machine. How large the download is and
/// which library it lands in are Steam's to say and Steam does not say them
/// before the wizard is walked, so they are not here and are not guessed at —
/// see the panel, which says plainly that Steam chooses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Preflight {
    /// Every Steam library and the room left on it, in Steam's own order.
    pub room: Vec<lxb_steam::library::Room>,
    /// A game already coming down, if one is. This one will queue behind it.
    pub already: Option<String>,
}

impl Preflight {
    /// How many Steam libraries are there today.
    pub fn libraries_present(&self) -> usize {
        self.room.len()
    }

    /// Whether this library, by the path Steam lists it under, is one of them.
    pub fn has_library(&self, path: &str) -> bool {
        self.room
            .iter()
            .any(|room| crate::settings::same_library(&room.path.to_string_lossy(), path))
    }

    /// The room line, or nothing where no library would say.
    ///
    /// One library is the ordinary machine and gets an exact answer. Several
    /// get the largest, because that is what the question is: whether there is
    /// room for this anywhere, given that Steam picks where.
    pub fn room_said(&self) -> Option<String> {
        let most = self.room.iter().filter_map(|room| room.free).max()?;
        Some(match self.room.len() {
            0 | 1 => crate::message!("steam-free-on-this-machine", "free" => format_size(most)),
            libraries => {
                crate::message!("steam-free-on-largest-library", "free" => format_size(most), "count" => libraries)
            }
        })
    }
}

/// How far one download has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fetching {
    pub done: u64,
    /// Zero until the manifests have been read, which is the short stretch at
    /// the start where the size is not yet known.
    ///
    /// This is the one of the two that is worth something. `done` beside it is
    /// written to the manifest so rarely that it reads nought for most of a
    /// download — see [`lxb_steam::webui::Live`], which carries the
    /// measurement — so the size is what this pair is kept for.
    pub total: u64,
    /// What Valve's client itself says about this download: how far along, how
    /// fast, how much longer.
    ///
    /// `None` where the client's interface cannot be reached, which is not a
    /// failure — it is a download somebody started in Steam's own window on a
    /// client this shell never woke. The row then says what the disk says.
    pub live: Option<lxb_steam::webui::Live>,
    /// Whether Valve's client says this is running and has written nothing for
    /// an hour — see [`lxb_steam::Event::InstallStuck`].
    ///
    /// Not a failure and not an ending. The download is still watched and the
    /// row still counts; what changes is that the row stops implying something
    /// is happening. A byte arriving takes it back.
    pub stuck: bool,
}

impl Fetching {
    /// How far along it is, as a share of one.
    ///
    /// The client's own answer first, because it is the only one that moves,
    /// and the manifest behind it for a client that will not answer. Nothing
    /// where neither says — see [`lxb_steam::library::fraction`] for why
    /// nought is not an answer.
    pub fn fraction(&self) -> Option<f32> {
        self.live
            .and_then(lxb_steam::webui::Live::fraction)
            .or_else(|| lxb_steam::library::fraction(self.done, self.total))
    }

    /// How fast it is arriving, where the client is saying.
    ///
    /// Nothing for the first second or two of a download: a rate is two samples
    /// divided, and the client has not taken the second one yet. Nothing, and
    /// not zero — a row reading "0 B/s" over a download that is going perfectly
    /// well is worse than a row that has not said yet.
    pub fn per_second(&self) -> Option<u64> {
        self.live.and_then(lxb_steam::webui::Live::per_second)
    }

    /// What the row under the game's name says while it is coming down.
    ///
    /// Worded as the library words the same fact about a download this session
    /// did not start — a percentage and how much there is of it — because they
    /// are one fact, and a row that said it two ways depending on who pressed
    /// the button would read as two different things happening. The rate is the
    /// one thing only this half can say: nothing on the disk carries it.
    pub fn said(&self) -> String {
        let so_far = match (self.fraction(), self.total) {
            (None, _) => crate::i18n::text("shell-installing").to_string(),
            // How far, and of what — except that the two now come from
            // different places, and the client knows how far before the
            // manifest has been written at all. Measured: a percentage was in
            // hand a whole second before the file first named a size. Saying
            // "of 0 B" there would be the row inventing the half it has not got.
            (Some(share), 0) => {
                crate::message!("steam-installing-percent", "percent" => format!("{:.0}", share * 100.0))
            }
            (Some(share), total) => {
                crate::message!("steam-installing-percent-of", "percent" => format!("{:.0}", share * 100.0), "size" => format_size(total))
            }
        };
        // The size and the rate are the same kind of number and are written the
        // same way, which is what lets them share a line without reading as two
        // different measurements.
        let so_far = match self.per_second() {
            Some(rate) => format!("{so_far} · {}/s", format_size(rate)),
            None => so_far,
        };
        match self.stuck {
            // Said plainly, because the alternative is what this shell did
            // before: a percentage that has not changed in an hour and a row
            // that goes on looking like something is happening. What to do
            // about it is in the menu — Steam's own downloads list — and this
            // is the line that sends somebody there.
            true => crate::message!("steam-not-moving", "progress" => so_far),
            false => so_far,
        }
    }
}

/// What the word in front of the game's name is when nothing more is known.
///
/// A press is answered before Valve's client has written anything down, and
/// this is what the card says in that gap. Downloading rather than Installing,
/// because the card is about bytes arriving — the row beside it is the one that
/// says what is being *installed*.
pub(crate) const DOWNLOADING: &str = "Downloading";

/// And what it says over a game Steam is reading back off the disk rather than
/// fetching. A check on a large game is twenty minutes, and calling that a
/// download is the mistake the row already refuses to make — see
/// [`lxb_steam::library::Standing::Validating`].
pub(crate) const CHECKING: &str = "Checking";

/// And over a copy already on the disk that Steam is putting bytes into,
/// whatever the manifest is calling the copy while it does it.
pub(crate) const UPDATING: &str = "Updating";

/// And over work that is not the game's content at all: a shader cache Steam
/// fetches before it will open the window. Not "Downloading", because what is
/// coming down is not the game — the game is on the disk, entire, and would
/// play — and not "Updating", because nothing about it is being changed.
pub(crate) const PREPARING: &str = "Preparing";

/// The word for a standing, for the head of the card.
///
/// Two words and not the standing's own [`lxb_steam::library::Standing::said`],
/// which is a phrase for a row that has the name above it — "Waiting to
/// download" reads as a state there and as a stammer in front of a name here.
/// The one distinction worth drawing is bytes arriving over a copy that is
/// already on the machine, which is not a download and is called one nowhere
/// else in this shell.
/// Whether the update this manifest describes is one Steam has put off.
///
/// A free function taking the time rather than reading the clock itself,
/// so the two states either side of an appointment can be tested without
/// waiting for one. `now` is Unix seconds.
///
/// **Zero is not a time and a time in the past is not an appointment.**
/// Steam writes `ScheduledAutoUpdate` when it decides an update can wait
/// and leaves the number written afterwards, so most manifests on a real
/// disk carry one that has already been and gone. Read off this machine on
/// 2026-09-02: Proton Experimental carried an appointment for the sixth
/// with nothing outstanding at all.
fn update_is_deferred(game: &Game, now: u64) -> bool {
    game.scheduled_for > now
}

/// The wall clock, as Steam counts it.
///
/// Before the epoch is not a time this can be asked at, and a machine whose
/// clock says so is one where nothing is deferred — which is the answer
/// that lets work happen rather than the one that stops it.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or_default()
}

fn verb_for(standing: lxb_steam::library::Standing) -> &'static str {
    match standing {
        lxb_steam::library::Standing::Updating => UPDATING,
        _ => DOWNLOADING,
    }
}

/// One download, as the corner of the guide needs it.
///
/// Not a [`Fetching`]: that is what *this session's job* knows, and this is the
/// one download on the machine whichever half of the library knows about it.
/// What it holds is what the card draws — the picture is looked up from the app
/// id by whatever is drawing, exactly as a row's cover is.
#[derive(Debug, Clone, PartialEq)]
pub struct Coming {
    /// Whose game it is, which is where the card's picture comes from.
    pub whose: Whose,
    pub name: String,
    /// "Downloading" or "Updating" — see [`verb_for`].
    pub verb: &'static str,
    /// How far, or nothing where nothing has said yet. Nothing and not nought:
    /// see [`lxb_steam::library::fraction`].
    pub share: Option<f32>,
    /// Whether it says it is running and nothing is arriving. The card keeps
    /// its reading and goes quiet, exactly as the row's bar does.
    pub stuck: bool,
    /// Whether this is the game's own content arriving, which every card used
    /// to be.
    ///
    /// False for the one card that is not a download: work Steam is doing
    /// before it will open the window, under a press that is waiting. See
    /// [`Steam::what_a_press_is_waiting_on`].
    ///
    /// What turns on it is the **finish**. A download finishing is not an event
    /// anybody is told about — it is the card losing its game, with the game
    /// playable on the disk underneath — and that is exactly what a shader
    /// cache finishing looks like, over a game that was playable the whole
    /// time. Without this the guide would announce a download of a game that
    /// had never been downloaded. See `Shell::a_download_ended`.
    pub a_download: bool,
}

/// Format typed library facts at the shell boundary; wire values and game
/// names remain untouched. Keep size conventions consistent with Steam.
/// Where a game pressed on the bar goes, as Settings > Games > Steam > Install
/// games to has it: asked, or into the library chosen there — which is made
/// Steam's own default on the way, because that is what the row promises. See
/// [`crate::settings::SteamValue::InstallTo`].
pub fn place_from_the_settings() -> lxb_steam::webui::Place {
    match crate::settings::steam_install_to() {
        None => lxb_steam::webui::Place::Ask,
        Some(path) => lxb_steam::webui::Place::In {
            path,
            by_default: true,
        },
    }
}

pub fn format_size(bytes: u64) -> String {
    crate::i18n::decimal(lxb_steam::library::said(bytes))
}

pub fn friend_doing(friend: &lxb_steam::Person) -> &str {
    match (&friend.game, friend.app_id) {
        (Some(game), _) => game,
        (None, Some(_)) => crate::i18n::builtin("In game"),
        (None, None) => crate::i18n::builtin(friend.presence.said()),
    }
}

fn game_progress_note(game: &Game, state: &str) -> String {
    match game.fraction() {
        Some(share) => crate::message!("steam-game-progress", "state" => state,
            "percent" => format!("{:.0}", share * 100.0),
            "size" => format_size(game.to_download)),
        None => state.to_owned(),
    }
}

fn game_note(game: &Game) -> String {
    use lxb_steam::library::Standing;
    if !matches!(game.standing, Standing::NotInstalled | Standing::Ready) {
        return game_progress_note(game, crate::i18n::builtin(game.standing.said()));
    }
    if !game.installed {
        return crate::i18n::text("integration-not-installed").to_owned();
    }
    let installed = crate::i18n::text("integration-installed");
    match (game.size_on_disk, game.playtime_minutes) {
        (0, _) => installed.to_owned(),
        (size, 0) => format!("{installed} · {}", format_size(size)),
        (size, minutes) => {
            let played = if minutes < 60 {
                crate::message!("steam-playtime-minutes", "minutes" => minutes)
            } else {
                let hours = f64::from(minutes) / 60.0;
                let number = if hours < 10.0 {
                    format!("{hours:.1}")
                } else {
                    format!("{hours:.0}")
                };
                crate::message!("steam-playtime-hours", "hours" => crate::i18n::decimal(number))
            };
            crate::message!("steam-installed-played", "size" => format_size(size), "played" => played)
        }
    }
}

/// Whose game a download card is about.
///
/// The card is one fact about the machine — something is coming down — so an
/// Epic game's download stands on the same card as a Steam game's rather than
/// on one of its own. What differs is only where its picture is found.
#[derive(Debug, Clone, PartialEq)]
pub enum Whose {
    /// A Steam game, whose client icon (else its cover) is looked up by app id
    /// by whatever is drawing, exactly as a row's cover is.
    Steam(u32),
    /// An Epic game, by Heroic's name for it, with the file its cover is in.
    /// Epic publishes no icon for a game, so the cover stands in for one the
    /// way a Steam cover does for a game Valve gave none.
    Epic {
        app_name: String,
        cover: Option<std::path::PathBuf>,
    },
}

impl Coming {
    /// The Steam game the card is about, where it is one.
    ///
    /// What the finish is keyed on: a Steam download ending is noticed by its
    /// card losing it, while an Epic one is an event the helper reports — see
    /// `Shell::epic_download_ended` — and must not be announced twice.
    pub fn steam_app_id(&self) -> Option<u32> {
        match self.whose {
            Whose::Steam(app_id) => Some(app_id),
            Whose::Epic { .. } => None,
        }
    }

    /// What the card's one line says.
    pub fn said(&self) -> String {
        format!("{} {}", crate::i18n::builtin(self.verb), self.name)
    }
}

/// What the sign-in panel is asking for.
#[derive(Debug)]
pub enum Stage {
    /// Nothing yet, because there is no Steam on this machine to sign in to
    /// and one is being installed.
    ///
    /// The first stage on a new machine and the only one that is not a
    /// question: what Valve packages is a launcher, and the client proper is
    /// half a gigabyte it fetches the first time anybody runs it. Until that
    /// has happened there is nothing here for an account to be signed in *to*,
    /// so it comes before [`Stage::Choosing`] rather than after it. See
    /// [`lxb_steam::setup`].
    ///
    /// It is also the one stage a user is invited to walk away from — it is a
    /// wait rather than a question, and there is nothing to answer. See
    /// [`Steam::let_the_setup_run_in_the_background`].
    FirstSetup(lxb_steam::setup::Step),
    /// Steam could not install itself, and this is what to tell the user.
    ///
    /// Its own stage rather than a [`Stage::Failed`], because what it offers is
    /// different: nothing has been asked of any account, nothing has to be
    /// asked again, and trying again means installing rather than signing in.
    SetupFailed(String),
    /// Which way to sign in. The first question, and the only one this shell
    /// asks rather than Steam.
    Choosing,
    /// A code on the screen, waiting to be photographed. `None` until the
    /// first one arrives, which is one round trip after the panel goes up.
    Qr(Option<lxb_steam::qr::Code>),
    /// The account name, being typed.
    Account(String),
    /// The password for it. See the module docs for where this goes.
    Password { account: String, secret: Secret },
    /// A Steam Guard code, being typed, and what Steam asked for.
    Code {
        confirmation: Confirmation,
        typed: String,
    },
    /// Something is happening elsewhere — Steam is being asked, or a phone is
    /// waiting to be pressed — and there is nothing to type.
    Waiting(String),
    /// It did not work, and this is what to tell the user.
    Failed(String),
    /// Authentication worked, but Steam did not deliver a usable catalogue.
    /// This is not a sign-out and retrying must not ask for credentials again.
    LibraryUnavailable(String),
}

/// What is known about one title's compatibility, which is a question that
/// takes time to answer.
///
/// Three states rather than an `Option`, because a menu has to draw all three
/// and they are not the same picture: a list that is coming, the list, and the
/// reason there is none. The last of those is a sentence and not a silence —
/// the press promised a list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Compat {
    /// Steam has been asked and has not answered. The client may be starting.
    Asking,
    /// What Steam said.
    Said(lxb_steam::webui::Compatibility),
    /// It could not be asked, and this is what to tell whoever is waiting.
    Unavailable(String),
}

/// The same three answers about how one game can be started.
///
/// Its own type rather than [`Compat`] with a different payload, because the
/// two are asked of different things and a menu that could be handed either
/// would be a menu that had to ask which it was holding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ways {
    /// Asked, and no answer yet. The client may be starting.
    Asking,
    /// What the client said. An empty list is a game with one way of starting,
    /// which is most of a library and is not a failure.
    Said(Vec<lxb_steam::webui::Way>),
    /// It could not be asked, and this is what to tell whoever is waiting.
    Unavailable(String),
}

/// What one pass over the worker's events changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Changed {
    pub trophies: bool,
    /// The library is different, so the column has to be rebuilt.
    pub library: bool,
    /// What the panel should say is different, so it has to be redrawn — or
    /// raised, or taken away.
    pub panel: bool,
    /// The Games row is different: somebody signed in or out.
    pub account: bool,
    /// Games that finished moving on or off the disk, however they finished. A
    /// list rather than one, because several can end in a single pass and none
    /// of them may be dropped.
    pub installed: Vec<Ended>,
    /// What Valve's background client has to say for itself, and which wake
    /// each word answers. A press waiting to start a game is waiting on this.
    ///
    /// A list rather than one, on the same terms as `installed`: two wakes can
    /// land in a single pass — a client warmed up at startup and a press made
    /// while it was still coming up — and neither may be dropped. Dropping one
    /// leaves the press behind it waiting on an answer that has already been
    /// and gone, which is a loading screen that ends only when its patience
    /// does.
    pub client: Vec<(lxb_steam::Ticket, lxb_steam::ClientReport)>,
    /// Whether how far away Steam is has changed, so the Games row is rebuilt.
    pub reach: bool,
    /// And a request that would have raised a window of Steam's own and did
    /// not — Open Steam, Install with Steam, Verify.
    ///
    /// Its own field rather than a shape of `client`, because the two used to
    /// share one and the shell told them apart by whether a game press happened
    /// to be waiting at that moment. Overlap them — press a game, then Open
    /// Steam while its client is still coming up — and a refusal belonging to
    /// one took down the loading screen of the other.
    pub hand_over: Option<lxb_steam::Refused>,
    /// The launch that was asked for has stopped, and wants a person.
    pub asking: Option<lxb_steam::Asking>,
    /// Or Valve's client has given up on it, which is not a wait and not a
    /// question: see [`lxb_steam::LaunchRefused`].
    pub launch_refused: Option<lxb_steam::LaunchRefused>,
    /// Or Valve's client is on a step of it — fetching the game, compiling its
    /// shaders, syncing its saves — which is a wait to be drawn rather than a
    /// question to be answered. See [`lxb_steam::Step`].
    pub step: Option<lxb_steam::Step>,
    /// Steam has said something about what one title runs under, so a menu
    /// waiting for it can be filled in and the Settings row redrawn.
    pub compat: Option<lxb_steam::webui::Which>,
    /// And a *choice* Valve's client would not take, which is a press that has
    /// to be answered rather than a list that failed to arrive.
    pub compat_refused: Option<String>,
    /// Valve's client has said how one game can be started, so a menu waiting
    /// for that list can be filled in.
    pub ways: Option<u32>,
    /// The friends list is different, so the panel showing it has to be
    /// redrawn.
    ///
    /// Its own flag rather than folded into `library`: the two move at
    /// completely different rates — a roster changes whenever anybody in it
    /// starts a game — and rebuilding a column of a thousand rows because
    /// somebody's friend signed in would be a bar that stutters for a reason
    /// nobody can see.
    pub friends: bool,
    /// Something in a conversation is different: a message arrived, a send was
    /// answered, a history came in, somebody started typing.
    ///
    /// Apart from `friends` because the two move for completely different
    /// reasons and only one of them means the *list* has changed: a message
    /// arriving reorders nobody.
    pub chat: bool,
    /// Messages that arrived for conversations nobody has open: who each was
    /// from, and what it said.
    ///
    /// A list rather than one, on the same terms as `installed`: several can
    /// arrive in a single pass and none of them may be dropped — two friends
    /// writing at once is two announcements, not the later one.
    ///
    /// The body travels because the shell is the only thing that knows whether
    /// it may be shown — a screen resting under the OLED rule is a screen
    /// somebody else may be standing in front of — and a decision to withhold
    /// it has to be made somewhere that has the text to withhold. See
    /// `Shell::announce_a_message`.
    pub messages: Vec<(u64, String)>,
    /// And the invitations to a game: who from, and which one of theirs.
    ///
    /// A list for the reason the messages are one, and the number rather than
    /// the invitation for the reason given at
    /// [`lxb_steam::chat::Moved::invited`] — what the shell says about it comes
    /// out of the store, and it has to be able to find it again long after the
    /// pass it arrived on.
    pub invites: Vec<(u64, u64)>,
    /// A CM session came up. Every conversation's history is stale — nothing
    /// said while the connection was down was pushed at a session that was not
    /// there — so the one that is open has to be asked for again. The rest ask
    /// for themselves the next time they are opened.
    pub reconnected: bool,
    /// Valve's client took a request of its own — Open Steam, Big Picture, the
    /// downloads list, Install with Steam, Verify — and the window it raises
    /// for it is about to appear.
    ///
    /// What this is for is the one thing the shell has to do about it: give the
    /// client sight. See `Shell::steam_hand_over`, and
    /// [`lxb_steam::Event::HandedOver`] for why it is given here rather than
    /// when the button was pressed — and for the one fact it carries, which
    /// decides how sight is given.
    pub handed_over: Option<lxb_steam::HandedOver>,
    /// Steam has finished installing itself on this machine, and it was not
    /// this panel that was waiting to hear it.
    ///
    /// Set only where the setup was let run in the background, because that is
    /// the only case with anything to announce: somebody who is watching the
    /// panel watches it move on to the sign-in questions by itself, and a
    /// notification about a thing that just happened on screen is noise. See
    /// `Shell::say_steam_is_ready`.
    pub steam_is_ready: bool,
    /// How far the move under way has got is different, so the row and the
    /// panel showing it have to be redrawn.
    pub moving: bool,
    /// Moves that ended, however they ended: the game, where it was sent, and
    /// how it went. A list for the reason `installed` is one.
    pub moved: Vec<(
        u32,
        String,
        Result<lxb_steam::webui::Moved, lxb_steam::StorageRefused>,
    )>,
    /// And libraries added, removed or repaired, or not.
    pub shelved: Vec<(
        lxb_steam::webui::Shelving,
        Result<(), lxb_steam::StorageRefused>,
    )>,
}

impl Changed {
    /// Fold what one event did into what the whole pass did.
    ///
    /// A method that takes the other apart field by field, rather than the run
    /// of `|=` this replaces, and that is the whole point of it: the run was
    /// written by hand and every field added to this struct afterwards had to
    /// be remembered in a second place. Two were not. `reach` was dropped, so
    /// the Games row went on saying "Connecting to Steam · library from just
    /// now" for the rest of a session that had connected seconds later — the
    /// shell knew perfectly well Steam was answering, and said so on the
    /// diagnostics panel, while the row a foot away said the opposite. And
    /// `hand_over` was dropped with it, so a `steam:` request the client
    /// refused — Open Steam, Install with Steam, Verify — put no panel up at
    /// all, and the press simply appeared to do nothing.
    ///
    /// Destructured rather than read through `one.`: a field added tomorrow
    /// and forgotten here is a build error, which is the only way this stays
    /// true. Nothing in it may be `..`-ignored for the same reason.
    fn absorb(&mut self, one: Changed) {
        let Changed {
            trophies,
            library,
            panel,
            account,
            installed,
            client,
            reach,
            hand_over,
            asking,
            launch_refused,
            step,
            compat,
            compat_refused,
            ways,
            friends,
            chat,
            messages,
            invites,
            reconnected,
            handed_over,
            steam_is_ready,
            moving,
            moved,
            shelved,
        } = one;
        self.trophies |= trophies;
        self.library |= library;
        self.panel |= panel;
        self.account |= account;
        self.reach |= reach;
        self.friends |= friends;
        self.chat |= chat;
        self.reconnected |= reconnected;
        self.handed_over = handed_over.or(self.handed_over.take());
        self.steam_is_ready |= steam_is_ready;
        self.moving |= moving;
        // Nor any of these: two things can end in one pass.
        self.moved.extend(moved);
        self.shelved.extend(shelved);
        // None of these may be dropped either: two friends writing in one pass
        // is two announcements.
        self.messages.extend(messages);
        // Nor these, on the same terms: two people can ask you into two
        // different games in the same pass.
        self.invites.extend(invites);
        // None of these may be dropped: several games can finish in one pass.
        self.installed.extend(installed);
        // None of these may be dropped: each answers a different wake, and a
        // press waiting on one of them is waiting on that one.
        self.client.extend(client);
        self.hand_over = hand_over.or(self.hand_over.take());
        self.asking = asking.or(self.asking.take());
        self.launch_refused = launch_refused.or(self.launch_refused.take());
        // The last word wins rather than the first: this is a percentage, and
        // two of them in one pass is one reading that is already stale.
        self.step = step.or(self.step.take());
        self.compat = compat.or(self.compat.take());
        self.compat_refused = compat_refused.or(self.compat_refused.take());
        self.ways = ways.or(self.ways.take());
    }
}

/// A game this session is moving from one Steam library to another.
#[derive(Debug, Clone, PartialEq)]
pub struct Relocation {
    pub app_id: u32,
    /// Where it is going, by the path Steam lists that library under.
    pub to: String,
    /// How far along, 0 to 100, once Valve's client has said — `None` for the
    /// second or two before it has, and for the whole of a wake before that.
    pub percent: Option<f32>,
    /// Whether Stop has been pressed and the move has not yet ended.
    pub stopping: bool,
}

/// The `steam:` URL that accepts one invitation — see [`Steam::join`], where
/// the two forms are argued.
///
/// Its own function because it is the whole of what an accepted invitation
/// *is* and it can be checked without a Steam, a client or a machine: what
/// travels is a URL, and getting it wrong means a game that starts and joins
/// nothing, which looks exactly like a game that started.
fn joining_url(app_id: u32, invite: &lxb_steam::chat::Invite) -> String {
    match invite.lobby() {
        Some(lobby) => format!("steam://joinlobby/{app_id}/{lobby}/{}", invite.from),
        // `+` for the spaces, because a URL cannot carry one. It is Steam's own
        // convention for this, not an escape invented here.
        None => format!(
            "steam://rungameid/{app_id}//{}",
            invite.connect.trim().replace(' ', "+")
        ),
    }
}

/// What an install is waiting on that only Steam's own window can ask, as the
/// fixed word each catalog selects its sentence on — see
/// `steam-install-needs-first`.
///
/// `lxb_steam` says it as an English phrase ("a product key to type"), which
/// used to be put into the middle of a translated sentence as it stood, so a
/// Polish shell said "Ta gra najpierw wymaga: a product key to type." Read
/// back into a word here, and a phrase this does not know is `other`, which
/// every catalog says as a question to answer without naming it.
pub fn what_the_install_needs(said: &str) -> &'static str {
    match said {
        "an agreement to accept" => "agreement",
        "a product key to type" => "key",
        "a password to type" => "password",
        "a disc to change" => "disc",
        "an account to sign up for" => "signup",
        _ => "other",
    }
}

/// How one game's journey on or off the disk ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    /// The game is on the disk and can be started.
    Done { app_id: u32 },
    /// It is not, and this is what to say. Nothing was left behind, so
    /// pressing again starts a whole download rather than resuming one.
    ///
    /// A download somebody stopped is not one of these. It is not a failure,
    /// there is nothing to say about it, and the row saying so is the answer.
    Failed { app_id: u32, why: Stopped },
    /// The game has gone from the disk, which is what was asked for. Nothing
    /// to announce: the row saying "Not installed" is the whole answer.
    Removed { app_id: u32 },
    /// It has not gone, and this is what to say. Worth interrupting somebody
    /// for, because they asked for space back and have not got it.
    RemoveFailed { app_id: u32, why: String },
}

/// What a keystroke did to the panel.
#[derive(Debug, PartialEq, Eq)]
pub enum Typed {
    /// No field of this panel wanted it.
    Elsewhere,
    /// It went into the field, which now says something different.
    Into,
    /// The field is finished with — Return, or Escape.
    Done { submitted: bool },
}

impl Steam {
    pub fn start() -> Steam {
        // Before the worker can start anything: Valve's client is an
        // application this shell starts, and it gets what the others get, less
        // one line. It was the one that did not, and the whole of the
        // difference was the guide button — but the pad this shell drives from
        // `hidraw` is never on the list the client is handed, because the
        // client is that pad's other driver and hiding it there is what leaves
        // a Steam game with no controller at all. See
        // [`crate::pad_guard::hidapi_ignore_list_for_valves_client`].
        lxb_steam::client::confine_children_with(
            crate::model::hide_guarded_pads_from_valves_client,
        );
        let client = lxb_steam::Steam::start();
        // Asked here rather than left to the ten-second look, because the
        // answer decides what every row of the column *says* and the column is
        // built long before that first look: a session that came up beside a
        // client somebody had already started would spend its first ten seconds
        // telling them nothing was being fetched.
        let client_running = client.client_is_running();
        let client_signed_in = client_running && client.client_is_signed_in();
        Steam {
            client,
            account: None,
            steam_id: None,
            games: Vec::new(),
            trophies: crate::trophies::Trophies::new(),
            // Whatever the file said, which is nothing on a machine where
            // nobody has chosen. Read here rather than when the first library
            // arrives, because the order is a preference and not a property of
            // any particular library: it holds across a sign-out, and the
            // settings are loaded before this is built.
            sort: crate::settings::steam_sort().unwrap_or_default(),
            search: String::new(),
            signing_in: None,
            setting_up: None,
            setup_is_watched: false,
            reach: lxb_steam::Reach::Online,
            library_as_of: None,
            fetching: BTreeMap::new(),
            removing: BTreeSet::new(),
            asked_after_the_client: Instant::now(),
            watching_the_client_go: None,
            how_the_close_went: None,
            closing: None,
            watched_launches: BTreeMap::new(),
            valve_is_doing: BTreeMap::new(),
            valve_is_fetching_shaders: BTreeSet::new(),
            valve_is_busy: false,
            // Long ago, so the first frame that asks gets a real answer rather
            // than an empty one held over from a session that had not looked.
            looked_at_valves_log: Instant::now() - WHAT_VALVE_IS_DOING,
            compat: BTreeMap::new(),
            ways: BTreeMap::new(),
            forcing: std::collections::BTreeSet::new(),
            places: BTreeMap::new(),
            moving: None,
            roster: lxb_steam::Roster::default(),
            conversations: lxb_steam::chat::Conversations::default(),
            client_running,
            client_signed_in,
            client_waking: false,
            driving: true,
        }
    }

    /// One that will never do anything, for a session that has turned Steam
    /// off and for tests.
    pub fn settled() -> Steam {
        Steam {
            client: lxb_steam::Steam::settled(),
            account: None,
            steam_id: None,
            games: Vec::new(),
            trophies: crate::trophies::Trophies::new(),
            sort: crate::settings::steam_sort().unwrap_or_default(),
            search: String::new(),
            signing_in: None,
            setting_up: None,
            setup_is_watched: false,
            reach: lxb_steam::Reach::Online,
            library_as_of: None,
            fetching: BTreeMap::new(),
            removing: BTreeSet::new(),
            asked_after_the_client: Instant::now(),
            watching_the_client_go: None,
            how_the_close_went: None,
            closing: None,
            watched_launches: BTreeMap::new(),
            valve_is_doing: BTreeMap::new(),
            valve_is_fetching_shaders: BTreeSet::new(),
            valve_is_busy: false,
            // Long ago, so the first frame that asks gets a real answer rather
            // than an empty one held over from a session that had not looked.
            looked_at_valves_log: Instant::now() - WHAT_VALVE_IS_DOING,
            compat: BTreeMap::new(),
            ways: BTreeMap::new(),
            forcing: std::collections::BTreeSet::new(),
            places: BTreeMap::new(),
            moving: None,
            roster: lxb_steam::Roster::default(),
            conversations: lxb_steam::chat::Conversations::default(),
            client_running: false,
            client_signed_in: false,
            client_waking: false,
            driving: false,
        }
    }

    /// Whether this session drives Valve's client itself, and so whether the
    /// client's windows are the shell's to keep off the screen.
    pub fn driving(&self) -> bool {
        self.driving
    }

    /// Pretend an account is signed in and owns these games.
    ///
    /// For `--debug-steam-library`, and needed for the reason the display
    /// fixtures are: the Steam column is built out of somebody's library, so
    /// there is no way to look at it — or to screenshot it, or to check that
    /// installed titles really do come first — without an account, a password
    /// and a network. Nothing is asked of Steam, nothing is signed in, and
    /// none of these rows can be started: the account name says as much on the
    /// row it appears on.
    pub fn invent(&mut self, games: &[(String, bool, Option<u8>)]) {
        tracing::warn!(
            games = games.len(),
            "--debug-steam-library: these rows are invented and nothing in them will start"
        );
        self.account = Some("a made-up account".to_string());
        self.games = lxb_steam::library::sorted(
            games
                .iter()
                .enumerate()
                .map(|(at, (name, installed, share))| {
                    let game = lxb_steam::Game::invented(at as u32 + 1, name.clone(), *installed);
                    match share {
                        Some(share) => game.coming_down(*share),
                        None => game,
                    }
                })
                .collect(),
        );
    }

    /// Pretend the account knows these people and has said these things to
    /// them.
    ///
    /// For `--debug-steam-chat`, and needed for the reason
    /// [`Steam::invent`] is, with one thing more: **a conversation cannot be
    /// looked at without another person on the other end of it**. Every state
    /// the panel draws — a history that is still coming, one that failed, a
    /// send that would not go, somebody typing, a column long enough to scroll
    /// — is a state that needs Steam and a friend who is awake and willing, and
    /// several of them cannot be produced on demand at all.
    ///
    /// **Nothing here reaches Steam and nothing here can.** There is no worker
    /// behind an invented session — see the `--debug-steam-library` arm in
    /// `main` — so the asks these conversations would make go nowhere, and no
    /// message this shell can be made to compose while one is on screen leaves
    /// the machine. That is the whole safety argument, and it is why the
    /// fixture is a *state* rather than a script: a fixture that drove the real
    /// send path would be one keystroke away from writing to somebody.
    ///
    /// `names` are the friends, in the order they are listed. Every one of them
    /// gets a conversation whose shape is decided by their place in the list, so
    /// one flag produces every state at once and the panel can be walked
    /// through all of them.
    pub fn invent_conversations(&mut self, names: &[String]) {
        use lxb_steam::chat::{Heard, Key, Said, Word};

        tracing::warn!(
            people = names.len(),
            "--debug-steam-chat: these conversations are invented and nothing in them was sent"
        );
        let me = self.steam_id.unwrap_or(INVENTED_ACCOUNT);
        self.steam_id = Some(me);
        self.account
            .get_or_insert_with(|| "a made-up account".to_string());
        self.conversations.signed_in_as(me);
        let now = Instant::now();
        let mut friends = Vec::with_capacity(names.len());
        for (at, name) in names.iter().enumerate() {
            let steam_id = INVENTED_FRIEND + at as u64;
            friends.push(lxb_steam::Person {
                steam_id,
                name: name.clone(),
                presence: match at % 3 {
                    0 => lxb_steam::Presence::Online,
                    1 => lxb_steam::Presence::Away,
                    _ => lxb_steam::Presence::Offline,
                },
                game: (at % 4 == 0).then(|| "Half-Life 2".to_string()),
                app_id: (at % 4 == 0).then_some(220),
                avatar: None,
            });
            let hear = |conversations: &mut lxb_steam::chat::Conversations, word| {
                conversations.heard(
                    Heard {
                        generation: 1,
                        account: me,
                        word,
                    },
                    now,
                );
            };
            // One CM generation for the lot, settled before anything is said —
            // otherwise the first word would read as a reconnect and throw the
            // rest away.
            hear(&mut self.conversations, Word::Listening);
            match at % 5 {
                // A conversation that has been read and has things in it, long
                // enough to scroll.
                0 => {
                    let Some(lxb_steam::chat::Wanted::History { request, .. }) =
                        self.conversations.open(steam_id)
                    else {
                        continue;
                    };
                    let said = (0..12)
                        .map(|n| Said {
                            key: Key::new(1_700_000_000 + n * 60, 0),
                            body: INVENTED_LINES[n as usize % INVENTED_LINES.len()].to_string(),
                            from_me: n % 2 == 1,
                        })
                        .collect();
                    hear(
                        &mut self.conversations,
                        Word::History {
                            with: steam_id,
                            request,
                            said: Ok(said),
                        },
                    );
                    // And an invitation to a game at the foot of it, which is
                    // the one thing in a conversation that cannot be produced
                    // without another person — somebody has to press Invite in
                    // a game for one of these to exist. The lobby is invented
                    // like everything else here and joining it would fail,
                    // which is the point: the fixture is for the card, the
                    // legend and the press, and it cannot reach Steam at all.
                    hear(
                        &mut self.conversations,
                        Word::Invited {
                            with: steam_id,
                            invite: lxb_steam::chat::Invited {
                                at: 1_700_000_800,
                                key: None,
                                connect: "+connect_lobby 109775240000000000".to_string(),
                                app_id: Some(220),
                                game: Some("Half-Life 2".to_string()),
                            },
                        },
                    );
                }
                // One that has been read and is empty.
                1 => {
                    let Some(lxb_steam::chat::Wanted::History { request, .. }) =
                        self.conversations.open(steam_id)
                    else {
                        continue;
                    };
                    hear(
                        &mut self.conversations,
                        Word::History {
                            with: steam_id,
                            request,
                            said: Ok(Vec::new()),
                        },
                    );
                }
                // One whose history would not come, which is the retry state.
                2 => {
                    let Some(lxb_steam::chat::Wanted::History { request, .. }) =
                        self.conversations.open(steam_id)
                    else {
                        continue;
                    };
                    hear(
                        &mut self.conversations,
                        Word::History {
                            with: steam_id,
                            request,
                            said: Err(crate::i18n::text("shell-steam-is-not-answering-try-again")
                                .to_string()),
                        },
                    );
                }
                // One with a message that would not go, and one still going.
                3 => {
                    let Some(lxb_steam::chat::Wanted::History { request, .. }) =
                        self.conversations.open(steam_id)
                    else {
                        continue;
                    };
                    hear(
                        &mut self.conversations,
                        Word::History {
                            with: steam_id,
                            request,
                            said: Ok(vec![Said {
                                key: Key::new(1_700_000_000, 0),
                                body: INVENTED_LINES[0].to_string(),
                                from_me: false,
                            }]),
                        },
                    );
                    let failing = self
                        .conversations
                        .send(steam_id, "this one will not go".into());
                    if let lxb_steam::chat::Wanted::Send { request, .. } = failing {
                        hear(
                            &mut self.conversations,
                            Word::Sent {
                                with: steam_id,
                                request,
                                said: Err(
                                    crate::i18n::text("shell-steam-would-not-take-it").to_string()
                                ),
                            },
                        );
                    }
                    self.conversations
                        .send(steam_id, "and this one is still going".into());
                }
                // And one nobody has opened, with messages waiting in it and
                // somebody typing.
                _ => {
                    hear(
                        &mut self.conversations,
                        Word::Arrived {
                            with: steam_id,
                            said: Said {
                                key: Key::new(1_700_000_100, 0),
                                body: INVENTED_LINES[1].to_string(),
                                from_me: false,
                            },
                        },
                    );
                    hear(&mut self.conversations, Word::Typing { with: steam_id });
                }
            }
        }
        // In the order Steam's own half hands a roster over — band, then name,
        // then id — because that is the order every other pass in this shell
        // assumes and the fixture must not be the one list that arrives
        // differently. See `lxb_steam::friends::Roll::roster`, which is where
        // the real one is sorted; a fixture built straight into a `Roster`
        // goes round it.
        friends.sort_by(|a, b| {
            a.band()
                .cmp(&b.band())
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                .then_with(|| a.steam_id.cmp(&b.steam_id))
        });
        self.roster = lxb_steam::Roster {
            me: Some(lxb_steam::Person {
                steam_id: me,
                name: "a made-up account".to_string(),
                presence: lxb_steam::Presence::Online,
                game: None,
                app_id: None,
                avatar: None,
            }),
            friends,
        };
    }

    /// Who is signed in, if anybody.
    pub fn account(&self) -> Option<&str> {
        self.account.as_deref()
    }

    /// What this session's Steam is doing, as a panel of named values.
    ///
    /// Everything the shell would otherwise only say once, in a sentence that
    /// has since gone: which of the two clients is being driven and out of
    /// which directory, whether Steam is answering, how old the column is, what
    /// is moving on or off the disk, and the last few things this session drove
    /// through Valve's own interface with what each came to.
    ///
    /// It exists because the failures worth reporting are the ones that leave
    /// nothing behind. A game that will not install says a sentence and the
    /// sentence is dismissed; a library that is a week old looks exactly like
    /// one that is current; a client belonging to another session refuses
    /// everything and the refusal is a line in a panel nobody kept. Someone
    /// writing a bug report about any of that had nothing to put in it.
    ///
    /// **Nothing secret is here.** The account's number is four digits of it,
    /// which tells one household account from another and identifies nobody;
    /// the record is [`lxb_steam::audit`], which takes credentials out of
    /// everything it writes rather than trusting what it is handed. There is no
    /// token, no guard data and no password anywhere near this.
    ///
    /// Read on the press, like [`crate::machine::Facts`] and for the same
    /// reason: the backend is a `PATH` walk and a handful of `is_dir` calls,
    /// and a value carried on a row would be one that went stale in a tree
    /// rebuilt for every other reason. Nothing here *drives* the client — that
    /// is a loopback call with a twenty-second patience, and a panel is
    /// something opened at a glance. What the client last said is in the
    /// record. The one thing here that touches the network is the debugging
    /// interface's line, which opens a socket to loopback and drops it: a
    /// closed port refuses at once and an open one accepts at once, so it
    /// cannot be the thing that makes a panel slow to appear.
    /// Whether the diagnostics panel should carry the button that closes
    /// Valve's debugging interface.
    ///
    /// Read on the press like everything else on that panel, and for the same
    /// reason: a value carried on a row would be one that went stale. See
    /// [`lxb_steam::webui::Exposure::can_be_shut`], which is where the rule
    /// about *whose* marker may be closed lives.
    pub fn interface_can_be_shut(&self) -> bool {
        lxb_steam::backend::Backend::chosen()
            .is_some_and(|backend| lxb_steam::webui::exposure(backend.root()).can_be_shut())
    }

    /// Take away the marker that makes Valve's client expose its interface.
    ///
    /// Only ever from the button, which is the whole of what makes it allowed:
    /// this is somebody turning off a thing on their own machine, not a shell
    /// deciding for them. Says whether anything was removed, because the panel
    /// comes back up and its line has to be the new truth.
    pub fn shut_the_interface(&self) -> bool {
        let Some(backend) = lxb_steam::backend::Backend::chosen() else {
            return false;
        };
        match lxb_steam::webui::shut_the_interface(backend.root()) {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(%error, "could not take away Steam's debugging marker");
                false
            }
        }
    }

    pub fn diagnostics(&self) -> Vec<(String, String)> {
        let mut values = Vec::new();

        let backend = lxb_steam::backend::Backend::chosen();
        values.push((
            crate::i18n::text("shell-client").to_string(),
            match backend.as_ref().and_then(|backend| backend.client.as_ref()) {
                Some(lxb_steam::client::Where::Native(path)) => {
                    crate::message!("steam-client-native", "path" => path.display().to_string())
                }
                Some(lxb_steam::client::Where::Flatpak) => "Flatpak".to_string(),
                None => crate::i18n::text("shell-none-on-this-machine").to_string(),
            },
        ));
        values.push((
            crate::i18n::text("shell-steam-directory").to_string(),
            match backend.as_ref() {
                Some(backend) => under_home(backend.root()),
                None => crate::i18n::text("shell-nothing-steam-shaped-found").to_string(),
            },
        ));
        values.push((
            crate::i18n::text("shell-driven-by-this-session").to_string(),
            match self.driving {
                true => crate::i18n::text("shell-yes").to_lowercase(),
                false => crate::i18n::text("shell-no").to_lowercase(),
            },
        ));

        // What the client's debugging interface — the one this shell signs
        // Steam in through — is doing on this machine, and whether this
        // session is the reason. A loopback socket with no authentication, so
        // while it is open every program running as this user can drive
        // Valve's client: sign it in, install, uninstall, read the account.
        //
        // On the panel because there was no way to find out. The shell takes
        // back the marker it makes, and refuses on principle to take back one
        // it did not — which is right, and which left a marker somebody else
        // made silently opening every Steam they ever started, forever. It
        // cannot be removed on the shell's own authority; it can be shown, and
        // the button beside this line is how it goes.
        values.push((
            crate::i18n::text("shell-debugging-interface").to_string(),
            match backend.as_ref() {
                Some(backend) => {
                    crate::i18n::builtin(lxb_steam::webui::exposure(backend.root()).said())
                        .to_string()
                }
                // Nothing to look at and nothing that could open: no client and
                // no directory means no marker and nothing to start from one.
                None => crate::i18n::text("shell-no-client-to-expose").to_string(),
            },
        ));

        values.push((
            crate::i18n::text("shell-account").to_string(),
            match (self.account.as_deref(), self.steam_id) {
                (Some(account), Some(id)) => format!("{account} (…{})", id % 10_000),
                (Some(account), None) => account.to_string(),
                (None, _) => crate::i18n::text("shell-nobody-signed-in").to_string(),
            },
        ));
        values.push((
            "Steam".to_string(),
            // `said` is what a *row* would carry, which is nothing at all when
            // all is well. A panel of values cannot have a blank in it: the
            // whole point of the line is that it says which of the two this is.
            self.reach
                .said()
                .map(str::to_string)
                .unwrap_or_else(|| crate::i18n::text("steam-answering").to_string()),
        ));
        values.push((
            crate::i18n::text("shell-library").to_string(),
            match self.library_as_of {
                Some(when) => crate::message!("steam-library-read", "count" => self.games.len(), "when" => ago(when)),
                None => crate::message!("count-games", "count" => self.games.len()),
            },
        ));

        // What is in flight, which is the half a screenshot of the bar cannot
        // show: a download's row says a percentage and says nothing about the
        // other three.
        let moving: Vec<String> = self
            .fetching
            .keys()
            .map(|app_id| crate::message!("steam-fetching-app", "app" => *app_id))
            .chain(
                self.removing
                    .iter()
                    .map(|app_id| crate::message!("steam-removing-app", "app" => *app_id)),
            )
            .collect();
        values.push((
            crate::i18n::text("shell-in-flight").to_string(),
            match moving.is_empty() {
                true => crate::i18n::text("steam-nothing-in-flight").to_string(),
                false => moving.join(", "),
            },
        ));

        // What the pictures behind the column cost, which is the one number
        // about somebody's disk this shell is in a position to do anything
        // about — the button beside this panel throws it away.
        values.push((
            crate::i18n::text("shell-artwork-cache").to_string(),
            match crate::art::cache_room() {
                Some((0, _)) => crate::i18n::text("steam-cache-empty").to_string(),
                Some((bytes, files)) => {
                    crate::message!("steam-size-in-files", "size" => format_size(bytes), "count" => files)
                }
                None => crate::i18n::text("shell-nowhere-to-keep-one").to_string(),
            },
        ));

        // And the record, newest first, because what somebody is looking for is
        // the last thing that happened.
        let lately = lxb_steam::audit::lately();
        if lately.is_empty() {
            // Short, because the panel gives a value one line and cuts what
            // runs past it. "Nothing has been asked of Valve's client" is the
            // sentence this means and it does not fit beside its own label.
            values.push((
                crate::i18n::text("shell-lately").to_string(),
                crate::i18n::text("shell-nothing-yet").to_string(),
            ));
        }
        for entry in lately.iter().rev().take(LATELY_SHOWN) {
            values.push((ago(entry.at), format!("{}: {}", entry.what, entry.how)));
        }
        values
    }

    /// What order the column is listed in.
    pub fn sort(&self) -> lxb_steam::library::Sort {
        self.sort
    }

    /// List it in another one. Returns whether that is a change — choosing the
    /// order the column is already in is not news, and there is nothing to
    /// rebuild for it.
    pub fn set_sort(&mut self, sort: lxb_steam::library::Sort) -> bool {
        if self.sort == sort {
            return false;
        }
        self.sort = sort;
        true
    }

    /// Look for something else. Returns whether that is a change — the same
    /// question asked twice narrows the same column to the same games, and
    /// rebuilding it would be a keystroke's worth of work for nothing.
    ///
    /// The narrowing itself is [`Self::rows`], which is where every question
    /// about what the column holds is answered together.
    pub fn set_search(&mut self, query: &str) -> bool {
        if self.search == query {
            return false;
        }
        self.search = query.to_string();
        true
    }

    /// What this library can be sorted by at all, which is what greys the rows
    /// of the Sort list that would do nothing. See [`lxb_steam::library::Orders`].
    pub fn orders(&self) -> lxb_steam::library::Orders {
        lxb_steam::library::orders(&self.games)
    }

    /// How far away Steam is.
    pub fn reach(&self) -> &lxb_steam::Reach {
        &self.reach
    }

    /// The line the Games row wears under the account name, or nothing while
    /// Steam is answering and there is only the account to say.
    ///
    /// A library that is a memory is dated rather than passed off as current:
    /// somebody looking at a column with a game missing from it should be able
    /// to see that the column is from this morning.
    pub fn standing(&self) -> Option<String> {
        let said = match &self.reach {
            lxb_steam::Reach::Restoring => {
                crate::i18n::text("shell-connecting-to-steam").to_owned()
            }
            other => other.said()?.to_owned(),
        };
        let Some(read_at) = self.library_as_of else {
            return Some(said);
        };
        Some(crate::message!("steam-library-from", "said" => said, "when" => ago(read_at)))
    }

    pub fn signed_in(&self) -> bool {
        self.account.is_some()
    }

    pub fn has_client(&self) -> bool {
        self.client.client_at().is_some()
    }

    /// Whether what is up is waiting to be typed into, which is what decides
    /// that keys are letters rather than buttons.
    pub fn field_wanted(&self) -> bool {
        matches!(
            self.signing_in,
            Some(Stage::Account(_) | Stage::Password { .. } | Stage::Code { .. })
        )
    }

    /// Whether what is being typed is a password, which is what decides that
    /// the board must not be taken away by Back — a password field with no
    /// keyboard on a console is a field that cannot be filled in.
    pub fn password_wanted(&self) -> bool {
        matches!(self.signing_in, Some(Stage::Password { .. }))
    }

    /// Take in everything the worker has said. Returns what has to be redrawn.
    pub fn sync(&mut self) -> Changed {
        let mut changed = Changed::default();
        // A client that has been installed or removed since the last look
        // changes every row in the column — whether it can be played, whether
        // it can be fetched — so the column is rebuilt for it exactly as it is
        // for a library that arrived.
        if self.asked_after_the_client.elapsed() >= self.how_often_to_look() {
            self.asked_after_the_client = Instant::now();
            changed.library |= self.client.recheck_client();
            // On the same clock and for the same reason: it changes what every
            // row in the column *says* — a game whose manifest describes work
            // says whether that work is happening — so the column is rebuilt
            // for it exactly as it is for a client that came or went.
            changed.library |= self.note_whether_a_client_is_running();
            // A client asked to go and gone is a client nothing is waiting on
            // any more, and so is one that has had long enough and is still
            // here. Either way the fast look stops.
            if self.watching_the_client_go.is_some_and(|since| {
                !self.client_running || since.elapsed() >= UNTIL_THE_CLIENT_GOES
            }) {
                tracing::info!(
                    went = !self.client_running,
                    "done watching the client that was asked to shut down"
                );
                self.watching_the_client_go = None;
            }
        }
        changed.library |= self.note_what_valve_is_doing();
        let said = self.client.take();
        changed.absorb(self.take_in(said));
        changed
    }

    /// Read the client's own account of what it has in hand, and say whether
    /// the answer moved.
    ///
    /// **On its own clock, and it rebuilds the column.** What this sees is work
    /// no manifest describes — a file check moves no bytes — so it is not only
    /// the two rules that close a client that need it: it is what a row *says*.
    /// Without the rebuild the bar would go on reading "Installed" over a game
    /// Steam had in hand until something else happened to change the column.
    ///
    /// A second, because the shutdown it also decides is five, and because a
    /// check on a small game is over in six. Free while nothing is running:
    /// the client's own state is asked first, and this session already knows
    /// the answer.
    ///
    /// Never on a session that is not driving Valve's client — the same rule
    /// [`Self::waiting_for_steam`] is under, and for the same reason: a library
    /// invented behind `--debug-steam-library` must not report the machine it
    /// happens to be running on.
    fn note_what_valve_is_doing(&mut self) -> bool {
        if !self.driving || self.looked_at_valves_log.elapsed() < WHAT_VALVE_IS_DOING {
            return false;
        }
        self.looked_at_valves_log = Instant::now();
        if !self.client.look_at_what_valve_is_doing() {
            return false;
        }
        // Two answers out of one reading, because the row and the shutdown are
        // asking different questions of it.
        let jobs = self.client.what_valve_is_doing();
        self.valve_is_busy = jobs.anything_in_hand();
        self.valve_is_doing = self
            .games
            .iter()
            .filter_map(|game| Some((game.app_id, jobs.to_the_game(game.app_id)?)))
            .collect();
        self.valve_is_fetching_shaders = self
            .games
            .iter()
            .map(|game| game.app_id)
            .filter(|app_id| jobs.fetching_shaders_for(*app_id))
            .collect();
        tracing::info!(
            doing = ?self.valve_is_doing,
            shaders = ?self.valve_is_fetching_shaders,
            busy = self.valve_is_busy,
            "what Valve's client says it has in hand"
        );
        true
    }

    /// How long to leave between two looks at the machine.
    ///
    /// One number nearly always, and the other for the seconds after a client
    /// has been asked to shut down: see [`WHILE_THE_CLIENT_GOES`].
    fn how_often_to_look(&self) -> Duration {
        match self.watching_the_client_go {
            Some(_) => WHILE_THE_CLIENT_GOES,
            None => AFTER_THE_CLIENT,
        }
    }

    /// Look at the machine again, and say whether the answer moved.
    ///
    /// Only for a session that drives Valve's client. A library invented behind
    /// `--debug-steam-library` has no client of its own and must not have the
    /// machine's: its downloads are drawn from the numbers it was given, and a
    /// fixture row reading "Waiting for Steam" would be the shell reporting
    /// this machine into a picture of an invented one.
    fn note_whether_a_client_is_running(&mut self) -> bool {
        if !self.driving {
            return false;
        }
        let now = self.client.client_is_running();
        // Asked on the same look and only of a client that is there: a log is
        // a record of what a client *was* doing, and with nothing running it
        // says who was signed in, not who is. Nothing the column draws moves
        // on this, so it is not part of the answer.
        self.client_signed_in = now && self.client.client_is_signed_in();
        if now == self.client_running {
            return false;
        }
        tracing::info!(running = now, "whether a Steam client is running");
        self.client_running = now;
        true
    }

    /// Whether this game's manifest describes work that nothing is doing.
    ///
    /// Two things at once: the manifest says Valve's client is in the middle of
    /// something, and there is no client running to be in the middle of
    /// anything. Both are needed and neither is enough — a manifest alone is a
    /// record of the past, and a client that is not running says nothing about
    /// any particular game.
    ///
    /// Whether there is a Steam on this machine *to* start is deliberately not
    /// asked here. It is true that nothing is doing this either way, which is
    /// what the row says; what depends on there being a client is what may be
    /// *offered* about it, and that is asked where the offer is made — see
    /// `steam_game_menu_rows` and `Shell::offer_to_start_steam`.
    fn waiting_for_steam(&self, game: &Game) -> bool {
        self.driving && game.standing.moving() && !self.client_running
    }

    /// What is standing between a press on `app_id` and its game this frame,
    /// in the words its loading screen should say — or nothing, where Steam is
    /// doing nothing in front of it and what is left is the window.
    ///
    /// `step` is the launch's own account of itself as the loading screen last
    /// heard it, and `asks_again` whether the client refused that press over
    /// work it is still doing — see `Shell::steam_would_not_start_it`. Asked
    /// once a frame by `Shell::sync_the_launch_downloads`; what the screen does
    /// with the answer is [`crate::launch::Launch::follow_the_work`].
    pub(crate) fn what_a_press_waits_on(
        &self,
        app_id: u32,
        step: Option<&crate::launch::Step>,
        asks_again: bool,
    ) -> Option<crate::Underway> {
        // The step first, because it is the client's own account of what it is
        // doing with this very press — and because for half of these there is
        // nothing on the disk that describes them at all. It is work in flight
        // like any other here: the line says what it is, and the patience stops
        // while it runs. See `Shell::steam_is_working_on_the_launch`.
        //
        // **This is what was missing when a press ran out of patience over a
        // client that was one fifth of the way through answering it.** The
        // game's manifest read `StateFlags 1158` — installed, update required,
        // files corrupt, update started, which is a copy being repaired rather
        // than one of the standings below — and its byte counters were nought.
        // Steam's own launch window said "Downloading content (19%)" the whole
        // time.
        //
        // Only while a client is running. A step is the last thing the client
        // *said*, out of a log that outlives it, and the watcher goes on
        // reading that log for five minutes — so a client that died mid-launch
        // would otherwise hold a loading screen up on a sentence about work
        // nothing is doing. The same guard the quiet stretch of an update is
        // under, and for the same reason.
        if let Some(step) = step.filter(|_| self.a_client_is_running()) {
            return Some(crate::Underway {
                // The manifest's own count where the client's interface has
                // none to give, which on an ordinary press is always — see
                // [`crate::launch::Step::said`]. Throwing it away here is what
                // left a repair counting nothing on the screen while the file
                // on the disk counted it perfectly well.
                said: step.said(self.how_far_along_percent(app_id)),
                arrived: self.game(app_id).map_or(0, |game| game.downloaded),
                // Nothing on the disk counts these, so what says the work is
                // moving is that the client is doing it — the same answer the
                // quiet stretch of an update gives. What ends it is the client
                // walking on to a step with nothing to say, or the launch
                // ending.
                quietly: true,
            });
        }
        // Steam saying what it is doing to this game, and Steam doing it
        // without saying — see [`Self::quietly_working_on_it`]. The second is
        // what the first ten minutes of an update look like from here, and it
        // is work in flight exactly as much as the first is.
        let quietly = self.quietly_working_on_it(app_id);
        self.game(app_id)
            .filter(|game| game.standing.moving() || quietly.is_some())
            .map(|game| crate::Underway {
                said: match quietly {
                    Some(standing) => game.note_working(standing),
                    None => game.note(),
                },
                arrived: game.downloaded,
                quietly: quietly.is_some(),
            })
            // And, under a press the client refused, anything it still has the
            // game in hand for. That is what the refusal was judged against —
            // `Shell::steam_would_not_start_it` counts a shader cache arriving
            // as work in front of the press — and a wait that ended on less
            // would make the second press into the middle of the same work, to
            // be refused for the same reason. Said the way the client's own
            // launch said it until the refusal, because it is the same work: on
            // 2026-09-26 the `DownloadingDepots` that was refused over was
            // Counter-Strike 2's shader cache.
            .or_else(|| {
                let game = self.game(app_id)?;
                (asks_again && self.the_client_has_it_in_hand(app_id)).then(|| crate::Underway {
                    said: crate::i18n::text("shell-downloading-content").to_string(),
                    arrived: game.downloaded,
                    // Counted by nothing on the disk, and ended by the job's
                    // own last line or the client going.
                    quietly: true,
                })
            })
    }

    /// Whether Steam is in the middle of an update to `app_id` that the
    /// manifest has not begun to describe.
    ///
    /// The mirror of [`Self::waiting_for_steam`] — the same two halves, both
    /// the other way round: the manifest says an update is outstanding rather
    /// than under way, and there *is* a client running to be getting on with
    /// it. See [`lxb_steam::Game::update_outstanding`] for how long a manifest
    /// will say nothing while that happens.
    ///
    /// Asked by the loading screen and by nothing else. The row is right to go
    /// on reading "Installed": the game is on the disk and it starts, which is
    /// what the row is answering. What this answers is a different question,
    /// asked only once somebody has pressed the game — whether a press that is
    /// visibly waiting is waiting for something.
    ///
    /// [`Standing::Ready`] and nothing else, which is tighter than "not
    /// moving" and has to be. A paused update is also a manifest with an
    /// outstanding build and no working bit, and it is the opposite case: the
    /// work has stopped and will not start until somebody says so, and a
    /// loading screen that waited it out would wait for ever. That one has its
    /// own answer already — see [`what_is_happening_to_it`] — and this must not
    /// take it.
    /// **And the manifest's own word for it, which is the third way of
    /// knowing and the only one that answered on 2026-09-03.**
    /// `Update Required` is a bit Steam sets on a game it will not start until
    /// it has fetched something, and it is not the same claim as the two build
    /// numbers: Counter-Strike 2 carried it with `TargetBuildID 0`,
    /// `ScheduledAutoUpdate 0` and `UpdateResult 4`, so every other source here
    /// was silent. The press opened a loading screen, Steam went off to fetch,
    /// no window appeared, and a minute later the shell said the game had not
    /// started and shut the client down on top of the download — which is what
    /// put `UpdateResult 4` there, and what made Steam put the next attempt off
    /// further each time. See [`lxb_steam::library::Game::update_required`].
    ///
    /// Only for the loading screen, and this is why it is on this function
    /// rather than in [`Self::quietly_working_on`] with the rest: a game with
    /// an update waiting is on the disk, is startable, and its row is right to
    /// say Installed. Saying "Updating" there is the regression the build
    /// numbers were narrowed to avoid, and it cost two days of a game.
    pub fn quietly_working_on_it(&self, app_id: u32) -> Option<lxb_steam::library::Standing> {
        use lxb_steam::library::Standing;

        let game = self.game(app_id)?;
        self.quietly_working_on(game)
            .or_else(|| {
                let waiting_on_an_update = self.driving
                    && self.client_running
                    && game.standing == Standing::Ready
                    && game.update_required;
                waiting_on_an_update.then_some(Standing::Updating)
            })
            .or_else(|| {
                // And a copy Steam is putting right, which is the second thing
                // this answers that the row must not. A press on a broken copy
                // is handed to the client like any other — Steam repairs one as
                // part of starting it, see
                // [`crate::apps::Game::the_client_can_put_it_right`] — and the
                // loading screen in front of that press has to say what is
                // happening. The row goes on reading "Needs repairing", which
                // is what the copy on the disk still is.
                //
                // "Checking", because that is what the client writes while it
                // does it: `App update changed : Running Update,Verifying
                // Installed,`, for as long as it takes on a 70 GB game.
                let being_repaired =
                    game.standing == Standing::Broken && self.the_client_has_it_in_hand(app_id);
                being_repaired.then_some(Standing::Validating)
            })
    }

    /// The same question about a game already in hand.
    ///
    /// **The build numbers are three situations and this is the one of them
    /// that is work**, which the first cut of this did not ask. `TargetBuildID
    /// != buildid` is equally true of a client working silently, a client that
    /// has looked at the game and put the update off until Tuesday, and a
    /// client that tried it and stopped — and the manifest answers the last two
    /// itself. See [`lxb_steam::Game::scheduled_for`] and
    /// [`lxb_steam::Game::last_result`].
    ///
    /// Reported off this machine, and it cost two days of a game: Counter-Strike
    /// 2 sat at `StateFlags 6` with a build a fortnight old and an appointment
    /// for two in the morning the day after tomorrow, and with any client
    /// running the shell greyed the row, said "Updating", and answered a press
    /// with *"Updating / Open Downloads in Steam"* — for a game that launches
    /// perfectly in Steam's own window.
    ///
    /// **Two independent ways of knowing, and they answer different halves of
    /// one stretch.** The build numbers are all there is for the first seconds:
    /// Valve's client picks a game up and writes nothing anywhere until it has
    /// decided what to do. Its own log then names the job — and keeps naming it
    /// for work the manifest never describes at all, which is how a file check
    /// is seen. See [`lxb_steam::client::work_in_flight`].
    ///
    /// `Ready` and nothing else, which is tighter than "not moving" and has to
    /// be. A paused update is a manifest with an outstanding build and no
    /// working bit and means the opposite — the work has stopped and will not
    /// start until somebody says so — and a copy that needs repairing is
    /// broken until the repair is finished, whatever the client is doing to it.
    /// Both have their own answers already; see [`what_is_happening_to_it`].
    fn quietly_working_on(&self, game: &Game) -> Option<lxb_steam::library::Standing> {
        use lxb_steam::library::Standing;

        if !self.driving || !self.client_running || game.standing != Standing::Ready {
            return None;
        }
        // The client's own account of it, which names the work.
        if let Some(&in_hand) = self.valve_is_doing.get(&game.app_id) {
            return Some(match in_hand {
                lxb_steam::client::InHand::Checking => Standing::Validating,
                lxb_steam::client::InHand::Working => Standing::Updating,
            });
        }
        // And the two build numbers, which are the only thing said in the
        // stretch before the client writes anything at all.
        let outstanding = game.update_outstanding
            && !update_is_deferred(game, unix_now())
            && game.last_result == 0;
        outstanding.then_some(Standing::Updating)
    }

    // --- conversations -----------------------------------------------------

    /// What has been said in every conversation this session has looked at.
    pub fn conversations(&self) -> &lxb_steam::chat::Conversations {
        &self.conversations
    }

    /// Whether Steam is in a state where a message could go out at all.
    ///
    /// Three things, and every one of them is a state the user can see for
    /// themselves: an account, a CM that is answering, and an account that is
    /// not standing offline on Steam. Offline is the same rule the roster is
    /// under — see `Shell::is_offline_on_steam` — because it is the same fact:
    /// a session that has told Steam it is not there is not a session that may
    /// write to people.
    ///
    /// What it does *not* ask is whether Valve's client is running. This is a
    /// conversation on the shell's own CM session, and it needs nothing of the
    /// client at all.
    pub fn can_chat(&self) -> bool {
        self.account.is_some()
            && self.reach.online()
            && self
                .roster
                .me
                .as_ref()
                .is_some_and(|me| me.presence.is_around())
    }

    /// Whether Steam still lists somebody as a friend.
    ///
    /// Asked of the roster every time rather than remembered, because the whole
    /// point of it is that it can stop being true while a conversation is open.
    pub fn is_a_friend(&self, steam_id: u64) -> bool {
        self.roster
            .friends
            .iter()
            .any(|friend| friend.steam_id == steam_id)
    }

    /// Open a conversation: mark it read, and fetch its history if it has not
    /// been fetched.
    pub fn open_conversation(&mut self, friend: u64) {
        if let Some(wanted) = self.conversations.open(friend) {
            self.client.chat(wanted);
        }
    }

    /// The newest invitation in one conversation, if there is one.
    ///
    /// What the accept button acts on and what the legend asks about, which is
    /// the same question twice — see [`lxb_steam::chat::Conversation::
    /// newest_invite`], where "newest" is settled.
    pub fn newest_invite(&self, friend: u64) -> Option<&lxb_steam::chat::Invite> {
        self.conversations.with(friend)?.newest_invite()
    }

    /// One invitation by its number, whichever conversation it is in.
    ///
    /// For a press that has been carrying that number about for a while: an
    /// announcement stands until somebody clears it, and the conversation
    /// behind it may have been opened, read and closed since.
    pub fn invite(&self, friend: u64, id: u64) -> Option<&lxb_steam::chat::Invite> {
        self.conversations
            .with(friend)?
            .invites()
            .iter()
            .find(|invite| invite.id == id)
    }

    /// Mark one as accepted, and give back what to hand to Valve's client.
    pub fn take_the_invite(&mut self, friend: u64, id: u64) -> Option<lxb_steam::chat::Invite> {
        self.conversations.take_the_invite(friend, id)
    }

    /// Put an invented invitation into a conversation, for
    /// `--debug-actions invite`.
    ///
    /// Through [`lxb_steam::chat::Word::Invited`], which is the door a real one
    /// comes through, so everything downstream of the store is exercised for
    /// real. Stamped with the moment it was invented rather than left
    /// unstamped, so that a second one does not land on the first: the connect
    /// string is an invitation's identity, and two invented ones a minute apart
    /// are two different lobbies.
    /// Answers which invitation it became, for the announcement the shell then
    /// raises about it — the fixture goes round `Changed` rather than through
    /// it, because a pass that carried an invented arrival would be a debug
    /// flag reaching into the path a real one takes.
    pub fn pretend_an_invitation(
        &mut self,
        from: u64,
        app_id: Option<u32>,
        game: Option<String>,
    ) -> Option<(u64, u64)> {
        let Some(account) = self.steam_id else {
            tracing::warn!("--debug-actions invite: nobody is signed in to be invited");
            return None;
        };
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_secs() as u32)
            .unwrap_or(0);
        let moved = self.conversations.heard(
            lxb_steam::chat::Heard {
                generation: self.conversations.generation(),
                account,
                word: lxb_steam::chat::Word::Invited {
                    with: from,
                    invite: lxb_steam::chat::Invited {
                        at: stamp,
                        key: None,
                        connect: format!("+connect_lobby 109775240000{stamp:06}"),
                        app_id,
                        game,
                    },
                },
            },
            Instant::now(),
        );
        moved.invited
    }

    /// Ask for a history again, after one that failed.
    pub fn read_the_history_again(&mut self, friend: u64) {
        if let Some(wanted) = self.conversations.read_it_again(friend) {
            self.client.chat(wanted);
        }
    }

    /// Whether what has been typed could be sent to `friend`, and if not, why.
    ///
    /// Asked before the press as well as on it, because the panel says the
    /// reason under the compose field rather than only when somebody tries.
    pub fn may_send(&self, friend: u64, body: &str) -> Result<String, lxb_steam::chat::Refused> {
        lxb_steam::chat::Conversations::may_send(body, self.is_a_friend(friend), self.can_chat())
    }

    /// Send one message. The body is cut to Steam's limit and trimmed on the
    /// way; what comes back is the refusal, where there is one.
    pub fn send_message(
        &mut self,
        friend: u64,
        body: &str,
    ) -> Result<(), lxb_steam::chat::Refused> {
        let body = self.may_send(friend, body)?;
        let wanted = self.conversations.send(friend, body);
        // Somebody who has sent a message has stopped typing, so the next
        // keystroke says so at once rather than waiting out the throttle.
        self.conversations.stopped_typing(friend);
        self.client.chat(wanted);
        Ok(())
    }

    /// Send a failed message again.
    pub fn send_it_again(&mut self, friend: u64, request: u64) -> bool {
        let Some(wanted) = self.conversations.retry(friend, request) else {
            return false;
        };
        self.client.chat(wanted);
        true
    }

    /// Tell somebody this account is typing, at most as often as
    /// [`lxb_steam::chat::TYPING_EVERY`].
    ///
    /// Called on every keystroke and swallowed on nearly all of them, which is
    /// where the throttle lives: a caller that had to decide when to say it
    /// would be a second copy of the rule. Nothing is said at all where a
    /// message could not be sent anyway.
    pub fn typing_at(&mut self, friend: u64) {
        if !self.can_chat() || !self.is_a_friend(friend) {
            return;
        }
        if let Some(wanted) = self.conversations.typing_at(friend, Instant::now()) {
            self.client.chat(wanted);
        }
    }

    /// Mark a conversation read without asking Steam anything.
    pub fn read_the_conversation(&mut self, friend: u64) -> bool {
        self.conversations.read(friend)
    }

    /// Who the account knows, and where each of them is.
    ///
    /// Empty for a session with nobody signed in, and for the second or two
    /// between signing in and Steam answering. Both are the same thing to the
    /// panel that draws it: a list with no rows in it, which says so.
    pub fn roster(&self) -> &lxb_steam::Roster {
        &self.roster
    }

    /// Whether there is a Valve client running at all, as of the last look.
    ///
    /// See [`Self::note_whether_a_client_is_running`], which is what looks.
    pub fn a_client_is_running(&self) -> bool {
        self.client_running
    }

    /// Whether that client is signed in to anybody, as of the last look.
    ///
    /// See [`Self::note_whether_a_client_is_running`], which is what looks,
    /// and [`Steam::client_signed_in`] for the one thing it decides.
    pub fn a_client_is_signed_in(&self) -> bool {
        self.client_signed_in
    }

    /// What a pass of the worker's answers does to the shell's own state.
    ///
    /// Split from [`Self::sync`] for the reason [`Self::apply`] is split from
    /// this: everything above this line needs a worker on the other end of a
    /// channel, and everything below it is the behaviour worth testing. A pass
    /// is not the sum of its events — the account arriving and Steam answering
    /// are two different fields of one answer — so the fold itself is a thing
    /// that can be got wrong, and was.
    fn take_in(&mut self, said: Vec<Event>) -> Changed {
        let mut changed = Changed::default();
        for event in said {
            changed.absorb(self.apply(event));
        }
        changed
    }

    /// What one thing the worker said does to the shell's own state.
    ///
    /// Split out of [`Self::sync`] rather than written inside its loop so that
    /// it can be exercised without a worker on the other end of the channel:
    /// what these events do to the panel is most of the behaviour in this
    /// module, and a test that had to start a thread and reach Steam to see
    /// any of it would test neither.
    fn apply(&mut self, event: Event) -> Changed {
        let mut changed = Changed::default();
        match event {
            Event::AchievementProgress(heard) => {
                changed.trophies = self.trophies.progress_heard(heard)
            }
            Event::Achievements(heard) => changed.trophies = self.trophies.heard(heard),
            // Valve's client installing itself, which happens once on a machine
            // and before anybody can sign in to anything. The panel is one view
            // of it and not where it is held — see [`Steam::setting_up`] — so
            // every arm here moves the state first and the panel only if the
            // panel is up.
            Event::Setup(word) => match word {
                lxb_steam::setup::SetUp::Working(step) => {
                    let moved = self.setting_up.as_ref() != Some(&step);
                    self.setting_up = Some(step.clone());
                    // The panel only if there is one. A setup somebody walked
                    // away from must not ask for a frame every second for the
                    // four minutes it takes, on a bar nobody is looking at it
                    // from.
                    if matches!(self.signing_in, Some(Stage::FirstSetup(_))) {
                        self.signing_in = Some(Stage::FirstSetup(step));
                        changed.panel = moved;
                    }
                }
                lxb_steam::setup::SetUp::Done => {
                    tracing::info!("Steam is installed on this machine");
                    self.setting_up = None;
                    // Straight on to the questions for whoever stayed, and a
                    // notification for whoever did not. Never both: somebody
                    // watching the panel move on does not also need telling.
                    if self.setup_is_watched {
                        self.signing_in = Some(Stage::Choosing);
                        changed.panel = true;
                    } else {
                        changed.steam_is_ready = true;
                    }
                    self.setup_is_watched = false;
                    // The Steam row says what the client is, and there is one
                    // now where there was none.
                    changed.account = true;
                }
                lxb_steam::setup::SetUp::Failed(why) => {
                    tracing::warn!(%why, "Steam could not install itself on this machine");
                    self.setting_up = None;
                    // Raised even where the panel had been dismissed. A
                    // download that finished is a good ending and can wait for
                    // somebody to come back to it; one that failed is a
                    // question — try again, or not — and a session that
                    // swallowed it would leave a Steam row that goes on
                    // offering to sign in and never can.
                    self.signing_in = Some(Stage::SetupFailed(why));
                    self.setup_is_watched = false;
                    changed.panel = true;
                }
            },
            Event::Installing {
                app_id,
                done,
                total,
                live,
            } => {
                // A byte arriving takes back "not moving", which is what makes
                // that word a description of now rather than a mark the row
                // wears for the rest of the session.
                let before = self.fetching.get(&app_id);
                let stuck = before.is_some_and(|before| before.stuck && before.done >= done);
                let now = Fetching {
                    done,
                    total,
                    live,
                    stuck,
                };
                changed.library |= before != Some(&now);
                self.fetching.insert(app_id, now);
            }
            Event::InstallStuck { app_id, quiet_for } => {
                tracing::warn!(
                    app_id,
                    quiet = quiet_for.as_secs(),
                    "this download says it is running and nothing is arriving"
                );
                if let Some(fetching) = self.fetching.get_mut(&app_id) {
                    changed.library |= !fetching.stuck;
                    fetching.stuck = true;
                }
            }
            Event::Installed { app_id, .. } => {
                self.fetching.remove(&app_id);
                // The row has to become a game that can be started, and what
                // decides that is the manifest on the disk rather than
                // anything said here. So: look again, now, rather than at the
                // next interval — somebody is watching this one finish.
                self.client.refresh();
                changed.library = true;
                changed.installed.push(Ended::Done { app_id });
            }
            Event::InstallFailed { app_id, why } => {
                self.fetching.remove(&app_id);
                changed.library = true;
                changed.installed.push(Ended::Failed { app_id, why });
            }
            Event::Uninstalling { app_id } => {
                changed.library |= self.removing.insert(app_id);
            }
            Event::Uninstalled { app_id } => {
                self.removing.remove(&app_id);
                // The same reason the finished download asks: what the row
                // says next is on the disk, and the disk has just changed.
                self.client.refresh();
                changed.library = true;
                changed.installed.push(Ended::Removed { app_id });
            }
            Event::UninstallFailed { app_id, why } => {
                self.removing.remove(&app_id);
                changed.library = true;
                changed.installed.push(Ended::RemoveFailed { app_id, why });
            }
            // How far a move has got. Only the one this session asked for: a
            // word about any other game is a word about a move this shell is
            // not showing anybody.
            Event::Moving(said) => {
                if let Some(moving) = self
                    .moving
                    .as_mut()
                    .filter(|moving| moving.app_id == said.app_id)
                {
                    let percent = Some(said.percent);
                    changed.moving = moving.percent != percent;
                    moving.percent = percent;
                }
            }
            // It ended. The game's manifest is somewhere else now, or it is
            // where it was, and either way the disk is looked at again now
            // rather than at the next interval.
            Event::Moved { app_id, to, how } => {
                if self
                    .moving
                    .as_ref()
                    .is_some_and(|moving| moving.app_id == app_id)
                {
                    self.moving = None;
                }
                self.client.refresh();
                changed.library = true;
                changed.moved.push((app_id, to, how));
            }
            Event::Shelved { job, how } => {
                self.client.refresh();
                changed.library = true;
                changed.shelved.push((job, how));
            }
            // Nothing to announce and nothing to explain: somebody asked for
            // this and the machine is as they left it. The row stops counting
            // and goes back to being a game that is not installed, which is
            // the whole of what happened.
            Event::InstallStopped { app_id } => {
                self.fetching.remove(&app_id);
                changed.library = true;
            }
            // Nor is there anything to announce here. The download stopped
            // moving without finishing and without failing — Steam paused it,
            // or what arrived needs repairing — and the row goes back to being
            // described by the library, which now says which of those it is in
            // its own words. What ends is the counting.
            Event::InstallWaiting { app_id } => {
                changed.library |= self.fetching.remove(&app_id).is_some();
            }
            Event::Client { ticket, report } => {
                tracing::info!(?ticket, ?report, "Valve's background client");
                // The freshest answer there is to the question every row of the
                // column asks — see [`Steam::client_running`]. Taken here rather
                // than waited for on the ten-second look, because a row that
                // says "Waiting for Steam" beside a client that has just come
                // up is the row being ten seconds behind the machine.
                let waking = matches!(report, lxb_steam::ClientReport::Waking);
                if report == lxb_steam::ClientReport::Ready {
                    // Ready is a client proved up *and* signed in as this
                    // account, so both answers are known here and now.
                    self.client_running = true;
                    self.client_signed_in = true;
                }
                if waking != self.client_waking {
                    self.client_waking = waking;
                    changed.library = true;
                }
                changed.library |= report == lxb_steam::ClientReport::Ready;
                // Whose answer it is is the gate's question, not this one's:
                // what a client is doing is a fact about the machine and true
                // for every row whoever asked. What travels with it is the
                // ticket, so the one thing that *is* somebody's — the press
                // waiting to start a game — can tell.
                changed.client.push((ticket, report));
            }
            Event::ClientClosing { ticket, how } => {
                // Only the ask that is still outstanding. Two are in flight
                // over one client whenever the grace period runs out and it is
                // asked again, and the first one's answer landing after the
                // second was made would have the shell deciding what became of
                // a client from the answer to a different question.
                if self.closing != Some(ticket.request) {
                    tracing::info!(
                        ?ticket,
                        ?how,
                        waiting_on = ?self.closing,
                        "an answer about closing a client nothing is waiting on"
                    );
                    return changed;
                }
                tracing::info!(
                    ?how,
                    "what became of the client that was asked to shut down"
                );
                self.closing = None;
                self.how_the_close_went = Some(how);
                // Nothing to watch for. The fast look exists to catch a client
                // going within a second of being asked, and a client that was
                // never asked — or that had already gone — is not going to.
                if matches!(
                    how,
                    lxb_steam::client::Closing::NotOurs | lxb_steam::client::Closing::Gone
                ) {
                    self.watching_the_client_go = None;
                }
            }
            Event::Reach(reach) => {
                if reach == lxb_steam::Reach::Online && self.reach != reach {
                    self.trophies.reconnected();
                }
                if self.reach != reach {
                    tracing::info!(?reach, "how far away Steam is");
                    self.reach = reach;
                    changed.reach = true;
                }
            }
            Event::LibraryAsOf(read_at) => {
                changed.reach |= self.library_as_of != read_at;
                self.library_as_of = read_at;
            }
            Event::HandOverRefused(refused) => {
                tracing::info!(?refused, "Valve's client was not handed a request");
                changed.hand_over = Some(refused);
            }
            Event::HandedOver(handed) => {
                tracing::info!(
                    after_signing_in = handed.after_signing_in,
                    "Valve's client took the request and is about to show itself"
                );
                changed.handed_over = Some(handed);
            }
            Event::LaunchIsAsking(asking) => {
                // The watcher polls Valve's client every two seconds and then
                // speaks, and the press it is speaking about can end in
                // between — so a question about a launch this shell has already
                // let go of would otherwise take down a loading screen for a
                // different game, on a display nobody pressed anything on.
                //
                // Asked of this game's own watch, not of the session's: another
                // display's launch being watched says nothing about this one.
                if self.watched_launches.get(&asking.app_id) != Some(&asking.request) {
                    tracing::info!(
                        app_id = asking.app_id,
                        request = asking.request,
                        watching = ?self.watched_launches.get(&asking.app_id),
                        "a launch nobody is watching any more has stopped to ask"
                    );
                    return changed;
                }
                tracing::info!(?asking, "the launch has stopped for a person");
                changed.asking = Some(asking);
            }
            Event::LaunchWasRefused(refused) => {
                // The same guard the question above is under, for the same
                // reason: the watcher decides to speak, and the press it is
                // speaking about can end between that decision and this line.
                if self.watched_launches.get(&refused.app_id) != Some(&refused.request) {
                    tracing::info!(
                        app_id = refused.app_id,
                        request = refused.request,
                        watching = ?self.watched_launches.get(&refused.app_id),
                        "a launch nobody is watching any more was given up on"
                    );
                    return changed;
                }
                tracing::warn!(?refused, "Valve's client will not start this game");
                changed.launch_refused = Some(refused);
            }
            Event::LaunchIsWorkingOnIt(step) => {
                // The same guard again: a watcher that decides to speak about
                // a press this shell has let go of speaks about nothing.
                if self.watched_launches.get(&step.app_id) != Some(&step.request) {
                    return changed;
                }
                changed.step = Some(step);
            }
            Event::Compatibility { which, said } => {
                self.forcing.remove(&which);
                self.compat.insert(which, Compat::Said(said));
                changed.compat = Some(which);
            }
            Event::CompatibilityUnavailable { which, why } => {
                tracing::warn!(%which, %why, "Steam would not say what this runs under");
                if self.forcing.remove(&which) {
                    changed.compat_refused = Some(why.clone());
                }
                // Forgotten rather than remembered as a refusal, where a press
                // was what failed: what is on the disk has not changed, and the
                // next look asks Steam again rather than drawing the last list
                // over an answer nobody knows any more.
                match changed.compat_refused.is_some() {
                    true => self.compat.remove(&which),
                    false => self.compat.insert(which, Compat::Unavailable(why)),
                };
                changed.compat = Some(which);
            }
            Event::TheWays { app_id, ways } => {
                self.ways.insert(app_id, Ways::Said(ways));
                changed.ways = Some(app_id);
            }
            Event::TheWaysUnavailable { app_id, why } => {
                tracing::warn!(app_id, %why, "Steam would not say how this game starts");
                self.ways.insert(app_id, Ways::Unavailable(why));
                changed.ways = Some(app_id);
            }
            Event::SignedIn(account) => {
                self.trophies.account(Some(account.steam_id));
                let name = Some(account.name);
                changed.account |= self.account != name;
                self.account = name;
                self.steam_id = Some(account.steam_id);
                // A different account takes every conversation with it — the
                // messages, the drafts' worth of pending sends, the unread
                // counts and anything still in flight. The *same* account
                // signing in again is a reconnect, and keeps what is drawn.
                changed.chat |= self.conversations.signed_in_as(account.steam_id);
                // The panel goes away the moment it has succeeded: what it
                // was asking has been answered, and the answer is a whole
                // column further along the bar.
                changed.panel |= self.signing_in.take().is_some();
            }
            Event::Friends(roster) => {
                changed.friends |= self.roster != roster;
                // Somebody who is no longer on the list can no longer be
                // written to. Nothing is *forgotten* — the conversation stays
                // on screen and stays readable, which is the honest answer:
                // what was said was said — but the compose field stops
                // offering. See [`Steam::may_send`], which asks the roster
                // every time rather than remembering an answer.
                self.roster = roster;
            }
            Event::Chat(heard) => {
                let moved = self.conversations.heard(heard, Instant::now());
                changed.chat |= moved.redraw;
                changed.reconnected |= moved.reconnected;
                changed.messages.extend(moved.announce);
                changed.invites.extend(moved.invited);
            }
            Event::SignedOut => {
                self.trophies.account(None);
                changed.trophies = true;
                // Back to the state a session with nobody signed in is in.
                // Leaving "offline" standing would put a reconnecting line
                // under the Sign in row, which is about an account that is no
                // longer there.
                changed.reach |=
                    self.reach != lxb_steam::Reach::Online || self.library_as_of.is_some();
                self.reach = lxb_steam::Reach::Online;
                self.library_as_of = None;
                changed.account |= self.account.take().is_some();
                self.steam_id = None;
                // The friends go with the account. Everything about them was
                // true of somebody who is no longer signed in, and a panel
                // still holding them would be showing the next person at this
                // machine a list of names they have never met.
                changed.friends |= self.roster != lxb_steam::Roster::default();
                self.roster = lxb_steam::Roster::default();
                // And every conversation, on exactly the same terms and for a
                // stronger reason: a roster left standing would show the next
                // person a list of names, and a conversation left standing
                // would show them what was said in it.
                changed.chat |= self.conversations.nobody_is_signed_in_now();
                changed.library |= !self.games.is_empty();
                self.games.clear();
                // And the rows that were counting. These are this session's
                // interest in a download, not the download — Valve's client
                // carries on with what it was asked for — and an interest in it
                // is exactly what signing out ends. Left standing, they were a
                // row in the next person's library counting up towards a game
                // they do not own. The worker forgets its own half at the same
                // moment; see `Watching::nobody_is_signed_in_now`.
                changed.library |= !self.fetching.is_empty() || !self.removing.is_empty();
                self.fetching.clear();
                self.removing.clear();
                // A question asked of one account is not asked of the next.
                // The column goes with the library either way; what this
                // stops is somebody signing in and finding four of their games
                // where the whole library should be, narrowed by a word the
                // last person typed.
                self.search.clear();
                // Only a sign-in that was under way: a session that starts
                // with nobody signed in says so, and there is no panel up
                // for it to be about.
                changed.panel |= self.signing_in.take().is_some();
                // And the two questions still out at Valve's client. Both were
                // asked about an account that has gone, and neither answer is
                // about this session any more: a shutdown asked for one
                // account's client, and a launch nobody here started. The
                // worker drops what it can — see `lxb_steam::Ticket` — and this
                // is the half that has to agree.
                self.closing = None;
                self.how_the_close_went = None;
                // Every launch in flight was started for the account that has
                // gone, so every watch goes — this is the one moment when
                // stopping all of them is the right answer.
                self.watched_launches.clear();
                self.client.stop_watching_every_launch();
            }
            Event::Library(games) => {
                changed.library |= self.games != games;
                self.trophies.library(&games);
                self.games = games;
                if matches!(self.signing_in, Some(Stage::LibraryUnavailable(_))) {
                    self.signing_in = None;
                    changed.panel = true;
                }
            }
            Event::Challenge { code, .. } => {
                self.signing_in = Some(Stage::Qr(Some(code)));
                changed.panel = true;
            }
            Event::CodeWanted(confirmation) => {
                self.signing_in = Some(Stage::Code {
                    confirmation,
                    typed: String::new(),
                });
                changed.panel = true;
            }
            Event::Waiting(note) => {
                self.signing_in = Some(Stage::Waiting(note));
                changed.panel = true;
            }
            Event::SignInFailed(why) => {
                // Only while a sign-in is on screen. A token refused in
                // the background is a sign-out, which arrives as one; a
                // panel conjured over the bar to report it would be the
                // shell interrupting somebody who was doing something
                // else.
                if self.signing_in.is_some() {
                    self.signing_in = Some(Stage::Failed(why));
                    changed.panel = true;
                }
            }
            Event::LibraryUnavailable(why) => {
                self.signing_in = Some(Stage::LibraryUnavailable(why));
                changed.panel = true;
            }
        }
        changed
    }

    /// The Trophies column has its own order and search, separate from Steam.
    pub fn trophy_games(&self) -> Vec<crate::apps::Entry> {
        if !self.signed_in() {
            return Vec::new();
        }
        self.trophies
            .rows(&self.games, lxb_steam::library::Sort::InstalledFirst)
    }

    pub fn watch_trophies(&mut self, games: BTreeSet<u32>, keys: &[crate::trophies::Key]) -> bool {
        let icons: Vec<_> = keys
            .iter()
            .filter_map(|key| self.trophies.icon_for(key))
            .collect();
        self.trophies.want_icons(&icons);
        self.trophies
            .watch(games, &self.client, self.reach == lxb_steam::Reach::Online)
    }

    /// The rows the Steam column is made of, in the order they go in.
    ///
    /// Empty whenever there is no column to be had — nobody signed in, or a
    /// library with nothing in it — which is what [`crate::apps::shelve_steam`]
    /// reads as "take the column away".
    ///
    /// This is where the order the user chose is put on: the library arrives
    /// installed-first and is kept that way, and every other order is a pass
    /// over borrowed rows on the way to the bar. Called when the library
    /// changes, when the order does and on each letter typed into the field,
    /// and not otherwise — nothing here is asked per frame.
    ///
    /// Three things stand over the column and they go in the order somebody
    /// arrives at them from below: the field, the row that empties it, and the
    /// index. Everything under them is what the search has left — the index
    /// included, since it is the same library seen another way and an index
    /// listing games the column no longer shows would be an index that lied.
    /// The column proper starts at the first game, because that is what
    /// somebody walking into their library came to see; see
    /// [`crate::apps::head_rows`].
    pub fn rows(&self) -> Vec<crate::apps::Entry> {
        if !self.signed_in() || self.games.is_empty() {
            return Vec::new();
        }
        // In the library's own order — installed first, each half by name —
        // whatever the column is about to be listed in, because the index
        // below is built out of this and that is the order a letter holds its
        // games in. One `contains` per game per letter typed.
        let matched: Vec<&Game> = match lxb_steam::library::sought(&self.search) {
            Some(needle) => self
                .games
                .iter()
                .filter(|game| game.matches(&needle))
                .collect(),
            None => self.games.iter().collect(),
        };

        let mut rows = Vec::with_capacity(matched.len() + 3);
        crate::apps::head(
            &mut rows,
            crate::apps::Searched::Library,
            &self.search,
            matched.len(),
            self.games.len(),
        );
        // A search that found nothing leaves the two rows that say why and
        // nothing else: no index, because there is nothing to index, and a
        // heading standing over an empty column is a press that opens nothing.
        if matched.is_empty() {
            return rows;
        }

        rows.push(Self::alphabetical(&matched, |game| self.row(game)));
        let mut listing = matched;
        // The default order is the one the library is already in, so the
        // ordinary column costs nothing to build.
        if self.sort != lxb_steam::library::Sort::default() {
            listing.sort_by(|a, b| self.sort.compare(a, b));
        }
        rows.extend(listing.into_iter().map(|game| self.row(game)));
        rows
    }

    /// The index at the head of the library: the whole of what the column shows
    /// again, under the letter each game starts with.
    ///
    /// A library of a few hundred titles is a column nobody can reach the far
    /// end of. Every order the Sort list offers is still one list — it answers
    /// "what have I got most of" or "what did I play last", and none of them
    /// answers "where is Portal". So the letters are the way *in* to a long
    /// list, in the one place a thing standing over a list can stand: above its
    /// first row, reached by pressing Up from it.
    ///
    /// The field above it answers "where is Portal" for somebody who knows the
    /// name and can type it; this answers it for somebody with a pad in their
    /// hands, who reaches twenty-seven rows in two presses and a keyboard in
    /// rather more. Neither is the other's fallback — they are the same
    /// question asked by two different people, and the column carries both for
    /// the same reason a shelf of music carries a field over an ordered list.
    ///
    /// Inside a letter the library's own order stands: installed first, then by
    /// name. The rest of the column is sorted however the user asked and this is
    /// not, but what a letter is asked is still "which of these can I play" —
    /// the same question the top of the column answers, and the one an index
    /// that buried an installed game under six the account merely owns would
    /// answer worst. `matching` arrives in that order, so the letters cost a
    /// walk and no sort.
    ///
    /// The games are built a second time rather than shared. What a row *says*
    /// is a snapshot of the moment it was built — how far a download has got,
    /// whether the disk has it — and two rows of one game with two different
    /// notes on them is the bug this avoids by having no way to happen: both
    /// come from [`Self::row`], in the same pass, out of the same library.
    ///
    /// A heading is a *letter* rather than a row with a letter written on it:
    /// the character is cut out of the shell's own face and stood in the room a
    /// row's mark has — see [`crate::icons::INDEX_LETTERS`] — so the column is
    /// read down the alphabet the way a shelf of books is, and the words on the
    /// rows are free to say how much is in each.
    pub(crate) fn alphabetical(
        matching: &[&Game],
        make_row: impl Fn(&Game) -> crate::apps::Entry,
    ) -> crate::apps::Entry {
        Self::index(
            matching.iter().map(|game| (game.initial(), make_row(game))),
            crate::icons::STEAM,
        )
    }

    /// The index at the head of a library of games, out of its rows and the
    /// letter each is filed under — Steam's column's, and Epic's, which is
    /// the same index over another store's games. See [`Self::alphabetical`].
    ///
    /// `fallback` is the mark a heading wears where the shell has no letter
    /// cut for it, which is the store's own.
    pub(crate) fn index(
        games: impl IntoIterator<Item = (Option<char>, crate::apps::Entry)>,
        fallback: &str,
    ) -> crate::apps::Entry {
        // `None` is everything that does not start with one of the headings the
        // shell cuts, and it goes first because that is where nearly all of it
        // already is in the column's own order — a digit sorts before a letter
        // — and because a heap of odd names is a thing to pass on the way to
        // the alphabet rather than something to find after it. `BTreeMap` puts
        // it there by itself, `None` before every `Some`, which is the whole
        // reason the letter is an `Option` here rather than a `char` with a
        // stand-in in it.
        let mut letters: BTreeMap<Option<char>, Vec<crate::apps::Entry>> = BTreeMap::new();
        for (letter, row) in games {
            letters.entry(letter).or_default().push(row);
        }

        let entries = letters
            .into_iter()
            .map(|(letter, games)| {
                // "#" for the rest, which is the mark a list of names has used
                // for "not a letter" since long before this shell.
                let heading = letter.unwrap_or('#');
                crate::apps::Entry::Folder(crate::apps::Folder {
                    title_message: None,
                    comment_message: None,
                    identity: None,
                    // What the row has to say, now that the letter is the mark
                    // and not the words: how far this heading goes. A letter is
                    // the one row in the shell whose title is a quantity, and it
                    // can be because the thing it is a heading *for* is already
                    // drawn beside it.
                    title: counted(games.len()),
                    comment: None,
                    // The letter itself, or the Steam mark for a heading this
                    // shell has no cell for — which cannot happen while
                    // `Game::initial` answers out of the same set, and is here
                    // because the two are in different crates and only one of
                    // them can be right about that.
                    icon: Some(
                        crate::icons::letter_mark(heading)
                            .unwrap_or(fallback)
                            .to_string(),
                    ),
                    entries: games,
                    place: None,
                    chosen: false,
                    over_the_list: false,
                    person: None,
                    portrait: None,
                    used: None,
                })
            })
            .collect();

        crate::apps::Entry::Folder(crate::apps::Folder {
            title_message: Some("shell-alphabetical"),
            comment_message: None,
            identity: None,
            title: crate::i18n::text("shell-alphabetical").to_string(),
            comment: Some(
                crate::i18n::text("shell-every-game-in-the-library-by-its-first-letter")
                    .to_string(),
            ),
            // The alphabet itself, named by its two ends and cut from the same
            // face as the headings behind the row — see
            // [`crate::icons::INDEX_MARK`]. The Steam mark stood here first,
            // which said only what every row in this column already says.
            icon: Some(crate::icons::INDEX_MARK.to_string()),
            entries,
            place: None,
            chosen: false,
            // The whole of why this is a field on a folder: the column opens on
            // the first game, and this is above it. See
            // [`crate::apps::head_rows`].
            over_the_list: true,
            person: None,
            portrait: None,
            used: None,
        })
    }

    /// One game, as a row.
    ///
    /// The one place a title becomes a row, so the column and the index cannot
    /// come to different conclusions about what a game is doing.
    fn row(&self, game: &Game) -> crate::apps::Entry {
        // A game this shell is moving on or off the disk says so, over
        // whatever the disk says: there is a stretch at the start of each —
        // before Valve's client has written anything — where the disk still
        // holds the answer to the previous question, and a row that gave it
        // would read as a press that did nothing.
        let fetching = self.fetching.get(&game.app_id);
        let removing = self.removing.contains(&game.app_id);
        // And a game whose manifest says Steam is in the middle of something
        // says so only while there is a Steam to be in the middle of it. See
        // [`Steam::waiting_for_steam`].
        //
        // Never over something this session is itself in the middle of: those
        // two carry their own wake, so a client that is not up yet is one that
        // is on its way, and the row already has its own words for both.
        let waiting_for_steam = !removing && fetching.is_none() && self.waiting_for_steam(game);
        // And the mirror of it: Steam **is** running and is in the middle of an
        // update this manifest has not begun to describe. See
        // [`Steam::quietly_working_on_it`].
        //
        // Reported by the user, off a real update, and it is the moment the
        // loading screen is dismissed with Back: the bar comes up and the row
        // reads "Installed · 10 GB · 119 hours played" — a game that looks
        // ready to play — until Steam writes its first working bit and it
        // turns into "Updating". On this machine that moment lasted thirty
        // seconds, and it can last ten minutes. The game is not startable
        // there: Steam has it, and a press would queue behind the update.
        // And what it is: a game Steam is checking over is not one it is
        // updating, and the word the row uses is the same word it would use if
        // the manifest had said so itself.
        let quietly = match !removing && fetching.is_none() {
            true => self.quietly_working_on(game),
            false => None,
        };
        let quietly_updating = quietly.is_some();
        let note = match (removing, fetching) {
            (true, _) => crate::i18n::text("shell-removing").to_string(),
            (false, Some(so_far)) => so_far.said(),
            // And while one is on its way up, the answer to "why is nothing
            // happening" has changed: something is.
            (false, None) if waiting_for_steam && self.client_waking => {
                crate::i18n::text("shell-starting-steam").to_string()
            }
            (false, None) if waiting_for_steam => {
                game_progress_note(game, crate::i18n::text("steam-waiting"))
            }
            (false, None) if let Some(standing) = quietly => {
                crate::i18n::builtin(standing.said()).to_owned()
            }
            (false, None) => game_note(game),
        };
        // And the same fact as a bar. Read from whichever of the two said the
        // sentence above, so the picture and the words on one row can never be
        // about different numbers.
        let progress = match (removing, fetching) {
            (true, _) => None,
            (false, Some(so_far)) => so_far.fraction().map(|share| crate::apps::Arriving {
                share,
                stuck: so_far.stuck,
            }),
            (false, None) => game.fraction().map(|share| crate::apps::Arriving {
                share,
                // Only a download this session is watching has a clock on it;
                // see `stuck` below, which is the same rule.
                stuck: false,
            }),
        };
        crate::apps::Entry::Game(crate::apps::Game {
            app_id: game.app_id,
            name: game.name.clone(),
            note,
            progress,
            ways: game.ways_here(),
            // Neither a game on its way in nor one on its way out can be
            // started, and both are busy: one row state for the two of them,
            // because what the bar does about it is the same.
            // Whether it can be *started*, which is what the press, the sort
            // and the colour of the cover all mean by "installed". A copy on
            // the disk that cannot run — one being repaired, one half
            // downloaded — is not this, and says which it is in `standing`.
            //
            // `quietly_updating` counts, and it has to: this is what drains the
            // colour out of the cover — see `Slots::drain` — and it is what the
            // press reads. Without it the row said "Updating" beside a cover
            // still lit as though the game were ready, and then greyed a second
            // later when Steam finally wrote its working bit. The user reported
            // exactly that: *it shows as installed for a second and then goes
            // as uninstalled*. The two states are one state and must look like
            // it.
            //
            // The press follows, and lands where it should: `offer_to_install`
            // sends a game on the disk that cannot start to
            // `what_is_happening_to_it`, which says "Updating" and offers
            // Downloads — which is precisely what the same press gets a second
            // later. **The loading screen is not lost**: it belongs to the
            // press made while Valve's client is *not* running, and
            // `quietly_updating` is false there, so that row is still
            // `installed` and still opens the splash that waits the update out.
            installed: game.standing.playable()
                && !removing
                && fetching.is_none()
                && !quietly_updating,
            // Only for a download this session is watching. A game somebody
            // started fetching in Steam an hour ago has no clock here, and the
            // shell has no business claiming it has stopped.
            stuck: fetching.is_some_and(|so_far| so_far.stuck),
            updating: game.updating || removing || fetching.is_some() || quietly_updating,
            waiting_for_steam,
            steam_client: self.client.client_at().is_some(),
            // What this session is doing to it wins over what the disk says,
            // for the same stretch and the same reason the note does: a press
            // is answered before Valve's client has written anything, and a
            // menu built from the disk in that gap would offer to install a
            // game that is already coming down.
            standing: match (removing, fetching) {
                (true, _) => lxb_steam::library::Standing::Uninstalling,
                (false, Some(_)) if !game.standing.on_the_disk() => {
                    lxb_steam::library::Standing::Queued
                }
                // And the work the manifest has not admitted to, which is a
                // standing like any other as far as everything downstream is
                // concerned: the menu branches on this, and a game Steam is
                // checking over must not be offered Verify.
                _ => quietly.unwrap_or(game.standing),
            },
        })
    }

    /// Ask the client to do one thing to one title — check it, remove it, or
    /// come to the front. Playing is not one of these; see [`Self::play`].
    pub fn tell(&self, app_id: u32, doing: Doing) -> Result<(), String> {
        self.client.tell(app_id, doing)
    }

    // --- the sign-in ------------------------------------------------------

    /// Raise the panel, on the first question — or on the wait that has to
    /// come before the questions.
    ///
    /// On a machine where Steam has never run there is nothing to sign in to
    /// yet: what a distribution packages is a launcher, and the client proper
    /// is half a gigabyte it fetches the first time it is started. So the first
    /// press starts that and the panel says so, and the questions come when it
    /// is done. Everything about the sign-in itself is unchanged — the code on
    /// the screen is Steam's own service and needs no client — but a person who
    /// signed in and then pressed a game would find the shell installing Steam
    /// under a loading screen with no idea why it was taking four minutes, so
    /// the install is put where somebody can see it.
    ///
    /// Pressing the row again while an install is running comes back here and
    /// gets the same panel back, which is the whole of how a setup somebody
    /// walked away from is returned to.
    pub fn begin(&mut self) {
        if let Some(step) = self.setting_up.clone() {
            self.setup_is_watched = true;
            self.signing_in = Some(Stage::FirstSetup(step));
            return;
        }
        if self.client.client_needs_setting_up() {
            self.client.set_up_the_client();
            let step = lxb_steam::setup::Step {
                said: crate::i18n::text("shell-getting-steam-ready").to_string(),
                percent: None,
            };
            self.setting_up = Some(step.clone());
            self.setup_is_watched = true;
            self.signing_in = Some(Stage::FirstSetup(step));
            return;
        }
        self.signing_in = Some(Stage::Choosing);
    }

    /// Whether the shell's own sign-in panel is up, without building it.
    ///
    /// [`Self::panel`] answers the same question and allocates a panel's worth
    /// of strings doing it, which is fine once a press and not fine on the pass
    /// that decides what the compositor may show — that runs whenever there is
    /// a window of Valve's being hidden.
    pub fn is_signing_in(&self) -> bool {
        self.signing_in.is_some()
    }

    /// Whether Valve's client is installing itself for this session right now.
    ///
    /// Asked by the shell for two things it decides: whether the row somebody
    /// pressed is a setup to come back to, and whether Valve's own windows may
    /// be let through. Nothing of the client's may reach the screen while this
    /// is true — the client it is installing comes up on its own login screen,
    /// and this shell has its own.
    pub fn setting_up(&self) -> bool {
        self.setting_up.is_some()
    }

    /// Take the panel down and leave the install running.
    ///
    /// What Back does on the one panel that is a wait rather than a question.
    /// Nothing is cancelled: half a gigabyte is coming down, stopping it would
    /// throw away whatever had arrived, and the person pressing Back is saying
    /// they would rather not watch — not that they have changed their mind
    /// about Steam. What they get instead is a notification when it is done.
    pub fn let_the_setup_run_in_the_background(&mut self) {
        self.setup_is_watched = false;
        self.signing_in = None;
    }

    /// Start the install again after one that failed.
    pub fn set_up_again(&mut self) {
        self.client.set_up_the_client();
        let step = lxb_steam::setup::Step {
            said: crate::i18n::text("shell-getting-steam-ready").to_string(),
            percent: None,
        };
        self.setting_up = Some(step.clone());
        self.setup_is_watched = true;
        self.signing_in = Some(Stage::FirstSetup(step));
    }

    /// Give up on whatever is on screen.
    ///
    /// Three stages are not sign-ins and must not be reported to Steam as
    /// abandoned ones: the library that would not arrive, and the two about
    /// Valve's client installing itself. Nothing has been asked of any account
    /// in any of them, and telling the worker a sign-in was cancelled while one
    /// was not under way is a message about the wrong thing. The install itself
    /// is not stopped either — see
    /// [`Self::let_the_setup_run_in_the_background`], which is what Back
    /// actually does on that panel.
    pub fn cancel(&mut self) {
        let cancelling_sign_in = self.signing_in.as_ref().is_some_and(|stage| {
            !matches!(
                stage,
                Stage::LibraryUnavailable(_) | Stage::FirstSetup(_) | Stage::SetupFailed(_)
            )
        });
        if matches!(self.signing_in, Some(Stage::FirstSetup(_))) {
            self.setup_is_watched = false;
        }
        self.signing_in = None;
        if cancelling_sign_in {
            self.client.cancel_sign_in();
        }
    }

    pub fn sign_out(&mut self) {
        self.client.sign_out();
    }

    pub fn refresh(&mut self) {
        self.client.refresh();
    }

    /// Put the account into one of Steam's statuses, because somebody chose it
    /// on the friends panel.
    ///
    /// Answered through [`Self::sync`] as [`Changed::friends`], like every
    /// other movement in the roster: what the head of the panel says is what
    /// Steam last said about the account, and this is a request rather than the
    /// answer to one. See [`lxb_steam::Ask::SetStatus`].
    pub fn set_status(&mut self, status: lxb_steam::Presence) {
        self.client.set_status(status);
    }

    /// Have Valve's client running and signed in, because a game is about to
    /// need it.
    ///
    /// Answered through [`Self::sync`] as [`Changed::client`]. On a client
    /// that is already up this is a few microseconds and one event; on a cold
    /// one it is most of a minute, which is what the loading screen is for.
    ///
    /// Returns the number the wake was asked under. Whoever is going to wait on
    /// it has to keep that number and check it against what comes back — see
    /// [`Gate`], which is what does.
    pub fn wake_client(&mut self) -> u64 {
        self.client.wake_client()
    }

    /// The same, for somebody who has been shown what it costs and said yes.
    ///
    /// Only ever reached from the panel that asks. A client belonging to
    /// another session on this machine is stopped by this, and its downloads
    /// with it; one signed in to another account is signed out of it.
    pub fn take_over_the_client(&mut self) -> u64 {
        self.client.take_over_the_client()
    }

    /// Ask Valve's client to shut down, if it is one this session started.
    ///
    /// Nothing comes back and nothing waits on it — see
    /// [`lxb_steam::Steam::close_the_client`], which is where the rule about
    /// whose client may be ended lives. Never called on a session that is not
    /// driving the client: [`Self::settled`] has no worker, so the request goes
    /// nowhere, but a caller that reached here at all would be a caller acting
    /// on a Steam that is not its business.
    ///
    /// **And the machine is watched from here**, which is the half that was
    /// missing. Nothing comes back from the request and `-shutdown` returns
    /// when the client has been *asked*, so the only account of whether it went
    /// is [`Self::client_is_running`] — which is asked every ten seconds. A
    /// client that shut down at once was invisibly still running for the whole
    /// of that, and everything deciding what to do about it decided on it.
    pub fn close_the_client(&mut self) {
        // Written down before anything else: the answer names this ask, and an
        // answer to the one before it is not about this client any more. See
        // [`Self::closing`].
        self.closing = Some(self.client.close_the_client());
        self.watching_the_client_go = Some(Instant::now());
        // This ask's answer has not arrived; the last one's is not about it.
        self.how_the_close_went = None;
    }

    /// What became of the last client this session asked to shut down.
    ///
    /// `None` while an ask is in flight, and from a session that has never
    /// asked. What it is *for* is the one answer that ends the question rather
    /// than continuing it — [`lxb_steam::client::Closing::NotOurs`], a client
    /// that was never asked because it is not this session's, and so a client
    /// no amount of waiting will see go.
    pub fn how_the_close_went(&self) -> Option<lxb_steam::client::Closing> {
        self.how_the_close_went
    }

    /// Whether the client is idle enough to be shut down under it.
    ///
    /// Three separate questions and all three have to be no, because each is a
    /// different way of losing somebody's work: a download this session started
    /// and is watching, a game on its way off the disk, and anything the
    /// library says Steam has in hand — which is the one that catches a job
    /// somebody began in Steam's own window before this shell was ever asked
    /// about it.
    ///
    /// **The last of those is [`Standing::moving`], and it used to be
    /// [`Standing::arriving`]**: this delegated to [`Self::downloading`], which
    /// is the guide's card and is right to draw only a real download. The two
    /// questions are not the same one and the doc here already said so.
    /// Measured on a real library, and every row of it is work this shell would
    /// have shut a client down in the middle of:
    ///
    /// ```text
    /// Queued        waiting behind another download
    /// Downloading
    /// Updating
    /// Validating    a 139 GB verify
    /// Uninstalling  begun in Steam's own window
    /// ```
    ///
    /// Only the middle two were counted. `removing` above is this session's own
    /// removals and nothing else, so a game somebody started deleting in
    /// Steam's window was a client shut down with a half-removed game on the
    /// disk.
    ///
    /// And the quiet stretch, for **every** app rather than for the pressed
    /// one. [`Self::still_working_on`] covered the press, and nothing covered
    /// the rest: a client could be shut down five seconds into a silent Proton
    /// update, which the manifest says nothing whatever about. See
    /// [`Self::quietly_working_on_it`].
    ///
    /// It is deliberately not asked about the *account*. A client signed in to
    /// somebody else is not stopped either, and that is decided where it has to
    /// be — in the worker, against the evidence on the disk. See
    /// [`lxb_steam::client::stop_if_ours`].
    ///
    /// **And the client's own log, because the manifests do not say either.**
    /// A file check moves no bytes, so nothing is written to
    /// `appmanifest_<id>.acf` for the whole of one: measured here, thirty-six
    /// seconds of `Verifying Installed` on a 15 GB game with `StateFlags 4` on
    /// the disk throughout — including for a check *this shell had just asked
    /// for*, off its own menu. See [`lxb_steam::Steam::work_in_flight`].
    ///
    /// [`Standing::moving`]: lxb_steam::library::Standing::moving
    /// [`Standing::arriving`]: lxb_steam::library::Standing::arriving
    pub fn nothing_is_under_way(&self) -> bool {
        self.fetching.is_empty()
            && self.removing.is_empty()
            // Anything the client has in hand, of any kind and for any app —
            // a Proton being updated is not in this library and is work, and a
            // shader cache is none of a row's business and is work. See
            // [`lxb_steam::client::Jobs::anything_in_hand`].
            && !self.valve_is_busy
            && !self
                .games
                .iter()
                .any(|game| game.standing.moving() || self.quietly_working_on(game).is_some())
    }

    /// Whether Valve's client has this game in hand at all, whatever the row
    /// is calling it.
    ///
    /// [`Self::quietly_working_on`] asks the same map and then throws the
    /// answer away for anything that is not [`Standing::Ready`], because what
    /// it is deciding is a *word for a row* and a copy being repaired must go
    /// on saying it needs repairing. This is not a word for anything: it is
    /// whether there is work between a press and a game, which is a question
    /// with no row in it.
    ///
    /// All three tracks, for the same reason the client-shutdown rule counts
    /// all three — a shader cache is none of a row's business and is still
    /// minutes between a press and a window.
    pub fn the_client_has_it_in_hand(&self, app_id: u32) -> bool {
        self.driving
            && self.client_running
            && (self.valve_is_doing.contains_key(&app_id)
                || self.valve_is_fetching_shaders.contains(&app_id))
    }

    /// Whether any game on the disk is carrying a job that never finished.
    ///
    /// **Not the same question as [`Self::nothing_is_under_way`]**, and the
    /// difference is the whole point. That one asks what a client is doing
    /// *now*; this asks what one would pick up if it were left alone for
    /// another minute. A job Steam has suspended is neither running nor
    /// forgotten, and nothing in this shell could see one.
    ///
    /// Reported from use 2026-09-04. Street Fighter 6 had a 1.4 GB job sitting
    /// at two per cent. Steam starts that job when the game is launched,
    /// suspends it because the game is running, and picks it up again once the
    /// game ends — measured on this machine at **fourteen and thirty-nine
    /// seconds** after the window went. This shell closes the client **five**
    /// seconds after a game. So every session started that job and killed it;
    /// it had moved two per cent since the 13th of August, and nothing
    /// anywhere said so. See [`lxb_steam::library::Game::work_outstanding`],
    /// which carries the evidence for reading it off the manifest at all.
    pub fn work_is_outstanding(&self) -> bool {
        self.games.iter().any(|game| game.work_outstanding() > 0)
    }

    /// Watch the launch that has just been handed over, and say if the client
    /// stops to ask something about it.
    pub fn watch_this_launch(&mut self, app_id: u32) {
        let request = self.client.watch_this_launch(app_id);
        self.watched_launches.insert(app_id, request);
    }

    /// Stop watching one game's launch, and only that one.
    ///
    /// Named rather than session-wide, because the other display may still be
    /// waiting on a game of its own: a press that ends is one launch ending,
    /// not every launch ending.
    pub fn stop_watching_the_launch(&mut self, app_id: u32) {
        // Forgotten here as well as told to the worker, and the order does not
        // matter because neither alone is enough: the thread may already have
        // decided to speak, and what it says arrives at a shell that is no
        // longer listening for it.
        self.watched_launches.remove(&app_id);
        self.client.stop_watching_the_launch(app_id);
    }

    /// And stop watching all of them at once, for the two things that end
    /// every launch there is: the account signing out, and the integration
    /// being turned off under the whole session.
    pub fn stop_watching_every_launch(&mut self) {
        self.watched_launches.clear();
        self.client.stop_watching_every_launch();
    }

    /// Carry back what somebody chose on the shell's own panel.
    /// Tell Valve's client to start the game with the shaders it has, which is
    /// the one press the loading screen offers while it is compiling them.
    ///
    /// Valve's own word for it — see [`lxb_steam::webui::SKIP_SHADERS`] — sent
    /// against the action the client is holding open. It is the button on the
    /// client's own dialog, pressed from a screen a controller can reach.
    pub fn skip_the_shaders(&mut self, action_id: u32) {
        tracing::info!(action_id, "letting the game start without its shaders");
        self.answer_the_launch(
            action_id,
            lxb_steam::webui::Carry::Go(lxb_steam::webui::SKIP_SHADERS.to_string()),
        );
    }

    pub fn answer_the_launch(&mut self, action_id: u32, carry: lxb_steam::webui::Carry) {
        self.client.answer_the_launch(action_id, carry);
    }

    /// Ask the running client to start one game.
    ///
    /// **`steam://launch/<app>/dialog`**, and the word on the end is the whole
    /// of how this shell reaches a game that can be started more than one way.
    /// A game with two launch options — OpenFront's plain one and its
    /// "Wayland workaround" — is a game where the client picks for itself
    /// unless it is told to ask, and the one it picks is not always the one
    /// that runs. `dialog` tells it to ask, the launch stops on
    /// `ShowLaunchOption`, the watch sees it and the shell puts the question up
    /// itself. See [`lxb_steam::webui::CHOOSING_HOW_TO_START`].
    ///
    /// **It costs nothing where there is nothing to ask.** Measured against a
    /// live client on 2026-09-19: an app the client offers exactly one option
    /// for (`228980`) went `CheckShaderDepotManifest` → … → `CreatingProcess`
    /// with no question at all. The client does the gating, so this shell does
    /// not have to count the options first — which would mean a round trip to
    /// the client's interface on the thread that draws, for every press.
    ///
    /// It replaced `steam://rungameid/<app>`, which was chosen because it is
    /// the one Steam registers for the whole of its own library — a title, a
    /// non-Steam shortcut, a tool. Nothing is lost: both arrive at the same
    /// `ExecuteSteamURL`, and what reaches here is always a plain app id,
    /// because that is what [`Self::play`] takes and what the library holds.
    ///
    /// Nothing comes back. What says the game started is the game's own window
    /// arriving on the display, which is what the splash is already watching
    /// for; there is no answer from Steam to wait on and none to be had.
    ///
    /// Straight from this thread, unlike every other request to the client:
    /// this one is only ever reached with the client already up and signed in —
    /// the splash has just spent as long as it took waiting for exactly that —
    /// so what runs here is a courier that exits in milliseconds, and its
    /// answer is what decides whether the splash carries on or the press is
    /// refused.
    ///
    /// **Proved again here, and never started.** The wake this is reached from
    /// proved the client it brought up, and then answered — and between that
    /// answer and this line the client can die, be shut down by somebody, or be
    /// replaced by one another account started. It used to go through
    /// `lxb_steam::client::open`, which *starts* a client where there is none:
    /// a game delivered to a Steam launched from cold, which signs itself into
    /// whichever account it last remembered and plays out of that library, with
    /// no check made of either half. So the two questions are asked again, in
    /// the instant before the URL goes, and a client that is not this session's
    /// and this account's is a press that says so — see
    /// [`lxb_steam::client::prove_and_deliver`].
    ///
    /// The account is asked for by its number rather than by a credential,
    /// which is the whole reason this can be done from here: proving whose a
    /// client is needs what the client says out loud, and nothing secret.
    pub fn play(&mut self, app_id: u32) -> Result<(), String> {
        self.hand_over_a_launch(app_id, &format!("steam://launch/{app_id}/dialog"))
    }

    /// Ask the running client to start one game **and join what somebody
    /// invited this account to**.
    ///
    /// The same delivery as [`Self::play`] down to the last check — the same
    /// proof of whose client it is, the same courier, the same answer — and a
    /// different URL, because what is being asked for is different. A game
    /// started and then joined is two acts with a menu between them; this is
    /// the one act Valve's own client performs when somebody presses Join on an
    /// invitation.
    ///
    /// **`steam://joinlobby/<app>/<lobby>/<who>` where there is a lobby.** That
    /// is what an invitation from Steam's own matchmaking is — the connect
    /// string is `+connect_lobby <id>` — and it is the form the client knows
    /// how to finish in either state the machine can be in: it starts the game
    /// with `+connect_lobby <id>` where the game is not running, and tells the
    /// running one to join where it is. Nothing else expresses the second half.
    ///
    /// **`steam://rungameid/<app>//<arguments>` for anything else.** A game may
    /// define its own connect string — a server address, a match id — and there
    /// is no joining a thing Steam does not model. Started with it on the
    /// command line is exactly what Valve's client does with those, and the
    /// separator is `+` because a URL cannot carry spaces. `steam://run/…` is
    /// deliberately not used: it is documented to take arguments and
    /// [drops them on Linux](https://github.com/ValveSoftware/steam-for-linux/issues/12264).
    ///
    /// The dialog that `play` asks for — the one that makes a game with two
    /// launch options ask which — is **not** asked for here, and cannot be:
    /// neither URL takes it. A game that stops to ask is a game that will ask
    /// in its own window, and the invitation's connect string travels either
    /// way.
    pub fn join(&mut self, app_id: u32, invite: &lxb_steam::chat::Invite) -> Result<(), String> {
        self.hand_over_a_launch(app_id, &joining_url(app_id, invite))
    }

    /// Prove the client and give it one `steam:` URL — the last few inches of
    /// both [`Self::play`] and [`Self::join`].
    ///
    /// One function because every word of the reasoning above is about the
    /// *delivery* rather than about which URL is delivered, and two copies of
    /// it would be two places for the proof to be dropped from.
    fn hand_over_a_launch(&mut self, app_id: u32, url: &str) -> Result<(), String> {
        let Some(steam_id) = self.steam_id else {
            return Err(
                crate::i18n::text("shell-this-session-is-not-signed-in-to-steam").to_string(),
            );
        };
        let Some(where_it_is) = lxb_steam::client::Where::find() else {
            return Err(crate::i18n::text(
                "shell-there-is-no-steam-client-installed-on-this-machine",
            )
            .to_string());
        };
        let Some(options) = lxb_steam::client::Options::for_client(&where_it_is) else {
            return Err(crate::i18n::text(
                "shell-steam-s-directories-could-not-be-found-on-this-machine",
            )
            .to_string());
        };
        let proven = lxb_steam::client::prove_and_deliver(
            &where_it_is,
            &options,
            steam_id as u32,
            url,
        )
        .map_err(
            |refusal| crate::message!("steam-refused-launch", "refusal" => refusal.to_string()),
        )?;
        tracing::info!(
            app_id,
            client = ?proven.pid,
            // The URL and not just the app: which of the two this was is the
            // first thing worth knowing when a game starts without joining
            // anything. It carries a lobby id, which is not a secret — it is
            // what was broadcast to be joined — and never an account's.
            url,
            "the game was handed to the client this session proved"
        );
        Ok(())
    }

    /// Stop the launch Valve's client is walking for one game.
    ///
    /// For a press that has stopped waiting on it — see
    /// `Shell::stop_waiting_for_a_download`. Says nothing back.
    pub fn stop_launch(&self, app_id: u32) {
        self.client.stop_launch(app_id);
    }

    /// Have Valve's client fetch a game the account owns and has not got, into
    /// the library `place` says — see [`lxb_steam::webui::Place`].
    ///
    /// Should the game have an agreement, it is read in the language the shell
    /// is speaking, where the publisher wrote one.
    pub fn install(&mut self, app_id: u32, place: lxb_steam::webui::Place) {
        self.places.insert(app_id, place.clone());
        self.client
            .install(app_id, place, crate::i18n::spoken().steam_name());
    }

    /// Record that the person has accepted these agreements, and fetch the
    /// game. Only ever called from the Accept button on the panel that showed
    /// them — see `Shell::accept_the_agreement`.
    ///
    /// Into the library the press before it was sent to, so a game whose
    /// library was chosen first and whose agreement was asked second goes
    /// where it was sent. A game with no press this session behind it goes
    /// where the settings say, as a press would.
    pub fn accept_and_install(&mut self, app_id: u32, accepting: Vec<lxb_steam::webui::Eula>) {
        let place = self
            .places
            .get(&app_id)
            .cloned()
            .unwrap_or_else(place_from_the_settings);
        self.client.accept_and_install(
            app_id,
            accepting,
            place,
            crate::i18n::spoken().steam_name(),
        );
    }

    /// The library the last press on this game was sent to, or asked about.
    pub fn place(&self, app_id: u32) -> Option<lxb_steam::webui::Place> {
        self.places.get(&app_id).cloned()
    }

    /// Make this library Steam's own default, if Valve's client is up — see
    /// [`lxb_steam::Steam::make_default_library`].
    pub fn make_default_library(&mut self, path: &str) {
        self.client.make_default_library(path.to_string());
    }

    pub fn stop_installing(&mut self, app_id: u32) {
        self.client.stop_installing(app_id);
    }

    /// Move one installed game into another library, by the path Steam lists
    /// it under — Settings > Games > Steam > Storage. See
    /// [`lxb_steam::Steam::move_game`].
    ///
    /// Remembered here from the press, before Steam has said anything, so the
    /// row and the panel can say it is moving from the moment it was asked.
    pub fn move_game(&mut self, app_id: u32, to: String) {
        self.moving = Some(Relocation {
            app_id,
            to: to.clone(),
            percent: None,
            stopping: false,
        });
        self.client.move_game(app_id, to);
    }

    /// Stop the move that is under way. What says it has stopped is the move
    /// ending, which the row and the panel wait for.
    pub fn cancel_move(&mut self) {
        if let Some(moving) = self.moving.as_mut() {
            moving.stopping = true;
        }
        self.client.cancel_move();
    }

    /// The game being moved between libraries, while one is.
    pub fn moving(&self) -> Option<&Relocation> {
        self.moving.as_ref()
    }

    /// Add, remove or repair one of Steam's libraries — see
    /// [`lxb_steam::Steam::shelve`].
    pub fn shelve(&mut self, job: lxb_steam::webui::Shelving) {
        self.client.shelve(job);
    }

    /// Take one game off the disk.
    ///
    /// Nothing of Steam's appears for this. Whoever calls it has already asked
    /// the user, because this is the point past which the game is gone — see
    /// the Uninstall row of the game menu.
    pub fn uninstall(&mut self, app_id: u32) {
        self.client.uninstall(app_id);
    }

    /// What Steam last said about which compatibility tool runs this, if it has
    /// been asked. See [`Compat`].
    pub fn compatibility(&self, which: lxb_steam::webui::Which) -> Option<&Compat> {
        self.compat.get(&which)
    }

    /// Ask Steam what it offers for this, unless it has already answered.
    ///
    /// Asked once per title per session. The list is Valve's knowledge of what
    /// this account may run a game with rather than a fact about this machine,
    /// so the only thing that moves it is somebody installing a compatibility
    /// tool — and paying two round trips into the client every time a menu
    /// opens, to be told the same eleven names, is what a menu that waits is
    /// made of. A failure is *not* remembered that way: the client that could
    /// not be reached a minute ago is very often up now, so a press on the row
    /// that said so tries again.
    pub fn ask_about_compatibility(&mut self, which: lxb_steam::webui::Which) {
        if matches!(
            self.compat.get(&which),
            Some(Compat::Asking | Compat::Said(_))
        ) {
            return;
        }
        self.compat.insert(which, Compat::Asking);
        self.client.compatibility(which);
    }

    /// Ask how one game can be started, unless it has already been answered.
    ///
    /// Asked once per game per session, on the terms
    /// [`Self::ask_about_compatibility`] keeps and for the same reason: the
    /// list moves only when the game itself is updated, and a menu that paid a
    /// round trip into the client every time it opened is a menu that waits.
    pub fn ask_about_the_ways(&mut self, app_id: u32) {
        if matches!(self.ways.get(&app_id), Some(Ways::Asking | Ways::Said(_))) {
            return;
        }
        self.ways.insert(app_id, Ways::Asking);
        self.client.ask_about_the_ways(app_id);
    }

    /// What Valve's client has said about how one game can be started.
    pub fn ways(&self, app_id: u32) -> Option<&Ways> {
        self.ways.get(&app_id)
    }

    /// Run it under this tool from now on, or under whatever Steam chooses.
    ///
    /// The tick moves at once and is then corrected by what comes back. Both
    /// halves are needed: the press has to answer immediately — waking a client
    /// can be most of a minute — and what Steam ended up with is the only thing
    /// worth drawing, so the answer overwrites this rather than confirming it.
    pub fn run_under(&mut self, which: lxb_steam::webui::Which, tool: Option<String>) {
        if let Some(Compat::Said(said)) = self.compat.get_mut(&which) {
            said.forced = tool.clone();
        }
        self.forcing.insert(which);
        self.client.force_compatibility(which, tool);
    }

    /// How far one game's download has got, if it is being fetched.
    pub fn fetching(&self, app_id: u32) -> Option<Fetching> {
        self.fetching.get(&app_id).copied()
    }

    /// What is worth knowing before agreeing to a download.
    ///
    /// The confirmation used to say only that the game was absent and "can be
    /// fetched", which is the one thing the person pressing already knew. What
    /// they cannot see is whether there is room for it and whether it will be
    /// queued behind something — and both of those are answerable from this
    /// machine, without asking Steam anything.
    ///
    /// Read on the press rather than kept: it is a `PATH` walk and one
    /// `statvfs` per Steam library, which is nothing once, and a number
    /// remembered from an hour ago is a number that has been spent since.
    pub fn preflight(&self) -> Preflight {
        Preflight {
            room: lxb_steam::backend::Backend::chosen()
                .map(|backend| lxb_steam::library::room_in_each(&backend))
                .unwrap_or_default(),
            // Read from the library rather than from this session's own
            // fetching list on purpose: a download somebody started in Steam
            // itself an hour ago will queue this one just the same, and the
            // list of what *this shell* asked for cannot see it.
            already: self
                .games
                .iter()
                .find(|game| game.standing.moving() && game.standing.is_a_download())
                .map(|game| game.name.clone()),
        }
    }

    /// How many games this session has asked Valve's client to fetch and is
    /// still watching come down.
    ///
    /// For the one panel that has to say what a press costs: signing out ends
    /// this session's account, and these are what it takes with it.
    pub fn downloads_under_way(&self) -> usize {
        self.fetching.len()
    }

    pub fn is_fetching(&self, app_id: u32) -> bool {
        self.fetching.contains_key(&app_id)
    }

    /// Whether one game is on its way off the disk right now.
    pub fn is_removing(&self, app_id: u32) -> bool {
        self.removing.contains(&app_id)
    }

    /// One game out of the library, by its app id.
    pub fn game(&self, app_id: u32) -> Option<&Game> {
        self.games.iter().find(|game| game.app_id == app_id)
    }

    /// How far Valve's client has got with this game, where the numbers on the
    /// disk are a *reading* rather than a leftover.
    ///
    /// **The manifest counts every job and admits to almost none of them.**
    /// Measured on this machine on 2026-09-04, watching a real one second by
    /// second: Street Fighter 6 fetched 190 MB of shader cache with
    /// `StateFlags` reading **4** — Fully Installed, nothing else — from the
    /// first byte to the last, while `BytesDownloaded` climbed to
    /// `BytesToDownload` underneath it. The same is true of a repair: a copy
    /// with `Files Corrupt` set is [`Standing::Broken`], which is not one of
    /// the standings [`lxb_steam::library::Standing::counting_bytes`] will
    /// read a pair for either. So both of the things somebody watches a
    /// percentage for had one on the disk, and nothing in the shell would read
    /// it.
    ///
    /// That refusal is right on its own terms and this is not an exception to
    /// it. For a game that is simply installed the pair is **the last job's
    /// leftovers**, and a bar under an idle game is an invented reading. What
    /// makes it live is that Valve's client has the game in hand *now* — which
    /// is a fact from the client's own log rather than from the file, and is
    /// the one thing that tells a job in flight from the ghost of one that
    /// ended. See [`Self::the_client_has_it_in_hand`].
    ///
    /// The manifest's own answer first, for the states it does describe: two
    /// sources agreeing is one source, and where they differ the standing is
    /// the more particular.
    pub fn how_far_along(&self, app_id: u32) -> Option<f32> {
        let game = self.game(app_id)?;
        game.fraction().or_else(|| {
            if !self.the_job_counts_bytes(app_id) {
                return None;
            }
            // And only while the pair still describes work left to do. A
            // complete pair is either the *last* job's — every finished game on
            // this disk carries one — or this job's final line, and either way
            // a full bar under work in flight has stopped meaning anything.
            // The window is real rather than theoretical: measured on
            // 2026-09-04, a shader job's first seconds read `0/1448374368`
            // before the client rewrote the pair for the job it had actually
            // decided on.
            (game.downloaded < game.to_download)
                .then(|| lxb_steam::library::fraction(game.downloaded, game.to_download))
                .flatten()
        })
    }

    /// Whether the job the client has this game in hand for is one that is
    /// measured in downloaded bytes at all.
    ///
    /// **A check is not**, and this is the trap that was caught on screen. A
    /// validate reads the disk back; the pair in the manifest is whatever the
    /// last download left there. Measured on 2026-09-04: a validate of
    /// Progressbar95, one second in, over a manifest reading
    /// `261296/261296` — which without this draws a **full** bar under a check
    /// that has barely started. It is the same reasoning
    /// [`lxb_steam::library::Standing::counting_bytes`] leaves `Validating` out
    /// for, asked of the client's log instead of the file.
    ///
    /// Read straight off the same two collections the card is built from, so
    /// the card and the bar on it cannot be drawn from different answers.
    fn the_job_counts_bytes(&self, app_id: u32) -> bool {
        match self.valve_is_doing.get(&app_id) {
            Some(lxb_steam::client::InHand::Checking) => false,
            Some(lxb_steam::client::InHand::Working) => true,
            // A shader cache is a download like any other; it is only the row
            // it must stay off. See [`lxb_steam::client::Track`].
            None => self.valve_is_fetching_shaders.contains(&app_id),
        }
    }

    /// And the same, as a whole number of per cent, for the lines that say it
    /// in words rather than draw it.
    pub fn how_far_along_percent(&self, app_id: u32) -> Option<u32> {
        self.how_far_along(app_id)
            .map(|share| (share * 100.0).round().clamp(0.0, 100.0) as u32)
    }

    /// Work Valve's client has in hand that no manifest describes and no press
    /// is standing in front of.
    ///
    /// **The third source for the card, and the reported gap.** A shader cache
    /// is fetched for a game that is on the disk and plays perfectly, so the
    /// manifest never picks up a working bit and the row is right to go on
    /// saying Installed — and until now that meant a 1.4 GB download existed
    /// nowhere in this shell at all. Reported on 2026-09-04: *"Street Fighter
    /// 6 update pending that is simply being ignored by the Shell."* It was:
    /// the only place it was ever said was under a press, and that press was
    /// over in fourteen seconds.
    ///
    /// The row still does not say it — see [`lxb_steam::client::Track`], and
    /// the game really is installed and really does start. The **card** says
    /// it, which is what the card is for: it is the corner that answers "what
    /// is coming down", about the machine rather than about a press.
    ///
    /// Last of the three, so a real download of a game's own content always
    /// wins the one card there is.
    pub fn what_the_client_is_getting_on_with(&self) -> Option<Coming> {
        let app_id = *self
            .valve_is_doing
            .keys()
            .chain(self.valve_is_fetching_shaders.iter())
            .find(|app_id| {
                // Only where nothing else would have said it. A game whose
                // manifest is moving is the card above's, and drawing it here
                // as well would be one moment with two cards.
                self.game(**app_id)
                    .is_some_and(|game| !game.standing.moving())
            })?;
        Some(Coming {
            whose: Whose::Steam(app_id),
            name: self.named(app_id),
            verb: match self.valve_is_doing.get(&app_id) {
                Some(lxb_steam::client::InHand::Checking) => CHECKING,
                Some(lxb_steam::client::InHand::Working) => UPDATING,
                // A shader cache, which is neither the game arriving nor the
                // game being changed. Steam's own housekeeping over a copy that
                // would play right now.
                None => PREPARING,
            },
            share: self.how_far_along(app_id),
            stuck: false,
            // Never a download in the sense the finish is keyed on: the game
            // was playable throughout, so announcing this as a download that
            // had finished would announce one that never happened. See
            // [`Coming::a_download`].
            a_download: false,
        })
    }

    /// The one download worth putting a card up for, or nothing.
    ///
    /// **One**, because Valve's client runs one at a time — the rest of a queue
    /// is waiting, not arriving — and because the card is a corner of the guide
    /// rather than a list. Where the disk somehow shows several, the one this
    /// session is watching wins and the lowest app id breaks the tie, so the
    /// card cannot swap between two games from frame to frame.
    ///
    /// Both halves of the library are asked, in the order [`Self::row`] asks
    /// them: a job this session started first, because it has the client's own
    /// account of how far it has got and answers before the manifest exists at
    /// all, and then the disk, so a download somebody began in Steam's own
    /// window an hour ago is on the card too. What the card says is therefore
    /// what the row says, which is the point — they are one fact.
    pub fn downloading(&self) -> Option<Coming> {
        let mine = self
            .fetching
            .iter()
            .find(|(app_id, _)| !self.removing.contains(app_id))
            .map(|(app_id, so_far)| Coming {
                whose: Whose::Steam(*app_id),
                name: self.named(*app_id),
                verb: self.verb_for(*app_id),
                share: so_far.fraction(),
                stuck: so_far.stuck,
                a_download: true,
            });
        mine.or_else(|| {
            self.games
                .iter()
                // Being done, and not merely written down as begun. The card is
                // there to say a download is happening on this machine; one
                // drawn over a manifest no client is acting on would be a
                // frozen bar in the corner of the guide for the whole of a
                // session. See [`Steam::waiting_for_steam`].
                .find(|game| {
                    game.standing.arriving()
                        && !self.waiting_for_steam(game)
                        && !self.removing.contains(&game.app_id)
                })
                .map(|game| Coming {
                    whose: Whose::Steam(game.app_id),
                    name: game.name.clone(),
                    verb: verb_for(game.standing),
                    share: game.fraction(),
                    // Only a download this session is watching has a clock on
                    // it — the same rule the row is under.
                    stuck: false,
                    a_download: true,
                })
        })
    }

    /// What Steam is doing for a game somebody has pressed and is standing in
    /// front of, for the card in the corner of the guide.
    ///
    /// **The second kind of card, and it is not a download.** The one above is
    /// bytes of the game arriving, which is a manifest saying so; this is the
    /// work that happens where no manifest says anything — an update Steam has
    /// begun and not admitted to, a check it is running over the disk, and a
    /// shader cache it fetches before it will open the window. All three are
    /// minutes of a press that looks like nothing at all.
    ///
    /// Reported off this machine on 2026-09-03. A press on Counter-Strike 2
    /// woke the client, and what the client did with it was fetch five
    /// gigabytes of **shader cache** while the game's own update waited behind
    /// it. Nothing in the shell said so: the row read "Installed", the guide's
    /// corner was empty, and the only thing on the screen was a loading screen
    /// counting down to a panel about a game that had not started.
    ///
    /// **The words are the loading screen's own**, which is the whole reason
    /// this is asked of [`Self::quietly_working_on_it`] rather than of the
    /// library: the card and the splash are two places drawing one moment, and
    /// two words for it would be two answers to one question.
    ///
    /// No percentage. What is on the disk to count with is the *previous*
    /// download's — see [`lxb_steam::library::Standing::counting_bytes`] — and
    /// a shader job is not counted in the game's manifest at all. The card
    /// draws its groove empty, which is what it already does for a download
    /// the client has not yet said anything about.
    pub fn what_a_press_is_waiting_on(&self, app_id: u32) -> Option<Coming> {
        use lxb_steam::library::Standing;

        let verb = match self.quietly_working_on_it(app_id) {
            Some(Standing::Validating) => CHECKING,
            Some(_) => verb_for(Standing::Updating),
            // Nothing about the game's own content, and a shader cache under a
            // press is still an answer to "why is this taking so long". It is
            // never an answer anywhere else — see
            // [`lxb_steam::client::Jobs::fetching_shaders_for`].
            None => {
                // The same two conditions the answer above is under, asked
                // again because this reads a different fact: a session that is
                // not driving Valve's client has no business reporting the
                // machine it happens to be running on, and a client that has
                // gone is not fetching anything.
                let fetching = self.driving
                    && self.client_running
                    && self.valve_is_fetching_shaders.contains(&app_id);
                match fetching {
                    true => PREPARING,
                    false => return None,
                }
            }
        };
        Some(Coming {
            whose: Whose::Steam(app_id),
            name: self.named(app_id),
            verb,
            share: None,
            stuck: false,
            a_download: false,
        })
    }

    /// And the card for a step of a launch, which is the client's own account
    /// of the press rather than a reading off the disk.
    ///
    /// The better half of [`Self::what_a_press_is_waiting_on`] wherever it
    /// answers, and it answers for work no manifest describes at all. The
    /// words are the loading screen's own — see [`crate::launch::words_for`] —
    /// for the same reason the words above are: the card and the splash are two
    /// places drawing one moment, and two words for it would be two answers to
    /// one question.
    ///
    /// `verb` is `None` for the game's own content arriving, which is the one
    /// step the card already has a word for and picks between Downloading and
    /// Updating by what is on the disk.
    ///
    /// **This one may carry a percentage**, where the two below cannot. Valve's
    /// client counts these steps itself — "Downloading content (19%)" — and
    /// hands the numbers over with the launch; the manifest's byte counts in
    /// the same moment are the *previous* download's.
    pub fn what_a_step_is_waiting_on(
        &self,
        app_id: u32,
        verb: Option<&'static str>,
        share: Option<f32>,
    ) -> Coming {
        Coming {
            whose: Whose::Steam(app_id),
            name: self.named(app_id),
            // Whether the game is on the disk, rather than what the manifest is
            // calling it — which under a launch may be neither of the two
            // standings [`verb_for`] distinguishes. The copy being repaired
            // that this was written for read `Broken`, and "Downloading
            // Counter-Strike 2" over seventy-one gigabytes already on the
            // machine is a card about the wrong thing.
            verb: verb.unwrap_or(
                match self
                    .game(app_id)
                    .is_some_and(|game| game.standing.on_the_disk())
                {
                    true => UPDATING,
                    false => DOWNLOADING,
                },
            ),
            share,
            stuck: false,
            // Never a download in the sense the finish is keyed on, even where
            // the bytes are the game's: what this is about is a *press*, and it
            // ends when the launch does rather than when the manifest goes
            // quiet. See [`Coming::a_download`].
            a_download: false,
        }
    }

    /// What that game is called, or something that can be read where the
    /// catalogue has not arrived yet.
    fn named(&self, app_id: u32) -> String {
        self.game(app_id)
            .map(|game| game.name.clone())
            .unwrap_or_else(|| crate::message!("steam-app-number", "app" => app_id))
    }

    /// Which word goes in front of the name, for a download this session
    /// started.
    ///
    /// The disk's own standing where it has one, so a press that lands on a
    /// game already on the machine says Updating from the first frame.
    fn verb_for(&self, app_id: u32) -> &'static str {
        self.game(app_id)
            .map_or(DOWNLOADING, |game| verb_for(game.standing))
    }

    /// Where Steam publishes that game's pictures, as the library was told.
    ///
    /// The library is the only thing that knows: the paths arrive in the same
    /// record as the game's name, and [`crate::art`] has no catalogue to look
    /// them up in. Nothing for a game the account does not own — one on the disk
    /// from somebody else's library — which is asked for by name instead.
    pub fn pictures(&self, app_id: u32) -> Option<&lxb_steam::art::Published> {
        self.game(app_id).map(|game| &game.pictures)
    }

    /// Sign in by photographing a code.
    pub fn with_qr(&mut self) {
        self.signing_in = Some(Stage::Qr(None));
        self.client.sign_in_with_qr();
    }

    /// Sign in with an account name and a password, starting with the name.
    pub fn with_password(&mut self) {
        self.signing_in = Some(Stage::Account(String::new()));
    }

    /// Hand over whatever the panel is asking for.
    ///
    /// One command for every field of this panel rather than one each, which
    /// is a departure from how the two other password fields in this shell are
    /// answered — those are deliberately separate commands, because one hands
    /// its password to `sudo` and the other to PAM, and a single name for both
    /// would be one mis-routed press away from answering the wrong question.
    /// Here there is one destination: the sign-in that raised the panel. The
    /// stage that is up *is* which question is being answered, and there is no
    /// second place for an answer to go.
    pub fn submit(&mut self) {
        let Some(stage) = self.signing_in.take() else {
            return;
        };
        match stage {
            Stage::Account(account) if !account.trim().is_empty() => {
                self.signing_in = Some(Stage::Password {
                    account: account.trim().to_string(),
                    secret: Secret::default(),
                });
            }
            // An empty account name is not an answer, so the panel stays where
            // it is rather than sending Steam a sign-in for nobody.
            Stage::Account(account) => self.signing_in = Some(Stage::Account(account)),
            Stage::Password { account, secret } => {
                let mut password = lxb_steam::Password::default();
                // The one way out of a `Secret`, and the only copy: what the
                // sink holds is overwritten when the client has encrypted it.
                if secret.hand_to(&mut password).is_err() {
                    self.signing_in = Some(Stage::Failed(
                        crate::i18n::text("shell-that-password-could-not-be-handed-over")
                            .to_string(),
                    ));
                    return;
                }
                self.signing_in = Some(Stage::Waiting(
                    crate::i18n::text("shell-signing-in").to_string(),
                ));
                self.client.sign_in_with_password(account, password);
            }
            Stage::Code {
                confirmation,
                typed,
            } if !typed.trim().is_empty() => {
                self.client.submit_code(typed.trim().to_string());
                self.signing_in = Some(Stage::Waiting(confirmation.asked()));
            }
            other => self.signing_in = Some(other),
        }
    }

    /// Apply one keystroke to whichever field this panel has up.
    pub fn type_into(&mut self, stroke: crate::keyboard::Stroke) -> Typed {
        let Some(stage) = self.signing_in.as_mut() else {
            return Typed::Elsewhere;
        };
        // A field of one line has no use for Tab, the arrows or the function
        // keys, and letting them past to the bar underneath would move the
        // cursor off the row the panel came out of.
        let into = |text: &mut String| match stroke {
            crate::keyboard::Stroke::Char(character) => {
                text.push(character);
                Typed::Into
            }
            crate::keyboard::Stroke::BACKSPACE => {
                text.pop();
                Typed::Into
            }
            crate::keyboard::Stroke::ENTER => Typed::Done { submitted: true },
            crate::keyboard::Stroke::ESCAPE => Typed::Done { submitted: false },
            _ => Typed::Into,
        };

        match stage {
            Stage::Account(text) => into(text),
            Stage::Code { typed, .. } => into(typed),
            Stage::Password { secret, .. } => match stroke {
                crate::keyboard::Stroke::Char(character) => {
                    secret.push(character);
                    Typed::Into
                }
                crate::keyboard::Stroke::BACKSPACE => {
                    secret.pop();
                    Typed::Into
                }
                crate::keyboard::Stroke::ENTER => Typed::Done { submitted: true },
                crate::keyboard::Stroke::ESCAPE => Typed::Done { submitted: false },
                _ => Typed::Into,
            },
            _ => Typed::Elsewhere,
        }
    }

    /// What the panel says and offers, or `None` when there is no sign-in on
    /// screen.
    ///
    /// Built afresh from the stage every time rather than edited in place, for
    /// the reason the password panel is: what is on screen is a function of
    /// what is being asked and of a *count* of characters, so there is nothing
    /// on the panel to keep in step with anything.
    pub fn panel(&self) -> Option<Panel> {
        let stage = self.signing_in.as_ref()?;
        let heading = dialog::Line::Heading(
            match stage {
                Stage::LibraryUnavailable(_) => crate::i18n::text("shell-steam-library"),
                // Named for what is happening rather than for what it is on
                // the way to. Somebody who pressed "Sign in to Steam" and got a
                // four-minute wait under that heading would reasonably think
                // the wait *was* the sign-in, and that Steam was being slow
                // about their account. It is not their account: it is Steam
                // arriving on the machine.
                Stage::FirstSetup(_) | Stage::SetupFailed(_) => {
                    crate::i18n::text("shell-setting-up-steam")
                }
                _ => crate::i18n::text("shell-sign-in-to-steam"),
            }
            .to_string(),
        );
        let cancel = menu::Entry::new(
            menu::Command::SteamCancel,
            crate::i18n::text("shell-cancel"),
        );

        let panel = match stage {
            Stage::FirstSetup(step) => Panel {
                lines: vec![
                    heading,
                    // Two sentences on two lines, and both of them are answers
                    // to questions somebody watching this would otherwise have
                    // to guess at: why is a sign-in downloading something, and
                    // is this going to happen every time. Neither is
                    // decoration.
                    //
                    // **Two lines because the panel does not wrap.** A
                    // [`dialog::Line::Note`] is drawn as one line and cut with
                    // an ellipsis, so a sentence written as one string and
                    // measured in a mock arrives on screen saying something
                    // else: this pair began as one note and reached a
                    // screenshot reading "Steam has to install itself on this
                    // machine before you can …".
                    dialog::Line::Note(
                        crate::i18n::text(
                            "shell-steam-has-to-install-itself-before-you-can-sign-in",
                        )
                        .to_string(),
                    ),
                    dialog::Line::Note(
                        crate::i18n::text("shell-this-only-happens-once").to_string(),
                    ),
                    dialog::Line::Note(match step.percent {
                        Some(percent) => format!("{}  ·  {percent}%", step.said),
                        None => step.said.clone(),
                    }),
                    // The bar where there is something to count, and the lights
                    // where there is not. The two are the same height, so the
                    // panel does not move under a thumb when the download ends
                    // and the unpack begins. See [`dialog::Line::Progress`].
                    match step.percent {
                        Some(percent) => dialog::Line::Progress(percent),
                        None => dialog::Line::Waiting,
                    },
                    dialog::Line::Rule,
                ],
                // The only way out, and it is not a cancel: half a gigabyte is
                // coming down and stopping it would throw away what had
                // arrived. What this offers is to stop *watching*, which is
                // what Back does on this panel too.
                buttons: vec![menu::Entry::new(
                    menu::Command::SteamSetupInBackground,
                    crate::i18n::text("shell-carry-on-in-the-background"),
                )],
                start: 0,
                typing: false,
            },
            Stage::SetupFailed(why) => Panel {
                lines: vec![heading, dialog::Line::Note(why.clone()), dialog::Line::Rule],
                buttons: vec![
                    menu::Entry::new(
                        menu::Command::SteamSetUpAgain,
                        crate::i18n::text("shell-try-again"),
                    ),
                    menu::Entry::new(menu::Command::SteamCancel, crate::i18n::text("shell-close")),
                ],
                start: 0,
                typing: false,
            },
            Stage::Choosing => Panel {
                lines: vec![
                    heading,
                    dialog::Line::Note(
                        crate::i18n::text(
                            "shell-your-library-appears-on-the-start-screen-as-a-column-of-its-own",
                        )
                        .to_string(),
                    ),
                    dialog::Line::Rule,
                ],
                buttons: vec![
                    menu::Entry::new(
                        menu::Command::SteamWithQr,
                        crate::i18n::text("shell-scan-a-code-with-your-phone"),
                    ),
                    menu::Entry::new(
                        menu::Command::SteamWithPassword,
                        crate::i18n::text("shell-type-an-account-name-and-password"),
                    ),
                    cancel.group(1),
                ],
                // On the code, which is the way in that needs no keyboard —
                // and this shell is driven with a thumb.
                start: 0,
                typing: false,
            },
            Stage::Qr(code) => Panel {
                lines: vec![
                    heading,
                    dialog::Line::Note(match code {
                        Some(_) => {
                            crate::i18n::text("shell-scan-this-in-the-steam-app-on-your-phone")
                                .to_string()
                        }
                        None => crate::i18n::text("shell-asking-steam-for-a-code").to_string(),
                    }),
                    match code {
                        Some(code) => dialog::Line::Qr(code.clone()),
                        // The panel keeps the room the code will take, so it
                        // does not grow under the user's eyes a moment after
                        // it opened.
                        None => dialog::Line::Waiting,
                    },
                    dialog::Line::Rule,
                ],
                buttons: vec![cancel],
                start: 0,
                typing: false,
            },
            Stage::Account(typed) => Panel {
                lines: vec![
                    heading,
                    dialog::Line::Note(
                        crate::i18n::text("shell-enter-your-steam-account-name").to_string(),
                    ),
                    dialog::Line::Entry(typed.clone()),
                    dialog::Line::Rule,
                ],
                buttons: vec![
                    menu::Entry::new(menu::Command::SteamSubmit, crate::i18n::text("shell-next")),
                    cancel,
                ],
                start: 0,
                typing: true,
            },
            Stage::Password { account, secret } => Panel {
                lines: vec![
                    heading,
                    dialog::Line::Note(crate::message!("enter-password-for", "name" => account)),
                    dialog::Line::Secret {
                        typed: secret.typed(),
                    },
                    dialog::Line::Rule,
                ],
                buttons: vec![
                    menu::Entry::new(
                        menu::Command::SteamSubmit,
                        crate::i18n::text("shell-sign-in"),
                    ),
                    cancel,
                ],
                start: 0,
                typing: true,
            },
            Stage::Code {
                confirmation,
                typed,
            } => Panel {
                lines: vec![
                    heading,
                    dialog::Line::Note(confirmation.asked()),
                    dialog::Line::Entry(typed.clone()),
                    dialog::Line::Rule,
                ],
                buttons: vec![
                    menu::Entry::new(
                        menu::Command::SteamSubmit,
                        crate::i18n::text("shell-confirm"),
                    ),
                    cancel,
                ],
                start: 0,
                typing: true,
            },
            Stage::Waiting(note) => Panel {
                lines: vec![
                    heading,
                    dialog::Line::Note(note.clone()),
                    dialog::Line::Waiting,
                    dialog::Line::Rule,
                ],
                buttons: vec![cancel],
                start: 0,
                typing: false,
            },
            Stage::Failed(why) => Panel {
                lines: vec![heading, dialog::Line::Note(why.clone()), dialog::Line::Rule],
                buttons: vec![
                    menu::Entry::new(
                        menu::Command::SteamSignIn,
                        crate::i18n::text("shell-try-again"),
                    ),
                    cancel,
                ],
                // On trying again: the panel is only ever here because
                // somebody was in the middle of signing in.
                start: 0,
                typing: false,
            },
            Stage::LibraryUnavailable(why) => Panel {
                lines: vec![heading, dialog::Line::Note(why.clone()), dialog::Line::Rule],
                buttons: vec![
                    menu::Entry::new(
                        menu::Command::SteamRefresh,
                        crate::i18n::text("shell-try-again"),
                    ),
                    menu::Entry::new(menu::Command::SteamCancel, crate::i18n::text("shell-close")),
                ],
                start: 0,
                typing: false,
            },
        };
        Some(panel)
    }
}

/// How many games a letter of the index holds, as its row says it.
fn counted(games: usize) -> String {
    crate::message!("count-games", "count" => games)
}

/// One frame of the sign-in panel.
pub struct Panel {
    pub lines: Vec<dialog::Line>,
    pub buttons: Vec<menu::Entry>,
    pub start: usize,
    /// Whether the on-screen keyboard belongs over it.
    pub typing: bool,
}

#[cfg(test)]
mod tests {
    #[test]
    fn polish_library_notes_translate_states_without_changing_names() {
        use crate::i18n::{self, Language};
        use lxb_steam::library::{Game, Standing};
        i18n::set(Language::Polish);
        let mut game = Game::invented(12, "Installed".into(), true);
        game.standing = Standing::Ready;
        game.size_on_disk = 1500;
        game.playtime_minutes = 90;
        assert!(super::game_note(&game).contains("Zainstalowane"));
        assert!(super::game_note(&game).contains("1,5 godz."));
        assert_eq!(game.name, "Installed");
        game.standing = Standing::Validating;
        assert_eq!(super::game_note(&game), "Sprawdzanie plików");
        i18n::set(Language::British);
        assert_eq!(super::game_note(&game), game.note());
    }

    use super::*;
    use crate::keyboard::Stroke;
    use std::path::PathBuf;

    fn typing(steam: &mut Steam, text: &str) {
        for character in text.chars() {
            assert_eq!(steam.type_into(Stroke::Char(character)), Typed::Into);
        }
    }

    /// A session part-way through installing Valve's client, without a machine
    /// that has to be installing one.
    ///
    /// [`Steam::begin`] is what puts a real session into this state and it
    /// reads the disk to decide; what is worth testing is everything after
    /// that decision, so the state is set here directly.
    fn setting_up(said: &str, percent: Option<u8>) -> Steam {
        let mut steam = Steam::settled();
        let step = lxb_steam::setup::Step {
            said: said.to_string(),
            percent,
        };
        steam.setting_up = Some(step.clone());
        steam.setup_is_watched = true;
        steam.signing_in = Some(Stage::FirstSetup(step));
        steam
    }

    /// What the panel says while Steam installs itself, and what it offers.
    ///
    /// The whole of the design decision in one test: it is a *wait*, not a
    /// question, so there is nothing to answer and nothing to cancel. The one
    /// button leaves it running.
    #[test]
    fn the_first_setup_panel_is_a_wait_that_can_be_walked_away_from() {
        let steam = setting_up("Downloading Steam", Some(42));
        let panel = steam.panel().expect("the setup raises a panel");

        assert_eq!(
            panel.lines.first(),
            Some(&dialog::Line::Heading("Setting up Steam".to_string())),
            "a four-minute wait under \"Sign in to Steam\" reads as a slow sign-in"
        );
        assert!(
            panel.lines.iter().any(|line| matches!(
                line,
                dialog::Line::Note(note) if note.contains("once")
            )),
            "the panel has to say this does not happen every time"
        );
        // The panel cuts a note rather than wrapping it, so every one of them
        // has to fit on its own line. Measured off a screenshot at 1280x800,
        // where a note of 58 characters was already being cut.
        for line in &panel.lines {
            if let dialog::Line::Note(note) = line {
                assert!(
                    note.chars().count() <= 55,
                    "this note will be cut on screen: {note:?}"
                );
            }
        }
        assert!(
            panel.lines.contains(&dialog::Line::Progress(42)),
            "a step that carries a number is drawn as a bar: {:?}",
            panel.lines
        );
        assert!(!panel.typing, "there is nothing here to type into");

        // One button, and it is not a cancel. Half a gigabyte is coming down.
        let commands: Vec<menu::Command> =
            panel.buttons.iter().map(|entry| entry.command).collect();
        assert_eq!(commands, vec![menu::Command::SteamSetupInBackground]);
    }

    /// The bar and the lights stand in the same place, so the panel does not
    /// change shape when the download gives way to the unpack.
    #[test]
    fn a_step_with_nothing_to_count_waits_rather_than_showing_an_empty_bar() {
        let counting = setting_up("Downloading Steam", Some(7));
        let not = setting_up("Unpacking Steam", None);

        let counting = counting.panel().expect("a panel").lines;
        let not = not.panel().expect("a panel").lines;
        assert_eq!(counting.len(), not.len(), "{counting:?} against {not:?}");
        assert!(counting.contains(&dialog::Line::Progress(7)));
        assert!(not.contains(&dialog::Line::Waiting));
        assert!(
            !not.iter()
                .any(|line| matches!(line, dialog::Line::Progress(_))),
            "an unpack has no length, and a bar at nothing reads as one that stalled"
        );
    }

    /// Back on the setup panel takes the panel away and nothing else.
    ///
    /// The two halves that matter: the install goes on — [`Steam::setting_up`]
    /// is still true, so pressing the row again comes back to it — and Steam is
    /// never told a sign-in was abandoned, because none was begun.
    #[test]
    fn walking_away_from_the_setup_leaves_it_running() {
        let mut steam = setting_up("Downloading Steam", Some(42));

        steam.let_the_setup_run_in_the_background();
        assert!(steam.panel().is_none(), "the panel went");
        assert!(steam.setting_up(), "and the install did not");

        // The same again through Back, which is the other way out of a panel
        // and must not mean something different.
        let mut steam = setting_up("Downloading Steam", Some(42));
        steam.cancel();
        assert!(steam.panel().is_none());
        assert!(steam.setting_up());
    }

    /// Who hears about the ending depends on who stayed to watch it.
    #[test]
    fn a_finished_setup_moves_the_panel_on_or_announces_itself_but_never_both() {
        // Watched: straight on to the questions, and nothing announced. A
        // notification about what is happening on screen is noise.
        let mut watched = setting_up("Installing Steam", None);
        let changed = watched.apply(Event::Setup(lxb_steam::setup::SetUp::Done));
        assert!(changed.panel);
        assert!(!changed.steam_is_ready);
        assert!(!watched.setting_up());
        assert!(matches!(watched.signing_in, Some(Stage::Choosing)));

        // Walked away from: nothing is raised over whatever they are doing
        // now, and they are told.
        let mut away = setting_up("Installing Steam", None);
        away.let_the_setup_run_in_the_background();
        let changed = away.apply(Event::Setup(lxb_steam::setup::SetUp::Done));
        assert!(changed.steam_is_ready);
        assert!(away.panel().is_none(), "nothing is put in front of them");
        assert!(!away.setting_up());
    }

    /// A setup that failed is raised whether or not anybody was watching.
    ///
    /// The asymmetry with the finish above is the point: a good ending can wait
    /// for somebody to come back to it, and a failure is a question — try
    /// again, or not — that a session which swallowed it would leave a Steam
    /// row offering a sign-in it can never do.
    #[test]
    fn a_failed_setup_is_put_in_front_of_whoever_asked_for_it() {
        let mut steam = setting_up("Downloading Steam", Some(3));
        steam.let_the_setup_run_in_the_background();
        assert!(steam.panel().is_none());

        let changed = steam.apply(Event::Setup(lxb_steam::setup::SetUp::Failed(
            "Steam could not set itself up: Not enough disk space".to_string(),
        )));
        assert!(changed.panel);
        assert!(!steam.setting_up(), "nothing is still running");

        let panel = steam.panel().expect("the failure is raised");
        assert!(
            panel.lines.iter().any(|line| matches!(
                line,
                dialog::Line::Note(note) if note.contains("Not enough disk space")
            )),
            "Valve's own reason is what is shown: {:?}",
            panel.lines
        );
        let commands: Vec<menu::Command> =
            panel.buttons.iter().map(|entry| entry.command).collect();
        assert_eq!(
            commands,
            vec![menu::Command::SteamSetUpAgain, menu::Command::SteamCancel],
            "trying again installs, and is not the Try again of a failed sign-in"
        );
    }

    /// Sight is given when the client takes the request, not when the button
    /// was pressed.
    ///
    /// The difference is only ever visible on the press that has to sign a
    /// client in first — and on that one it is the whole thing, because those
    /// seconds are seconds with Valve's own login window on the display.
    #[test]
    fn a_hand_over_gives_sight_when_it_lands() {
        let mut steam = Steam::settled();

        // The landing is what asks for sight, and it is the only thing that
        // does: the press says nothing about it, because whatever the worker
        // has to do first takes as long as it takes.
        let landed = lxb_steam::HandedOver {
            after_signing_in: false,
            asked: lxb_steam::Doing::Open,
        };
        assert_eq!(
            steam.apply(Event::HandedOver(landed)).handed_over,
            Some(landed)
        );

        // A refusal is not a landing: a client that would not take the request
        // has no window to show, and revealing it would be the shell giving
        // sight for nothing.
        let refused = steam.apply(Event::HandOverRefused(lxb_steam::Refused::Failed(
            "no".to_string(),
        )));
        assert!(refused.handed_over.is_none());
        assert!(refused.hand_over.is_some());
    }

    /// Progress moves the panel where one is up, and moves nothing where one
    /// is not.
    ///
    /// The second half is what keeps a backgrounded install from asking for a
    /// frame once a second for four minutes, on a bar nobody is looking at it
    /// from.
    #[test]
    fn progress_only_redraws_a_panel_that_is_on_screen() {
        let mut watched = setting_up("Downloading Steam", Some(10));
        let changed = watched.apply(Event::Setup(lxb_steam::setup::SetUp::Working(
            lxb_steam::setup::Step {
                said: "Downloading Steam".to_string(),
                percent: Some(11),
            },
        )));
        assert!(changed.panel);
        assert!(watched
            .panel()
            .unwrap()
            .lines
            .contains(&dialog::Line::Progress(11)));

        let mut away = setting_up("Downloading Steam", Some(10));
        away.let_the_setup_run_in_the_background();
        let changed = away.apply(Event::Setup(lxb_steam::setup::SetUp::Working(
            lxb_steam::setup::Step {
                said: "Downloading Steam".to_string(),
                percent: Some(11),
            },
        )));
        assert!(!changed.panel, "nothing is on screen to redraw");
        assert!(away.setting_up(), "and it is still going");
    }

    /// The sign-in by account name is two questions, and the second one is
    /// about the name given to the first.
    /// What the install panel can say, and what it refuses to invent.
    ///
    /// The confirmation used to say only that the game was absent and could be
    /// fetched, which is what the person pressing already knew. Room and queue
    /// are answerable from this machine; the size and the folder are Steam's
    /// and are not guessed at.
    #[test]
    fn the_preflight_says_what_this_machine_knows_and_no_more() {
        use lxb_steam::library::Room;
        use std::path::PathBuf;

        // Nothing readable: no line at all, rather than a zero that reads as a
        // full disk.
        let nothing = Preflight::default();
        assert_eq!(nothing.room_said(), None);
        assert_eq!(nothing.already, None);

        // One library, which is the ordinary machine, and an exact answer.
        let one = Preflight {
            room: vec![Room {
                path: PathBuf::from("/home/someone/.steam/steam"),
                free: Some(68_000_000_000),
            }],
            already: None,
        };
        assert_eq!(
            one.room_said().as_deref(),
            Some("68 GB free on this machine.")
        );

        // Several: the largest, because the question is whether it fits
        // anywhere, given that Steam picks where.
        let several = Preflight {
            room: vec![
                Room {
                    path: PathBuf::from("/home/someone/.steam/steam"),
                    free: Some(12_000_000_000),
                },
                Room {
                    path: PathBuf::from("/mnt/games/SteamLibrary"),
                    free: Some(900_000_000_000),
                },
                // A drive that has been unplugged since the list was read says
                // nothing, and must not be counted as empty.
                Room {
                    path: PathBuf::from("/mnt/gone/SteamLibrary"),
                    free: None,
                },
            ],
            already: None,
        };
        assert_eq!(
            several.room_said().as_deref(),
            Some("900 GB free on the largest of 3 Steam libraries.")
        );
    }

    #[test]
    fn the_password_panel_names_the_account_it_is_for() {
        let mut steam = Steam::settled();
        steam.begin();
        assert!(matches!(steam.signing_in, Some(Stage::Choosing)));

        steam.with_password();
        typing(&mut steam, "  someone  ");
        steam.submit();

        let Some(Stage::Password { account, .. }) = steam.signing_in.as_ref() else {
            panic!("the panel did not move on to the password");
        };
        assert_eq!(account, "someone", "the field was not trimmed");

        let panel = steam.panel().expect("a panel is up");
        assert!(panel.typing, "a field with no keyboard cannot be filled in");
        assert!(panel
            .lines
            .iter()
            .any(|line| matches!(line, dialog::Line::Secret { typed: 0 })));
        assert!(
            panel
                .lines
                .iter()
                .any(|line| matches!(line, dialog::Line::Note(note) if note.contains("someone"))),
            "the panel does not say whose password it wants"
        );
    }

    #[test]
    fn login_fields_keep_the_keyboard_dismissed_while_typing() {
        let mut steam = Steam::settled();
        let mut osk = crate::keyboard::Osk::default();
        osk.set_controller_in_hand(true);
        steam.with_password();
        let panel = steam.panel().unwrap();
        assert!(panel.lines.iter().any(
            |l| matches!(l,dialog::Line::Note(text) if text=="Enter your Steam account name.")
        ));
        assert!(osk.offer_shell_field(panel.typing));
        // First physical key through the grab: input redraws before the board closes.
        osk.set_controller_in_hand(false);
        assert_eq!(steam.type_into(Stroke::Char('s')), Typed::Into);
        osk.offer_shell_field(steam.panel().unwrap().typing);
        osk.close();
        for c in "omeone".chars() {
            assert_eq!(steam.type_into(Stroke::Char(c)), Typed::Into);
            osk.offer_shell_field(steam.panel().unwrap().typing);
            assert!(!osk.is_open());
        }
        steam.submit();
        let panel = steam.panel().unwrap();
        assert!(panel.lines.iter().any(|l|matches!(l,dialog::Line::Note(text) if text.contains("password") && text.contains("someone"))));
        osk.offer_shell_field(panel.typing);
        assert!(!osk.is_open());
        for c in "private".chars() {
            steam.type_into(Stroke::Char(c));
            osk.offer_shell_field(steam.panel().unwrap().typing);
            assert!(!osk.is_open());
        }
        assert!(steam
            .panel()
            .unwrap()
            .lines
            .iter()
            .any(|l| matches!(l, dialog::Line::Secret { typed: 7 })));
        steam.cancel();
        osk.offer_shell_field(false);
    }

    /// An empty account name is not an answer: the panel stays where it is
    /// rather than asking Steam to sign in as nobody.
    #[test]
    fn an_empty_account_name_does_not_move_on() {
        let mut steam = Steam::settled();
        steam.with_password();
        typing(&mut steam, "   ");
        steam.submit();
        assert!(
            matches!(steam.signing_in, Some(Stage::Account(_))),
            "an empty name was accepted"
        );
    }

    /// The password is drawn from a count and never from itself, so nothing
    /// the panel holds is the password.
    #[test]
    fn the_panel_holds_a_count_and_never_the_password() {
        let mut steam = Steam::settled();
        steam.with_password();
        typing(&mut steam, "someone");
        steam.submit();
        typing(&mut steam, "hunter2");

        let panel = steam.panel().expect("a panel is up");
        assert!(panel
            .lines
            .iter()
            .any(|line| matches!(line, dialog::Line::Secret { typed: 7 })));
        let said = format!("{:?}", panel.lines);
        assert!(!said.contains("hunter2"), "{said}");
    }

    /// Every stage that has a field takes the keyboard, and none of the others
    /// does — a panel that swallowed keys with nothing to type into would take
    /// the bar's own keys with it.
    #[test]
    fn only_the_stages_with_a_field_want_the_keyboard() {
        let mut steam = Steam::settled();
        assert!(!steam.field_wanted(), "nothing is on screen");

        steam.begin();
        assert!(!steam.field_wanted());
        assert_eq!(steam.type_into(Stroke::Char('x')), Typed::Elsewhere);

        steam.with_qr();
        assert!(!steam.field_wanted());
        assert_eq!(steam.type_into(Stroke::Char('x')), Typed::Elsewhere);

        steam.with_password();
        assert!(steam.field_wanted());
        assert!(
            !steam.password_wanted(),
            "an account name is not a password"
        );
        typing(&mut steam, "someone");
        steam.submit();
        assert!(steam.password_wanted());

        // And when the panel has gone, the keyboard is the bar's again.
        steam.cancel();
        assert!(!steam.field_wanted());
        assert_eq!(steam.type_into(Stroke::Char('x')), Typed::Elsewhere);
    }

    /// Return finishes a field and Escape leaves it, and both say so rather
    /// than acting on their own — what happens next needs the whole shell.
    #[test]
    fn return_and_escape_finish_a_field() {
        let mut steam = Steam::settled();
        steam.with_password();
        assert_eq!(
            steam.type_into(Stroke::ENTER),
            Typed::Done { submitted: true }
        );
        assert_eq!(
            steam.type_into(Stroke::ESCAPE),
            Typed::Done { submitted: false }
        );
        // The arrows are the field's, so the bar underneath does not move
        // while somebody is typing into a panel over it.
        assert_eq!(steam.type_into(Stroke::Named("Left")), Typed::Into);
    }

    /// Only a failure that happened while a sign-in was on screen puts a panel
    /// up. A token refused in the background is a sign-out, and a panel
    /// conjured over the bar to report it would interrupt somebody who was
    /// doing something else entirely.
    // --- conversations ---
    const ACCOUNT_A: u64 = 76_561_198_000_000_001;
    const ACCOUNT_B: u64 = 76_561_198_000_000_002;
    const A_FRIEND: u64 = 76_561_198_000_000_011;

    fn signed_in_as(steam: &mut Steam, steam_id: u64) {
        steam.apply(Event::SignedIn(lxb_steam::Account {
            name: format!("account-{steam_id}"),
            steam_id,
        }));
        steam.apply(Event::Chat(lxb_steam::chat::Heard {
            generation: 1,
            account: steam_id,
            word: lxb_steam::chat::Word::Listening,
        }));
        // And a friend to talk to, which is what `may_send` asks the roster.
        steam.apply(Event::Friends(lxb_steam::Roster {
            me: Some(lxb_steam::Person {
                steam_id,
                name: "me".to_string(),
                presence: lxb_steam::Presence::Online,
                game: None,
                app_id: None,
                avatar: None,
            }),
            friends: vec![lxb_steam::Person {
                steam_id: A_FRIEND,
                name: "a friend".to_string(),
                presence: lxb_steam::Presence::Online,
                game: None,
                app_id: None,
                avatar: None,
            }],
        }));
        steam.reach = lxb_steam::Reach::Online;
    }

    fn arrived(account: u64, from: u64, at: u32, body: &str) -> Event {
        Event::Chat(lxb_steam::chat::Heard {
            generation: 1,
            account,
            word: lxb_steam::chat::Word::Arrived {
                with: from,
                said: lxb_steam::chat::Said {
                    key: lxb_steam::chat::Key::new(at, 0),
                    body: body.to_string(),
                    from_me: false,
                },
            },
        })
    }

    /// A message about account B must not reach account A's panel, whatever
    /// order the two arrive in.
    #[test]
    fn a_message_about_another_account_is_dropped() {
        let mut steam = Steam::settled();
        signed_in_as(&mut steam, ACCOUNT_A);
        let changed = steam.apply(arrived(ACCOUNT_B, A_FRIEND, 100, "not for you"));
        assert!(!changed.chat);
        assert!(changed.messages.is_empty());
        assert_eq!(steam.conversations().unread(), 0);
        // And the same message about the account that *is* signed in does land.
        let changed = steam.apply(arrived(ACCOUNT_A, A_FRIEND, 100, "for you"));
        assert!(changed.chat);
        assert_eq!(changed.messages.len(), 1);
        assert_eq!(steam.conversations().unread(), 1);
    }

    /// Signing out clears every conversation, the unread counts with them.
    #[test]
    fn signing_out_clears_every_conversation() {
        let mut steam = Steam::settled();
        signed_in_as(&mut steam, ACCOUNT_A);
        steam.apply(arrived(ACCOUNT_A, A_FRIEND, 100, "hello"));
        assert_eq!(steam.conversations().unread(), 1);
        let changed = steam.apply(Event::SignedOut);
        assert!(changed.chat);
        assert!(steam.conversations().with(A_FRIEND).is_none());
        assert_eq!(steam.conversations().unread(), 0);
        assert!(steam.conversations().account().is_none());
    }

    /// And one account replacing another does the same, immediately.
    #[test]
    fn a_second_account_replaces_the_first_s_conversations() {
        let mut steam = Steam::settled();
        signed_in_as(&mut steam, ACCOUNT_A);
        steam.apply(arrived(ACCOUNT_A, A_FRIEND, 100, "hello"));
        signed_in_as(&mut steam, ACCOUNT_B);
        assert!(steam.conversations().with(A_FRIEND).is_none());
        assert_eq!(steam.conversations().account(), Some(ACCOUNT_B));
        // And the first account's answers no longer reach anything.
        let changed = steam.apply(arrived(ACCOUNT_A, A_FRIEND, 200, "late"));
        assert!(!changed.chat);
    }

    /// Somebody who leaves the friends list can no longer be written to, and
    /// the panel is told why. What was said stays: it was said.
    #[test]
    fn unfriending_somebody_closes_their_conversation_to_writing() {
        let mut steam = Steam::settled();
        signed_in_as(&mut steam, ACCOUNT_A);
        steam.apply(arrived(ACCOUNT_A, A_FRIEND, 100, "hello"));
        assert!(steam.may_send(A_FRIEND, "hi").is_ok());
        // Steam's next roster no longer lists them.
        steam.apply(Event::Friends(lxb_steam::Roster {
            me: steam.roster().me.clone(),
            friends: Vec::new(),
        }));
        assert_eq!(
            steam.may_send(A_FRIEND, "hi"),
            Err(lxb_steam::chat::Refused::NotAFriend)
        );
        assert!(
            steam.conversations().with(A_FRIEND).is_some(),
            "what was said was thrown away with the friendship"
        );
    }

    /// Nothing is sent while this account is standing offline on Steam, or
    /// while the CM is reconnecting — and the messages already on screen stay.
    #[test]
    fn nothing_is_sent_while_steam_is_out_of_reach() {
        let mut steam = Steam::settled();
        signed_in_as(&mut steam, ACCOUNT_A);
        steam.apply(arrived(ACCOUNT_A, A_FRIEND, 100, "hello"));

        steam.reach = lxb_steam::Reach::Offline("the network went".to_string());
        assert_eq!(
            steam.may_send(A_FRIEND, "hi"),
            Err(lxb_steam::chat::Refused::NotConnected)
        );
        steam.reach = lxb_steam::Reach::Online;

        // And the account standing offline on Steam, which is the other half.
        let mut me = steam.roster().me.clone().expect("a persona");
        me.presence = lxb_steam::Presence::Offline;
        steam.apply(Event::Friends(lxb_steam::Roster {
            me: Some(me),
            friends: steam.roster().friends.clone(),
        }));
        assert_eq!(
            steam.may_send(A_FRIEND, "hi"),
            Err(lxb_steam::chat::Refused::NotConnected)
        );
        assert_eq!(
            steam
                .conversations()
                .with(A_FRIEND)
                .expect("a conversation")
                .len(),
            1,
            "going offline took a message off the screen"
        );
    }

    /// Two messages in one pass are two announcements. Neither may be dropped.
    #[test]
    fn every_message_in_a_pass_is_announced() {
        let mut steam = Steam::settled();
        signed_in_as(&mut steam, ACCOUNT_A);
        let mut changed = Changed::default();
        changed.absorb(steam.apply(arrived(ACCOUNT_A, A_FRIEND, 100, "one")));
        changed.absorb(steam.apply(arrived(ACCOUNT_A, A_FRIEND, 101, "two")));
        assert_eq!(changed.messages.len(), 2);
        assert_eq!(changed.messages[0].1, "one");
        assert_eq!(changed.messages[1].1, "two");
    }

    /// The fixture invents an account, a roster and every state the panel
    /// draws — and reaches nothing.
    #[test]
    fn the_chat_fixture_invents_a_conversation_and_asks_steam_nothing() {
        let mut steam = Steam::settled();
        steam.invent_conversations(&[
            "Ann".to_string(),
            "Bea".to_string(),
            "Cal".to_string(),
            "Dee".to_string(),
            "Eve".to_string(),
        ]);
        assert_eq!(steam.roster().friends.len(), 5);
        // In band order, like every roster Steam sends — the one thing a
        // fixture built straight into a `Roster` can get wrong, and did.
        let bands: Vec<lxb_steam::Band> = steam
            .roster()
            .friends
            .iter()
            .map(|friend| friend.band())
            .collect();
        assert!(
            bands.windows(2).all(|pair| pair[0] <= pair[1]),
            "the invented roster is not in band order: {bands:?}"
        );
        // Every one of the five states, by the id each was given: the list is
        // sorted, so a place in it is not the place a name was given in.
        let ids: Vec<u64> = (0..5).map(|at| INVENTED_FRIEND + at).collect();
        let read = steam.conversations().with(ids[0]).expect("a conversation");
        assert!(read.len() > 1 && read.history().failure().is_none());
        let empty = steam.conversations().with(ids[1]).expect("a conversation");
        assert!(empty.is_empty() && empty.history().failure().is_none());
        let failed = steam.conversations().with(ids[2]).expect("a conversation");
        assert!(failed.history().failure().is_some());
        let sending = steam.conversations().with(ids[3]).expect("a conversation");
        let lines = sending.lines();
        assert!(lines.iter().any(|line| line.failure().is_some()));
        assert!(lines.iter().any(|line| line.sending()));
        let waiting = steam.conversations().with(ids[4]).expect("a conversation");
        assert_eq!(waiting.unread(), 1);
        assert!(waiting.typing(Instant::now()));
        // Nothing invented can collide with a real account, and nothing here
        // has a worker to send through: an invented session is a settled one.
        assert!(ids.iter().all(|id| *id < 76_561_197_960_265_728));
        assert!(!steam.driving());
    }

    #[test]
    fn a_failure_with_no_panel_up_does_not_conjure_one() {
        let mut steam = Steam::settled();
        steam.apply(Event::SignInFailed("no".to_string()));
        assert!(steam.panel().is_none());

        steam.begin();
        steam.apply(Event::SignInFailed("no".to_string()));
        assert!(matches!(steam.signing_in, Some(Stage::Failed(_))));
        let panel = steam.panel().expect("a panel is up");
        assert_eq!(panel.buttons.len(), 2, "there is a way on and a way out");
    }

    /// What the diagnostics panel says, and — the whole reason it is checked —
    /// what it does not say.
    ///
    /// The account's number is four digits of it. A panel that printed the
    /// whole SteamID would be an identifier in a screenshot somebody puts in a
    /// bug report, which is exactly the thing this panel exists to make easy to
    /// send.
    #[test]
    fn the_diagnostics_panel_says_what_it_may_and_no_more() {
        let mut steam = Steam::settled();
        steam.apply(Event::SignedIn(lxb_steam::Account {
            name: "someone".to_string(),
            steam_id: 76561198042371721,
        }));

        let values = steam.diagnostics();
        let value = |wanted: &str| {
            values
                .iter()
                .find(|(label, _)| label == wanted)
                .map(|(_, value)| value.clone())
                .unwrap_or_else(|| panic!("no {wanted} on the panel"))
        };
        assert_eq!(value("Account"), "someone (…1721)");
        // Not the number itself, in any line of it.
        assert!(
            !values
                .iter()
                .any(|(_, value)| value.contains("76561198042371721")),
            "the whole SteamID is on the panel"
        );
        assert_eq!(value("In flight"), "nothing");
        // And the line that says whether anything running as this user can
        // drive Valve's client, which is the one value on the panel that is
        // about the machine's safety rather than about Steam's state.
        assert!(
            !value("Debugging interface").is_empty(),
            "the panel has to say what the interface is doing"
        );
        assert!(value("Steam").len() > 1, "the reach has to say which it is");

        // And with nobody signed in it still answers, which is the state it is
        // most likely to be opened in.
        let empty = Steam::settled().diagnostics();
        assert!(empty
            .iter()
            .any(|(label, value)| label == "Account" && value == "nobody signed in"));

        // Every value fits the one line the panel gives it. What is checked is
        // the wording this shell chose, not a path off the machine this runs
        // on: a Steam directory is as long as somebody's home is.
        for (label, value) in &values {
            if label == "Steam directory" {
                continue;
            }
            assert!(
                value.chars().count() <= FITS_ON_A_LINE,
                "{label}: {value} is too long for the panel"
            );
        }
    }

    /// A path is printed against the user's own home, because the panel's width
    /// is better spent on the part that differs between machines.
    #[test]
    fn a_steam_directory_is_named_from_the_home_it_is_in() {
        let home = std::path::Path::new("/home/somebody");
        assert_eq!(
            said_under(
                std::path::Path::new("/home/somebody/.steam/steam"),
                Some(home)
            ),
            "~/.steam/steam"
        );
        // And anything outside it is printed as it is, rather than being
        // mangled into a relative path that would be a different directory.
        assert_eq!(
            said_under(std::path::Path::new("/mnt/games/Steam"), Some(home)),
            "/mnt/games/Steam"
        );
        // A session with no home in its environment, which is not a session
        // worth a panic over.
        assert_eq!(
            said_under(std::path::Path::new("/mnt/games/Steam"), None),
            "/mnt/games/Steam"
        );
    }

    /// One word from Valve's client about a wake nothing here is waiting on.
    ///
    /// Every test below is about what a report does to the library and to the
    /// rows, which is a fact about the machine and true whoever asked. Which
    /// press an answer belongs to is the gate's question and is tested there.
    fn client_said(report: lxb_steam::ClientReport) -> Event {
        Event::Client {
            ticket: lxb_steam::Ticket::default(),
            report,
        }
    }

    /// A press waiting on Valve's client, for the gate's own tests.
    ///
    /// The number is the wake it asked for, and it is what every answer below
    /// is addressed to. Two presses in one test are two numbers, because the
    /// whole of what the gate does is tell them apart.
    fn press(request: u64, app_id: u32) -> Awaiting {
        Awaiting {
            request,
            app_id,
            name: format!("Game {app_id}"),
            display: crate::Display::for_a_test(0),
            from: [0.0; 4],
            asked: std::time::Instant::now(),
            joining: None,
        }
    }

    /// What an accepted invitation is handed to Valve's client as.
    ///
    /// The lobby form is the one that matters: it is what the client's own Join
    /// does, and it is the only form that works in *both* states the machine
    /// can be in — the game running and the game not. Anything else is a
    /// command line, and there the spaces become plus signs because a URL
    /// cannot carry a space.
    #[test]
    fn an_accepted_invitation_is_a_join_or_a_command_line() {
        let invite = |connect: &str| lxb_steam::chat::Invite {
            id: 1,
            from: 76_561_198_000_000_002,
            at: 0,
            key: None,
            connect: connect.to_string(),
            app_id: Some(220),
            game: None,
            taken: false,
        };
        assert_eq!(
            joining_url(220, &invite("+connect_lobby 109775240000000000")),
            "steam://joinlobby/220/109775240000000000/76561198000000002"
        );
        assert_eq!(
            joining_url(220, &invite("+connect 127.0.0.1:27015")),
            "steam://rungameid/220//+connect+127.0.0.1:27015"
        );
        // And an invitation carrying nothing at all still starts the game,
        // which is what the person who sent it was asking for.
        assert_eq!(joining_url(220, &invite("")), "steam://rungameid/220//");
    }

    /// Two games in flight are two watches, and one of them ending leaves the
    /// other's question still worth hearing.
    ///
    /// Both halves of what one watch got wrong, from the shell's end. The watch
    /// was one number for the session, so arming the second screen's launch
    /// overwrote the first screen's — a question about the first game then read
    /// as a launch nobody was watching and was dropped — and ending either
    /// loading screen cleared the number for both.
    #[test]
    fn a_question_about_one_launch_is_not_lost_to_another() {
        let mut steam = Steam::settled();
        steam.watch_this_launch(367520);
        steam.watch_this_launch(1145360);

        // Whatever number each was started under, a question naming that number
        // is that game's own and is heard.
        let first = steam.watched_launches[&367520];
        let second = steam.watched_launches[&1145360];
        assert_ne!(first, second, "two launches were watched under one number");

        let asking = |request, app_id| {
            Event::LaunchIsAsking(lxb_steam::Asking {
                request,
                app_id,
                task: "cloud sync".to_string(),
                question: None,
            })
        };
        assert!(steam.apply(asking(first, 367520)).asking.is_some());

        // The first game's loading screen ends. The second is still loading,
        // and the client stopping *it* to ask something still reaches the
        // screen.
        steam.stop_watching_the_launch(367520);
        assert!(steam.apply(asking(first, 367520)).asking.is_none());
        assert!(steam.apply(asking(second, 1145360)).asking.is_some());

        // And a question under a number that was never this shell's is nobody's
        // — which is the race the numbers are for: a watcher decides to speak,
        // and the press it is speaking about ends before the message is read.
        assert!(steam.apply(asking(second + 99, 1145360)).asking.is_none());

        // A launch the client gave up on is under the same numbers, and the
        // same race: the watch that speaks about it is the watch that was
        // started for it, or nothing at all.
        let refused = |request, app_id| {
            Event::LaunchWasRefused(lxb_steam::LaunchRefused {
                request,
                app_id,
                why: "AppError_19".to_string(),
            })
        };
        assert!(
            steam
                .apply(refused(second, 1145360))
                .launch_refused
                .is_some(),
            "the launch this shell is watching was refused"
        );
        assert!(
            steam
                .apply(refused(second + 99, 1145360))
                .launch_refused
                .is_none(),
            "and one under a number nobody is watching is nobody's"
        );
        steam.stop_watching_the_launch(1145360);
        assert!(steam
            .apply(refused(second, 1145360))
            .launch_refused
            .is_none());
    }

    /// The client comes up after the press that asked for it has been given up
    /// on — and the game must not start.
    ///
    /// This is the failure the gate exists for. Waking a cold client is most of
    /// two minutes and a loading screen runs out of patience before that, so
    /// the shell says the game did not start, takes the screen back, and *then*
    /// the wake answers. Starting the game there opens it over a start screen
    /// with no loading screen, no dip and no warning, minutes after the shell
    /// said it had not started.
    #[test]
    fn a_client_that_comes_up_late_does_not_start_the_game() {
        let mut gate = Gate::default();
        gate.press(press(1, 504230));
        assert!(gate.busy());

        // The loading screen gives up.
        let given_up = gate.gave_up().expect("the press it was waiting on");
        assert_eq!(given_up.app_id, 504230);
        assert!(!gate.busy(), "the gate is still holding an ended press");

        // And then the client answers.
        assert!(
            matches!(gate.answered(1), Answer::NoGame(NoGame::ThePressEnded)),
            "a late answer would have started the game"
        );
    }

    /// A gate that is not let go of swallows every press for the rest of the
    /// session, which is what it did before there was one function for ending a
    /// press.
    #[test]
    fn a_press_that_ended_does_not_block_the_next_one() {
        let mut gate = Gate::default();
        gate.press(press(1, 504230));

        // A second press while the first is waiting does nothing at all: one
        // game starts at a time.
        assert!(gate.busy());

        // Every terminal path is this one call — patience, an unplugged
        // display, a dismissed panel — and after any of them the next press is
        // taken.
        assert!(gate.gave_up().is_some());
        gate.press(press(2, 220200));
        assert_eq!(gate.waiting().map(|press| press.app_id), Some(220200));

        // The second press's own wake starts the second press's game.
        assert!(matches!(gate.answered(2), Answer::Waiting(press) if press.app_id == 220200));
    }

    /// **The failure this whole envelope exists for.**
    ///
    /// A press is made and given up on. Another press is made. The *first*
    /// wake — most of two minutes into starting a program, and impossible to
    /// call back — comes up and says `Ready`.
    ///
    /// The gate used to hold one anonymous mark, which a new press cleared, so
    /// that answer was read as the second press's and started its game against
    /// a client woken for the first. With an account signed out and another
    /// signed in inside that stretch, it started one account's game on the
    /// other account's client. Now the second press waits for its own number
    /// and this one is exactly what it was: a wake nobody is waiting on.
    #[test]
    fn an_abandoned_wake_does_not_answer_the_press_that_came_after_it() {
        let mut gate = Gate::default();
        gate.press(press(1, 7));
        gate.gave_up();
        gate.press(press(2, 9));

        assert!(
            matches!(gate.answered(1), Answer::NoGame(NoGame::ThePressEnded)),
            "the abandoned wake was handed to the press that came after it"
        );
        assert_eq!(
            gate.waiting().map(|press| press.app_id),
            Some(9),
            "the press that is still waiting was answered by somebody else's wake"
        );

        // And its own answer, when it comes, is its own.
        assert!(matches!(gate.answered(2), Answer::Waiting(press) if press.app_id == 9));
    }

    /// Giving up twice, and giving up on nothing, are both nothing.
    #[test]
    fn there_is_nothing_to_give_up_on_twice() {
        let mut gate = Gate::default();
        assert!(
            gate.gave_up().is_none(),
            "it gave up on a press nobody made"
        );

        gate.press(press(1, 7));
        assert!(gate.gave_up().is_some());
        assert!(gate.gave_up().is_none());
        // Still one answer to spend, not two.
        assert!(matches!(
            gate.answered(1),
            Answer::NoGame(NoGame::ThePressEnded)
        ));
        assert!(matches!(gate.answered(1), Answer::Unexpected));
    }

    /// A wake given up on is still a client on its way up, and the halves that
    /// close one have to know.
    ///
    /// [`Gate::busy`] answers "is a press waiting", which is what the gate is
    /// for and is not this question. A press abandoned early — Back off the
    /// loading screen, or its patience running out — leaves a wake most of the
    /// way through starting a program with nobody waiting for it, and the
    /// shell reads "no press starting, no client running" as nothing left to
    /// decide: the client comes up a minute later and stays up for the session
    /// with the setting saying not to. See
    /// `Shell::close_steam_when_the_press_is_over`.
    #[test]
    fn a_wake_nobody_is_waiting_on_is_still_a_client_coming_up() {
        let mut gate = Gate::default();
        assert!(!gate.a_wake_is_still_out_there());

        gate.press(press(1, 7));
        assert!(gate.a_wake_is_still_out_there());

        // Given up on. `busy` is now false and the wake is still running.
        gate.gave_up();
        assert!(!gate.busy());
        assert!(gate.a_wake_is_still_out_there());

        // And it ends: a wake always answers, and the answer spends the number,
        // so this cannot hold a client open for the session.
        assert!(matches!(
            gate.answered(1),
            Answer::NoGame(NoGame::ThePressEnded)
        ));
        assert!(!gate.a_wake_is_still_out_there());
    }

    /// A wake the shell asked for on nobody's behalf is still a wake it has to
    /// account for.
    ///
    /// Three reasons to wake a client and only one of them is a game, so a
    /// failure owes three different things: nothing at all for a client warmed
    /// up before anybody pressed anything, a panel for a row that promised
    /// Steam would start, and nothing again for a press the user has already
    /// been told about. Told apart by number, because they overlap — the
    /// session warms a client up at startup and somebody presses a game while
    /// it is still coming up.
    #[test]
    fn every_reason_to_wake_a_client_is_answered_as_what_it_was() {
        let mut gate = Gate::default();
        gate.nobody_asked(1);
        gate.steam_was_promised(2);
        gate.press(press(3, 7));
        gate.gave_up();

        // All three are out there at once, and each is answered as itself.
        assert!(gate.a_wake_is_still_out_there());
        assert!(matches!(
            gate.answered(2),
            Answer::NoGame(NoGame::SteamWasPromised)
        ));
        assert!(matches!(
            gate.answered(3),
            Answer::NoGame(NoGame::ThePressEnded)
        ));
        assert!(matches!(
            gate.answered(1),
            Answer::NoGame(NoGame::NobodyAsked)
        ));
        assert!(!gate.a_wake_is_still_out_there());
    }

    /// An answer to a wake this shell never asked for is the one worth a line.
    ///
    /// Every other arm is a wake with a number the gate knows. This one is the
    /// shell losing something, and saying nothing about it is how it stays
    /// lost.
    #[test]
    fn a_wake_nobody_asked_for_is_not_a_press_that_ended() {
        let mut gate = Gate::default();
        assert!(
            matches!(gate.answered(1), Answer::Unexpected),
            "a wake nothing asked for was passed off as a press that ended"
        );

        gate.press(press(1, 7));
        gate.gave_up();
        assert!(matches!(
            gate.answered(1),
            Answer::NoGame(NoGame::ThePressEnded)
        ));
    }

    /// Signing out, or the Steam integration being turned off, ends every wake
    /// this shell was accounting for.
    ///
    /// The ground under all of them has gone: the account that pressed, the
    /// worker that was going to answer, or both. What is left of them must not
    /// keep [`Gate::a_wake_is_still_out_there`] true for the rest of the
    /// session — that is what decides whether a client is worth closing.
    #[test]
    fn turning_steam_off_lets_go_of_every_wake() {
        let mut gate = Gate::default();
        gate.nobody_asked(1);
        gate.press(press(2, 7));

        let waiting = gate.everything_is_off().expect("the press that was up");
        assert_eq!(waiting.app_id, 7);
        assert!(!gate.a_wake_is_still_out_there());
        assert!(matches!(gate.answered(1), Answer::Unexpected));
        assert!(matches!(gate.answered(2), Answer::Unexpected));
    }

    /// A download that says it is running while nothing arrives says so on the
    /// row, and takes it back the moment a byte turns up.
    ///
    /// The word matters more than it looks. Without it the row counts the same
    /// percentage for the rest of the session and goes on implying that
    /// something is happening, which is the failure this exists for — see
    /// [`lxb_steam::Event::InstallStuck`].
    #[test]
    fn a_download_that_stops_moving_says_so_on_its_row() {
        let mut steam = Steam::settled();
        steam.apply(Event::Installing {
            app_id: 504230,
            done: 40,
            total: 100,
            live: None,
        });
        assert_eq!(steam.fetching[&504230].said(), "Installing… 40% of 100 B");
        assert_eq!(steam.fetching[&504230].fraction(), Some(0.4));

        let changed = steam.apply(Event::InstallStuck {
            app_id: 504230,
            quiet_for: std::time::Duration::from_secs(3600),
        });
        assert!(changed.library, "the column was not redrawn");
        assert_eq!(
            steam.fetching[&504230].said(),
            "Installing… 40% of 100 B — not moving"
        );
        // The bar is still drawn, and still at forty: a download that has
        // stopped has still got as far as it has got. What changes is that
        // the fill goes quiet — see [`crate::apps::Arriving::stuck`].
        assert_eq!(steam.fetching[&504230].fraction(), Some(0.4));

        // The same look at the disk, again: still nothing, still said.
        steam.apply(Event::Installing {
            app_id: 504230,
            done: 40,
            total: 100,
            live: None,
        });
        assert!(steam.fetching[&504230].stuck);

        // And a byte arriving takes it back, which is what makes the word a
        // description of now rather than a mark the row wears for ever.
        let changed = steam.apply(Event::Installing {
            app_id: 504230,
            done: 41,
            total: 100,
            live: None,
        });
        assert!(changed.library);
        assert_eq!(steam.fetching[&504230].said(), "Installing… 41% of 100 B");
    }

    /// The row is drawn from what the client says while the client will say it,
    /// and from the disk when it will not.
    ///
    /// The manifest is not a progress source. Measured on real hardware:
    /// through a whole install of a 626 MB game `BytesDownloaded` read nought
    /// for six of the nine seconds it took, and briefly nought *again*
    /// halfway. The client's own overview over the same nine seconds went 4,
    /// 35, 47, 58, 71, 82, 98, 100 with a rate beside each. So the client
    /// answers first — and the size still comes off the disk, which is the one
    /// thing that pair is good for.
    #[test]
    fn a_download_says_what_the_client_says_and_falls_back_to_the_disk() {
        let live = |percent: u8, bytes_per_second: u64, moving: bool| {
            Some(lxb_steam::webui::Live {
                app_id: 945360,
                percent,
                bytes_per_second,
                seconds_left: None,
                moving,
            })
        };

        // What the disk said for most of that install, and on its own it is
        // nothing: no percentage, no bar, one word.
        let disk_only = Fetching {
            done: 0,
            total: 656_543_488,
            live: None,
            stuck: false,
        };
        assert_eq!(disk_only.fraction(), None);
        assert_eq!(disk_only.said(), "Installing…");

        // The same instant with the client answering. The rate is the one
        // thing nothing on the disk carries.
        let answered = Fetching {
            live: live(47, 61_937_624, true),
            ..disk_only
        };
        assert_eq!(answered.fraction(), Some(0.47));
        assert_eq!(answered.said(), "Installing… 47% of 657 MB · 62 MB/s");

        // A client that has not taken a second sample yet says how far but not
        // how fast, and a row must not read "0 B/s" over a download that is
        // going perfectly well.
        let no_rate = Fetching {
            live: live(4, 0, true),
            ..disk_only
        };
        assert_eq!(no_rate.said(), "Installing… 4% of 657 MB");

        // And the client knows how far along it is before the manifest names a
        // size at all — a whole second before, measured. The row says the half
        // it has rather than "of 0 B".
        let early = Fetching {
            done: 0,
            total: 0,
            live: live(1, 0, true),
            stuck: false,
        };
        assert_eq!(early.said(), "Installing… 1%");

        // And where the client will not answer at all, the disk is still read
        // — this is a download somebody started in Steam's own window.
        let from_the_disk = Fetching {
            done: 300,
            total: 1000,
            live: None,
            stuck: false,
        };
        assert_eq!(from_the_disk.fraction(), Some(0.3));
        assert_eq!(from_the_disk.said(), "Installing… 30% of 1.0 KB");

        // The client's answer wins over the disk's where both have one: it is
        // the one that moves.
        let both = Fetching {
            live: live(82, 72_283_456, true),
            ..from_the_disk
        };
        assert_eq!(both.fraction(), Some(0.82));

        // And "not moving" is still said over the top of whichever answered.
        let stalled = Fetching {
            stuck: true,
            ..both
        };
        assert_eq!(
            stalled.said(),
            "Installing… 82% of 1.0 KB · 72 MB/s — not moving"
        );
    }

    /// The two facts a pass carries about the same row, arriving one pass
    /// apart — which is what a session with a stored credential always does:
    /// the account and the remembered library come off the disk at once, and
    /// Steam answers a second or two later.
    ///
    /// The second pass is the whole of this test. It carries nothing but how
    /// far away Steam is, and for a long time that was quietly thrown away
    /// where the pass was folded together: the row went on saying "Connecting
    /// to Steam · library from just now" for the rest of the session, over a
    /// column of games read from Steam that minute, while the diagnostics
    /// panel a press away said Steam was answering. See [`Changed::absorb`].
    #[test]
    fn a_pass_carrying_nothing_but_the_reach_still_rebuilds_the_row() {
        let mut steam = Steam::settled();
        let restored = steam.take_in(vec![
            Event::SignedIn(lxb_steam::Account {
                name: "someone".to_string(),
                steam_id: 1,
            }),
            Event::Reach(lxb_steam::Reach::Restoring),
            Event::LibraryAsOf(Some(std::time::SystemTime::now())),
        ]);
        assert!(restored.account && restored.reach);
        assert_eq!(
            steam.standing().as_deref(),
            Some("Connecting to Steam · library from just now")
        );

        let answered = steam.take_in(vec![
            Event::Reach(lxb_steam::Reach::Online),
            Event::LibraryAsOf(None),
        ]);
        assert!(
            answered.reach,
            "the Games row is built from this and nothing else says it changed"
        );
        assert!(!answered.account, "nobody signed in or out");
        assert_eq!(steam.standing(), None, "there is nothing left to say");
    }

    /// And the other field the fold used to drop: a `steam:` request the
    /// client refused. Nothing else in a pass carries it, so a pass that let
    /// go of it was a press — Open Steam, Install with Steam, Verify — that
    /// appeared to do nothing at all.
    #[test]
    fn a_refusal_survives_the_pass_it_arrived_on() {
        let mut steam = Steam::settled();
        let changed = steam.take_in(vec![Event::HandOverRefused(lxb_steam::Refused::Failed(
            "Steam would not take that".to_string(),
        ))]);
        assert!(matches!(
            changed.hand_over,
            Some(lxb_steam::Refused::Failed(why)) if why == "Steam would not take that"
        ));
    }

    /// Steam is asked what a title runs under once, and asked again only where
    /// the asking failed.
    ///
    /// Both halves matter. Asking twice for an answer already in hand is a menu
    /// that waits on a client for a list it is holding; *not* asking again after
    /// a failure is a row that says "Steam is not signed in" for the rest of a
    /// session that signed in ten seconds later.
    #[test]
    fn a_compatibility_list_is_asked_for_once_and_a_failure_is_asked_again() {
        let which = lxb_steam::webui::Which::Game(7);
        let offered = lxb_steam::webui::Compatibility {
            tools: vec![lxb_steam::webui::Tool {
                name: "proton_experimental".to_string(),
                display: "Proton Experimental".to_string(),
            }],
            forced: None,
        };

        let mut steam = Steam::settled();
        assert_eq!(steam.compatibility(which), None, "nobody has asked");
        steam.ask_about_compatibility(which);
        assert_eq!(steam.compatibility(which), Some(&Compat::Asking));

        let changed = steam.take_in(vec![Event::Compatibility {
            which,
            said: offered.clone(),
        }]);
        assert_eq!(
            changed.compat,
            Some(which),
            "the panel waiting for this has to be told"
        );
        steam.ask_about_compatibility(which);
        assert!(
            matches!(steam.compatibility(which), Some(Compat::Said(said)) if *said == offered),
            "an answer in hand is not asked for again"
        );

        steam.take_in(vec![Event::CompatibilityUnavailable {
            which,
            why: "Steam is not signed in.".to_string(),
        }]);
        steam.ask_about_compatibility(which);
        assert_eq!(
            steam.compatibility(which),
            Some(&Compat::Asking),
            "a client that could not be reached a minute ago is very often up now"
        );
    }

    /// Choosing a tool moves the tick at once, and what Steam says afterwards
    /// is what stands.
    ///
    /// The press cannot wait for the answer — telling the client can mean
    /// starting it, which is most of a minute — and the answer cannot be
    /// assumed, because a choice Steam quietly did not take would otherwise
    /// leave a tick beside a tool the game is not running under.
    #[test]
    fn choosing_a_tool_moves_the_tick_before_steam_answers() {
        let which = lxb_steam::webui::Which::Game(7);
        let mut steam = Steam::settled();
        steam.take_in(vec![Event::Compatibility {
            which,
            said: lxb_steam::webui::Compatibility {
                tools: vec![lxb_steam::webui::Tool {
                    name: "proton_experimental".to_string(),
                    display: "Proton Experimental".to_string(),
                }],
                forced: None,
            },
        }]);

        steam.run_under(which, Some("proton_experimental".to_string()));
        assert!(
            matches!(steam.compatibility(which),
                Some(Compat::Said(said)) if said.forced.as_deref() == Some("proton_experimental")),
            "the row answers the press rather than the client"
        );

        // And Steam, having been told, says what it actually has.
        steam.take_in(vec![Event::Compatibility {
            which,
            said: lxb_steam::webui::Compatibility {
                tools: Vec::new(),
                forced: None,
            },
        }]);
        assert!(
            matches!(steam.compatibility(which), Some(Compat::Said(said)) if said.forced.is_none()),
            "what Steam ended up with outranks what it was asked for"
        );
    }

    /// A choice Valve's client would not take is a panel, and a list that
    /// would not come is a row on the menu waiting for it. The same event
    /// carries both.
    ///
    /// Without the difference, a refused choice is a press that silently did
    /// nothing: the menu was answered and put away when the row was pressed, so
    /// there is no panel left for the answer to be written on.
    #[test]
    fn a_choice_steam_would_not_take_is_said_out_loud() {
        let which = lxb_steam::webui::Which::Game(7);
        let mut steam = Steam::settled();

        // Nobody pressed anything: this is the list failing to arrive, and it
        // belongs on the menu that asked for it.
        let asking = steam.take_in(vec![Event::CompatibilityUnavailable {
            which,
            why: "Steam is not signed in.".to_string(),
        }]);
        assert_eq!(asking.compat, Some(which));
        assert_eq!(asking.compat_refused, None);
        assert_eq!(
            steam.compatibility(which),
            Some(&Compat::Unavailable("Steam is not signed in.".to_string()))
        );

        // And this one is a press.
        steam.run_under(which, Some("proton_experimental".to_string()));
        let refused = steam.take_in(vec![Event::CompatibilityUnavailable {
            which,
            why: "Steam would not take that".to_string(),
        }]);
        assert_eq!(
            refused.compat_refused.as_deref(),
            Some("Steam would not take that")
        );
        assert_eq!(
            steam.compatibility(which),
            None,
            "and what it is set to is not known any more, so the next look asks"
        );
    }

    /// Signing in takes the panel away: what it was asking has been answered.
    #[test]
    fn success_takes_the_panel_away() {
        let mut steam = Steam::settled();
        steam.with_qr();
        let changed = steam.apply(Event::SignedIn(lxb_steam::Account {
            name: "someone".to_string(),
            steam_id: 1,
        }));
        assert!(changed.panel && changed.account);
        assert!(steam.panel().is_none());
        assert_eq!(steam.account(), Some("someone"));
    }

    #[test]
    fn a_library_failure_keeps_the_account_and_a_success_closes_its_panel() {
        let mut steam = Steam::settled();
        steam.apply(Event::SignedIn(lxb_steam::Account {
            name: "someone".to_string(),
            steam_id: 1,
        }));

        let changed = steam.apply(Event::LibraryUnavailable("try later".to_string()));
        assert!(changed.panel);
        assert_eq!(steam.account(), Some("someone"));
        let panel = steam.panel().expect("the failure is visible");
        assert_eq!(panel.buttons[0].command, menu::Command::SteamRefresh);

        let changed = steam.apply(Event::Library(Vec::new()));
        assert!(changed.panel, "the recovered catalogue closes the notice");
        assert!(steam.panel().is_none());
        assert_eq!(steam.account(), Some("someone"));
    }

    /// There is a stretch between the press and Valve's client writing
    /// anything to the disk — waking a cold client is most of a minute — and
    /// without the shell keeping this the row would read "Not installed" for
    /// the whole of it, which is a press that appears to have done nothing.
    #[test]
    fn a_game_that_is_coming_down_says_so_on_its_row() {
        let mut steam = Steam::settled();
        steam.account = Some("someone".to_string());
        steam.games = vec![Game::invented(504230, "Celeste".to_string(), false)];

        // Past the index the library carries over its first game; see
        // [`Steam::alphabetical`].
        let note = |steam: &Steam| {
            let rows = steam.rows();
            match &rows[crate::apps::head_rows(&rows)] {
                crate::apps::Entry::Game(game) => (game.note.clone(), game.progress),
                _ => panic!("that is not a game row"),
            }
        };
        assert_eq!(note(&steam), ("Not installed".to_string(), None));

        // Before the manifests have been read there is no total, and the row
        // still has to say something.
        steam.apply(Event::Installing {
            app_id: 504230,
            done: 0,
            total: 0,
            live: None,
        });
        // No total, so no bar: an empty groove at the start of every install
        // reads as a download that has not begun.
        assert_eq!(note(&steam), ("Installing…".to_string(), None));

        let changed = steam.apply(Event::Installing {
            app_id: 504230,
            done: 300,
            total: 1000,
            live: None,
        });
        assert!(changed.library, "the row has to be drawn again");
        assert_eq!(
            note(&steam),
            (
                "Installing… 30% of 1.0 KB".to_string(),
                Some(crate::apps::Arriving {
                    share: 0.3,
                    stuck: false
                })
            )
        );

        // The same number twice is not a redraw. A download sends a great many
        // of these and the bar is rebuilt for each one that changes anything.
        assert!(
            !steam
                .apply(Event::Installing {
                    app_id: 504230,
                    done: 300,
                    total: 1000,
                    live: None,
                })
                .library
        );
    }

    #[test]
    fn a_download_that_ends_leaves_the_row_to_the_disk_again() {
        let mut steam = Steam::settled();
        steam.account = Some("someone".to_string());
        steam.games = vec![Game::invented(504230, "Celeste".to_string(), false)];
        steam.apply(Event::Installing {
            app_id: 504230,
            done: 1,
            total: 2,
            live: None,
        });
        assert!(steam.is_fetching(504230));

        let done = steam.apply(Event::Installed {
            app_id: 504230,
            into: PathBuf::from("/games/Celeste"),
        });
        assert!(!steam.is_fetching(504230));
        assert_eq!(done.installed, vec![Ended::Done { app_id: 504230 }]);
        assert!(done.library);

        steam.apply(Event::Installing {
            app_id: 504230,
            done: 1,
            total: 2,
            live: None,
        });
        let failed = steam.apply(Event::InstallFailed {
            app_id: 504230,
            why: Stopped::Failed("the line went away".to_string()),
        });
        assert!(
            !steam.is_fetching(504230),
            "and the row goes back to the disk"
        );
        assert_eq!(
            failed.installed,
            vec![Ended::Failed {
                app_id: 504230,
                why: Stopped::Failed("the line went away".to_string())
            }]
        );
    }

    /// A game on its way off the disk says so too, and is not startable while
    /// it goes. Removing is quick but it is not instant, and the row has to
    /// answer for the press that started it — otherwise it reads as "Installed"
    /// right up until the game vanishes.
    #[test]
    fn a_game_being_removed_says_so_and_cannot_be_started() {
        let mut steam = Steam::settled();
        steam.account = Some("someone".to_string());
        steam.games = vec![Game::invented(504230, "Celeste".to_string(), true)];

        let row = |steam: &Steam| {
            let rows = steam.rows();
            match &rows[crate::apps::head_rows(&rows)] {
                crate::apps::Entry::Game(game) => {
                    (game.note.clone(), game.installed, game.updating)
                }
                _ => panic!("that is not a game row"),
            }
        };
        let (_, installed, _) = row(&steam);
        assert!(installed, "it starts out as a game that can be played");

        let changed = steam.apply(Event::Uninstalling { app_id: 504230 });
        assert!(changed.library, "the row has to be drawn again");
        assert!(steam.is_removing(504230));
        assert_eq!(row(&steam), ("Removing…".to_string(), false, true));

        // And when it has gone, the row goes back to being whatever the disk
        // says — which is what takes "Removing…" off it. Nothing else in the
        // session can, so an event that never arrived would strand it.
        let gone = steam.apply(Event::Uninstalled { app_id: 504230 });
        assert!(!steam.is_removing(504230));
        assert_eq!(gone.installed, vec![Ended::Removed { app_id: 504230 }]);
    }

    /// A removal that did not happen has to reach the user. They asked for the
    /// space back and have not got it, and the row alone cannot say so — it
    /// looks exactly like a game nobody touched.
    #[test]
    fn a_removal_that_failed_is_announced_and_lets_go_of_the_row() {
        let mut steam = Steam::settled();
        steam.account = Some("someone".to_string());
        steam.games = vec![Game::invented(504230, "Celeste".to_string(), true)];

        steam.apply(Event::Uninstalling { app_id: 504230 });
        let failed = steam.apply(Event::UninstallFailed {
            app_id: 504230,
            why: "Steam would not take it".to_string(),
        });
        assert!(!steam.is_removing(504230), "the row stops saying Removing");
        assert_eq!(
            failed.installed,
            vec![Ended::RemoveFailed {
                app_id: 504230,
                why: "Steam would not take it".to_string()
            }]
        );
    }

    /// Every question `lxb_steam` can say an install is waiting on reaches the
    /// catalogs as a word they select on, and one it has never said is a
    /// question nobody names rather than an English phrase in the middle of a
    /// translated sentence.
    #[test]
    fn what_an_install_waits_on_is_a_word_for_the_catalogs() {
        for (said, word) in [
            ("an agreement to accept", "agreement"),
            ("a product key to type", "key"),
            ("a password to type", "password"),
            ("a disc to change", "disc"),
            ("an account to sign up for", "signup"),
            ("something new Valve thought of", "other"),
        ] {
            assert_eq!(what_the_install_needs(said), word, "{said}");
        }
    }

    /// The two ways an install can end without the game arriving are not the
    /// same thing to say: one is that something went wrong, the other is that
    /// Steam has a question. The shell answers them with different panels, so
    /// they have to arrive as different values rather than as two wordings.
    #[test]
    fn a_game_that_wants_asking_about_is_not_a_failed_download() {
        let mut steam = Steam::settled();
        steam.account = Some("someone".to_string());
        steam.games = vec![Game::invented(504230, "Celeste".to_string(), false)];

        steam.apply(Event::Installing {
            app_id: 504230,
            done: 0,
            total: 0,
            live: None,
        });
        let asked = steam.apply(Event::InstallFailed {
            app_id: 504230,
            why: Stopped::Asks("an agreement to accept".to_string()),
        });
        assert!(
            !steam.is_fetching(504230),
            "the row stops counting either way"
        );
        assert_eq!(
            asked.installed,
            vec![Ended::Failed {
                app_id: 504230,
                why: Stopped::Asks("an agreement to accept".to_string())
            }]
        );
    }

    /// The rows the column is built from: none at all when nobody is signed
    /// in, which is what takes the column off the bar.
    /// A library with these titles in it, signed in, in the order Steam sends
    /// one: installed first, each half by name.
    fn library(games: &[(u32, &str, bool)]) -> Steam {
        let mut steam = Steam::settled();
        steam.account = Some("someone".to_string());
        steam.games = lxb_steam::library::sorted(
            games
                .iter()
                .map(|(app_id, name, installed)| {
                    Game::invented(*app_id, (*name).to_string(), *installed)
                })
                .collect(),
        );
        steam
    }

    /// A loading screen standing on a press of `app_id`, handed to the client.
    fn loading(app_id: u32, now: Instant) -> crate::launch::Launch {
        let mut splash = crate::launch::Launch::new(
            "Counter-Strike 2".to_string(),
            None,
            crate::Display::for_a_test(0),
            [0.0; 4],
            None,
            now,
            crate::launch::Before {
                windows: &[],
                foreground: "",
            },
        )
        .through_steam(app_id);
        splash.now_starting_through_steam(now);
        splash
    }

    /// The step the client's launch reports, as the watcher hands it on.
    fn launch_step(task: &str, how_far: Option<u32>) -> crate::launch::Step {
        crate::launch::Step {
            doing: crate::launch::words_for(task).expect("a step the shell names"),
            how_far,
            action_id: None,
        }
    }

    /// One frame of `Shell::sync_the_launch_downloads` for one loading
    /// screen: the same two calls, and whether the press is made again.
    fn a_frame(steam: &Steam, splash: &mut crate::launch::Launch, now: Instant) -> bool {
        let work = steam.what_a_press_waits_on(
            splash.game().expect("a game's loading screen"),
            splash.step(),
            splash.will_ask_again(),
        );
        splash.follow_the_work(now, work)
    }

    /// **Counter-Strike 2 on 2026-09-26, frame by frame**, off the shell's
    /// journal and Valve's content log together. The press was refused with
    /// `AppError_19` at 00:32:25, the moment the shader cache it was fetching
    /// committed; the client then ran the game's update, which finished at
    /// 00:32:32; the library was read again at 00:32:33. The press has to be
    /// made again then — not before, into the middle of the update, and not
    /// never, which is what the screen did.
    #[test]
    fn a_refused_press_is_made_again_when_the_update_it_was_refused_over_ends() {
        use lxb_steam::client::InHand;
        use lxb_steam::library::Standing;

        let t0 = Instant::now();
        let at = |seconds: u64| t0 + Duration::from_secs(seconds);
        let mut steam = library(&[(730, "Counter-Strike 2", true)]);
        steam.driving = true;
        steam.client_running = true;
        let installed = steam.games[0].clone();
        assert_eq!(installed.standing, Standing::Ready);
        // What the manifest said as the press was made: on the disk, and not
        // to be started until Steam has fetched something.
        steam.games[0].update_required = true;
        let mut splash = loading(730, t0);

        // 00:32:16 — the launch is on DownloadingDepots, which was the shader
        // cache arriving.
        steam.valve_is_fetching_shaders.insert(730);
        splash.working_on(Some(launch_step("DownloadingDepots", None)));
        for second in 1..=9 {
            assert!(!a_frame(&steam, &mut splash, at(second)));
        }
        assert_eq!(splash.said(), Some("Downloading content"));
        assert!(splash.is_fetching());

        // 00:32:25 — the shader cache commits, the update starts, and the
        // client gives up on the launch. What `Shell::steam_would_not_start_it`
        // does to the screen:
        steam.valve_is_fetching_shaders.clear();
        steam.valve_is_doing.insert(730, InHand::Working);
        splash.the_launch_was_refused();
        assert!(splash.ask_again_when_the_work_ends());

        // 00:32:27 to 00:32:31 — the update runs; the manifest has not said so
        // yet, then does.
        for second in 11..=13 {
            assert!(
                !a_frame(&steam, &mut splash, at(second)),
                "not into the middle of the update the press was refused over"
            );
        }
        assert_eq!(splash.said(), Some("Updating"));
        steam.games[0] = steam.games[0].clone().doing(Standing::Updating);
        for second in 14..=16 {
            assert!(!a_frame(&steam, &mut splash, at(second)));
        }
        assert!(splash.is_fetching());

        // 00:32:32 — `App update changed : None`. The client has let go of it,
        // and the library in memory still carries the manifest from before.
        steam.valve_is_doing.clear();
        assert!(
            !a_frame(&steam, &mut splash, at(17)),
            "the manifest still says it is being updated"
        );

        // 00:32:33 — the library is read again: Fully Installed, and nothing
        // required of Steam before it starts.
        steam.games[0] = installed;
        assert!(
            a_frame(&steam, &mut splash, at(18)),
            "this is the moment the game can be started, and the press is made"
        );
        assert!(!splash.is_fetching());
        assert!(!a_frame(&steam, &mut splash, at(19)), "and it is made once");
    }

    /// **What the screen did on the 26th**, kept as the same frames with the
    /// refusal's step left standing: the update over, nothing in hand, the
    /// manifest settled — and the loading screen still reading work, for as
    /// long as a client runs. It is here so the rule it breaks cannot be undone
    /// without a test saying which session that was.
    #[test]
    fn a_step_left_behind_by_a_refusal_would_hold_the_press_for_ever() {
        let t0 = Instant::now();
        let mut steam = library(&[(730, "Counter-Strike 2", true)]);
        steam.driving = true;
        steam.client_running = true;
        let mut splash = loading(730, t0);
        splash.working_on(Some(launch_step("DownloadingDepots", Some(2))));
        // Refused, and the step not forgotten — the defect.
        assert!(splash.ask_again_when_the_work_ends());

        // Ten minutes of frames over a game that is ready.
        for second in 0..600 {
            assert!(!a_frame(
                &steam,
                &mut splash,
                t0 + Duration::from_secs(second)
            ));
        }
        assert_eq!(splash.said(), Some("Downloading content · 2%"));

        // And forgetting it is the whole of the difference.
        splash.the_launch_was_refused();
        assert!(a_frame(&steam, &mut splash, t0 + Duration::from_secs(600)));
    }

    /// A press refused over a shader cache alone waits for the shader cache,
    /// and nothing else holds a loading screen on one.
    ///
    /// The first of the two refusals on 2026-09-26 came at 00:31:29, as a
    /// shader cache committed and a second one started — work
    /// `Shell::steam_would_not_start_it` counts as being in front of the press.
    /// The manifest carried the update then; this is the same refusal where it
    /// does not, and the in-hand job is all there is to wait on.
    #[test]
    fn a_press_refused_over_shaders_waits_for_the_shaders_and_only_then() {
        let t0 = Instant::now();
        let at = |seconds: u64| t0 + Duration::from_secs(seconds);
        let mut steam = library(&[(730, "Counter-Strike 2", true)]);
        steam.driving = true;
        steam.client_running = true;
        steam.valve_is_fetching_shaders.insert(730);

        // An ordinary launch is **not** held by a shader cache: Steam fetches
        // them while a game is played, and a loading screen that waited on one
        // would wait on a game that is already open behind it.
        let mut ordinary = loading(730, t0);
        assert!(!a_frame(&steam, &mut ordinary, at(1)));
        assert!(!ordinary.is_fetching(), "nothing held it");

        // A refused one is.
        let mut refused = loading(730, t0);
        refused.working_on(Some(launch_step("DownloadingDepots", None)));
        refused.the_launch_was_refused();
        assert!(refused.ask_again_when_the_work_ends());
        for second in 1..=30 {
            assert!(!a_frame(&steam, &mut refused, at(second)));
        }
        assert!(refused.is_fetching());
        assert_eq!(
            refused.said(),
            Some("Downloading content"),
            "said as the client's own launch said it"
        );

        // `Shader update changed : None`.
        steam.valve_is_fetching_shaders.clear();
        assert!(a_frame(&steam, &mut refused, at(31)));

        // And a client that goes away takes all of it with it: the step and
        // the job are both things it said, and it is not there to be doing
        // either.
        let mut stranded = loading(730, t0);
        stranded.working_on(Some(launch_step("DownloadingDepots", None)));
        assert!(stranded.ask_again_when_the_work_ends());
        steam.valve_is_fetching_shaders.insert(730);
        steam.client_running = false;
        assert!(a_frame(&steam, &mut stranded, at(32)));
    }

    /// A row whose manifest says Steam is in the middle of something says so
    /// only while there is a Steam to be in the middle of it.
    ///
    /// The defect this is about: a manifest is a record of what Valve's client
    /// *was* doing and keeps saying it for as long as nobody starts Steam
    /// again. A session that came up with the client not started — Settings >
    /// Games > Steam > Start with the shell, off — drew "Updating" over a game
    /// nothing was fetching, for the whole of that session, and pressing it
    /// offered to repair a copy with nothing wrong with it.
    #[test]
    fn work_nothing_is_doing_says_it_is_waiting() {
        let mut steam = library(&[(1, "A Game", true)]);
        steam.driving = true;
        steam.games[0] = steam.games[0]
            .clone()
            .doing(lxb_steam::library::Standing::Updating);
        steam.games[0].to_download = 1_300_000_000;
        steam.games[0].downloaded = steam.games[0].to_download / 3;

        // No client running: nothing is fetching this, and the row says so
        // without throwing away how far it had got.
        let note = |steam: &Steam| {
            steam
                .rows()
                .into_iter()
                .find_map(|row| row.game().map(|game| game.note.clone()))
                .expect("the library has a row")
        };
        assert_eq!(note(&steam), "Waiting for Steam · 33% of 1.3 GB");
        assert!(the_game(&steam).waiting_for_steam);

        // One on its way up because this session asked for it: something *is*
        // happening, and a press that started Steam must not go on reading as
        // a press that did nothing.
        steam.apply(client_said(lxb_steam::ClientReport::Waking));
        assert_eq!(note(&steam), "Starting Steam…");

        // And once it is up, the manifest is describing something that is
        // actually happening again.
        steam.apply(client_said(lxb_steam::ClientReport::Ready));
        assert_eq!(note(&steam), "Updating · 33% of 1.3 GB");
        assert!(!the_game(&steam).waiting_for_steam);
    }

    /// An update Steam is quietly getting on with is work in flight, whether
    /// or not the manifest has admitted to it.
    ///
    /// The manifest carries a `TargetBuildID` that is not the build on the
    /// disk, and no working bit — which on this machine lasted ten minutes and
    /// thirty-nine seconds while Steam verified the game.
    /// [`Steam::quietly_working_on_it`] is what sees that stretch, and both the row
    /// and the loading screen are drawn from it.
    #[test]
    fn an_update_with_no_working_bit_is_still_an_update() {
        let mut steam = library(&[(1, "A Game", true)]);
        steam.driving = true;
        steam.games[0].update_outstanding = true;

        let note = |steam: &Steam| {
            steam
                .rows()
                .into_iter()
                .find_map(|row| row.game().map(|game| game.note.clone()))
                .expect("the library has a row")
        };

        // No client: nothing is doing this and nothing is going to until
        // somebody starts Steam, which is what the press is for. The row is
        // about the game, and the game is here.
        assert!(!steam.quietly_working_on_it(1).is_some());
        assert_eq!(note(&steam), "Installed · 1.0 GB");
        assert!(
            !the_game(&steam).waiting_for_steam,
            "an update nobody has started is not work that has stopped"
        );

        // A client running: the same manifest, and now it means Steam is in
        // the middle of it. The row says so — this once said "Installed", which
        // is what the user reported — and the press still opens the loading
        // screen that waits it out. See
        // [`a_game_steam_is_quietly_updating_does_not_read_as_one_ready_to_play`].
        steam.apply(client_said(lxb_steam::ClientReport::Ready));
        assert!(steam.quietly_working_on_it(1).is_some());
        assert_eq!(note(&steam), "Updating");

        // And once the manifest admits to it, this stops being the thing that
        // says so: the standing does, and it carries the percentage.
        steam.games[0] = steam.games[0]
            .clone()
            .doing(lxb_steam::library::Standing::Updating);
        assert!(
            !steam.quietly_working_on_it(1).is_some(),
            "an update that is counting bytes is not a quiet one"
        );

        // Nor is one that has stopped. A paused update has the same two marks
        // — a build outstanding, and no working bit — and means the opposite:
        // nothing will happen until somebody says so, and a loading screen
        // waiting it out would wait for ever.
        steam.games[0] = steam.games[0]
            .clone()
            .doing(lxb_steam::library::Standing::UpdatePaused);
        assert!(
            !steam.quietly_working_on_it(1).is_some(),
            "an update that has stopped is not one to wait out"
        );
    }

    /// A percentage is a reading of the job in hand, and a check has none.
    ///
    /// **Both halves of this were measured on the machine, not reasoned out**,
    /// and the second was caught in a screenshot of the shell running against
    /// the real library on 2026-09-04.
    ///
    /// Steam counts every job in the manifest and admits to almost none of
    /// them in `StateFlags`, so the bytes are the only reading there is — but
    /// they are only a reading of the job *in flight*, and for a game that is
    /// simply installed they are whatever the last one left behind. Every
    /// finished game on that disk carries the two exactly equal.
    #[test]
    fn a_percentage_is_only_ever_read_off_a_job_that_is_in_hand() {
        use lxb_steam::client::InHand;

        let mut steam = library(&[(1, "A Game", true)]);
        steam.driving = true;
        steam.apply(client_said(lxb_steam::ClientReport::Ready));

        let at = |steam: &mut Steam, downloaded, to_download| {
            steam.games[0].downloaded = downloaded;
            steam.games[0].to_download = to_download;
        };

        // Street Fighter 6's shader job, watched second by second while it ran:
        // `StateFlags` read 4 — Fully Installed — from the first byte to the
        // last, so the standing is `Ready` and nothing else in the shell would
        // read these two numbers at all.
        at(&mut steam, 32_537_312, 1_439_173_440);
        assert_eq!(
            steam.how_far_along(1),
            None,
            "nothing has the game in hand, so the pair is the last job's"
        );

        steam.valve_is_fetching_shaders.insert(1);
        assert_eq!(
            steam.how_far_along_percent(1),
            Some(2),
            "and with the client on it, the same numbers are the job's own"
        );
        at(&mut steam, 326_650_976, 703_190_272);
        assert_eq!(steam.how_far_along_percent(1), Some(46));

        // The job's last line, and the state every finished game sits in
        // afterwards. A full bar under work in flight has stopped meaning
        // anything, and it would stand there until the card went.
        at(&mut steam, 190_143_904, 190_143_904);
        assert_eq!(steam.how_far_along(1), None);

        // **A check is not measured in downloaded bytes**, and this is the one
        // the screenshot caught. A validate of Progressbar95 over a manifest
        // reading `261296/261296` drew a card correctly — and without this rule
        // it would have drawn a *full bar* on it, one second into a check of a
        // 175 MB game.
        steam.valve_is_fetching_shaders.clear();
        at(&mut steam, 261_296, 261_296);
        steam.valve_is_doing.insert(1, InHand::Checking);
        assert_eq!(steam.how_far_along(1), None);
        at(&mut steam, 100, 200);
        assert_eq!(
            steam.how_far_along(1),
            None,
            "a check reads the disk back; these are the previous download's"
        );
        assert_eq!(
            steam.what_the_client_is_getting_on_with().map(|it| it.verb),
            Some(CHECKING),
            "it still gets a card — it is minutes of work — with an empty groove"
        );

        // And an update the manifest has not admitted to is counted.
        steam.valve_is_doing.insert(1, InHand::Working);
        assert_eq!(steam.how_far_along_percent(1), Some(50));
        assert_eq!(
            steam.what_the_client_is_getting_on_with().map(|it| it.verb),
            Some(UPDATING)
        );

        // The manifest's own answer wins wherever it has one: two sources
        // agreeing is one source, and the standing is the more particular.
        steam.games[0] = steam.games[0]
            .clone()
            .doing(lxb_steam::library::Standing::Updating);
        at(&mut steam, 25, 100);
        assert_eq!(steam.how_far_along_percent(1), Some(25));
        assert_eq!(
            steam.what_the_client_is_getting_on_with(),
            None,
            "and a manifest that is moving is the download card's, not this one's"
        );
    }

    /// A copy Steam is repairing is a press to wait through, not a signpost to
    /// Steam.
    ///
    /// **Reported from use on 2026-09-04**: *"There's now information that CS
    /// needs to be repaired with Steam instead of proper Splash Screen that
    /// could be controlled with the shell."* The press on Counter-Strike 2 —
    /// `StateFlags 1158`, which is installed, update required, files corrupt,
    /// update started — answered with a panel offering *Repair with Steam*,
    /// which opens Valve's own window: a mouse, on a session driven with a
    /// controller.
    ///
    /// Steam repairs a copy as part of starting it. One second after such a
    /// press this machine wrote `App update changed : Running Update,Verifying
    /// Installed,`. So the press is the client's to answer, and this is the
    /// line the loading screen says while it does — the third thing this
    /// function answers that the row deliberately does not.
    #[test]
    fn a_copy_being_repaired_is_a_wait_the_loading_screen_can_say() {
        use lxb_steam::library::Standing;

        let mut steam = library(&[(1, "A Game", true)]);
        steam.driving = true;
        steam.games[0] = steam.games[0].clone().doing(Standing::Broken);
        steam.apply(client_said(lxb_steam::ClientReport::Ready));

        // Nothing has picked it up yet. A broken copy nobody is mending is a
        // broken copy, and a loading screen must not wait one out for ever.
        assert_eq!(steam.quietly_working_on_it(1), None);
        assert!(!steam.the_client_has_it_in_hand(1));

        // And the client takes it, which is what a press makes it do.
        steam
            .valve_is_doing
            .insert(1, lxb_steam::client::InHand::Checking);
        assert!(steam.the_client_has_it_in_hand(1));
        assert_eq!(
            steam.quietly_working_on_it(1),
            Some(Standing::Validating),
            "the word Steam's own log uses, and the word the splash says"
        );
        assert_eq!(
            steam
                .what_a_press_is_waiting_on(1)
                .map(|coming| coming.verb),
            Some(CHECKING),
            "and the card in the guide says the same thing, not a second one"
        );

        // **The row is untouched**, which is the whole reason this is asked
        // here rather than with the rest. The copy on the disk really is
        // broken and the row really should say so; what changes is only what a
        // press does about it.
        assert_eq!(the_game(&steam).standing, Standing::Broken);
        assert_eq!(the_game(&steam).note, "Needs repairing");
        assert!(
            !the_game(&steam).installed,
            "it is not startable, and the cover stays grey"
        );
        assert!(
            the_game(&steam).the_client_can_put_it_right(),
            "and the press is still handed to the client"
        );

        // A shader cache counts too. It is none of a row's business and it is
        // still minutes between a press and a window.
        steam.valve_is_doing.remove(&1);
        assert!(!steam.the_client_has_it_in_hand(1));
        steam.valve_is_fetching_shaders.insert(1);
        assert!(steam.the_client_has_it_in_hand(1));

        // And a session that is not driving Valve's client reports nothing
        // about the machine it happens to be running on.
        steam.driving = false;
        assert!(!steam.the_client_has_it_in_hand(1));
        assert_eq!(steam.quietly_working_on_it(1), None);
    }

    /// A game Steam will not start until it has updated it is not a game whose
    /// window is about to appear.
    ///
    /// **Reported off this machine on 2026-09-03, and it had been getting
    /// worse each time it was tried.** Counter-Strike 2 sat at `StateFlags 6`
    /// with `TargetBuildID 0`, `ScheduledAutoUpdate 0` and `UpdateResult 4`, so
    /// every source above was silent: no build differed, nothing was in the
    /// diary, and the last attempt had ended badly — which is read as work that
    /// is *not* in flight. The row said "Installed", the press opened a loading
    /// screen saying "Starting the game", Steam went off to fetch five
    /// gigabytes, and a minute later the shell said the game had not started
    /// and shut the client down on top of the download. That is what wrote
    /// `UpdateResult 4`, and Steam then put the next attempt off further each
    /// time: three hours, then three days, in the client's own log.
    ///
    /// The bit that says all this is the one the manifest has carried
    /// throughout, and the shell had never read it. See
    /// [`lxb_steam::library::Game::update_required`].
    #[test]
    fn a_game_steam_must_update_before_it_starts_is_not_one_that_is_starting() {
        let mut steam = library(&[(1, "A Game", true)]);
        steam.driving = true;
        // The manifest as it was on the disk: the bit set, and every other
        // source of an answer saying nothing.
        steam.games[0].update_required = true;
        steam.games[0].update_outstanding = false;
        steam.games[0].scheduled_for = 0;
        steam.games[0].last_result = 4;

        // No client, so nothing is doing it and nothing is going to until the
        // press starts one — which the press does.
        assert_eq!(steam.quietly_working_on_it(1), None);

        steam.apply(client_said(lxb_steam::ClientReport::Ready));
        assert_eq!(
            steam.quietly_working_on_it(1),
            Some(lxb_steam::library::Standing::Updating),
            "Steam has something to fetch before it will open a window"
        );

        // **And the row is untouched**, which is the whole reason this is
        // asked here rather than with the rest. A game with an update waiting
        // is on the disk and starts; a row that greyed it and said "Updating"
        // is the regression the build numbers were narrowed to avoid, and it
        // cost two days of a game.
        assert_eq!(the_game(&steam).note, "Installed · 1.0 GB");
        assert!(the_game(&steam).installed, "and it can still be pressed");
        assert!(!the_game(&steam).updating);

        // Only over a copy that is otherwise ready. A paused update carries the
        // same bit and means the opposite — nothing will happen until somebody
        // says so — and a loading screen waiting that out would wait for ever.
        steam.games[0] = steam.games[0]
            .clone()
            .doing(lxb_steam::library::Standing::UpdatePaused);
        steam.games[0].update_required = true;
        assert_eq!(steam.quietly_working_on_it(1), None);

        // And a manifest without the bit is a game that starts, which is nearly
        // all of them.
        steam.games[0] = steam.games[0]
            .clone()
            .doing(lxb_steam::library::Standing::Ready);
        steam.games[0].update_required = false;
        assert_eq!(steam.quietly_working_on_it(1), None);
    }

    /// A press that is waiting gets a card for what Steam is actually doing,
    /// including the one job no row will ever speak about.
    ///
    /// **The five gigabytes nobody could see.** Counter-Strike 2 on this
    /// machine, 2026-09-03: the press woke the client and the client went off
    /// to fetch a shader cache — `AppID 730 Shader update changed : Running
    /// Update,Downloading,Staging,` against `update started : download
    /// 0/5191457952` — while the game's own update waited behind it. The row
    /// said "Installed", the guide's corner was empty, and the only thing on
    /// the screen was a loading screen counting down to a panel about a game
    /// that had not started.
    ///
    /// A shader cache is still not a row's business — the game is on the disk
    /// and plays perfectly while Steam fetches one — which is why this is asked
    /// of a press rather than of the library, and why the word for it is
    /// neither Downloading nor Updating.
    #[test]
    fn a_press_that_is_waiting_says_what_it_is_waiting_on() {
        use lxb_steam::client::InHand;

        let mut steam = library(&[(1, "A Game", true)]);
        steam.driving = true;
        steam.valve_is_fetching_shaders = BTreeSet::from([1]);

        // No client, so nothing is fetching anything, whatever a stale reading
        // of a log says. The same rule the answer beside it is under.
        assert_eq!(steam.what_a_press_is_waiting_on(1), None);

        steam.apply(client_said(lxb_steam::ClientReport::Ready));
        let card = steam.what_a_press_is_waiting_on(1).expect("a card");
        assert_eq!(card.said(), "Preparing A Game");
        assert_eq!(
            card.share, None,
            "nothing on this disk counts a shader job, and a nought is not a reading"
        );
        assert!(
            !card.a_download,
            "so that its ending is not announced as a game that has arrived"
        );

        // And it is not the download card. That one is about bytes of the game
        // arriving, and no manifest here says any are.
        assert_eq!(steam.downloading(), None);
        assert!(the_game(&steam).installed, "the row is untouched");
        assert_eq!(the_game(&steam).note, "Installed · 1.0 GB");

        // A game nobody is fetching anything for has nothing to say.
        assert_eq!(steam.what_a_press_is_waiting_on(2), None);

        // And the two the loading screen already knew about, in the loading
        // screen's own words: the card and the splash are one moment drawn in
        // two places, and two words for it would be two answers.
        steam.valve_is_doing = BTreeMap::from([(1, InHand::Working)]);
        assert_eq!(
            steam.what_a_press_is_waiting_on(1).expect("a card").said(),
            "Updating A Game"
        );
        steam.valve_is_doing = BTreeMap::from([(1, InHand::Checking)]);
        assert_eq!(
            steam.what_a_press_is_waiting_on(1).expect("a card").said(),
            "Checking A Game",
            "a check on a large game is twenty minutes and is not a download"
        );
    }

    /// An update Steam has put in the diary is not an update Steam is doing.
    ///
    /// **Reported off this machine, and it cost two days of a game.**
    /// `TargetBuildID != buildid` is true in three situations and the first cut
    /// of this read all three as work in flight: Steam working silently, Steam
    /// having scheduled the work for later, and Steam having tried it and
    /// stopped. Counter-Strike 2 on 2026-09-02 — `StateFlags 6`, build
    /// 24828357 on the disk against 24916958 wanted, `ScheduledAutoUpdate` for
    /// two in the morning two days out — and with any client running the shell
    /// greyed the row, said "Updating", and answered a press with *"Updating /
    /// Open Downloads in Steam"*, for a game that launches perfectly in Steam.
    ///
    /// Proton 11.0 and Proton Hotfix were in the same state, hidden from the
    /// column as tools; the same manifest on the same disk still says so.
    #[test]
    fn an_update_steam_has_put_off_is_not_one_to_wait_for() {
        // The appointment itself, either side of the moment, without waiting
        // for one: zero is not a time and a time that has been and gone is not
        // an appointment, which is the shape most manifests on a real disk are
        // in — Steam leaves the old number written after it has kept it.
        let game = Game::invented(1, "A Game".to_string(), true);
        assert!(!update_is_deferred(&game, 1_788_000_000));
        let mut booked = game.clone();
        booked.scheduled_for = 1_788_481_228;
        assert!(update_is_deferred(&booked, 1_788_000_000));
        assert!(
            !update_is_deferred(&booked, 1_788_481_228),
            "an appointment kept is not one outstanding"
        );
        assert!(!update_is_deferred(&booked, 1_789_000_000));

        let mut steam = library(&[(1, "A Game", true)]);
        steam.driving = true;
        steam.games[0].update_outstanding = true;
        steam.apply(client_said(lxb_steam::ClientReport::Ready));
        let row = |steam: &Steam| {
            steam
                .rows()
                .into_iter()
                .find_map(|row| row.game().cloned())
                .expect("the library has a row")
        };

        // The silent stretch, which is what this all reads as without the two
        // fields: a client running, an outstanding build, and nothing else on
        // the disk to say which of the three it is.
        assert!(steam.quietly_working_on_it(1).is_some());

        // An appointment in the future. Nothing is happening, the game is on
        // the disk and starts, and the row has no business saying otherwise.
        steam.games[0].scheduled_for = unix_now() + 60 * 60;
        assert!(!steam.quietly_working_on_it(1).is_some());
        assert_eq!(row(&steam).note, "Installed · 1.0 GB");
        assert!(row(&steam).installed, "and it can still be pressed");

        // One that has been and gone says nothing at all, which is the
        // ordinary state of a manifest Steam has updated before.
        steam.games[0].scheduled_for = unix_now() - 60 * 60;
        assert!(steam.quietly_working_on_it(1).is_some());

        // And the attempt that ended badly. 4 was seen on this machine on a
        // branch switch that did not take; nothing here reads the number as an
        // enumeration, only as not-nought — Steam clears it to 0 when it picks
        // the work up again, which is what makes that safe.
        steam.games[0].scheduled_for = 0;
        steam.games[0].last_result = 4;
        assert!(!steam.quietly_working_on_it(1).is_some());
        assert_eq!(row(&steam).note, "Installed · 1.0 GB");
        steam.games[0].last_result = 0;
        assert!(steam.quietly_working_on_it(1).is_some());
    }

    /// A file check is work no manifest describes, so the row says so from the
    /// client's own log or it does not say so at all.
    ///
    /// The half of `c18815d` that was missing: the log was asked whether to
    /// shut the client down and by nothing else, so the bar went on reading
    /// "Installed" over a game Steam was reading back off the disk — and a
    /// press on it handed `rungameid` to a client that queued the launch behind
    /// the check. The splash then spends `STEAM_PATIENCE`, says the game did
    /// not start, and the game starts. Measured here: thirty-six seconds for a
    /// 15 GB game, and it scales with the game.
    #[test]
    fn a_game_being_checked_over_says_so_rather_than_installed() {
        use lxb_steam::client::InHand;
        use lxb_steam::library::Standing;

        let mut steam = library(&[(1, "A Game", true)]);
        steam.driving = true;
        steam.apply(client_said(lxb_steam::ClientReport::Ready));
        let row = |steam: &Steam| {
            steam
                .rows()
                .into_iter()
                .find_map(|row| row.game().cloned())
                .expect("the library has a row")
        };

        // Nothing in hand: a game that is here and starts, which is what the
        // manifest says and is true.
        assert_eq!(row(&steam).note, "Installed · 1.0 GB");
        assert!(row(&steam).installed);
        assert!(steam.nothing_is_under_way());

        // The client says it is reading the disk back. The manifest says
        // exactly what it said a moment ago — `StateFlags 4` — and the row must
        // not.
        steam.valve_is_doing = BTreeMap::from([(1, InHand::Checking)]);
        steam.valve_is_busy = true;
        assert_eq!(steam.quietly_working_on_it(1), Some(Standing::Validating));
        assert_eq!(row(&steam).note, "Checking files");
        assert_eq!(
            row(&steam).standing,
            Standing::Validating,
            "and the menu branches on this, so a check is not offered Verify"
        );
        assert!(
            !row(&steam).installed,
            "the cover greys and the press stops promising a game that will not start"
        );
        assert!(!steam.nothing_is_under_way());

        // And every other phase of a job is an update, which is a different
        // word for a different wait — a check moves no bytes and a download
        // does.
        steam.valve_is_doing = BTreeMap::from([(1, InHand::Working)]);
        assert_eq!(steam.quietly_working_on_it(1), Some(Standing::Updating));
        assert_eq!(row(&steam).note, "Updating");

        // **A shader cache is the third track and reaches neither the word nor
        // the cover.** It is fetched while the game is being played, so a row
        // that greyed for one would be greying a game somebody is in the middle
        // of. It holds the client open all the same — one of them ran for
        // sixty-four minutes on this machine.
        steam.valve_is_doing = BTreeMap::new();
        assert_eq!(steam.quietly_working_on_it(1), None);
        assert_eq!(row(&steam).note, "Installed · 1.0 GB");
        assert!(row(&steam).installed);
        assert!(
            !steam.nothing_is_under_way(),
            "and a client is not shut down under one"
        );
        steam.valve_is_busy = false;
        assert!(steam.nothing_is_under_way());
        steam.valve_is_doing = BTreeMap::from([(1, InHand::Working)]);
        steam.valve_is_busy = true;

        // A log is a record, exactly as a manifest is: with no client running
        // it says what *was* being done. See [`Steam::waiting_for_steam`].
        steam.client_running = false;
        assert_eq!(steam.quietly_working_on_it(1), None);
        assert_eq!(row(&steam).note, "Installed · 1.0 GB");

        // And a fixture library never reports the machine it happens to run on.
        steam.client_running = true;
        steam.driving = false;
        assert_eq!(steam.quietly_working_on_it(1), None);
    }

    /// What holds a client open is everything Steam has in hand, not the one
    /// download that has a card in the guide.
    ///
    /// Both rules that close a client gate on this, so every row below was a
    /// client asked to shut down in the middle of somebody's work. It read
    /// [`Steam::downloading`] — which is the guide's card, and is right to draw
    /// only a real download — where its own doc said *moving*.
    #[test]
    fn a_client_is_not_shut_down_over_work_that_is_not_a_download() {
        use lxb_steam::library::Standing;

        let idle = |standing: Standing| {
            let mut steam = library(&[(1, "A Game", true)]);
            steam.driving = true;
            steam.games[0] = steam.games[0].clone().doing(standing);
            steam.apply(client_said(lxb_steam::ClientReport::Ready));
            steam
        };
        assert!(
            idle(Standing::Ready).nothing_is_under_way(),
            "a library with nothing happening in it"
        );

        // Measured against a real library, and only the middle two of these
        // were counted. A queue is work waiting its turn, a verify on a 139 GB
        // game is most of an hour, and a removal begun in Steam's own window is
        // seen by nothing else here — `removing` is this session's own list.
        for standing in [
            Standing::Queued,
            Standing::Downloading,
            Standing::Updating,
            Standing::Validating,
            Standing::Uninstalling,
        ] {
            assert!(
                !idle(standing).nothing_is_under_way(),
                "the client would have been shut down over {standing:?}"
            );
        }

        // And what has stopped is not work: a paused download waits for
        // somebody, not for Steam, and a copy that needs repairing waits for
        // the same. Holding a client open for either would be holding it open
        // for the session.
        for standing in [Standing::Paused, Standing::UpdatePaused, Standing::Broken] {
            assert!(
                idle(standing).nothing_is_under_way(),
                "the client was held open by {standing:?}, which nothing is doing"
            );
        }

        // The quiet stretch, for **every** app rather than for the pressed one.
        // This is what was missing: the press had its own question about its
        // own game, and a silent Proton update — which is exactly the shape of
        // manifest that says nothing — was a client shut down five seconds in.
        let mut quiet = idle(Standing::Ready);
        quiet.games[0].update_outstanding = true;
        assert!(!quiet.nothing_is_under_way());
        quiet.games[0].update_outstanding = false;
        assert!(quiet.nothing_is_under_way());

        // And the same manifest with an appointment on it is not work, here as
        // everywhere else it is asked.
        quiet.games[0].scheduled_for = unix_now() + 60 * 60;
        assert!(quiet.nothing_is_under_way());
    }

    /// The row says "Updating" while Steam is quietly updating it, and still
    /// answers a press with the loading screen.
    ///
    /// Reported by the user off a real update, and it is the moment the loading
    /// screen is dismissed with Back: the bar comes up and the game reads
    /// "Installed · 1.0 GB · 119 hours played" — a row that looks ready to play
    /// — until Steam writes its first working bit and it turns into "Updating".
    /// Thirty seconds of that on this machine, and it can be ten minutes.
    #[test]
    fn a_game_steam_is_quietly_updating_does_not_read_as_one_ready_to_play() {
        let mut steam = library(&[(1, "A Game", true)]);
        steam.driving = true;
        steam.games[0].update_outstanding = true;
        let row = |steam: &Steam| {
            steam
                .rows()
                .into_iter()
                .find_map(|row| row.game().cloned())
                .expect("the library has a row")
        };

        // No client: nothing is happening to it, it starts, and the row says
        // so. This is the state a press turns into a loading screen.
        assert_eq!(row(&steam).note, "Installed · 1.0 GB");
        assert!(row(&steam).installed);

        // A client running: Steam has it, and the words say so.
        steam.apply(client_said(lxb_steam::ClientReport::Ready));
        assert_eq!(row(&steam).note, "Updating");

        // **And it stops reading as a game that is ready to play.** `installed`
        // is what drains the colour out of the cover, so a row saying
        // "Updating" beside a cover still lit is the row saying two things —
        // which is what the user saw, a second of it before Steam's working bit
        // landed and greyed it.
        assert!(!row(&steam).installed, "the cover is still lit under it");
        assert!(row(&steam).updating);

        // The loading screen is not lost with it. That one belongs to the press
        // made while no client is running, and this is false there — so the row
        // is `installed`, and the press opens the splash that waits the update
        // out. See `a_download_the_launch_is_waiting_on_is_not_a_game_that_never_started`.
        steam.client_running = false;
        assert!(!steam.quietly_working_on_it(1).is_some());
        assert!(row(&steam).installed);
    }

    /// A session that is not driving Valve's client never says a word about
    /// whether one is running.
    ///
    /// `--debug-steam-library` invents a library and has no client of its own;
    /// a fixture row reading "Waiting for Steam" would be the shell reporting
    /// the machine it happens to be running on into a picture of an invented
    /// one. See [`Steam::note_whether_a_client_is_running`].
    #[test]
    fn an_invented_library_is_never_waiting_for_steam() {
        let mut steam = library(&[(1, "A Game", true)]);
        steam.games[0] = steam.games[0]
            .clone()
            .doing(lxb_steam::library::Standing::Updating);
        assert!(!steam.driving);
        assert!(!the_game(&steam).waiting_for_steam);
        assert_eq!(the_game(&steam).note, "Updating");
    }

    /// The guide's download card is about a download that is happening.
    ///
    /// The same fact as the row above, in the second place it is drawn: a card
    /// built from a manifest no client is acting on is a frozen bar in the
    /// corner of the guide, sitting at whatever percentage the download had
    /// reached when Steam was last running.
    #[test]
    fn the_download_card_is_not_drawn_over_work_nothing_is_doing() {
        let mut steam = library(&[(1, "A Game", true)]);
        steam.driving = true;
        steam.games[0] = steam.games[0]
            .clone()
            .doing(lxb_steam::library::Standing::Updating);
        steam.games[0].to_download = 1_300_000_000;
        steam.games[0].downloaded = steam.games[0].to_download / 3;
        assert!(steam.downloading().is_none(), "nothing is fetching it");

        // And a client that is up puts it back, because now something is.
        steam.apply(client_said(lxb_steam::ClientReport::Ready));
        let coming = steam.downloading().expect("a download to draw");
        assert_eq!(coming.steam_app_id(), Some(1));
        assert!(coming
            .share
            .is_some_and(|share| (share - 0.333).abs() < 0.01));
    }

    /// The one game a library of one has, as the bar holds it.
    fn the_game(steam: &Steam) -> crate::apps::Game {
        steam
            .rows()
            .into_iter()
            .find_map(|row| row.game().cloned())
            .expect("the library has a row")
    }

    /// What a folder row opens onto, by title.
    fn inside(entry: &crate::apps::Entry) -> Vec<&str> {
        entry
            .entries()
            .expect("that row opens onto nothing")
            .iter()
            .map(crate::apps::Entry::title)
            .collect()
    }

    /// The field and the index stand over the column rather than in it, so a
    /// display walking into somebody's library arrives on a game.
    ///
    /// In that order, which is the order somebody arriving from below meets
    /// them in: the letters are the way into the list under them, and the field
    /// is the way into everything including the letters.
    #[test]
    fn trophies_sort_and_search_are_independent_of_steam() {
        use crate::trophies::Sort as Trophies;
        use lxb_steam::library::Sort;
        let mut steam = library(&[(1, "Alpha", false), (2, "Zulu", true)]);
        steam.trophies.account(Some(1));
        let mut browser = crate::trophies::Browser {
            sort: Trophies::NameDescending,
            search: String::new(),
        };
        steam.set_sort(Sort::NameAscending);
        let rows = |browser: &crate::trophies::Browser, steam: &Steam| {
            browser.rows(steam.trophy_games(), None, |id| {
                steam.game(id).map(|game| game.installed)
            })
        };
        assert_eq!(rows(&browser, &steam)[2].title(), "Zulu");
        steam.set_search("Alpha");
        assert_eq!(rows(&browser, &steam).len(), 4);
        browser.search = "Zulu".into();
        assert_eq!(steam.rows().last().unwrap().title(), "Alpha");
        assert_eq!(rows(&browser, &steam).last().unwrap().title(), "Zulu");
        steam.set_sort(Sort::InstalledFirst);
        assert_eq!(browser.sort, Trophies::NameDescending);
        browser.sort = Trophies::NameAscending;
        assert_eq!(steam.sort(), Sort::InstalledFirst);
    }

    #[test]
    fn the_library_carries_its_index_above_the_first_game() {
        let steam = library(&[(1, "Aeonic", false), (2, "Zenith", true)]);
        let rows = steam.rows();

        assert_eq!(
            crate::apps::head_rows(&rows),
            2,
            "two rows stand over the column"
        );
        assert_eq!(rows[0].title(), "Search");
        assert_eq!(rows[0].comment(), Some("Search this library by name"));
        assert_eq!(rows[1].title(), "Alphabetical");
        assert!(rows[0].over_the_list() && rows[1].over_the_list());
        assert_eq!(
            rows[2..]
                .iter()
                .map(crate::apps::Entry::title)
                .collect::<Vec<_>>(),
            ["Zenith", "Aeonic"],
            "and everything under them is a game, in the column's own order"
        );
    }

    /// A search narrows everything under the field, the index included — it is
    /// the same library seen another way, and one that went on offering letters
    /// full of games the column no longer shows would be an index that lied.
    #[test]
    fn a_search_narrows_the_column_and_the_index_with_it() {
        let mut steam = library(&[
            (1, "Portal", true),
            (2, "Portal 2", false),
            (3, "Celeste", true),
        ]);
        assert!(steam.set_search("portal"));

        let rows = steam.rows();
        assert_eq!(
            crate::apps::head_rows(&rows),
            3,
            "the field, the row that empties it, and the index"
        );
        assert_eq!(rows[0].title(), "portal", "the field says what was typed");
        assert_eq!(rows[0].comment(), Some("2 of 3 games match"));
        assert_eq!(rows[1].title(), "Clear search");
        assert_eq!(rows[1].comment(), Some("Show all 3 games"));
        assert_eq!(
            rows[3..]
                .iter()
                .map(crate::apps::Entry::title)
                .collect::<Vec<_>>(),
            ["Portal", "Portal 2"],
            "and Celeste is not in the column"
        );

        let letters = rows[2].entries().expect("the index opens onto letters");
        assert_eq!(
            letters
                .iter()
                .map(crate::apps::Entry::icon)
                .collect::<Vec<_>>(),
            [Some("lxb:letter-p")],
            "nor under C in the index"
        );
        assert_eq!(inside(&letters[0]), ["Portal", "Portal 2"]);
    }

    /// What is looked for is folded the way a name is, so somebody typing what
    /// they see on the screen finds it.
    #[test]
    fn a_search_is_neither_case_nor_the_spaces_round_it() {
        let mut steam = library(&[(1, "The Witness", true)]);
        assert!(steam.set_search("  WITNESS "));
        let rows = steam.rows();
        assert_eq!(rows[3].title(), "The Witness");

        // And a query of nothing but spaces asks nothing, so it keeps nothing
        // out — a field with a space in it must not answer with the games whose
        // names have one. The row that empties the field is still offered,
        // because there is still something in it to empty.
        assert!(steam.set_search("   "));
        let rows = steam.rows();
        assert_eq!(rows[3].title(), "The Witness");
        assert_eq!(rows[0].comment(), Some("1 of 1 game matches"));
    }

    /// A search that finds nothing keeps the two rows that say why. A column
    /// left empty would leave somebody looking at nothing with no way back to
    /// their own library but the one they could not see.
    #[test]
    fn a_search_that_finds_nothing_still_says_so() {
        let mut steam = library(&[(1, "Portal", true), (2, "Celeste", true)]);
        assert!(steam.set_search("qqq"));

        let rows = steam.rows();
        assert_eq!(rows.len(), 2, "the field and the row that empties it");
        assert_eq!(rows[0].comment(), Some("No games match"));
        assert_eq!(rows[1].title(), "Clear search");
        assert!(
            rows.iter().all(|row| row.title() != "Alphabetical"),
            "and no index over an empty column"
        );
    }

    /// Every game under the heading it starts with, whatever else is not a
    /// heading at all — and inside one, installed first, however the column
    /// itself is listed.
    #[test]
    fn the_index_files_a_game_under_its_heading_and_the_rest_under_a_hash() {
        let mut steam = library(&[
            (1, "Aeonic", false),
            (2, "112 Operator", false),
            (3, "alpha protocol", true),
            (4, "Zenith", true),
            (5, "Портал", false),
        ]);
        // Anything but by name, so that an index which quietly followed the
        // column would come out in the wrong order rather than the same one.
        assert!(steam.set_sort(lxb_steam::library::Sort::LargestFirst));

        let rows = steam.rows();
        let letters = rows[1].entries().expect("the index opens onto letters");

        // The heading is the row's *mark*, cut from the shell's own face.
        assert_eq!(
            letters
                .iter()
                .map(crate::apps::Entry::icon)
                .collect::<Vec<_>>(),
            [
                Some("lxb:letter-hash"),
                Some("lxb:letter-a"),
                Some("lxb:letter-z")
            ],
            "what is not a heading first, then the alphabet"
        );
        // So the words on it are free to say how far it goes, and there is
        // nothing left for a second line to add.
        assert_eq!(
            letters
                .iter()
                .map(crate::apps::Entry::title)
                .collect::<Vec<_>>(),
            ["2 games", "2 games", "1 game"]
        );
        assert!(
            letters
                .iter()
                .all(|row| crate::apps::Entry::comment(row).is_none()),
            "the title is the whole of the row"
        );

        assert_eq!(
            inside(&letters[1]),
            ["alpha protocol", "Aeonic"],
            "installed first inside a letter, then by name"
        );
        assert_eq!(
            inside(&letters[0]),
            ["112 Operator", "Портал"],
            "and a name in another script files with the digits"
        );
    }

    /// The two rows one game has — its own, and the one inside its letter — are
    /// built in the same pass out of the same library, so a download counts up
    /// on both.
    #[test]
    fn a_game_says_the_same_thing_in_the_index_as_it_does_in_the_column() {
        let mut steam = library(&[(504230, "Celeste", false)]);
        steam.apply(Event::Installing {
            app_id: 504230,
            done: 3_000_000_000,
            total: 6_000_000_000,
            live: None,
        });

        let rows = steam.rows();
        let letters = rows[1].entries().expect("the index opens onto letters");
        let inside = letters[0].entries().expect("a letter holds games");
        let column = rows[2].game().expect("the column is games");
        let indexed = inside[0].game().expect("and so is a letter");

        assert_eq!(column.app_id, indexed.app_id);
        assert_eq!(column.note, indexed.note);
        assert!(column.note.starts_with("Installing"), "{}", column.note);
        assert_eq!(column.updating, indexed.updating);
    }

    #[test]
    fn a_signed_out_session_has_no_rows() {
        let mut steam = Steam::settled();
        steam.games = vec![];
        assert!(steam.rows().is_empty());
    }

    /// The chord that reaches Valve's overlay is Shift+Tab, in the kernel's own
    /// numbering, in that order.
    ///
    /// The order is the substance rather than a detail: the modifier has to be
    /// down before the key it modifies arrives, or what the game is handed is a
    /// bare Tab — which in a game is very often something else entirely.
    #[test]
    fn the_overlay_chord_is_shift_then_tab() {
        // `KEY_TAB` and `KEY_LEFTSHIFT` from `input-event-codes.h`.
        assert_eq!(OVERLAY_CHORD, [42, 15]);
        // And the arrows the D-pad sends are written in the same numbering, so
        // one of the two cannot quietly become a keysym.
        assert_eq!(crate::controller::KEY_UP, 103);
    }

    /// The corner of the guide is told about one download, and which one is not
    /// a matter of luck.
    ///
    /// Two halves of the library can know about a download and both are asked:
    /// a job this session started, which has the client's own account of it and
    /// answers before Valve has written a manifest at all, and the disk, which
    /// is the only thing that knows about a download somebody began in Steam's
    /// own window. The first wins where both have something, which is the order
    /// [`Steam::row`] reads them in — the card and the row have to be one fact.
    #[test]
    fn the_card_is_told_about_one_download_and_the_session_s_own_wins() {
        let mut steam = Steam::settled();
        assert_eq!(steam.downloading(), None, "a quiet session shows no card");

        steam.games = vec![
            Game::invented(504230, "Celeste".to_string(), false),
            Game::invented(945360, "Among Us".to_string(), false).coming_down(30),
        ];
        // Off the disk alone: nobody pressed anything in this shell.
        let from_the_disk = steam.downloading().expect("the disk's own download");
        assert_eq!(from_the_disk.steam_app_id(), Some(945360));
        assert_eq!(from_the_disk.said(), "Downloading Among Us");
        assert_eq!(from_the_disk.share, Some(0.3));
        assert!(!from_the_disk.stuck);

        // And now this session presses install on the other one. Steam runs one
        // download at a time, so the card follows the press: it is the one with
        // a percentage that moves, and the one somebody is watching.
        steam.apply(Event::Installing {
            app_id: 504230,
            done: 0,
            total: 0,
            live: Some(lxb_steam::webui::Live {
                app_id: 504230,
                percent: 12,
                bytes_per_second: 4_000_000,
                seconds_left: None,
                moving: true,
            }),
        });
        let mine = steam.downloading().expect("this session's own download");
        assert_eq!(mine.steam_app_id(), Some(504230));
        assert_eq!(mine.said(), "Downloading Celeste");
        assert_eq!(mine.share, Some(0.12));
    }

    /// Bytes arriving over a copy that is already on the machine is an update,
    /// and is called one — here and nowhere else in the shell is it a download.
    /// And a game being taken off the disk never gets a card, whatever else is
    /// true of it: the card is about something arriving.
    #[test]
    fn the_card_says_updating_over_a_copy_that_is_already_here() {
        let mut steam = Steam::settled();
        steam.games = vec![Game::invented(504230, "Celeste".to_string(), true)
            .doing(lxb_steam::library::Standing::Updating)];
        assert_eq!(
            steam.downloading().map(|coming| coming.said()),
            Some("Updating Celeste".to_string())
        );

        // A press this session made says the same thing, from the first frame:
        // the verb is read off the disk, so it does not start out as
        // "Downloading" and correct itself once the manifest catches up.
        steam.apply(Event::Installing {
            app_id: 504230,
            done: 0,
            total: 0,
            live: None,
        });
        assert_eq!(
            steam.downloading().map(|coming| coming.said()),
            Some("Updating Celeste".to_string())
        );

        steam.apply(Event::Uninstalling { app_id: 504230 });
        assert_eq!(steam.downloading(), None, "a removal is not an arrival");
    }

    /// A download waiting its turn, being checked over, or paused has nothing
    /// arriving and so has no card. The one that *is* downloading has it.
    ///
    /// The narrow rule matters because the card would otherwise say
    /// "Downloading" over a game Steam has queued behind another — which is a
    /// card about the wrong game, since the one actually coming down is the
    /// other one.
    #[test]
    fn only_a_download_that_is_actually_arriving_gets_a_card() {
        use lxb_steam::library::Standing;
        let mut steam = Steam::settled();
        for standing in [
            Standing::Queued,
            Standing::Validating,
            Standing::Paused,
            Standing::UpdatePaused,
            Standing::Broken,
            Standing::Ready,
        ] {
            steam.games =
                vec![Game::invented(504230, "Celeste".to_string(), false).doing(standing)];
            assert_eq!(
                steam.downloading(),
                None,
                "{standing:?} is not something arriving"
            );
        }
        steam.games = vec![
            Game::invented(504230, "Celeste".to_string(), false).doing(Standing::Queued),
            Game::invented(945360, "Among Us".to_string(), false).coming_down(60),
        ];
        assert_eq!(
            steam.downloading().and_then(|coming| coming.steam_app_id()),
            Some(945360)
        );
    }

    /// A download that says it is running while nothing arrives keeps its
    /// reading on the card and is marked as stopped, exactly as the row is —
    /// and only a download this session is watching has a clock on it at all.
    #[test]
    fn a_card_carries_the_same_stall_the_row_does() {
        let mut steam = Steam::settled();
        steam.games = vec![Game::invented(504230, "Celeste".to_string(), false)];
        steam.apply(Event::Installing {
            app_id: 504230,
            done: 40,
            total: 100,
            live: None,
        });
        assert_eq!(steam.downloading().map(|coming| coming.stuck), Some(false));

        steam.apply(Event::InstallStuck {
            app_id: 504230,
            quiet_for: std::time::Duration::from_secs(3600),
        });
        let stalled = steam.downloading().expect("the stalled download");
        assert_eq!(stalled.share, Some(0.4), "it keeps what it had reached");
        assert!(stalled.stuck);
    }
}
