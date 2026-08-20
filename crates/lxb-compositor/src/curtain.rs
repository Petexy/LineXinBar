//! The black a session goes out behind.
//!
//! Turning the machine off used to be the one thing this session did with no
//! transition at all: the user chose it, the command ran, and the picture
//! stopped — at whatever moment the kernel got round to it, mid-frame, with
//! the menu still on screen. What that looks like is a display losing its
//! signal, and there is nothing on screen to tell it apart from a crash.
//!
//! So the screen is taken down first. A black sheet over every display, faded
//! in over [`FADE`], and the shell only asks for the machine once the black is
//! actually on screen — see [`Curtain::a_frame_was_drawn`] and
//! `lxb_shell_v1.screen_is_black`. What the user sees is the session leaving,
//! and everything after that happens behind a screen that is already black.
//!
//! It is drawn here rather than by the shell because it has to cover
//! *everything*. The shell owns one surface per display and an application in
//! front of it owns the screen; a shell blacking out its own surfaces would
//! take down the display its menu was on and leave a game playing on the
//! other. The compositor is the only thing in the session that is over all of
//! it, which is the same reason [`crate::flash`] lives here.
//!
//! ## Input
//!
//! While the curtain is anything but fully up the session takes no input at
//! all — see [`Curtain::holds_input`], which [`crate::input`] asks before it
//! looks at an event. Half a second is long enough to press something in, and
//! a machine on its way down must not be driven.
//!
//! That includes the bindings that would *leave* the session: a black screen
//! that swallowed `Ctrl+Alt+F2` as well is a session with no way out of it if
//! the shutdown it was drawn for never happens. The way out is the curtain
//! itself — whoever asked for it can take it back up, and the shell does
//! exactly that if the machine is still here long after being told to go. That
//! is deliberately the *asking* client's job rather than a timer here: a
//! compositor that lifted its own curtain would do it in the middle of a
//! shutdown that was merely slow, putting the session back on screen seconds
//! before it died.
//!
//! ## Frames
//!
//! Like the flash, how black it is now is a pure function of the clock,
//! sampled once per rendered frame. Unlike the flash, it carries a commit
//! counter that moves with it: a solid colour whose commit never changes is an
//! element the damage tracker considers unchanged, however much its colour
//! moved, so a fade drawn without one would appear at its first frame's alpha
//! and stay there over a screen with nothing else animating on it.
//!
//! The counter stops when the fade does, which is what makes a black screen
//! free: nothing changes, no damage is reported, no frame is queued, and the
//! last black frame stays on the display for as long as the machine takes.

use std::time::{Duration, Instant};

use smithay::backend::renderer::element::Id;
use smithay::backend::renderer::utils::CommitCounter;
use smithay::output::Output;

/// How long the screen takes to go black, and to come back off it.
///
/// Half a second, which is the length the user asked for and is about right
/// for what it is: long enough to read as the session leaving rather than as a
/// display giving up, short enough that nobody who has just chosen to turn the
/// machine off is kept waiting for an animation about it.
pub const FADE: Duration = Duration::from_millis(500);

/// One curtain, up or down, and how far it has got.
#[derive(Debug)]
struct Sheet {
    /// Stable for the life of the curtain, so the damage tracker sees one
    /// element changing rather than a new element every frame.
    id: Id,
    /// When it started moving, and which way.
    started: Instant,
    down: bool,
    /// How black it was when it started moving that way. A curtain turned
    /// round halfway carries on from where it is rather than jumping to the
    /// far end and running back.
    from: f32,
    /// The displays that have drawn a frame with the black fully over them, by
    /// name — a display can go away and come back underneath the fade, and a
    /// name compares equal to the display that returns.
    black: Vec<String>,
    /// Whether the client that asked has been told the screen is black, so it
    /// is told once rather than on every frame after.
    told: bool,
}

/// The session's curtain. There is one, over every display: this is a
/// statement about the session rather than about a screen.
#[derive(Debug, Default)]
pub struct Curtain {
    sheet: Option<Sheet>,
}

impl Curtain {
    /// Put the curtain down, or take it back up, from `now`.
    ///
    /// Asking for what is already happening is ignored rather than restarted,
    /// so a client that repeats itself does not put the fade back to the
    /// beginning.
    pub fn cover(&mut self, covered: bool, now: Instant) {
        if let Some(sheet) = &self.sheet {
            if sheet.down == covered {
                return;
            }
        } else if !covered {
            // Nothing to take up. There is no sheet at all in an ordinary
            // session, which is what keeps this free when it is not in use.
            return;
        }
        let from = self.black(now).map_or(0.0, |(_, black, _)| black);
        let id = match self.sheet.take() {
            // The same element carrying on in the other direction.
            Some(sheet) => sheet.id,
            None => Id::new(),
        };
        self.sheet = Some(Sheet {
            id,
            started: now,
            down: covered,
            from,
            black: Vec::new(),
            told: false,
        });
    }

    /// Whether the session is holding its input back: true from the frame the
    /// curtain starts down until it has finished coming back up.
    pub fn holds_input(&self) -> bool {
        self.sheet.is_some()
    }

    /// Drop a curtain that has finished coming up. Called once a pass of the
    /// session's loop, beside the flashes that have burnt out — and it is what
    /// makes [`Self::holds_input`] a question about a sheet existing.
    pub fn prune(&mut self, now: Instant) {
        let done = self
            .sheet
            .as_ref()
            .is_some_and(|sheet| !sheet.down && self.black(now).is_none());
        if done {
            tracing::info!("the curtain is up; the session takes input again");
            self.sheet = None;
        }
    }

    /// What to draw over a display now: the element's id, how black it is, and
    /// the commit that says it has changed since the last frame. `None` when
    /// there is nothing over the screen.
    pub fn black(&self, now: Instant) -> Option<(Id, f32, CommitCounter)> {
        let sheet = self.sheet.as_ref()?;
        let elapsed = now.saturating_duration_since(sheet.started);
        let black = travelled(sheet.from, sheet.down, elapsed);
        if black <= 0.0 {
            return None;
        }
        // Milliseconds since this curtain started moving, which stops moving
        // when it does — see the note on frames above.
        let commit = elapsed.min(FADE).as_millis() as usize;
        Some((sheet.id.clone(), black, CommitCounter::from(commit)))
    }

    /// Note that `output` has drawn a frame, and `started` is when that frame
    /// began — before its elements were built, so a frame counted as black was
    /// black for every element in it and not merely by the time it finished.
    ///
    /// Drawn rather than presented, which on the DRM backend is up to one
    /// retrace ahead of the screen. That is the honest limit of what the
    /// compositor knows here, and it is a sixteenth of a second at the end of
    /// a half-second fade the machine then spends whole seconds going down
    /// behind.
    pub fn a_frame_was_drawn(&mut self, output: &Output, started: Instant) {
        let Some(sheet) = &mut self.sheet else {
            return;
        };
        if !sheet.down {
            return;
        }
        let black = travelled(
            sheet.from,
            sheet.down,
            started.saturating_duration_since(sheet.started),
        );
        if black < 1.0 {
            return;
        }
        let name = output.name();
        if !sheet.black.contains(&name) {
            tracing::debug!(display = %name, "the curtain is down on this display");
            sheet.black.push(name);
        }
    }

    /// Whether every one of `outputs` has shown the black, and nobody has been
    /// told yet. Says so once: the answer is what sends the event, and an
    /// event sent every frame after would be a session shutting down twice.
    pub fn everything_is_black(&mut self, mut outputs: impl Iterator<Item = Output>) -> bool {
        let Some(sheet) = &mut self.sheet else {
            return false;
        };
        if !sheet.down || sheet.told {
            return false;
        }
        // A display with no frame in it yet is a display the black is not on.
        // With no displays at all this is vacuously true, which is the right
        // answer: a session with nothing to black out has nothing to wait for.
        if !outputs.all(|output| sheet.black.contains(&output.name())) {
            return false;
        }
        sheet.told = true;
        true
    }
}

/// How black the sheet is `elapsed` into a move that started at `from` and is
/// going to black (`down`) or off it, smoothstepped so neither end is a jump.
///
/// The whole [`FADE`] whichever direction it is going and however far it has
/// to travel. A curtain turned round after a moment therefore comes back
/// slower than it went, which is the cheap way round: the expensive way would
/// be to shorten the journey, and what that buys is a reversal that snaps.
fn travelled(from: f32, down: bool, elapsed: Duration) -> f32 {
    let to = if down { 1.0 } else { 0.0 };
    let t = smoothstep(elapsed.as_secs_f32() / FADE.as_secs_f32());
    (from + (to - from) * t).clamp(0.0, 1.0)
}

fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(name: &str) -> Output {
        Output::new(
            name.to_string(),
            smithay::output::PhysicalProperties {
                size: (0, 0).into(),
                subpixel: smithay::output::Subpixel::Unknown,
                make: "test".into(),
                model: "test".into(),
            },
        )
    }

    /// Nothing is drawn and nothing is held back in an ordinary session.
    #[test]
    fn a_session_nobody_is_leaving_has_no_curtain() {
        let mut curtain = Curtain::default();
        let now = Instant::now();
        assert!(curtain.black(now).is_none());
        assert!(!curtain.holds_input());
        // And asking for the curtain that is already up does not make one.
        curtain.cover(false, now);
        assert!(curtain.black(now).is_none());
    }

    /// It goes black over the fade and stays there, which is the difference
    /// between this and the screenshot flash.
    #[test]
    fn the_curtain_goes_black_and_stays() {
        let mut curtain = Curtain::default();
        let t0 = Instant::now();
        curtain.cover(true, t0);

        let (_, start, _) = curtain
            .black(t0 + Duration::from_millis(1))
            .expect("black from the first frame");
        assert!(start < 0.25, "the fade starts dark: {start}");
        let (_, half, _) = curtain.black(t0 + FADE / 2).unwrap();
        assert_eq!(half, 0.5, "half way across in half the time");
        assert_eq!(curtain.black(t0 + FADE).unwrap().1, 1.0);
        assert_eq!(
            curtain.black(t0 + FADE * 20).unwrap().1,
            1.0,
            "a curtain that came back off would show the machine going down"
        );
    }

    /// Neither end is a jump, and the ramp is not a straight line.
    #[test]
    fn the_fade_is_eased_at_both_ends() {
        assert!(travelled(0.0, true, FADE / 4) < 0.25);
        assert!(travelled(0.0, true, FADE * 3 / 4) > 0.75);
        assert_eq!(travelled(0.0, true, FADE / 2), 0.5);
    }

    /// The damage tracker has to see it move: a solid colour whose commit
    /// never changes is one the tracker leaves on screen exactly as it first
    /// drew it.
    #[test]
    fn every_frame_of_the_fade_carries_a_new_commit() {
        let mut curtain = Curtain::default();
        let t0 = Instant::now();
        curtain.cover(true, t0);
        let frame = |at| curtain.black(at).unwrap().2;
        assert_ne!(
            frame(t0 + Duration::from_millis(16)),
            frame(t0 + Duration::from_millis(32))
        );
        // And stops moving with the fade, so a black screen costs nothing.
        assert_eq!(frame(t0 + FADE), frame(t0 + FADE * 4));
    }

    /// Input is held from the first frame of the fade until the curtain is
    /// fully up again — including the whole of the way back up.
    #[test]
    fn the_session_takes_no_input_behind_the_curtain() {
        let mut curtain = Curtain::default();
        let t0 = Instant::now();
        assert!(!curtain.holds_input());
        curtain.cover(true, t0);
        assert!(curtain.holds_input());
        curtain.cover(false, t0 + FADE);
        assert!(curtain.holds_input(), "the screen is still black");
        curtain.prune(t0 + FADE + FADE / 2);
        assert!(curtain.holds_input(), "and still on its way up");
        curtain.prune(t0 + FADE * 3);
        assert!(!curtain.holds_input(), "and now the session is back");
    }

    /// Turned round, it carries on from where it is rather than jumping.
    #[test]
    fn a_curtain_taken_back_up_starts_from_where_it_got_to() {
        let mut curtain = Curtain::default();
        let t0 = Instant::now();
        curtain.cover(true, t0);
        let (id, half, _) = curtain.black(t0 + FADE / 2).unwrap();
        assert_eq!(half, 0.5);
        curtain.cover(false, t0 + FADE / 2);
        let (same, still, _) = curtain.black(t0 + FADE / 2).unwrap();
        assert_eq!(still, 0.5, "it did not jump to black on the way up");
        assert_eq!(same, id, "and it is the same element coming back");
        assert!(
            curtain.black(t0 + FADE / 2 + FADE).is_none(),
            "and then gone"
        );
    }

    /// Asking again for what is already happening does not restart it.
    #[test]
    fn asking_twice_does_not_put_the_fade_back_to_the_start() {
        let mut curtain = Curtain::default();
        let t0 = Instant::now();
        curtain.cover(true, t0);
        curtain.cover(true, t0 + FADE / 2);
        assert_eq!(curtain.black(t0 + FADE).unwrap().1, 1.0);
    }

    /// The screen is black when every display has drawn it, and said so once.
    #[test]
    fn the_black_is_reported_when_every_display_has_drawn_it() {
        let (a, b) = (output("A"), output("B"));
        let displays = || [a.clone(), b.clone()].into_iter();
        let mut curtain = Curtain::default();
        let t0 = Instant::now();
        curtain.cover(true, t0);
        assert!(
            !curtain.everything_is_black(displays()),
            "nothing drawn yet"
        );

        // A frame from before the fade ended is not a black one.
        curtain.a_frame_was_drawn(&a, t0 + FADE / 2);
        assert!(!curtain.everything_is_black(displays()));

        curtain.a_frame_was_drawn(&a, t0 + FADE);
        assert!(
            !curtain.everything_is_black(displays()),
            "one display of two is half a black screen"
        );
        curtain.a_frame_was_drawn(&b, t0 + FADE);
        assert!(curtain.everything_is_black(displays()));
        assert!(
            !curtain.everything_is_black(displays()),
            "a session cannot be told to go down twice"
        );
    }

    /// A curtain on its way up never reports anything: the event answers the
    /// request that put it down.
    #[test]
    fn a_curtain_going_up_says_nothing() {
        let out = output("A");
        let mut curtain = Curtain::default();
        let t0 = Instant::now();
        curtain.cover(true, t0);
        curtain.cover(false, t0 + FADE);
        curtain.a_frame_was_drawn(&out, t0 + FADE);
        assert!(!curtain.everything_is_black([out].into_iter()));
    }
}
