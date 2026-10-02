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

use lxb_protocol::overview::{spring, Focus, Rect, CARD_SPRING, LONGEST_STEP};

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

/// The furthest [`Overviews::glide_at`] looks ahead of the last frame the display
/// drew, in seconds. A display that has not drawn for longer than this is not
/// one whose next frame will be where a long look ahead says: the spring steps
/// no more than [`LONGEST_STEP`] on the first frame back, and a card that is
/// not being drawn is not one worth being clever about.
const LOOKS_AHEAD_AT_MOST: f64 = 0.25;

/// One step of a card's spring towards `target`, each edge on its own, all four
/// at the same stiffness, so a card keeps its shape on the way.
fn advance(glide: Glide, target: Rect, dt: f64) -> Glide {
    let (mut at, mut speed) = (glide.at, glide.velocity);
    (at.x, speed.x) = spring(at.x, speed.x, target.x, CARD_SPRING, dt);
    (at.y, speed.y) = spring(at.y, speed.y, target.y, CARD_SPRING, dt);
    (at.w, speed.w) = spring(at.w, speed.w, target.w, CARD_SPRING, dt);
    (at.h, speed.h) = spring(at.h, speed.h, target.h, CARD_SPRING, dt);
    Glide {
        at,
        velocity: speed,
    }
}

#[derive(Debug, Clone)]
struct OverviewAnim {
    output: Output,
    open: bool,
    /// Selected card, as the shell last reported it. Drives the row's
    /// scroll position through the shared layout.
    selected: usize,
    /// Which half the user is driving, as the shell last reported it. Drives
    /// the slide across a display too narrow for the menu and the cards side
    /// by side, through the same layout.
    focus: Focus,
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
                focus: Focus::Menu,
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

    /// Record which half of the overview the shell says the user is driving on
    /// `output`.
    ///
    /// Nothing is animated here: the focus moves the cards' slots, and every
    /// window already glides to its slot on the spring the shell's own half of
    /// the slide rides — see [`Overviews::glide`].
    pub fn set_focus(&mut self, output: &Output, focus: Focus) {
        if let Some(anim) = self.anims.iter_mut().find(|anim| &anim.output == output) {
            anim.focus = focus;
        }
    }

    /// The focus last reported for `output`.
    pub fn focus(&self, output: &Output) -> Focus {
        self.anims
            .iter()
            .find(|anim| &anim.output == output)
            .map(|anim| anim.focus)
            .unwrap_or_default()
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
        *glide = advance(*glide, target, dt);
        glide.at
    }

    /// Where `window`'s card will be at `at`, if its target stays where it is:
    /// [`Overviews::glide`] looked ahead rather than taken.
    ///
    /// **Moves nothing.** It reads the eased card as the last frame left it and
    /// solves the spring forward on a copy, so a picture of the overview drawn
    /// for a moment that has not come yet neither ticks the clock the display's
    /// own frames step by nor seats a card the display has not met. The spring
    /// is closed-form, so what this says is what [`Overviews::glide`] would have
    /// said had the display stepped the card in frames, each no longer than a
    /// spring is carried in one go — which is what it does while it is drawing.
    /// A display that has stopped drawing for longer than that steps its card
    /// less far on the first frame back than this looks; there is nothing to
    /// show a picture of then.
    ///
    /// A card the display has not seen yet is at its target, which is where the
    /// display will first put it.
    pub fn glide_at(&self, output: &Output, window: u32, target: Rect, at: Instant) -> Rect {
        let Some(glide) = self.eased.borrow().get(&(output.name(), window)).copied() else {
            return target;
        };
        let Some(since) = self.ticked.borrow().get(&output.name()).copied() else {
            return glide.at;
        };
        // In steps no longer than a spring will take: `spring` refuses to be
        // carried over a stall in one go, and a look ahead that crossed one
        // would stop short of where the display will be.
        let mut ahead = at
            .saturating_duration_since(since)
            .as_secs_f64()
            .min(LOOKS_AHEAD_AT_MOST);
        let mut glide = glide;
        while ahead > 0.0 {
            let step = ahead.min(LONGEST_STEP);
            glide = advance(glide, target, step);
            ahead -= step;
        }
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

    /// An overview starts on the menu, keeps the focus the shell last named
    /// for as long as it is open, and belongs to the one display it was named
    /// for.
    #[test]
    fn the_focus_starts_on_the_menu_and_follows_the_shell() {
        let (a, b) = (output("A"), output("B"));
        let mut overviews = Overviews::default();
        let t0 = Instant::now();

        overviews.set(&a, true, t0);
        assert_eq!(overviews.focus(&a), Focus::Menu);
        overviews.set_focus(&a, Focus::Cards);
        assert_eq!(overviews.focus(&a), Focus::Cards);
        // A display with no overview on it has nothing to be focused.
        overviews.set_focus(&b, Focus::Cards);
        assert_eq!(overviews.focus(&b), Focus::Menu);
    }

    fn card(x: f64, w: f64) -> Rect {
        Rect {
            x,
            y: 100.0,
            w,
            h: w * 0.5625,
        }
    }

    fn same(a: Rect, b: Rect) -> bool {
        [(a.x, b.x), (a.y, b.y), (a.w, b.w), (a.h, b.h)]
            .iter()
            .all(|(a, b)| (a - b).abs() < 1e-9)
    }

    /// A card that is gliding towards a slot the display has been moving it to,
    /// at the moment the display last stepped it.
    fn gliding(out: &Output, t0: Instant) -> Overviews {
        let overviews = Overviews::default();
        // Seated at the first slot, then the slot moves and the card is carried.
        overviews.glide(out, 7, card(300.0, 640.0), 0.0);
        overviews.tick(out, t0 - Duration::from_millis(16));
        overviews.glide(out, 7, card(500.0, 640.0), 0.016);
        overviews.tick(out, t0);
        overviews
    }

    /// A look ahead leaves the display's own stepping exactly as it was: nothing
    /// is ticked, nothing is seated, nothing is carried.
    #[test]
    fn looking_ahead_moves_nothing() {
        let out = output("A");
        let t0 = Instant::now() + Duration::from_secs(60);
        let overviews = gliding(&out, t0);
        let eased = overviews.eased.borrow().clone();
        let ticked = overviews.ticked.borrow().clone();

        let _ = overviews.glide_at(&out, 7, card(500.0, 640.0), t0 + Duration::from_millis(33));
        // A card the display has never met is not seated by being asked about.
        let _ = overviews.glide_at(&out, 8, card(900.0, 640.0), t0 + Duration::from_millis(33));

        assert_eq!(overviews.eased.borrow().len(), eased.len());
        for (key, was) in eased.iter() {
            let now = overviews.eased.borrow()[key];
            assert!(same(now.at, was.at) && same(now.velocity, was.velocity));
        }
        assert_eq!(*overviews.ticked.borrow(), ticked);
    }

    /// And what it says is where the display's own stepping would have put the
    /// card, whether that stepping came in one frame or three: the spring is
    /// closed-form, which is what makes a picture of a moment still to come a
    /// picture of the moment it will be.
    #[test]
    fn looking_ahead_is_what_stepping_would_have_said() {
        let out = output("A");
        let t0 = Instant::now() + Duration::from_secs(60);
        let target = card(500.0, 640.0);
        for ahead in [0.016, 0.033, 0.1, 0.25] {
            let seen =
                gliding(&out, t0).glide_at(&out, 7, target, t0 + Duration::from_secs_f64(ahead));
            // The same card stepped by the display: in one go where one go is
            // all a spring will take, and in the pieces it is cut into past that.
            let stepped = gliding(&out, t0);
            let mut left = ahead;
            let mut at = None;
            while left > 1e-12 {
                let step = left.min(LONGEST_STEP);
                at = Some(stepped.glide(&out, 7, target, step));
                left -= step;
            }
            assert!(same(seen, at.unwrap()), "{ahead}: {seen:?} against {at:?}");
        }
    }

    /// A card the display has not met is at its target, which is where the
    /// display will put it; and with no frame ticked yet there is nothing to
    /// look ahead *from*, so a card is where it is.
    #[test]
    fn looking_ahead_at_what_has_not_been_stepped_says_where_it_stands() {
        let out = output("A");
        let t0 = Instant::now() + Duration::from_secs(60);
        let target = card(500.0, 640.0);

        let fresh = Overviews::default();
        assert!(same(fresh.glide_at(&out, 1, target, t0), target));
        assert!(fresh.eased.borrow().is_empty());

        // Seated, never ticked: where it was seated, whatever the moment.
        fresh.glide(&out, 1, card(300.0, 640.0), 0.0);
        fresh.ticked.borrow_mut().remove(&out.name());
        let said = fresh.glide_at(&out, 1, target, t0 + Duration::from_secs(5));
        assert!(same(said, card(300.0, 640.0)), "{said:?}");
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
