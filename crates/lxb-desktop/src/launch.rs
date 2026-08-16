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

use std::time::Instant;

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
}

/// An application that has been started and has not appeared yet.
pub struct Launch {
    /// What to call it while it starts.
    pub name: String,
    /// Its icon, by theme name; the slot is looked up when it is drawn.
    pub icon: Option<String>,
    /// The display it is opening on, and the tile it is opening out of.
    pub panel: usize,
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
        panel: usize,
        from: [f32; 4],
        pid: Option<u32>,
        now: Instant,
        before: Before<'_>,
    ) -> Self {
        Self {
            name,
            icon,
            panel,
            from,
            pid,
            through_steam: false,
            game: None,
            doing: None,
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

    /// The game this is opening, if it is a game at all.
    pub fn game(&self) -> Option<u32> {
        self.game
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
    pub fn now_starting_through_steam(&mut self, now: Instant) {
        self.waiting_since = now;
        self.doing = Some(Doing::Game);
    }

    /// Whether a game started through Valve's client never appeared.
    ///
    /// Only [`Arrival::GaveUp`] counts, and only for a Steam launch: a launch
    /// of ours that dies is caught by its process going away, and this one has
    /// no process of ours to watch.
    pub fn steam_never_appeared(&self) -> bool {
        self.through_steam && matches!(self.arrived, Some((_, Arrival::GaveUp)))
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
        if let Some((_, how)) = self.arrived {
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
                tracing::debug!(app = %self.name, "what the splash handed over to has gone again");
                self.arrived = None;
            }
            return None;
        }
        let waited = now.duration_since(self.waiting_since).as_secs_f32();
        let patience = if self.through_steam {
            STEAM_PATIENCE
        } else {
            PATIENCE
        };
        let how = if windows.iter().any(|id| !self.known.contains(id)) {
            Arrival::Window
        } else if !foreground.is_empty() && foreground != self.foreground {
            Arrival::Raised
        } else if !alive && !self.through_steam {
            // Not for a Steam launch. The process that carried the request
            // exits within milliseconds of being started — it has done its
            // whole job by then — and reading that as the game dying would
            // take the splash away before the game had begun to load.
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
            Some((at, Arrival::Window | Arrival::Raised)) if self.game.is_some() => {
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
        let Some((at, _)) = self.arrived else {
            return 1.0;
        };
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
        // watch for.
        if matches!(how, Arrival::Gone | Arrival::GaveUp) {
            return true;
        }
        let watching = if self.through_steam {
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
            0,
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
    }

    /// A game that never appeared has nothing at the other end of a dip — what
    /// is behind the splash is the bar the press came from. So it fades off
    /// the way an application's does, and the user is told what went wrong
    /// rather than being shown a second of black for no reason.
    #[test]
    fn a_game_that_never_arrives_does_not_dip() {
        let t0 = Instant::now();
        let mut splash = game(t0);
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

    /// It waits far longer than an ordinary launch: the client may update the
    /// game, build a Proton prefix or unpack a shader cache first, and none of
    /// that is failure. But it does not wait for ever.
    #[test]
    fn a_steam_launch_waits_minutes_and_then_says_so() {
        let t0 = Instant::now();
        let mut splash = launch(t0).through_steam(504230);

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
            0,
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
}
