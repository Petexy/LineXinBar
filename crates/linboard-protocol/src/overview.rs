//! The overview's card layout, shared by both sides of the protocol.
//!
//! While the overview is on, the compositor draws each window scaled into a
//! card slot and the shell draws the frame, title and selection around it —
//! two processes painting one composition. They can only line up if they
//! compute the same geometry, so the algorithm lives here, in the crate both
//! already depend on, instead of being specified prose-first in the protocol
//! and implemented twice.
//!
//! Everything is in logical coordinates. The shell's layer surface is sized
//! in logical pixels too, so the same numbers serve both sides unchanged.
//!
//! The shape (after the SteamOS quick-access column and the Xbox app
//! switcher): a menu column on the left, and to its right one vertical
//! column of equally sized cards — every window plus, last, the start
//! screen. The selected card is always vertically centred and Up/Down slide
//! the whole column past it, so the neighbours peek in from the screen's
//! edges: the cut-off card *is* the affordance that there is more to scroll
//! to. That is why the selected index is part of the layout call and travels
//! over the protocol.

/// An axis-aligned rectangle in logical coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn right(&self) -> f64 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }
}

/// Card height as a share of the output's; width follows at 16:9. One size
/// for every card — the current application is told apart by coming first
/// and by the selection frame, not by dwarfing everything else. Big enough
/// that the centred card reads as *the* card, small enough that both
/// neighbours still peek past the screen edges.
const CARD_HEIGHT: f64 = 0.54;

/// Vertical gap between cards, as a share of the output height. A card's
/// title sits in this gap, under the card, so it has to clear a text line
/// with air around it.
const GAP: f64 = 0.09;

/// How stiff the spring is that carries a card to its slot when scrolling
/// moves the column, in radians per second. Both sides ease their halves of a
/// card — the compositor the window, the shell the frame and title — so
/// sharing the rate, and [`spring`] itself, is what keeps them travelling
/// together.
pub const CARD_SPRING: f64 = 19.0;

/// One step of a critically damped spring: `position`, travelling at
/// `velocity`, moves towards `target` over `dt` seconds at stiffness `rate`.
/// Returns where it is and how fast it is going.
///
/// Critically damped, so it accelerates from rest and settles without ever
/// crossing the target. That acceleration is the point: the plain exponential
/// chase this replaces left at full speed the instant a target moved, which
/// no amount of softness in the landing stops reading as mechanical.
///
/// Solved in closed form rather than integrated step by step, for two
/// reasons. A long frame cannot make it overshoot or explode — the worst it
/// can do is arrive. And because the solution is exact, two processes
/// stepping the same spring on their own frame timings reach the same place
/// at the same moment, which is the whole reason this lives here.
pub fn spring(position: f64, velocity: f64, target: f64, rate: f64, dt: f64) -> (f64, f64) {
    // A stall — or a resume from sleep — must not be replayed all at once.
    let dt = dt.clamp(0.0, 0.1);
    let offset = position - target;
    // x(t) = target + (offset + c·t)·e^(-rate·t), the critically damped
    // solution whose velocity at t = 0 is the velocity it is carrying.
    let c = velocity + rate * offset;
    let decay = (-rate * dt).exp();
    (
        target + (offset + c * dt) * decay,
        (velocity - c * rate * dt) * decay,
    )
}

/// How long a card takes to fly between its window's real geometry and its
/// slot.
///
/// Shared for the same reason as [`CARD_SPRING`]: the compositor flies
/// the windows while the shell flies the start screen beside them, and they
/// only arrive together if it is one number. It is also the shell's answer to
/// *have the cards landed yet* — the question that decides when it may start
/// decorating a window it does not draw.
pub const FLIGHT: std::time::Duration = std::time::Duration::from_millis(300);

/// Width of the menu column for an output `width` wide.
///
/// Proportional with a floor and a ceiling: a fraction alone turns illegible
/// on a small nested window and cavernous on an ultrawide.
pub fn sidebar_width(width: f64) -> f64 {
    (width * 0.22).clamp(280.0, 520.0).min(width * 0.5)
}

/// The card slots for `count` cards with `selected` centred, in card order —
/// the current window first, the start screen last.
///
/// Every card gets a slot, including the ones hanging past the screen's top
/// and bottom edges: both renderers clip for free, and a card that scrolls
/// into view eases from where it really was instead of materialising.
///
/// Slots are boxes, not final frames — a window is aspect-fitted into its
/// slot with [`fit`].
pub fn card_slots(width: f64, height: f64, count: usize, selected: usize) -> Vec<Rect> {
    if count == 0 {
        return Vec::new();
    }
    let selected = selected.min(count - 1);

    let margin = height * 0.05;
    let left = sidebar_width(width) + margin;
    let region_w = (width - margin - left).max(0.0);

    let mut card_h = height * CARD_HEIGHT;
    let mut card_w = card_h * 16.0 / 9.0;
    // A narrow region (portrait output, huge sidebar) shrinks the card
    // rather than running it under the menu.
    if card_w > region_w && region_w > 0.0 {
        card_w = region_w;
        card_h = card_w * 9.0 / 16.0;
    }

    let pitch = card_h + height * GAP;
    let x = left + (region_w - card_w) / 2.0;
    // The selected card is centred with equal free space above and below;
    // everything else hangs off it at a fixed pitch.
    let selected_y = (height - card_h) / 2.0;

    (0..count)
        .map(|index| Rect {
            x,
            y: selected_y + (index as f64 - selected as f64) * pitch,
            w: card_w,
            h: card_h,
        })
        .collect()
}

/// Centre a `window_w` × `window_h` window inside `slot` at its own aspect
/// ratio. This is the rectangle the compositor scales the window into, and
/// therefore the one the shell frames.
pub fn fit(slot: &Rect, window_w: f64, window_h: f64) -> Rect {
    if window_w <= 0.0 || window_h <= 0.0 || slot.w <= 0.0 || slot.h <= 0.0 {
        return *slot;
    }
    let scale = (slot.w / window_w).min(slot.h / window_h);
    let w = window_w * scale;
    let h = window_h * scale;
    Rect {
        x: slot.x + (slot.w - w) / 2.0,
        y: slot.y + (slot.h - h) / 2.0,
        w,
        h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the spring is here for: it leaves at rest and arrives at rest,
    /// unlike the exponential chase it replaced, which was at full speed on
    /// its first frame.
    #[test]
    fn a_card_accelerates_away_and_settles_without_overshooting() {
        let frame = 1.0 / 60.0;
        let (mut at, mut speed) = (0.0, 0.0);

        let mut steps = Vec::new();
        for _ in 0..60 {
            (at, speed) = spring(at, speed, 100.0, CARD_SPRING, frame);
            steps.push(at);
        }

        // The first frame is a fraction of the fastest one. An exponential
        // chase's first frame *is* its fastest, which is the difference.
        let fastest = steps
            .windows(2)
            .map(|pair| pair[1] - pair[0])
            .fold(0.0f64, f64::max);
        assert!(
            steps[0] * 2.0 < fastest,
            "it should leave gently: {} against {fastest} at speed",
            steps[0]
        );
        // Critically damped: it never crosses the target on the way.
        assert!(steps.iter().all(|step| *step <= 100.0));
        assert!((at - 100.0).abs() < 0.1, "and arrives: {at}");
        assert!(speed.abs() < 1.0, "at rest: {speed}");
    }

    /// The reason it is solved rather than integrated: the compositor and the
    /// shell step their halves of a card on their own frame timings, and a
    /// card whose two halves disagreed would show as a frame off its window.
    #[test]
    fn two_frame_rates_reach_the_same_place() {
        let run = |frame: f64, frames: usize| {
            let (mut at, mut speed) = (0.0, 0.0);
            for _ in 0..frames {
                (at, speed) = spring(at, speed, 100.0, CARD_SPRING, frame);
            }
            at
        };
        // A tenth of a second at 60Hz, at 120Hz, and in one long frame.
        let sixty = run(1.0 / 60.0, 6);
        assert!((sixty - run(1.0 / 120.0, 12)).abs() < 1e-9);
        assert!((sixty - run(0.1, 1)).abs() < 1e-9);
    }

    #[test]
    fn every_card_is_the_same_size() {
        let slots = card_slots(1920.0, 1080.0, 4, 0);
        assert_eq!(slots.len(), 4);
        for slot in &slots {
            assert!((slot.w - slots[0].w).abs() < 1e-9);
            assert!((slot.h - slots[0].h).abs() < 1e-9);
        }
    }

    /// The shell trims each card by repainting the wallpaper just outside it,
    /// so that a client whose surface runs past the window geometry it
    /// declared cannot hang a square-edged strip off the card. That margin —
    /// 8% of the card's smaller side, in the shell's quad shader — has to stay
    /// inside the space between two cards, or trimming one would erase the
    /// edge of the next.
    #[test]
    fn a_cards_trim_cannot_reach_its_neighbour() {
        const TRIM: f64 = 0.08;
        for (width, height) in [(1280.0, 800.0), (1920.0, 1080.0), (3840.0, 2160.0)] {
            let slots = card_slots(width, height, 3, 1);
            let gap = slots[1].y - slots[0].bottom();
            let trim = slots[0].w.min(slots[0].h) * TRIM;
            assert!(
                trim * 2.0 < gap,
                "{width}x{height}: two trims of {trim} do not fit in a gap of {gap}"
            );
        }
    }

    #[test]
    fn the_selected_card_is_centred_and_clears_the_sidebar() {
        for width in [1280.0, 1920.0, 3440.0] {
            for selected in 0..3 {
                let slots = card_slots(width, 1080.0, 3, selected);
                let card = &slots[selected];
                assert!(card.x > sidebar_width(width));
                assert!(card.right() < width);
                // Equal free space above and below the selected card.
                assert!((card.y - (1080.0 - card.bottom())).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn a_lone_card_is_centred_in_the_region() {
        let (width, height) = (1920.0, 1080.0);
        let slots = card_slots(width, height, 1, 0);
        let card = &slots[0];

        let margin = height * 0.05;
        let left = sidebar_width(width) + margin;
        let region_w = width - margin - left;
        let centre = left + region_w / 2.0;
        assert!(((card.x + card.w / 2.0) - centre).abs() < 1e-6);
        assert!((card.y - (height - card.bottom())).abs() < 1e-6);
    }

    #[test]
    fn the_neighbours_peek_past_the_screen_edges() {
        let (width, height) = (1920.0, 1080.0);
        let slots = card_slots(width, height, 3, 1);

        // The card above hangs off the top but its lower edge is on screen,
        // and symmetrically below — the cut-off card is the scroll affordance.
        assert!(slots[0].y < 0.0 && slots[0].bottom() > 0.0);
        assert!(slots[2].y < height && slots[2].bottom() > height);

        // The gap between cards has room for the title line drawn in it.
        assert!(slots[1].y - slots[0].bottom() > 40.0);
    }

    #[test]
    fn scrolling_slides_the_column_by_one_pitch() {
        let (width, height) = (1920.0, 1080.0);
        let count = 5;
        let a = card_slots(width, height, count, 1);
        let b = card_slots(width, height, count, 2);

        let pitch = a[1].y - a[0].y;
        assert!(pitch > a[0].h, "the pitch must include the title gap");
        for (before, after) in a.iter().zip(&b) {
            assert!((before.y - after.y - pitch).abs() < 1e-6);
            assert!((before.x - after.x).abs() < 1e-9, "only the y may move");
        }
    }

    #[test]
    fn a_selection_past_the_end_clamps_instead_of_scrolling_into_nothing() {
        let (width, height) = (1920.0, 1080.0);
        assert_eq!(
            card_slots(width, height, 3, 99),
            card_slots(width, height, 3, 2)
        );
    }

    #[test]
    fn a_narrow_region_shrinks_the_card_instead_of_overlapping_the_sidebar() {
        // Portrait-ish output: the 16:9 card at 0.54 of the height would be
        // wider than the space right of the sidebar.
        let (width, height) = (900.0, 1440.0);
        let slots = card_slots(width, height, 2, 0);
        for slot in &slots {
            assert!(slot.x >= sidebar_width(width));
            assert!(slot.right() <= width + 1e-6);
            assert!((slot.w / slot.h - 16.0 / 9.0).abs() < 1e-6);
        }
    }

    #[test]
    fn fit_preserves_aspect_and_stays_inside_the_slot() {
        let slot = Rect {
            x: 100.0,
            y: 50.0,
            w: 400.0,
            h: 300.0,
        };
        // Wider than the slot: pillar-boxed.
        let wide = fit(&slot, 1920.0, 1080.0);
        assert!((wide.w / wide.h - 1920.0 / 1080.0).abs() < 1e-6);
        assert!(wide.x >= slot.x && wide.right() <= slot.right() + 1e-6);
        // Taller than the slot: letter-boxed.
        let tall = fit(&slot, 600.0, 800.0);
        assert!((tall.w / tall.h - 600.0 / 800.0).abs() < 1e-6);
        assert!(tall.y >= slot.y && tall.bottom() <= slot.bottom() + 1e-6);

        // Degenerate windows must not produce NaN geometry.
        assert_eq!(fit(&slot, 0.0, 0.0), slot);
    }
}
