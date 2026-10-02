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
//!
//! A display too narrow for the two side by side — one standing on its side
//! — keeps both at the size they need rather than squeezing either, and lays
//! the cards out past its right-hand edge. Which half is then in view follows
//! which half the user is driving: the menu, with the cards peeking in at the
//! edge, or the cards, slid into the middle of the display with the edge of
//! the menu left showing. That is the second thing that travels over the
//! protocol — see [`Focus`].

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

/// The longest one step of [`spring`] is taken for, in seconds. Longer than this
/// is a stall, or a resume from sleep, and is not replayed.
pub const LONGEST_STEP: f64 = 0.1;

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
    let dt = dt.clamp(0.0, LONGEST_STEP);
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

/// The height every length the menu is laid out in is written against: a
/// display this many lines tall draws the menu at a scale of one.
const REFERENCE_HEIGHT: f64 = 1080.0;

/// How much larger than the reference the menu is drawn on a display `height`
/// tall — the shell's own `guide_scale`, to the digit.
///
/// Restated here rather than left to the shell, because the width of the menu
/// column depends on it now — see [`sidebar_width`] — and the compositor has
/// to arrive at the same column to put the windows beside it. The shell's
/// tests hold its copy to this one.
pub fn layout_scale(height: f64) -> f64 {
    (height / REFERENCE_HEIGHT).clamp(0.6, 2.5)
}

/// The narrowest the menu column may be, in reference pixels drawn at
/// [`layout_scale`].
///
/// The column's contents are laid out against the display's height and its
/// width was taken from the display's width alone, so a display standing on
/// its side gave them a good deal less room than they are: the hour printed
/// over the day beside it, and the four tiles taken in to under half their
/// size to share a line. This is the room they were settled in. A 16:10
/// display gives the column 22% of 1280 at 800 lines, which is 380 of these,
/// and every landscape display of that shape or wider, up to 1440 lines,
/// gives it at least that already — so every one of them, a Steam Deck's
/// included, draws exactly what it drew before. A squarer display, or one
/// taller than that at a scale of one, was squeezing the column in the same
/// way and is given this instead.
pub const SIDEBAR_LEAST: f64 = 380.0;

/// Width of the menu column for an output `width` × `height`.
///
/// Proportional with a floor and a ceiling: a fraction alone turns illegible
/// on a small nested window and cavernous on an ultrawide. And never narrower
/// than its own contents — see [`SIDEBAR_LEAST`] — even where that is more of
/// the display than the half it is otherwise held to: what then has no room
/// beside it waits past the display's edge rather than squeezing the column.
/// See [`reach`].
pub fn sidebar_width(width: f64, height: f64) -> f64 {
    let proportional = (width * 0.22).clamp(280.0, 520.0).min(width * 0.5);
    proportional
        .max(SIDEBAR_LEAST * layout_scale(height))
        .min(width.max(0.0))
}

/// Which half of the overview the user is driving: the menu column, or the
/// cards beside it.
///
/// On a display wide enough for the two side by side it changes nothing. On
/// one that is not, it is which of them is in view — the menu, with the cards
/// waiting past the right-hand edge, or the cards, with the view slid over to
/// them. Both processes need it for the reason they both need the selection:
/// the compositor draws the windows and the shell draws everything round
/// them, and the two have to slide as one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    /// The menu column, which is where the overview always opens.
    #[default]
    Menu,
    /// The cards.
    Cards,
}

/// The air round the cards, as a share of the display's shorter side.
///
/// The shorter side rather than the height, which on every landscape display
/// is the same number. On one standing on its side the height is the long
/// side, and a margin taken from it would be a sixth of the width spent on
/// air either side of a card.
const MARGIN: f64 = 0.05;

/// How much of the menu column is left in view while the cards have the
/// focus, at the least, as a share of the display's width.
///
/// The cut-off column is what says Left goes back to it, exactly as a
/// cut-off card says Up and Down go on, so the slide never takes it all away.
/// A card on a display standing on its side leaves more than this in view by
/// itself; this is what holds a wider card back from taking the rest.
const PEEK: f64 = 0.12;

/// How much larger a card the slide has to show before it is worth making.
///
/// A third. A card shrunk to fit beside the column is what every landscape
/// display has always drawn, and a display only a little too narrow for it
/// keeps that rather than start moving under the user for a card slightly
/// larger. The slide is for the display on which the card beside the column
/// is a sliver.
const SLIDE_GAIN: f64 = 4.0 / 3.0;

/// Where the cards stand with the menu in view, how large they are, and how
/// far the view slides to bring them in.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Deck {
    x: f64,
    w: f64,
    h: f64,
    reach: f64,
}

/// The shape of a card's slot.
///
/// 16:9 on a display at least as wide as it is tall, so that every card reads
/// as one of a set whatever shape its window is. On one taller than it is
/// wide, the display's own shape: every window on it is that shape too, and
/// fitted into a 16:9 slot one came out as a sliver in the middle of an empty
/// card.
fn card_aspect(width: f64, height: f64) -> f64 {
    if width >= height || width <= 0.0 {
        16.0 / 9.0
    } else {
        width / height
    }
}

/// Lay the cards out for a display `width` × `height`.
fn deck(width: f64, height: f64) -> Deck {
    let margin = width.min(height) * MARGIN;
    let left = sidebar_width(width, height) + margin;
    let region_w = (width - margin - left).max(0.0);
    let aspect = card_aspect(width, height);
    let card_h = height * CARD_HEIGHT;
    let card_w = card_h * aspect;

    // The card a view of the cards alone could show: all of it, or as much of
    // it as leaves the column's edge and a margin either side in view.
    let apart = card_w.min((width - 2.0 * (margin + width * PEEK)).max(0.0));
    if apart > card_w.min(region_w) * SLIDE_GAIN {
        return Deck {
            x: left,
            w: apart,
            h: apart / aspect,
            // Far enough to stand the card in the middle of the display,
            // which leaves as much of the column in view on its left as there
            // is wallpaper on its right.
            reach: (left + apart * 0.5 - width * 0.5).max(0.0),
        };
    }

    // A narrow region (a squarer display, a huge sidebar) shrinks the card
    // rather than running it under the menu.
    let (card_w, card_h) = if card_w > region_w && region_w > 0.0 {
        (region_w, region_w / aspect)
    } else {
        (card_w, card_h)
    };
    Deck {
        x: left + (region_w - card_w) / 2.0,
        w: card_w,
        h: card_h,
        reach: 0.0,
    }
}

/// How far the view slides to bring the cards in, on a display too narrow for
/// them beside the menu column, in logical pixels.
///
/// Nought on every display wide enough for both, which is every landscape
/// display: there the cards stand beside the column and nothing slides.
pub fn reach(width: f64, height: f64) -> f64 {
    deck(width, height).reach
}

/// How far left of where the layout puts them everything in the overview is
/// drawn — the column, the cards and whatever is on them — with `focus` in
/// charge.
pub fn pan(width: f64, height: f64, focus: Focus) -> f64 {
    match focus {
        Focus::Menu => 0.0,
        Focus::Cards => reach(width, height),
    }
}

/// The card slots for `count` cards with `selected` centred, in card order —
/// the current window first, the start screen last — with `focus` deciding
/// which half of a narrow display is in view. See [`pan`].
///
/// Every card gets a slot, including the ones hanging past the screen's
/// edges: both renderers clip for free, and a card that scrolls into view
/// eases from where it really was instead of materialising.
///
/// Slots are boxes, not final frames — a window is aspect-fitted into its
/// slot with [`fit`].
pub fn card_slots(
    width: f64,
    height: f64,
    count: usize,
    selected: usize,
    focus: Focus,
) -> Vec<Rect> {
    if count == 0 {
        return Vec::new();
    }
    let selected = selected.min(count - 1);

    let deck = deck(width, height);
    let x = deck.x
        - match focus {
            Focus::Menu => 0.0,
            Focus::Cards => deck.reach,
        };
    let pitch = deck.h + height * GAP;
    // The selected card is centred with equal free space above and below;
    // everything else hangs off it at a fixed pitch.
    let selected_y = (height - deck.h) / 2.0;

    (0..count)
        .map(|index| Rect {
            x,
            y: selected_y + (index as f64 - selected as f64) * pitch,
            w: deck.w,
            h: deck.h,
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

    /// The layout as it stood before the column had a width of its own to
    /// keep, transcribed so that what it drew can be compared against.
    fn old_sidebar_width(width: f64) -> f64 {
        (width * 0.22).clamp(280.0, 520.0).min(width * 0.5)
    }

    fn old_card_slots(width: f64, height: f64, count: usize, selected: usize) -> Vec<Rect> {
        let selected = selected.min(count - 1);
        let margin = height * 0.05;
        let left = old_sidebar_width(width) + margin;
        let region_w = (width - margin - left).max(0.0);
        let mut card_h = height * CARD_HEIGHT;
        let mut card_w = card_h * 16.0 / 9.0;
        if card_w > region_w && region_w > 0.0 {
            card_w = region_w;
            card_h = card_w * 9.0 / 16.0;
        }
        let pitch = card_h + height * GAP;
        let x = left + (region_w - card_w) / 2.0;
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

    /// The landscape displays this layout was drawn for are drawn exactly as
    /// they were: the same column, the same cards in the same places, and
    /// nothing that slides whichever half has the focus.
    #[test]
    fn a_landscape_display_is_laid_out_as_it_always_was() {
        for (width, height) in [
            (800.0, 600.0),
            (1024.0, 600.0),
            (1024.0, 768.0),
            (1280.0, 720.0),
            (1280.0, 800.0),
            (1366.0, 768.0),
            (1440.0, 900.0),
            (1600.0, 900.0),
            (1680.0, 1050.0),
            (1920.0, 1080.0),
            (1920.0, 1200.0),
            (2304.0, 1440.0),
            (2560.0, 1080.0),
            (2560.0, 1440.0),
            (3440.0, 1440.0),
            (3840.0, 1080.0),
            (5120.0, 1440.0),
        ] {
            assert_eq!(
                sidebar_width(width, height),
                old_sidebar_width(width),
                "{width}x{height}"
            );
            assert_eq!(reach(width, height), 0.0, "{width}x{height}");
            for (count, selected) in [(1, 0), (3, 1), (5, 4)] {
                let old = old_card_slots(width, height, count, selected);
                for focus in [Focus::Menu, Focus::Cards] {
                    let new = card_slots(width, height, count, selected, focus);
                    for (a, b) in old.iter().zip(&new) {
                        for (was, is) in [(a.x, b.x), (a.y, b.y), (a.w, b.w), (a.h, b.h)] {
                            assert!(
                                (was - is).abs() < 1e-6,
                                "{width}x{height} {focus:?}: {a:?} became {b:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    /// The column is never narrower than its own contents need at the size
    /// they are drawn — the fault this was written for, where the hour printed
    /// over the day beside it — on a display of any shape.
    #[test]
    fn the_column_keeps_the_room_its_contents_need() {
        for (width, height) in [
            (1280.0, 800.0),
            (1280.0, 1024.0),
            (2560.0, 1600.0),
            (3840.0, 2160.0),
            (1080.0, 1920.0),
            (620.0, 1473.0),
            (800.0, 1280.0),
        ] {
            let sidebar = sidebar_width(width, height);
            assert!(
                sidebar >= SIDEBAR_LEAST * layout_scale(height) - 1e-9,
                "{width}x{height}: a column {sidebar} wide"
            );
            assert!(sidebar <= width);
        }
    }

    /// A display standing on its side keeps the column and slides to the
    /// cards rather than shrinking them to nothing beside it.
    ///
    /// With the menu in view the whole column is on the display and the cards
    /// wait past its right-hand edge, the nearest of them peeking in. With the
    /// cards in view the selected one stands whole in the middle of the
    /// display, at the display's own shape, and the column's edge is still
    /// there on the left to go back to.
    #[test]
    fn a_display_on_its_side_slides_between_the_column_and_the_cards() {
        for (width, height) in [
            (620.0, 1473.0),
            (800.0, 1280.0),
            (1080.0, 1920.0),
            (1200.0, 1920.0),
            (1440.0, 2560.0),
            (1440.0, 3440.0),
        ] {
            let sidebar = sidebar_width(width, height);
            let reach = reach(width, height);
            assert!(reach > 0.0, "{width}x{height} has room for both");

            let menu = card_slots(width, height, 3, 1, Focus::Menu)[1];
            assert!(menu.x >= sidebar, "{width}x{height}: under the column");
            assert!(menu.x < width, "{width}x{height}: nothing peeking in");
            assert!(
                menu.right() > width,
                "{width}x{height}: nothing to slide to"
            );

            let cards = card_slots(width, height, 3, 1, Focus::Cards)[1];
            assert!(cards.x > 0.0 && cards.right() < width, "{width}x{height}");
            assert!(
                ((cards.x + cards.w * 0.5) - width * 0.5).abs() < 1e-6,
                "{width}x{height}: the card is not in the middle"
            );
            assert!((cards.w / cards.h - width / height).abs() < 1e-9);
            assert!(
                sidebar - reach >= width * PEEK - 1e-9,
                "{width}x{height}: the column's edge went too"
            );
            // Slid, not re-laid: the same card, the whole view moved over.
            assert!((menu.x - reach - cards.x).abs() < 1e-9);
            assert_eq!((menu.y, menu.w, menu.h), (cards.y, cards.w, cards.h));
            assert_eq!(pan(width, height, Focus::Cards), reach);
            assert_eq!(pan(width, height, Focus::Menu), 0.0);
        }
    }

    /// And slides only where it is worth it: a card that is still a card
    /// beside the column stays there, on a display a little taller than it is
    /// wide and on a square one.
    #[test]
    fn a_display_with_room_for_a_card_beside_the_column_keeps_it_there() {
        for (width, height) in [(1080.0, 1080.0), (1200.0, 1600.0), (1080.0, 1350.0)] {
            assert_eq!(reach(width, height), 0.0, "{width}x{height}");
            let slot = card_slots(width, height, 1, 0, Focus::Cards)[0];
            assert!(slot.x >= sidebar_width(width, height), "{width}x{height}");
            assert!(slot.right() <= width, "{width}x{height}");
        }
    }

    #[test]
    fn every_card_is_the_same_size() {
        let slots = card_slots(1920.0, 1080.0, 4, 0, Focus::Menu);
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
        for (width, height) in [
            (1280.0, 800.0),
            (1920.0, 1080.0),
            (3840.0, 2160.0),
            (1080.0, 1920.0),
            (620.0, 1473.0),
        ] {
            let slots = card_slots(width, height, 3, 1, Focus::Menu);
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
                let slots = card_slots(width, 1080.0, 3, selected, Focus::Menu);
                let card = &slots[selected];
                assert!(card.x > sidebar_width(width, 1080.0));
                assert!(card.right() < width);
                // Equal free space above and below the selected card.
                assert!((card.y - (1080.0 - card.bottom())).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn a_lone_card_is_centred_in_the_region() {
        let (width, height) = (1920.0, 1080.0);
        let slots = card_slots(width, height, 1, 0, Focus::Menu);
        let card = &slots[0];

        let margin = height * 0.05;
        let left = sidebar_width(width, height) + margin;
        let region_w = width - margin - left;
        let centre = left + region_w / 2.0;
        assert!(((card.x + card.w / 2.0) - centre).abs() < 1e-6);
        assert!((card.y - (height - card.bottom())).abs() < 1e-6);
    }

    #[test]
    fn the_neighbours_peek_past_the_screen_edges() {
        let (width, height) = (1920.0, 1080.0);
        let slots = card_slots(width, height, 3, 1, Focus::Menu);

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
        let a = card_slots(width, height, count, 1, Focus::Menu);
        let b = card_slots(width, height, count, 2, Focus::Menu);

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
            card_slots(width, height, 3, 99, Focus::Menu),
            card_slots(width, height, 3, 2, Focus::Menu)
        );
    }

    /// A region narrower than the card shrinks the card rather than running
    /// it under the menu — where the slide is not worth making.
    #[test]
    fn a_narrow_region_shrinks_the_card_instead_of_overlapping_the_sidebar() {
        // Squarer than any display this was drawn for: the 16:9 card at 0.54
        // of the height would be wider than the space right of the sidebar.
        let (width, height) = (1080.0, 1080.0);
        let slots = card_slots(width, height, 2, 0, Focus::Menu);
        for slot in &slots {
            assert!(slot.x >= sidebar_width(width, height));
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
