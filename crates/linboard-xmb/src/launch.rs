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

/// How long to wait for a window before concluding that none is coming.
///
/// Generous, because the alternative failure is worse: a splash torn away
/// from an application that was merely slow leaves the user staring at the
/// bar wondering whether their press did anything. A launch that dies is
/// caught by its process exiting rather than by this.
const PATIENCE: f32 = 20.0;

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
    started: Instant,
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
            started: now,
            known: before.windows.to_vec(),
            foreground: before.foreground.to_string(),
            arrived: None,
        }
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
        if self.arrived.is_some() {
            return None;
        }
        let waited = now.duration_since(self.started).as_secs_f32();
        let how = if windows.iter().any(|id| !self.known.contains(id)) {
            Arrival::Window
        } else if !foreground.is_empty() && foreground != self.foreground {
            Arrival::Raised
        } else if !alive {
            Arrival::Gone
        } else if waited >= PATIENCE {
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

    /// How much of the splash is left: 1 while it is waiting, falling to 0 as
    /// the application takes the screen.
    pub fn fade(&self, now: Instant) -> f32 {
        let Some((at, _)) = self.arrived else {
            return 1.0;
        };
        let since = now.duration_since(at).as_secs_f32();
        1.0 - ((since - SETTLE) / HANDOVER).clamp(0.0, 1.0)
    }

    /// Whether it is still waiting for the application, as against handing the
    /// screen over to one that has arrived. The indicator stops asking.
    pub fn waiting(&self) -> bool {
        self.arrived.is_none()
    }

    /// Nothing left to draw.
    pub fn finished(&self, now: Instant) -> bool {
        self.fade(now) <= 0.0
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
        assert!(splash.finished(at(t0, 1.0 + SETTLE + HANDOVER)));
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
