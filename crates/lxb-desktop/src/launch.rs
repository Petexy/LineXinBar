//! The splash between pressing A and the application being there.
//!
//! Starting an application is the one thing the shell does that it cannot do
//! instantly: the process has to be forked, linked, its toolkit brought up and
//! its first frame drawn, which is anywhere from a tenth of a second to
//! several. Without something to look at, the bar simply sits there and then
//! the window appears — the press reads as ignored, and the application as
//! having popped out of nowhere.
//!
//! So the shell answers the press itself. A panel grows out of the tile that
//! was chosen, filling the display with that application's icon and name and
//! something that says *working*, and stays there until the window turns up
//! underneath it. Phones have done this for years and it is the same trick:
//! the animation is not decoration, it is the acknowledgement, and the loading
//! happens inside it.
//!
//! A game out of the Steam library is answered differently — its own picture
//! is already behind the display and stays there, and what grows is the title
//! rather than a panel — but the *timing* is the same one, which is the whole
//! reason there is one of these rather than two.
//!
//! This module owns *when*: how far out of its tile the splash is, whether the
//! application has arrived, and when there is nothing left to draw. Where and
//! what it looks like belongs to [`crate::ui`].

use std::path::{Path, PathBuf};
use std::time::Instant;

// The two words the guide's card already uses for work that is not a download,
// taken from where the card's own words are so that a step of a launch and a
// manifest cannot describe one moment in two vocabularies.
use crate::steam::{CHECKING, PREPARING};

/// How long the panel takes to grow out of its tile.
pub const OPEN: f32 = 0.34;

/// How long it holds after the window turns up. A window is mapped when the
/// client commits its first buffer, and plenty of toolkits commit a blank one
/// before they have drawn anything into it; handing over on the instant shows
/// the user that blank frame.
const SETTLE: f32 = 0.12;

/// How long the splash takes to fade off the application behind it.
const HANDOVER: f32 = 0.3;

/// A game does not fade off its window; it dips through black. How long the
/// screen takes to go black, how long it stays there, and how long the game
/// takes to come up out of it.
///
/// Three reasons, and only the last is about how it looks.
///
/// The picture a game's splash stands on is drawn on the *background* layer,
/// under every application window, so the instant the game's own window maps
/// that picture is gone — and a cross-fade would spend its whole length
/// showing a title floating over a game nobody has been shown yet. Black is
/// the one thing the shell can hold over the window while that happens.
///
/// It also buys the game the moment [`SETTLE`] buys an application, and buys
/// it far more cheaply. A window is mapped when its first buffer is committed
/// and a game's first buffer is a long way from its first frame — a black
/// screen, a splash image, an anti-cheat notice — and none of that is worth
/// cutting to. Behind black it costs nothing to wait through.
///
/// And it is what every console does, because it is what film does: two
/// pictures that have nothing to do with each other are not cut between, they
/// are dipped between. The hold is what makes it a dip rather than a flicker,
/// and the way up is slower than the way down because that is the half the
/// eye is actually reading.
const BLACK_IN: f32 = 0.26;
const BLACK_HOLD: f32 = 0.34;
const BLACK_OUT: f32 = 0.42;

/// How long the splash goes on watching after it has faded away.
///
/// It draws nothing in this stretch and costs nothing; what it is for is being
/// able to come *back*. The hand-over is a bet that the window which just
/// appeared is the application, and the bet is sometimes lost — a toolkit that
/// discards its first window, a game that swaps one for a fullscreen one — at
/// which point the display is the bar again with nothing on it. Without this
/// there is nothing left to notice that with: the splash has already been
/// dropped, and the press ends up looking like it failed.
///
/// Longer for a game, because the swap can happen well into loading.
const WATCHING: f32 = 1.5;
const STEAM_WATCHING: f32 = 8.0;

/// How long to wait for a window before concluding that none is coming.
///
/// Generous, because the alternative failure is worse: a splash torn away
/// from an application that was merely slow leaves the user staring at the
/// bar wondering whether their press did anything. A launch that dies is
/// caught by its process exiting rather than by this.
const PATIENCE: f32 = 20.0;

/// How long a game started through Heroic gets to show a window.
///
/// Measured on 2026-09-24: a cold Heroic had Cat Quest launching eleven
/// seconds after the press, first-run Wine prefix included — but that first
/// run also fetched two anti-cheat runtimes by itself, a game may install its
/// redistributables through Proton, and an EA or Ubisoft title opens its own
/// store's window first. Two minutes, on [`STEAM_PATIENCE`]'s argument: the
/// cost of waiting too long is a loading screen somebody watches for a while,
/// and the cost of too short is the shell saying a game did not start and the
/// game then opening over it.
const HEROIC_PATIENCE: f32 = 120.0;

/// And how long for a game started through Valve's client.
///
/// Longer than [`PATIENCE`], because there is no process to watch — the
/// `steam steam://rungameid/…` that carries the request hands it over and
/// exits within milliseconds, so the only sign a game is coming is its window
/// arriving — and because a great deal can happen first: the client checks the
/// installation, applies an update it decided was due, unpacks a shader cache,
/// builds a Proton prefix on the game's first run, and shows an anti-cheat
/// installer. Twenty seconds of that is normal and none of it is failure.
///
/// A minute is the cap, and it is a minute of the *game* rather than of the
/// press: [`Launch::now_starting_through_steam`] restarts the clock at the
/// moment the client is actually asked, so waking and signing in a cold client
/// — most of a minute on its own — is not spent out of this.
///
/// It is a deliberate trade rather than a safe upper bound. The cost of being
/// wrong in this direction is a loading screen somebody waits at for too long;
/// the cost of being wrong in the other is the shell giving up on a game that
/// then opens over the bar anyway, having already said it did not start. A
/// first-run Proton prefix or a large shader cache can outlast this, and when
/// it does the user is told the game did not open and the game opens — which
/// is the failure this number buys, knowingly.
const STEAM_PATIENCE: f32 = 60.0;

/// And how long for the half before it: Valve's client being started and signed
/// in, which is the wait a press pays before the game is so much as asked for.
///
/// Its own number because it is its own wait, and because the one that used to
/// cover both was the shorter of the two. [`STEAM_PATIENCE`] began at the press
/// and ran for a minute; the client is allowed half again as long as that to
/// answer at all — see [`lxb_steam::client::LONGEST_ORDINARY_WAKE`], which is
/// where the wait itself is set. So a cold client reliably outlived the loading
/// screen watching for it: the splash said the game had not started, and then
/// the client came up and the game started, over the panel saying it had not.
///
/// Taken from the client module's own constant plus a few seconds of slack, so
/// that changing the wait cannot leave the patience behind again.
const STEAM_CLIENT_PATIENCE: f32 = lxb_steam::client::LONGEST_ORDINARY_WAKE.as_secs() as f32 + 10.0;

/// The whole of what went wrong, in one line the compiler checks: a wait may
/// not be shorter than the thing it is waiting for. Said here rather than in a
/// test because it is a fact about two constants, and one of them is now read
/// out of another crate — where somebody changing it has no reason to look at
/// this file at all.
const _: () = assert!(STEAM_CLIENT_PATIENCE > STEAM_PATIENCE);

/// How long a window has to have stood before its going means the application
/// leaving rather than a hand-over to the wrong window.
///
/// The two look identical at the instant it happens: a window that was there is
/// not there, and the display has nothing on it. What tells them apart is how
/// long it lasted. **What Valve's client and X11 toolkits throw away, they
/// throw away at once** — a window built and discarded on the way to the real
/// one is gone inside a second, well inside the dip the splash is still
/// playing — and a window somebody looked at and then closed is not that.
///
/// Treating the second as the first is what was reported from a real session:
/// closing a game, from the guide or from the game's own menu, put "Starting
/// the game" back on the screen over the bar the user had just been handed, and
/// then a panel saying Steam had never opened it.
const STAYED: f32 = 2.0;

/// Longer than the dip, deliberately: a window that vanishes while the screen
/// is still black cannot be one anybody has looked at.
const _: () = assert!(STAYED > BLACK_IN + BLACK_HOLD + BLACK_OUT);

/// Why the splash stopped waiting — which decides nothing about the drawing,
/// only what gets said in the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arrival {
    /// A window that was not there before.
    Window,
    /// No new window, but the display's foreground changed: an application
    /// that was already running and raised what it had.
    Raised,
    /// The process is gone without ever showing anything.
    Gone,
    /// Long enough.
    GaveUp,
    /// The application it handed the display over to had really been there,
    /// and has gone again — the user closed it, or it closed itself.
    ///
    /// Not a failure and not a hand-over: it is the launch being over. The
    /// splash ends here with nothing to draw and nothing to say, and the
    /// display goes back to whatever is behind. Told from a window discarded on
    /// the way to the real one by [`STAYED`].
    Left,
}

/// What Valve's client is doing about a game under a launch, as the loading
/// screen needs it.
///
/// The shell's own shape rather than [`lxb_steam::Step`], which carries Valve's
/// name for the task and the numbers that say which press a watcher is
/// speaking about. By the time it reaches here the screen has been found, those
/// are spent, and the task has been through [`words_for`] — so what is left is
/// what goes on the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Step {
    /// What the shell calls this step, in its own voice.
    pub doing: Words,
    /// How far, where the client's interface can be asked. Nothing for a client
    /// that was already running when this session came up — its own log says
    /// *which* step, and only its interface says how far — and nothing for the
    /// steps that do not count themselves.
    pub how_far: Option<u32>,
    /// And the action to answer to be let past. `None` is a wait with nothing
    /// to press — see [`Launch::step_may_be_skipped`].
    pub action_id: Option<u32>,
}

impl Step {
    /// What the loading screen says while this is going on.
    ///
    /// The shell's own words and its own punctuation, like every other line on
    /// that screen. Valve's own are "Processing Vulkan shaders (34%)" and
    /// "Downloading content (19%)"; the API the shaders are compiled against is
    /// not what somebody waiting for a game needs to be told, and the middle
    /// dot is what this shell puts between a state and its reading everywhere
    /// else.
    /// `also` is what the game's own manifest says, for the percentage the
    /// client's interface cannot be asked for.
    ///
    /// **Which is every ordinary press.** The interface is opened only on the
    /// path that needs it — installing — and a game press is a `steam:` URL
    /// over the client's pipe, so a client that can sign itself in exposes
    /// nothing at all. Measured on this machine on 2026-09-04, watching a real
    /// press second by second: the interface answered *nothing* for the whole
    /// of a launch. So the interface was never going to be where a percentage
    /// came from on the screen somebody is actually looking at, and this is.
    pub fn said(&self, also: Option<u32>) -> String {
        match self.how_far().or(also.filter(|_| self.doing.counts)) {
            Some(how_far) => format!("{} · {how_far}%", self.doing.line),
            // Neither half could be asked. Half a sentence is the honest half.
            None => self.doing.line.to_string(),
        }
    }

    /// And how far, as the guide's card wants it.
    pub fn share(&self) -> Option<f32> {
        self.how_far().map(|how_far| how_far as f32 / 100.0)
    }

    /// How far through, where that is a reading and not a number that happens
    /// to be lying about.
    ///
    /// Every game action carries a `strNumDone` and a `strNumTotal` and only
    /// some of the steps mean anything by them; see [`Words::counts`].
    fn how_far(&self) -> Option<u32> {
        self.how_far.filter(|_| self.doing.counts)
    }
}

/// What the shell says about one step of a launch: a line for the loading
/// screen, and a word for the corner of the guide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Words {
    /// The line under the indicator on the loading screen.
    pub line: &'static str,
    /// And the word in front of the game's name on the guide's card, where it
    /// is not simply the game's own. `None` is the game's own content arriving,
    /// which the card already has a word for and picks between Downloading and
    /// Updating by what is on the disk.
    pub verb: Option<&'static str>,
    /// Whether the step counts itself, so that a percentage beside it is a
    /// reading rather than a pair of numbers that happen to be set.
    ///
    /// **Four of them do**, and this is Valve's own list rather than a guess:
    /// its launch dialog formats `#LaunchApp_Action_<task>` with a percentage
    /// for `DownloadingDepots`, `DownloadingWorkshop`, `ProcessingShaderCache`
    /// and `DelayLaunch`, and with no argument at all for the rest. Every game
    /// action carries a `strNumDone` and a `strNumTotal` whatever step it is
    /// on — `RunningInstallScript` puts the script's *name* in the same place
    /// the others put a number — so reading them unconditionally would put a
    /// percentage on the screen that counts nothing.
    pub counts: bool,
}

/// What the shell says while Valve's client is on that step of a launch, and
/// nothing at all for the steps it does not name.
///
/// **The list is Valve's own.** Its client ships a localised string for each of
/// these under `#LaunchApp_Action_<task>` — read out of
/// `~/.local/share/Steam/steamui/localization/` on 2026-09-04 — and the tasks
/// with no string there are the ones its own launch window says nothing about
/// either. Where Valve has a word, so has this; where it has none, the step is
/// over before anybody could read it.
///
/// Four are deliberately left out of a list Valve does have words for.
/// `Starting`, `CreatingProcess` and `WaitingGameWindow` all mean the game
/// itself is opening, which is what this screen says in its own words already
/// and has said since before any of this — and they are the one stretch the
/// patience is *for*, so naming them would stop the clock on the wait it
/// exists to bound. `ShowEula` is an agreement to read, which is a question
/// with a panel of its own; see [`lxb_steam::Asking`].
///
/// The words are the shell's, not translations of Valve's. "Verifying
/// executable" and "Updating executable" are the client's names for what it is
/// doing to itself; somebody waiting for a game is told what is happening to
/// the game.
pub fn words_for(task: &str) -> Option<Words> {
    let (line, verb, counts) = match task {
        // The game's own content arriving, which is the one of these that is a
        // download in the sense the rest of the shell means it — and the one
        // that was reported. Valve: "Downloading content (%1$s%)".
        "DownloadingDepots" => (crate::i18n::text("shell-downloading-content"), None, true),
        // Not the game: items it needs from the Workshop. A card saying
        // "Downloading <game>" over a game already on the disk would be a card
        // about the wrong thing.
        "DownloadingWorkshop" => (
            crate::i18n::text("shell-downloading-workshop-items"),
            Some(PREPARING),
            true,
        ),
        "ProcessingShaderCache" => (
            crate::i18n::text("shell-processing-shaders"),
            Some(PREPARING),
            true,
        ),
        // Valve puts the script's name where the others put a percentage. The
        // name of a Visual C++ redistributable is not what somebody waiting for
        // a game needs, so this one says only what is happening.
        "RunningInstallScript" => (
            crate::i18n::text("shell-running-the-game-s-installer"),
            Some(PREPARING),
            false,
        ),
        "SynchronizingCloud" => (
            crate::i18n::text("shell-syncing-saved-games"),
            Some(PREPARING),
            false,
        ),
        "VerifyingFiles" => (
            crate::i18n::text("shell-checking-the-game-s-files"),
            Some(CHECKING),
            false,
        ),
        "UpdatingDRM" => (
            crate::i18n::text("shell-updating-the-game-s-files"),
            Some(PREPARING),
            false,
        ),
        "GettingLegacyKey" => (
            crate::i18n::text("shell-getting-the-product-key"),
            Some(PREPARING),
            false,
        ),
        "ConnectingToSteam" => (
            crate::i18n::text("shell-connecting-to-steam"),
            Some(PREPARING),
            false,
        ),
        // The step every launch begins on: the client asking Steam what it
        // knows about the game, which is how it finds out there is an update.
        "UpdatingAppInfo" => (
            crate::i18n::text("shell-checking-for-an-update"),
            Some(CHECKING),
            false,
        ),
        // A game set to start at a time of its own. Valve counts this one down
        // as a percentage, which is why it is here at all.
        "DelayLaunch" => (
            crate::i18n::text("shell-waiting-to-start"),
            Some(PREPARING),
            true,
        ),
        _ => return None,
    };
    Some(Words { line, verb, counts })
}

/// What a game's splash is waiting on, in the only two steps the shell can
/// actually tell apart.
///
/// Two rather than one because they fail differently and take wildly different
/// amounts of time, and because only the second is about the game. A cold
/// client is the better part of a minute of the wait, and it is a minute spent
/// on something the user never asked for and cannot see — so it is the half
/// most worth naming, not the least.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Doing {
    /// Valve's client is being started and signed in.
    Steam,
    /// It is up, and it has been asked for the game.
    Game,
    /// And it answered by fetching the game rather than starting it: an update
    /// it decided was due first.
    ///
    /// The third step, and the shell *can* tell it apart — the game's own
    /// manifest starts moving, which is the same thing the row on the bar and
    /// the guide's download card are drawn from. Naming it is the whole of the
    /// difference between a loading screen that says nothing for ten minutes
    /// and one that says what is being downloaded and how far along it is.
    Fetching,
}

/// An application that has been started and has not appeared yet.
pub struct Launch {
    /// What to call it while it starts.
    pub name: String,
    /// Its icon, by theme name; the slot is looked up when it is drawn.
    pub icon: Option<String>,
    /// The display it is opening on, and the tile it is opening out of.
    ///
    /// Named rather than numbered, because a splash outlives the shape of the
    /// display list: a screen unplugged while this is waiting shifts every
    /// panel after it down one, and an index written down here would then be
    /// pointing at somebody else's screen. See [`crate::Display`].
    pub display: crate::Display,
    pub from: [f32; 4],
    /// Its process, so a launch that dies takes its splash with it.
    pub pid: Option<u32>,
    /// Whether Valve's client is starting this rather than the shell.
    ///
    /// It changes two things and nothing else: how long to wait, and that
    /// there is no process whose death means the launch failed. The `steam`
    /// that carries the request exits at once and the game is a child of the
    /// client, so liveness says nothing here and only the window counts.
    through_steam: bool,
    /// Whether Heroic is starting this. The same two differences as
    /// [`Self::through_steam`] and for the same reason: a Heroic that is
    /// already running takes the request and the process that carried it exits
    /// at once, so only the window counts — and it is allowed
    /// [`HEROIC_PATIENCE`].
    through_heroic: bool,
    /// Whether no Heroic was running when this was pressed, so the process
    /// that carried the request is Heroic itself, there for as long as the
    /// game is. Its going before any window came is then the launch over —
    /// Heroic gave up, most often at something it needed the network for —
    /// rather than a hand-over to a Heroic already running.
    heroic_carries_it: bool,
    /// Whether this is a game opening on its own picture although Valve's
    /// client is not starting it — an Epic game, whose backdrop is standing
    /// behind the display the way a Steam game's is. Drawn the way a Steam
    /// game's launch is: no panel, the logo or the name in the middle of the
    /// picture, the ring in a corner, and a dip through black onto the game.
    /// See [`Self::drawn_as_a_game`].
    own_picture: bool,
    /// That game's wordmark on this disk, where it has one. A Steam game's is
    /// asked for by app id instead; see [`crate::ui::LaunchView::logo`].
    logo: Option<PathBuf>,
    /// What Valve's client is doing to the game instead of starting it, in the
    /// words the row on the bar uses — "Updating · 33% of 1.3 GB".
    ///
    /// The shell's, rather than a sentence of this module's, and deliberately:
    /// it is the same fact drawn in two places at once, and a loading screen
    /// describing the download differently from the row it was pressed on
    /// would be two answers to one question.
    said: Option<String>,
    /// How much of that had arrived when this was last told, which is what the
    /// patience below is measured against rather than the clock.
    fetched: u64,
    /// Which game, for a launch out of the Steam library.
    ///
    /// The one thing the splash needs that is not about waiting: a game opens
    /// under its own artwork on its own picture, and both are asked for by app
    /// id — see [`crate::ui::build_launch`]. `None` for everything else, which
    /// is what puts an ordinary application back on the panel it always had.
    game: Option<u32>,
    /// And which step of starting it the shell is on, for the line under the
    /// indicator. Set with the game and moved on once Valve's client is up;
    /// `None` for an ordinary application, whose panel says its name already.
    doing: Option<Doing>,
    /// Which step of the launch Valve's client is on, while it is on one this
    /// shell has words for.
    ///
    /// Its own state rather than a line in [`Self::said`], for two reasons.
    /// One step carries something to press — Steam stops on the shader cache
    /// and offers to start the game with what has been compiled so far — and
    /// **every** one of them stops the patience, because a client that is
    /// visibly working on this press is not a client to give up on.
    ///
    /// Reported from use twice on 2026-09-04, both as a Valve window that came
    /// out from under this screen. "Processing Vulkan shaders (0%)", with
    /// `Skip` and `Cancel`, on a session driven with a controller and no way to
    /// press either. And "Downloading content (19%)", after this screen had
    /// already said the game failed to start, one minute into an update the
    /// same press had asked for.
    step: Option<Step>,
    /// And whether somebody has already pressed past them.
    ///
    /// The watcher polls the client every two seconds, so for a moment after
    /// the press it still reports what it last saw — and a hint that came back
    /// after being answered would read as a press that did nothing. Once past,
    /// always past: nobody who has skipped the compile wants to be offered it
    /// again on the way to the same game.
    skipped: bool,
    /// Whether the press behind this loading screen is to be made again once
    /// the work Steam is doing has finished.
    ///
    /// **A launch Valve's client refuses is over**, and the update it refused
    /// over is one the same press asked it to schedule. Read off this machine
    /// on 2026-09-03: `LaunchApp failed with AppError_19` — "update required" —
    /// twenty-one seconds into the press, and eighteen seconds after that the
    /// client finished the update and started nothing, because the launch it
    /// belonged to had been over the whole time. So the loading screen stays,
    /// says what Steam is doing, and asks again when Steam has stopped doing
    /// it. See `Shell::steam_would_not_start_it`.
    ask_again: bool,
    /// And whether that has already happened, because it happens **once**. A
    /// second refusal is a refusal about something else, and a press that
    /// answered every refusal with another press would be a loading screen
    /// asking for ever.
    asked_again: bool,
    /// Which of Valve's own windows this is waiting for, where what is
    /// opening is the client itself rather than a game.
    ///
    /// Open Steam, Open Steam (Client) and the rows that hand the client a
    /// request about a title all end in a window of the client's own, and
    /// each of them may take as long as starting Steam takes — most of a
    /// minute on a cold client. That wait used to be answered by nothing:
    /// the row folded away and the bar sat there, so the press read as
    /// ignored, the next press was the same row again, and the window
    /// arrived over whatever the person had moved on to. So the client is
    /// opened on the same terms as everything else on this bar, behind a
    /// loading screen that holds the display until the window is there.
    ///
    /// What was asked for is kept so the panel that says it never came can
    /// offer the same press again. `None` for every other launch.
    asked_of_the_client: Option<lxb_steam::Doing>,
    /// When the press was answered. The panel's own animation is measured from
    /// this and nothing moves it, because it is the moment the user acted.
    started: Instant,
    /// And when the wait for a window began, which is not the same moment.
    ///
    /// Two clocks because they answer two questions. A game going through
    /// Valve's client cannot be asked for until the client is up, so the
    /// patience for its window has to start when it was *asked for* rather
    /// than when the button was pressed — otherwise most of a cold client's
    /// minute is spent out of the splash's four. Measuring the animation from
    /// the same clock is what made the panel grow out of its tile twice: the
    /// press opened it, and the hand-over to Steam opened it again.
    waiting_since: Instant,
    /// The windows already on that display when it started. Anything outside
    /// this set is the application arriving.
    known: Vec<u32>,
    /// And what the display's foreground was called then, for the application
    /// that opens no new window because it already had one.
    foreground: String,
    /// When it turned up, and how.
    arrived: Option<(Instant, Arrival)>,
}

/// What was already on the display when the launch started.
///
/// The splash has no way to ask which window belongs to the process it
/// started — the compositor announces titles and sizes, not pids — so it works
/// by difference: anything here is somebody else's, and the first thing that
/// is not is the application arriving.
pub struct Before<'a> {
    pub windows: &'a [u32],
    pub foreground: &'a str,
}

impl Launch {
    pub fn new(
        name: String,
        icon: Option<String>,
        display: crate::Display,
        from: [f32; 4],
        pid: Option<u32>,
        now: Instant,
        before: Before<'_>,
    ) -> Self {
        Self {
            name,
            icon,
            display,
            from,
            pid,
            through_steam: false,
            through_heroic: false,
            heroic_carries_it: false,
            own_picture: false,
            logo: None,
            said: None,
            fetched: 0,
            game: None,
            doing: None,
            step: None,
            skipped: false,
            ask_again: false,
            asked_again: false,
            asked_of_the_client: None,
            started: now,
            waiting_since: now,
            known: before.windows.to_vec(),
            foreground: before.foreground.to_string(),
            arrived: None,
        }
    }

    /// Mark this as a game Valve's client is starting, and say which game.
    ///
    /// The splash then waits [`STEAM_PATIENCE`] rather than [`PATIENCE`], and
    /// stops treating "no process" as "it died" — there is no process of ours
    /// to have died. The app id comes with it because the two are the same
    /// fact: every launch that goes through the client is a title out of the
    /// library, and there is no such thing as one without an id.
    pub fn through_steam(mut self, game: u32) -> Self {
        self.through_steam = true;
        self.game = Some(game);
        self.doing = Some(Doing::Steam);
        self
    }

    /// Mark this as a game Heroic is starting. See the field of the same name.
    pub fn through_heroic(mut self) -> Self {
        self.through_heroic = true;
        self
    }

    /// And say whether a Heroic was running already when it was pressed —
    /// which decides what the process that carried the request is. See the
    /// `heroic_carries_it` field.
    pub fn heroic_already_running(mut self, running: bool) -> Self {
        self.heroic_carries_it = self.through_heroic && !running;
        self
    }

    /// Whether a game Heroic was asked to start never appeared: Heroic went
    /// without opening a window, or the wait ran out. Answered with a panel,
    /// the way a Steam game that never appeared is — a loading screen that
    /// simply goes is a press nobody answered.
    pub fn heroic_never_started_it(&self) -> bool {
        self.through_heroic && matches!(self.arrived, Some((_, Arrival::Gone | Arrival::GaveUp)))
    }

    /// Mark this as a game opening on its own picture, with its own logo where
    /// it has one — see the `own_picture` field. The line beside the ring says
    /// the game is starting, which is the one step there is to name: Heroic
    /// says nothing about how far along it is.
    pub fn on_its_own_picture(mut self, logo: Option<PathBuf>) -> Self {
        self.own_picture = true;
        self.logo = logo;
        self.doing = Some(Doing::Game);
        self
    }

    /// Whether Heroic is starting this.
    pub fn is_through_heroic(&self) -> bool {
        self.through_heroic
    }

    /// Whether this is drawn as a game opens — on its own picture, dipping
    /// through black — rather than as an application's panel. Every Steam
    /// game, and a game [`Self::on_its_own_picture`] was said of.
    pub fn drawn_as_a_game(&self) -> bool {
        self.game.is_some() || self.own_picture
    }

    /// The game's own logo on this disk, for a game that is not Steam's.
    pub fn logo_file(&self) -> Option<&Path> {
        self.logo.as_deref()
    }

    /// Mark this as Valve's client itself being opened, and say what for.
    ///
    /// The same footing as a game handed to the client and for the same two
    /// reasons: there is no process of ours to watch — the request is carried
    /// to the client over its pipe, or by a courier that exits the moment it
    /// has handed it over — and the wait is the client's, which is
    /// [`STEAM_CLIENT_PATIENCE`] until the client takes the request and
    /// [`STEAM_PATIENCE`] for its window after that. Not a game, though: the
    /// splash is the ordinary panel with the client's own mark on it, and it
    /// fades onto the window rather than dipping through black, because what
    /// is behind it is Steam's storefront and not a picture the shell chose.
    pub fn for_valves_client(mut self, asked: lxb_steam::Doing) -> Self {
        self.through_steam = true;
        self.doing = Some(Doing::Steam);
        self.asked_of_the_client = Some(asked);
        self
    }

    /// The game this is opening, if it is a game at all.
    pub fn game(&self) -> Option<u32> {
        self.game
    }

    /// What Valve's client was asked for, where this is the client itself
    /// opening. See [`Self::for_valves_client`].
    pub fn asked_of_the_client(&self) -> Option<lxb_steam::Doing> {
        self.asked_of_the_client
    }

    /// The window this was waiting for is on the display, brought forward
    /// rather than newly mapped. `true` when that ends the wait.
    ///
    /// [`Self::advance`] decides an arrival by difference — a window that was
    /// not there before, or a foreground that has changed — and one press
    /// leaves both unchanged: Open Steam (Client) on a client whose storefront
    /// is already mapped and already in front, with the bar standing over it.
    /// The client brings that window forward and opens nothing. The shell's
    /// watch on Valve's windows knows this is the window the press was for —
    /// see `Wanted` in the shell — and says so here. Nothing happens to a
    /// splash that has already been answered.
    pub fn the_window_was_raised(&mut self, now: Instant) -> bool {
        if self.arrived.is_some() {
            return false;
        }
        tracing::debug!(app = %self.name, "the window the splash was waiting for was brought forward");
        self.arrived = Some((now, Arrival::Raised));
        true
    }

    /// Which step of starting it the splash should say it is on.
    pub fn doing(&self) -> Option<Doing> {
        self.doing
    }

    /// The windows on that display which were not there when this began.
    ///
    /// The same difference [`Self::advance`] decides an arrival by, handed out
    /// so the caller can keep it. It is the one moment anything knows which
    /// window a launch turned into: a window carries the class its binary
    /// announces and nothing that says who started it, so a shell that did not
    /// write it down here cannot work it out afterwards.
    pub fn newcomers<'a>(&'a self, windows: &'a [u32]) -> impl Iterator<Item = u32> + 'a {
        windows
            .iter()
            .copied()
            .filter(|id| !self.known.contains(id))
    }

    /// The same, for a game Valve's client has just been asked to start.
    ///
    /// There is no pid: the game will be the client's child and this shell
    /// never sees it. What matters is the clock — the time spent starting and
    /// signing in the client is not the game failing to appear, and on a cold
    /// client that is most of a minute of the patience already gone.
    ///
    /// Only the patience, and the line under the indicator. The panel on
    /// screen is in the middle of its own opening, or long finished with it,
    /// and is not disturbed: this happens while the user is watching, and a
    /// splash that started growing out of its tile for a second time would
    /// read as a second application opening.
    ///
    /// The line changes because this is the moment it stops being true. Up to
    /// here the wait was Valve's client coming up; from here it is the game,
    /// and a loading screen still saying "Steam" a minute into a shader cache
    /// is a loading screen lying about what it is waiting for.
    ///
    /// The same moment for the client's own window — see
    /// [`Self::for_valves_client`]: the client has taken the request, and
    /// what is left is the window it raises for it. The step is the second
    /// one either way; only the panel's own name says what is coming.
    pub fn now_starting_through_steam(&mut self, now: Instant) {
        self.waiting_since = now;
        self.doing = Some(Doing::Game);
    }

    /// Say which step of the launch the client is on, or that it has moved on
    /// to one with nothing to say.
    ///
    /// The line itself is not set here: it is set where every other line on
    /// this screen is set, once a frame, out of everything the shell knows
    /// about the game — see `Shell::sync_the_launch_downloads`. What this
    /// keeps is the fact and the press that goes with it.
    pub fn working_on(&mut self, step: Option<Step>) {
        // The compile that was skipped never comes back, and nothing else is
        // affected by having skipped it. See [`Self::skipped`].
        let step = step.filter(|step| !self.skipped || step.action_id.is_none());
        if self.step != step {
            tracing::info!(app = %self.name, ?step, "what the client is doing about this launch");
        }
        self.step = step;
    }

    /// Valve's client has given up on the launch this screen was watching:
    /// forget the step it was on.
    ///
    /// **A step is the client's account of one launch, and that launch is
    /// over.** The watcher that reported it stops at the refusal and sends
    /// nothing after it, so a step left here is never replaced — and
    /// `Shell::sync_the_launch_downloads` reads any step as work in flight,
    /// ahead of everything on the disk, for as long as a client is running.
    /// The press Steam refused is made again when that work ends, so a step
    /// that never ends is a press never made again.
    ///
    /// Reported from use on 2026-09-26. Counter-Strike 2 was refused with
    /// `AppError_19` at the moment its shader cache finished arriving; the
    /// update behind it ran for seven seconds and finished, the guide said so,
    /// and this screen went on saying "Downloading content" over a game that
    /// was ready — still saying it three minutes later, when the session was
    /// logged out of. What it says from the refusal on comes from the disk and
    /// the client's own jobs, which is where the work it was refused over is
    /// described.
    pub fn the_launch_was_refused(&mut self) {
        if let Some(step) = self.step.take() {
            tracing::info!(app = %self.name, ?step, "the launch that step belonged to is over");
        }
    }

    /// Somebody has pressed past the compile: take the line and the hint away
    /// now, and do not put them back.
    pub fn step_was_skipped(&mut self) {
        self.skipped = true;
        self.step = None;
    }

    /// Which step it is on, for the line and the legend.
    pub fn step(&self) -> Option<&Step> {
        self.step.as_ref()
    }

    /// The action to answer to be let past it, where there is one.
    ///
    /// Only ever the shader cache, and only on a client that exposes an
    /// interface — its log says the launch has stopped there, and only the
    /// interface can be told to go on. A screen with nothing to press must not
    /// offer a press.
    pub fn step_may_be_skipped(&self) -> Option<u32> {
        self.step.as_ref()?.action_id
    }

    /// Make this press again when whatever Steam is doing to the game is done,
    /// and say whether that was taken.
    ///
    /// `false` where the press has already been made a second time; see
    /// [`Self::asked_again`], which is why this is a question rather than an
    /// instruction.
    pub fn ask_again_when_the_work_ends(&mut self) -> bool {
        if self.asked_again {
            return false;
        }
        self.ask_again = true;
        true
    }

    /// Take one frame's answer to what the press is waiting on, and say whether
    /// the answer is to make the press again.
    ///
    /// Work in front of it keeps the screen saying what the work is, and keeps
    /// its patience off — see [`Self::now_fetching`]. No work is the wait for
    /// the window starting again — see [`Self::done_fetching`] — and, for a
    /// press the client refused over the work that has just ended, the moment
    /// to press again, which is answered once. The answer itself is
    /// `Steam::what_a_press_waits_on`.
    pub fn follow_the_work(&mut self, now: Instant, work: Option<crate::Underway>) -> bool {
        match work {
            Some(work) => {
                self.now_fetching(now, work.said, work.arrived, work.quietly);
                false
            }
            None => {
                self.done_fetching(now);
                self.take_the_second_press()
            }
        }
    }

    /// Whether the press is waiting to be made again, without spending it.
    pub fn will_ask_again(&self) -> bool {
        self.ask_again
    }

    /// Whether now is that moment, asked once — the answer is taken away by
    /// asking for it.
    pub fn take_the_second_press(&mut self) -> bool {
        if !self.ask_again {
            return false;
        }
        self.ask_again = false;
        self.asked_again = true;
        true
    }

    /// Valve's client answered the press by fetching the game rather than
    /// starting it.
    ///
    /// Told once a frame while the game's own manifest says Steam is working on
    /// it, with what the row on the bar says and how many bytes have arrived.
    /// Two things follow from it, and they are the whole of why this state
    /// exists.
    ///
    /// The line under the indicator stops being "Starting the game", which for
    /// a 1.3 GB update is a loading screen lying for ten minutes about what it
    /// is waiting for.
    ///
    /// And the patience stops running. Up to here the splash gives up after
    /// [`STEAM_PATIENCE`] and says the game did not start — which for an update
    /// is a minute of waiting, a panel saying it failed, and then the game
    /// opening by itself twenty minutes later over whatever is on the screen by
    /// then. There is nothing to guess at here: Steam is working, the shell can
    /// see it working, and a clock would be a clock on somebody's connection.
    /// What ends it instead is the work stopping — the manifest stops saying
    /// Steam is on it, and the ordinary patience takes over from that moment —
    /// or the user, who may leave at any point.
    pub fn now_fetching(&mut self, now: Instant, said: String, arrived: u64, quietly: bool) {
        if self.doing != Some(Doing::Fetching) {
            tracing::info!(app = %self.name, %said, quietly, "the launch is waiting on a download");
        }
        self.doing = Some(Doing::Fetching);
        self.said = Some(said);
        // The clock is kept at the last moment the work moved rather than at
        // now, so that the patience which takes over when the fetch ends is
        // measured from the end of the work and not from the frame the shell
        // noticed it had ended.
        //
        // `quietly` is for the stretch of an update where there are no bytes to
        // keep it with. The manifest counts nothing while Steam reconfigures,
        // preallocates and verifies — ten minutes and thirty-nine seconds of it
        // on the machine this was measured on, against a patience of sixty
        // seconds — so what says the work is still moving there is that a
        // client is running to move it. The caller asks that once a frame; see
        // [`crate::steam::Steam::quietly_updating`]. A client that goes away
        // stops saying it, and the ordinary patience runs out on a launch
        // nothing is working on any more.
        if quietly || arrived != self.fetched {
            self.fetched = arrived;
            self.waiting_since = now;
        }
    }

    /// And it has stopped: whatever Steam was doing to the game is done, and
    /// what is left is the game appearing.
    ///
    /// The ordinary patience starts again here rather than carrying on from
    /// where the fetch began, because it is the same wait it always was —
    /// [`STEAM_PATIENCE`] for a window — and it has not had a second of it yet.
    pub fn done_fetching(&mut self, now: Instant) {
        if self.doing != Some(Doing::Fetching) {
            return;
        }
        tracing::info!(app = %self.name, "the download the launch was waiting on has ended");
        self.doing = Some(Doing::Game);
        self.said = None;
        self.waiting_since = now;
    }

    /// What Steam is doing to the game instead of starting it, where it is
    /// doing anything.
    pub fn said(&self) -> Option<&str> {
        self.said.as_deref()
    }

    /// Whether this splash is waiting on Steam fetching the game, which is the
    /// one wait the user may leave.
    pub fn is_fetching(&self) -> bool {
        self.doing == Some(Doing::Fetching)
    }

    /// Whether this is a wait somebody may walk away from.
    ///
    /// Two of the three steps, and the third is deliberately not one: a game
    /// that is starting is starting, and [`STEAM_PATIENCE`] is a minute at the
    /// outside. The other two are not.
    ///
    /// [`Doing::Fetching`] is however long somebody's connection takes over
    /// however large the update is, and it is the one this was written for.
    /// [`Doing::Steam`] is the cold client, and it was left out on the
    /// reasoning that nothing else takes long enough for Back to mean
    /// anything — but it waits [`STEAM_CLIENT_PATIENCE`], seventy seconds, on
    /// a picture with a spinner on it and no way off. Seventy seconds is long
    /// enough. The guide does not open over a splash, so without this the only
    /// thing to do is wait it out.
    pub fn may_be_left(&self) -> bool {
        matches!(self.doing, Some(Doing::Fetching | Doing::Steam))
    }

    /// Whether a game started through Valve's client never appeared.
    ///
    /// Only [`Arrival::GaveUp`] counts, and only for a Steam launch: a launch
    /// of ours that dies is caught by its process going away, and this one has
    /// no process of ours to watch.
    pub fn steam_never_appeared(&self) -> bool {
        self.through_steam && matches!(self.arrived, Some((_, Arrival::GaveUp)))
    }

    /// And which of the two waits it gave up in, for a splash that gave up at
    /// all.
    ///
    /// The panel that answers them is not the same panel. A client that never
    /// came up is a Steam that can be tried again or opened by hand; a game
    /// that never appeared, from a client that did come up, is a game. And the
    /// difference reaches further than the wording: the first leaves a wake
    /// still running out there, and the press that was waiting on it has to
    /// stop waiting or it swallows every press after it.
    pub fn gave_up_waiting_for_the_client(&self) -> bool {
        self.steam_never_appeared() && self.doing == Some(Doing::Steam)
    }

    /// Bring the splash up to date with what is on its display. Called once a
    /// frame while it is up; returns the arrival, once, on the frame it
    /// happens, so the caller can say so.
    pub fn advance(
        &mut self,
        now: Instant,
        windows: &[u32],
        foreground: &str,
        alive: bool,
    ) -> Option<Arrival> {
        if let Some((at, how)) = self.arrived {
            // It handed over to something that is no longer there. A window
            // that comes and goes again is not the application arriving: X11
            // toolkits build one window, throw it away and build the one they
            // meant, and Valve's client is full of windows that exist for a
            // moment — so handing the screen over on the first of them and
            // never looking again leaves the user on the bar, with no splash
            // and no game, wondering whether their press did anything.
            //
            // Only for the two arrivals that are claims about what is on the
            // screen. A launch that died or ran out of patience has ended and
            // does not un-end.
            let vanished = matches!(how, Arrival::Window | Arrival::Raised)
                && foreground.is_empty()
                && !windows.iter().any(|id| !self.known.contains(id));
            if vanished {
                // And which of the two it is, which is the whole of what
                // [`STAYED`] decides. A window that stood is the application
                // itself, and it leaving is this launch being over — coming
                // back to wait for it would put a loading screen over a display
                // the user has just been handed, and then say the game never
                // opened, which it plainly did.
                if now.duration_since(at).as_secs_f32() >= STAYED {
                    tracing::debug!(app = %self.name, "what the splash handed over to has closed");
                    self.arrived = Some((now, Arrival::Left));
                    return Some(Arrival::Left);
                }
                tracing::debug!(app = %self.name, "what the splash handed over to has gone again");
                self.arrived = None;
            }
            return None;
        }
        let waited = now.duration_since(self.waiting_since).as_secs_f32();
        let patience = match (self.through_steam, self.doing) {
            _ if self.through_heroic => HEROIC_PATIENCE,
            // Waiting on Valve's client, which has not been asked for the game
            // yet and cannot be until it is up.
            (true, Some(Doing::Steam)) => STEAM_CLIENT_PATIENCE,
            // Not a wait on anything that might not come: Steam is fetching the
            // game and the shell can watch it arrive. See [`Self::now_fetching`],
            // where the clock is restarted by the bytes rather than by the
            // frame — so this is only ever reached by a download that has
            // stopped moving without the manifest saying it has stopped.
            (true, Some(Doing::Fetching)) => STEAM_PATIENCE,
            (true, _) => STEAM_PATIENCE,
            (false, _) => PATIENCE,
        };
        let how = if windows.iter().any(|id| !self.known.contains(id)) {
            Arrival::Window
        } else if !foreground.is_empty() && foreground != self.foreground {
            Arrival::Raised
        } else if !alive && !self.through_steam && (!self.through_heroic || self.heroic_carries_it)
        {
            // Not for a Steam launch. The process that carried the request
            // exits within milliseconds of being started — it has done its
            // whole job by then — and reading that as the game dying would
            // take the splash away before the game had begun to load. Nor for
            // one Heroic took from an instance already running, for the same
            // reason; but where the process is Heroic itself, its going is.
            Arrival::Gone
        } else if waited >= patience {
            Arrival::GaveUp
        } else {
            return None;
        };
        self.arrived = Some((now, how));
        Some(how)
    }

    /// When the press was, for the log.
    pub fn started(&self) -> Instant {
        self.started
    }

    /// How far the panel is out of its tile: 0 on the tile, 1 filling the
    /// display. Unshaped — the drawing eases it.
    pub fn open(&self, now: Instant) -> f32 {
        (now.duration_since(self.started).as_secs_f32() / OPEN).clamp(0.0, 1.0)
    }

    /// How long ago a game began dipping through black, if that is what is
    /// happening.
    ///
    /// Only a game, and only an arrival that is a claim about something being
    /// on the screen. A launch that died or ran out of patience has nothing to
    /// reveal at the other end of a dip — what is behind it is the bar — so it
    /// fades off the way an application's does and the user is told what went
    /// wrong.
    fn dipping(&self, now: Instant) -> Option<f32> {
        match self.arrived {
            Some((at, Arrival::Window | Arrival::Raised)) if self.drawn_as_a_game() => {
                Some(now.duration_since(at).as_secs_f32())
            }
            _ => None,
        }
    }

    /// How much of the splash is left: 1 while it is waiting, falling to 0 as
    /// the application takes the screen.
    ///
    /// A game's goes with the black rather than with the window — it is gone
    /// by the time the screen is, so nothing of the shell's is still being
    /// drawn over a picture that is already black.
    pub fn fade(&self, now: Instant) -> f32 {
        if let Some(since) = self.dipping(now) {
            return 1.0 - (since / BLACK_IN).clamp(0.0, 1.0);
        }
        let Some((at, how)) = self.arrived else {
            return 1.0;
        };
        // Nothing at all for an application that has left. It had already
        // faded off this display when it arrived; there is nothing here to
        // fade a second time, and drawing one frame of it would be a loading
        // screen flashing over a game somebody has just closed.
        if matches!(how, Arrival::Left) {
            return 0.0;
        }
        let since = now.duration_since(at).as_secs_f32();
        1.0 - ((since - SETTLE) / HANDOVER).clamp(0.0, 1.0)
    }

    /// How black the display is: 0 while the shell's own picture is showing, 1
    /// while nothing but black is, and back to 0 as the game comes up out of
    /// it. Unshaped — the drawing eases it.
    ///
    /// Always 0 for anything that is not a game reaching its window.
    pub fn blackout(&self, now: Instant) -> f32 {
        let Some(since) = self.dipping(now) else {
            return 0.0;
        };
        if since < BLACK_IN {
            return (since / BLACK_IN).clamp(0.0, 1.0);
        }
        let up = since - BLACK_IN - BLACK_HOLD;
        if up <= 0.0 {
            return 1.0;
        }
        1.0 - (up / BLACK_OUT).clamp(0.0, 1.0)
    }

    /// Whether the black is coming *off* the game rather than going on over
    /// the shell's own picture.
    ///
    /// The two halves of a dip look the same from outside — one number going
    /// up and then down — but the shell has to do opposite things behind
    /// them. On the way down the picture under the black is the game's hero,
    /// which the shell is drawing and must keep drawing. On the way up it is
    /// the game's own window, which it must not draw over: repainting the
    /// hero there would reveal the picture the user came from instead of the
    /// game they asked for.
    pub fn uncovering(&self, now: Instant) -> bool {
        self.dipping(now)
            .is_some_and(|since| since >= BLACK_IN + BLACK_HOLD)
    }

    /// Whether it is still waiting for the application, as against handing the
    /// screen over to one that has arrived. The indicator stops asking.
    pub fn waiting(&self) -> bool {
        self.arrived.is_none()
    }

    /// Whether there is anything on the screen for this. False through the
    /// stretch where it has faded out but is still watching, which is when the
    /// display it is on has no reason to keep drawing for it.
    ///
    /// The black counts. A game whose splash has dissolved is still holding
    /// the display — with nothing of its own on it, but holding it — and a
    /// display that stopped drawing there would freeze the screen black over
    /// the game it was about to reveal.
    pub fn drawing(&self, now: Instant) -> bool {
        self.fade(now) > 0.0 || self.blackout(now) > 0.0
    }

    /// Nothing left to draw, and nothing left to change its mind about.
    ///
    /// Faded out is not enough. For [`WATCHING`] afterwards this stays alive
    /// with nothing on the screen, so that a hand-over to a window which then
    /// goes away can be taken back — see [`Self::advance`].
    pub fn finished(&self, now: Instant) -> bool {
        let Some((at, how)) = self.arrived else {
            return false;
        };
        if self.drawing(now) {
            return false;
        }
        // A launch that ended because nothing was ever coming has nothing to
        // watch for — nor has one whose application has been and gone.
        if matches!(how, Arrival::Gone | Arrival::GaveUp | Arrival::Left) {
            return true;
        }
        let watching = if self.through_steam || self.through_heroic {
            STEAM_WATCHING
        } else {
            WATCHING
        };
        now.duration_since(at).as_secs_f32() >= watching
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn launch(now: Instant) -> Launch {
        Launch::new(
            "Celeste".to_string(),
            Some("celeste".to_string()),
            crate::Display::for_a_test(0),
            [100.0, 100.0, 80.0, 80.0],
            Some(4242),
            now,
            Before {
                windows: &[7],
                foreground: "",
            },
        )
    }

    /// The same press, on a title out of the Steam library.
    fn game(now: Instant) -> Launch {
        launch(now).through_steam(504230)
    }

    fn at(t0: Instant, seconds: f32) -> Instant {
        t0 + Duration::from_secs_f32(seconds)
    }

    /// A game Steam is downloading before it will start it does not run out of
    /// patience while the download moves.
    ///
    /// The failure this is about, from a real session: press a game whose
    /// update Valve's client decided was due, and the client fetches for
    /// minutes before launching anything. The splash waited a minute for a
    /// window, said the game had not started, and then the game started — over
    /// whatever was on the screen by then. There is nothing to guess at: the
    /// game's own manifest is moving, which is what the row on the bar is drawn
    /// from, so the splash is told and says so.
    #[test]
    fn a_download_the_launch_is_waiting_on_is_not_a_game_that_never_started() {
        let t0 = Instant::now();
        let mut splash = game(t0);
        splash.now_starting_through_steam(t0);

        // A minute in, with nothing said: the old behaviour, and still the
        // right one for a game that is simply not appearing.
        let mut alone = game(t0);
        alone.now_starting_through_steam(t0);
        assert_eq!(
            alone.advance(at(t0, STEAM_PATIENCE), &[], "", true),
            Some(Arrival::GaveUp)
        );

        // Told that Steam is fetching it, the same minute passes and the splash
        // is still waiting — because the download moved.
        splash.now_fetching(
            at(t0, 5.0),
            "Updating · 10% of 1.3 GB".to_string(),
            130_000_000,
            false,
        );
        assert_eq!(splash.doing(), Some(Doing::Fetching));
        assert_eq!(splash.said(), Some("Updating · 10% of 1.3 GB"));
        assert!(splash.is_fetching());
        assert_eq!(splash.advance(at(t0, STEAM_PATIENCE), &[], "", true), None);

        // Ten minutes of it, so long as the bytes keep arriving.
        for minute in 1..10 {
            let now = at(t0, 60.0 * minute as f32);
            splash.now_fetching(
                now,
                format!("Updating · {}% of 1.3 GB", minute * 10),
                130_000_000 * minute as u64,
                false,
            );
            assert_eq!(splash.advance(now, &[], "", true), None, "minute {minute}");
        }

        // And a download that stops moving is not waited on for ever: the
        // manifest is written as the client works, so bytes that stop arriving
        // for a whole minute have stopped.
        let stuck = at(t0, 60.0 * 9.0);
        splash.now_fetching(
            stuck,
            "Updating · 90% of 1.3 GB".to_string(),
            130_000_000 * 9,
            false,
        );
        assert_eq!(
            splash.advance(at(t0, 60.0 * 9.0 + STEAM_PATIENCE), &[], "", true),
            Some(Arrival::GaveUp)
        );
    }

    /// An update Steam has not begun counting is still an update, and the
    /// splash waits it out.
    ///
    /// The half of the same failure the test above does not cover, and the one
    /// that survived it. Steam does not write a working bit or a byte count
    /// until bytes are moving, and before that it reconfigures, preallocates
    /// and — for an update it has been interrupted in once — verifies
    /// everything already on the disk. Ten minutes and thirty-nine seconds of
    /// that were measured on this machine against Valve's own log, with the
    /// manifest untouched throughout; the splash's patience is sixty seconds.
    /// So a press would have said the game did not start, nine minutes before
    /// it started.
    ///
    /// There are no bytes to keep the clock with, so what keeps it is that a
    /// client is running to be doing the work. See
    /// [`crate::steam::Steam::quietly_updating`].
    #[test]
    fn an_update_the_manifest_is_saying_nothing_about_is_still_waited_out() {
        let t0 = Instant::now();
        let mut splash = game(t0);
        splash.now_starting_through_steam(t0);

        // Ten minutes of a manifest that never moves, told once a frame that
        // Steam is quietly getting on with it. The byte count is the same
        // every time, because it is the *last* operation's and nothing is
        // writing a new one.
        for second in (5..=600).step_by(5) {
            let now = at(t0, second as f32);
            splash.now_fetching(now, "Updating".to_string(), 0, true);
            assert_eq!(
                splash.advance(now, &[], "", true),
                None,
                "gave up {second}s into a silent update"
            );
        }
        assert_eq!(splash.doing(), Some(Doing::Fetching));
        assert_eq!(splash.said(), Some("Updating"));
        assert!(
            splash.is_fetching(),
            "and it can still be walked away from, which is what makes the wait bearable"
        );

        // And it is not waited out for ever. What was keeping the clock is a
        // client that is running; when that stops saying so — the client
        // closed, or Steam gave the update up — the ordinary patience takes
        // over from the last frame that did, and runs out.
        assert_eq!(
            splash.advance(at(t0, 600.0 + STEAM_PATIENCE - 1.0), &[], "", true),
            None
        );
        assert_eq!(
            splash.advance(at(t0, 600.0 + STEAM_PATIENCE), &[], "", true),
            Some(Arrival::GaveUp),
            "a launch nothing is working on any more is a launch that ended"
        );
    }

    /// When the download ends, the wait for the window starts over.
    ///
    /// Not carried on from where the download began, because it is the same
    /// wait it always was — a minute for a window — and it has not had a second
    /// of it yet. Without this a ten-minute update would be followed by a
    /// splash that gave up immediately, on a game that was about to open.
    #[test]
    fn the_two_waits_on_valves_client_are_the_two_that_may_be_left() {
        let t0 = Instant::now();
        let mut splash = game(t0);

        // Valve's client coming up, which is `STEAM_CLIENT_PATIENCE` — seventy
        // seconds of a picture and a spinner. It was left out of this on the
        // reasoning that nothing but a download takes long enough for Back to
        // mean anything; seventy seconds is long enough, and the guide does not
        // open over a splash, so there was nothing else to do but wait it out.
        assert_eq!(splash.doing(), Some(Doing::Steam));
        assert!(splash.may_be_left());

        // The game itself starting, which may not be: a game that is starting
        // is starting, and this is a minute at the outside.
        splash.now_starting_through_steam(t0);
        assert!(!splash.may_be_left());

        // And the download, which is however long somebody's connection takes.
        splash.now_fetching(t0, "Updating · 10%".to_string(), 1, false);
        assert!(splash.may_be_left());

        // An ordinary application is none of the three. Its wait is `PATIENCE`
        // and there is nothing on the other end of it to let go of.
        let ordinary = Launch::new(
            "Something".to_string(),
            None,
            crate::Display::for_a_test(0),
            [0.0; 4],
            None,
            t0,
            Before {
                windows: &[],
                foreground: "",
            },
        );
        assert_eq!(ordinary.doing(), None);
        assert!(!ordinary.may_be_left());
    }

    /// A press refused over work Steam is doing is made again when the work is
    /// done — once, and only once.
    ///
    /// The loop this closes was read off this machine on 2026-09-03. The press
    /// asks Valve's client for the game; the client schedules the update the
    /// game needs, refuses the launch over that very update
    /// (`LaunchApp failed with AppError_19`), then goes and does it — and the
    /// press it belonged to has been over since twenty-one seconds in. Asking
    /// again when the work stops is what turns that into a game starting.
    ///
    /// Once, because a second refusal is about something else, and a press that
    /// answered every refusal with another press would be a loading screen
    /// asking for ever.
    #[test]
    fn a_refused_press_is_made_again_once_the_work_is_done() {
        let t0 = Instant::now();
        let mut splash = game(t0);

        assert!(
            !splash.take_the_second_press(),
            "a press nothing refused is not made twice"
        );
        assert!(splash.ask_again_when_the_work_ends());
        assert!(splash.take_the_second_press());
        assert!(
            !splash.take_the_second_press(),
            "and the answer is taken away by asking for it"
        );

        // Refused a second time, over work that is still not done. There is
        // nothing left to try: the loading screen comes down and says so.
        assert!(
            !splash.ask_again_when_the_work_ends(),
            "one press is made again; the next refusal is answered on the screen"
        );
        assert!(!splash.take_the_second_press());
    }

    /// A refused launch takes its step with it, or the work in front of the
    /// press never ends.
    ///
    /// Reported from use on 2026-09-26: Counter-Strike 2's launch was on
    /// `DownloadingDepots` when the client refused it, the watcher stopped at
    /// the refusal, and the step stayed — so the screen read it as work in
    /// flight for as long as a client was running, and the second press the
    /// update was waiting for was never made. See
    /// [`Launch::the_launch_was_refused`].
    #[test]
    fn a_refused_launch_leaves_no_step_behind() {
        let t0 = Instant::now();
        let mut splash = game(t0);
        splash.working_on(Some(Step {
            doing: words_for("DownloadingDepots").expect("a step the shell names"),
            how_far: Some(2),
            action_id: None,
        }));
        assert!(splash.step().is_some());

        splash.the_launch_was_refused();
        assert!(
            splash.step().is_none(),
            "the launch that step was about is over"
        );
        // And the press is still to be made again: forgetting the step is what
        // lets the end of the work be seen, not a way of giving up on it.
        assert!(!splash.will_ask_again());
        assert!(splash.ask_again_when_the_work_ends());
        assert!(
            splash.will_ask_again(),
            "and it can be asked without spending it"
        );
        assert!(splash.will_ask_again());
        assert!(splash.take_the_second_press());
        assert!(!splash.will_ask_again());

        // Nothing to forget is not an error either.
        splash.the_launch_was_refused();
        assert!(splash.step().is_none());
    }

    /// A launch stopped on the game's shaders says so on the loading screen,
    /// and offers the press that gets past it.
    ///
    /// **Reported from use on 2026-09-04, with a screenshot.** Valve's own
    /// dialog — "Launching Counter-Strike 2 / Processing Vulkan shaders (0%)",
    /// with `Skip` and `Cancel` — was standing behind this shell's loading
    /// screen on a session driven with a controller. Steam's desktop UI is
    /// where that dialog comes from; its own gamepad build never shows it and
    /// puts the line and a SKIP button on its launch screen instead, which is
    /// what this screen is.
    #[test]
    fn a_launch_compiling_shaders_says_so_and_offers_the_way_past() {
        let t0 = Instant::now();
        let mut splash = game(t0);
        let shaders = words_for("ProcessingShaderCache").expect("a step the shell names");

        assert!(splash.step().is_none());
        assert_eq!(splash.step_may_be_skipped(), None);

        // A client this session started, which exposes its interface: the
        // reading and the action to answer both come back.
        splash.working_on(Some(Step {
            doing: shaders,
            how_far: Some(34),
            action_id: Some(1),
        }));
        assert_eq!(
            splash.step().map(|step| step.said(None)),
            Some("Processing shaders · 34%".to_string())
        );
        assert_eq!(splash.step_may_be_skipped(), Some(1));

        // And one that was already running when the session came up, which
        // exposes nothing. Its own log says *which* step it is on and never how
        // far, and there is nothing to press — so the screen says the half it
        // has and offers nothing.
        splash.working_on(Some(Step {
            doing: shaders,
            how_far: None,
            action_id: None,
        }));
        assert_eq!(
            splash.step().map(|step| step.said(None)),
            Some("Processing shaders".to_string())
        );
        assert_eq!(
            splash.step_may_be_skipped(),
            None,
            "a screen with nothing to press must not offer a press"
        );

        splash.working_on(None);
        assert!(splash.step().is_none());

        // And once somebody has pressed past it, it does not come back. The
        // watcher polls every two seconds and still reports what it last saw
        // for a moment after the press; a hint that returned after being
        // answered would read as a press that did nothing.
        splash.working_on(Some(Step {
            doing: shaders,
            how_far: Some(60),
            action_id: Some(1),
        }));
        splash.step_was_skipped();
        assert!(splash.step().is_none());
        splash.working_on(Some(Step {
            doing: shaders,
            how_far: Some(61),
            action_id: Some(1),
        }));
        assert!(splash.step().is_none(), "it was already answered");
        assert_eq!(splash.step_may_be_skipped(), None);

        // Skipping the compile is not skipping the launch. What the client
        // does next is walk on to the next step, and that one has a line of its
        // own — a screen that went blank after Skip would read as a press that
        // ended the launch.
        let fetching = words_for("DownloadingDepots").expect("a step the shell names");
        splash.working_on(Some(Step {
            doing: fetching,
            how_far: Some(19),
            action_id: None,
        }));
        assert_eq!(
            splash.step().map(|step| step.said(None)),
            Some("Downloading content · 19%".to_string())
        );
    }

    /// A press the client is visibly answering does not run out of patience,
    /// whatever the disk says.
    ///
    /// **Reported from use on 2026-09-04, with a screenshot** of Valve's own
    /// launch window — *"Starting game / Counter-Strike 2 / LAUNCHING /
    /// Downloading content (19%)"* — which came out from under this screen
    /// *after* the shell had said the game failed to start.
    ///
    /// The timing is this test. The press went out at 00:54:39; one second
    /// later the client wrote `changed task to DownloadingDepots` and then
    /// nothing more, because a step that does not change is not written down
    /// again; at 00:55:39 the minute was up. Nothing on the disk said a word:
    /// the manifest read `StateFlags 1158` — installed, update required, files
    /// corrupt, update started, which is a copy being *repaired* rather than
    /// any standing this screen watches — and every one of its byte counters
    /// was nought.
    ///
    /// So the step is the whole of what holds the screen, and it holds it the
    /// way a download does: the clock is kept at the last moment the work moved
    /// rather than at the frame the shell looked. See [`Launch::now_fetching`].
    #[test]
    fn a_press_the_client_is_answering_does_not_run_out_of_patience() {
        let t0 = Instant::now();
        let mut splash = game(t0);
        splash.now_starting_through_steam(t0);
        let fetching = words_for("DownloadingDepots").expect("a step the shell names");

        // A second in, the client says what it is doing with the press. The two
        // calls the shell makes each frame, in that order — see
        // `Shell::steam_is_working_on_the_launch` and
        // `Shell::sync_the_launch_downloads`.
        splash.working_on(Some(Step {
            doing: fetching,
            how_far: Some(19),
            action_id: None,
        }));
        for second in 1..(STEAM_PATIENCE as u32 * 4) {
            let now = at(t0, second as f32);
            let said = splash
                .step()
                .map(|step| step.said(None))
                .expect("a step to say");
            splash.now_fetching(now, said, 0, true);
            assert_eq!(
                splash.advance(now, &[], "", true),
                None,
                "the client is one fifth of the way through answering this press"
            );
        }
        assert_eq!(
            splash.said(),
            Some("Downloading content · 19%"),
            "and the screen says so, rather than counting down to a panel"
        );
        assert!(
            splash.may_be_left(),
            "a wait this long must have a way off it"
        );

        // And when the client walks on to a step this screen has no words for —
        // `CreatingProcess`, the game itself opening — the ordinary patience
        // starts again, from that moment rather than from the press.
        let done = at(t0, STEAM_PATIENCE * 4.0);
        splash.working_on(None);
        splash.done_fetching(done);
        assert_eq!(splash.said(), None);
        assert_eq!(
            splash.advance(at(done, STEAM_PATIENCE - 1.0), &[], "", true),
            None
        );
        assert_eq!(
            splash.advance(at(done, STEAM_PATIENCE), &[], "", true),
            Some(Arrival::GaveUp),
            "the wait for the window is the wait it always was"
        );
    }

    /// The steps the shell has words for are the steps Valve has words for,
    /// less the ones that mean the game itself is opening.
    ///
    /// The list was read out of the client's own localisation files on
    /// 2026-09-04 — every `#LaunchApp_Action_<task>` it ships. What matters
    /// about the omissions is not the wording: an unnamed step does not stop
    /// the patience, and `WaitingGameWindow` is the wait the patience exists
    /// to bound. Naming it would leave a screen up for ever over a game that
    /// never opened.
    #[test]
    fn the_steps_the_shell_names_are_the_ones_valve_names() {
        // The two that were reported, and the one the guide's card already had
        // a word for.
        assert_eq!(
            words_for("DownloadingDepots").map(|words| (words.line, words.verb, words.counts)),
            Some(("Downloading content", None, true)),
            "the game's own content, which the card calls by the game's own verb"
        );
        // The four Valve's own dialog formats with a percentage, and no others.
        for task in [
            "DownloadingDepots",
            "DownloadingWorkshop",
            "ProcessingShaderCache",
            "DelayLaunch",
        ] {
            assert_eq!(words_for(task).map(|words| words.counts), Some(true));
        }
        for task in [
            "RunningInstallScript",
            "SynchronizingCloud",
            "VerifyingFiles",
            "UpdatingDRM",
            "GettingLegacyKey",
            "UpdatingAppInfo",
        ] {
            assert_eq!(
                words_for(task).map(|words| words.counts),
                Some(false),
                "{task} counts nothing, whatever numbers the action carries"
            );
        }
        assert_eq!(
            words_for("ProcessingShaderCache").map(|words| words.verb),
            Some(Some(PREPARING))
        );
        assert_eq!(
            words_for("VerifyingFiles").map(|words| words.verb),
            Some(Some(CHECKING))
        );

        for task in [
            "Starting",
            "CreatingProcess",
            "WaitingGameWindow",
            "ShowEula",
        ] {
            assert_eq!(words_for(task), None, "{task} is not this screen's to name");
        }
        // And the tasks Valve's own launch window says nothing about either.
        for task in [
            "CheckShaderDepotManifest",
            "SynchronizingStats",
            "SynchronizingControllerConfig",
            "SiteLicenseSeatCheckout",
            "ShowInterstitials",
            "UnlockingH264",
            "ProcessingInstallScript",
            "Completed",
            "Failed",
        ] {
            assert_eq!(words_for(task), None, "{task} has no word in Steam either");
        }
    }

    #[test]
    fn the_wait_for_the_window_starts_again_when_the_download_ends() {
        let t0 = Instant::now();
        let mut splash = game(t0);
        splash.now_starting_through_steam(t0);
        splash.now_fetching(
            t0,
            "Updating · 10% of 1.3 GB".to_string(),
            130_000_000,
            false,
        );
        let ended = at(t0, 600.0);
        splash.done_fetching(ended);
        assert_eq!(splash.doing(), Some(Doing::Game));
        assert_eq!(splash.said(), None, "there is nothing being fetched now");
        assert!(!splash.is_fetching(), "and nothing left to walk away from");
        assert!(!splash.may_be_left(), "a game that is starting is starting");

        assert_eq!(
            splash.advance(at(t0, 600.0 + STEAM_PATIENCE - 1.0), &[], "", true),
            None
        );
        assert_eq!(
            splash.advance(at(t0, 600.0 + STEAM_PATIENCE), &[], "", true),
            Some(Arrival::GaveUp)
        );
    }

    /// A game that opened, was played and was closed is not a game that never
    /// opened — and the loading screen does not come back over the bar the
    /// user has just been handed.
    ///
    /// Reported from a real session: closing a game, from the guide or from
    /// the game's own menu, put "Starting the game" back on the screen and
    /// then a panel saying Steam had not opened it.
    #[test]
    fn a_game_that_was_closed_is_not_a_game_that_never_opened() {
        let t0 = Instant::now();
        let mut splash = game(t0);
        splash.now_starting_through_steam(t0);

        // It opens.
        assert_eq!(
            splash.advance(at(t0, 6.0), &[7, 99], "smb", true),
            Some(Arrival::Window)
        );
        // And is closed again while the splash is still watching — which it is
        // for `STEAM_WATCHING` after every window it hands over to, and a game
        // that swaps windows as it loads keeps restarting that.
        assert_eq!(
            splash.advance(at(t0, 9.0), &[7], "", true),
            Some(Arrival::Left),
            "a window that stood for three seconds and went is the game leaving"
        );

        // Nothing of the loading screen comes back: what is behind it is the
        // display the user has just been given back.
        assert_eq!(splash.fade(at(t0, 9.0)), 0.0);
        assert_eq!(splash.blackout(at(t0, 9.0)), 0.0);
        assert!(!splash.waiting());
        // It is over at once, with nothing left to watch for.
        assert!(splash.finished(at(t0, 9.0)));
        // And it never says the game did not open. It opened; the user closed
        // it.
        assert!(
            !splash.steam_never_appeared(),
            "a game that was played and closed was reported as never opening"
        );
    }

    /// And the case the coming-back is *for* still works: a window thrown away
    /// on the way to the real one is one application, not two.
    ///
    /// The difference is how long it stood. What Valve's client and X11
    /// toolkits discard, they discard at once — inside the dip the splash is
    /// still playing — and a window somebody looked at is not that.
    #[test]
    fn a_window_thrown_away_at_once_is_still_the_same_launch() {
        let t0 = Instant::now();
        let mut splash = game(t0);
        splash.now_starting_through_steam(t0);

        assert_eq!(
            splash.advance(at(t0, 6.0), &[7, 99], "smb", true),
            Some(Arrival::Window)
        );
        // The toolkit throws its first window away, well inside `STAYED`.
        assert_eq!(splash.advance(at(t0, 6.4), &[7], "", true), None);
        assert!(
            splash.waiting(),
            "the splash did not come back for the swap"
        );
        assert_eq!(splash.fade(at(t0, 6.4)), 1.0);
        // And builds the one it meant.
        assert_eq!(
            splash.advance(at(t0, 6.6), &[7, 100], "smb", true),
            Some(Arrival::Window)
        );
        assert!(!splash.waiting());
        assert!(!splash.steam_never_appeared());
    }

    /// The whole point: the press is answered at once and the waiting happens
    /// inside the answer.
    #[test]
    fn the_panel_is_out_of_its_tile_before_the_application_exists() {
        let t0 = Instant::now();
        let mut splash = launch(t0);

        assert_eq!(splash.open(t0), 0.0);
        assert!(splash.open(at(t0, OPEN * 0.5)) > 0.4);
        assert_eq!(splash.open(at(t0, OPEN)), 1.0);
        // And it stays: nothing has arrived, so nothing is handed over.
        assert_eq!(splash.fade(at(t0, 5.0)), 1.0);
        assert!(!splash.finished(at(t0, 5.0)));
        assert!(splash.waiting());
        assert_eq!(splash.advance(at(t0, 5.0), &[7], "", true), None);
    }

    /// A window that was not there before is the application arriving. The
    /// splash then holds a moment — a mapped window is not a drawn one — and
    /// fades off it.
    #[test]
    fn a_new_window_hands_the_screen_over() {
        let t0 = Instant::now();
        let mut splash = launch(t0);

        assert_eq!(
            splash.advance(at(t0, 1.0), &[7, 9], "", true),
            Some(Arrival::Window)
        );
        assert!(!splash.waiting());
        // Reported once, not on every frame after.
        assert_eq!(splash.advance(at(t0, 1.1), &[7, 9], "", true), None);

        assert_eq!(splash.fade(at(t0, 1.0)), 1.0, "it holds first");
        let fading = splash.fade(at(t0, 1.0 + SETTLE + HANDOVER * 0.5));
        assert!(fading > 0.0 && fading < 1.0);

        // Faded off, and so drawing nothing — but not done with: it goes on
        // watching for a moment in case what it handed over to goes away.
        let faded = at(t0, 1.0 + SETTLE + HANDOVER);
        assert!(!splash.drawing(faded));
        assert!(!splash.finished(faded));
        assert!(splash.finished(at(t0, 1.0 + WATCHING + 1.0)));
        // An application dissolves off its window; it does not dip.
        assert_eq!(splash.blackout(at(t0, 1.0 + SETTLE)), 0.0);
        assert!(!splash.uncovering(at(t0, 1.0 + SETTLE)));
    }

    /// A game does not dissolve off its window — it dips through black, and
    /// the picture behind it has to hold the whole way down.
    ///
    /// Not decoration. The hero a game's splash stands on is drawn on the
    /// background layer, under every window, so the instant the game maps its
    /// own the picture is gone; a cross-fade would spend its whole length
    /// showing a title floating over an unrevealed game. Black is the only
    /// thing the shell can hold over the window while that happens.
    #[test]
    fn a_game_hands_the_screen_over_through_black() {
        let t0 = Instant::now();
        let mut splash = game(t0);
        assert_eq!(
            splash.advance(at(t0, 1.0), &[7, 9], "", false),
            Some(Arrival::Window)
        );

        // Down: the splash goes as the black comes, and is gone by the time
        // the screen is — nothing of the shell's is drawn on a black screen.
        let half = at(t0, 1.0 + BLACK_IN * 0.5);
        assert!(splash.fade(half) > 0.0 && splash.fade(half) < 1.0);
        assert!(splash.blackout(half) > 0.0 && splash.blackout(half) < 1.0);
        assert!(!splash.uncovering(half), "it is still on its own picture");

        // Black, and held there: a mapped window is a long way from a drawn
        // one, and this is the stretch that costs nothing to wait through.
        for held in [0.0, BLACK_HOLD * 0.5] {
            let now = at(t0, 1.0 + BLACK_IN + held);
            assert_eq!(splash.blackout(now), 1.0, "held {held}");
            assert_eq!(splash.fade(now), 0.0);
            assert!(splash.drawing(now), "the display stopped drawing on black");
            assert!(!splash.finished(now));
        }

        // The way up is the half that shows the game, so the shell must stop
        // painting the picture it came from — and the screen has to be solid
        // black at the moment it does, or dropping the picture is a flash of
        // the game a frame before the reveal.
        let turn = at(t0, 1.0 + BLACK_IN + BLACK_HOLD);
        assert!(splash.uncovering(turn));
        assert!(splash.blackout(turn) > 0.999, "{}", splash.blackout(turn));
        let up = at(t0, 1.0 + BLACK_IN + BLACK_HOLD + BLACK_OUT * 0.5);
        let showing = splash.blackout(up);
        assert!(showing > 0.0 && showing < 1.0, "{showing}");
        assert!(splash.drawing(up));

        // And then it is off the game entirely.
        let done = at(t0, 1.0 + BLACK_IN + BLACK_HOLD + BLACK_OUT);
        assert_eq!(splash.blackout(done), 0.0);
        assert!(!splash.drawing(done));
        assert!(!splash.finished(done), "it is still watching");
        assert!(splash.finished(at(t0, 1.0 + STEAM_WATCHING + 1.0)));
    }

    /// Watching is not being on the screen, and for a game the difference is
    /// seconds long.
    ///
    /// The shell asks both questions and they are not interchangeable. What is
    /// still *drawn* is what stands in front of the user: while any of it is
    /// there the Home button is refused, the bar holds the overlay layer above
    /// the application, and the display keeps redrawing. What still *exists*
    /// is only a claim on the display, kept so a hand-over to the wrong window
    /// can be taken back.
    ///
    /// Asking the second where the first was meant cost the user five seconds
    /// of a dead Home button after every game they started, with the game
    /// already on screen in front of them and the shell still holding the
    /// layer above it.
    #[test]
    fn a_faded_splash_is_watching_rather_than_standing_in_front() {
        let t0 = Instant::now();
        let mut splash = game(t0);
        assert_eq!(
            splash.advance(at(t0, 1.0), &[7, 9], "", false),
            Some(Arrival::Window)
        );

        // The dip is the last of it on the screen, and it is over in about a
        // second.
        let up = 1.0 + BLACK_IN + BLACK_HOLD + BLACK_OUT;
        assert!(splash.drawing(at(t0, up - 0.01)));
        assert!(!splash.drawing(at(t0, up)));

        // And then it is out of the way for seconds while it goes on watching
        // — which is the whole of what this test is about.
        assert!(!splash.finished(at(t0, up)));
        assert!(!splash.drawing(at(t0, 1.0 + STEAM_WATCHING - 0.01)));
        assert!(
            STEAM_WATCHING - (up - 1.0) > 5.0,
            "the gap the shell must not spend in front of the user"
        );
        assert!(splash.finished(at(t0, 1.0 + STEAM_WATCHING)));
    }

    /// Which window a game turned out to be is knowable at exactly one moment
    /// — this one — and nothing can work it out afterwards: a window carries
    /// the class its binary announces and nothing that says who started it.
    /// It is what lets the guide offer to close a game by its own name rather
    /// than by whatever a Proton binary happens to be called.
    #[test]
    fn a_launch_says_which_windows_it_turned_out_to_be() {
        let t0 = Instant::now();
        let mut splash = game(t0);

        // Nothing new yet, so nothing to claim.
        assert_eq!(
            splash.newcomers(&[7]).collect::<Vec<u32>>(),
            Vec::<u32>::new()
        );

        assert_eq!(
            splash.advance(at(t0, 1.0), &[7, 9, 11], "", false),
            Some(Arrival::Window)
        );
        // The two that were not there, and not the one that was — a window
        // somebody else already had is not this launch's to name.
        assert_eq!(
            splash.newcomers(&[7, 9, 11]).collect::<Vec<_>>(),
            vec![9, 11]
        );

        // And it goes on answering after the hand-over, which is what the
        // shell asks of it: the first window is not always the game — a
        // launcher is swapped for the game itself, an anti-cheat installer
        // comes and goes — and a shell that asked only once would be left
        // holding the id of a window that had already closed.
        assert_eq!(splash.newcomers(&[7, 13]).collect::<Vec<_>>(), vec![13]);
    }

    /// A game that never appeared has nothing at the other end of a dip — what
    /// is behind the splash is the bar the press came from. So it fades off
    /// the way an application's does, and the user is told what went wrong
    /// rather than being shown a second of black for no reason.
    #[test]
    fn a_game_that_never_arrives_does_not_dip() {
        let t0 = Instant::now();
        let mut splash = game(t0);
        // Asked for, so the wait is the game's rather than the client's.
        splash.now_starting_through_steam(t0);
        assert_eq!(
            splash.advance(at(t0, STEAM_PATIENCE), &[7], "", true),
            Some(Arrival::GaveUp)
        );
        for after in [0.0, BLACK_IN, BLACK_IN + BLACK_HOLD] {
            assert_eq!(splash.blackout(at(t0, STEAM_PATIENCE + after)), 0.0);
            assert!(!splash.uncovering(at(t0, STEAM_PATIENCE + after)));
        }
        assert!(splash.steam_never_appeared());
        assert!(splash.finished(at(t0, STEAM_PATIENCE + SETTLE + HANDOVER + 0.01)));
    }

    /// What the line beside the indicator says follows what is actually being
    /// waited for, and the moment it stops being Valve's client is the moment
    /// the game is asked for.
    #[test]
    fn the_splash_says_which_step_of_starting_a_game_it_is_on() {
        let t0 = Instant::now();
        // An ordinary application has nothing to say: its panel carries its
        // name and its icon already.
        assert_eq!(launch(t0).doing(), None);

        let mut splash = game(t0);
        assert_eq!(splash.doing(), Some(Doing::Steam));
        splash.now_starting_through_steam(at(t0, 4.0));
        assert_eq!(splash.doing(), Some(Doing::Game));
    }

    /// The hand-over is a bet that the window which appeared is the
    /// application, and it is sometimes lost: a toolkit throws its first
    /// window away and builds the one it meant, a game swaps its window for a
    /// fullscreen one. The display is the bare bar in between.
    ///
    /// So the splash comes back rather than leaving the user looking at it.
    /// This is the whole reason it outlives its own fade.
    #[test]
    fn a_window_that_goes_away_again_brings_the_splash_back() {
        let t0 = Instant::now();
        let mut splash = launch(t0);

        assert_eq!(
            splash.advance(at(t0, 1.0), &[7, 9], "", true),
            Some(Arrival::Window)
        );
        let faded = at(t0, 1.0 + SETTLE + HANDOVER);
        assert!(!splash.drawing(faded), "it has handed the screen over");

        // And the display is empty again: the window it handed over to has
        // gone and nothing took its place.
        assert_eq!(splash.advance(faded, &[7], "", true), None);
        assert!(splash.waiting(), "it is waiting for the application again");
        assert_eq!(splash.fade(faded), 1.0, "and is back on the screen");
        assert!(!splash.finished(faded));

        // The real window, second time round, is an arrival like any other.
        assert_eq!(
            splash.advance(at(t0, 3.0), &[7, 11], "", true),
            Some(Arrival::Window)
        );
    }

    /// It only comes back for an empty display. A game that swaps which window
    /// it is showing, or one whose foreground the compositor renames, has not
    /// gone anywhere and must not be interrupted by a loading screen.
    #[test]
    fn a_display_that_still_has_something_on_it_keeps_the_screen() {
        let t0 = Instant::now();
        let mut splash = launch(t0);
        splash.advance(at(t0, 1.0), &[7, 9], "", true);
        let faded = at(t0, 1.0 + SETTLE + HANDOVER);

        // A different new window, but still a new window.
        assert_eq!(splash.advance(faded, &[7, 11], "", true), None);
        assert!(!splash.waiting(), "it has not gone back to waiting");

        // Nothing new, but something named in front — an application that
        // raised what it already had.
        let mut splash = launch(t0);
        splash.advance(at(t0, 1.0), &[7, 9], "", true);
        assert_eq!(splash.advance(faded, &[7], "Celeste", true), None);
        assert!(!splash.waiting());
    }

    /// A launch that ended because nothing was ever coming does not un-end.
    /// There is no window to lose, and a splash that came back would be
    /// waiting for something already known not to exist.
    #[test]
    fn a_launch_that_gave_up_stays_given_up() {
        let t0 = Instant::now();
        let mut splash = launch(t0);
        assert_eq!(
            splash.advance(at(t0, PATIENCE + 1.0), &[7], "", true),
            Some(Arrival::GaveUp)
        );
        assert_eq!(splash.advance(at(t0, PATIENCE + 1.1), &[7], "", true), None);
        assert!(!splash.waiting(), "it is not waiting for anything now");
        assert!(splash.finished(at(t0, PATIENCE + 1.0 + SETTLE + HANDOVER)));
    }

    /// An application that was already running opens no second window; it
    /// raises the one it has, and the foreground changing is the only sign.
    #[test]
    fn an_application_that_was_already_running_still_hands_over() {
        let t0 = Instant::now();
        let mut splash = launch(t0);
        assert_eq!(
            splash.advance(at(t0, 0.4), &[7], "Celeste", true),
            Some(Arrival::Raised)
        );
    }

    /// A launch that dies must not leave the display behind a splash for the
    /// whole of its patience.
    #[test]
    fn a_launch_that_dies_takes_its_splash_with_it() {
        let t0 = Instant::now();
        let mut splash = launch(t0);
        assert_eq!(splash.advance(at(t0, 0.2), &[7], "", true), None);
        assert_eq!(
            splash.advance(at(t0, 0.5), &[7], "", false),
            Some(Arrival::Gone)
        );
        assert!(splash.finished(at(t0, 0.5 + SETTLE + HANDOVER)));
        assert!(!splash.steam_never_appeared());
    }

    /// A game started through Valve's client has no process of ours, and the
    /// one that carried the request exits at once. Reading that as the game
    /// dying would take the splash away before the game had begun to load.
    #[test]
    fn a_steam_launch_is_not_dead_merely_because_nothing_of_ours_is_running() {
        let t0 = Instant::now();
        let mut splash = launch(t0).through_steam(504230);

        assert_eq!(splash.advance(at(t0, 0.5), &[7], "", false), None);
        assert_eq!(splash.advance(at(t0, 30.0), &[7], "", false), None);
        assert!(splash.waiting(), "the splash gave up on a live launch");

        // And the window still hands the screen over, exactly as it would.
        assert_eq!(
            splash.advance(at(t0, 45.0), &[7, 9], "", false),
            Some(Arrival::Window)
        );
        assert!(!splash.steam_never_appeared());
    }

    /// A game started through Heroic, on both of the terms a Steam launch
    /// has: the process that carried the request exits at once where Heroic
    /// is already running, which is not the game dying, and the wait is
    /// Heroic's two minutes rather than an application's twenty seconds.
    #[test]
    fn a_heroic_launch_outlives_its_courier_and_waits_its_own_patience() {
        let t0 = Instant::now();
        let mut splash = launch(t0).through_heroic();

        assert_eq!(splash.advance(at(t0, 0.5), &[7], "", false), None);
        assert_eq!(
            splash.advance(at(t0, PATIENCE + 5.0), &[7], "", false),
            None,
            "it gave up at an ordinary launch's patience"
        );
        assert_eq!(
            splash.advance(at(t0, HEROIC_PATIENCE - 1.0), &[7, 9], "", false),
            Some(Arrival::Window)
        );

        let mut never = launch(t0).through_heroic();
        assert_eq!(
            never.advance(at(t0, HEROIC_PATIENCE + 1.0), &[7], "", false),
            Some(Arrival::GaveUp)
        );
        assert!(never.heroic_never_started_it(), "and that is said");
        assert!(!splash.heroic_never_started_it());
    }

    /// Where no Heroic was running, the process the shell started is Heroic,
    /// and it going with no window is the launch over — said at once, not
    /// two minutes later. Where one was, its going means nothing.
    #[test]
    fn a_heroic_that_goes_without_a_window_is_a_game_that_did_not_start() {
        let t0 = Instant::now();
        let mut cold = launch(t0).through_heroic().heroic_already_running(false);
        assert_eq!(cold.advance(at(t0, 1.0), &[7], "", true), None);
        assert_eq!(
            cold.advance(at(t0, 2.6), &[7], "", false),
            Some(Arrival::Gone)
        );
        assert!(cold.heroic_never_started_it());

        let mut handed = launch(t0).through_heroic().heroic_already_running(true);
        assert_eq!(handed.advance(at(t0, 2.6), &[7], "", false), None);
        assert_eq!(
            handed.advance(at(t0, 20.0), &[7, 9], "", false),
            Some(Arrival::Window)
        );
        assert!(!handed.heroic_never_started_it());

        // Only ever a Heroic launch.
        let plain = launch(t0).heroic_already_running(false);
        assert!(!plain.heroic_never_started_it());
    }

    /// An Epic game opens the way a Steam game does — on its own picture,
    /// with its logo, dipping through black onto its window — without being
    /// a Steam game: nothing Steam-shaped is asked of it.
    #[test]
    fn an_epic_game_opens_on_its_own_picture_like_a_steam_game() {
        let t0 = Instant::now();
        let logo = PathBuf::from("/cache/lxb/epic-art/Quail/logo.png");
        let mut splash = launch(t0)
            .through_heroic()
            .on_its_own_picture(Some(logo.clone()));
        assert!(splash.drawn_as_a_game());
        assert_eq!(splash.game(), None, "not a Steam game");
        assert_eq!(splash.logo_file(), Some(logo.as_path()));
        assert_eq!(splash.doing(), Some(Doing::Game));
        assert!(!launch(t0).drawn_as_a_game(), "an application is a panel");

        assert_eq!(
            splash.advance(at(t0, 30.0), &[7, 9], "", false),
            Some(Arrival::Window)
        );
        assert_eq!(splash.blackout(at(t0, 30.0)), 0.0);
        assert!(
            splash.blackout(at(t0, 30.0 + BLACK_IN)) > 0.99,
            "it dips through black onto the game, as a Steam game's does"
        );
        assert!(splash.uncovering(at(t0, 30.0 + BLACK_IN + BLACK_HOLD + 0.1)));
    }

    /// It waits far longer than an ordinary launch: the client may update the
    /// game, build a Proton prefix or unpack a shader cache first, and none of
    /// that is failure. But it does not wait for ever.
    #[test]
    fn a_steam_launch_waits_minutes_and_then_says_so() {
        let t0 = Instant::now();
        let mut splash = launch(t0).through_steam(504230);
        splash.now_starting_through_steam(t0);

        assert_eq!(
            splash.advance(at(t0, PATIENCE + 1.0), &[7], "", true),
            None,
            "it gave up at an ordinary launch's patience"
        );
        assert_eq!(
            splash.advance(at(t0, STEAM_PATIENCE), &[7], "", true),
            Some(Arrival::GaveUp)
        );
        assert!(
            splash.steam_never_appeared(),
            "nothing will tell the user their press went nowhere"
        );
    }

    /// The wait for Valve's client is not the wait for the game, and it is the
    /// longer of the two.
    ///
    /// This is the one the shell got wrong. A cold client is allowed two and a
    /// half minutes to come up and sign in, and the loading screen watching for
    /// it gave up after one: it announced that the game had not started, and
    /// then the client came up and the game started, over the panel saying it
    /// had not. Both numbers are honest on their own; what was wrong was one
    /// clock covering two waits.
    #[test]
    fn the_wait_for_the_client_is_not_the_wait_for_the_game() {
        let t0 = Instant::now();
        let mut splash = launch(t0).through_steam(504230);

        assert_eq!(
            splash.advance(at(t0, STEAM_PATIENCE + 1.0), &[7], "", true),
            None,
            "it gave up on the client at the game's patience"
        );
        assert_eq!(
            splash.advance(at(t0, STEAM_CLIENT_PATIENCE), &[7], "", true),
            Some(Arrival::GaveUp)
        );
        assert!(
            splash.gave_up_waiting_for_the_client(),
            "the panel cannot tell which of the two waits ended"
        );
    }

    /// And the game's own patience begins when the game is asked for, not when
    /// the press was made — however long the client took to answer.
    #[test]
    fn the_game_gets_its_whole_patience_after_a_slow_client() {
        let t0 = Instant::now();
        let mut splash = launch(t0).through_steam(504230);

        // Most of the client's patience spent, and then the game is asked for.
        let asked = STEAM_CLIENT_PATIENCE - 10.0;
        assert_eq!(splash.advance(at(t0, asked), &[7], "", true), None);
        splash.now_starting_through_steam(at(t0, asked));

        assert_eq!(
            splash.advance(at(t0, asked + STEAM_PATIENCE - 1.0), &[7], "", true),
            None,
            "the game was given the client's leftovers rather than its own wait"
        );
        assert_eq!(
            splash.advance(at(t0, asked + STEAM_PATIENCE), &[7], "", true),
            Some(Arrival::GaveUp)
        );
        assert!(
            !splash.gave_up_waiting_for_the_client(),
            "a game that never appeared is reported as a client that never came up"
        );
    }

    /// The press opens the panel once, and only once. A game going through
    /// Valve's client is asked for a second time — after the client is up —
    /// and that has to move the patience without touching what is on screen.
    /// Measuring both from one clock made the panel grow out of its tile
    /// again, which reads as a second application opening.
    #[test]
    fn handing_the_game_over_does_not_open_the_panel_a_second_time() {
        let t0 = Instant::now();
        let mut splash = launch(t0).through_steam(504230);
        assert_eq!(splash.open(at(t0, OPEN)), 1.0);

        // Four seconds in, the client is up and the game is asked for.
        splash.now_starting_through_steam(at(t0, 4.0));
        assert_eq!(
            splash.open(at(t0, 4.0)),
            1.0,
            "the panel grew out of its tile a second time"
        );
        assert_eq!(splash.open(at(t0, 4.0 + OPEN * 0.5)), 1.0);

        // And the wait for the window now runs from when it was asked for,
        // which is the whole reason there are two clocks.
        assert_eq!(
            splash.advance(at(t0, 4.0 + STEAM_PATIENCE - 1.0), &[7], "", true),
            None
        );
        assert_eq!(
            splash.advance(at(t0, 4.0 + STEAM_PATIENCE), &[7], "", true),
            Some(Arrival::GaveUp)
        );
    }

    /// And one that neither dies nor appears is eventually let go, rather than
    /// holding the display for ever.
    #[test]
    fn a_launch_that_never_appears_is_given_up_on() {
        let t0 = Instant::now();
        let mut splash = launch(t0);
        assert_eq!(splash.advance(at(t0, PATIENCE - 1.0), &[7], "", true), None);
        assert_eq!(
            splash.advance(at(t0, PATIENCE), &[7], "", true),
            Some(Arrival::GaveUp)
        );
    }

    /// A window belonging to something else — one that was already on the
    /// display — is not the application this splash is waiting for.
    #[test]
    fn windows_that_were_already_there_are_not_the_arrival() {
        let t0 = Instant::now();
        let mut splash = launch(t0);
        assert_eq!(splash.advance(at(t0, 1.0), &[7], "", true), None);
        // Nor is the foreground it already had.
        let mut settled = Launch::new(
            "Celeste".to_string(),
            None,
            crate::Display::for_a_test(0),
            [0.0; 4],
            None,
            t0,
            Before {
                windows: &[7],
                foreground: "Hollow Knight",
            },
        );
        assert_eq!(
            settled.advance(at(t0, 1.0), &[7], "Hollow Knight", true),
            None
        );
    }

    /// Valve's client opened for its own window waits as a game does — the
    /// client's patience until the request is taken, the window's after — and
    /// is drawn as an application: no dip, and the ordinary fade onto what
    /// arrived.
    ///
    /// The press this is about: Open Steam on a machine where the client is
    /// not running. The client takes most of a minute to come up and sign in,
    /// and the shell used to answer that with nothing on the screen, so the
    /// press read as ignored and was made again.
    #[test]
    fn the_client_opened_for_its_own_window_waits_like_a_game_and_looks_like_an_application() {
        let t0 = Instant::now();
        let mut splash = Launch::new(
            "Steam".to_string(),
            None,
            crate::Display::for_a_test(0),
            [0.0; 4],
            None,
            t0,
            Before {
                windows: &[7],
                foreground: "",
            },
        )
        .for_valves_client(lxb_steam::Doing::Open);
        assert_eq!(splash.game(), None, "the client is not a game");
        assert_eq!(splash.asked_of_the_client(), Some(lxb_steam::Doing::Open));
        assert!(
            splash.may_be_left(),
            "the cold client is the one wait worth leaving"
        );

        // No process of ours exists, so nothing dying means anything; and the
        // ordinary patience is far too short for a client coming up.
        assert_eq!(
            splash.advance(at(t0, PATIENCE + 1.0), &[7], "", false),
            None
        );
        assert_eq!(
            splash.advance(at(t0, STEAM_PATIENCE + 1.0), &[7], "", true),
            None
        );

        // The client takes the request: from here the wait is for its window.
        splash.now_starting_through_steam(at(t0, 40.0));
        assert!(
            !splash.may_be_left(),
            "a window seconds away is not a wait to leave"
        );
        assert_eq!(splash.advance(at(t0, 41.0), &[7], "", true), None);

        // Which arrives as any application's does.
        assert_eq!(
            splash.advance(at(t0, 44.0), &[7, 9], "Steam", true),
            Some(Arrival::Window)
        );
        assert_eq!(
            splash.blackout(at(t0, 44.1)),
            0.0,
            "a client is not dipped to"
        );
        assert_eq!(splash.fade(at(t0, 44.0 + SETTLE + HANDOVER + 0.05)), 0.0);
        assert!(!splash.steam_never_appeared());
    }

    /// And the one arrival the difference cannot see: the client brought
    /// forward a window it already had. The shell's watch on Valve's windows
    /// says so, once, and a splash already answered ignores it.
    #[test]
    fn a_window_the_client_brought_forward_answers_the_splash_once() {
        let t0 = Instant::now();
        let mut splash = Launch::new(
            "Steam".to_string(),
            None,
            crate::Display::for_a_test(0),
            [0.0; 4],
            None,
            t0,
            Before {
                windows: &[7],
                foreground: "Steam",
            },
        )
        .for_valves_client(lxb_steam::Doing::Open);
        splash.now_starting_through_steam(at(t0, 1.0));
        // Nothing changed on the display, so nothing here can tell.
        assert_eq!(splash.advance(at(t0, 2.0), &[7], "Steam", true), None);

        assert!(splash.the_window_was_raised(at(t0, 2.0)));
        assert!(!splash.waiting());
        assert!(!splash.the_window_was_raised(at(t0, 2.5)), "answered twice");
        assert_eq!(splash.fade(at(t0, 2.0 + SETTLE + HANDOVER + 0.05)), 0.0);

        // A client the wait ran out on says so, and says which wait.
        let mut never = Launch::new(
            "Steam".to_string(),
            None,
            crate::Display::for_a_test(0),
            [0.0; 4],
            None,
            t0,
            Before {
                windows: &[],
                foreground: "",
            },
        )
        .for_valves_client(lxb_steam::Doing::BigPicture);
        assert_eq!(
            never.advance(at(t0, STEAM_CLIENT_PATIENCE), &[], "", true),
            Some(Arrival::GaveUp)
        );
        assert!(never.steam_never_appeared());
        assert!(never.gave_up_waiting_for_the_client());
    }
}
