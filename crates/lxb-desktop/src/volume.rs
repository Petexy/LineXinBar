//! The volume control the keys raise.
//!
//! The session's volume already has a control: the bar in the guide's sidebar,
//! which is where somebody who has come looking for it finds it. The keys are
//! the other half of the same thing — a hand that reaches for the speaker key
//! on a keyboard is a hand that does not want to open anything — and they are
//! pressed in the one place that bar cannot be seen, which is under a game
//! holding the whole display.
//!
//! So a press raises the control on its own, over whatever is in front, and
//! takes it away again shortly afterwards. What it raises is deliberately the
//! *same* picture the sidebar draws — see [`crate::ui::build_volume`] — because
//! a volume set two ways should not be two controls that look alike.
//!
//! Nothing about the value lives here; that is [`crate::system::Quick`], which
//! is where the machine's mixer is. This is only how long the picture of it
//! stays on screen, and it is kept apart for the reason every transition in
//! this shell is: what is on screen has to go on being drawn until it has
//! finished leaving, and a control that vanished the instant it stopped being
//! wanted would blink out under the user's hand.

use std::time::Instant;

/// How long the control takes to arrive, and to leave again.
///
/// It arrives faster than it goes. A control raised by a key has to be *there*
/// by the time the eye has moved to it — the value it is showing has already
/// changed — where the way out is the part somebody watches, and a fast one
/// reads as a flicker.
const RISE: f32 = 0.14;
const FALL: f32 = 0.28;

/// How long it stays after the last press.
///
/// Long enough to press again without the control leaving between presses, and
/// short enough that it is not standing over somebody's game a beat after they
/// stopped adjusting it. A quarter of the corner's [`crate::notify::DWELL`]:
/// an announcement is something to be read, and this is something already read
/// by the time it is finished arriving.
const DWELL: f32 = 1.0;

/// Which part of its life the control is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    In,
    Holding,
    Out,
}

impl Stage {
    /// How long this part of it lasts.
    fn over(self) -> f32 {
        match self {
            Stage::In => RISE,
            Stage::Holding => DWELL,
            Stage::Out => FALL,
        }
    }
}

/// The volume control raised by the keys, and where it has got to.
///
/// Empty for almost the whole of a session: nothing is drawn and nothing is
/// asked of the frame loop until a key is pressed.
#[derive(Debug, Default)]
pub struct Overlay {
    shown: Option<Shown>,
    /// How solid it is, 0 to 1, worked out by [`Overlay::advance`] so that
    /// everything drawn on one frame reads one answer.
    ///
    /// Linear, and eased where it is drawn — the same division the corner's
    /// bubbles make, and for the same reason: a transition turned around
    /// half-way through has to carry on from where it had got to, which is a
    /// question about progress rather than about how progress looks.
    fade: f32,
}

#[derive(Debug, Clone, Copy)]
struct Shown {
    stage: Stage,
    since: Instant,
}

impl Overlay {
    /// A volume key was pressed: raise the control, or start its wait again.
    ///
    /// A press while it is leaving turns it around rather than starting it
    /// over, which is what makes a second press half a second later look like
    /// one adjustment instead of two: the arrival begins from however solid
    /// the control already was.
    pub fn raise(&mut self, now: Instant) {
        let stage = match self.shown {
            // Already up: only the wait is restarted, so a run of presses is
            // one showing that outlasts the last of them.
            Some(Shown {
                stage: Stage::Holding,
                ..
            }) => Stage::Holding,
            _ if self.fade >= 1.0 => Stage::Holding,
            _ => Stage::In,
        };
        // Back-dated by however far in it already is, so nothing jumps.
        let done = if stage == Stage::In { self.fade } else { 0.0 };
        self.shown = Some(Shown {
            stage,
            since: now - std::time::Duration::from_secs_f32(stage.over() * done),
        });
    }

    /// Send it away early, wherever it has got to.
    ///
    /// The guide opening is what does this: its sidebar has this very control
    /// in it, and two of one bar on one screen is one of them a copy. It is
    /// *sent*, not taken — nothing in this shell vanishes before its
    /// transition has ended.
    pub fn dismiss(&mut self, now: Instant) {
        if let Some(shown) = &mut self.shown {
            if shown.stage != Stage::Out {
                shown.stage = Stage::Out;
                // From where it stands: a control dismissed while it was still
                // arriving has less of the way back to go.
                shown.since = now - std::time::Duration::from_secs_f32(FALL * (1.0 - self.fade));
            }
        }
    }

    /// Step it, and say whether the frame loop should go on drawing.
    ///
    /// True for the whole of a showing rather than only while something moves,
    /// which is the answer the corner gives too: the control is on screen over
    /// an application that would otherwise have stopped the shell drawing
    /// altogether, and the frame it leaves on is a frame somebody has to ask
    /// for.
    pub fn advance(&mut self, now: Instant) -> bool {
        // As many parts of its life as the time since the last pass covers,
        // and not one of them: a pass that ran long — a game starting, a
        // library landing — must leave the control where the clock says it is,
        // not one part behind for every slow frame. Each part begins where the
        // one before it was due to end rather than at `now`, for the same
        // reason: a wait that grew by however late the pass was would be a
        // control that outstays its welcome by a little more every time the
        // machine is busy.
        while let Some(shown) = self.shown {
            let over = shown.stage.over();
            let progress = now.saturating_duration_since(shown.since).as_secs_f32() / over;
            if progress < 1.0 {
                self.fade = match shown.stage {
                    Stage::In => progress,
                    Stage::Holding => 1.0,
                    Stage::Out => 1.0 - progress,
                };
                break;
            }
            let due = shown.since + std::time::Duration::from_secs_f32(over);
            self.shown = match shown.stage {
                Stage::In => Some(Shown {
                    stage: Stage::Holding,
                    since: due,
                }),
                Stage::Holding => Some(Shown {
                    stage: Stage::Out,
                    since: due,
                }),
                Stage::Out => {
                    self.fade = 0.0;
                    None
                }
            };
        }
        self.shown.is_some()
    }

    /// How solid the control is, 0 when it is not on screen at all.
    pub fn fade(&self) -> f32 {
        self.fade
    }

    /// Whether it is on screen — which is also what lifts this display's
    /// surface over the application underneath, and what keeps it drawing.
    pub fn on_screen(&self) -> bool {
        self.shown.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A moment past a boundary, so a test that means "once this part is over"
    /// is not asking whether two f32 durations happen to add up exactly.
    const BEAT: f32 = 0.005;

    fn secs(seconds: f32) -> Duration {
        Duration::from_secs_f32(seconds)
    }

    #[test]
    fn a_press_raises_it_and_it_leaves_on_its_own() {
        let start = Instant::now();
        let mut overlay = Overlay::default();
        assert!(
            !overlay.on_screen(),
            "nothing is drawn until a key is pressed"
        );
        assert!(!overlay.advance(start), "and no frames are asked for");

        overlay.raise(start);
        assert!(overlay.advance(start + secs(RISE * 0.5)));
        let arriving = overlay.fade();
        assert!(
            arriving > 0.0 && arriving < 1.0,
            "half way in, it is half there: {arriving}"
        );

        assert!(overlay.advance(start + secs(RISE + BEAT)));
        assert_eq!(overlay.fade(), 1.0);

        // Still there for the whole of its wait, and still asking for frames.
        assert!(overlay.advance(start + secs(RISE + DWELL * 0.5)));
        assert_eq!(overlay.fade(), 1.0);

        // Then it leaves, and is gone once it has finished leaving.
        assert!(overlay.advance(start + secs(RISE + DWELL + FALL * 0.5)));
        let leaving = overlay.fade();
        assert!(
            leaving > 0.0 && leaving < 1.0,
            "half way out, it is half gone: {leaving}"
        );
        assert!(overlay.on_screen(), "and still on screen while it goes");

        assert!(!overlay.advance(start + secs(RISE + DWELL + FALL + BEAT)));
        assert!(!overlay.on_screen());
        assert_eq!(overlay.fade(), 0.0);
    }

    /// A pass of the loop that arrives a whole showing late must not leave the
    /// control standing on the screen: what took it away was the clock, not
    /// the number of times anything was called.
    #[test]
    fn one_late_pass_can_carry_it_all_the_way_out() {
        let start = Instant::now();
        let mut overlay = Overlay::default();
        overlay.raise(start);
        assert!(!overlay.advance(start + secs(RISE + DWELL + FALL + BEAT)));
        assert!(!overlay.on_screen());
        assert_eq!(overlay.fade(), 0.0);
    }

    /// And a slow pass must not lengthen the wait either: each part of its
    /// life begins where the one before it was due to end.
    #[test]
    fn a_slow_pass_does_not_stretch_the_wait() {
        let start = Instant::now();
        let mut overlay = Overlay::default();
        overlay.raise(start);
        // A pass that lands well into the wait, and then one at the moment the
        // control is due to start leaving.
        assert!(overlay.advance(start + secs(RISE + DWELL * 0.9)));
        assert!(overlay.advance(start + secs(RISE + DWELL + FALL * 0.5)));
        assert!(
            overlay.fade() < 1.0,
            "it began leaving on time, not half a wait later"
        );
    }

    #[test]
    fn a_second_press_holds_it_where_it_is() {
        let start = Instant::now();
        let mut overlay = Overlay::default();
        overlay.raise(start);
        overlay.advance(start + secs(RISE + DWELL * 0.9));
        assert_eq!(overlay.fade(), 1.0);

        // Pressed again just before it would have gone: the wait starts over
        // from the press rather than from the first one.
        overlay.raise(start + secs(RISE + DWELL * 0.9));
        assert!(overlay.advance(start + secs(RISE + DWELL)));
        assert_eq!(overlay.fade(), 1.0, "it did not begin leaving");
        assert!(overlay.advance(start + secs(RISE + DWELL * 1.9 + BEAT)));
        assert!(
            overlay.fade() < 1.0,
            "and it does leave, a whole wait after the last press"
        );
    }

    #[test]
    fn a_press_while_it_is_leaving_turns_it_around() {
        let start = Instant::now();
        let mut overlay = Overlay::default();
        overlay.raise(start);
        overlay.advance(start + secs(RISE + DWELL + FALL * 0.5));
        let caught = overlay.fade();
        assert!(
            caught > 0.0 && caught < 1.0,
            "caught half way out: {caught}"
        );

        overlay.raise(start + secs(RISE + DWELL + FALL * 0.5));
        // Nothing jumps: the next frame is no dimmer than the one before it,
        // and no brighter than fully arrived.
        assert!(overlay.advance(start + secs(RISE + DWELL + FALL * 0.5)));
        let turned = overlay.fade();
        assert!(
            (turned - caught).abs() < 0.01,
            "it carried on from {caught}, not from {turned}"
        );
        assert!(overlay.advance(start + secs(RISE + DWELL + FALL * 0.5 + RISE)));
        assert_eq!(overlay.fade(), 1.0, "and it finishes arriving");
    }

    #[test]
    fn the_guide_sends_it_away_without_it_vanishing() {
        let start = Instant::now();
        let mut overlay = Overlay::default();
        overlay.raise(start);
        overlay.advance(start + secs(RISE + BEAT));
        assert_eq!(overlay.fade(), 1.0);

        overlay.dismiss(start + secs(RISE + BEAT));
        assert!(overlay.on_screen(), "it is sent away, not taken away");
        assert!(overlay.advance(start + secs(RISE + BEAT + FALL * 0.5)));
        assert!(overlay.fade() > 0.0);
        assert!(!overlay.advance(start + secs(RISE + BEAT + FALL + BEAT)));
        assert!(!overlay.on_screen());
    }

    #[test]
    fn dismissing_it_while_it_arrives_leaves_from_where_it_stands() {
        let start = Instant::now();
        let mut overlay = Overlay::default();
        overlay.raise(start);
        overlay.advance(start + secs(RISE * 0.5));
        let caught = overlay.fade();

        overlay.dismiss(start + secs(RISE * 0.5));
        assert!(overlay.advance(start + secs(RISE * 0.5)));
        assert!(
            (overlay.fade() - caught).abs() < 0.01,
            "it turned round where it was"
        );
        // And it has only that much of the way left to go.
        assert!(!overlay.advance(start + secs(RISE * 0.5 + FALL * caught + BEAT)));
    }

    #[test]
    fn dismissing_nothing_does_nothing() {
        let start = Instant::now();
        let mut overlay = Overlay::default();
        overlay.dismiss(start);
        assert!(!overlay.on_screen());
        assert!(!overlay.advance(start));
    }
}
