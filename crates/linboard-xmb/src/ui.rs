//! Turns the [`Xmb`] model into flat lists of quads and text runs.
//!
//! The layout is anchored on a single "cross" point. Categories run
//! horizontally through it and the selected category's applications run
//! vertically through it, which is what makes the shape a cross rather than
//! two unrelated lists.
//!
//! Following the original cross media bar, the item column is *split around*
//! the category row: everything before the selection sits above the row,
//! the selection and everything after it sit below, and a scrolling entry
//! glides through the widened gap between the two halves. That gap is what
//! keeps item labels and category icons from ever printing over one another.

use crate::gpu::{Quad, Text, TextAlign, GLOW_SLOT, SOLID_SLOT, SQUIRCLE_CORNER};
use crate::guide::{self, separator_rows, Bar, Guide, Item, Pane};
use crate::icons;
use crate::keyboard;
use crate::model::{Cursor, Xmb};
use crate::system::Level;
use crate::theme::theme;
use linboard_protocol::overview;

/// Layout constants, expressed against a 1080p reference and scaled at runtime.
const REFERENCE_HEIGHT: f32 = 1080.0;
const CATEGORY_SPACING: f32 = 200.0;
const ITEM_SPACING: f32 = 124.0;

/// Icon sizes. The focused entry is nearly twice its neighbours — that jump in
/// scale, together with the glow, is what makes the selection legible from a
/// couch, which a same-size square never was.
const CATEGORY_ICON: f32 = 84.0;
const CATEGORY_ICON_FOCUSED: f32 = 148.0;
const ITEM_ICON: f32 = 64.0;
/// Not chosen on its own, but against the disc under it: the disc is what the
/// column's gaps are measured in, so the icon is the one of the two free to
/// move once the fit below is satisfied.
const ITEM_ICON_FOCUSED: f32 = 105.0;
/// The glass disc an icon stands on, as a multiple of the icon. The item's is
/// named because the launch splash grows out of exactly that disc; the
/// category's because the label under it has to clear it.
///
/// The item's is a true circle, so what it has to clear is the icon's
/// *corners*, not its sides: a square reaches √2 half-widths from its centre,
/// and anything narrower than that crops the corners of every icon whose art
/// runs to its own edge — a white tile with a picture on it, of which there
/// are plenty. Those corners hanging over the rim read as the icon having
/// slipped off the selection rather than as a round tile. The margin above √2
/// is the sliver of glass that says it is standing on the disc.
///
/// The category's is a squircle, which at a fourth-power corner already holds
/// a square this size with room to spare.
const ITEM_DISC: f32 = std::f32::consts::SQRT_2 * 1.04;
const CATEGORY_DISC: f32 = 1.30;
/// The selected category's name, and the air on either side of it.
///
/// Not the same air above as below. A label belongs to the thing it names, and
/// what says so is being nearer to it than to anything else — set evenly
/// between the button above and the column below, it reads as a caption
/// stranded between two rows rather than as part of the button. The two still
/// sum to what they did, so tightening the one above does not drag the item
/// column up with it.
const CATEGORY_LABEL: f32 = 26.0;
const CATEGORY_LABEL_ABOVE: f32 = 3.0;
const CATEGORY_LABEL_BELOW: f32 = 25.0;

/// The category row sits in a gap in the item column: these are the distances
/// from the row to the centre of the nearest item above and below it.
const ITEM_GAP_ABOVE: f32 = 168.0;
/// Below is larger because the selected category's label lives in that
/// stretch, and it is added up rather than chosen so that it cannot stop
/// being true. What has to fit is the category's own disc, the label clear of
/// it, and air before the first item's disc begins — three sizes that have
/// all changed at least once since this row was first laid out, each time
/// leaving a hand-picked total quietly wrong.
const ITEM_GAP_BELOW: f32 = CATEGORY_ICON_FOCUSED * CATEGORY_DISC / 2.0
    + CATEGORY_LABEL_ABOVE
    + CATEGORY_LABEL
    + CATEGORY_LABEL_BELOW
    + ITEM_ICON_FOCUSED * ITEM_DISC / 2.0;

/// Where the cross's arms meet, as a share of the display. The focused entry
/// sits here, so it is also where a launch opens from.
const BAR_CROSS_X: f32 = 0.22;
const BAR_CROSS_Y: f32 = 0.30;

/// The launch splash: how big the application's icon settles at, how many
/// dots go round it and how long the light takes to travel the ring.
const LAUNCH_ICON: f32 = 168.0;
const LAUNCH_DOTS: usize = 12;
const LAUNCH_SPIN: f32 = 1.4;

/// Seconds per breath of the selection glow.
const PULSE_PERIOD: f32 = 1.8;

/// Dimmest a category on the row may get. Far enough below 1.0 to read as
/// unfocused, far enough above 0.0 that it is never invisible.
const CATEGORY_MIN_ALPHA: f32 = 0.62;

/// How much a display that is not taking input is dimmed by.
///
/// The bars on other displays stay legible — the user is choosing which one to
/// move to — but must not compete with the one the controller is driving.
const UNFOCUSED_DIM: f32 = 0.45;

/// Guide overlay metrics, likewise against 1080p.
const GUIDE_ROW_HEIGHT: f32 = 76.0;
/// A quick-settings bar's row. Shorter than a button's: it holds a glyph and a
/// track, with no label needing air around it.
const GUIDE_BAR_HEIGHT: f32 = 62.0;
/// The line the two tiles share, and how big a tile is on it. Taller than a
/// button's row because a tile is square and a square the width of a row would
/// be half the sidebar.
const GUIDE_TILE_ROW: f32 = 86.0;
const GUIDE_TILE: f32 = 68.0;
/// The air between one tile and the next.
const GUIDE_TILE_GAP: f32 = 14.0;
/// The glyph inside a tile, as a fraction of it.
const GUIDE_TILE_GLYPH: f32 = 0.62;
/// How round a tile's corners are, as a fraction of its height. A capsule —
/// which is what every other chip in the column is — would make a square one a
/// disc, and a disc reads as a button that does something once rather than as
/// a switch that is in a state.
const GUIDE_TILE_RADIUS: f32 = 0.30;
const GUIDE_PADDING: f32 = 44.0;
/// How long the sidebar takes to slide in, seconds.
const GUIDE_SLIDE: f32 = 0.28;
/// Where the entry column starts, below the header.
///
/// Clearance for the tallest the header gets — the clock, the application, and
/// the name of the display when there is more than one — and no more. What is
/// under the header is now the two quick-settings bars rather than the first
/// button, and a bar reads as belonging to the header above it; left at the
/// old distance it looked like a third list on its own.
const GUIDE_ENTRIES_TOP: f32 = 178.0;
/// The space the rule between the application entries and the rest opens up.
const GUIDE_SEPARATOR_GAP: f32 = 30.0;
/// How far the column is inset from the sidebar's own edges. The power button
/// keeps the same distance from the foot of the sidebar as from its side, so
/// the square in the corner is padded like the chips above it instead of
/// floating away from one edge and hugging the other.
const GUIDE_MARGIN: f32 = 16.0;
/// The power button at the foot of the column. Square, so [`GUIDE_MARGIN`] on
/// every side is all it takes to sit properly in the corner.
const POWER_BUTTON: f32 = 66.0;

/// The glyph inside it, as a fraction of the button. This is the *ring's*
/// radius, and the stroke rises a third of that again above the ring, so the
/// whole symbol stands `2.32 × POWER_GLYPH` tall — a little over half the
/// button, which is what keeps it inside its chip instead of overhanging it.
const POWER_GLYPH: f32 = 0.26;

/// How much of shutdown.svg's cell the symbol itself covers, top to bottom.
/// The rest is the margin its shadow needs. Dividing the height the button
/// wants by this gives the atlas cell that produces it, so the drawn glyph
/// lands at the same size the two quads above it used to.
const SHUTDOWN_INK: f32 = 0.775;

/// Corner radii, against the same 1080p reference. Controls are capsules —
/// their radius is half their own height — so what is left to name is the
/// card, which follows the shape of the window inside it.
pub const CARD_RADIUS: f32 = 18.0;
/// Panels — the guide's sidebar and the power dialog — are the largest shapes
/// the shell draws and carry the largest radius.
const PANEL_RADIUS: f32 = 30.0;
/// How far the sidebar floats clear of the screen's edges. Glass reads as a
/// slab laid *over* the wallpaper, which needs the wallpaper to run past it.
const PANEL_INSET: f32 = 14.0;

/// The glass material, in one place so every pane is cut from the same stuff.
///
/// `DEPTH` is how thick the slab is, in reference pixels, which is also how
/// wide its rim is rounded over: a panel is a slab, a control is a lozenge cut
/// from a thinner sheet. `FROST` is how much of what it transmits it
/// scatters — a panel has to carry text over anything at all, a control sits
/// on something already legible and can afford to be nearly clear. `GLOSS` is
/// how strongly it takes the light: full for anything the user can act on,
/// halved for surfaces that are only a place to put things.
///
/// Every one of them is a property of the material rather than of the picture,
/// which is why there is no separate knob for the rim, the sheen or the
/// shadow. Those are what a slab of these dimensions *does* with the one lamp
/// the shell is lit by.
const DEPTH_PANEL: f32 = 22.0;
const DEPTH_CONTROL: f32 = 9.0;
const FROST_PANEL: f32 = 0.95;
const FROST_CONTROL: f32 = 0.2;
const GLOSS_FULL: f32 = 1.0;
const GLOSS_QUIET: f32 = 0.45;

/// The pulsing halo outside the selection frame: how many rings approximate
/// the falloff, and how far apart they sit against the 1080p reference.
const HALO_RINGS: usize = 5;
const HALO_STEP: f32 = 3.0;

/// The power dialog, centred on the display: its width, its row height, and
/// how far the rest of the overlay is dimmed behind it.
const DIALOG_WIDTH: f32 = 460.0;
const DIALOG_ROW: f32 = 64.0;
const DIALOG_DIM: f32 = 0.28;

/// The share of the dialog's growth that passes before its contents begin to
/// appear. A folder's icons do not read at the size of a folder icon, and
/// letting them try makes the whole thing look like a shrunken dialog being
/// enlarged rather than something opening out of the button.
const DIALOG_CONTENT_IN: f32 = 0.45;

/// The entries arrive one after another: how long the first of them waits for
/// the slab to arrive under it, how long each waits behind the one above it,
/// and how long its own slide takes, in seconds.
///
/// The lead is what makes the top of the column an entrance at all. Without
/// it the first entries fade up *while the sidebar is still sliding in*, and
/// by the time the slab has settled they have settled too — so the two tiles
/// at the head of the column, which are the first two entries, simply arrived
/// with the panel already drawn, while everything below them was still
/// visibly coming in. They were not missing an animation; they were playing
/// it behind the one thing that was moving.
const ENTRY_LEAD: f32 = 0.12;
const ENTRY_STAGGER: f32 = 0.045;
const ENTRY_SLIDE: f32 = 0.26;

/// How much of the lit capsule has arrived over the entry at `rect`: 1 when it
/// is sitting on it, 0 while it is still an entry away.
///
/// Measured in the entry's own widths and heights rather than in seconds, so it
/// answers the only question that matters — is there light on this entry yet —
/// however long the glide takes and however many entries it crosses.
///
/// All four numbers, not just the vertical position. A column of full-width
/// rows only ever differs in `y`, so for those this is what it always was; the
/// two tiles share a line, and one that asked about `y` alone would call the
/// light *arrived* the instant the selection moved anywhere along that line —
/// which is a tile handing its own chip over while the light is still crossing
/// the gap to it, and a hole in the sidebar for the length of the glide. The
/// size counts for the same reason: a capsule the width of the sidebar sitting
/// exactly on a tile's corner is not light on that tile, it is a row-shaped
/// chip that has not become a tile yet.
fn highlight_arrival(highlight: [f32; 4], rect: [f32; 4]) -> f32 {
    let (w, h) = (rect[2].max(1.0), rect[3].max(1.0));
    let apart = (highlight[0] - rect[0]).abs() / w
        + (highlight[1] - rect[1]).abs() / h
        + (highlight[2] - rect[2]).abs() / w
        + (highlight[3] - rect[3]).abs() / h;
    1.0 - apart.clamp(0.0, 1.0)
}

/// How far entry `index` has arrived, `age` seconds after the menu opened.
fn entry_appear(age: f32, index: usize) -> f32 {
    ease((age - ENTRY_LEAD - index as f32 * ENTRY_STAGGER) / ENTRY_SLIDE)
}

/// The chip rectangle for menu entry `index`, in sidebar-local coordinates:
/// x is measured from the sidebar's own left edge, so the caller can ease the
/// selection between rows without the slide-in animation dragging it about.
///
/// Shared by the drawing below and by the caller easing the selection, for
/// the same reason the card layout is shared with the compositor — two places
/// computing one rectangle have to agree.
///
/// The whole column is passed rather than just a row number because two
/// entries move the rest: the rule above Dashboard, and the power button,
/// which leaves the stack entirely for the foot of the sidebar.
pub fn menu_item_rect(items: &[Item], index: usize, width: f32, height: f32) -> [f32; 4] {
    let scale = guide_scale(height);
    let [panel_x, _, panel_w, _] = sidebar_panel_rect(width, height);
    let margin = GUIDE_MARGIN * scale;

    if items.get(index) == Some(&Item::Power) {
        return power_button_rect(width, height);
    }

    // Stacked rather than multiplied out, because the rows are no longer all
    // the same height: a bar is shorter than a button, and the tiles share one
    // line between them.
    let rules = separator_rows(items);
    let lines = guide::lines(items);
    let mut y = GUIDE_ENTRIES_TOP * scale;
    for (first, count) in &lines {
        if rules.contains(first) {
            y += GUIDE_SEPARATOR_GAP * scale;
        }
        if index < first + count {
            // The line the entry is on. A tile is placed along it; everything
            // else fills it.
            let row = row_height(items[*first]) * scale;
            if items[*first].is_tile() {
                let size = GUIDE_TILE * scale;
                let column = (index - first) as f32;
                return [
                    panel_x + margin + column * (size + GUIDE_TILE_GAP * scale),
                    y + (row - size) * 0.5,
                    size,
                    size,
                ];
            }
            return [
                panel_x + margin,
                y + 5.0 * scale,
                panel_w - margin * 2.0,
                row - 10.0 * scale,
            ];
        }
        y += row_height(items[*first]) * scale;
    }

    // Past the end of the column, which only an index nothing selected can be.
    [panel_x + margin, y, panel_w - margin * 2.0, 0.0]
}

/// How much of the column a line takes up, before its chip's own padding.
fn row_height(item: Item) -> f32 {
    if item.is_tile() {
        GUIDE_TILE_ROW
    } else if item.bar().is_some() {
        GUIDE_BAR_HEIGHT
    } else {
        GUIDE_ROW_HEIGHT
    }
}

/// How round an entry's chip is. Everything in the column is a capsule but the
/// tiles, which are rounded squares — see [`GUIDE_TILE_RADIUS`].
fn chip_radius(item: Option<Item>, height: f32) -> f32 {
    if item.is_some_and(Item::is_tile) {
        height * GUIDE_TILE_RADIUS
    } else {
        height * 0.5
    }
}

/// The power button's chip: a square in the sidebar's bottom-left corner,
/// [`GUIDE_MARGIN`] from both edges.
///
/// Its own function rather than a branch of [`menu_item_rect`] because it is
/// the one entry whose place has nothing to do with how many rows are above
/// it — and because the dialog it opens has to grow out of this rectangle.
pub fn power_button_rect(width: f32, height: f32) -> [f32; 4] {
    let scale = guide_scale(height);
    let [panel_x, panel_y, _, panel_h] = sidebar_panel_rect(width, height);
    let margin = GUIDE_MARGIN * scale;
    let size = POWER_BUTTON * scale;
    [
        panel_x + margin,
        panel_y + panel_h - margin - size,
        size,
        size,
    ]
}

/// The sidebar itself: a slab floating clear of the display's edges, in
/// sidebar-local coordinates like everything else here.
///
/// It floats because glass only reads as a layer when what it is laid over
/// runs past it — pinned to the edges it is just a differently coloured
/// region of the screen.
pub fn sidebar_panel_rect(width: f32, height: f32) -> [f32; 4] {
    let scale = guide_scale(height);
    let inset = PANEL_INSET * scale;
    let sidebar_w = overview::sidebar_width(width as f64) as f32;
    [inset, inset, sidebar_w - inset * 2.0, height - inset * 2.0]
}

/// The rules, in the same coordinates: one above each row where the column
/// changes from one kind of thing to another.
fn menu_separator_rects(items: &[Item], width: f32, height: f32) -> Vec<[f32; 4]> {
    let scale = guide_scale(height);
    separator_rows(items)
        .into_iter()
        .map(|row| {
            let [x, y, w, _] = menu_item_rect(items, row, width, height);
            [
                x + w * 0.06,
                y - GUIDE_SEPARATOR_GAP * scale * 0.5,
                w * 0.88,
                (1.0 * scale).max(1.0),
            ]
        })
        .collect()
}

/// The guide's layout scale for a display `height` tall.
fn guide_scale(height: f32) -> f32 {
    (height / REFERENCE_HEIGHT).clamp(0.6, 2.5)
}

/// One frame's worth of drawing.
///
/// Quads are drawn before text, so a scene cannot be layered over another one:
/// the lower scene's text would rise above the upper one's panels. Each frame
/// therefore draws exactly one scene.
#[derive(Default)]
pub struct Scene {
    pub quads: Vec<Quad>,
    pub texts: Vec<Text>,
}

impl Scene {
    /// Squeeze this scene into `rect` (`[x, y, w, h]`) of a `width`-wide
    /// display.
    ///
    /// The start screen's overview card is drawn this way: the same bar,
    /// laid out for the whole display and then shrunk, so the card is a
    /// miniature of what selecting it returns to — icons and all — rather
    /// than a second, smaller layout that would look like a different
    /// screen. Interpolating the rectangle towards the display's own bounds
    /// is what flies it back up to fullscreen.
    pub fn place_into(&mut self, rect: [f32; 4], width: f32, height: f32) {
        let [x, y, w, _] = rect;
        if width <= 0.0 || w <= 0.0 {
            return;
        }
        let scale = w / width;

        // A display clips whatever runs past its edges — the category row
        // carries on off both sides — but a card has no edge to hide an
        // overhang against, so what the display would cut is dropped here
        // instead. Text keeps any run that shows at all: its box is a
        // generous wrapping width rather than the ink, and a right-aligned
        // clock reaches the edge without ever touching it.
        self.quads
            .retain(|q| q.x >= 0.0 && q.y >= 0.0 && q.x + q.w <= width && q.y + q.h <= height);
        self.texts.retain(|t| {
            t.x < width && t.y < height && t.x + t.max_width > 0.0 && t.y + t.size > 0.0
        });
        for quad in &mut self.quads {
            quad.x = x + quad.x * scale;
            quad.y = y + quad.y * scale;
            quad.w *= scale;
            quad.h *= scale;
            quad.radius *= scale;
            quad.border *= scale;
            quad.notch *= scale;
            // The glass is miniaturised along with everything else rather than
            // switched off: a pane reads what is behind it out of the frame,
            // and inside a card what is behind it is the miniature — the same
            // scene this one is part of, at the same scale. A slab left at
            // full depth in a thumbnail would be a card-sized bevel.
            quad.thickness *= scale;
        }
        for text in &mut self.texts {
            text.x = x + text.x * scale;
            text.y = y + text.y * scale;
            text.size *= scale;
            text.max_width *= scale;
        }
    }

    /// Scale everything in this scene by `factor`, then shift it by `offset`.
    ///
    /// [`place_into`](Self::place_into)'s transform without the clipping and
    /// without the origin fixed at the display's corner: what the power
    /// dialog's contents ride out of the button on, laid out once at their
    /// settled size and then carried to wherever the panel currently is.
    pub fn scale_by(&mut self, factor: f32, offset: [f32; 2]) {
        for quad in &mut self.quads {
            quad.x = quad.x * factor + offset[0];
            quad.y = quad.y * factor + offset[1];
            quad.w *= factor;
            quad.h *= factor;
            quad.radius *= factor;
            quad.border *= factor;
            quad.notch *= factor;
            quad.thickness *= factor;
        }
        for text in &mut self.texts {
            text.x = text.x * factor + offset[0];
            text.y = text.y * factor + offset[1];
            text.size *= factor;
            text.max_width *= factor;
        }
    }

    /// Multiply every alpha in the scene, for fading a whole layer in or out.
    ///
    /// Through the pane's own opacity, not its colour: on glass, the alpha in
    /// `color` is how strongly the pane *stains* what is behind it, and much
    /// of what makes it visible — the lit rim especially — is added by the
    /// shader afterwards. Dimming the tint would leave a fading pane at full
    /// strength and merely less purple.
    pub fn fade(&mut self, alpha: f32) {
        for quad in &mut self.quads {
            quad.fade *= alpha;
        }
        for text in &mut self.texts {
            text.color[3] *= alpha;
        }
    }

    /// Drop the text runs a modal panel covers.
    ///
    /// Every quad in a scene is drawn before every text run, so a panel laid
    /// over a scene does not hide that scene's labels — they print straight
    /// through it. Text the panel would have covered therefore has to go,
    /// which is exactly what being behind an opaque panel means.
    pub fn hide_text_behind(&mut self, rect: [f32; 4]) {
        let [x, y, w, h] = rect;
        self.texts.retain(|text| {
            text.x >= x + w
                || text.x + text.max_width <= x
                || text.y >= y + h
                || text.y + text.size * 1.4 <= y
        });
    }

    /// Fade out text approaching `edge` from the left, over `feather` pixels.
    ///
    /// Within a scene every quad is drawn before every text run, so a bar
    /// flying past the guide's sidebar would print its labels *over* the
    /// panel that should be covering them. Dissolving them as they reach it
    /// reads as passing behind, which is what the eye expects.
    pub fn fade_text_before(&mut self, edge: f32, feather: f32) {
        if feather <= 0.0 {
            return;
        }
        for text in &mut self.texts {
            let visible = ((text.x - (edge - feather)) / feather).clamp(0.0, 1.0);
            text.color[3] *= visible;
        }
    }
}

/// Look up an atlas slot for an icon name.
pub trait SlotLookup {
    fn slot_for(&self, icon: Option<&str>) -> Option<u32>;

    /// One of the shell's own glyphs, with no fallback.
    ///
    /// Separate from [`Self::slot_for`], which stands in an application icon
    /// for anything it cannot find: a speaker that came out as the generic
    /// executable icon would be worse than an empty space, because it would
    /// read as an application sitting in the volume row.
    fn glyph(&self, name: &str) -> Option<u32> {
        self.slot_for(Some(name))
    }
}

/// Lay out one display's bar.
///
/// `focused` is whether this is the display the controller and keyboard are
/// driving. Every display draws its own [`Cursor`], so the others show what
/// they are pointing at, dimmed, rather than a copy of this one.
///
/// `time` runs the selection pulse.
#[allow(clippy::too_many_arguments)]
pub fn build(
    xmb: &Xmb,
    cursor: &Cursor,
    width: f32,
    height: f32,
    focused: bool,
    clock: Option<&str>,
    time: f32,
    slots: &impl SlotLookup,
) -> Scene {
    let mut quads = Vec::new();
    let mut texts = Vec::new();
    let theme = theme();

    let scale = (height / REFERENCE_HEIGHT).clamp(0.6, 2.5);
    let cross_x = width * BAR_CROSS_X;
    let cross_y = height * BAR_CROSS_Y;
    let attention = if focused { 1.0 } else { UNFOCUSED_DIM };
    // The glow breathes: 0..1 and back over PULSE_PERIOD seconds.
    let pulse = 0.5 + 0.5 * (time * std::f32::consts::TAU / PULSE_PERIOD).sin();

    // The clock lives on every display; it is part of the wallpaper more than
    // part of the controls.
    if let Some(clock) = clock {
        let clock_size = 24.0 * scale;
        let box_w = 360.0 * scale;
        texts.push(Text {
            content: clock.to_string(),
            x: width - 48.0 * scale - box_w,
            y: 36.0 * scale,
            size: clock_size,
            color: theme.text_soft.a(0.85 * attention),
            bold: false,
            max_width: box_w,
            align: TextAlign::Right,
        });
    }

    if xmb.is_empty() {
        texts.push(Text {
            content: "No applications found".to_string(),
            x: cross_x,
            y: cross_y,
            size: 32.0 * scale,
            color: theme.text.a(0.85 * attention),
            bold: true,
            max_width: width,
            align: TextAlign::Left,
        });
        return Scene { quads, texts };
    }

    let category_spacing = CATEGORY_SPACING * scale;
    let item_spacing = ITEM_SPACING * scale;
    let gap_above = ITEM_GAP_ABOVE * scale;
    let gap_below = ITEM_GAP_BELOW * scale;

    // While the row glides sideways the column belongs to nobody: fade it out
    // with the old category and back in with the new one, as the original bar
    // does, instead of teleporting its contents.
    let category_travel = (cursor.category_position - cursor.selected_category as f32)
        .abs()
        .min(1.0);
    let column_alpha = (1.0 - category_travel) * attention;

    // Fixed text column: anchored to the focused entry's extent so labels do
    // not shuffle sideways as focus (and therefore icon size) moves around.
    //
    // Its disc's extent, not its icon's — the same distinction the category's
    // label is measured with. The glass is half again the icon standing on it,
    // and a column measured from the icon puts the names over the rim.
    let text_x = cross_x + ITEM_ICON_FOCUSED * ITEM_DISC * scale / 2.0 + 12.0 * scale;
    let text_max = (width - text_x - 48.0 * scale).max(0.0);

    // --- applications of the selected category ---------------------------
    // Drawn first so the category row overlaps them, as on the real bar.
    if let Some(category) = cursor.current_category(xmb) {
        if category.apps.is_empty() && column_alpha > 0.01 {
            texts.push(Text {
                content: category.empty_note().to_string(),
                x: text_x,
                y: cross_y + gap_below - 14.0 * scale,
                size: 22.0 * scale,
                color: theme.text_soft.a(0.7 * column_alpha),
                bold: false,
                max_width: text_max,
                align: TextAlign::Left,
            });
        }

        for (index, app) in category.apps.iter().enumerate() {
            if column_alpha <= 0.01 {
                break;
            }
            let offset = index as f32 - cursor.item_position;
            let y = item_y(offset, cross_y, gap_above, gap_below, item_spacing);

            // Skip rows that cannot be on screen.
            if y < -item_spacing || y > height + item_spacing {
                continue;
            }

            let distance = offset.abs();
            let focus = 1.0 - distance.min(1.0);
            // Fade with distance so the column dissolves instead of ending
            // abruptly.
            let mut alpha = (1.0 - (distance / 6.0)).clamp(0.0, 1.0) * column_alpha;
            // An entry crossing the category row dips while it passes behind,
            // so the two never fight for the same pixels at full strength.
            if offset > -1.0 && offset < 0.0 {
                alpha *= 1.0 - 0.7 * (1.0 - (2.0 * offset + 1.0).abs());
            }
            // And the column dissolves before the screen edges. The top keeps
            // clear of the clock; at the bottom only the row's own half-icon
            // and a small margin remain now that there is no control footer.
            let fade_range = 70.0 * scale;
            alpha *= ((y - 90.0 * scale) / fade_range).clamp(0.0, 1.0);
            let bottom_clearance = (ITEM_ICON / 2.0 + 16.0) * scale;
            alpha *= ((height - bottom_clearance - y) / fade_range).clamp(0.0, 1.0);
            if alpha <= 0.01 {
                continue;
            }

            let selected = distance < 0.5;
            let icon_size = lerp(ITEM_ICON, ITEM_ICON_FOCUSED, focus) * scale;

            if selected {
                // The breathing bloom behind the focused entry, and then the
                // glass it stands on: a disc of the same material the menu's
                // buttons are cut from, so "this is the thing you have
                // chosen" looks the same everywhere in the shell.
                let glow = icon_size * (2.3 + 0.2 * pulse);
                quads.push(Quad {
                    x: cross_x - glow / 2.0,
                    y: y - glow / 2.0,
                    w: glow,
                    h: glow,
                    slot: GLOW_SLOT,
                    color: theme.accent.a((0.34 + 0.26 * pulse) * alpha),
                    ..Quad::default()
                });
                let disc = icon_size * ITEM_DISC;
                quads.push(Quad {
                    x: cross_x - disc / 2.0,
                    y: y - disc / 2.0,
                    w: disc,
                    h: disc,
                    slot: SOLID_SLOT,
                    // Lightly stained, because what is behind it is already
                    // the accent: the bloom above shows *through* this, and
                    // tinting a purple bloom purple is how a pane stops
                    // looking like glass and starts looking like paint.
                    color: theme.accent.a(0.13),
                    radius: disc / 2.0,
                    thickness: DEPTH_CONTROL * scale,
                    frost: FROST_CONTROL,
                    gloss: GLOSS_FULL,
                    fade: alpha,
                    ..Quad::default()
                });
            }

            quads.push(icon_quad(
                slots.slot_for(app.icon.as_deref()),
                cross_x - icon_size / 2.0,
                y - icon_size / 2.0,
                icon_size,
                alpha,
                theme.accent_deep.a(alpha * 0.75),
            ));

            if selected {
                let name_size = 30.0 * scale;
                let has_comment = app.comment.is_some();
                // With a comment the pair straddles the icon's centre line;
                // without one the name alone sits on it.
                let name_y = if has_comment {
                    y - 40.0 * scale
                } else {
                    y - name_size * 0.62
                };
                texts.push(Text {
                    content: app.name.clone(),
                    x: text_x,
                    y: name_y,
                    size: name_size,
                    color: theme.text.a(alpha),
                    bold: true,
                    max_width: text_max,
                    align: TextAlign::Left,
                });
                if let Some(comment) = &app.comment {
                    texts.push(Text {
                        content: comment.clone(),
                        x: text_x,
                        y: y + 4.0 * scale,
                        size: 19.0 * scale,
                        color: theme.text_soft.a(alpha * 0.85),
                        bold: false,
                        max_width: text_max,
                        align: TextAlign::Left,
                    });
                }
            } else {
                let text_size = 22.0 * scale;
                texts.push(Text {
                    content: app.name.clone(),
                    x: text_x,
                    y: y - text_size * 0.62,
                    size: text_size,
                    color: theme.text.a(alpha * 0.62),
                    bold: false,
                    max_width: text_max,
                    align: TextAlign::Left,
                });
            }
        }
    }

    // --- the category row ------------------------------------------------
    for (index, category) in xmb.categories.iter().enumerate() {
        let offset = index as f32 - cursor.category_position;
        let x = cross_x + offset * category_spacing;

        if x < -category_spacing || x > width + category_spacing {
            continue;
        }

        let distance = offset.abs();
        let focus = 1.0 - distance.min(1.0);
        // Unlike the item column, the category row never fades a category out:
        // the row is the map of where everything lives, so every category that
        // fits on screen has to stay readable. Distance only dims it, down to a
        // floor, to keep the selected one obviously selected.
        let alpha = (1.0 - distance * 0.10).clamp(CATEGORY_MIN_ALPHA, 1.0) * attention;

        let selected = distance < 0.5;
        let icon_size = lerp(CATEGORY_ICON, CATEGORY_ICON_FOCUSED, focus) * scale;

        // Every category stands on its own tile of glass, sized to the icon
        // it holds, so the row reads as a rank of buttons rather than as
        // loose icons — and so the selected one is the same object, lit,
        // rather than a different kind of thing.
        //
        // Squircles, not discs. A circle is the one rounded shape with nothing
        // to line up: a rank of them reads as beads on a wire, and the icons
        // inside — which are square, as icons are — sit in them at an angle
        // nothing else on the screen agrees with. A superellipse keeps the
        // roundness and gets back the horizon.
        let disc = icon_size * CATEGORY_DISC;
        quads.push(Quad {
            x: x - disc / 2.0,
            y: cross_y - disc / 2.0,
            w: disc,
            h: disc,
            slot: SOLID_SLOT,
            color: theme.glass_raised.a(0.05),
            radius: disc / 2.0,
            corner: SQUIRCLE_CORNER,
            thickness: DEPTH_CONTROL * scale,
            frost: FROST_CONTROL,
            gloss: GLOSS_QUIET,
            fade: alpha * 0.85,
            ..Quad::default()
        });

        if selected {
            // Fades over the half-step where selection hands over, so during a
            // glide the glow and label cross-fade between neighbours instead
            // of blinking.
            let handover = (1.0 - distance * 2.0).clamp(0.0, 1.0);

            let glow = icon_size * (2.0 + 0.2 * pulse);
            quads.push(Quad {
                x: x - glow / 2.0,
                y: cross_y - glow / 2.0,
                w: glow,
                h: glow,
                slot: GLOW_SLOT,
                color: theme.accent.a((0.28 + 0.24 * pulse) * handover * attention),
                ..Quad::default()
            });
            quads.push(Quad {
                x: x - disc / 2.0,
                y: cross_y - disc / 2.0,
                w: disc,
                h: disc,
                slot: SOLID_SLOT,
                color: theme.accent.a(0.12),
                radius: disc / 2.0,
                corner: SQUIRCLE_CORNER,
                thickness: DEPTH_CONTROL * scale,
                frost: FROST_CONTROL,
                gloss: GLOSS_FULL,
                fade: handover * attention,
                ..Quad::default()
            });

            // The label sits *under* its button, centred, as on the original
            // bar — inside the widened gap, where no item text can reach it.
            //
            // Clear of the disc, not of the icon: the icon is a good deal
            // smaller than the glass it stands on, and measuring from it put
            // the label's head inside the button. Measured from the focused
            // size whatever this one currently is, so the label holds still
            // while the row glides underneath it rather than bobbing with the
            // button growing beneath.
            let label_size = CATEGORY_LABEL * scale;
            let box_w = category_spacing * 1.7;
            texts.push(Text {
                content: category.title.to_string(),
                x: x - box_w / 2.0,
                y: cross_y
                    + CATEGORY_ICON_FOCUSED * CATEGORY_DISC * scale / 2.0
                    + CATEGORY_LABEL_ABOVE * scale,
                size: label_size,
                color: theme.text.a(handover * attention),
                bold: true,
                max_width: box_w,
                align: TextAlign::Center,
            });
        }

        quads.push(icon_quad(
            slots.slot_for(Some(category.icon)),
            x - icon_size / 2.0,
            cross_y - icon_size / 2.0,
            icon_size,
            alpha,
            theme.accent_soft.a(alpha * 0.85),
        ));
    }

    Scene { quads, texts }
}

/// Vertical centre of the item at `offset` (in item units from the selection).
///
/// Offsets at or past 0 stack below the category row from `gap_below`;
/// offsets at or past −1 stack above it from `gap_above`. In between — an
/// entry mid-scroll — the position blends linearly, so the entry glides
/// through the row's gap instead of jumping over it.
fn item_y(offset: f32, cross_y: f32, gap_above: f32, gap_below: f32, spacing: f32) -> f32 {
    if offset >= 0.0 {
        cross_y + gap_below + offset * spacing
    } else if offset <= -1.0 {
        cross_y - gap_above + (offset + 1.0) * spacing
    } else {
        cross_y + gap_below + offset * (gap_below + gap_above)
    }
}

/// One card in the overview, as the shell needs it for drawing: the
/// compositor scales the live window itself, the shell only frames and
/// labels the spot. The start-screen card is the exception — nothing lives
/// under its slot, so the shell owns all of it.
pub struct Card {
    pub title: String,
    pub width: f32,
    pub height: f32,
    /// The trailing start-screen card: choosing it returns to the bar.
    pub start: bool,
    /// Where the card is *right now*: its aspect-fitted slot, eased by the
    /// caller at the same rate the compositor eases the window underneath.
    /// Cards mid-glide hang past the screen edges; drawing clips them.
    pub rect: [f32; 4],
}

/// The sidebar's header: the time, large, and the day beside it.
///
/// Two runs rather than one string, because they are not the same thing to
/// look at. The time is what the header is *for* — a glance at the menu should
/// answer it — and the date is context, at the size of everything else in the
/// column.
pub struct Clock<'a> {
    pub time: &'a str,
    pub date: &'a str,
}

/// Everything `build_guide` draws from.
pub struct GuideView<'a> {
    pub guide: &'a Guide,
    /// The wall clock, or `None` when local time cannot be worked out — the
    /// header then names the shell, which is better than a wrong clock.
    pub clock: Option<Clock<'a>>,
    /// Where the two quick-settings bars stand. `None` for a control this
    /// machine has not got, which is also how [`Guide::items`] knew to leave
    /// the row out — the two are set from the same place.
    pub volume: Option<Level>,
    pub brightness: Option<Level>,
    /// Whether the right stick is moving the pointer in the application in
    /// front of this display.
    pub stick_pointer: bool,
    /// The foreground application's title, for the sidebar's header.
    pub app: Option<&'a str>,
    /// Title of the window the Close entry would kill — the one whose card is
    /// selected. `None` when that is the start screen, which cannot be closed
    /// and so is why the entry is absent rather than merely unlabelled.
    pub close_target: Option<&'a str>,
    /// Name of the display the menu is on, when there is more than one.
    pub screen: Option<&'a str>,
    /// The windows the compositor is showing as cards, topmost first.
    pub cards: &'a [Card],
    /// Eased rectangle of the selected card, `[x, y, w, h]`.
    pub highlight: Option<[f32; 4]>,
    /// Eased rectangle of the selected menu entry's chip, in sidebar-local
    /// coordinates — see [`menu_item_rect`]. `None` snaps it to the selection.
    pub menu_highlight: Option<[f32; 4]>,
    /// How softly the wallpaper *behind* the overlay is being drawn, so the
    /// sidebar's glass bends the same wallpaper the layer below is showing
    /// rather than a sharp copy of it.
    pub behind: f32,
    /// How long the cards have been arriving — the compositor's clock, not the
    /// menu's. Everything drawn *on* the cards fades up on it, so nothing is
    /// painted over a window that is still on its way.
    pub card_age: f32,
    /// How far the power dialog is out of its button, 0 shut and 1 open. Not
    /// taken from the guide's own state, which flips the instant a key is
    /// pressed — this is the eased position, which is still finishing.
    pub power: f32,
    /// The global clock, for the selection pulse.
    pub time: f32,
    /// For the shell's own glyphs — the two on the quick-settings bars.
    pub slots: &'a dyn SlotLookup,
}

impl GuideView<'_> {
    /// Whether a tile's switch is on.
    fn tile_on(&self, item: Item) -> bool {
        match item {
            Item::Pointer => self.stick_pointer && self.tile_live(item),
            // Nothing to be on yet: the mixer is a place kept in the column
            // for a control that has still to be written.
            _ => false,
        }
    }

    /// Whether a tile can do anything from where the user is standing.
    ///
    /// The same answer the highlight uses to decide whether to stop on it, and
    /// deliberately the same answer: a control drawn as available that the
    /// selection then skips over is worse than either failure on its own.
    fn tile_live(&self, item: Item) -> bool {
        self.guide.is_enabled(item)
    }
}

/// How long the cards take to fade up once their windows have stopped moving.
///
/// Short, and it starts the moment they land: the decoration is the cards
/// settling into place, not an animation of its own to be watched.
const CARD_FADE: f32 = 0.14;

/// How far the guide's cards have arrived, `age` seconds after the compositor
/// was told to start flying their windows.
///
/// Nothing at all until they have landed, and this is the whole reason the
/// answer is a function of the flight's clock rather than the menu's. Every
/// mark this pass makes on a card — the frame, the title, the selection, and
/// the start screen's own miniature — is drawn at the rectangle the layout
/// says the card *will* occupy, on a surface stacked above the windows the
/// compositor is still carrying there. Drawn a frame early, a frame is a
/// hairline ruled across the middle of a window that is still crossing the
/// display, and the start screen is a whole display of opaque content laid
/// over the very application the user is leaving.
///
/// So the shell waits out the flight it does not perform, and only then does
/// anything appear. Late is free; early is the bug.
pub fn card_fade(age: f32) -> f32 {
    ease((age - crate::CARD_ARRIVAL) / CARD_FADE)
}

/// Lay out the guide overlay: a menu column sliding in from the left, and
/// frames and titles for the window cards beside it.
///
/// The cards themselves are live windows, drawn by the compositor *under*
/// this surface at exactly the rectangles the shared layout dictates —
/// everything here except the sidebar is painted onto transparency around
/// them.
pub fn build_guide(view: GuideView, width: f32, height: f32) -> Scene {
    let mut quads = Vec::new();
    let mut texts = Vec::new();

    let scale = guide_scale(height);
    // The start-screen card means the deck is never empty, so the cards say
    // nothing about whether an application is running — only the app does.
    // Whether anything can be closed is a different question again: it is the
    // card under the cursor, which may be the start screen.
    let closable = view.close_target.is_some();
    let items = view.guide.items(closable);
    let selected = view.guide.selected_index(closable);
    let pane = view.guide.pane();
    let pulse = 0.5 + 0.5 * (view.time * std::f32::consts::TAU / PULSE_PERIOD).sin();

    // The entrance: the sidebar slides in decelerating while it fades up, and
    // the card decorations hold back until the windows have landed.
    let age = view.guide.age();
    let slide = ease(age / GUIDE_SLIDE);
    let card_fade = card_fade(view.card_age);

    let theme = theme();
    let sidebar_w = overview::sidebar_width(width as f64) as f32;
    let sidebar_x = (slide - 1.0) * sidebar_w;
    let [panel_x, panel_y, panel_w, panel_h] = sidebar_panel_rect(width, height);
    let padding = GUIDE_PADDING * scale;
    let text_x = panel_x + padding * 0.8;
    let text_w = panel_w - padding * 1.6;

    // --- the sidebar -------------------------------------------------------
    // One slab of glass, floating clear of the display's edges, bending the
    // wallpaper it is laid over at its rim. Everything else in the column is
    // an object resting on it.
    quads.push(Quad {
        x: sidebar_x + panel_x,
        y: panel_y,
        w: panel_w,
        h: panel_h,
        slot: SOLID_SLOT,
        color: theme.glass.a(0.5),
        radius: PANEL_RADIUS * scale,
        thickness: DEPTH_PANEL * scale,
        behind: view.behind,
        frost: FROST_PANEL,
        gloss: GLOSS_FULL,
        fade: slide,
        ..Quad::default()
    });

    let title_size = 34.0 * scale;
    let subtitle_size = 18.0 * scale;
    texts.push(Text {
        content: match &view.clock {
            Some(clock) => clock.time.to_string(),
            None => "Linboard".to_string(),
        },
        x: sidebar_x + text_x,
        y: 52.0 * scale,
        size: title_size,
        color: theme.text.a(0.95 * slide),
        bold: true,
        max_width: text_w,
        align: TextAlign::Left,
    });
    // The day, on the clock's own line and pushed to the far side of the
    // column. Sharing the line keeps the header two rows tall — the sidebar is
    // narrow, and a fourth stacked line of the same left-aligned text would
    // read as a list rather than as a heading. Dropped by the difference in
    // the two sizes so both sit on one baseline instead of one hanging.
    if let Some(clock) = &view.clock {
        texts.push(Text {
            content: clock.date.to_string(),
            x: sidebar_x + text_x,
            y: 52.0 * scale + (title_size - subtitle_size) * 0.72,
            size: subtitle_size,
            color: theme.text_soft.a(0.7 * slide),
            bold: false,
            max_width: text_w,
            align: TextAlign::Right,
        });
    }
    texts.push(Text {
        content: match view.app {
            Some(app) => app.to_string(),
            None => "Nothing is running".to_string(),
        },
        x: sidebar_x + text_x,
        y: 52.0 * scale + title_size * 1.35,
        size: subtitle_size,
        color: theme.text_soft.a(0.85 * slide),
        bold: false,
        max_width: text_w,
        align: TextAlign::Left,
    });
    if let Some(screen) = view.screen {
        texts.push(Text {
            content: format!("Screen {screen}"),
            x: sidebar_x + text_x,
            y: 52.0 * scale + title_size * 1.35 + subtitle_size * 1.45,
            size: subtitle_size,
            color: theme.text_soft.a(0.6 * slide),
            bold: false,
            max_width: text_w,
            align: TextAlign::Left,
        });
    }

    // The entry column. Each entry is a rounded chip so it reads as something
    // that can be pressed, and they arrive one after another rather than all
    // at once. While focus is over on the cards the selection stays but drops
    // to a murmur, so the user always knows where Left leads back to.
    let pane_focus = if pane == Pane::Menu { 1.0 } else { 0.35 };

    // The selection is one capsule that glides between rows, eased by the
    // caller. Falling back to the row itself keeps the first frame honest.
    let [hx, hy, hw, hh] = view
        .menu_highlight
        .unwrap_or_else(|| menu_item_rect(&items, selected, width, height));
    let glow_h = hh * 2.6;
    quads.push(Quad {
        x: sidebar_x + hx + hw * 0.5 - sidebar_w * 0.55,
        y: hy + hh * 0.5 - glow_h * 0.5,
        w: sidebar_w * 1.1,
        h: glow_h,
        slot: GLOW_SLOT,
        color: theme.accent.a((0.13 + 0.05 * pulse) * pane_focus * slide),
        ..Quad::default()
    });
    // The selected capsule is lit glass rather than a filled shape: same
    // material as the buttons under it, tilted towards the accent and
    // catching more light.
    //
    // It goes down with the entry under it when that entry is pressed. It has
    // to: it is drawn *over* the chip, so a press that sank only what was
    // underneath would happen entirely behind the one thing the user is
    // looking at. The rectangle it is measured from is left alone — where the
    // light has got to is a different question from how far the switch is
    // down, and the rows below decide whether to keep their own chips from
    // the first.
    let selected_press = items
        .get(selected)
        .and_then(|item| view.guide.press_progress(*item));
    let [lx, ly, lw, lh] =
        scaled_about_centre([hx, hy, hw, hh], selected_press.map_or(1.0, press_scale));
    quads.push(Quad {
        x: sidebar_x + lx,
        y: ly,
        w: lw,
        h: lh,
        slot: SOLID_SLOT,
        color: theme.accent.a(0.46 + 0.05 * pulse),
        radius: chip_radius(items.get(selected).copied(), lh),
        thickness: DEPTH_CONTROL * scale,
        behind: view.behind,
        frost: FROST_CONTROL,
        gloss: GLOSS_FULL,
        fade: pane_focus * slide,
        ..Quad::default()
    });

    // The rules between the bands of the column: what the session sounds and
    // looks like, what the menu does to the application in front of it, what
    // it does to the session. Barely there on purpose — they are groupings,
    // not borders.
    for [sx, sy, sw, sh] in menu_separator_rects(&items, width, height) {
        quads.push(Quad {
            x: sidebar_x + sx,
            y: sy,
            w: sw,
            h: sh,
            slot: SOLID_SLOT,
            color: theme.accent_soft.a(0.16 * slide),
            ..Quad::default()
        });
    }

    for (index, item) in items.iter().enumerate() {
        let [rx, ry, rw, rh] = menu_item_rect(&items, index, width, height);
        let focused = index == selected;
        // Entries arrive in turn, each sliding the last of its own distance.
        let appear = entry_appear(age, index);
        let drift = (1.0 - appear) * 26.0 * scale;

        // Every entry is a capsule of frosted glass resting on the panel: a
        // rank of buttons, not a list with one of them coloured in. The
        // selected one is the same capsule, already drawn above, lit — but it
        // only gives its own up once that lit capsule has arrived over it.
        // Handing it over the instant the selection changed left the row a
        // hole in the column for the length of the glide, with the light
        // still crossing the gap to fill it.
        let handed_over = if focused {
            highlight_arrival([hx, hy, hw, hh], [rx, ry, rw, rh]) * pane_focus
        } else {
            0.0
        };
        // A tile being pressed goes down, chip and all. Everything else in the
        // column either closes the menu or opens a dialog when it is chosen,
        // so there is nothing left on screen for a press to be seen on.
        let press = view.guide.press_progress(*item);
        let chip = scaled_about_centre(
            [sidebar_x + rx + drift, ry, rw, rh],
            press.map_or(1.0, press_scale),
        );
        // A tile that cannot be reached is not given a chip at all: it is
        // outlined where its chip would be.
        //
        // Dimming one was not enough, and could not have been. Every chip in
        // the column is a slab of glass over a dark panel, so a dimmer one is
        // a chip in slightly less light — which is what a chip *behind the
        // selection* also looks like, and there are five of those on screen at
        // the time. A hairline is a different kind of thing rather than a
        // quieter one: nothing has been put here to press, and the shape says
        // where the control will be when there is.
        if item.is_tile() && !view.tile_live(*item) {
            quads.push(Quad {
                x: chip[0],
                y: chip[1],
                w: chip[2],
                h: chip[3],
                slot: SOLID_SLOT,
                color: theme.text_soft.a(0.22),
                radius: chip_radius(Some(*item), chip[3]),
                border: (1.5 * scale).max(1.0),
                fade: appear * slide,
                ..Quad::default()
            });
        } else {
            quads.push(Quad {
                x: chip[0],
                y: chip[1],
                w: chip[2],
                h: chip[3],
                slot: SOLID_SLOT,
                color: theme.glass_raised.a(0.10),
                radius: chip_radius(Some(*item), chip[3]),
                thickness: DEPTH_CONTROL * scale,
                behind: view.behind,
                frost: FROST_CONTROL,
                gloss: GLOSS_QUIET,
                fade: appear * slide * (1.0 - handed_over),
                ..Quad::default()
            });
        }

        if item.is_tile() {
            quads.extend(tile(
                [sidebar_x + rx + drift, ry, rw, rh],
                *item,
                TileState {
                    on: view.tile_on(*item),
                    live: view.tile_live(*item),
                    focused,
                    press,
                },
                scale,
                view.behind,
                appear * slide,
                view.slots,
            ));
            continue;
        }

        if *item == Item::Power {
            // No label: the glyph is the whole button, as it is on the panel
            // of every desktop this borrows from. Which is also why this is
            // the one control that falls back to drawing itself out of quads
            // rather than leaving the chip empty — an empty disc here says
            // nothing at all, where an empty quick-settings tile at least
            // still has the bar beside it.
            let glyph = rh * POWER_GLYPH;
            let lit = (if focused { 1.0 } else { 0.75 }) * appear * slide;
            match view.slots.glyph(icons::SHUTDOWN) {
                Some(slot) => {
                    // The drawing fills `SHUTDOWN_INK` of its cell and is
                    // centred in it, so the cell that gives the symbol the
                    // height this button wants is that height divided back
                    // out again.
                    let cell = glyph * 2.32 / SHUTDOWN_INK;
                    quads.push(Quad {
                        x: sidebar_x + rx + (rw - cell) * 0.5 + drift,
                        y: ry + (rh - cell) * 0.5,
                        w: cell,
                        h: cell,
                        slot,
                        color: [1.0, 1.0, 1.0, lit],
                        ..Quad::default()
                    });
                }
                None => quads.extend(power_glyph(
                    sidebar_x + rx + rw * 0.5 + drift,
                    // The stroke rises above the ring, so the symbol's own
                    // middle sits above the ring's centre; dropping the ring
                    // by half that overhang is what centres the *symbol* in
                    // its button.
                    ry + rh * 0.5 + glyph * 0.16,
                    glyph,
                    scale,
                    theme.text.a(lit),
                )),
            }
            continue;
        }

        if let Some(bar) = item.bar() {
            let level = match bar {
                Bar::Volume => view.volume,
                Bar::Brightness => view.brightness,
            };
            // The row exists because the control does — the same answer put
            // both here — so a missing level is a control that went away
            // between the column being built and this frame being drawn.
            if let Some(level) = level {
                quads.extend(quick_bar(
                    [sidebar_x + rx + drift, ry, rw, rh],
                    bar,
                    level,
                    scale,
                    (if focused { 1.0 } else { 0.82 }) * appear * slide,
                    view.slots,
                ));
            }
            continue;
        }

        let label_size = 24.0 * scale;
        texts.push(Text {
            content: item.label(view.close_target),
            x: sidebar_x + rx + 24.0 * scale + drift,
            // Centred in its chip now that no description shares the row.
            y: ry + rh * 0.5 - label_size * 0.62,
            size: label_size,
            color: theme
                .text
                .a(if focused { 1.0 } else { 0.78 } * appear * slide),
            bold: focused,
            max_width: text_w,
            align: TextAlign::Left,
        });
    }

    // --- the cards -----------------------------------------------------
    // The compositor draws the windows; this pass frames the spots so empty
    // bezels never outline nothing, and labels each with its title. The
    // start-screen card has no window under it: the shell draws its own bar
    // as a miniature there, and this pass frames and labels that the same
    // way as the rest.
    // Each card's rectangle arrives pre-eased in `card.rect`, so a scroll
    // slides the frames in step with the windows gliding underneath.
    let selected_index = view.guide.selected_window(view.cards.len());
    for (index, card) in view.cards.iter().enumerate() {
        let [x, y, w, h] = card.rect;
        if w <= 0.0 || h <= 0.0 || y >= height || y + h <= 0.0 {
            continue; // fully past the screen's edges
        }
        let selected_card = pane == Pane::Windows && index == selected_index;

        quads.push(Quad {
            x,
            y,
            w,
            h,
            slot: SOLID_SLOT,
            color: theme.rim.a(0.16 * card_fade),
            radius: CARD_RADIUS * scale,
            border: 1.5 * scale,
            ..Quad::default()
        });

        let label_size = 18.0 * scale;
        texts.push(Text {
            content: card.title.clone(),
            x,
            y: y + h + 10.0 * scale,
            size: label_size,
            color: theme
                .text
                .a(if selected_card { 0.95 } else { 0.55 } * card_fade),
            bold: selected_card,
            max_width: w.max(120.0 * scale),
            align: TextAlign::Left,
        });
    }

    // The selection frame sits on the card it belongs to from the first frame
    // of a scroll — the caller hands over that card's own rectangle, which is
    // already gliding, so the frame travels *with* it rather than chasing it.
    if let (Some([x, y, w, h]), false) = (view.highlight, view.cards.is_empty()) {
        let focus = if pane == Pane::Windows { 1.0 } else { 0.30 };
        let gap = 5.0 * scale;

        // Outside the frame, a halo that breathes. Concentric rings rather
        // than one soft sprite: a glow centred on the card would wash out the
        // live window inside it, and what should read as lit is the edge.
        for ring in 1..=HALO_RINGS {
            let spread = gap + ring as f32 * HALO_STEP * scale;
            let falloff = 1.0 - (ring as f32 - 1.0) / HALO_RINGS as f32;
            quads.push(Quad {
                x: x - spread,
                y: y - spread,
                w: w + spread * 2.0,
                h: h + spread * 2.0,
                slot: SOLID_SLOT,
                color: theme.accent_soft.a(0.17
                    * falloff
                    * falloff
                    * (0.45 + 0.55 * pulse)
                    * focus
                    * card_fade),
                radius: CARD_RADIUS * scale + spread,
                border: HALO_STEP * scale,
                ..Quad::default()
            });
        }

        quads.push(Quad {
            x: x - gap,
            y: y - gap,
            w: w + gap * 2.0,
            h: h + gap * 2.0,
            slot: SOLID_SLOT,
            color: theme.accent.a(0.92 * focus * card_fade),
            radius: CARD_RADIUS * scale + gap,
            border: 3.5 * scale,
            ..Quad::default()
        });
        quads.push(Quad {
            x: x - scale,
            y: y - scale,
            w: w + 2.0 * scale,
            h: h + 2.0 * scale,
            slot: SOLID_SLOT,
            color: theme.rim.a(0.34 * focus * card_fade),
            radius: CARD_RADIUS * scale + scale,
            border: scale,
            ..Quad::default()
        });
    }

    let mut scene = Scene { quads, texts };
    // Drawn on the eased position, not on whether it is open: a dismissed
    // dialog still has to fall back into the button it came out of.
    if view.power > 0.0 {
        push_power_dialog(&mut scene, &view, width, height, scale, pulse);
    }
    scene
}

/// Where the power dialog's panel sits: centred on the display.
///
/// Public because everything drawn before it has to know — a scene is all its
/// quads and then all its text, so the panel cannot cover a label that was
/// added earlier, however far in front it is meant to be.
pub fn power_dialog_rect(width: f32, height: f32, rows: usize) -> [f32; 4] {
    let scale = guide_scale(height);
    let panel_w = (DIALOG_WIDTH * scale).min(width * 0.8);
    let panel_h = (78.0 + 14.0) * scale + DIALOG_ROW * scale * rows as f32;
    [
        (width - panel_w) * 0.5,
        (height - panel_h) * 0.5,
        panel_w,
        panel_h,
    ]
}

/// Where the dialog is when it is `progress` of the way out of its button.
///
/// It leaves as one shape rather than growing into its proportions: a single
/// factor carried on a travelling centre, from the button's width to its own,
/// the way a folder opens on a phone. Keeping the panel's aspect the whole way
/// is what lets the contents ride out on the same factor — see
/// [`Scene::scale_by`].
pub fn power_dialog_bounds(width: f32, height: f32, rows: usize, progress: f32) -> [f32; 4] {
    let [px, py, pw, ph] = power_dialog_rect(width, height, rows);
    let [bx, by, bw, bh] = power_button_rect(width, height);
    if pw <= 0.0 {
        return [px, py, pw, ph];
    }
    let factor = lerp(bw / pw, 1.0, progress);
    let cx = lerp(bx + bw * 0.5, px + pw * 0.5, progress);
    let cy = lerp(by + bh * 0.5, py + ph * 0.5, progress);
    [
        cx - pw * factor * 0.5,
        cy - ph * factor * 0.5,
        pw * factor,
        ph * factor,
    ]
}

/// Push a scene behind the dialog: dim it, and drop the text the panel would
/// otherwise print through.
///
/// Called for each scene drawn before the modal — the guide's own, and the
/// bar the start-screen card is a miniature of, which is assembled separately
/// and whose labels were the ones showing through the panel.
///
/// Both halves follow the dialog out of its button. The text goes as the panel
/// reaches it rather than the moment the button is pressed, which is the
/// difference between labels the panel swallows and labels that vanish a
/// fifth of a second before anything covers them.
pub fn recede_behind_dialog(
    scene: &mut Scene,
    width: f32,
    height: f32,
    rows: usize,
    progress: f32,
) {
    scene.fade(lerp(1.0, DIALOG_DIM, progress));
    scene.hide_text_behind(power_dialog_bounds(width, height, rows, progress));
}

/// Put the power dialog in front of everything else on the display.
fn push_power_dialog(
    scene: &mut Scene,
    view: &GuideView,
    width: f32,
    height: f32,
    scale: f32,
    pulse: f32,
) {
    let items = view.guide.power_items();
    let open = view.power.clamp(0.0, 1.0);
    recede_behind_dialog(scene, width, height, items.len(), open);

    // The scrim: dims the compositor's cards showing through this surface.
    scene.quads.push(Quad {
        x: 0.0,
        y: 0.0,
        w: width,
        h: height,
        slot: SOLID_SLOT,
        color: theme().glass.a(0.78 * open),
        ..Quad::default()
    });

    let theme = theme();
    let selected = view.guide.power_index();
    let [panel_x, panel_y, panel_w, _] = power_dialog_rect(width, height, items.len());
    let [grown_x, grown_y, grown_w, grown_h] =
        power_dialog_bounds(width, height, items.len(), open);
    let row_h = DIALOG_ROW * scale;
    let title_h = 78.0 * scale;

    // Glass, and it can be: what it refracts is read out of the frame rather
    // than guessed at, so over an application it bends the scrim just drawn
    // across that application instead of an invented wallpaper. Deeply
    // frosted, because it is the one surface in the shell that has to be
    // legible over literally anything.
    scene.quads.push(Quad {
        x: grown_x,
        y: grown_y,
        w: grown_w,
        h: grown_h,
        slot: SOLID_SLOT,
        color: theme.glass.a(0.94),
        // The one thing not carried by the growth: a panel shrunk to button
        // size would have button-sized *corners*, near enough square, when
        // what should leave the button is the button's own round shape
        // squaring off as it opens out.
        radius: lerp(
            power_button_rect(width, height)[3] * 0.5,
            PANEL_RADIUS * scale,
            open,
        ),
        thickness: DEPTH_PANEL * scale * open,
        frost: FROST_PANEL,
        gloss: GLOSS_FULL,
        // Over the button, which is still lit underneath it.
        fade: ease(open / 0.25),
        ..Quad::default()
    });

    // The contents are laid out at the size they settle at and then carried
    // out of the button whole, so nothing inside the panel moves relative to
    // anything else on the way.
    let mut inside = Scene::default();
    let title_size = 26.0 * scale;
    inside.texts.push(Text {
        content: "Power".to_string(),
        x: panel_x,
        y: panel_y + (title_h - title_size) * 0.5,
        size: title_size,
        color: theme.text.a(0.95),
        bold: true,
        max_width: panel_w,
        align: TextAlign::Center,
    });

    let label_size = 22.0 * scale;
    for (index, item) in items.iter().enumerate() {
        let y = panel_y + title_h + index as f32 * row_h;
        let focused = index == selected;
        let inset = 12.0 * scale;
        let capsule = row_h - 8.0 * scale;
        // Warm for the two choices there is no coming back from, so the
        // difference is visible before the label is read.
        let tint = if item.is_grave() {
            theme.danger
        } else {
            theme.accent
        };
        inside.quads.push(Quad {
            x: panel_x + inset,
            y: y + 4.0 * scale,
            w: panel_w - inset * 2.0,
            h: capsule,
            slot: SOLID_SLOT,
            color: if focused {
                tint.a(0.68 + 0.08 * pulse)
            } else {
                theme.glass_raised.a(0.09)
            },
            radius: capsule * 0.5,
            thickness: DEPTH_CONTROL * scale,
            frost: FROST_CONTROL,
            gloss: if focused { GLOSS_FULL } else { GLOSS_QUIET },
            ..Quad::default()
        });
        inside.texts.push(Text {
            content: item.label().to_string(),
            x: panel_x,
            y: y + (row_h - label_size) * 0.5 - label_size * 0.12,
            size: label_size,
            color: theme.text.a(if focused { 1.0 } else { 0.78 }),
            bold: focused,
            max_width: panel_w,
            align: TextAlign::Center,
        });
    }

    // Onto the panel wherever it currently is. The factor is the panel's own,
    // so the contents cannot drift out of it, and they hold back until it is
    // big enough to read them in.
    if panel_w > 0.0 {
        let factor = grown_w / panel_w;
        inside.scale_by(
            factor,
            [grown_x - panel_x * factor, grown_y - panel_y * factor],
        );
        inside.fade(ease((open - DIALOG_CONTENT_IN) / (1.0 - DIALOG_CONTENT_IN)));
    }
    scene.quads.extend(inside.quads);
    scene.texts.extend(inside.texts);
}

// --- the on-screen keyboard ------------------------------------------------

/// One grid column of the board, and one row, at the reference height. A
/// column is a little wider than a row is tall — which is what a keycap is.
const KEY_UNIT: f32 = 62.0;
const KEY_HEIGHT: f32 = 56.0;
/// Air between neighbouring keys, taken out of each key rather than added
/// between them, so the grid arithmetic stays in whole columns.
const KEY_GAP: f32 = 8.0;
const KEY_RADIUS: f32 = 13.0;
/// What is printed on a function key, which is set smaller than the rest: the
/// row is half height, and `F11` at the size of `Enter` would fill its cap.
const KEY_CAP_FUNCTION: f32 = 14.0;
const BOARD_PADDING: f32 = 20.0;
/// How far the board floats clear of the display's bottom edge.
const BOARD_MARGIN: f32 = 34.0;
/// How long it takes to rise into place from below that edge.
const BOARD_SLIDE: f32 = 0.24;

/// The scale the board is drawn at.
///
/// The guide's, unless the grid would not fit across the display — a narrow or
/// rotated screen shrinks the whole board rather than letting it run off both
/// sides, because a keyboard missing its outer columns is missing letters.
fn board_scale(width: f32, height: f32) -> f32 {
    let scale = guide_scale(height);
    let natural = (KEY_UNIT * keyboard::COLUMNS + (BOARD_PADDING + BOARD_MARGIN) * 2.0) * scale;
    if width > 0.0 && natural > width {
        scale * (width / natural)
    } else {
        scale
    }
}

/// How far down the keys a row starts, and how tall it is, at the reference
/// height.
///
/// Not `row * (KEY_HEIGHT + KEY_GAP)`: the function row is a half-height strip
/// (see [`keyboard::row_scale`]), so everything below it sits higher than a
/// uniform grid would put it.
fn row_band(row: usize) -> (f32, f32) {
    let top = (0..row)
        .map(|above| keyboard::row_scale(above) * KEY_HEIGHT + KEY_GAP)
        .sum();
    (top, keyboard::row_scale(row) * KEY_HEIGHT)
}

/// How tall all the keys together are.
fn keys_height() -> f32 {
    let (top, height) = row_band(keyboard::ROW_COUNT - 1);
    top + height
}

/// Where the board's panel sits: centred, along the foot of the display.
pub fn keyboard_panel_rect(width: f32, height: f32) -> [f32; 4] {
    let scale = board_scale(width, height);
    let w = (KEY_UNIT * keyboard::COLUMNS + BOARD_PADDING * 2.0) * scale;
    let h = (keys_height() + BOARD_PADDING * 2.0) * scale;
    [(width - w) * 0.5, height - BOARD_MARGIN * scale - h, w, h]
}

/// Where one key sits, in display coordinates.
pub fn keyboard_key_rect(row: usize, column: usize, width: f32, height: f32) -> [f32; 4] {
    let scale = board_scale(width, height);
    let [panel_x, panel_y, _, _] = keyboard_panel_rect(width, height);
    let unit = KEY_UNIT * scale;
    let gap = KEY_GAP * scale;
    let padding = BOARD_PADDING * scale;
    let (start, span) = keyboard::row_layout(row)
        .get(column)
        .copied()
        .unwrap_or((0.0, 1.0));
    let (top, tall) = row_band(row);
    [
        panel_x + padding + start * unit + gap * 0.5,
        panel_y + padding + top * scale,
        span * unit - gap,
        tall * scale,
    ]
}

/// Which key is under a point, in display coordinates.
///
/// The inverse of [`keyboard_key_rect`], done by walking the same rectangles
/// rather than by arithmetic: the rows are not the same height and their keys
/// are not the same width, so a formula here would be a second layout, and a
/// second layout is a way for the key the user pressed to differ from the key
/// they were looking at.
///
/// Points in the panel that are not on any key — the padding at the rim, the
/// gaps between keys — are nothing rather than the nearest key. Half of
/// clicking accurately is being able to miss.
pub fn keyboard_key_at(x: f32, y: f32, width: f32, height: f32) -> Option<(usize, usize)> {
    for row in 0..keyboard::ROW_COUNT {
        for column in 0..keyboard::row_keys(row).len() {
            let [kx, ky, kw, kh] = keyboard_key_rect(row, column, width, height);
            if x >= kx && x < kx + kw && y >= ky && y < ky + kh {
                return Some((row, column));
            }
        }
    }
    None
}

/// Everything [`build_keyboard`] draws from.
pub struct KeyboardView<'a> {
    pub board: &'a keyboard::Board,
    /// For the arrow caps, which are drawings rather than characters — see
    /// [`keyboard::Arrow`].
    pub slots: &'a dyn SlotLookup,
    /// Seconds since it appeared, for the rise from below the screen's edge.
    pub age: f32,
    /// How softly the wallpaper behind the board is drawn. Nearly always 0:
    /// the keyboard is over an application, and what is behind it is that
    /// application, drawn by the compositor at full sharpness.
    pub behind: f32,
    /// The global clock, for the selection pulse.
    pub time: f32,
}

/// Lay out the on-screen keyboard.
///
/// A slab of the same glass the guide's sidebar is cut from, with a keycap for
/// every key resting on it. The selected key is one lit capsule, drawn before
/// the caps so the letter on it stays legible.
pub fn build_keyboard(view: KeyboardView, width: f32, height: f32) -> Scene {
    let mut quads = Vec::new();
    let mut texts = Vec::new();
    let theme = theme();
    let scale = board_scale(width, height);
    let pulse = 0.5 + 0.5 * (view.time * std::f32::consts::TAU / PULSE_PERIOD).sin();

    // It rises from under the display's edge rather than fading in. A keyboard
    // that appeared on the spot over a running application reads as the
    // application having done something; one that slides up reads as the shell
    // putting it there.
    let arrived = ease(view.age / BOARD_SLIDE);
    let [panel_x, panel_y, panel_w, panel_h] = keyboard_panel_rect(width, height);
    let lift = (1.0 - arrived) * (panel_h + BOARD_MARGIN * scale);

    quads.push(Quad {
        x: panel_x,
        y: panel_y + lift,
        w: panel_w,
        h: panel_h,
        slot: SOLID_SLOT,
        color: theme.glass.a(0.52),
        radius: PANEL_RADIUS * scale,
        thickness: DEPTH_PANEL * scale,
        behind: view.behind,
        frost: FROST_PANEL,
        gloss: GLOSS_FULL,
        ..Quad::default()
    });

    let (selected_row, selected_column) = view.board.selected();
    let shifted = view.board.shifted();

    // The selection first, so that the caps — text, and every text run in a
    // scene is drawn after every quad anyway — are never fighting it.
    let [hx, hy, hw, hh] = keyboard_key_rect(selected_row, selected_column, width, height);
    let glow = hh * 2.2;
    quads.push(Quad {
        x: hx + hw * 0.5 - glow * 0.5,
        y: hy + lift + hh * 0.5 - glow * 0.5,
        w: glow,
        h: glow,
        slot: GLOW_SLOT,
        color: theme.accent.a(0.30 + 0.08 * pulse),
        ..Quad::default()
    });
    quads.push(Quad {
        x: hx,
        y: hy + lift,
        w: hw,
        h: hh,
        slot: SOLID_SLOT,
        color: theme.accent.a(0.52 + 0.05 * pulse),
        radius: KEY_RADIUS * scale,
        corner: SQUIRCLE_CORNER,
        thickness: DEPTH_CONTROL * scale,
        behind: view.behind,
        frost: FROST_CONTROL,
        gloss: GLOSS_FULL,
        ..Quad::default()
    });

    for row in 0..keyboard::ROW_COUNT {
        for (column, key) in keyboard::row_keys(row).into_iter().enumerate() {
            let [x, y, w, h] = keyboard_key_rect(row, column, width, height);
            let y = y + lift;
            let focused = (row, column) == (selected_row, selected_column);
            // Shift, Caps, Ctrl and Alt stay lit after they are left: they
            // have changed what the next press will do, and the board has to
            // say so.
            let held = view.board.latched(key).is_on();
            let locked = view.board.locked(key);

            if !focused {
                quads.push(Quad {
                    x,
                    y,
                    w,
                    h,
                    slot: SOLID_SLOT,
                    color: if held {
                        // Held down brighter than armed for one letter: the
                        // two states change the whole board's caps, and mean
                        // different things about the next press.
                        theme.accent.a(if locked { 0.50 } else { 0.32 })
                    } else if matches!(key, keyboard::Key::Char(..)) {
                        theme.glass_raised.a(0.10)
                    } else {
                        // Everything that is not a letter — the modifiers, the
                        // function row, the arrows — sits a shade darker, so
                        // the block a word is typed from reads as one thing.
                        theme.glass_raised.a(0.17)
                    },
                    radius: KEY_RADIUS * scale,
                    corner: SQUIRCLE_CORNER,
                    thickness: DEPTH_CONTROL * scale,
                    behind: view.behind,
                    frost: FROST_CONTROL,
                    gloss: GLOSS_QUIET,
                    ..Quad::default()
                });
            }

            // Close is outlined so it can be found without reading the row.
            // It is the way out of a keyboard that appeared on its own, which
            // is the one thing a user who did not summon it will be looking
            // for.
            if key.is_close() {
                quads.push(Quad {
                    x,
                    y,
                    w,
                    h,
                    slot: SOLID_SLOT,
                    color: theme.accent_soft.a(if focused { 0.55 } else { 0.34 }),
                    radius: KEY_RADIUS * scale,
                    corner: SQUIRCLE_CORNER,
                    border: 1.5 * scale,
                    ..Quad::default()
                });
            }

            // Some keys are drawn rather than lettered: the arrows, because
            // the bundled font has no arrow glyphs and a cap reading "Left" is
            // not an arrow key, and the way out, because a keyboard folding
            // away is read at a distance that a word is not.
            if let Some(name) = key.glyph() {
                if let Some(slot) = view.slots.glyph(name) {
                    let mark = h * 0.46;
                    quads.push(Quad {
                        x: x + w * 0.5 - mark * 0.5,
                        y: y + h * 0.5 - mark * 0.5,
                        w: mark,
                        h: mark,
                        slot,
                        color: theme.text.a(if focused { 1.0 } else { 0.82 }),
                        ..Quad::default()
                    });
                }
                continue;
            }

            // A letter is set larger than a word: the caps are read at a
            // glance while the cursor moves, and "Backspace" at the size of
            // "g" would be a smear. The function row is smaller again, being
            // half the height of the rest and none of the reason the board is
            // on screen.
            let label = key.cap(shifted);
            let size = if row == 0 {
                KEY_CAP_FUNCTION * scale
            } else if matches!(key, keyboard::Key::Char(..)) {
                26.0 * scale
            } else {
                17.0 * scale
            };
            texts.push(Text {
                content: label,
                x,
                y: y + h * 0.5 - size * 0.66,
                size,
                color: theme.text.a(if focused || held { 1.0 } else { 0.82 }),
                bold: focused,
                max_width: w,
                align: TextAlign::Center,
            });
        }
    }

    // No fade to go with the rise: a board that faded up would arrive as a
    // ghost over the application. It comes up solid, simply from further down,
    // and until it has cleared the edge there is nothing of it on screen to
    // see.
    Scene { quads, texts }
}

/// The keyboard hint's glyphs and label, and the air around them.
const HINT_GLYPH: f32 = 27.0;
const HINT_LABEL: f32 = 19.0;
const HINT_PADDING: f32 = 15.0;
const HINT_GAP: f32 = 8.0;
const HINT_MARGIN: f32 = 26.0;
/// Roughly how wide one character of the label is, as a share of its size.
/// The shell cannot measure a text run before the GPU shapes it, and the chip
/// behind the run has to be sized now; the estimate is generous, so the label
/// sits in the chip rather than against its end.
const HINT_ADVANCE: f32 = 0.58;
/// What the hint says. Two glyphs and one word: it is a reminder for someone
/// holding the controller, not documentation.
const HINT_LABEL_TEXT: &str = "Keyboard";

/// Where the hint chip sits: the bottom-right corner, out of the way of
/// anything an application is likely to have put in the middle.
pub fn keyboard_hint_rect(width: f32, height: f32) -> [f32; 4] {
    let scale = guide_scale(height);
    let glyph = HINT_GLYPH * scale;
    let label = HINT_LABEL * scale;
    let w = HINT_PADDING * 2.0 * scale
        + glyph * 2.0
        + HINT_GAP * 3.0 * scale
        // The "+" between the two buttons, at the label's size.
        + label * 0.6
        + HINT_LABEL_TEXT.chars().count() as f32 * label * HINT_ADVANCE;
    let h = glyph + HINT_PADDING * 1.5 * scale;
    let margin = HINT_MARGIN * scale;
    [width - margin - w, height - margin - h, w, h]
}

/// Everything [`build_keyboard_hint`] draws from.
pub struct HintView<'a> {
    /// For the two controller glyphs.
    pub slots: &'a dyn SlotLookup,
    /// How far it has faded up, 0 to 1.
    pub fade: f32,
    pub behind: f32,
}

/// The corner chip that says how to summon the keyboard.
///
/// It names the two buttons by drawing them rather than by lettering them.
/// "Press X" is wrong on a PlayStation pad, which has no X, and worse than
/// wrong on a Nintendo one, where X is the button *above* the one meant — the
/// letters are swapped between the two most common layouts. "Press Select" is
/// no better: the same button is Back, View, Share, Create or a minus sign
/// depending on whose pad it is. A picture of the cluster with one button
/// filled in is true on all of them.
pub fn build_keyboard_hint(view: HintView, width: f32, height: f32) -> Scene {
    let mut quads = Vec::new();
    let mut texts = Vec::new();
    let theme = theme();
    let scale = guide_scale(height);
    let [x, y, w, h] = keyboard_hint_rect(width, height);
    let fade = view.fade.clamp(0.0, 1.0);

    quads.push(Quad {
        x,
        y,
        w,
        h,
        slot: SOLID_SLOT,
        color: theme.glass.a(0.62),
        radius: h * 0.5,
        thickness: DEPTH_CONTROL * scale * 1.4,
        behind: view.behind,
        frost: FROST_PANEL,
        gloss: GLOSS_FULL,
        fade,
        ..Quad::default()
    });

    let glyph = HINT_GLYPH * scale;
    let label = HINT_LABEL * scale;
    let gap = HINT_GAP * scale;
    let mut at = x + HINT_PADDING * scale;
    let middle = y + h * 0.5;

    for (index, name) in [icons::PAD_SELECT, icons::PAD_WEST].into_iter().enumerate() {
        if index > 0 {
            texts.push(Text {
                content: "+".to_string(),
                x: at,
                y: middle - label * 0.66,
                size: label,
                color: theme.text_soft.a(0.75 * fade),
                bold: false,
                max_width: label * 0.6,
                align: TextAlign::Center,
            });
            at += label * 0.6 + gap;
        }
        // A glyph the shell could not rasterise leaves a gap rather than the
        // fallback application icon, which in a row like this would read as
        // "press the app".
        if let Some(slot) = view.slots.glyph(name) {
            quads.push(Quad {
                x: at,
                y: middle - glyph * 0.5,
                w: glyph,
                h: glyph,
                slot,
                color: theme.text.a(0.95 * fade),
                ..Quad::default()
            });
        }
        at += glyph + gap;
    }

    texts.push(Text {
        content: HINT_LABEL_TEXT.to_string(),
        x: at,
        y: middle - label * 0.66,
        size: label,
        color: theme.text.a(0.95 * fade),
        bold: false,
        max_width: x + w - at,
        align: TextAlign::Left,
    });

    Scene { quads, texts }
}

/// What a tile has to say about itself.
struct TileState {
    /// Whether its switch is on.
    on: bool,
    /// Whether it can do anything at all from here. A tile that cannot is not
    /// merely unlit — the highlight will not stop on it either.
    live: bool,
    /// Whether the highlight is sitting on it.
    focused: bool,
    /// How far through a press it is, if it is being pressed.
    press: Option<f32>,
}

/// How far a tile sinks under a press, as a share of itself, and how far it
/// springs back past its own size on the way out.
const PRESS_DIP: f32 = 0.14;
const PRESS_BOUNCE: f32 = 0.05;
/// The share of the press spent going down. The rest is the spring back, which
/// is slower: a switch is thrown quickly and settles at its leisure.
const PRESS_DOWN: f32 = 0.3;

/// How big a tile is drawn `t` of the way through a press, as a multiple of
/// its own size.
///
/// A switch is a physical thing: it goes down under the thumb, comes back
/// past where it started, and settles. Tinting it instead says only that
/// something has been *selected*, which the highlight already said — and
/// leaves the one control in the column whose whole purpose is to change
/// state with nothing to show for having changed it.
fn press_scale(t: f32) -> f32 {
    if !(0.0..1.0).contains(&t) {
        return 1.0;
    }
    if t < PRESS_DOWN {
        return 1.0 - PRESS_DIP * ease(t / PRESS_DOWN);
    }
    let back = ease((t - PRESS_DOWN) / (1.0 - PRESS_DOWN));
    // Out of the dip, and past the top on the way. The bounce is held back
    // until the dip has nearly closed — cubed, so its arch is late and
    // narrow — because a bounce that peaked with the recovery would only
    // cancel it, and the tile would crawl back up to its own size having
    // never gone past it. It ends at exactly zero, so the tile settles on its
    // own size rather than near it.
    1.0 - PRESS_DIP * (1.0 - back) + PRESS_BOUNCE * (back.powi(3) * std::f32::consts::PI).sin()
}

/// A rectangle scaled about its own centre.
fn scaled_about_centre([x, y, w, h]: [f32; 4], scale: f32) -> [f32; 4] {
    [
        x + w * (1.0 - scale) * 0.5,
        y + h * (1.0 - scale) * 0.5,
        w * scale,
        h * scale,
    ]
}

/// One of the two switch tiles, laid into the chip at `chip`.
///
/// A switch has to say which of its two states it is in from across a room,
/// and it has to say it while the lit selection capsule is sitting on top of
/// it — so being *lit* cannot be the signal, since the selection already is.
/// What says it instead is a second pane inside the chip: on, the tile is
/// filled with the accent and the glyph is white; off, the fill is not there
/// at all and the glyph is dimmed. Filled or not reads at a glance and reads
/// the same whether or not the row is selected.
fn tile(
    chip: [f32; 4],
    item: Item,
    state: TileState,
    scale: f32,
    behind: f32,
    alpha: f32,
    slots: &dyn SlotLookup,
) -> Vec<Quad> {
    let theme = theme();
    let mut quads = Vec::with_capacity(2);

    // Everything the tile is made of moves together under the press — the
    // fill, the glyph, and (back in the caller) the chip they rest on. A
    // glyph that stayed put while its chip sank would read as a hole opening
    // behind it rather than as a button going down.
    let press = state.press.map_or(1.0, press_scale);
    let [x, y, w, h] = scaled_about_centre(chip, press);

    if state.on {
        quads.push(Quad {
            x,
            y,
            w,
            h,
            slot: SOLID_SLOT,
            color: theme.accent.a(0.55),
            radius: chip_radius(Some(item), h),
            thickness: DEPTH_CONTROL * scale,
            behind,
            frost: FROST_CONTROL,
            gloss: GLOSS_FULL,
            // The fill arrives with the spring back rather than with the
            // press: the switch is thrown on the way *up*, which is where a
            // real one latches.
            fade: alpha * state.press.map_or(1.0, fill_arrival),
            ..Quad::default()
        });
    }

    let Some(slot) = item.glyph().and_then(|name| slots.glyph(name)) else {
        // A glyph the shell could not rasterise leaves the chip empty rather
        // than a wrong drawing in its place.
        return quads;
    };
    let glyph = h * GUIDE_TILE_GLYPH;
    // Four depths, and the order is the point: a switch that is on is the
    // brightest thing in the column, a switch that is merely selected is
    // next, and one that cannot be reached at all is a long way behind both.
    let lit = match (state.live, state.on, state.focused) {
        // A ghost of a glyph, inside the hairline the caller drew instead of a
        // chip. Three things say the same thing about a tile nothing can be
        // done with — no chip, no light in the glyph, and a highlight that
        // refuses to stop on it — because on a panel of five lit controls no
        // one of them is enough on its own.
        (false, _, _) => 0.3,
        (_, true, _) => 1.0,
        (_, false, true) => 0.95,
        _ => 0.82,
    };
    quads.push(Quad {
        x: x + (w - glyph) * 0.5,
        y: y + (h - glyph) * 0.5,
        w: glyph,
        h: glyph,
        slot,
        color: [1.0, 1.0, 1.0, lit * alpha],
        ..Quad::default()
    });

    quads
}

/// How much of the accent fill has arrived, `t` of the way through a press.
///
/// Held back until the tile is at the bottom of its travel, so the colour
/// changing and the switch going down are one movement rather than two.
fn fill_arrival(t: f32) -> f32 {
    ease((t - PRESS_DOWN * 0.6) / (1.0 - PRESS_DOWN * 0.6))
}

/// The track of a quick-settings bar, as a share of the chip it sits in: how
/// far the glyph is in from the left, how big it is, and how much air there is
/// between it and the track, and between the track and the right-hand end.
const BAR_INSET: f32 = 20.0;
const BAR_GLYPH: f32 = 0.54;
const BAR_GAP: f32 = 16.0;
/// How thick the track is drawn, and how much bigger than that the handle on
/// the end of the filled part is.
const BAR_TRACK: f32 = 7.0;
const BAR_HANDLE: f32 = 2.1;

/// One quick-settings bar, laid into the chip at `chip`.
///
/// Everything is white rather than accent-coloured, and deliberately: this
/// same row is drawn both on a plain glass chip and under the lit accent
/// capsule that glides onto it when it is selected. A fill tinted with the
/// accent would disappear the moment the row was chosen, which is exactly when
/// it most needs reading.
fn quick_bar(
    chip: [f32; 4],
    bar: Bar,
    level: Level,
    scale: f32,
    alpha: f32,
    slots: &dyn SlotLookup,
) -> Vec<Quad> {
    let theme = theme();
    let [x, y, w, h] = chip;
    let mut quads = Vec::with_capacity(4);

    let glyph = h * BAR_GLYPH;
    let glyph_x = x + BAR_INSET * scale;
    let name = match (bar, level.muted) {
        (Bar::Volume, false) => icons::VOLUME,
        (Bar::Volume, true) => icons::VOLUME_MUTED,
        (Bar::Brightness, _) => icons::BRIGHTNESS,
    };
    if let Some(slot) = slots.glyph(name) {
        quads.push(Quad {
            x: glyph_x,
            y: y + (h - glyph) * 0.5,
            w: glyph,
            h: glyph,
            slot,
            color: [1.0, 1.0, 1.0, alpha],
            ..Quad::default()
        });
    }

    let track_x = glyph_x + glyph + BAR_GAP * scale;
    let track_w = (x + w - BAR_INSET * scale) - track_x;
    if track_w <= 0.0 {
        return quads;
    }
    let track_h = BAR_TRACK * scale;
    let track_y = y + (h - track_h) * 0.5;

    quads.push(Quad {
        x: track_x,
        y: track_y,
        w: track_w,
        h: track_h,
        slot: SOLID_SLOT,
        color: theme.rim.a(0.20 * alpha),
        radius: track_h * 0.5,
        ..Quad::default()
    });

    // A muted session is still at whatever volume it was left at, so the fill
    // stays where it is and goes quiet instead of emptying — turning the sound
    // back on must not look like it also turned it up.
    let filled = track_w * level.value.clamp(0.0, 1.0);
    let lit = if level.muted { 0.30 } else { 0.95 };
    if filled > 0.0 {
        quads.push(Quad {
            x: track_x,
            y: track_y,
            w: filled,
            h: track_h,
            slot: SOLID_SLOT,
            color: theme.rim.a(lit * alpha),
            radius: track_h * 0.5,
            ..Quad::default()
        });
    }

    // The handle. Small, and only there to say that the end of the fill is a
    // place the value *is* rather than where a drawing happens to stop.
    let handle = track_h * BAR_HANDLE;
    quads.push(Quad {
        x: track_x + filled - handle * 0.5,
        y: track_y + (track_h - handle) * 0.5,
        w: handle,
        h: handle,
        slot: SOLID_SLOT,
        color: theme.rim.a(lit * alpha),
        radius: handle * 0.5,
        ..Quad::default()
    });

    quads
}

/// The standby symbol every desktop draws on its power button: a ring broken
/// at the top with a stroke rising through the break, centred on `cx, cy`
/// with the given `radius`.
///
/// The break is cut out of the ring rather than painted over, because this
/// overlay is transparent — anything "erased" with a background colour would
/// show as a dark bar against the application behind it.
fn power_glyph(cx: f32, cy: f32, radius: f32, scale: f32, color: [f32; 4]) -> [Quad; 2] {
    let stroke = (2.6 * scale).max(1.5);
    let gap = stroke * 1.5;
    [
        Quad {
            x: cx - radius,
            y: cy - radius,
            w: radius * 2.0,
            h: radius * 2.0,
            slot: SOLID_SLOT,
            color,
            radius,
            border: stroke,
            notch: gap,
            ..Quad::default()
        },
        Quad {
            x: cx - stroke * 0.5,
            y: cy - radius * 1.32,
            w: stroke,
            h: radius * 1.32,
            slot: SOLID_SLOT,
            color,
            radius: stroke * 0.5,
            ..Quad::default()
        },
    ]
}

/// Cubic ease-in-out: the shell's shape for anything that runs on its own
/// clock rather than chasing a target — every fade and every slide-in here.
///
/// A ramp straight off its clock is what makes an animation read as
/// mechanical, and a pure ease-out, the usual choice for something a button
/// summoned, still leaves at full speed. Only the arrival is softened, and the
/// departure is the half you watch. This is gentle at both ends, like the
/// springs the chases ride and the smoothstep the compositor flies its
/// windows on.
pub fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        let back = -2.0 * t + 2.0;
        1.0 - back * back * back / 2.0
    }
}

/// A square icon, or a tinted placeholder when the theme had no such icon —
/// which keeps rows aligned instead of leaving a hole.
fn icon_quad(slot: Option<u32>, x: f32, y: f32, size: f32, alpha: f32, missing: [f32; 4]) -> Quad {
    match slot {
        Some(slot) => Quad {
            x,
            y,
            w: size,
            h: size,
            slot,
            color: [1.0, 1.0, 1.0, alpha],
            ..Quad::default()
        },
        None => Quad {
            x,
            y,
            w: size,
            h: size,
            slot: SOLID_SLOT,
            color: missing,
            ..Quad::default()
        },
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

fn lerp_rect(from: [f32; 4], to: [f32; 4], t: f32) -> [f32; 4] {
    let mut out = [0.0; 4];
    for (slot, (from, to)) in out.iter_mut().zip(from.iter().zip(&to)) {
        *slot = lerp(*from, *to, t);
    }
    out
}

// --- the launch splash ---------------------------------------------------

/// The glass disc the bar's focused entry stands on — where an application
/// launched right now would open out of.
///
/// Derived from the same anchors [`build`] draws that disc at; the test
/// `a_launch_opens_out_of_the_tile_it_was_chosen_from` holds the two together.
/// It is the settled position, which is the right answer even mid-glide: the
/// cross's focus is a fixed point on the display, and the row is travelling
/// *to* it.
pub fn launch_origin(width: f32, height: f32) -> [f32; 4] {
    let scale = guide_scale(height);
    let cross_x = width * BAR_CROSS_X;
    let cross_y = height * BAR_CROSS_Y;
    let y = item_y(
        0.0,
        cross_y,
        ITEM_GAP_ABOVE * scale,
        ITEM_GAP_BELOW * scale,
        ITEM_SPACING * scale,
    );
    let size = ITEM_ICON_FOCUSED * scale * ITEM_DISC;
    [cross_x - size * 0.5, y - size * 0.5, size, size]
}

/// The splash panel `open` of the way out of `from`, and the corner radius to
/// draw it with.
///
/// The tile is a disc and the display is a rectangle with no corners at all,
/// so the radius travels between the two rather than riding the growth: a
/// panel that kept its tile's proportional rounding would reach full screen
/// still visibly a rounded card.
pub fn launch_panel(from: [f32; 4], width: f32, height: f32, open: f32) -> ([f32; 4], f32) {
    let grown = ease(open);
    (
        lerp_rect(from, [0.0, 0.0, width, height], grown),
        lerp(from[3] * 0.5, 0.0, grown),
    )
}

/// One frame of the launch splash.
pub struct LaunchView<'a> {
    pub name: &'a str,
    /// The application's icon, already resolved to a texture slot.
    pub icon: Option<u32>,
    /// The tile it is opening out of.
    pub from: [f32; 4],
    /// 0 on the tile, 1 filling the display.
    pub open: f32,
    /// 1 while it holds the screen, falling to 0 as the application takes it.
    pub fade: f32,
    /// Whether it is still waiting for the application, as against handing
    /// over to one that has arrived.
    pub waiting: bool,
    /// The global clock, for the indicator.
    pub time: f32,
}

/// Draw the splash: a panel out of the tile, the application's icon and name
/// on it, and something that says the wait is expected.
///
/// Everything on it arrives *after* the panel has, so nothing is legible
/// while it is still tile-sized — the same reason the power dialog's rows
/// hold back, and the difference between an application opening and a
/// screenful of furniture being scaled up.
pub fn build_launch(view: LaunchView, width: f32, height: f32) -> Scene {
    let theme = theme();
    let scale = guide_scale(height);
    let mut scene = Scene::default();
    let ([px, py, pw, ph], radius) = launch_panel(view.from, width, height, view.open);
    let grown = ease(view.open);

    // Opaque, unlike everything else the shell draws. Glass would be wrong
    // twice over: the application is meant to be revealed from *behind* this
    // rather than seen through it, and light transmitted through a pane is
    // linear — a couple of percent of a bright window still washes a
    // screen-sized panel visibly grey.
    scene.quads.push(Quad {
        x: px,
        y: py,
        w: pw,
        h: ph,
        slot: SOLID_SLOT,
        color: theme.glass.a(1.0),
        radius,
        // Lit like a button while it is still button-sized, and not at all
        // once it is the screen: a rim is a property of an edge you can see
        // all of, and a sheen down the face of a full display is just a
        // gradient nobody asked for.
        gloss: GLOSS_QUIET * (1.0 - grown),
        ..Quad::default()
    });

    // The icon travels from the tile to its place on the panel, growing as it
    // goes: one object moving, so the eye follows the thing it chose all the
    // way to where it lands.
    let icon_size = lerp(view.from[3] / ITEM_DISC, LAUNCH_ICON * scale, grown);
    let icon_x = lerp(view.from[0] + view.from[2] * 0.5, width * 0.5, grown);
    let icon_y = lerp(view.from[1] + view.from[3] * 0.5, height * 0.44, grown);

    // The bloom under it, which is what the bar's focused entry already had.
    let bloom = icon_size * 2.1;
    scene.quads.push(Quad {
        x: icon_x - bloom * 0.5,
        y: icon_y - bloom * 0.5,
        w: bloom,
        h: bloom,
        slot: GLOW_SLOT,
        color: theme.accent.a(0.3 * grown),
        ..Quad::default()
    });
    scene.quads.push(icon_quad(
        view.icon,
        icon_x - icon_size * 0.5,
        icon_y - icon_size * 0.5,
        icon_size,
        1.0,
        theme.glass_raised.a(0.5),
    ));

    // The name, and then the indicator under it. Both hold back until the
    // panel is most of the way out.
    let arrived = ease(((view.open - 0.55) / 0.45).clamp(0.0, 1.0));
    let name_size = 34.0 * scale;
    scene.texts.push(Text {
        content: view.name.to_string(),
        x: 0.0,
        y: icon_y + icon_size * 0.5 + 46.0 * scale,
        size: name_size,
        color: theme.text.a(0.96 * arrived),
        bold: true,
        max_width: width,
        align: TextAlign::Center,
    });

    // A ring of dots lighting in turn. Not a bar: nothing here knows how far
    // along the application is, and a bar that does not measure anything is a
    // lie about how much longer this will take.
    let dot = 9.0 * scale;
    let orbit = icon_size * 0.5 + 34.0 * scale;
    let phase = view.time / LAUNCH_SPIN;
    for index in 0..LAUNCH_DOTS {
        let turn = index as f32 / LAUNCH_DOTS as f32;
        let angle = turn * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
        // Each dot is brightest as the sweep passes it and dims behind it.
        let behind = (phase - turn).rem_euclid(1.0);
        let lit = if view.waiting {
            0.2 + 0.8 * (1.0 - behind).powi(3)
        } else {
            // Nothing left to wait for: the ring settles rather than spinning
            // on over an application that is already there.
            0.2
        };
        scene.quads.push(Quad {
            x: icon_x + angle.cos() * orbit - dot * 0.5,
            y: icon_y + angle.sin() * orbit - dot * 0.5,
            w: dot,
            h: dot,
            slot: SOLID_SLOT,
            color: theme.accent_soft.a(lit * arrived),
            radius: dot * 0.5,
            ..Quad::default()
        });
    }

    scene.fade(view.fade);
    scene
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::{App, Category};
    use crate::model::Action;
    use std::path::PathBuf;

    /// An icon, as opposed to the glass it stands on or the bloom behind it:
    /// icons are drawn square and unlit, everything else is material.
    fn is_icon(quad: &Quad) -> bool {
        quad.slot != GLOW_SLOT && quad.gloss == 0.0 && quad.radius == 0.0
    }

    struct NoSlots;
    impl SlotLookup for NoSlots {
        fn slot_for(&self, _icon: Option<&str>) -> Option<u32> {
            None
        }
    }

    /// Every icon resolves, so a quad's alpha is the layout's own and not the
    /// missing-icon placeholder tint.
    struct AllSlots;
    impl SlotLookup for AllSlots {
        fn slot_for(&self, _icon: Option<&str>) -> Option<u32> {
            Some(7)
        }
    }

    /// A distinct slot per name, so a test can tell *which* drawing the layout
    /// asked for rather than only that it asked for one.
    struct Named;
    impl Named {
        fn slot_of(name: &str) -> u32 {
            match name {
                icons::VOLUME => 10,
                icons::VOLUME_MUTED => 11,
                icons::BRIGHTNESS => 12,
                icons::POINTER_STICK => 21,
                icons::VOLUME_MIXER => 22,
                icons::PAD_SELECT => 14,
                icons::PAD_WEST => 15,
                icons::ARROW_LEFT => 16,
                icons::ARROW_DOWN => 17,
                icons::ARROW_UP => 18,
                icons::ARROW_RIGHT => 19,
                icons::KEYBOARD_HIDE => 20,
                _ => 13,
            }
        }
    }
    impl SlotLookup for Named {
        fn slot_for(&self, icon: Option<&str>) -> Option<u32> {
            Some(Self::slot_of(icon.unwrap_or_default()))
        }
        fn glyph(&self, name: &str) -> Option<u32> {
            Some(Self::slot_of(name))
        }
    }

    fn app(name: &str) -> App {
        App {
            name: name.into(),
            comment: Some("does a thing".into()),
            icon: Some("icon".into()),
            exec: "true".into(),
            terminal: false,
            categories: Vec::new(),
            path: PathBuf::from("/tmp/x.desktop"),
        }
    }

    /// Lay a bar out for a display that is taking input.
    fn focused(xmb: &Xmb, width: f32, height: f32, slots: &impl SlotLookup) -> Scene {
        let cursor = Cursor::new(xmb.categories.len());
        build_with(xmb, &cursor, width, height, true, slots)
    }

    fn build_with(
        xmb: &Xmb,
        cursor: &Cursor,
        width: f32,
        height: f32,
        focused: bool,
        slots: &impl SlotLookup,
    ) -> Scene {
        build(xmb, cursor, width, height, focused, None, 0.0, slots)
    }

    fn settle(cursor: &mut Cursor) {
        while cursor.animate(1.0 / 60.0) {}
    }

    /// The selected category's name goes under its button, and under means
    /// clear of it: the icon is a good deal smaller than the disc it stands
    /// on, and a label measured from the icon has its head inside the glass.
    #[test]
    fn a_categorys_label_clears_the_button_it_names() {
        let xmb = Xmb::new(vec![Category {
            id: "dev",
            title: "Development",
            icon: "dev",
            apps: vec![app("first")],
        }]);

        for (width, height) in [(1280.0, 800.0), (1920.0, 1080.0), (3840.0, 2160.0)] {
            let scene = focused(&xmb, width, height, &AllSlots);
            let cross_y = height * BAR_CROSS_Y;

            // The button: the largest circle centred on the category row.
            let disc = scene
                .quads
                .iter()
                .filter(|quad| {
                    (quad.radius - quad.w * 0.5).abs() < 0.01
                        && ((quad.y + quad.h * 0.5) - cross_y).abs() < 0.5
                })
                .max_by(|a, b| a.w.total_cmp(&b.w))
                .expect("the selected category stands on a disc");
            let label = scene
                .texts
                .iter()
                .find(|text| text.content == "Development")
                .expect("and is named under it");

            assert!(
                label.y >= disc.y + disc.h,
                "{width}x{height}: the label starts at {} inside a disc ending at {}",
                label.y,
                disc.y + disc.h
            );
            // Under it, not adrift from it — the gap is air, not a new row.
            assert!(label.y - (disc.y + disc.h) < label.size);

            // And clear at the bottom too. The stretch it sits in is the gap
            // between the category row and the first item, so making room
            // above the label is only half of it: the item column's own
            // button is what it would run into next.
            let column = launch_origin(width, height);
            assert!(
                label.y + label.size <= column[1],
                "{width}x{height}: the label ends at {} over a button starting at {}",
                label.y + label.size,
                column[1]
            );
        }
    }

    /// An icon on a round tile has to fit it corner-first. Icon art is often
    /// drawn to the edge of its square — a white sheet with a picture on it —
    /// and on a disc under √2 icons across, those four corners hang out over
    /// the rim: the icon reads as having come loose from the selection rather
    /// than as sitting on it. Nothing about the icon says which kind it is, so
    /// the fit has to hold for the square, not for the artwork inside it.
    #[test]
    fn a_focused_items_icon_stays_inside_the_disc_it_stands_on() {
        let xmb = Xmb::new(vec![Category {
            id: "a",
            title: "A",
            icon: "a",
            apps: vec![app("first"), app("second")],
        }]);

        for (width, height) in [(1280.0, 800.0), (1920.0, 1080.0), (3840.0, 2160.0)] {
            let scene = focused(&xmb, width, height, &AllSlots);
            let disc = launch_origin(width, height);
            let centre = [disc[0] + disc[2] * 0.5, disc[1] + disc[3] * 0.5];
            let icon = scene
                .quads
                .iter()
                .filter(|quad| is_icon(quad))
                .find(|quad| {
                    (quad.x + quad.w * 0.5 - centre[0]).abs() < 0.5
                        && (quad.y + quad.h * 0.5 - centre[1]).abs() < 0.5
                })
                .expect("the focused item stands an icon on its disc");

            let reach = icon.w * std::f32::consts::SQRT_2 / 2.0;
            assert!(
                reach <= disc[2] * 0.5,
                "{width}x{height}: the icon's corners reach {reach} out of a disc {} across",
                disc[2] * 0.5
            );
        }
    }

    /// The splash grows out of the bar's focused entry, so it has to know
    /// where that is. Two places computing one rectangle drift, and the drift
    /// shows as an application opening out of thin air beside its own icon —
    /// so the answer is checked against the disc the bar actually draws.
    #[test]
    fn a_launch_opens_out_of_the_tile_it_was_chosen_from() {
        for (width, height) in [(1280.0, 800.0), (1920.0, 1080.0), (3840.0, 2160.0)] {
            let xmb = Xmb::new(vec![Category {
                id: "a",
                title: "A",
                icon: "a",
                apps: vec![app("first"), app("second")],
            }]);
            let scene = focused(&xmb, width, height, &AllSlots);
            let tile = launch_origin(width, height);

            let disc = scene.quads.iter().find(|quad| {
                (quad.radius - quad.w * 0.5).abs() < 0.01
                    && (quad.x - tile[0]).abs() < 0.5
                    && (quad.y - tile[1]).abs() < 0.5
                    && (quad.w - tile[2]).abs() < 0.5
                    && (quad.h - tile[3]).abs() < 0.5
            });
            assert!(
                disc.is_some(),
                "{width}x{height}: nothing on the bar is at {tile:?}"
            );
        }
    }

    /// It opens *out of* the tile and ends up the display: anything else and
    /// the application appears to arrive from somewhere the user did not
    /// press. What is written on it holds back until there is room for it.
    #[test]
    fn the_launch_splash_grows_from_the_tile_to_the_whole_display() {
        let (width, height) = (1920.0, 1080.0);
        let tile = launch_origin(width, height);

        let (shut, radius) = launch_panel(tile, width, height, 0.0);
        assert_eq!(shut, tile, "it starts as the tile itself");
        assert!(
            (radius - tile[3] * 0.5).abs() < 0.01,
            "and as round as the tile is: {radius}"
        );
        let (open, radius) = launch_panel(tile, width, height, 1.0);
        assert_eq!(open, [0.0, 0.0, width, height]);
        assert_eq!(radius, 0.0, "a full display has no corners left to round");

        let at = |open: f32, fade: f32| {
            build_launch(
                LaunchView {
                    name: "Celeste",
                    icon: Some(7),
                    from: tile,
                    open,
                    fade,
                    waiting: true,
                    time: 0.0,
                },
                width,
                height,
            )
        };
        let label = |scene: &Scene| {
            scene
                .texts
                .iter()
                .find(|text| text.content == "Celeste")
                .map(|text| text.color[3])
                .unwrap_or(0.0)
        };
        // Tile-sized, so there is nothing legible on it yet.
        assert_eq!(label(&at(0.2, 1.0)), 0.0);
        assert!(label(&at(1.0, 1.0)) > 0.5);

        // And the whole thing goes, panel included, as the application takes
        // the screen — an opaque panel left at full strength would hide it.
        let handing = at(1.0, 0.0);
        assert!(
            handing.quads.iter().all(|quad| quad.fade <= 0.001),
            "nothing may outlast the handover"
        );
        assert_eq!(label(&handing), 0.0);
    }

    #[test]
    fn empty_model_still_renders_a_message() {
        let xmb = Xmb::new(Vec::new());
        let scene = focused(&xmb, 1920.0, 1080.0, &NoSlots);
        assert!(scene.quads.is_empty());
        assert_eq!(scene.texts.len(), 1);
    }

    #[test]
    fn offscreen_rows_are_culled() {
        // A long list must not emit a quad per entry.
        let apps: Vec<App> = (0..500).map(|i| app(&format!("app{i}"))).collect();
        let xmb = Xmb::new(vec![Category {
            id: "a",
            title: "A",
            icon: "a",
            apps,
        }]);

        let scene = focused(&xmb, 1920.0, 1080.0, &NoSlots);
        assert!(
            scene.quads.len() < 40,
            "expected culling, got {} quads",
            scene.quads.len()
        );
    }

    #[test]
    fn a_display_that_is_not_taking_input_is_dimmed_but_still_legible() {
        let xmb = Xmb::new(vec![Category {
            id: "a",
            title: "A",
            icon: "a",
            apps: vec![app("first"), app("second")],
        }]);
        let cursor = Cursor::new(1);

        let live = build_with(&xmb, &cursor, 1920.0, 1080.0, true, &AllSlots);
        let idle = build_with(&xmb, &cursor, 1920.0, 1080.0, false, &AllSlots);

        let brightest = |scene: &Scene| {
            scene
                .quads
                .iter()
                .map(|quad| quad.color[3])
                .fold(0.0_f32, f32::max)
        };
        assert!(
            brightest(&idle) < brightest(&live),
            "an idle display should be dimmer"
        );
        assert!(
            brightest(&idle) > 0.2,
            "but not so dim the user cannot see what is on it"
        );
    }

    /// With the instruction footer gone, the fourth application on a compact
    /// display remains useful instead of fading almost completely into space
    /// reserved for text that is no longer there.
    #[test]
    fn the_item_column_uses_the_space_below_the_old_footer() {
        let xmb = Xmb::new(vec![Category {
            id: "a",
            title: "A",
            icon: "a",
            apps: vec![app("first"), app("second"), app("third"), app("fourth")],
        }]);

        let scene = focused(&xmb, 960.0, 600.0, &AllSlots);
        let fourth = scene
            .texts
            .iter()
            .find(|text| text.content == "fourth")
            .expect("the fourth row should still be drawn");
        assert!(
            fourth.color[3] > 0.20,
            "the reclaimed bottom row is still too faint: {}",
            fourth.color[3]
        );
        assert!(scene.texts.iter().all(|text| {
            !text.content.contains("D-pad") && !text.content.contains("Keyboard:")
        }));
    }

    #[test]
    fn every_category_that_fits_on_screen_stays_visible() {
        // A category the user can see must never be faded out: the row is how
        // they know what else is there.
        let categories: Vec<Category> = ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k"]
            .into_iter()
            .map(|id| Category {
                id,
                title: "Category",
                icon: "icon",
                apps: vec![app("only")],
            })
            .collect();
        let expected = categories.len();

        let xmb = Xmb::new(categories);
        // Wide enough that all eleven fit on the row, so anything missing was
        // faded away rather than legitimately culled off-screen.
        let scene = focused(&xmb, 3840.0, 1080.0, &AllSlots);

        let row_y = 1080.0 * 0.30;
        // The selection bloom and the glass each category stands on straddle
        // the row too; only the icons themselves count here.
        let row: Vec<&Quad> = scene
            .quads
            .iter()
            .filter(|quad| is_icon(quad))
            .filter(|quad| quad.y < row_y && quad.y + quad.h > row_y)
            .collect();
        assert_eq!(row.len(), expected, "every category should be drawn");
        for quad in row {
            assert!(
                quad.color[3] >= CATEGORY_MIN_ALPHA,
                "category faded to {}",
                quad.color[3]
            );
        }
    }

    #[test]
    fn focused_entry_gets_a_glow_and_comment() {
        let xmb = Xmb::new(vec![Category {
            id: "a",
            title: "A",
            icon: "a",
            apps: vec![app("first"), app("second")],
        }]);
        let scene = focused(&xmb, 1920.0, 1080.0, &NoSlots);

        // Selected item and selected category each breathe under a glow.
        assert_eq!(
            scene.quads.iter().filter(|q| q.slot == GLOW_SLOT).count(),
            2
        );
        // Focused item contributes both a name and a comment line.
        assert!(scene.texts.iter().any(|t| t.content == "first"));
        assert!(scene.texts.iter().any(|t| t.content == "does a thing"));
    }

    #[test]
    fn the_item_column_splits_around_the_category_row() {
        // The regression this guards against: item rows printing over the
        // category icons and label, which made both unreadable.
        let xmb = Xmb::new(vec![Category {
            id: "a",
            title: "Settings",
            icon: "a",
            apps: vec![app("first"), app("second"), app("third")],
        }]);
        let mut cursor = Cursor::new(1);
        cursor.navigate(Action::Down, &xmb);
        settle(&mut cursor);

        let (width, height) = (1920.0, 1080.0);
        let scene = build_with(&xmb, &cursor, width, height, true, &AllSlots);

        // The exclusion band the category row owns: its icons plus the label
        // beneath the selected one.
        let cross_y = height * 0.30;
        let band_top = cross_y - CATEGORY_ICON_FOCUSED / 2.0;
        let band_bottom = cross_y + CATEGORY_ICON_FOCUSED / 2.0 + 12.0 + 26.0 * 1.25;

        // Item icons live on the cross column; the category icon there is the
        // one centred exactly on the row.
        let item_icons: Vec<&Quad> = scene
            .quads
            .iter()
            .filter(|q| is_icon(q))
            .filter(|q| (q.x + q.w / 2.0 - width * 0.22).abs() < 1.0)
            .filter(|q| (q.y + q.h / 2.0 - cross_y).abs() > 1.0)
            .collect();
        assert_eq!(item_icons.len(), 3, "all three items should be drawn");

        let above: Vec<_> = item_icons
            .iter()
            .filter(|q| q.y + q.h <= band_top)
            .collect();
        let below: Vec<_> = item_icons.iter().filter(|q| q.y >= band_bottom).collect();
        assert_eq!(above.len(), 1, "the passed entry moves above the row");
        assert_eq!(below.len(), 2, "selection and the rest sit below it");

        // And no text may enter the band either — text overlapping the
        // category icons is exactly what the old layout did wrong.
        for text in &scene.texts {
            let top = text.y;
            let bottom = text.y + text.size * 1.25;
            let is_category_label = text.content == "Settings";
            assert!(
                is_category_label || bottom <= band_top || top >= band_bottom,
                "{:?} at {top}..{bottom} intrudes into the category band {band_top}..{band_bottom}",
                text.content,
            );
        }
    }

    #[test]
    fn the_selection_glow_breathes_over_time() {
        let xmb = Xmb::new(vec![Category {
            id: "a",
            title: "A",
            icon: "a",
            apps: vec![app("first")],
        }]);
        let cursor = Cursor::new(1);

        let glow_alpha = |time: f32| {
            let scene = build(&xmb, &cursor, 1920.0, 1080.0, true, None, time, &AllSlots);
            scene
                .quads
                .iter()
                .filter(|q| q.slot == GLOW_SLOT)
                .map(|q| q.color[3])
                .fold(0.0_f32, f32::max)
        };

        // A quarter period apart the pulse must visibly differ; a static
        // highlight is what this replaces.
        let delta = (glow_alpha(0.0) - glow_alpha(PULSE_PERIOD / 4.0)).abs();
        assert!(delta > 0.05, "glow should pulse, changed by only {delta}");
    }

    fn card(title: &str) -> Card {
        Card {
            title: title.to_string(),
            width: 1920.0,
            height: 1080.0,
            start: false,
            rect: [0.0; 4],
        }
    }

    /// A machine with both quick-settings controls, which is the column at its
    /// longest. The default is neither: nothing has looked yet.
    const BOTH_BARS: crate::guide::Bars = crate::guide::Bars {
        volume: true,
        brightness: true,
    };

    /// Fill each card's rectangle from the shared layout, the way the
    /// shell's draw loop does once easing has settled.
    fn lay_out(cards: &mut [Card], selected: usize, width: f32, height: f32) {
        let slots = overview::card_slots(width as f64, height as f64, cards.len(), selected);
        for (card, slot) in cards.iter_mut().zip(&slots) {
            let fitted = overview::fit(slot, card.width as f64, card.height as f64);
            card.rect = [
                fitted.x as f32,
                fitted.y as f32,
                fitted.w as f32,
                fitted.h as f32,
            ];
        }
    }

    fn guide_scene(
        guide: &Guide,
        app: Option<&str>,
        screen: Option<&str>,
        cards: &[Card],
        highlight: Option<[f32; 4]>,
    ) -> Scene {
        guide_scene_with(guide, app, screen, cards, highlight, &AllSlots)
    }

    /// The same scene with the atlas swapped out, for the one test that has to
    /// see what the column does when a glyph did not rasterise.
    fn guide_scene_with(
        guide: &Guide,
        app: Option<&str>,
        screen: Option<&str>,
        cards: &[Card],
        highlight: Option<[f32; 4]>,
        slots: &dyn SlotLookup,
    ) -> Scene {
        // A window is selected beside the column whenever there is one to
        // select, which is what the shell passes.
        let close_target = cards.first().filter(|card| !card.start).map(|c| &*c.title);
        // A bar has a level exactly when the column has its row: the shell
        // sets both from the same answer, and a test that could disagree
        // about it would be testing a state that cannot happen.
        let items = guide.items(close_target.is_some());
        let level = |item: Item, value: f32| {
            items.contains(&item).then_some(Level {
                value,
                muted: false,
            })
        };
        build_guide(
            GuideView {
                guide,
                clock: Some(Clock {
                    time: "15:18",
                    date: "Tue 5 Aug",
                }),
                volume: level(Item::Volume, 0.35),
                brightness: level(Item::Brightness, 0.85),
                stick_pointer: false,
                app,
                close_target,
                screen,
                cards,
                highlight,
                // Settled on the selected row, as it is once the glide ends.
                menu_highlight: None,
                behind: 0.0,
                card_age: guide.age(),
                // Fully out of its button, as it is once it has opened.
                power: if guide.power_open() { 1.0 } else { 0.0 },
                time: 0.0,
                slots,
            },
            1920.0,
            1080.0,
        )
    }

    /// Every fade and slide in the shell runs through this, so what it does
    /// at the ends is what the whole overlay feels like.
    #[test]
    fn the_shells_ramps_leave_and_arrive_at_rest() {
        assert_eq!(ease(0.0), 0.0);
        assert_eq!(ease(1.0), 1.0);
        assert!((ease(0.5) - 0.5).abs() < 1e-6);

        // The complaint this answers: a ramp that covers ground the instant
        // it starts. A tenth of the way through, it is nowhere near a tenth
        // of the way there — and symmetrically at the far end.
        assert!(
            ease(0.1) < 0.02,
            "it should still be leaving: {}",
            ease(0.1)
        );
        assert!(ease(0.9) > 0.98);

        // Never backwards, and never past either end however it is called.
        let mut previous = 0.0;
        for step in 0..=100 {
            let now = ease(step as f32 / 100.0);
            assert!(now >= previous);
            previous = now;
        }
        assert_eq!(ease(-1.0), 0.0);
        assert_eq!(ease(2.0), 1.0);
    }

    /// The menu opens on one display, so with more than one it has to say
    /// which.
    #[test]
    fn the_guide_names_its_display_only_when_there_is_a_choice() {
        let mut guide = Guide::default();
        guide.open();

        let alone = guide_scene(&guide, Some("Celeste"), None, &[], None);
        assert!(alone.texts.iter().all(|t| !t.content.starts_with("Screen")));

        let among_others = guide_scene(&guide, Some("Celeste"), Some("DP-2"), &[], None);
        assert!(among_others
            .texts
            .iter()
            .any(|t| t.content == "Screen DP-2"));
    }

    /// The column is buttons and nothing else: no per-entry explanation under
    /// the selection, and no block of control hints at its foot. Both spent
    /// the sidebar's height on text nobody reads twice.
    #[test]
    fn the_column_carries_no_prose() {
        let mut guide = Guide::default();
        guide.open();
        guide.backdate_open(2.0);
        let cards = [card("Celeste")];
        let scene = guide_scene(&guide, Some("Celeste"), Some("DP-2"), &cards, None);

        let labels: Vec<&str> = scene.texts.iter().map(|t| t.content.as_str()).collect();
        assert!(
            labels.contains(&"Resume") && labels.contains(&"Close Celeste"),
            "{labels:?}"
        );
        for text in &scene.texts {
            assert!(
                !text.content.contains("Enter")
                    && !text.content.contains("D-pad")
                    && !text.content.contains(" / "),
                "{:?} is an instruction, not a button",
                text.content
            );
        }
    }

    /// The power button is a glyph at the foot of the sidebar, not a row in
    /// the stack — and its ring is drawn broken, which is the one thing a
    /// transparent overlay cannot fake by painting over it.
    #[test]
    fn the_power_button_sits_at_the_foot_of_the_column() {
        let mut guide = Guide::default();
        guide.open();
        guide.backdate_open(2.0);
        let scene = guide_scene(&guide, Some("Celeste"), None, &[], None);

        let items = guide.items(false);
        let power = items.len() - 1;
        let [px, py, pw, ph] = menu_item_rect(&items, power, 1920.0, 1080.0);
        assert!((pw - ph).abs() < 1.0, "the power button should be square");
        assert!(py > 1080.0 * 0.8, "and sit at the foot of the sidebar");
        let last_row = menu_item_rect(&items, power - 1, 1920.0, 1080.0);
        assert!(py > last_row[1] + last_row[3] * 2.0, "well below the rows");

        // A square in a corner is only in the corner if both its gaps match.
        let [sidebar_x, sidebar_y, _, sidebar_h] = sidebar_panel_rect(1920.0, 1080.0);
        let from_left = px - sidebar_x;
        let from_foot = (sidebar_y + sidebar_h) - (py + ph);
        assert!(
            (from_left - from_foot).abs() < 0.5,
            "the button is {from_left} from the side and {from_foot} from the foot"
        );
        // And it keeps the column's own inset, so it lines up with the chips.
        assert!((px - menu_item_rect(&items, 0, 1920.0, 1080.0)[0]).abs() < 0.5);

        // The symbol comes out of the atlas now rather than being assembled
        // from quads, so what there is to check is that it lands square and
        // centred on the chip it has instead of a label.
        let centre = px + pw * 0.5;
        let symbol = scene
            .quads
            .iter()
            .filter(|q| is_icon(q))
            .find(|q| {
                (q.x + q.w * 0.5 - centre).abs() < 1.0
                    && (q.y + q.h * 0.5 - (py + ph * 0.5)).abs() < 1.0
            })
            .expect("the power button should carry the shutdown glyph");
        assert!((symbol.w - symbol.h).abs() < 0.01, "drawn square");
        assert!(
            symbol.w < ph,
            "and inside its chip rather than over its edge"
        );

        // A glyph that failed to rasterise leaves every other control in the
        // column an empty chip beside a bar that still says what it is. This
        // one has no label at all, so it falls back to assembling the symbol.
        // The ring has to be *cut* rather than painted over: nothing here is
        // opaque, and a painted notch would show as a bar of the wrong colour
        // laid across whatever is behind the sidebar.
        let bare = guide_scene_with(&guide, Some("Celeste"), None, &[], None, &NoSlots);
        let notched = bare
            .quads
            .iter()
            .find(|q| q.notch > 0.0)
            .expect("with no glyph the power button should still draw its ring");
        assert!(notched.border > 0.0, "and draw it as a ring");
        assert!((notched.x + notched.w * 0.5 - centre).abs() < 1.0);
    }

    /// The header answers the question a glance at the menu is asking. What
    /// used to be there — the name of the shell you are already looking at —
    /// answered nothing.
    #[test]
    fn the_sidebar_is_headed_by_the_time_and_the_day() {
        let mut guide = Guide::default();
        guide.open();
        guide.backdate_open(2.0);
        let scene = guide_scene(&guide, Some("Celeste"), None, &[], None);
        let text = |content: &str| {
            scene
                .texts
                .iter()
                .find(|text| text.content == content)
                .unwrap_or_else(|| panic!("{content:?} should be in the header"))
        };

        let time = text("15:18");
        let date = text("Tue 5 Aug");
        assert!(time.bold, "the time is the heading");
        assert!(
            date.size < time.size,
            "and the day is beside it, not over it"
        );
        // One line, not two: the sidebar is narrow, and the app it is over
        // still has to fit under both of them.
        assert!((date.y - time.y).abs() < time.size);
        assert_eq!(date.align, TextAlign::Right, "pushed to the far side");
        assert!(
            !scene.texts.iter().any(|text| text.content == "Linboard"),
            "the shell no longer introduces itself"
        );

        // Without a clock the header falls back to naming the shell, which is
        // better than an empty space or a wrong time.
        let mut blind = GuideView {
            guide: &guide,
            clock: None,
            volume: None,
            brightness: None,
            stick_pointer: false,
            app: None,
            close_target: None,
            screen: None,
            cards: &[],
            highlight: None,
            menu_highlight: None,
            behind: 0.0,
            card_age: guide.age(),
            power: 0.0,
            time: 0.0,
            slots: &AllSlots,
        };
        blind.clock = None;
        let scene = build_guide(blind, 1920.0, 1080.0);
        assert!(scene.texts.iter().any(|text| text.content == "Linboard"));
    }

    /// The two bars sit at the top of the column, above everything the menu
    /// does, and are ruled off from it.
    #[test]
    fn the_quick_settings_bars_head_the_column() {
        let mut guide = Guide::default();
        guide.set_bars(BOTH_BARS);
        let items = guide.items(true);
        assert_eq!(
            items,
            vec![
                Item::Mixer,
                Item::Volume,
                Item::Brightness,
                Item::Resume,
                Item::Close,
                Item::Dashboard,
                Item::Power
            ]
        );

        // Two rules: one under the tiles and bars, one above Dashboard.
        let rules = menu_separator_rects(&items, 1920.0, 1080.0);
        assert_eq!(rules.len(), 2);
        let index_of = |wanted: Item| items.iter().position(|item| *item == wanted).unwrap();
        let resume = menu_item_rect(&items, index_of(Item::Resume), 1920.0, 1080.0);
        assert!(rules[0][1] < resume[1], "the first rule is above Resume");
        let brightness = menu_item_rect(&items, index_of(Item::Brightness), 1920.0, 1080.0);
        assert!(
            rules[0][1] > brightness[1] + brightness[3],
            "and below the bars"
        );

        // Rows in order, none overlapping, whatever heights they are. The
        // second tile is skipped: it is beside the first rather than under it,
        // which is exactly what the next assertion checks.
        let tiles: Vec<[f32; 4]> = items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.is_tile())
            .map(|(index, _)| menu_item_rect(&items, index, 1920.0, 1080.0))
            .collect();
        for pair in tiles.windows(2) {
            assert_eq!(pair[0][1], pair[1][1], "tiles share a line");
            assert!(pair[1][0] > pair[0][0] + pair[0][2], "and sit side by side");
        }
        let column: Vec<[f32; 4]> = (0..items.len() - 1)
            .filter(|index| !items[*index].is_tile() || *index == 0)
            .map(|index| menu_item_rect(&items, index, 1920.0, 1080.0))
            .collect();
        for pair in column.windows(2) {
            assert!(
                pair[1][1] >= pair[0][1] + pair[0][3],
                "{:?} runs into {:?}",
                pair[0],
                pair[1]
            );
        }
        // And the power button is still in the corner, out of the stack.
        let power = menu_item_rect(&items, items.len() - 1, 1920.0, 1080.0);
        assert_eq!(power, power_button_rect(1920.0, 1080.0));

        // A machine with neither bar has neither row, and Resume is back
        // directly under the tile line.
        let plain = Guide::default();
        assert_eq!(
            plain.items(true),
            vec![
                Item::Mixer,
                Item::Resume,
                Item::Close,
                Item::Dashboard,
                Item::Power
            ]
        );
        let plain_items = plain.items(true);
        assert_eq!(
            menu_item_rect(&plain_items, 0, 1920.0, 1080.0),
            menu_item_rect(&items, 0, 1920.0, 1080.0),
            "the tile line heads the column whichever rows follow it"
        );
        // Resume is a row lower there than the bar is here, by exactly the
        // rule that separates the quick settings from what the menu does —
        // with no bars, the tiles are that whole band on their own.
        let button = menu_item_rect(&plain_items, 1, 1920.0, 1080.0);
        let bar = menu_item_rect(&items, index_of(Item::Volume), 1920.0, 1080.0);
        assert_eq!(button[0], bar[0], "and every row is inset the same");
        assert!(bar[3] < button[3], "a bar's row is shorter than a button's");
    }

    /// The two tiles at the head of the column are square, side by side on one
    /// line, and inset like every other control on the panel.
    #[test]
    fn the_tiles_sit_side_by_side_at_the_head_of_the_column() {
        let mut guide = Guide::default();
        guide.set_pointer_control(true);
        guide.set_bars(BOTH_BARS);
        let items = guide.items(true);

        let pointer = menu_item_rect(&items, 0, 1920.0, 1080.0);
        let mixer = menu_item_rect(&items, 1, 1920.0, 1080.0);
        assert_eq!(items[0], Item::Pointer);
        assert_eq!(items[1], Item::Mixer);

        // Square, both of them, and the same square.
        assert!((pointer[2] - pointer[3]).abs() < 0.01, "{pointer:?}");
        assert_eq!([pointer[2], pointer[3]], [mixer[2], mixer[3]]);
        // Beside one another on one line, with air between.
        assert_eq!(pointer[1], mixer[1]);
        assert!(mixer[0] > pointer[0] + pointer[2], "{pointer:?} {mixer:?}");
        // And well short of the sidebar, unlike the rows below them.
        let volume = menu_item_rect(&items, 2, 1920.0, 1080.0);
        assert_eq!(pointer[0], volume[0], "inset like everything else");
        assert!(mixer[0] + mixer[2] < volume[0] + volume[2]);
        // The row under the tiles clears them rather than overlapping.
        assert!(volume[1] >= pointer[1] + pointer[3], "{volume:?}");
    }

    /// A switch has to say which of its two states it is in while the lit
    /// selection capsule is sitting on top of it, so being lit cannot be the
    /// signal. Being *filled* is.
    #[test]
    fn a_tile_says_whether_its_switch_is_on() {
        let mut base = Guide::default();
        base.set_pointer_control(true);
        base.open();
        base.backdate_open(2.0);

        // Whether there is an application to be about is the guide's own
        // answer, not the view's: it decides what the highlight will stop on
        // as well as how the tile is drawn, and the two must not disagree.
        let scene = |on: bool, target: bool| {
            let mut guide = Guide::default();
            guide.set_pointer_control(true);
            guide.set_pointer_target(target);
            guide.open();
            guide.backdate_open(2.0);
            build_guide(
                GuideView {
                    guide: &guide,
                    clock: None,
                    volume: None,
                    brightness: None,
                    stick_pointer: on,
                    app: Some("Celeste"),
                    close_target: None,
                    screen: None,
                    cards: &[],
                    highlight: None,
                    menu_highlight: None,
                    behind: 0.0,
                    card_age: guide.age(),
                    power: 0.0,
                    time: 0.0,
                    slots: &Named,
                },
                1920.0,
                1080.0,
            )
        };

        let items = base.items(false);
        let tile = menu_item_rect(&items, 0, 1920.0, 1080.0);
        let in_tile = move |q: &&Quad| {
            q.x >= tile[0] - 0.5
                && q.y >= tile[1] - 0.5
                && q.x + q.w <= tile[0] + tile[2] + 0.5
                && q.y + q.h <= tile[1] + tile[3] + 0.5
        };
        let glyph = |scene: &Scene| {
            scene
                .quads
                .iter()
                .filter(|q| q.slot == Named::slot_of(icons::POINTER_STICK))
                .find(in_tile)
                .copied()
                .expect("the tile's own glyph")
        };
        let panes = |scene: &Scene| {
            scene
                .quads
                .iter()
                .filter(|q| q.slot == SOLID_SLOT && q.thickness > 0.0)
                .filter(in_tile)
                .count()
        };

        // On: a second pane fills the chip, and the glyph is at full strength.
        assert_eq!(panes(&scene(true, true)), 2, "the fill, over the chip");
        assert_eq!(panes(&scene(false, true)), 1, "off, the chip alone");
        assert!(glyph(&scene(true, true)).color[3] > glyph(&scene(false, true)).color[3]);

        // And with nothing in front to be about, the tile is not a chip at
        // all: no glass, a hairline where the chip would be, and a ghost of
        // the glyph. A merely dimmer chip is what an *unselected* chip looks
        // like, and there are five of those beside it.
        let dead = scene(true, false);
        assert_eq!(panes(&dead), 0, "nothing to be on for");
        let outline = dead
            .quads
            .iter()
            .filter(|q| q.slot == SOLID_SLOT && q.border > 0.0)
            .find(in_tile)
            .expect("the outline standing in for the chip");
        assert_eq!(outline.thickness, 0.0, "an outline is not a slab");
        assert!(glyph(&scene(false, false)).color[3] < glyph(&scene(false, true)).color[3]);
    }

    /// The bug this exists for, in its second form: a tile handed its own chip
    /// over the instant the selection reached its *line*, because arrival was
    /// measured down the column only. The two tiles share a line, so the light
    /// was still crossing the gap between them with neither drawn under it.
    #[test]
    fn a_tile_keeps_its_chip_until_the_light_has_actually_reached_it() {
        let tile = [30.0, 200.0, 68.0, 68.0];
        let beside = [112.0, 200.0, 68.0, 68.0];
        // Same line, one tile away: no light here yet.
        assert_eq!(highlight_arrival(beside, tile), 0.0);
        // Half way between them, and still mostly not.
        let between = [71.0, 200.0, 68.0, 68.0];
        assert!(highlight_arrival(between, tile) < 0.5);
        // Sitting on it.
        assert!(highlight_arrival(tile, tile) > 0.99);

        // And a full-width row's capsule parked on a tile's corner is not
        // light on that tile either: it has not become a tile yet.
        let row = [30.0, 200.0, 390.0, 66.0];
        assert!(highlight_arrival(row, tile) < 0.1);

        // The column of rows behaves exactly as it did: only y differs there.
        let below = [30.0, 276.0, 390.0, 66.0];
        assert_eq!(highlight_arrival(below, row), 0.0);
        let nudged = [30.0, 201.0, 390.0, 66.0];
        assert!(highlight_arrival(nudged, row) > 0.95);
    }

    /// The entrance the two tiles appeared not to have. They are the first
    /// two entries in the column, and the column used to start arriving the
    /// instant the menu opened — so they finished while the sidebar was still
    /// sliding in under them, and what the user saw was a slab arriving with
    /// two tiles already printed on it. Every entry now waits for the slab.
    #[test]
    fn the_column_arrives_on_to_a_sidebar_that_is_already_there() {
        // Barely anything has happened while the sidebar is still on its way.
        assert!(entry_appear(GUIDE_SLIDE * 0.5, 0) < 0.05, "half way in");
        // The first entry is still visibly arriving once it has settled.
        assert!(entry_appear(GUIDE_SLIDE, 0) < 0.95, "the top of the column");
        assert!(
            entry_appear(GUIDE_SLIDE, 1) < 0.95,
            "and the tile beside it"
        );
        // Which is what the rows further down the column always did.
        assert!(entry_appear(GUIDE_SLIDE, 4) < entry_appear(GUIDE_SLIDE, 0));
        // And they still arrive in order, and all of them do arrive.
        assert!(entry_appear(0.4, 0) > entry_appear(0.4, 1));
        assert_eq!(entry_appear(1.0, 7), 1.0);
    }

    /// A switch is a physical thing: it goes down under the thumb, comes back
    /// past where it started, and settles at exactly its own size.
    #[test]
    fn a_pressed_tile_goes_down_and_springs_back() {
        assert_eq!(press_scale(0.0), 1.0);
        let bottom = press_scale(PRESS_DOWN);
        assert!(
            (bottom - (1.0 - PRESS_DIP)).abs() < 1e-3,
            "fully down at the turn: {bottom}"
        );
        assert!(press_scale(0.15) < 1.0 && press_scale(0.15) > bottom);

        // Past its own size on the way back out, and settled on it at the end.
        let overshoot = (5..10)
            .map(|n| press_scale(n as f32 / 10.0))
            .fold(0.0, f32::max);
        assert!(overshoot > 1.0, "it springs past the top: {overshoot}");
        assert!((press_scale(0.999) - 1.0).abs() < 0.01);
        // Outside the press it is simply itself.
        assert_eq!(press_scale(1.0), 1.0);
        assert_eq!(press_scale(4.0), 1.0);

        // The whole tile moves together — chip, fill and glyph — so that a
        // press reads as a button going down rather than as a hole opening
        // behind a glyph that stayed put.
        let mut guide = Guide::default();
        guide.set_pointer_control(true);
        guide.set_pointer_target(true);
        guide.open();
        guide.backdate_open(2.0);
        let items = guide.items(false);
        let at_rest = guide_scene(&guide, Some("Celeste"), None, &[], None);
        guide.press(Item::Pointer);
        // At the bottom of the travel, where the difference is largest.
        guide.backdate_press(crate::guide::PRESS_TIME * PRESS_DOWN);
        let pressed = guide_scene(&guide, Some("Celeste"), None, &[], None);

        let tile = menu_item_rect(&items, 0, 1920.0, 1080.0);
        let inside = |scene: &Scene| -> Vec<Quad> {
            scene
                .quads
                .iter()
                .filter(|q| {
                    q.x >= tile[0] - 1.0
                        && q.y >= tile[1] - 1.0
                        && q.x + q.w <= tile[0] + tile[2] + 1.0
                        && q.y + q.h <= tile[1] + tile[3] + 1.0
                })
                .copied()
                .collect()
        };
        let resting = inside(&at_rest);
        let sinking = inside(&pressed);
        assert!(!resting.is_empty(), "the tile draws something at rest");
        assert_eq!(resting.len(), sinking.len(), "the same parts, smaller");
        for (rest, sink) in resting.iter().zip(&sinking) {
            assert!(sink.w < rest.w, "{:?} did not go down", sink.slot);
            // Down about its own middle, not towards a corner.
            let centre = |q: &Quad| (q.x + q.w * 0.5, q.y + q.h * 0.5);
            let (rx, ry) = centre(rest);
            let (sx, sy) = centre(sink);
            assert!((rx - sx).abs() < 0.01 && (ry - sy).abs() < 0.01);
        }
    }

    /// The mixer is a place kept in the column for a control that has still to
    /// be written, so it is drawn and it is never on.
    #[test]
    fn the_mixer_tile_is_drawn_and_inert() {
        let mut guide = Guide::default();
        guide.open();
        guide.backdate_open(2.0);
        let scene = guide_scene(&guide, Some("Celeste"), None, &[], None);

        let items = guide.items(false);
        let mixer = menu_item_rect(&items, 0, 1920.0, 1080.0);
        assert_eq!(items[0], Item::Mixer);
        assert!(
            scene.quads.iter().any(|q| {
                q.slot != SOLID_SLOT
                    && (q.x - mixer[0]).abs() < mixer[2]
                    && (q.y - mixer[1]).abs() < mixer[3]
            }),
            "the mixer tile carries a glyph"
        );
        assert!(
            scene.texts.iter().all(|text| !text.content.is_empty()),
            "and no empty label was written for it"
        );
    }

    /// A bar is drawn as a track with the value filled in, and the fill is the
    /// only part of the row that moves.
    #[test]
    fn a_bar_fills_its_track_in_proportion_to_where_it_stands() {
        let mut guide = Guide::default();
        guide.set_bars(BOTH_BARS);
        guide.open();
        guide.backdate_open(2.0);
        let scene = guide_scene(&guide, Some("Celeste"), None, &[], None);

        let items = guide.items(false);
        let row = |index: usize| menu_item_rect(&items, index, 1920.0, 1080.0);
        // Everything laid inside a row, ignoring the chip it rests on.
        let inside = |[rx, ry, rw, rh]: [f32; 4]| -> Vec<&Quad> {
            scene
                .quads
                .iter()
                .filter(|q| {
                    q.x >= rx
                        && q.y >= ry
                        && q.x + q.w <= rx + rw + 0.5
                        && q.y + q.h <= ry + rh + 0.5
                        && q.w < rw * 0.99
                })
                .collect()
        };

        // The track is the widest thing in the row; the fill is the widest
        // thing that starts where it does and stops short of its end.
        let track_of = |parts: &[&Quad]| -> (f32, f32) {
            let track = parts
                .iter()
                .max_by(|a, b| a.w.total_cmp(&b.w))
                .expect("a track");
            let fill = parts
                .iter()
                .filter(|q| (q.x - track.x).abs() < 0.5 && q.w < track.w)
                .max_by(|a, b| a.w.total_cmp(&b.w))
                .expect("a fill");
            (track.w, fill.w)
        };

        // Looked up by entry rather than by row number: the tiles above them
        // mean the bars are no longer the first two things in the column.
        let index_of = |wanted: Item| items.iter().position(|item| *item == wanted).unwrap();
        let (volume_track, volume_fill) = track_of(&inside(row(index_of(Item::Volume))));
        let (bright_track, bright_fill) = track_of(&inside(row(index_of(Item::Brightness))));
        assert!(
            (volume_fill / volume_track - 0.35).abs() < 0.02,
            "volume at {}",
            volume_fill / volume_track
        );
        assert!(
            (bright_fill / bright_track - 0.85).abs() < 0.02,
            "brightness at {}",
            bright_fill / bright_track
        );
        // The two tracks are the same run of pixels, so the bars can be
        // compared to each other at a glance.
        assert!((volume_track - bright_track).abs() < 0.5);
    }

    /// Silencing a session does not turn it down, so the bar must not empty:
    /// unmuting would then look like it had also put the volume back up.
    #[test]
    fn muting_dims_the_bar_and_crosses_out_the_speaker_without_emptying_it() {
        let mut guide = Guide::default();
        guide.set_bars(crate::guide::Bars {
            volume: true,
            brightness: false,
        });
        guide.open();
        guide.backdate_open(2.0);

        let at = |muted: bool| {
            build_guide(
                GuideView {
                    guide: &guide,
                    clock: None,
                    volume: Some(Level { value: 0.4, muted }),
                    brightness: None,
                    stick_pointer: false,
                    app: None,
                    close_target: None,
                    screen: None,
                    cards: &[],
                    highlight: None,
                    menu_highlight: None,
                    behind: 0.0,
                    card_age: guide.age(),
                    power: 0.0,
                    time: 0.0,
                    slots: &Named,
                },
                1920.0,
                1080.0,
            )
        };
        let items = guide.items(false);
        let volume = items.iter().position(|item| *item == Item::Volume).unwrap();
        let row = menu_item_rect(&items, volume, 1920.0, 1080.0);
        let in_row = move |q: &&Quad| q.y > row[1] && q.y + q.h <= row[1] + row[3] + 0.5;

        // `Named` hands out one slot per glyph, so the icon in the row says
        // which drawing the layout asked for.
        let glyph = |scene: &Scene| {
            scene
                .quads
                .iter()
                .filter(|q| q.slot != SOLID_SLOT)
                .find(in_row)
                .map(|q| q.slot)
                .expect("the speaker")
        };
        assert_eq!(glyph(&at(false)), Named::slot_of(icons::VOLUME));
        assert_eq!(glyph(&at(true)), Named::slot_of(icons::VOLUME_MUTED));

        // How far the fill runs, and how brightly.
        let fill = |scene: &Scene| {
            let parts: Vec<&Quad> = scene
                .quads
                .iter()
                .filter(|q| q.slot == SOLID_SLOT && q.radius > 0.0)
                .filter(in_row)
                .collect();
            let track = parts.iter().max_by(|a, b| a.w.total_cmp(&b.w)).unwrap();
            let fill = parts
                .iter()
                .filter(|q| (q.x - track.x).abs() < 0.5 && q.w < track.w)
                .max_by(|a, b| a.w.total_cmp(&b.w))
                .expect("a fill");
            (fill.w, fill.color[3])
        };
        let (loud_w, loud_a) = fill(&at(false));
        let (quiet_w, quiet_a) = fill(&at(true));
        assert!((loud_w - quiet_w).abs() < 0.5, "the fill stays where it is");
        assert!(quiet_a < loud_a * 0.5, "and goes quiet instead of emptying");
    }

    /// The rule groups what the menu does to the application apart from what
    /// it does to the session, and the row below it is pushed clear.
    #[test]
    fn a_faint_rule_separates_the_application_entries() {
        let guide = Guide::default();
        let items = guide.items(true);
        let rules = menu_separator_rects(&items, 1920.0, 1080.0);
        // Two, with no bars: under the tiles, and under the entries about the
        // application. The second is the one this is about.
        assert_eq!(rules.len(), 2);
        let rule = *rules.last().expect("the column has a rule");
        let dashboard = items
            .iter()
            .position(|item| *item == Item::Dashboard)
            .unwrap();
        let above = menu_item_rect(&items, dashboard - 1, 1920.0, 1080.0);
        let below = menu_item_rect(&items, dashboard, 1920.0, 1080.0);
        assert!(rule[1] > above[1] + above[3], "the rule is below Close");
        assert!(rule[1] < below[1], "and above Dashboard");
        assert!(rule[3] <= 2.0, "a hairline, not a border");

        let mut guide = Guide::default();
        guide.open();
        guide.backdate_open(2.0);
        let cards = [card("Celeste")];
        let scene = guide_scene(&guide, Some("Celeste"), None, &cards, None);
        let drawn = scene
            .quads
            .iter()
            .find(|q| (q.y - rule[1]).abs() < 0.5 && q.h <= 2.0)
            .expect("the rule should be drawn");
        assert!(drawn.color[3] < 0.2, "barely visible: {}", drawn.color[3]);
    }

    /// The modal dims everything behind it, including the sidebar's own text —
    /// which a scrim quad cannot do, because every quad is drawn under every
    /// text run.
    #[test]
    fn the_power_dialog_dims_what_it_covers_and_offers_four_choices() {
        let mut guide = Guide::default();
        guide.open();
        guide.backdate_open(2.0);
        let plain = guide_scene(&guide, Some("Celeste"), None, &[], None);
        let title_alpha = |scene: &Scene| {
            scene
                .texts
                .iter()
                .find(|t| t.content == "15:18")
                .map(|t| t.color[3])
                .expect("the sidebar header")
        };

        guide.open_power();
        let modal = guide_scene(&guide, Some("Celeste"), None, &[], None);
        assert!(
            title_alpha(&modal) < title_alpha(&plain) * 0.5,
            "the sidebar behind the dialog should be dimmed"
        );

        for label in [
            "Power",
            "Suspend System",
            "Turn Off System",
            "Exit Linboard Shell",
            "Cancel",
        ] {
            assert!(
                modal.texts.iter().any(|t| t.content == label),
                "{label:?} missing from the dialog"
            );
        }
        // Centred on the display, not tucked into the sidebar.
        let panel = modal
            .texts
            .iter()
            .find(|t| t.content == "Power")
            .expect("title");
        assert!((panel.x + panel.max_width * 0.5 - 960.0).abs() < 1.0);
    }

    /// The dialog leaves the button it belongs to, the way a folder leaves
    /// its icon: no bigger than the button to begin with, on the button, and
    /// keeping its own proportions the whole way out — which is what lets its
    /// contents ride the same single factor.
    #[test]
    fn the_power_dialog_grows_out_of_its_button() {
        let (width, height, rows) = (1920.0, 1080.0, 4);
        let button = power_button_rect(width, height);
        let settled = power_dialog_rect(width, height, rows);

        let shut = power_dialog_bounds(width, height, rows, 0.0);
        assert!(
            (shut[2] - button[2]).abs() < 0.5,
            "starts the button's width"
        );
        let centre = |rect: [f32; 4]| [rect[0] + rect[2] * 0.5, rect[1] + rect[3] * 0.5];
        assert!((centre(shut)[0] - centre(button)[0]).abs() < 0.5);
        assert!((centre(shut)[1] - centre(button)[1]).abs() < 0.5);

        let open = power_dialog_bounds(width, height, rows, 1.0);
        for (grown, want) in open.iter().zip(&settled) {
            assert!(
                (grown - want).abs() < 1e-3,
                "{open:?} should end at {settled:?}"
            );
        }

        // Never a differently shaped dialog on the way, and never travelling
        // backwards.
        let aspect = settled[2] / settled[3];
        let mut last = shut[2];
        for step in 1..=10 {
            let mid = power_dialog_bounds(width, height, rows, step as f32 / 10.0);
            assert!(
                (mid[2] / mid[3] - aspect).abs() < 1e-3,
                "{mid:?} is misshapen"
            );
            assert!(mid[2] > last, "and should only ever grow");
            last = mid[2];
        }
    }

    /// Opening it is one movement out of the button, so nothing inside may
    /// arrive before the panel is big enough to hold it — and what is behind
    /// steps back over the same stretch rather than the instant it is asked
    /// for.
    #[test]
    fn the_dialogs_contents_wait_for_a_panel_that_can_hold_them() {
        let mut guide = Guide::default();
        guide.open();
        guide.backdate_open(2.0);
        guide.open_power();

        let at = |power: f32| {
            build_guide(
                GuideView {
                    guide: &guide,
                    clock: Some(Clock {
                        time: "15:18",
                        date: "Tue 5 Aug",
                    }),
                    volume: None,
                    brightness: None,
                    stick_pointer: false,
                    app: Some("Celeste"),
                    close_target: None,
                    screen: None,
                    cards: &[],
                    highlight: None,
                    menu_highlight: None,
                    behind: 0.0,
                    card_age: guide.age(),
                    power,
                    time: 0.0,
                    slots: &AllSlots,
                },
                1920.0,
                1080.0,
            )
        };
        let alpha = |scene: &Scene, label: &str| {
            scene
                .texts
                .iter()
                .find(|text| text.content == label)
                .map(|text| text.color[3])
                .unwrap_or(0.0)
        };

        // A panel the size of the button has no room for a label.
        assert_eq!(alpha(&at(0.15), "Cancel"), 0.0);
        assert!(alpha(&at(1.0), "Cancel") > 0.5);

        // The sidebar dims as the panel comes out over it, not before.
        assert!(alpha(&at(0.15), "15:18") > alpha(&at(1.0), "15:18"));
    }

    /// The bug this exists to prevent: the start card is a miniature of the
    /// *bar*, a scene of its own, and its labels printed straight through the
    /// dialog's panel because every quad is drawn before every text run. A
    /// scene put behind the modal has to give up the text the panel covers.
    #[test]
    fn text_behind_the_dialog_panel_is_dropped_not_merely_dimmed() {
        let rows = 4;
        let [px, py, pw, ph] = power_dialog_rect(1920.0, 1080.0, rows);
        let under = Text {
            content: "KDE System Settings".to_string(),
            x: px + pw * 0.3,
            y: py + ph * 0.5,
            size: 20.0,
            color: [1.0; 4],
            bold: false,
            max_width: 300.0,
            align: TextAlign::Left,
        };
        let beside = Text {
            content: "Start screen".to_string(),
            x: px - 400.0,
            y: py + ph * 0.5,
            size: 20.0,
            color: [1.0; 4],
            bold: false,
            max_width: 300.0,
            align: TextAlign::Left,
        };

        let mut scene = Scene {
            quads: Vec::new(),
            texts: vec![under, beside],
        };
        recede_behind_dialog(&mut scene, 1920.0, 1080.0, rows, 1.0);

        let left: Vec<&str> = scene.texts.iter().map(|t| t.content.as_str()).collect();
        assert_eq!(left, ["Start screen"], "only what the panel covers goes");
        assert!(scene.texts[0].color[3] < 0.5, "and the rest steps back");
    }

    /// The sidebar is the menu's home; nothing it says may leak under the
    /// cards to its right.
    #[test]
    fn sidebar_text_stays_inside_the_sidebar() {
        let mut guide = Guide::default();
        guide.open();
        let scene = guide_scene(&guide, Some("Celeste"), Some("DP-2"), &[], None);

        let sidebar_w = overview::sidebar_width(1920.0) as f32;
        for text in &scene.texts {
            if text.content == "Nothing is running" {
                continue; // the empty-state message lives in the card region
            }
            assert!(
                text.x + text.max_width <= sidebar_w + 1.0,
                "{:?} reaches x={} past the sidebar at {sidebar_w}",
                text.content,
                text.x + text.max_width
            );
        }
    }

    /// The frames and titles must land exactly on the rectangles the cards
    /// arrive with — the same ones the compositor scales the windows to.
    #[test]
    fn cards_are_framed_and_titled_at_their_rectangles() {
        let mut guide = Guide::default();
        guide.open();
        let mut cards = [card("Celeste"), card("Files")];
        lay_out(&mut cards, 0, 1920.0, 1080.0);
        let scene = guide_scene(&guide, Some("Celeste"), None, &cards, None);

        for (index, this) in cards.iter().enumerate() {
            let [x, y, w, h] = this.rect;
            // Top edge of the hairline frame.
            assert!(
                scene.quads.iter().any(|q| (q.x - x).abs() < 0.5
                    && (q.y - y).abs() < 0.5
                    && (q.w - w).abs() < 1.0),
                "card {index} has no frame at its rectangle"
            );
            // Its title, just under the card.
            assert!(
                scene
                    .texts
                    .iter()
                    .any(|t| t.content == this.title && t.y > y + h),
                "card {index} is missing its title below the card"
            );
        }
    }

    /// A card fully past the screen edge is skipped outright — no frame, no
    /// title floating in from nowhere — while a merely cut-off one is drawn.
    #[test]
    fn cards_fully_off_screen_are_not_drawn() {
        let mut guide = Guide::default();
        guide.open();
        guide.backdate_open(2.0);
        let mut cards = [card("Above"), card("Peeking"), card("Selected")];
        lay_out(&mut cards, 2, 1920.0, 1080.0);
        assert!(
            cards[0].rect[1] + cards[0].rect[3] <= 0.0,
            "with the last card selected the first should be fully above"
        );

        let scene = guide_scene(&guide, Some("Selected"), None, &cards, None);
        assert!(scene.texts.iter().all(|t| t.content != "Above"));
        assert!(scene.texts.iter().any(|t| t.content == "Peeking"));
    }

    /// Every full-width entry is a rounded chip, and the selected one is drawn
    /// from the eased rectangle rather than at its row — that is what lets it
    /// slide.
    #[test]
    fn menu_entries_are_rounded_and_the_selection_rides_its_eased_chip() {
        let mut guide = Guide::default();
        guide.open();
        guide.backdate_open(2.0);

        // A window beside the column, so that there are three full-width rows
        // for the glide to run between: the tiles at the head of the column
        // are neither capsules nor the width this is measuring.
        let closable = true;
        let cards = [card("Celeste")];
        let sidebar_w = overview::sidebar_width(1920.0) as f32;
        let settled = guide_scene(&guide, Some("Celeste"), None, &cards, None);
        // Capsules: radius exactly half their height, which is what tells a
        // button apart from the panel it rests on.
        let capsules = |scene: &Scene| -> Vec<[f32; 2]> {
            scene
                .quads
                .iter()
                .filter(|q| {
                    (q.radius - q.h * 0.5).abs() < 0.01 && q.w > sidebar_w * 0.5 && q.fade > 0.001
                })
                .map(|q| [q.y, q.h])
                .collect()
        };
        // The full-width rows: everything but the tiles, which share a line
        // and are square, and the power button, which has left the column for
        // the sidebar's corner.
        let items = guide.items(closable);
        let rows = items
            .iter()
            .filter(|item| !item.is_tile() && **item != Item::Power)
            .count();
        assert_eq!(
            capsules(&settled).len(),
            rows,
            "settled, every row shows one chip and the selected one is lit"
        );

        // Mid-glide the chip sits between rows, and the accent is drawn there
        // rather than snapped to the row it is heading for.
        let index_of = |wanted: Item| items.iter().position(|item| *item == wanted).unwrap();
        let first = menu_item_rect(&items, index_of(Item::Close), 1920.0, 1080.0);
        let second = menu_item_rect(&items, index_of(Item::Dashboard), 1920.0, 1080.0);
        let between = [first[0], (first[1] + second[1]) / 2.0, first[2], first[3]];
        let gliding = build_guide(
            GuideView {
                guide: &guide,
                clock: Some(Clock {
                    time: "15:18",
                    date: "Tue 5 Aug",
                }),
                volume: None,
                brightness: None,
                stick_pointer: false,
                app: Some("Celeste"),
                close_target: Some("Celeste"),
                screen: None,
                cards: &[],
                highlight: None,
                menu_highlight: Some(between),
                behind: 0.0,
                card_age: guide.age(),
                power: 0.0,
                time: 0.0,
                slots: &AllSlots,
            },
            1920.0,
            1080.0,
        );
        assert!(
            gliding
                .quads
                .iter()
                .any(|q| (q.y - between[1]).abs() < 0.5 && q.radius > 0.0),
            "the selected chip should be drawn where the glide has reached"
        );

        // The bug: the selected row gave up its own capsule the instant it was
        // selected, so until the light arrived the column had a hole in it.
        // Every row still shows a button, plus the lit one in between.
        let mid = capsules(&gliding);
        assert_eq!(
            mid.len(),
            rows + 1,
            "mid-glide the row being left for keeps its own chip: {mid:?}"
        );
        assert!(
            mid.iter().any(|[y, _]| (y - first[1]).abs() < 0.5),
            "and the row the light is arriving at is one of them"
        );

        // It is handed over as the light lands, not before: with the chip a
        // whisker from the row, the row's own is nearly gone.
        let nearly = [first[0], first[1] + first[3] * 0.02, first[2], first[3]];
        assert!(highlight_arrival(nearly, first) > 0.95);
        assert_eq!(
            highlight_arrival(second, first),
            0.0,
            "a row away, untouched"
        );
    }

    /// Rounded corners are what the cards are framed with now, so the frame
    /// has to be one ring rather than four straight runs.
    #[test]
    fn card_frames_are_rounded_rings() {
        let mut guide = Guide::default();
        guide.open();
        guide.backdate_open(2.0);
        let mut cards = [card("Celeste")];
        lay_out(&mut cards, 0, 1920.0, 1080.0);
        let scene = guide_scene(&guide, Some("Celeste"), None, &cards, None);

        let [x, y, w, h] = cards[0].rect;
        let frame = scene
            .quads
            .iter()
            .find(|q| (q.x - x).abs() < 0.5 && (q.y - y).abs() < 0.5 && (q.w - w).abs() < 0.5)
            .expect("the card should be framed at its rectangle");
        assert!(frame.radius > 0.0, "the frame should be rounded");
        assert!(frame.border > 0.0, "and drawn as an outline, not a fill");
        assert!((frame.h - h).abs() < 0.5);
    }

    /// The sidebar is one slab of glass over the wallpaper, and it has to bend
    /// the wallpaper *as the layer below is drawing it* — the backdrop under
    /// the menu is blurred, and a pane refracting a sharp copy of it would
    /// show a crisp rectangle of wallpaper against a soft one.
    #[test]
    fn the_sidebar_refracts_the_backdrop_it_is_laid_over() {
        let mut guide = Guide::default();
        guide.open();
        guide.backdate_open(2.0);

        let scene = build_guide(
            GuideView {
                guide: &guide,
                clock: Some(Clock {
                    time: "15:18",
                    date: "Tue 5 Aug",
                }),
                volume: None,
                brightness: None,
                stick_pointer: false,
                app: Some("Celeste"),
                close_target: None,
                screen: None,
                cards: &[],
                highlight: None,
                menu_highlight: None,
                behind: 0.8,
                card_age: guide.age(),
                power: 0.0,
                time: 0.0,
                slots: &AllSlots,
            },
            1920.0,
            1080.0,
        );

        let [px, py, pw, ph] = sidebar_panel_rect(1920.0, 1080.0);
        let panel = scene
            .quads
            .iter()
            .find(|q| (q.w - pw).abs() < 1.0 && (q.h - ph).abs() < 1.0)
            .expect("the sidebar should be one pane");
        assert!(
            panel.thickness > 0.0,
            "and it should be a slab, not a rectangle"
        );
        assert!(
            panel.frost > 0.0,
            "frosted, so a label on it is legible over anything"
        );
        assert_eq!(panel.behind, 0.8, "bending the wallpaper it is actually on");
        assert!(panel.gloss > 0.0, "with a lit edge");

        // Floating, not pinned: glass only reads as a layer if what it is laid
        // over runs past it.
        assert!(px > 0.0 && py > 0.0);
        assert!(px + pw < overview::sidebar_width(1920.0) as f32);
        assert!(py + ph < 1080.0);
    }

    /// The busiest frame the shell can put up has to fit inside the renderer's
    /// snapshot budget.
    ///
    /// Running out of it does not fail, it *degrades*: panes past the budget
    /// share an older snapshot and show the frame as it stood before the thing
    /// they are lying on was drawn. What that looks like is glass that has
    /// stopped frosting — the dialog's rows showing the start screen's icons
    /// sharply, straight through a panel that should have blurred them away.
    /// Nothing about that says "budget", so it is asserted here instead of
    /// waiting to be noticed.
    #[test]
    fn the_deepest_screen_fits_inside_the_snapshot_budget() {
        // The bar, shrunk into its card, with the guide over it and the power
        // dialog over that — the same order the renderer is handed.
        let xmb = Xmb::new(vec![Category {
            id: "a",
            title: "Settings",
            icon: "a",
            apps: vec![app("first"), app("second"), app("third")],
        }]);
        let (width, height) = (1920.0, 1080.0);
        let mut scene = build_with(&xmb, &Cursor::new(1), width, height, true, &AllSlots);
        scene.place_into([900.0, 200.0, 900.0, 520.0], width, height);

        let mut guide = Guide::default();
        guide.set_bars(BOTH_BARS);
        guide.open();
        guide.backdate_open(2.0);
        guide.open_power();
        let cards = [card("Celeste")];
        let over = guide_scene(&guide, Some("Celeste"), None, &cards, None);
        scene.quads.extend(over.quads);

        let needed = crate::gpu::snapshots_needed(&scene.quads);
        assert!(
            needed <= crate::gpu::MAX_GLASS_BATCHES,
            "the deepest screen needs {needed} snapshots and the budget is {}",
            crate::gpu::MAX_GLASS_BATCHES,
        );
    }

    /// The category row is cut from squircles.
    ///
    /// Two things make one, and a shape needs both: a radius that reaches the
    /// whole half-width, and a corner cut to a norm above the circle's. With
    /// only the first it is a disc, and with only the second it is a rounded
    /// rectangle with a very slightly nicer corner. The pairing is the thing
    /// worth holding onto, because either half looks like a typo on its own.
    #[test]
    fn the_category_row_stands_on_squircles() {
        let xmb = Xmb::new(vec![Category {
            id: "a",
            title: "Settings",
            icon: "a",
            apps: vec![app("first")],
        }]);
        let cursor = Cursor::new(1);
        let (width, height) = (1920.0, 1080.0);
        let scene = build_with(&xmb, &cursor, width, height, true, &AllSlots);

        let cross_y = height * 0.30;
        let tiles: Vec<&Quad> = scene
            .quads
            .iter()
            .filter(|q| q.radius > 0.0 && (q.y + q.h / 2.0 - cross_y).abs() < 1.0)
            .collect();
        assert!(!tiles.is_empty(), "the row should stand on something");

        for tile in tiles {
            assert_eq!(tile.corner, SQUIRCLE_CORNER, "a disc got left in the row");
            assert!((tile.w - tile.h).abs() < 0.01, "and it has to be square");
            assert!(
                (tile.radius - tile.w / 2.0).abs() < 0.01,
                "with the corner reaching the whole way",
            );
        }
    }

    /// Nothing else moved. A corner norm is the sort of thing that is easy to
    /// set once and then find on every shape in the interface.
    #[test]
    fn the_rest_of_the_shell_keeps_its_circular_corners() {
        let mut guide = Guide::default();
        guide.open();
        guide.backdate_open(2.0);
        let cards = [card("Celeste")];
        let scene = guide_scene(&guide, Some("Celeste"), None, &cards, None);

        for quad in &scene.quads {
            assert_eq!(
                quad.corner,
                crate::gpu::CIRCULAR_CORNER,
                "the guide should have no squircles in it",
            );
        }
    }

    /// One material, one shape language: everything the user can act on in
    /// the column is cut from the same glass, and its corners say which kind
    /// of control it is — a capsule for a row, a rounded square for a tile.
    #[test]
    fn every_control_in_the_column_has_the_corners_of_its_kind() {
        let mut guide = Guide::default();
        guide.set_pointer_control(true);
        // With an application in front, so every entry in the column is a
        // control the user can act on. One that cannot be is deliberately not
        // cut from this glass at all — see the tile tests.
        guide.set_pointer_target(true);
        guide.open();
        guide.backdate_open(2.0);
        let cards = [card("Celeste")];
        let scene = guide_scene(&guide, Some("Celeste"), None, &cards, None);

        let items = guide.items(true);
        for (index, item) in items.iter().enumerate() {
            let [rx, ry, _, rh] = menu_item_rect(&items, index, 1920.0, 1080.0);
            let control = scene
                .quads
                .iter()
                .find(|q| (q.x - rx).abs() < 1.0 && (q.y - ry).abs() < 1.0 && q.border == 0.0)
                .unwrap_or_else(|| panic!("{item:?} has no pane"));
            assert!(
                (control.radius - chip_radius(Some(*item), rh)).abs() < 0.01,
                "{item:?} is the wrong shape: radius {} of height {rh}",
                control.radius
            );
            // And a tile is a rounded square rather than a disc, which is what
            // a capsule's radius would make of something this shape.
            if item.is_tile() {
                assert!(control.radius < rh * 0.5, "{item:?} is a disc");
            }
            assert!(control.gloss > 0.0, "{item:?} should be lit like glass");
        }
    }

    /// Fading a scene has to fade the glass in it. A pane's colour alpha is
    /// how strongly it stains the wallpaper, and its lit rim is added by the
    /// shader on top of that — dim the tint and the pane is still there, just
    /// less purple. This was the start screen's card staying visible over an
    /// application it was supposed to be fading in behind.
    #[test]
    fn fading_a_scene_fades_the_glass_and_not_merely_its_tint() {
        let tint = [0.5, 0.3, 0.9, 0.55];
        let mut scene = Scene {
            quads: vec![Quad {
                w: 100.0,
                h: 40.0,
                slot: SOLID_SLOT,
                color: tint,
                radius: 20.0,
                thickness: 8.0,
                gloss: 1.0,
                ..Quad::default()
            }],
            texts: Vec::new(),
        };
        scene.fade(0.25);
        assert_eq!(scene.quads[0].fade, 0.25, "the pane should be fading out");
        assert_eq!(scene.quads[0].color, tint, "not merely losing its stain");
    }

    /// A slab shrinks with the scene it is part of, exactly as its corners do.
    ///
    /// Depth is measured in pixels, and a pane whose corners have been taken
    /// down to a quarter while its bevel stayed 22 pixels wide is not a
    /// miniature of anything — it is a card-sized rim with a dot of face in
    /// the middle of it.
    #[test]
    fn glass_is_miniaturised_along_with_the_scene_it_is_in() {
        let mut scene = Scene {
            quads: vec![Quad {
                x: 0.0,
                y: 0.0,
                w: 200.0,
                h: 80.0,
                slot: SOLID_SLOT,
                color: [1.0; 4],
                radius: 40.0,
                thickness: 9.0,
                gloss: 1.0,
                ..Quad::default()
            }],
            texts: Vec::new(),
        };
        scene.place_into([100.0, 50.0, 480.0, 270.0], 1920.0, 1080.0);

        let card = &scene.quads[0];
        let shrunk = 480.0 / 1920.0;
        assert!((card.thickness - 9.0 * shrunk).abs() < 1e-4);
        assert!(
            (card.thickness / card.radius - 9.0 / 40.0).abs() < 1e-4,
            "depth and corner have to shrink together"
        );
        assert!(card.gloss > 0.0, "and it is still lit glass");
    }

    /// The frame is lit from outside: rings beyond its edge, breathing, and
    /// never over the live window inside it — a glow centred on the card
    /// would wash out the very thing being chosen.
    #[test]
    fn the_selection_frame_is_haloed_outside_its_edge() {
        let mut guide = Guide::default();
        guide.open();
        guide.backdate_open(2.0);
        guide.move_focus(crate::guide::Move::Right, 1);
        let mut cards = [card("Celeste")];
        lay_out(&mut cards, 0, 1920.0, 1080.0);
        let [x, y, w, h] = cards[0].rect;

        let at = |time: f32| {
            build_guide(
                GuideView {
                    guide: &guide,
                    clock: Some(Clock {
                        time: "15:18",
                        date: "Tue 5 Aug",
                    }),
                    volume: None,
                    brightness: None,
                    stick_pointer: false,
                    app: Some("Celeste"),
                    close_target: Some("Celeste"),
                    screen: None,
                    cards: &cards,
                    highlight: Some(cards[0].rect),
                    menu_highlight: None,
                    behind: 0.0,
                    card_age: guide.age(),
                    power: 0.0,
                    time,
                    slots: &AllSlots,
                },
                1920.0,
                1080.0,
            )
        };

        let scene = at(0.0);
        let halo: Vec<&Quad> = scene
            .quads
            .iter()
            .filter(|q| q.border > 0.0 && q.x < x - 6.0 && q.y < y - 6.0)
            .collect();
        assert!(halo.len() >= 2, "the halo should be more than one ring");
        for ring in &halo {
            assert!(
                ring.x + ring.w > x + w && ring.y + ring.h > y + h,
                "a halo ring must enclose the frame, not sit inside it"
            );
            assert!(ring.color[3] < 0.2, "and stay faint: {}", ring.color[3]);
        }

        // It breathes: the same ring is dimmer half a period later.
        let brightest = |scene: &Scene| {
            scene
                .quads
                .iter()
                .filter(|q| q.border > 0.0 && q.x < x - 6.0 && q.y < y - 6.0)
                .map(|q| q.color[3])
                .fold(0.0f32, f32::max)
        };
        let peak = (0..24)
            .map(|step| brightest(&at(step as f32 * PULSE_PERIOD / 24.0)))
            .fold(0.0f32, f32::max);
        let trough = (0..24)
            .map(|step| brightest(&at(step as f32 * PULSE_PERIOD / 24.0)))
            .fold(f32::MAX, f32::min);
        assert!(
            peak > trough * 1.3,
            "the halo should pulse: {trough}..{peak}"
        );
    }

    /// The bug: the outline of the application being scaled out, drawn at the
    /// card it had not reached yet. The compositor carries the window there
    /// over [`crate::CARD_ARRIVAL`] seconds while this pass paints above it, so
    /// for every frame of that flight the card's rectangle is somewhere the
    /// window is not — and a frame, a title or a halo put there lands across
    /// the middle of the window instead of around it.
    #[test]
    fn nothing_is_drawn_on_a_card_until_its_window_has_arrived() {
        let mut guide = Guide::default();
        guide.open();
        guide.backdate_open(2.0);
        guide.move_focus(crate::guide::Move::Right, 1);
        let mut cards = [card("Celeste")];
        lay_out(&mut cards, 0, 1920.0, 1080.0);
        let [x, y, w, h] = cards[0].rect;

        let at = |card_age: f32| {
            build_guide(
                GuideView {
                    guide: &guide,
                    clock: None,
                    volume: None,
                    brightness: None,
                    stick_pointer: false,
                    app: Some("Celeste"),
                    close_target: Some("Celeste"),
                    screen: None,
                    cards: &cards,
                    highlight: Some(cards[0].rect),
                    menu_highlight: None,
                    behind: 0.0,
                    card_age,
                    power: 0.0,
                    time: 0.0,
                    slots: &AllSlots,
                },
                1920.0,
                1080.0,
            )
        };

        // Anything reaching the card's neighbourhood: its frame, the halo
        // rings outside it, and the title in the gap below.
        let near_card = |scene: &Scene| {
            let quads = scene
                .quads
                .iter()
                .filter(|q| q.x + q.w > x - 40.0 && q.x < x + w + 40.0 && q.y < y + h + 80.0)
                .map(|q| q.color[3]);
            let texts = scene
                .texts
                .iter()
                .filter(|t| t.x + t.max_width > x - 40.0 && t.y > y - 40.0)
                .map(|t| t.color[3]);
            quads.chain(texts).fold(0.0f32, f32::max)
        };

        for step in 0..=10 {
            let age = crate::CARD_ARRIVAL * step as f32 / 10.0;
            assert_eq!(
                near_card(&at(age)),
                0.0,
                "the card must stay bare while its window is still flying, at {age}s"
            );
        }
        assert!(
            near_card(&at(crate::CARD_ARRIVAL + 0.2)) > 0.5,
            "and be framed once it has landed"
        );
    }

    /// A scene shrunk into a card has to take its corner radii with it, or
    /// the miniature's rounding would be as big as the full-size screen's.
    #[test]
    fn placing_a_scene_scales_its_corner_radii() {
        let mut scene = Scene {
            quads: vec![Quad {
                x: 0.0,
                y: 0.0,
                w: 400.0,
                h: 200.0,
                slot: SOLID_SLOT,
                color: [1.0; 4],
                radius: 16.0,
                border: 4.0,
                notch: 8.0,
                ..Quad::default()
            }],
            texts: Vec::new(),
        };
        scene.place_into([100.0, 50.0, 480.0, 270.0], 1920.0, 1080.0);
        assert!((scene.quads[0].radius - 4.0).abs() < 1e-3);
        assert!((scene.quads[0].border - 1.0).abs() < 1e-3);
        assert!((scene.quads[0].notch - 2.0).abs() < 1e-3);
    }

    /// The start card is a miniature of the start screen, so what it shows
    /// has to be the bar itself, scaled — icons included.
    #[test]
    fn a_scene_placed_into_a_card_is_the_same_scene_shrunk() {
        let xmb = Xmb::new(vec![Category {
            id: "games",
            title: "Games",
            icon: "games",
            apps: vec![app("Celeste")],
        }]);
        let cursor = Cursor::new(1);
        let full = focused(&xmb, 1920.0, 1080.0, &AllSlots);
        // What the display itself shows: the row of category icons runs off
        // the screen, and those are the ones a card cannot keep.
        let on_screen: Vec<&Quad> = full
            .quads
            .iter()
            .filter(|q| q.x >= 0.0 && q.y >= 0.0 && q.x + q.w <= 1920.0 && q.y + q.h <= 1080.0)
            .collect();
        assert!(on_screen.len() > 1, "the bar should have icons to shrink");

        let mut mini = build(&xmb, &cursor, 1920.0, 1080.0, true, None, 0.0, &AllSlots);
        // A card a quarter of the display's width, at its aspect ratio.
        let card = [1200.0, 300.0, 480.0, 270.0];
        mini.place_into(card, 1920.0, 1080.0);

        assert_eq!(mini.quads.len(), on_screen.len(), "nothing else may drop");
        for (small, large) in mini.quads.iter().zip(&on_screen) {
            assert!((small.w - large.w * 0.25).abs() < 1e-3);
            assert!((small.x - (1200.0 + large.x * 0.25)).abs() < 1e-3);
            assert!((small.y - (300.0 + large.y * 0.25)).abs() < 1e-3);
        }
        // And all of it lands inside the card, so nothing pokes out past the
        // frame drawn around it.
        for quad in &mini.quads {
            assert!(quad.x >= card[0] - 0.5 && quad.x + quad.w <= card[0] + card[2] + 0.5);
            assert!(quad.y >= card[1] - 0.5 && quad.y + quad.h <= card[1] + card[3] + 0.5);
        }
        for text in &mini.texts {
            assert!(text.x >= card[0] - 0.5 && text.y >= card[1] - 0.5);
        }
    }

    /// Text draws above every quad in a scene, so a bar flying past the
    /// sidebar has to dissolve rather than print over it.
    #[test]
    fn text_reaching_the_sidebar_fades_out() {
        let mut scene = Scene {
            quads: Vec::new(),
            texts: vec![
                Text {
                    content: "under the panel".into(),
                    x: 40.0,
                    y: 0.0,
                    size: 20.0,
                    color: [1.0; 4],
                    bold: false,
                    max_width: 100.0,
                    align: TextAlign::Left,
                },
                Text {
                    content: "clear of it".into(),
                    x: 900.0,
                    y: 0.0,
                    size: 20.0,
                    color: [1.0; 4],
                    bold: false,
                    max_width: 100.0,
                    align: TextAlign::Left,
                },
            ],
        };
        scene.fade_text_before(400.0, 200.0);

        assert_eq!(scene.texts[0].color[3], 0.0, "should be gone by the panel");
        assert_eq!(scene.texts[1].color[3], 1.0, "should be untouched");
    }

    /// The eased highlight rectangle is drawn as a frame around the selected
    /// card, and dims (but stays) while focus is over in the menu column.
    #[test]
    fn the_card_highlight_follows_focus_between_the_panes() {
        let mut guide = Guide::default();
        guide.open();
        // Past the entrance animation, where everything is still faded out.
        guide.backdate_open(2.0);
        let cards = [card("Celeste")];
        let highlight = Some([700.0, 200.0, 800.0, 450.0]);

        let accent_alpha = |scene: &Scene| {
            scene
                .quads
                .iter()
                .filter(|q| q.color[2] > 0.9 && q.color[0] < 0.6 && q.slot == SOLID_SLOT)
                .map(|q| q.color[3])
                .fold(0.0_f32, f32::max)
        };

        let menu_pane = guide_scene(&guide, Some("Celeste"), None, &cards, highlight);
        guide.move_focus(crate::guide::Move::Right, cards.len());
        let cards_pane = guide_scene(&guide, Some("Celeste"), None, &cards, highlight);

        assert!(
            accent_alpha(&cards_pane) > accent_alpha(&menu_pane),
            "the frame should brighten when the cards take focus"
        );
        assert!(
            accent_alpha(&menu_pane) > 0.05,
            "but it must stay visible as the way back"
        );
    }

    // --- the on-screen keyboard --------------------------------------------

    fn board(width: f32, height: f32, board: &keyboard::Board) -> Scene {
        build_keyboard(
            KeyboardView {
                board,
                slots: &Named,
                // Settled, so the rise is not being raced.
                age: 10.0,
                behind: 0.0,
                time: 0.0,
            },
            width,
            height,
        )
    }

    /// Every key, in reading order, with where it sits.
    fn keys(width: f32, height: f32) -> Vec<(keyboard::Key, [f32; 4])> {
        (0..keyboard::ROW_COUNT)
            .flat_map(|row| {
                keyboard::row_keys(row)
                    .into_iter()
                    .enumerate()
                    .map(move |(column, key)| (key, keyboard_key_rect(row, column, width, height)))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// The hit test and the layout are one answer read two ways: the key
    /// under a point has to be the key drawn at that point, or the letter
    /// typed is not the letter clicked.
    #[test]
    fn the_key_under_the_cursor_is_the_key_drawn_there() {
        for (width, height) in [(1920.0, 1080.0), (1280.0, 800.0), (1080.0, 1920.0)] {
            for row in 0..keyboard::ROW_COUNT {
                for column in 0..keyboard::row_keys(row).len() {
                    let [x, y, w, h] = keyboard_key_rect(row, column, width, height);
                    // The middle of it, and each of its corners just inside.
                    for (at_x, at_y) in [
                        (x + w * 0.5, y + h * 0.5),
                        (x + 0.5, y + 0.5),
                        (x + w - 0.5, y + h - 0.5),
                    ] {
                        assert_eq!(
                            keyboard_key_at(at_x, at_y, width, height),
                            Some((row, column)),
                            "{width}x{height}: ({at_x}, {at_y}) is not {row},{column}"
                        );
                    }
                }
            }

            // And off the board is nothing rather than the nearest key: half
            // of being able to click accurately is being able to miss.
            let [px, py, pw, ph] = keyboard_panel_rect(width, height);
            assert_eq!(
                keyboard_key_at(px - 4.0, py + ph * 0.5, width, height),
                None
            );
            assert_eq!(
                keyboard_key_at(px + pw * 0.5, py - 4.0, width, height),
                None
            );
            assert_eq!(keyboard_key_at(0.0, 0.0, width, height), None);
            // Including the panel's own padding, which is a keyboard's frame.
            assert_eq!(keyboard_key_at(px + 1.0, py + 1.0, width, height), None);
        }
    }

    #[test]
    fn the_board_stays_on_the_display_and_its_keys_never_overlap() {
        // Including the shapes a handheld and a rotated monitor actually
        // present: the board shrinks to fit rather than losing its outer
        // columns, and a keyboard missing letters is not a keyboard.
        for (width, height) in [
            (1920.0, 1080.0),
            (3840.0, 2160.0),
            (1280.0, 800.0),
            (1280.0, 400.0),
            (1080.0, 1920.0),
        ] {
            let [px, py, pw, ph] = keyboard_panel_rect(width, height);
            assert!(px >= 0.0 && py >= 0.0, "{width}x{height}: panel off screen");
            assert!(
                px + pw <= width + 0.01 && py + ph <= height + 0.01,
                "{width}x{height}: panel runs off at {px}+{pw} / {py}+{ph}"
            );
            assert!(
                (px - (width - pw - px)).abs() < 0.01,
                "{width}x{height}: panel is not centred"
            );

            let laid_out = keys(width, height);
            for (key, rect) in &laid_out {
                let [x, y, w, h] = *rect;
                assert!(w > 0.0 && h > 0.0, "{width}x{height}: {key:?} has no size");
                assert!(
                    x >= px && y >= py && x + w <= px + pw + 0.01 && y + h <= py + ph + 0.01,
                    "{width}x{height}: {key:?} at {rect:?} is outside the panel"
                );
            }
            for (first, (left, a)) in laid_out.iter().enumerate() {
                for (right, b) in laid_out.iter().skip(first + 1) {
                    let apart = a[0] + a[2] <= b[0] + 0.01
                        || b[0] + b[2] <= a[0] + 0.01
                        || a[1] + a[3] <= b[1] + 0.01
                        || b[1] + b[3] <= a[1] + 0.01;
                    assert!(apart, "{width}x{height}: {left:?} and {right:?} overlap");
                }
            }
        }
    }

    #[test]
    fn every_key_is_drawn_and_labelled() {
        let mut model = keyboard::Board::default();
        let scene = board(1920.0, 1080.0, &model);
        let expected = keys(1920.0, 1080.0);
        // The keys that carry a drawing instead of a word: the four arrows and
        // the way out.
        let pictured = expected
            .iter()
            .filter(|(key, _)| key.glyph().is_some())
            .count();
        assert_eq!(pictured, 5, "four arrows and a way out");

        // One chip each, plus the panel behind them and the lit capsule and
        // glow over the selected one; Close gets an outline as well.
        assert!(
            scene.quads.len() >= expected.len() + 3,
            "{} quads for {} keys",
            scene.quads.len(),
            expected.len()
        );
        assert_eq!(
            scene.texts.len(),
            expected.len() - pictured,
            "every key that is not drawn carries exactly one cap"
        );

        // And each drawn key carries its own drawing: a cluster that pointed
        // the same way four times would be worse than none, and a way out that
        // looked like an arrow key would be pressed by accident.
        let drawn: Vec<u32> = scene
            .quads
            .iter()
            .filter(|quad| is_icon(quad))
            .map(|quad| quad.slot)
            .collect();
        assert_eq!(
            drawn,
            vec![
                Named::slot_of(icons::ARROW_LEFT),
                Named::slot_of(icons::ARROW_DOWN),
                Named::slot_of(icons::ARROW_UP),
                Named::slot_of(icons::ARROW_RIGHT),
                Named::slot_of(icons::KEYBOARD_HIDE),
            ]
        );

        // The caps say what the keys type, and Shift changes all of them at
        // once — which is the whole reason there is no second page of symbols.
        let caps: Vec<String> = scene.texts.iter().map(|t| t.content.clone()).collect();
        for wanted in [
            "q", "a", "z", "1", "`", "\\", "Esc", "F1", "F12", "Tab", "Caps", "Shift", "Ctrl",
            "Alt", "Space", "Back", "Enter",
        ] {
            assert!(caps.contains(&wanted.to_string()), "no {wanted} key");
        }
        assert!(
            !caps.contains(&"Close".to_string()),
            "the way out is drawn now, not lettered"
        );
        assert!(
            !caps.iter().any(|cap| cap.is_empty()),
            "a key was drawn with nothing on it"
        );

        model.arm_shift();
        let shifted: Vec<String> = board(1920.0, 1080.0, &model)
            .texts
            .iter()
            .map(|t| t.content.clone())
            .collect();
        for wanted in ["Q", "A", "Z", "!", "@", "?"] {
            assert!(
                shifted.contains(&wanted.to_string()),
                "no {wanted} on shift"
            );
        }
        assert!(
            !shifted.contains(&"q".to_string()),
            "a cap was left unshifted"
        );
    }

    #[test]
    fn the_selected_key_is_lit_where_it_stands() {
        let mut model = keyboard::Board::default();
        let scene = board(1920.0, 1080.0, &model);
        let (row, column) = model.selected();
        let [kx, ky, kw, kh] = keyboard_key_rect(row, column, 1920.0, 1080.0);

        // Exactly one filled pane sits on the selected key: the lit capsule.
        // Its own chip is left out precisely so there is not a second.
        let on_the_key = |scene: &Scene, [x, y, _, _]: [f32; 4]| -> Vec<[f32; 4]> {
            scene
                .quads
                .iter()
                .filter(|quad| quad.gloss == GLOSS_FULL && quad.border == 0.0)
                .filter(|quad| (quad.x - x).abs() < 0.01 && (quad.y - y).abs() < 0.01)
                .map(|quad| [quad.x, quad.y, quad.w, quad.h])
                .collect()
        };
        assert_eq!(
            on_the_key(&scene, [kx, ky, kw, kh]),
            vec![[kx, ky, kw, kh]],
            "the selected key should carry one lit capsule and no chip"
        );

        // And it travels with the cursor rather than staying put.
        model.move_selection(crate::guide::Move::Right);
        let (row, column) = model.selected();
        let next = keyboard_key_rect(row, column, 1920.0, 1080.0);
        let moved = board(1920.0, 1080.0, &model);
        assert!(next[0] > kx, "the cursor did not move");
        assert_eq!(on_the_key(&moved, next), vec![next]);
        assert!(
            on_the_key(&moved, [kx, ky, kw, kh]).is_empty(),
            "the capsule stayed behind on the key it left"
        );
    }

    #[test]
    fn the_board_rises_into_place_from_under_the_screens_edge() {
        let model = keyboard::Board::default();
        let [_, settled, panel_w, _] = keyboard_panel_rect(1920.0, 1080.0);
        // The slab itself, which is the only quad as wide as the panel.
        let slab_top = |scene: &Scene| {
            scene
                .quads
                .iter()
                .find(|quad| (quad.w - panel_w).abs() < 0.01)
                .expect("no panel")
                .y
        };

        let arriving = build_keyboard(
            KeyboardView {
                board: &model,
                slots: &Named,
                age: 0.0,
                behind: 0.0,
                time: 0.0,
            },
            1920.0,
            1080.0,
        );
        let top = slab_top(&arriving);
        assert!(
            top > settled,
            "the board should start below where it settles: {top} vs {settled}"
        );
        assert!(top >= 1080.0, "and off the bottom of the display entirely");

        assert!(
            (slab_top(&board(1920.0, 1080.0, &model)) - settled).abs() < 0.01,
            "it must land on its mark"
        );
    }

    #[test]
    fn the_hint_names_two_buttons_by_drawing_them() {
        let scene = build_keyboard_hint(
            HintView {
                slots: &Named,
                fade: 1.0,
                behind: 0.0,
            },
            1920.0,
            1080.0,
        );

        // The two glyphs, and no letter standing in for either: A/B/X/Y are
        // swapped between Xbox and Nintendo pads and absent from PlayStation
        // ones, so a lettered hint would be wrong on two layouts out of three.
        let drawn: Vec<u32> = scene
            .quads
            .iter()
            .filter(|quad| is_icon(quad))
            .map(|quad| quad.slot)
            .collect();
        assert_eq!(
            drawn,
            vec![
                Named::slot_of(icons::PAD_SELECT),
                Named::slot_of(icons::PAD_WEST)
            ],
            "the hint must draw the select button and the west face button"
        );
        let said: Vec<&str> = scene.texts.iter().map(|t| t.content.as_str()).collect();
        assert_eq!(said, vec!["+", "Keyboard"]);

        // In the bottom-right corner, clear of both edges, and everything it
        // draws inside its own chip.
        let [x, y, w, h] = keyboard_hint_rect(1920.0, 1080.0);
        assert!(x + w < 1920.0 && y + h < 1080.0, "the chip touches an edge");
        assert!(x > 1920.0 * 0.5 && y > 1080.0 * 0.5, "not in the corner");
        for quad in scene.quads.iter().filter(|quad| is_icon(quad)) {
            assert!(
                quad.x >= x && quad.x + quad.w <= x + w,
                "a glyph hangs out of the chip"
            );
        }
        let label = scene.texts.last().unwrap();
        assert!(
            label.x + label.max_width <= x + w + 0.01,
            "the label overruns"
        );
    }

    #[test]
    fn a_glyph_the_shell_could_not_load_leaves_a_gap_rather_than_an_app_icon() {
        // `slot_for` stands an application icon in for anything it cannot
        // find. In a row that means "press this button", that would read as
        // "press the app".
        struct NoGlyphs;
        impl SlotLookup for NoGlyphs {
            fn slot_for(&self, _icon: Option<&str>) -> Option<u32> {
                Some(99)
            }
            fn glyph(&self, _name: &str) -> Option<u32> {
                None
            }
        }
        let scene = build_keyboard_hint(
            HintView {
                slots: &NoGlyphs,
                fade: 1.0,
                behind: 0.0,
            },
            1920.0,
            1080.0,
        );
        assert!(
            !scene.quads.iter().any(|quad| quad.slot == 99),
            "the fallback application icon reached the hint"
        );
    }
}
