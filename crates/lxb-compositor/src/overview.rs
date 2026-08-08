//! The window overview: every window on a display, animated into cards.
//!
//! The shell asks for it through `lxb_shell_v1.set_output_overview` when
//! its guide menu opens. While it is on, [`crate::render::output_elements`]
//! draws each window scaled into a card slot from
//! [`lxb_protocol::overview`] instead of at its real geometry; this
//! module only owns *when* — which display is in the overview and how far
//! along the enter/leave animation is.
//!
//! The animation needs no ticking. Progress is a pure function of the clock,
//! sampled once per rendered frame; the shell's own overlay animates every
//! frame while the overview is up, so the compositor keeps rendering and the
//! samples keep coming.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use smithay::desktop::Window;
use smithay::output::Output;

use lxb_protocol::overview::{spring, Rect, CARD_SPRING};

/// A card's eased rectangle and the speed each of its edges is travelling at.
///
/// The velocity is the whole reason a scroll accelerates away rather than
/// leaving at full pelt: it is what a spring carries between frames, and what
/// lets a target that moves again mid-glide be picked up rather than restarted.
#[derive(Debug, Clone, Copy)]
struct Glide {
    at: Rect,
    velocity: Rect,
}

/// How long a window takes to fly between its real place and its card. The
/// shell times the start screen and the cards' decoration on the same value.
const FLIGHT: Duration = lxb_protocol::overview::FLIGHT;

#[derive(Debug, Clone)]
struct OverviewAnim {
    output: Output,
    open: bool,
    /// Selected card, as the shell last reported it. Drives the row's
    /// scroll position through the shared layout.
    selected: usize,
    /// When `open` last flipped, and the linear progress at that moment —
    /// so reversing mid-flight continues from where the windows are, rather
    /// than teleporting them to an endpoint and animating from there.
    changed_at: Instant,
    progress_at_change: f64,
}

impl OverviewAnim {
    /// Linear progress towards open (1.0) or closed (0.0).
    fn linear(&self, now: Instant) -> f64 {
        let travelled =
            now.saturating_duration_since(self.changed_at).as_secs_f64() / FLIGHT.as_secs_f64();
        if self.open {
            (self.progress_at_change + travelled).min(1.0)
        } else {
            (self.progress_at_change - travelled).max(0.0)
        }
    }
}

/// Every display's overview state. At most one is open — the overview belongs
/// to the display the user is driving — but several can be mid-flight when
/// control moves quickly, so each animates independently.
#[derive(Debug, Default)]
pub struct Overviews {
    anims: Vec<OverviewAnim>,
    /// Each card's eased on-screen rectangle, keyed by output and window.
    /// This is what makes the row *glide* when scrolling moves the slots.
    /// Interior mutability because it is advanced from the render path,
    /// which sees the state immutably; the compositor is single-threaded.
    eased: RefCell<HashMap<(String, u32), Glide>>,
    /// When each output's cards last advanced, for the glide's time step.
    ticked: RefCell<HashMap<String, Instant>>,
}

impl Overviews {
    /// Enter or leave the overview on `output`. Entering it on one display
    /// leaves it on every other.
    pub fn set(&mut self, output: &Output, enabled: bool, now: Instant) {
        if enabled {
            for anim in &mut self.anims {
                if &anim.output != output && anim.open {
                    anim.progress_at_change = anim.linear(now);
                    anim.changed_at = now;
                    anim.open = false;
                }
            }
        }

        match self.anims.iter_mut().find(|anim| &anim.output == output) {
            Some(anim) => {
                if anim.open != enabled {
                    anim.progress_at_change = anim.linear(now);
                    anim.changed_at = now;
                    anim.open = enabled;
                }
            }
            None if enabled => self.anims.push(OverviewAnim {
                output: output.clone(),
                open: true,
                selected: 0,
                changed_at: now,
                progress_at_change: 0.0,
            }),
            None => {}
        }

        // Fully closed animations are finished business; dropping them also
        // releases outputs that have been unplugged.
        self.anims
            .retain(|anim| anim.open || anim.linear(now) > 0.0);
        let live: Vec<String> = self.anims.iter().map(|a| a.output.name()).collect();
        self.eased
            .borrow_mut()
            .retain(|(output, _), _| live.iter().any(|name| name == output));
    }

    /// Close every overview at once, flying the windows back.
    ///
    /// For losing the shell: the overview is a state the shell asked for and
    /// only the shell ever cancels, so a shell that exits while it is on
    /// would otherwise leave every window shrunk into a card with nothing
    /// left to press.
    pub fn close_all(&mut self, now: Instant) {
        let outputs: Vec<Output> = self
            .anims
            .iter()
            .filter(|anim| anim.open)
            .map(|anim| anim.output.clone())
            .collect();
        for output in outputs {
            self.set(&output, false, now);
        }
    }

    /// Record which card the shell says is selected on `output`.
    pub fn set_selection(&mut self, output: &Output, index: usize) {
        if let Some(anim) = self.anims.iter_mut().find(|anim| &anim.output == output) {
            anim.selected = index;
        }
    }

    /// The selection last reported for `output`.
    pub fn selection(&self, output: &Output) -> usize {
        self.anims
            .iter()
            .find(|anim| &anim.output == output)
            .map(|anim| anim.selected)
            .unwrap_or(0)
    }

    /// Seconds since this output's cards last advanced. Called once per
    /// rendered frame, before gliding that frame's cards.
    pub fn tick(&self, output: &Output, now: Instant) -> f64 {
        let mut ticked = self.ticked.borrow_mut();
        let last = ticked.insert(output.name(), now);
        last.map(|last| now.saturating_duration_since(last).as_secs_f64())
            .unwrap_or(0.0)
    }

    /// Advance `window`'s card towards `target` by `dt` seconds and return
    /// where it is now. A card seen for the first time starts at its target,
    /// so the open/close flight stays purely progress-driven; afterwards the
    /// eased position is what makes a scrolling row glide.
    pub fn glide(&self, output: &Output, window: u32, target: Rect, dt: f64) -> Rect {
        let key = (output.name(), window);
        let mut eased = self.eased.borrow_mut();
        let glide = eased.entry(key).or_insert(Glide {
            at: target,
            velocity: Rect {
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 0.0,
            },
        });

        // Each edge on its own spring, all four at the same stiffness, so a
        // card keeps its shape on the way.
        let at = &mut glide.at;
        let speed = &mut glide.velocity;
        (at.x, speed.x) = spring(at.x, speed.x, target.x, CARD_SPRING, dt);
        (at.y, speed.y) = spring(at.y, speed.y, target.y, CARD_SPRING, dt);
        (at.w, speed.w) = spring(at.w, speed.w, target.w, CARD_SPRING, dt);
        (at.h, speed.h) = spring(at.h, speed.h, target.h, CARD_SPRING, dt);
        glide.at
    }

    /// Eased progress for `output`: 0.0 is normal rendering, 1.0 is every
    /// window seated in its card. Smoothsteps the flight so windows leave and
    /// arrive gently in both directions.
    pub fn progress(&self, output: &Output, now: Instant) -> f64 {
        self.anims
            .iter()
            .find(|anim| &anim.output == output)
            .map(|anim| {
                let p = anim.linear(now);
                p * p * (3.0 - 2.0 * p)
            })
            .unwrap_or(0.0)
    }
}

/// A window's stable overview handle, minted on first sight and carried in
/// its user data so nothing needs cleaning up when it closes. This is the id
/// `output_window` events announce and `activate_window` names.
struct OverviewId(u32);

pub fn window_id(window: &Window) -> u32 {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
    window
        .user_data()
        .insert_if_missing(|| OverviewId(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    window.user_data().get::<OverviewId>().unwrap().0
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

    #[test]
    fn progress_rises_to_one_and_falls_back() {
        let out = output("A");
        let mut overviews = Overviews::default();
        let t0 = Instant::now();

        overviews.set(&out, true, t0);
        assert_eq!(overviews.progress(&out, t0), 0.0);
        let mid = overviews.progress(&out, t0 + FLIGHT / 2);
        assert!(mid > 0.1 && mid < 0.9);
        assert_eq!(overviews.progress(&out, t0 + FLIGHT), 1.0);

        overviews.set(&out, false, t0 + FLIGHT * 2);
        assert_eq!(overviews.progress(&out, t0 + FLIGHT * 3), 0.0);
    }

    #[test]
    fn reversing_mid_flight_continues_from_where_the_windows_are() {
        let out = output("A");
        let mut overviews = Overviews::default();
        let t0 = Instant::now();

        overviews.set(&out, true, t0);
        let before = overviews.progress(&out, t0 + FLIGHT / 2);
        overviews.set(&out, false, t0 + FLIGHT / 2);
        let after = overviews.progress(&out, t0 + FLIGHT / 2);
        assert!(
            (before - after).abs() < 1e-9,
            "reversal must not jump: {before} vs {after}"
        );
        // And it closes from there rather than replaying a full flight.
        assert_eq!(overviews.progress(&out, t0 + FLIGHT * 2), 0.0);
    }

    #[test]
    fn opening_on_a_second_display_closes_the_first() {
        let (a, b) = (output("A"), output("B"));
        let mut overviews = Overviews::default();
        let t0 = Instant::now();

        overviews.set(&a, true, t0);
        overviews.set(&b, true, t0 + FLIGHT * 2);

        assert!(overviews.progress(&a, t0 + FLIGHT * 4) == 0.0);
        assert!(overviews.progress(&b, t0 + FLIGHT * 4) == 1.0);
    }
}
