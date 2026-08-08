//! Bringing a window back: the flight from the tile that asked for it out to
//! the whole display.
//!
//! The overview flies windows *into* cards, and this is the same motion read
//! the other way — one window, starting at a rectangle the shell names, ending
//! where it really is. It exists because the alternative reads as a lie: a
//! shell that answered "open Firefox" with a splash panel and a spinner would
//! be reporting a load that is not happening, when the whole point is that the
//! application is already running and the user is being given back exactly the
//! window they left.
//!
//! Only the compositor can draw this, because only the compositor has the
//! window's pixels — which is why it is a request rather than something the
//! shell animates for itself.
//!
//! Like the overview, nothing here is ticked: progress is a pure function of
//! the clock, sampled once per rendered frame.

use std::time::Instant;

use smithay::output::Output;

use std::time::Duration;

use lxb_protocol::overview::{Rect, FLIGHT};

/// How long the window keeps being drawn in front after it has stopped moving.
///
/// The flight ends by definition when the window reaches its real geometry —
/// but "in front" is what the flight *is*, and dropping that on the same frame
/// puts the window back under the shell's overlay while the shell is still on
/// it. The shell steps down when its own copy of the flight ends, and the two
/// clocks are not the same clock: it is one process deciding on its draw and
/// another on its own. Measured without this, the display showed a single
/// frame of bare start screen between the two.
///
/// So the window stays in front, at full size and indistinguishable from the
/// ordinary drawing of it, for a few frames longer than anything needs.
const TAIL: Duration = Duration::from_millis(150);

/// One window on its way back out of a tile.
#[derive(Debug, Clone)]
struct Flight {
    window: u32,
    /// By name, because an output can go away underneath a flight and a name
    /// compares equal to the display that comes back.
    output: String,
    from: Rect,
    started: Instant,
}

/// Every window currently flying back, across all displays.
#[derive(Debug, Default)]
pub struct Restores {
    flights: Vec<Flight>,
}

impl Restores {
    /// Start `window` flying out of `from` on `output`.
    ///
    /// A window already flying is restarted rather than doubled: pressing the
    /// same tile twice is one application arriving, not two.
    pub fn begin(&mut self, window: u32, output: &Output, from: Rect, now: Instant) {
        self.flights.retain(|flight| flight.window != window);
        if from.w <= 0.0 || from.h <= 0.0 {
            // Nothing to grow out of. The window is still raised by the
            // caller; it simply appears, which is what an empty rectangle
            // asks for.
            return;
        }
        self.flights.push(Flight {
            window,
            output: output.name(),
            from,
            started: now,
        });
    }

    /// Where `window` is in its flight: the rectangle it started from, and how
    /// much of the way back it still has to go — 1.0 at the tile, 0.0 once it
    /// is at its real geometry. `None` when it is not flying.
    ///
    /// Smoothstepped, so it leaves the tile and settles into the display
    /// gently rather than starting and stopping at full speed.
    pub fn flight(&self, output: &Output, window: u32, now: Instant) -> Option<(Rect, f64)> {
        let flight = self
            .flights
            .iter()
            .find(|flight| flight.window == window && flight.output == output.name())?;
        if finished(flight, now) {
            return None;
        }
        let travelled =
            now.saturating_duration_since(flight.started).as_secs_f64() / FLIGHT.as_secs_f64();
        if travelled >= 1.0 {
            // The tail: arrived, still drawn in front. Identical to the
            // ordinary drawing of the window except for what it is drawn over.
            return Some((flight.from, 0.0));
        }
        let eased = travelled * travelled * (3.0 - 2.0 * travelled);
        Some((flight.from, 1.0 - eased))
    }

    /// Whether anything is still flying on `output`, which is what tells the
    /// render pass to keep asking for frames.
    pub fn flying(&self, output: &Output, now: Instant) -> bool {
        self.flights
            .iter()
            .any(|flight| flight.output == output.name() && !finished(flight, now))
    }

    /// Drop flights that have landed. Called from the render pass, which is
    /// the only thing that samples them.
    pub fn prune(&mut self, now: Instant) {
        self.flights.retain(|flight| !finished(flight, now));
    }

    /// Forget a window's flight outright — it closed, or its display went
    /// away, and a landed flight must not be resumed against a recycled id.
    pub fn forget(&mut self, window: u32) {
        self.flights.retain(|flight| flight.window != window);
    }
}

fn finished(flight: &Flight, now: Instant) -> bool {
    now.saturating_duration_since(flight.started) >= FLIGHT + TAIL
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

    fn tile() -> Rect {
        Rect {
            x: 100.0,
            y: 200.0,
            w: 80.0,
            h: 80.0,
        }
    }

    /// The flight starts at the tile and ends at the window's own geometry,
    /// and it is over when it is over.
    #[test]
    fn a_window_flies_from_its_tile_and_lands() {
        let out = output("A");
        let mut restores = Restores::default();
        let t0 = Instant::now();
        restores.begin(1, &out, tile(), t0);

        let (from, left) = restores.flight(&out, 1, t0).unwrap();
        assert_eq!(from, tile());
        assert_eq!(left, 1.0, "starts at the tile");

        let (_, mid) = restores.flight(&out, 1, t0 + FLIGHT / 2).unwrap();
        assert!(mid > 0.1 && mid < 0.9, "somewhere in between: {mid}");

        assert_eq!(
            restores.flight(&out, 1, t0 + FLIGHT).unwrap().1,
            0.0,
            "arrived, and still drawn in front for the tail"
        );
        assert!(
            restores.flight(&out, 1, t0 + FLIGHT + TAIL).is_none(),
            "after the tail it is an ordinary window again"
        );
        assert!(!restores.flying(&out, t0 + FLIGHT + TAIL));
    }

    /// The tail is what stops the display showing one frame of bare start
    /// screen: the window is still in front for the whole of the gap between
    /// the compositor finishing the motion and the shell stepping down.
    #[test]
    fn the_window_stays_in_front_after_it_has_stopped_moving() {
        let out = output("A");
        let mut restores = Restores::default();
        let t0 = Instant::now();
        restores.begin(1, &out, tile(), t0);

        for after in [Duration::ZERO, TAIL / 2, TAIL - Duration::from_millis(1)] {
            let (_, left) = restores
                .flight(&out, 1, t0 + FLIGHT + after)
                .expect("dropped while the shell was still on top");
            assert_eq!(left, 0.0, "the tail does not move the window");
        }
        assert!(restores.flying(&out, t0 + FLIGHT + TAIL / 2));
    }

    /// It eases. A linear flight is the one thing the motion here may not be.
    #[test]
    fn the_flight_is_eased_at_both_ends() {
        let out = output("A");
        let mut restores = Restores::default();
        let t0 = Instant::now();
        restores.begin(1, &out, tile(), t0);

        let quarter = restores.flight(&out, 1, t0 + FLIGHT / 4).unwrap().1;
        let half = restores.flight(&out, 1, t0 + FLIGHT / 2).unwrap().1;
        let three = restores.flight(&out, 1, t0 + FLIGHT * 3 / 4).unwrap().1;

        assert_eq!(half, 0.5, "symmetric about the middle");
        // Slow near the tile and slow near the display: the quarter has
        // covered less than a linear quarter would, and by symmetry so has the
        // last one.
        assert!(1.0 - quarter < 0.25, "left the tile too fast: {quarter}");
        assert!(three < 0.25, "arrived too fast: {three}");
    }

    /// Pressing the same tile again is one arrival, not two.
    #[test]
    fn flying_a_window_again_restarts_it() {
        let out = output("A");
        let mut restores = Restores::default();
        let t0 = Instant::now();
        restores.begin(1, &out, tile(), t0);
        restores.begin(1, &out, tile(), t0 + FLIGHT / 2);
        assert_eq!(restores.flights.len(), 1);
        // The clock restarted with it, so it is at the tile again.
        assert_eq!(restores.flight(&out, 1, t0 + FLIGHT / 2).unwrap().1, 1.0);
    }

    /// A flight belongs to the display it was asked for on. The same window
    /// cannot be mid-flight on somebody else's screen.
    #[test]
    fn a_flight_belongs_to_one_display() {
        let (a, b) = (output("A"), output("B"));
        let mut restores = Restores::default();
        let t0 = Instant::now();
        restores.begin(1, &a, tile(), t0);
        assert!(restores.flight(&a, 1, t0).is_some());
        assert!(restores.flight(&b, 1, t0).is_none());
        assert!(!restores.flying(&b, t0));
    }

    /// Nothing to grow out of is not a flight — the window is simply there.
    #[test]
    fn an_empty_rectangle_is_no_flight() {
        let out = output("A");
        let mut restores = Restores::default();
        let t0 = Instant::now();
        restores.begin(
            1,
            &out,
            Rect {
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 0.0,
            },
            t0,
        );
        assert!(restores.flight(&out, 1, t0).is_none());
    }

    /// Landed flights are dropped, and a window that closes takes its flight
    /// with it — an id can be reused, and a stale flight would fly the
    /// stranger that inherited it.
    #[test]
    fn flights_do_not_outlive_what_they_were_for() {
        let out = output("A");
        let mut restores = Restores::default();
        let t0 = Instant::now();
        restores.begin(1, &out, tile(), t0);
        restores.begin(2, &out, tile(), t0);

        restores.prune(t0 + FLIGHT / 2);
        assert_eq!(restores.flights.len(), 2, "still flying");
        restores.forget(2);
        assert_eq!(restores.flights.len(), 1);
        restores.prune(t0 + FLIGHT);
        assert_eq!(restores.flights.len(), 1, "still in front for the tail");
        restores.prune(t0 + FLIGHT + TAIL);
        assert!(restores.flights.is_empty());
    }
}
