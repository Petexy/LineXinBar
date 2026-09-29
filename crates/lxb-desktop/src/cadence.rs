//! How often a display is drawn while something on it moves, in low-end
//! hardware mode.
//!
//! At the display's own refresh wherever the device keeps up with it, and at
//! every other refresh where it does not. A device that draws most frames in
//! time and misses the rest is drawing at a rate that changes from one frame to
//! the next, which reads as a stutter; half the refresh, every frame on time,
//! is a slower glide and an even one. Only a device that has shown it cannot
//! keep the full rate is given the half, and it is given the full rate back
//! once it has shown for long enough that it can.
//!
//! The measure is the display's own answer. A frame drawn the moment the last
//! one was shown is answered one refresh later if it was finished in time and
//! two if it was not, so how long the answer took says whether that frame kept
//! up — the compositor's work included, which on a device drawing on its
//! processor is half of the cost. The same answers are the beat the halved
//! frames keep: a frame drawn on a beat is shown on one, and a frame drawn at
//! "about two refreshes later" by the clock would drift across them.
//!
//! What a display is drawn at while nothing moves is not decided here: nothing
//! on it is changing, so there is no rate to be even.

use std::time::{Duration, Instant};

/// The refresh assumed of a display that has not said its own.
const SIXTY_HERTZ: Duration = Duration::from_nanos(16_666_667);

/// How many of the latest frames drawn in motion are looked at, and how many
/// of them may miss their refresh before the device is said not to keep up.
///
/// A fifth. One frame in a while that misses is a new column being laid out
/// or a picture arriving, and the full rate with that in it is still the
/// smoother of the two; a device missing a fifth of its frames is drawing at
/// something under fifty a second on a sixty-hertz display, unevenly, which is
/// worse than thirty evenly.
const LOOKED_AT: u32 = 30;
const MISSES_ALLOWED: u32 = 5;

/// How many frames in a row a device at half the refresh has to finish within
/// one before the full rate is tried again — two seconds of moving at thirty —
/// and the most that ever grows to. It doubles each time the full rate has had
/// to be given up again, so a device on the edge settles on the half rather
/// than trading between the two.
const TRUST: u32 = 60;
const TRUST_AT_MOST: u32 = 960;

/// How long the next frame waits for the display to say it showed the last
/// one. An answer this late is one that is not coming.
const ANSWER_AT_MOST: Duration = Duration::from_millis(100);

/// When the next frame may be drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    /// Now.
    Now,
    /// Not before this moment, which is a refresh of the display.
    At(Instant),
    /// Once the display has said it showed the last frame.
    Answer,
}

/// One display's pace while something on it moves.
#[derive(Debug, Clone)]
pub struct Cadence {
    period: Duration,
    halved: bool,
    /// When the last frame was drawn, and whether something was moving then.
    drawn: Option<Instant>,
    moved: bool,
    /// Whether the last frame was drawn on a beat, in the middle of a
    /// movement, so how long its answer takes says something.
    timed: bool,
    /// When the display last said it showed a frame, and whether it has said
    /// so since the last one was drawn.
    answered: Option<Instant>,
    shown: bool,
    /// The latest [`LOOKED_AT`] frames at the full rate, one bit each, set
    /// where a frame missed its refresh.
    misses: u64,
    /// Frames in a row that were finished within a refresh, at half of it.
    kept: u32,
    trust: u32,
}

impl Default for Cadence {
    fn default() -> Self {
        Self {
            period: SIXTY_HERTZ,
            halved: false,
            drawn: None,
            moved: false,
            timed: false,
            answered: None,
            shown: false,
            misses: 0,
            kept: 0,
            trust: TRUST,
        }
    }
}

impl Cadence {
    /// The display's refresh, in millihertz as displays report it. Nothing is
    /// changed by a display that does not know its own.
    pub fn set_refresh(&mut self, millihertz: u32) {
        if millihertz >= 1000 {
            self.period = Duration::from_nanos(1_000_000_000_000 / u64::from(millihertz));
        }
    }

    /// Whether frames go out at every other refresh.
    pub fn halved(&self) -> bool {
        self.halved
    }

    /// Whether the last frame was drawn while something moved. The frame
    /// after it — the one that shows the thing stopped — is paced as a moving
    /// one too, or every movement would end on a late frame.
    pub fn moved(&self) -> bool {
        self.moved
    }

    /// When the next frame may be drawn, while something moves.
    pub fn next(&self, now: Instant) -> Next {
        let Some(drawn) = self.drawn.filter(|_| self.moved) else {
            return Next::Now;
        };
        let Some(answered) = self.answered.filter(|_| self.shown) else {
            return if now.saturating_duration_since(drawn) >= ANSWER_AT_MOST {
                Next::Now
            } else {
                Next::Answer
            };
        };
        if !self.halved {
            return Next::Now;
        }
        // The first refresh that is at least a refresh and a half after the
        // last frame: the one after the refresh that showed it, where it was
        // finished in time, and the one that showed it where it was not.
        let wanted = drawn + self.period * 3 / 2;
        let beat = if answered >= wanted {
            answered
        } else {
            let beats = (wanted - answered)
                .as_nanos()
                .div_ceil(self.period.as_nanos().max(1));
            answered + self.period * u32::try_from(beats).unwrap_or(u32::MAX)
        };
        if now >= beat {
            Next::Now
        } else {
            Next::At(beat)
        }
    }

    /// A frame was drawn at `now`, while something was or was not moving.
    pub fn drew(&mut self, now: Instant, moving: bool) {
        self.timed = moving && self.moved && self.shown;
        self.drawn = Some(now);
        self.moved = moving;
        self.shown = false;
    }

    /// The display said at `now` that it showed the last frame.
    pub fn answered(&mut self, now: Instant) {
        let first = self.drawn.is_some() && !self.shown;
        self.answered = Some(now);
        self.shown = true;
        if !(first && self.timed) {
            return;
        }
        self.timed = false;
        let took = self
            .drawn
            .map_or(Duration::ZERO, |drawn| now.saturating_duration_since(drawn));
        self.judge(took > self.period * 3 / 2);
    }

    fn judge(&mut self, missed: bool) {
        if self.halved {
            self.kept = if missed { 0 } else { self.kept + 1 };
            if self.kept >= self.trust {
                self.halved = false;
                self.misses = 0;
                self.trust = (self.trust * 2).min(TRUST_AT_MOST);
                tracing::info!(
                    after = self.kept,
                    "low-end drawing: trying the display's full refresh again"
                );
            }
            return;
        }
        self.misses = (self.misses << 1 | u64::from(missed)) & ((1 << LOOKED_AT) - 1);
        if self.misses.count_ones() > MISSES_ALLOWED {
            self.halved = true;
            self.kept = 0;
            tracing::info!(
                refresh_ms = self.period.as_secs_f32() * 1000.0,
                "low-end drawing: this device misses the display's refresh, so frames go out \
                 on every other one"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REFRESH: Duration = SIXTY_HERTZ;

    /// A movement of `frames` frames, each drawn as soon as it is due and
    /// answered `took` after it was drawn.
    fn moving(cadence: &mut Cadence, from: Instant, frames: u32, took: Duration) -> Instant {
        let mut now = from;
        for _ in 0..frames {
            if let Next::At(beat) = cadence.next(now) {
                now = beat;
            }
            assert_eq!(cadence.next(now), Next::Now, "a frame is due at its answer");
            cadence.drew(now, true);
            now += took;
            cadence.answered(now);
        }
        now
    }

    #[test]
    fn a_device_that_keeps_up_draws_at_every_refresh() {
        let mut cadence = Cadence::default();
        let start = Instant::now();
        let now = moving(&mut cadence, start, 120, REFRESH);
        assert!(!cadence.halved());
        assert_eq!(cadence.next(now), Next::Now);
    }

    #[test]
    fn a_frame_waits_for_the_last_one_to_be_shown() {
        let mut cadence = Cadence::default();
        let start = Instant::now();
        cadence.drew(start, true);
        assert_eq!(cadence.next(start + REFRESH / 2), Next::Answer);
        assert_eq!(
            cadence.next(start + ANSWER_AT_MOST),
            Next::Now,
            "but not for an answer that is not coming"
        );
        cadence.answered(start + REFRESH);
        assert_eq!(cadence.next(start + REFRESH), Next::Now);
    }

    #[test]
    fn nothing_waits_on_a_still_frame() {
        let mut cadence = Cadence::default();
        let start = Instant::now();
        cadence.drew(start, false);
        assert!(!cadence.moved());
        assert_eq!(cadence.next(start + Duration::from_millis(1)), Next::Now);
    }

    #[test]
    fn a_few_late_frames_do_not_halve_it() {
        let mut cadence = Cadence::default();
        let start = Instant::now();
        let mut now = moving(&mut cadence, start, 10, REFRESH);
        now = moving(&mut cadence, now, MISSES_ALLOWED, REFRESH * 2);
        moving(&mut cadence, now, 40, REFRESH);
        assert!(!cadence.halved());
    }

    #[test]
    fn a_device_that_misses_goes_to_every_other_refresh_on_the_beat() {
        let mut cadence = Cadence::default();
        let start = Instant::now();
        // Every other frame missed: thirty-odd a second, unevenly.
        let mut now = start;
        for frame in 0..LOOKED_AT {
            now = moving(
                &mut cadence,
                now,
                1,
                if frame % 2 == 0 { REFRESH } else { REFRESH * 2 },
            );
        }
        assert!(cadence.halved());

        // A frame drawn on the beat, and answered a refresh later: the next
        // is due on the refresh after that, counted from the answer.
        let drawn = match cadence.next(now) {
            Next::At(beat) => beat,
            _ => now,
        };
        cadence.drew(drawn, true);
        let shown = drawn + REFRESH + Duration::from_micros(300);
        cadence.answered(shown);
        assert_eq!(cadence.next(shown), Next::At(shown + REFRESH));
        assert_eq!(cadence.next(shown + REFRESH), Next::Now);

        // One that took longer than a refresh is followed the moment it is
        // shown, which is already every other refresh.
        let drawn = shown + REFRESH;
        cadence.drew(drawn, true);
        let shown = drawn + REFRESH * 2;
        cadence.answered(shown);
        assert_eq!(cadence.next(shown), Next::Now);
    }

    #[test]
    fn a_halved_device_is_trusted_again_slower_each_time() {
        let mut cadence = Cadence::default();
        let start = Instant::now();
        // The first frame of a movement is not judged, so one more.
        let now = moving(&mut cadence, start, MISSES_ALLOWED + 2, REFRESH * 2);
        assert!(cadence.halved());
        let now = moving(&mut cadence, now, TRUST - 1, REFRESH);
        assert!(
            cadence.halved(),
            "not before it has shown it for long enough"
        );
        let now = moving(&mut cadence, now, 1, REFRESH);
        assert!(!cadence.halved(), "the full rate is tried again");

        let now = moving(&mut cadence, now, MISSES_ALLOWED + 1, REFRESH * 2);
        assert!(cadence.halved());
        let now = moving(&mut cadence, now, TRUST, REFRESH);
        assert!(cadence.halved(), "the second time it takes twice as long");
        moving(&mut cadence, now, TRUST, REFRESH);
        assert!(!cadence.halved());
    }

    #[test]
    fn only_frames_drawn_in_the_middle_of_a_movement_are_judged() {
        let mut cadence = Cadence::default();
        let start = Instant::now();
        let mut now = start;
        // The first frame of each movement is drawn whenever the press came,
        // not on a beat, and may take anything up to two refreshes to show.
        for _ in 0..20 {
            cadence.drew(now, false);
            now += REFRESH * 2;
            cadence.answered(now);
            cadence.drew(now, true);
            now += REFRESH * 2;
            cadence.answered(now);
            now += Duration::from_millis(500);
        }
        assert!(!cadence.halved());
    }

    #[test]
    fn a_display_says_its_own_refresh() {
        let mut cadence = Cadence::default();
        cadence.set_refresh(90_000);
        assert_eq!(cadence.period, Duration::from_nanos(11_111_111));
        cadence.set_refresh(0);
        assert_eq!(cadence.period, Duration::from_nanos(11_111_111));
    }
}
