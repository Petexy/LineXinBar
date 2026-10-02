//! When what the compositor draws for the shell's glass will be on screen.
//!
//! The shell cannot read another client's pixels, so a pane of glass over an
//! application is handed a small picture of the windows behind it: the shell
//! asks, [`crate::shell_control`] draws it there and then, and the shell puts it
//! in the frame it draws next. That frame is not on screen yet. It is committed
//! when the display next answers the shell, and composited by the repaint after
//! that — two refreshes on from the ask, and more when the shell draws less
//! often. Over a window that is flying (the Home menu's overview moves every
//! window for [`lxb_protocol::overview::FLIGHT`]) a picture of the moment it was
//! asked for is a picture of where the window *was*: a ghost trailing it by
//! tens of pixels at sixty hertz and twice that at thirty, and over a white
//! window on a dark wallpaper it is the brightest thing in the pane.
//!
//! So the picture is drawn for the moment it will be seen, not the moment it was
//! asked for — see [`Repaints::shown_at`], and [`predict`] for the arithmetic.
//! Nothing here changes what is drawn; the animations a picture contains are
//! functions of the clock, or are solved forward from it without being moved —
//! see [`crate::render::Clock`] — and this only says which instant to read them
//! at.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use smithay::output::Output;

use crate::capture::Side;

/// The refresh assumed of a display that has not said its own, and that no
/// repaint has yet measured.
const SIXTY_HERTZ: Duration = Duration::from_nanos(16_666_667);

/// The shortest and longest a refresh is taken to be, whatever was measured: a
/// compositor that stalled for a second has not become a one hertz display, and
/// a burst of back-to-back repaints is not a kilohertz one.
const SHORTEST: Duration = Duration::from_millis(4);
const LONGEST: Duration = Duration::from_millis(70);

/// The most repaints the shell's frames are taken to be spaced by.
///
/// A shell draws every refresh while something moves, and at every other one
/// when low-end hardware mode has found the device cannot keep that up (see the
/// shell's own `cadence`). Anything slower is the shell drawing nothing that
/// moves, where the horizon does not matter — and carrying a long gap over into
/// the first frame of a movement would show the window further along than it is.
const MOST_SPACED: u32 = 2;

/// How much of a new measurement of the refresh is believed. A repaint that
/// came late is one sample among many, not the new rate.
const BELIEF: f64 = 0.25;

/// How near the display's own refresh a gap between two repaints has to be to be
/// one repaint wide: within a quarter under and two fifths over. A refresh that
/// was dropped is two repaints wide, and the compositor idling at a tenth of its
/// rate when nothing changes is ten — neither is what the display refreshes at,
/// and a learned rate that let them in would drift towards them.
const NEAR: std::ops::RangeInclusive<f64> = 0.75..=1.4;

/// What is known of one display's repaints.
#[derive(Debug, Clone, Copy, Default)]
struct Beat {
    /// The latest repaint's sample instant: the moment the display's own frame
    /// read the clock, which is the moment every window in it was placed for.
    sampled: Option<Instant>,
    /// How far apart those are, measured.
    interval: Option<Duration>,
    /// When each side's picture was last asked for, and the time before that.
    /// The gap between two asks is the gap between two of the shell's frames,
    /// because it asks once per frame it draws.
    asked: [[Option<Instant>; 2]; 2],
}

/// Every display's repaint clock.
///
/// Interior mutability for the reason [`crate::overview::Overviews`] has it:
/// [`crate::render::post_repaint`], which is where a repaint is recorded, sees
/// the compositor state immutably, and the compositor is single-threaded.
#[derive(Debug, Default)]
pub struct Repaints {
    beats: RefCell<HashMap<String, Beat>>,
}

impl Repaints {
    /// Say that `output` has just been repainted, from a sample of the clock
    /// taken at `sampled` — the instant its frame's windows were placed for.
    ///
    /// Not [`crate::overview::Overviews::tick`], which screenshots and the
    /// screen recorder move too: those are not repaints, and a clock that
    /// counted them would hear a display refreshing twice as fast.
    pub fn repainted(&self, output: &Output, sampled: Instant) {
        let mut beats = self.beats.borrow_mut();
        let beat = beats.entry(output.name()).or_default();
        // What the display itself says it refreshes at, which is what every
        // gap is read against — and not what was learned, which is only ever a
        // refinement of it. A display switched to another mode refreshes at
        // another rate, and a rate learned for the old one is dropped.
        let mode = refresh_of(output);
        if beat.interval.is_some_and(|learned| !near(learned, mode)) {
            beat.interval = None;
        }
        if let Some(previous) = beat.sampled {
            let gap = sampled.saturating_duration_since(previous);
            if near(gap, mode) {
                let held = beat.interval.unwrap_or(mode).as_secs_f64();
                let learned = held + BELIEF * (gap.as_secs_f64() - held);
                beat.interval = Some(Duration::from_secs_f64(learned));
            }
        }
        beat.sampled = Some(sampled);
    }

    /// The instant a picture of `side` of `output`, asked for at `asked`, will
    /// be on screen: the moment to place its windows for.
    ///
    /// Also what remembers the ask, so the next one can tell how far apart the
    /// shell's frames are.
    pub fn shown_at(&self, output: &Output, side: Side, asked: Instant) -> Instant {
        let mut beats = self.beats.borrow_mut();
        let beat = beats.entry(output.name()).or_default();
        let slot = match side {
            Side::Below => 0,
            Side::Above => 1,
        };
        let mode = refresh_of(output);
        let period = beat
            .interval
            .filter(|learned| near(*learned, mode))
            .unwrap_or(mode);
        let at = predict(asked, beat.sampled, period, beat.asked[slot]);
        beat.asked[slot] = [Some(asked), beat.asked[slot][0]];
        at
    }

    /// Forget a display that has gone, so one that comes back by the same name
    /// starts clean.
    pub fn forget(&self, output: &Output) {
        self.beats.borrow_mut().remove(&output.name());
    }
}

/// Whether `gap` is one refresh of a display whose mode says it is `mode`.
fn near(gap: Duration, mode: Duration) -> bool {
    NEAR.contains(&(gap.as_secs_f64() / mode.as_secs_f64().max(f64::EPSILON)))
}

/// The time between two refreshes of `output`, as its current mode says.
fn refresh_of(output: &Output) -> Duration {
    output
        .current_mode()
        .filter(|mode| mode.refresh > 0)
        .map(|mode| Duration::from_secs_f64(1000.0 / mode.refresh as f64))
        .unwrap_or(SIXTY_HERTZ)
}

/// The moment a picture asked for at `asked` will be on screen.
///
/// The shell asks right after it commits a frame, which it drew in answer to
/// the callbacks of the repaint that sampled at `sampled`. That commit is
/// composited by the next repaint, `sampled + period` — or by `asked` itself
/// if the compositor has been idle and is about to draw. The picture is handed
/// back at once and goes into the frame the shell draws after that one: drawn
/// at the callbacks of this next repaint, committed at once and composited by
/// the one after, a `period` later. So, for a shell that draws every refresh,
/// the picture is on screen `period` after the first repaint that could show the
/// commit it followed — `sampled + 2 × period` in steady state.
///
/// A shell that draws every `spacing` refreshes has its frame drawn `spacing`
/// repaints on instead, and on screen for `spacing` of them. One instant cannot
/// be right for all of those, and the middle of them is the one that is least
/// wrong: `(3 × spacing − 1) / 2` periods after the first repaint, which for a
/// spacing of one is the single instant above. `spacing` is read off the gaps
/// between the last asks, in repaints, because a shell asks once per frame it
/// draws: the *shorter* of the last two, so that one frame that came late is
/// not taken for a shell that has slowed down — which would aim the next
/// picture a frame and a half past where its frame is shown. It is one with no
/// previous ask, or when the last gap is so long it is not a cadence but a
/// pause — whatever the asks before it were.
///
/// Never earlier than `asked`, and never more than four periods ahead: a
/// movement the compositor has no way to foresee, such as one that is told to
/// reverse, is worth being wrong about by a frame or two, not by a long way.
pub(crate) fn predict(
    asked: Instant,
    sampled: Option<Instant>,
    period: Duration,
    previous_asks: [Option<Instant>; 2],
) -> Instant {
    let period = period.clamp(SHORTEST, LONGEST);
    // The first repaint that can show what the shell has just committed. After a
    // long quiet there is none in between: the compositor is waking for this
    // commit, and draws it as soon as it has answered this ask.
    let first = match sampled {
        Some(sampled) if asked.saturating_duration_since(sampled) <= period * 5 / 2 => {
            (sampled + period).max(asked)
        }
        _ => asked,
    };
    // The gap since the last ask says whether the shell is drawing at a cadence
    // at all; if it is, the shorter of that and the one before it says which.
    let spacing = match previous_asks[0].map(|latest| asked.saturating_duration_since(latest)) {
        Some(gap) if gap <= period * 5 / 2 => {
            let gap = match previous_asks {
                [Some(latest), Some(before)] => gap.min(latest.saturating_duration_since(before)),
                _ => gap,
            };
            ((gap.as_secs_f64() / period.as_secs_f64()).round() as u32).clamp(1, MOST_SPACED)
        }
        _ => 1,
    };
    let at = first + period.mul_f64((3.0 * spacing as f64 - 1.0) / 2.0);
    at.clamp(asked, asked + period * 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    use smithay::output::{Mode, PhysicalProperties, Subpixel};

    const T: Duration = Duration::from_micros(16_667);

    fn ms(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    /// An instant a minute after now, so that the moments a test counts back
    /// from it are not before the machine started.
    fn present() -> Instant {
        Instant::now() + Duration::from_secs(60)
    }

    fn output_at(name: &str, millihertz: i32) -> Output {
        let output = Output::new(
            name.to_string(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "test".into(),
                model: "test".into(),
            },
        );
        output.change_current_state(
            Some(Mode {
                size: (1920, 1080).into(),
                refresh: millihertz,
            }),
            None,
            None,
            None,
        );
        output
    }

    fn learned(repaints: &Repaints, output: &Output) -> Option<Duration> {
        repaints.beats.borrow().get(&output.name())?.interval
    }

    /// Repaint `output` `count` times, `gap` apart, from `from`.
    fn repaint(
        repaints: &Repaints,
        output: &Output,
        from: Instant,
        gap: Duration,
        count: u32,
    ) -> Instant {
        let mut at = from;
        for _ in 0..count {
            repaints.repainted(output, at);
            at += gap;
        }
        at
    }

    /// The last two asks of a shell that has been asking every `repaints`
    /// refreshes, as an ask at `asked` finds them.
    fn steady(asked: Instant, repaints: u32) -> [Option<Instant>; 2] {
        steady_at(asked, T, repaints)
    }

    fn steady_at(asked: Instant, period: Duration, repaints: u32) -> [Option<Instant>; 2] {
        let gap = period * repaints;
        [Some(asked - gap), Some(asked - gap * 2)]
    }

    /// The ordinary case, a shell drawing every refresh: asked a few
    /// milliseconds after the repaint whose callbacks it answered, the picture
    /// is for the repaint two periods after that one — the one that will show
    /// the frame drawn with it.
    #[test]
    fn a_shell_that_draws_every_refresh_is_two_repaints_ahead() {
        let sampled = present();
        let asked = sampled + ms(4);
        let at = predict(asked, Some(sampled), T, steady(asked, 1));
        assert_eq!(at, sampled + T * 2);
    }

    /// The answer does not depend on how long into the refresh the shell took
    /// to commit: what it is anchored to is the repaint, not the ask.
    #[test]
    fn it_does_not_depend_on_how_late_in_the_refresh_the_ask_is() {
        let sampled = present();
        let early = predict(
            sampled + ms(1),
            Some(sampled),
            T,
            steady(sampled + ms(1), 1),
        );
        let late = predict(
            sampled + ms(12),
            Some(sampled),
            T,
            steady(sampled + ms(12), 1),
        );
        assert_eq!(early, late);
        assert_eq!(early, sampled + T * 2);
    }

    /// A shell that draws every other refresh is a repaint and a half further:
    /// the middle of the two repaints its frame is on screen for.
    #[test]
    fn a_halved_cadence_aims_at_the_middle_of_its_two_repaints() {
        let sampled = present();
        let asked = sampled + ms(4);
        let at = predict(asked, Some(sampled), T, steady(asked, 2));
        assert_eq!(at, sampled + T + T.mul_f64(2.5));
    }

    /// One frame that came late is not a shell that has slowed down: taking it
    /// for one would aim the next picture a frame and a half past where its
    /// frame is shown. Two gaps in a row are a cadence.
    #[test]
    fn one_late_frame_is_not_a_slower_shell() {
        let sampled = present();
        let asked = sampled + ms(4);
        let after_a_late_frame = [Some(asked - T * 2), Some(asked - T * 3)];
        assert_eq!(
            predict(asked, Some(sampled), T, after_a_late_frame),
            sampled + T * 2
        );
        assert_eq!(
            predict(asked, Some(sampled), T, steady(asked, 2)),
            sampled + T + T.mul_f64(2.5)
        );
    }

    /// The first ask, and one after a long quiet, say nothing about the shell's
    /// pace: a shell at its still pace is drawing nothing that moves, and a
    /// movement beginning must not start with a picture of a window further
    /// along than it is.
    #[test]
    fn a_first_ask_or_a_long_pause_is_taken_at_every_refresh() {
        let sampled = present();
        let asked = sampled + ms(4);
        assert_eq!(predict(asked, Some(sampled), T, [None; 2]), sampled + T * 2);
        let after_a_while = predict(asked, Some(sampled), T, [Some(asked - ms(900)), None]);
        assert_eq!(after_a_while, sampled + T * 2);
        let paused = [Some(asked - ms(900)), Some(asked - ms(1_800))];
        assert_eq!(predict(asked, Some(sampled), T, paused), sampled + T * 2);
        // A pause that follows a patch of slow frames is still a pause: the
        // shell starts again from every refresh, not from where it left off.
        let after_slow_ones = [Some(asked - ms(900)), Some(asked - ms(900) - T * 2)];
        assert_eq!(
            predict(asked, Some(sampled), T, after_slow_ones),
            sampled + T * 2
        );
    }

    /// A compositor that has been idle and is about to draw for this very
    /// commit has no repaint in between: the commit is shown by the next one,
    /// which is now, and the picture a period after it.
    #[test]
    fn after_a_quiet_the_ask_comes_before_its_own_repaint() {
        let sampled = present();
        let asked = sampled + ms(400);
        let at = predict(asked, Some(sampled), T, [None; 2]);
        assert_eq!(at, asked + T);
        // And with no repaint recorded at all.
        assert_eq!(predict(asked, None, T, [None; 2]), asked + T);
    }

    /// Never in the past, and never further than the clamp says, whatever the
    /// clock did in between.
    #[test]
    fn the_answer_stays_within_its_bounds() {
        let now = present();
        for sampled in [now - ms(5_000), now - ms(1), now, now + ms(3)] {
            for previous in [
                [None; 2],
                [Some(now - T), None],
                [Some(now - T * 2), Some(now - T * 4)],
                [Some(now - ms(900)), Some(now - T)],
            ] {
                for period in [Duration::ZERO, ms(1), T, ms(33), ms(500)] {
                    let at = predict(now, Some(sampled), period, previous);
                    assert!(at >= now);
                    assert!(at <= now + period.clamp(SHORTEST, LONGEST) * 4);
                }
            }
        }
    }

    /// The slower the display, the further ahead: thirty hertz is twice sixty.
    #[test]
    fn a_slow_display_looks_further_ahead() {
        let sampled = present();
        let asked = sampled + ms(4);
        let slow = Duration::from_micros(33_333);
        let sixty = predict(asked, Some(sampled), T, steady(asked, 1)) - asked;
        let thirty = predict(asked, Some(sampled), slow, steady_at(asked, slow, 1)) - asked;
        assert!(thirty > sixty + ms(14));
    }

    /// The refresh is learned from gaps one repaint wide, and from nothing else:
    /// a refresh that was dropped is two repaints wide and the compositor idling
    /// is ten, and neither is what the display refreshes at.
    #[test]
    fn the_refresh_is_learned_from_gaps_one_repaint_wide_and_nothing_else() {
        let repaints = Repaints::default();
        let output = output_at("A", 60_000);
        let start = present();

        // A nested session draws a little after every sixteenth millisecond.
        let end = repaint(&repaints, &output, start, ms(18), 30);
        let learned_now = learned(&repaints, &output).expect("it has seen thirty repaints");
        assert!(
            learned_now > ms(17) && learned_now <= ms(18),
            "{learned_now:?}"
        );

        // A dropped refresh, and a compositor that went idle and woke.
        repaints.repainted(&output, end + ms(36));
        repaints.repainted(&output, end + ms(36) + ms(100));
        assert_eq!(learned(&repaints, &output), Some(learned_now));
    }

    /// A display switched to another mode has another refresh, and what was
    /// learned for the old one is let go of — not held to, rejecting every gap of
    /// the new one for the rest of the session.
    #[test]
    fn a_display_switched_to_another_mode_learns_its_new_refresh() {
        let repaints = Repaints::default();
        let output = output_at("A", 60_000);
        let end = repaint(&repaints, &output, present(), ms(17), 20);
        assert!(learned(&repaints, &output).is_some_and(|learned| learned > ms(16)));

        for millihertz in [144_000, 30_000, 240_000, 60_000] {
            output.change_current_state(
                Some(Mode {
                    size: (1920, 1080).into(),
                    refresh: millihertz,
                }),
                None,
                None,
                None,
            );
            let period = Duration::from_secs_f64(1000.0 / f64::from(millihertz));
            repaint(&repaints, &output, end + ms(1_000), period, 20);
            let learned = learned(&repaints, &output).expect("it has seen twenty repaints");
            let wrong = learned.as_secs_f64() / period.as_secs_f64();
            assert!(
                (0.95..1.05).contains(&wrong),
                "{millihertz}: {learned:?} for {period:?}"
            );
        }
    }

    /// The two sides of the shell's surfaces are asked for on their own beats: a
    /// picture of one is not a frame of the other.
    #[test]
    fn each_side_keeps_the_gaps_of_its_own_asks() {
        let repaints = Repaints::default();
        let output = output_at("A", 60_000);
        let base = present();
        repaints.repainted(&output, base);
        for step in 0..3 {
            repaints.shown_at(&output, Side::Below, base + T * step);
        }
        repaints.shown_at(&output, Side::Above, base + T * 5);

        let beats = repaints.beats.borrow();
        let beat = &beats[&output.name()];
        assert_eq!(beat.asked[0], [Some(base + T * 2), Some(base + T)]);
        assert_eq!(beat.asked[1], [Some(base + T * 5), None]);
    }

    /// A display that has gone takes its clock with it.
    #[test]
    fn a_display_that_has_gone_is_forgotten() {
        let repaints = Repaints::default();
        let (a, b) = (output_at("A", 60_000), output_at("B", 60_000));
        repaints.repainted(&a, present());
        repaints.repainted(&b, present());
        repaints.forget(&a);
        let beats = repaints.beats.borrow();
        assert!(!beats.contains_key("A") && beats.contains_key("B"));
    }
}
