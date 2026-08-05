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
use crate::guide::{Guide, Item, Pane};
use crate::model::{Cursor, Xmb};
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
const ITEM_ICON_FOCUSED: f32 = 116.0;
/// The glass disc an icon stands on, as a multiple of the icon. The item's is
/// named because the launch splash grows out of exactly that disc; the
/// category's because the label under it has to clear it.
const ITEM_DISC: f32 = 1.34;
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
const GUIDE_PADDING: f32 = 44.0;
/// How long the sidebar takes to slide in, seconds.
const GUIDE_SLIDE: f32 = 0.28;
/// Where the entry column starts, below the header.
const GUIDE_ENTRIES_TOP: f32 = 220.0;
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

/// The entries arrive one after another: how long each waits behind the one
/// above it, and how long its own slide takes, in seconds.
const ENTRY_STAGGER: f32 = 0.045;
const ENTRY_SLIDE: f32 = 0.26;

/// How much of the lit capsule has arrived over the row at `rect`: 1 when it
/// is sitting on it, 0 while it is still a row away.
///
/// Measured in row heights rather than seconds, so it answers the only
/// question that matters — is there light on this row yet — however long the
/// glide takes and however many rows it crosses.
fn highlight_arrival(highlight: [f32; 4], rect: [f32; 4]) -> f32 {
    let span = rect[3].max(1.0);
    1.0 - ((highlight[1] - rect[1]).abs() / span).clamp(0.0, 1.0)
}

/// How far entry `index` has arrived, `age` seconds after the menu opened.
fn entry_appear(age: f32, index: usize) -> f32 {
    ease((age - index as f32 * ENTRY_STAGGER) / ENTRY_SLIDE)
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

    let row_height = GUIDE_ROW_HEIGHT * scale;
    let below_rule = separator_row(items).is_some_and(|rule| index >= rule);
    [
        panel_x + margin,
        GUIDE_ENTRIES_TOP * scale
            + index as f32 * row_height
            + if below_rule {
                GUIDE_SEPARATOR_GAP * scale
            } else {
                0.0
            }
            + 5.0 * scale,
        panel_w - margin * 2.0,
        row_height - 10.0 * scale,
    ]
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

/// The row the rule is drawn above: what the menu does to the application
/// ends there, and what it does to the session begins.
fn separator_row(items: &[Item]) -> Option<usize> {
    items.iter().position(|item| *item == Item::Dashboard)
}

/// The rule itself, in the same coordinates — `None` when the column has
/// nothing on both sides of it to separate.
fn menu_separator_rect(items: &[Item], width: f32, height: f32) -> Option<[f32; 4]> {
    let row = separator_row(items)?;
    let scale = guide_scale(height);
    let [x, y, w, _] = menu_item_rect(items, row, width, height);
    Some([
        x + w * 0.06,
        y - GUIDE_SEPARATOR_GAP * scale * 0.5,
        w * 0.88,
        (1.0 * scale).max(1.0),
    ])
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
}

/// Lay out one display's bar.
///
/// `focused` is whether this is the display the controller and keyboard are
/// driving. Every display draws its own [`Cursor`], so the others show what
/// they are pointing at, dimmed, rather than a copy of this one.
///
/// `hint` is the footer's control summary, which the caller composes because it
/// depends on how many displays there are. `time` runs the selection pulse.
#[allow(clippy::too_many_arguments)]
pub fn build(
    xmb: &Xmb,
    cursor: &Cursor,
    width: f32,
    height: f32,
    focused: bool,
    hint: &str,
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

    // Fixed text column: anchored to the focused icon's extent so labels do
    // not shuffle sideways as focus (and therefore icon size) moves around.
    let text_x = cross_x + ITEM_ICON_FOCUSED * scale / 2.0 + 30.0 * scale;
    let text_max = (width - text_x - 48.0 * scale).max(0.0);

    // --- applications of the selected category ---------------------------
    // Drawn first so the category row overlaps them, as on the real bar.
    if let Some(category) = cursor.current_category(xmb) {
        if category.apps.is_empty() && column_alpha > 0.01 {
            texts.push(Text {
                content: "No applications in this category".to_string(),
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
            // And the column dissolves before the screen edges, so its last
            // visible row can never print over the clock or the control hints.
            let fade_range = 70.0 * scale;
            alpha *= ((y - 90.0 * scale) / fade_range).clamp(0.0, 1.0);
            alpha *= ((height - 96.0 * scale - y) / fade_range).clamp(0.0, 1.0);
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

    // --- footer ----------------------------------------------------------
    // Only the display taking input explains the controls; repeating them on
    // every screen is noise, and their absence is another cue about which one
    // is live.
    if focused {
        let hint_size = 17.0 * scale;
        texts.push(Text {
            content: hint.to_string(),
            x: cross_x - CATEGORY_ICON_FOCUSED * scale / 2.0,
            y: height - 56.0 * scale,
            size: hint_size,
            color: theme.text_soft.a(0.66),
            bold: false,
            max_width: width - cross_x,
            align: TextAlign::Left,
        });
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

/// Everything `build_guide` draws from.
pub struct GuideView<'a> {
    pub guide: &'a Guide,
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
}

/// How far the guide's cards have arrived, `age` seconds after it opened.
///
/// Their decoration holds back until the windows flying to them have nearly
/// landed, so frames settle around windows rather than waiting empty for
/// them. A start screen with nowhere to fly from fades up on this same
/// schedule, which is what keeps it from popping in ahead of the rest.
pub fn card_fade(age: f32) -> f32 {
    ease((age - 0.12) / 0.22)
}

/// The same for the start screen's own card, which waits longer.
///
/// A frame is a hairline drawn around a window that is nearly home; the start
/// screen's card is a whole display of opaque content, and the shell paints it
/// *above* everything the compositor is still moving. Arriving on the frames'
/// schedule, it lies over an application that is still halfway to its slot —
/// which is exactly what it looks like: the start screen on top of the app the
/// user is leaving. So it holds back until the flight is over.
pub fn start_card_fade(age: f32) -> f32 {
    ease((age - 0.26) / 0.18)
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
    // the card decorations hold back until the windows have nearly landed.
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
    texts.push(Text {
        content: "Linboard".to_string(),
        x: sidebar_x + text_x,
        y: 52.0 * scale,
        size: title_size,
        color: theme.text.a(0.95 * slide),
        bold: true,
        max_width: text_w,
        align: TextAlign::Left,
    });
    let subtitle_size = 18.0 * scale;
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
        .unwrap_or_else(|| menu_item_rect(items, selected, width, height));
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
    quads.push(Quad {
        x: sidebar_x + hx,
        y: hy,
        w: hw,
        h: hh,
        slot: SOLID_SLOT,
        color: theme.accent.a(0.46 + 0.05 * pulse),
        radius: hh * 0.5,
        thickness: DEPTH_CONTROL * scale,
        behind: view.behind,
        frost: FROST_CONTROL,
        gloss: GLOSS_FULL,
        fade: pane_focus * slide,
        ..Quad::default()
    });

    // The rule between what the menu does to the application above it and
    // what it does to the session below. Barely there on purpose: it is a
    // grouping, not a border.
    if let Some([sx, sy, sw, sh]) = menu_separator_rect(items, width, height) {
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
        let [rx, ry, rw, rh] = menu_item_rect(items, index, width, height);
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
        quads.push(Quad {
            x: sidebar_x + rx + drift,
            y: ry,
            w: rw,
            h: rh,
            slot: SOLID_SLOT,
            color: theme.glass_raised.a(0.10),
            radius: rh * 0.5,
            thickness: DEPTH_CONTROL * scale,
            behind: view.behind,
            frost: FROST_CONTROL,
            gloss: GLOSS_QUIET,
            fade: appear * slide * (1.0 - handed_over),
            ..Quad::default()
        });

        if *item == Item::Power {
            // No label: the glyph is the whole button, as it is on the panel
            // of every desktop this borrows from.
            let glyph = rh * POWER_GLYPH;
            quads.extend(power_glyph(
                sidebar_x + rx + rw * 0.5 + drift,
                // The stroke rises above the ring, so the symbol's own middle
                // sits above the ring's centre; dropping the ring by half that
                // overhang is what centres the *symbol* in its button.
                ry + rh * 0.5 + glyph * 0.16,
                glyph,
                scale,
                theme
                    .text
                    .a((if focused { 1.0 } else { 0.75 }) * appear * slide),
            ));
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
        build(
            xmb, cursor, width, height, focused, "hint", None, 0.0, slots,
        )
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
        // Only the display taking input explains the controls.
        assert!(live.texts.iter().any(|t| t.content == "hint"));
        assert!(!idle.texts.iter().any(|t| t.content == "hint"));
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
            let scene = build(
                &xmb, &cursor, 1920.0, 1080.0, true, "hint", None, time, &AllSlots,
            );
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
        // A window is selected beside the column whenever there is one to
        // select, which is what the shell passes.
        let close_target = cards.first().filter(|card| !card.start).map(|c| &*c.title);
        build_guide(
            GuideView {
                guide,
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
        let [px, py, pw, ph] = menu_item_rect(items, power, 1920.0, 1080.0);
        assert!((pw - ph).abs() < 1.0, "the power button should be square");
        assert!(py > 1080.0 * 0.8, "and sit at the foot of the sidebar");
        let last_row = menu_item_rect(items, power - 1, 1920.0, 1080.0);
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
        assert!((px - menu_item_rect(items, 0, 1920.0, 1080.0)[0]).abs() < 0.5);

        let notched = scene.quads.iter().find(|q| q.notch > 0.0).expect(
            "the power glyph's ring should be cut, not painted over — nothing is opaque here",
        );
        assert!(notched.border > 0.0, "and drawn as a ring");
        let centre = px + pw * 0.5;
        assert!((notched.x + notched.w * 0.5 - centre).abs() < 1.0);
    }

    /// The rule groups what the menu does to the application apart from what
    /// it does to the session, and the row below it is pushed clear.
    #[test]
    fn a_faint_rule_separates_the_application_entries() {
        let guide = Guide::default();
        let items = guide.items(true);
        let rule = menu_separator_rect(items, 1920.0, 1080.0).expect("the column has a rule");
        let dashboard = items
            .iter()
            .position(|item| *item == Item::Dashboard)
            .unwrap();
        let above = menu_item_rect(items, dashboard - 1, 1920.0, 1080.0);
        let below = menu_item_rect(items, dashboard, 1920.0, 1080.0);
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
                .find(|t| t.content == "Linboard")
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
        assert!(alpha(&at(0.15), "Linboard") > alpha(&at(1.0), "Linboard"));
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

        let closable = false;
        let sidebar_w = overview::sidebar_width(1920.0) as f32;
        let settled = guide_scene(&guide, Some("Celeste"), None, &[], None);
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
        assert_eq!(
            capsules(&settled).len(),
            guide.items(closable).len() - 1,
            "settled, every entry but the square power button shows one chip"
        );

        // Mid-glide the chip sits between rows, and the accent is drawn there
        // rather than snapped to the row it is heading for.
        let items = guide.items(closable);
        let first = menu_item_rect(items, 0, 1920.0, 1080.0);
        let second = menu_item_rect(items, 1, 1920.0, 1080.0);
        let between = [first[0], (first[1] + second[1]) / 2.0, first[2], first[3]];
        let gliding = build_guide(
            GuideView {
                guide: &guide,
                app: Some("Celeste"),
                close_target: None,
                screen: None,
                cards: &[],
                highlight: None,
                menu_highlight: Some(between),
                behind: 0.0,
                card_age: guide.age(),
                power: 0.0,
                time: 0.0,
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
            guide.items(closable).len(),
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

    /// One material, one shape language: everything the user can act on in the
    /// column is a capsule — radius exactly half its height.
    #[test]
    fn every_control_in_the_column_is_a_capsule() {
        let mut guide = Guide::default();
        guide.open();
        guide.backdate_open(2.0);
        let cards = [card("Celeste")];
        let scene = guide_scene(&guide, Some("Celeste"), None, &cards, None);

        let items = guide.items(true);
        for (index, item) in items.iter().enumerate() {
            let [rx, ry, _, rh] = menu_item_rect(items, index, 1920.0, 1080.0);
            let control = scene
                .quads
                .iter()
                .find(|q| (q.x - rx).abs() < 1.0 && (q.y - ry).abs() < 1.0 && q.border == 0.0)
                .unwrap_or_else(|| panic!("{item:?} has no pane"));
            assert!(
                (control.radius - rh * 0.5).abs() < 0.01,
                "{item:?} is not a capsule: radius {} of height {rh}",
                control.radius
            );
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

        let mut mini = build(
            &xmb, &cursor, 1920.0, 1080.0, true, "hint", None, 0.0, &AllSlots,
        );
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
}
