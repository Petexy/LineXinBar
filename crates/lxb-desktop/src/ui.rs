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
//!
//! Subcategories add a third axis to the two: depth. Stepping into one opens
//! its column beside the one it came from and slides the whole cross over, so
//! the columns behind stay on screen showing only the row each was opened
//! from — the trail that reads, left to right, as the path taken.

use crate::apps::{Entry, Role};
use crate::dialog::{Dialog, Line};
use crate::gpu::{Quad, Text, TextAlign, GLOW_SLOT, SOLID_SLOT, SQUIRCLE_CORNER};
use crate::guide::{self, separator_rows, Bar, Guide, Item, Pane};
use crate::icons;
use crate::keyboard;
use crate::menu::{Entry as MenuEntry, Menu};
use crate::model::{Cursor, Standing, Xmb};
use crate::system::Level;
use crate::theme::theme;
use lxb_protocol::overview;

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

/// How tall a row is drawn in a column of the user's own pictures, unfocused
/// and focused.
///
/// A column of films or photographs is not a list of names with marks beside
/// them; the picture *is* the row, and it has to be big enough to be looked at
/// rather than identified. Half again the icon unfocused, and getting on for
/// three times it under the light.
const CARD_HEIGHT: f32 = 96.0;
const CARD_HEIGHT_FOCUSED: f32 = 176.0;

/// The clear glass between one card and the next, as a share of how wide they
/// are.
///
/// A share and not a distance, because a gap is only ever read against the
/// thing either side of it. A film's frame is wide, and two of them a long way
/// apart still read as one list; a cover is barely a third of that across, and
/// the same absolute gap turns the same column into a stack of islands with
/// wallpaper running between them. Measured against the width rather than the
/// height for the same reason — the width is what the column *is*, and the
/// height is what changes when a row is chosen.
///
/// The number is what the film column already stood at, so that column has not
/// moved: 0.30 of 171 is the 52 pixels its rows have always had between them.
const CARD_AIR: f32 = 0.30;

/// The shape of the card itself, whatever shape the picture in it is.
///
/// Fixed rather than the picture's own, and that is the whole of what makes a
/// column of them read as a list: with a card per aspect the labels beside
/// them would step in and out by a hundred pixels a row, and a column of
/// holiday photographs half of which are portrait would look like a fault. So
/// the card is one shape — the one a film is — and the picture is fitted
/// inside it, standing on the glass with the plate showing either side of a
/// tall one. Which is what a photograph on a light table looks like.
const CARD_ASPECT: f32 = 16.0 / 9.0;

/// And the shape of a Steam cover, which is Valve's portrait capsule: 600 by
/// 900, the one picture every game in a library has and the one a person
/// recognises a game by.
const COVER_ASPECT: f32 = 600.0 / 900.0;

/// The border of glass a picture stands on, as a share of the picture's own
/// shorter side.
///
/// An *even* border, the same number of pixels on all four sides, which is
/// what a mount is and what makes one invisible: the eye is very good at
/// seeing that the strip above a picture is half again the strip beside it,
/// and reads the difference as the picture having slipped rather than as a
/// frame. That is what makes the card's shape a consequence of this rather
/// than a constant — see [`Cards::width`].
const CARD_MOUNT: f32 = 0.05;

/// The corner of a card, likewise.
const CARD_CORNER: f32 = 0.09;
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

/// How wide a round preview is, as a multiple of the room a glyph would have
/// had.
///
/// Wider than the glyph and narrower than the glass under it. A glyph is a
/// drawing with its own margin inside a square, so a photograph drawn to the
/// same square would read as the smaller object of the two; and one drawn to
/// the disc would cover the glass exactly, leaving the chosen row with no glass
/// showing at all. Between the two, the picture is the largest thing on the
/// row and the light still reaches round it.
const ITEM_PREVIEW: f32 = ITEM_DISC * 0.8;
const CATEGORY_DISC: f32 = 1.30;
/// What says where the next letter will land, in the one row of the bar that
/// can be typed into.
///
/// A character on the end of the string rather than a quad drawn after it,
/// because the text is shaped on the GPU and nothing on this side of the wire
/// knows how wide a word comes out. A bar positioned from here would be near
/// the end of the query rather than at it, and near is worse than nothing.
///
/// It does not blink. A blink is a redraw twice a second for as long as the
/// board is up, on a shell that otherwise draws only when something has
/// happened — and a caret that simply stays put is no harder to find, which is
/// the only thing blinking was ever for.
const CARET: char = '|';
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

/// The category row's own half-height: the glass its icon stands on, which is
/// the thing every row near it has to keep clear of.
const CATEGORY_HALF: f32 = CATEGORY_ICON_FOCUSED * CATEGORY_DISC / 2.0;

/// The clear air above the category row, between its glass and the bottom of
/// the row standing over it.
///
/// Read out of [`ITEM_GAP_ABOVE`] rather than chosen, because that is the air
/// the bar has always had there and none of this is meant to change a column
/// of icons. What it is measured *to* is what changes: half an unfocused icon
/// in a column of applications, half an unfocused card in a column of covers,
/// which is two and a half times as much. Left as one distance to the row's
/// centre, a library's first cover sat on the category's glass.
const CATEGORY_AIR_ABOVE: f32 = ITEM_GAP_ABOVE - CATEGORY_HALF - ITEM_ICON / 2.0;

/// Everything between the category row's centre and the bottom of its label:
/// its own glass, and the label set under it.
const CATEGORY_TO_LABEL: f32 = CATEGORY_HALF + CATEGORY_LABEL_ABOVE + CATEGORY_LABEL;

/// And the air under that label, before the column begins.
///
/// More of it under a column of cards, which is not a decoration: what follows
/// the label in a column of icons is a *disc*, at its nearest only directly
/// under the middle of the word and falling away on both sides of it, so the
/// eye reads the air as the diagonal. A card is a straight edge running the
/// whole width of the column, and the same measured distance under one looks
/// like half as much.
const CATEGORY_LABEL_TO_ICONS: f32 = CATEGORY_LABEL_BELOW;
const CATEGORY_LABEL_TO_CARDS: f32 = 50.0;

/// How far the centre of the first row below the category row stands from it.
///
/// Added up rather than chosen, so that it cannot stop being true: the
/// category's glass, its label, the air after the label, and half of whatever
/// the first row of the column is — which is the *chosen* row, and therefore
/// the biggest thing the column ever draws.
const ITEM_GAP_BELOW: f32 =
    CATEGORY_TO_LABEL + CATEGORY_LABEL_TO_ICONS + ITEM_ICON_FOCUSED * ITEM_DISC / 2.0;

fn gap_below(cards: Option<Cards>) -> f32 {
    CATEGORY_TO_LABEL
        + match cards {
            Some(cards) => CATEGORY_LABEL_TO_CARDS + cards.focused / 2.0,
            None => CATEGORY_LABEL_TO_ICONS + ITEM_ICON_FOCUSED * ITEM_DISC / 2.0,
        }
}

/// The same, for the row standing *above* the category row. See
/// [`CATEGORY_AIR_ABOVE`].
fn gap_above(cards: Option<Cards>) -> f32 {
    CATEGORY_HALF + CATEGORY_AIR_ABOVE + cards.map_or(ITEM_ICON, |cards| cards.height) / 2.0
}

/// Where the cross's arms meet, as a share of the display. The focused entry
/// sits here, so it is also where a launch opens from.
const BAR_CROSS_X: f32 = 0.22;
const BAR_CROSS_Y: f32 = 0.30;

/// How close the outermost column may come to the left edge: clearance for a
/// row's own glass disc, which is the widest thing in a column, and air.
const COLUMN_MARGIN: f32 = ITEM_ICON_FOCUSED * ITEM_DISC / 2.0 + 24.0;

/// How much of the gap between two columns belongs to the one in front — its
/// own row's glass, and a little air before the label behind it reaches that.
const COLUMN_CLEAR: f32 = ITEM_ICON_FOCUSED * ITEM_DISC / 2.0 + 10.0;

/// How far a subcategory's column stands to the right of the one it was
/// opened from.
///
/// Exactly the room there is to the left of the cross on the reference
/// display. Every step in slides the whole chain one of these to the left, so
/// the column just opened stands where the cross always is and the one it was
/// opened from stands where this leaves it — which on a 16:9 display is
/// [`COLUMN_MARGIN`], the near edge. A path therefore always looks the same
/// however deep it has gone: the column being browsed in one place, the one
/// behind it in another, and everything older gone off the side.
///
/// That is the whole reason the bar can be walked into indefinitely. A trail
/// that laid each new column out beside the last would spend a long path
/// marching across the screen, and the fourth subcategory of a path would be
/// somewhere the third had left no room for.
///
/// It has to be about this wide, too. What stands in the gap between two
/// columns is not an icon but the row one of them was opened from, label and
/// all, so at anything like the category row's spacing the trail would read as
/// names printed over the icons of the column in front of them.
const SUBCOLUMN_STEP: f32 = REFERENCE_HEIGHT * 16.0 / 9.0 * BAR_CROSS_X - COLUMN_MARGIN;

/// One step further back: how much smaller it is drawn, and how much of its
/// ink survives the distance.
///
/// The trail does not merely dim behind the open column, it *recedes*, and
/// each column behind recedes one step further than the one in front of it —
/// so three levels down the category sits behind the column it holds, which
/// sits behind the one opened from that, which sits behind the one the user is
/// standing in. That is the whole of what tells a glance how far in it has
/// gone, and it is why the two are one pair of numbers rather than a dimming
/// applied to everything behind: at a fixed dimming a path four deep and a
/// path one deep look the same.
///
/// Size and ink together, because either alone is something else. Shrinking
/// without dimming is a row that has been made small; dimming without
/// shrinking is one that has been switched off. The haze is the stronger of
/// the two on purpose: distance takes more of an object's contrast against
/// what is behind it than it takes of its size, which is why the far end of a
/// street is grey long before it is small.
const DEPTH_SHRINK: f32 = 0.86;
const DEPTH_HAZE: f32 = 0.72;

/// How far back the recession is allowed to carry something.
///
/// Perspective has no such limit and legibility does. A trail is there to be
/// read — it is the only thing on screen saying how the user got where they
/// are — so past about four steps it stops receding and simply stays at the
/// back, which is also the point past which another step would be telling the
/// eye nothing it has not already been told.
const DEPTH_FLOOR_SCALE: f32 = 0.5;
const DEPTH_FLOOR_HAZE: f32 = 0.35;

/// How something `steps` of recession away is drawn: the scale it takes, and
/// the share of its ink that reaches the front.
fn receded(steps: f32) -> (f32, f32) {
    let steps = steps.max(0.0);
    (
        DEPTH_SHRINK.powf(steps).max(DEPTH_FLOOR_SCALE),
        DEPTH_HAZE.powf(steps).max(DEPTH_FLOOR_HAZE),
    )
}

/// How much of its journey out something still has to run by the time it has
/// given up the last of its ink: a third of it.
///
/// The number is where the two lists cross. Any lower and they cross while
/// both are still strong enough to read, which is the thing this exists to
/// stop; much higher and what is leaving is gone before what replaces it is
/// halfway up, leaving the middle of the screen briefly empty.
const COLUMN_GONE_BY: f32 = 0.35;

/// What a row on its way off the screen has left, from how much of that
/// journey is still to run — 1 as the button lands, 0 once it is
/// [`COLUMN_GONE_BY`] of the way there.
///
/// Every step the bar takes through a path lays one list over another. Going
/// in, the column opening slides over the one it was opened from, which keeps
/// its trail row and gives up everything else; coming back, the column being
/// left stands over that same list the whole way home. Either way the two are
/// read in the same place — cards across labels, labels across the column
/// after that — and ink given up at the pace of the slide leaves the pair at
/// half strength each across the middle of the move. That is not one list
/// handing over to another. It is two lists printed over one another, and
/// neither of them can be read while it lasts.
///
/// So the ink goes first and the movement follows: what is leaving keeps
/// sliding and receding for the whole glide, which is what says where it went,
/// but it is out of the way while what replaces it is still coming up. Eased
/// at both ends like everything else the shell fades, so it leans out rather
/// than blinking off on the frame the button lands.
///
/// Only what leaves. What *arrives* has the space ahead of it and nothing to
/// clear, so it fades up over the whole of its glide: the same ramp applied
/// there would be a column that waited until it had almost stopped and then
/// appeared.
fn departing(left: f32) -> f32 {
    ease((left - COLUMN_GONE_BY) / (1.0 - COLUMN_GONE_BY))
}

/// The mark on the row a settings list is currently set to: how big it is as a
/// fraction of the icon it sits on, and how far its centre is from the icon's.
///
/// Out at the corner rather than over the middle. What it marks is the swatch,
/// and a mark large enough to read that sat on the centre of a colour would be
/// covering the one thing the row is there to show.
const CHOSEN_BADGE: f32 = 0.46;
const CHOSEN_BADGE_AT: f32 = 0.34;

/// The launch splash: how big the application's icon settles at, how many
/// dots go round it and how long the light takes to travel the ring.
const LAUNCH_ICON: f32 = 168.0;
const LAUNCH_DOTS: usize = 12;
const LAUNCH_SPIN: f32 = 1.4;

/// The box a game's logo settles inside, as shares of the display.
///
/// Shares rather than a figure against 1080p, unlike nearly everything else
/// here, because what this is measuring itself against is the *picture behind
/// it*: the hero fills the display whatever shape the display is, and a logo
/// sized in scaled pixels would sit differently on it at every aspect ratio.
///
/// A box and not a size: a logo is fitted into it at its own shape, so the
/// stacked ones reach the full height and the one-line ones the full width,
/// and neither is stretched to meet the other. The width is the binding
/// constraint for a wordmark set in one line, and it is held under half the
/// display on purpose — a title that reached the edges would be a poster
/// rather than a game starting.
const LAUNCH_LOGO_WIDTH: f32 = 0.38;
const LAUNCH_LOGO_HEIGHT: f32 = 0.30;

/// How much smaller the logo starts than it settles.
///
/// Not out of the tile, which is what an application's icon does. The tile a
/// game was chosen from holds its *cover*, and a logo growing out of a cover
/// is one picture of the game turning into a different one — so this one comes
/// forward out of the picture it is already standing on instead, which is the
/// same move Valve's own library makes when a game is chosen.
const LAUNCH_LOGO_FROM: f32 = 0.74;

/// Where the ring stands while a game loads: how far its centre is from each
/// corner, and how far the dots orbit it, against 1080p.
///
/// The corner rather than under the title, because on a game there is nothing
/// under the title — the picture behind the whole thing is the game's own, and
/// a spinner in the middle of it would be the shell writing across somebody's
/// artwork. In the corner it says *working* without standing on anything, and
/// it is where a console has always put it.
const LAUNCH_RING_INSET: f32 = 78.0;
const LAUNCH_RING_ORBIT: f32 = 30.0;

/// The name a game falls back to when Valve has no logo for it: how big it is
/// set against 1080p, and how much of the display's width it may run across.
const LAUNCH_NAME: f32 = 62.0;
const LAUNCH_NAME_WIDTH: f32 = 0.72;

/// And how strongly it is ringed in shade.
///
/// Half of what a bubble's summary gets. A ring's reach is a share of the size
/// the run is drawn at rather than a number of pixels — see
/// [`crate::gpu::Text`]'s `halo` — and this run is nearly three times a
/// bubble's. At a bubble's strength the same ring comes out as a hard black
/// outline round every letter, which is a sticker rather than a title standing
/// on a picture.
const LAUNCH_NAME_HALO: f32 = 0.5;

/// The line beside the indicator that says what is being waited for: its size
/// against 1080p, the air between it and the ring, and how far back towards
/// the middle of the display it may run before it is cut.
///
/// Not bold and not large. It is the answer to a question nobody asked out
/// loud — *is this going anywhere* — and a heading-sized one would compete
/// with the title in the middle of the screen, which is the thing the display
/// is actually about.
const LAUNCH_DOING: f32 = 25.0;
const LAUNCH_DOING_GAP: f32 = 18.0;
const LAUNCH_DOING_WIDTH: f32 = 0.34;

/// How far past the ring the shade under it reaches, as a multiple of the
/// ring's own radius.
///
/// Wide, because what makes a bloom read as light and not as a disc is that
/// its edge is nowhere near the thing it is lighting. Anything close in starts
/// to show as a grey coin in the corner of the picture.
const LAUNCH_RING_SHADE: f32 = 3.6;

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
/// The line the tiles share, and how big a tile is on it. Taller than a
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
/// floating away from one edge and hugging the other. It also has to clear the
/// sidebar's fifteen-pixel bevel by enough to sit on the broad face rather
/// than looking as though every button rests on the rim.
const GUIDE_MARGIN: f32 = 24.0;
/// Air inside a labelled button, on both sides of the text's available line.
/// Matching the panel-side margin makes the nested shape read deliberately:
/// panel, equal air, button, equal air, label.
const GUIDE_LABEL_PADDING: f32 = 24.0;
/// Air above and below a non-tile chip inside the line allotted to it.
const GUIDE_ROW_PADDING: f32 = 5.0;
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
/// Every one of them is a property of the material rather than of the picture.
/// The shader derives the rim, sheen and shadow from those properties and the
/// one lamp the shell is lit by; the sidebar's broad face gets its own shallow
/// curvature below because its scale is unlike any of these compact panes.
const DEPTH_PANEL: f32 = 22.0;
const DEPTH_CONTROL: f32 = 9.0;
const FROST_PANEL: f32 = 0.95;
const FROST_CONTROL: f32 = 0.2;
const GLOSS_FULL: f32 = 1.0;
const GLOSS_QUIET: f32 = 0.45;

/// The guide is tall enough that the modal-panel recipe above turns almost
/// all of its face into one flat field. Its own cut is shallower and clearer:
/// the animated current remains visible through it, while the controls still
/// stand proud as the frostier objects on top.
const DEPTH_SIDEBAR: f32 = 15.0;
const FROST_SIDEBAR: f32 = 0.46;
const GLOSS_SIDEBAR: f32 = 0.66;
const CURVE_SIDEBAR: f32 = 1.0;
const SIDEBAR_STAIN: f32 = 0.38;
/// The light beneath that glass. These are deliberately quiet; their job is
/// to give the large face depth and the active palette, not to compete with
/// the selected control's bloom.
const SIDEBAR_HEADER_LIGHT: f32 = 0.075;
const SIDEBAR_FOOT_LIGHT: f32 = 0.04;
const SIDEBAR_RIM: f32 = 0.10;

/// The pulsing halo outside the selection frame: how many rings approximate
/// the falloff, and how far apart they sit against the 1080p reference.
const HALO_RINGS: usize = 5;
const HALO_STEP: f32 = 3.0;

/// The power dialog, centred on the display: its width, its row height, and
/// how far the rest of the overlay is dimmed behind it.
const POWER_WIDTH: f32 = 460.0;
const POWER_ROW: f32 = 64.0;
const POWER_DIM: f32 = 0.28;

/// The share of the dialog's growth that passes before its contents begin to
/// appear. A folder's icons do not read at the size of a folder icon, and
/// letting them try makes the whole thing look like a shrunken dialog being
/// enlarged rather than something opening out of the button.
const POWER_CONTENT_IN: f32 = 0.45;

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
                // The whole line at its own size, and how much room the column
                // actually has for it.
                let tiles = *count as f32;
                let mut size = GUIDE_TILE * scale;
                let mut gap = GUIDE_TILE_GAP * scale;
                let mut run = size * tiles + gap * count.saturating_sub(1) as f32;
                let inside = panel_w - margin * 2.0;
                // A narrow sidebar carrying every tile this shell has cannot
                // hold them all at full size — 280 px is the floor a sidebar
                // is allowed to be, and it is nearly the width of four of
                // them. The line is taken in as a whole rather than cropped at
                // one end or allowed to overhang the inset every other control
                // on the panel keeps: tiles and gaps shrink together, so what
                // reads as a row of squares with even air between them stays
                // one whichever screen it is drawn on.
                if run > inside && run > 0.0 {
                    let taken_in = inside / run;
                    size *= taken_in;
                    gap *= taken_in;
                    run = inside;
                }
                // Centred on the panel rather than started at its margin. The
                // tiles are the one line of the column that does not span it,
                // so a run left-aligned under a rank of full-width capsules
                // reads as a row that failed to finish rather than as a line
                // of its own — and how far short it falls depends on how many
                // tiles this session turned out to have, which is not
                // something the eye can be asked to allow for.
                let column = (index - first) as f32;
                return [
                    panel_x + (panel_w - run) * 0.5 + column * (size + gap),
                    y + (row - size) * 0.5,
                    size,
                    size,
                ];
            }
            let row_padding = GUIDE_ROW_PADDING * scale;
            return [
                panel_x + margin,
                y + row_padding,
                panel_w - margin * 2.0,
                row - row_padding * 2.0,
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

/// The guide's large surface, built as light under glass rather than as one
/// uniformly frosted block.
///
/// The two glows are painted first, wholly inside the panel, so the pane
/// scatters and bends them together with the animated wallpaper. That makes
/// them part of the material instead of colour sprayed over its face. The
/// hairline drawn last restores a precise silhouette without pretending the
/// whole circumference catches the same broad highlight.
fn sidebar_surface([x, y, w, h]: [f32; 4], scale: f32, behind: f32, fade: f32) -> [Quad; 4] {
    let theme = theme();
    let header_h = (300.0 * scale).min(h * 0.36);
    let foot_h = (360.0 * scale).min(h * 0.38);
    let vertical_inset = 2.0 * scale;

    [
        Quad {
            x: x + w * 0.06,
            y: y + vertical_inset,
            w: w * 0.88,
            h: header_h,
            slot: GLOW_SLOT,
            color: theme.accent_soft.a(SIDEBAR_HEADER_LIGHT),
            fade,
            ..Quad::default()
        },
        Quad {
            x: x + w * 0.10,
            y: y + h - foot_h - vertical_inset,
            w: w * 0.80,
            h: foot_h,
            slot: GLOW_SLOT,
            color: theme.accent.a(SIDEBAR_FOOT_LIGHT),
            fade,
            ..Quad::default()
        },
        Quad {
            x,
            y,
            w,
            h,
            slot: SOLID_SLOT,
            color: theme.glass.a(SIDEBAR_STAIN),
            radius: PANEL_RADIUS * scale,
            thickness: DEPTH_SIDEBAR * scale,
            behind,
            frost: FROST_SIDEBAR,
            gloss: GLOSS_SIDEBAR,
            face_curve: CURVE_SIDEBAR,
            fade,
            ..Quad::default()
        },
        Quad {
            x,
            y,
            w,
            h,
            slot: SOLID_SLOT,
            color: theme.accent_soft.a(SIDEBAR_RIM),
            radius: PANEL_RADIUS * scale,
            border: (1.0 * scale).max(1.0),
            fade,
            ..Quad::default()
        },
    ]
}

/// The rules, in the same coordinates: one above each row where the column
/// changes from one kind of thing to another.
fn menu_separator_rects(items: &[Item], width: f32, height: f32) -> Vec<[f32; 4]> {
    let scale = guide_scale(height);
    let [panel_x, _, panel_w, _] = sidebar_panel_rect(width, height);
    // The header and rules share one visual measure even though the glass
    // controls themselves sit further out.
    let side = GUIDE_PADDING * scale * 0.8;
    let rule_h = (1.0 * scale).max(1.0);
    separator_rows(items)
        .into_iter()
        .filter_map(|row| {
            let above = (0..row).rev().find(|index| items[*index] != Item::Power)?;
            let [_, above_y, _, above_h] = menu_item_rect(items, above, width, height);
            let [_, below_y, _, _] = menu_item_rect(items, row, width, height);
            // Centre the rule between the *visible* chip edges. Positioning it
            // from the lower line alone ignored each chip's own row padding,
            // which left visibly more air above the rule than below it.
            let y = (above_y + above_h + below_y - rule_h) * 0.5;
            Some([panel_x + side, y, panel_w - side * 2.0, rule_h])
        })
        .collect()
}

/// The guide's layout scale for a display `height` tall.
fn guide_scale(height: f32) -> f32 {
    (height / REFERENCE_HEIGHT).clamp(0.6, 2.5)
}

/// How far left of where it settles the sidebar still is, `age` seconds after
/// the menu opened. Zero once it has arrived.
///
/// Every rectangle in the column is measured from the sidebar's settled edge —
/// that is what lets the selection glide between rows without the entrance
/// dragging it about — so this is what the drawing adds and what a hit test has
/// to add with it. A quarter of a second is short, but a click landing a
/// sidebar's width away from what it looked like it was on is not something to
/// leave to the user being slow.
pub fn sidebar_slide_x(age: f32, width: f32) -> f32 {
    (ease(age / GUIDE_SLIDE) - 1.0) * overview::sidebar_width(width as f64) as f32
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
        // overhang against. So the display's own edges travel into the card
        // with the scene: everything is cut to them first, and what the screen
        // never showed is not shown in the miniature either.
        //
        // The category row is what this is about. Open a path and the row
        // slides off the left edge with it, and every category's name is
        // centred on a box wider than the button it sits under — so the name of
        // the one the path hangs off reaches past the edge long before the
        // button does, and on a display narrower than the reference one the
        // button goes past it as well.
        //
        // Cut rather than dropped, which is the whole of the difference. A
        // dropped icon is one that *vanishes* the moment the guide is pressed
        // and reappears whole when it closes — the icon of the category a path
        // was opened from, and the mark on the trail beside it, gone from a
        // card that is meant to be a picture of the screen behind it. Cut, it
        // is the same half of an icon in the card as it is on the display.
        let screen = [0.0, 0.0, width, height];
        self.quads.retain_mut(|quad| {
            let clip = match quad.clip {
                Some(clip) => intersection(clip, screen),
                None => screen,
            };
            // Nothing of it was on the screen, so nothing of it is in the card.
            let shown = intersection([quad.x, quad.y, quad.w, quad.h], clip);
            if shown[2] <= 0.0 || shown[3] <= 0.0 {
                return false;
            }
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
            let [cx, cy, cw, ch] = clip;
            quad.clip = Some([x + cx * scale, y + cy * scale, cw * scale, ch * scale]);
            true
        });
        // Text keeps any run that shows at all: its box is a generous wrapping
        // width rather than the ink, and a right-aligned clock reaches the edge
        // without ever touching it.
        self.texts.retain(|t| {
            t.x < width && t.y < height && t.x + t.max_width > 0.0 && t.y + t.size > 0.0
        });
        // Kept is not the same as kept whole for a run either: one the display
        // cuts in half has to be cut here too, or the half the screen never
        // showed is drawn outside the card — the word "Multimedia" on the
        // wallpaper beside the very miniature it is part of.
        self.texts.retain_mut(|text| {
            // Whatever was already standing over the run, cut down to the
            // screen: both are rectangles in the display's own pixels, and
            // both have to travel into the card.
            let clip = match text.clip {
                Some(clip) => intersection(clip, screen),
                None => screen,
            };
            if clip[2] <= 0.0 || clip[3] <= 0.0 {
                return false;
            }
            text.x = x + text.x * scale;
            text.y = y + text.y * scale;
            text.size *= scale;
            text.max_width *= scale;
            let [cx, cy, cw, ch] = clip;
            text.clip = Some([x + cx * scale, y + cy * scale, cw * scale, ch * scale]);
            true
        });
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
            // Whatever is cutting the pane travels with it, for the same
            // reason a run's does: this is the same scene arriving somewhere
            // else, not a new one.
            quad.clip = quad.clip.map(|[x, y, w, h]| {
                [
                    x * factor + offset[0],
                    y * factor + offset[1],
                    w * factor,
                    h * factor,
                ]
            });
        }
        for text in &mut self.texts {
            text.x = text.x * factor + offset[0];
            text.y = text.y * factor + offset[1];
            text.size *= factor;
            text.max_width *= factor;
            // Whatever is standing over the run travels with it: this is the
            // same scene arriving somewhere else, not a new one.
            text.clip = text.clip.map(|[x, y, w, h]| {
                [
                    x * factor + offset[0],
                    y * factor + offset[1],
                    w * factor,
                    h * factor,
                ]
            });
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

    /// Cut the text runs a panel covers back to the parts of them it does not.
    ///
    /// Every quad in a scene is drawn before every text run, so a panel laid
    /// over a scene does not hide that scene's labels — they print straight
    /// through it, and a menu raised over the guide's sidebar would have the
    /// sidebar's words written across it. What the panel covers therefore has
    /// to be taken away here.
    ///
    /// Only what it covers, though. A run is cut at the panel's edges rather
    /// than dropped whole, because these panels stand *over* things that are
    /// wider than they are — the bar's own rows reach from the cross to the
    /// far edge of the display — so the runs in their way are usually
    /// half-covered ones. Dropping those emptied every label the panel so much
    /// as touched, which reads as the bar losing its words rather than as
    /// something being in front of them.
    ///
    /// Both sides are kept, which is the whole of the fix here. This used to
    /// keep whichever side had more *room* in the run's box, and that is not
    /// the side the words are on: a bar label is laid out in a box running to
    /// the edge of the display and printed at the left of it, so a panel in the
    /// middle left more empty box on the right, and the label was cut down to a
    /// stretch of it that had nothing in it. That is why every name behind the
    /// install panel disappeared. A run has one shaping and two clips now, and
    /// the words survive on whichever side they were actually on.
    pub fn hide_text_behind(&mut self, rect: [f32; 4]) {
        self.cut_text_behind(rect, 1.0);
    }

    /// The same, for a panel that is not opaque yet: the part of a run it
    /// covers is faded by `amount` rather than taken away, and only gone at 1.
    ///
    /// Needed because these panels travel as one whole rectangle rather than
    /// growing into one — see [`dialog_bounds`] — so a panel one frame out of
    /// the control it came from already covers, on paper, everything it will
    /// ever cover, while being very nearly invisible. Dropping the text under
    /// it outright therefore takes labels away a fifth of a second before
    /// anything is over them, which reads as the menu underneath losing its
    /// words rather than as a panel arriving on top of it.
    pub fn dim_text_behind(&mut self, rect: [f32; 4], amount: f32) {
        self.cut_text_behind(rect, amount.clamp(0.0, 1.0));
    }

    /// One run in, up to three out: what is left of it on either side of the
    /// panel, and — while the panel is still arriving — what is under it,
    /// faded by how far arrived it is.
    ///
    /// Horizontal only. Vertically the run is one line inside its own box
    /// already, and a panel that overlaps it at all overlaps that whole line —
    /// a label cut through the middle by a panel edge would be worse than
    /// either answer here.
    ///
    /// Cutting rather than re-laying out: the pieces keep the run's own `x`,
    /// `max_width` and alignment, so every glyph stays exactly where it was and
    /// only the scissor differs. See [`crate::gpu::Text::clip`].
    fn cut_text_behind(&mut self, rect: [f32; 4], amount: f32) {
        let [x, _, w, _] = rect;
        if w <= 0.0 || amount <= 0.0 {
            return;
        }
        let mut kept = Vec::with_capacity(self.texts.len());
        for text in std::mem::take(&mut self.texts) {
            if !behind(&text, rect) {
                kept.push(text);
                continue;
            }
            // A piece of the run, in its own box rather than in its ink: the
            // ink is only known once the run has been shaped, which happens two
            // crates away, and a scissor that is too generous costs nothing —
            // there are no pixels out there to cut.
            let piece = |from: f32, width: f32| {
                let box_of_it = [from, text.y, width, text.size * 2.0];
                let cut = match text.clip {
                    Some(clip) => intersection(clip, box_of_it),
                    None => box_of_it,
                };
                (cut[2] > 0.0 && cut[3] > 0.0).then_some(cut)
            };
            let ends = [
                piece(text.x, x - text.x),
                piece(x + w, text.x + text.max_width - (x + w)),
            ];
            for clip in ends.into_iter().flatten() {
                kept.push(Text {
                    clip: Some(clip),
                    halo: 0.0,
                    lines: 1,
                    ..text.clone()
                });
            }
            if amount < 1.0 {
                if let Some(clip) = piece(x, w) {
                    let mut under = text;
                    under.color[3] *= 1.0 - amount;
                    under.clip = Some(clip);
                    kept.push(under);
                }
            }
        }
        self.texts = kept;
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

/// What two rectangles have in common, which may be nothing: a width or a
/// height of zero or less means they do not meet at all.
fn intersection([ax, ay, aw, ah]: [f32; 4], [bx, by, bw, bh]: [f32; 4]) -> [f32; 4] {
    let x = ax.max(bx);
    let y = ay.max(by);
    [x, y, (ax + aw).min(bx + bw) - x, (ay + ah).min(by + bh) - y]
}

/// Whether a run sits inside `rect`, which is what "behind the panel" means to
/// a scene whose quads are all drawn before any of its text.
fn behind(text: &Text, [x, y, w, h]: [f32; 4]) -> bool {
    !(text.x >= x + w
        || text.x + text.max_width <= x
        || text.y >= y + h
        || text.y + text.size * 1.4 <= y)
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

    /// The picture of one of the user's own files, if one has been made and is
    /// still in the atlas.
    ///
    /// Answers `None` far more often than not, and that is the ordinary case
    /// rather than a failure: a thumbnail is made only for the rows around the
    /// cursor, and a row drawn before its picture arrives shows the glyph the
    /// column is marked with. Nothing about the row's size or place depends on
    /// the answer — see [`CARD_ASPECT`] — so the picture fades into a card
    /// that was already there instead of pushing the list about as it lands.
    fn thumbnail(&self, _path: &std::path::Path) -> Option<crate::gpu::Thumb> {
        None
    }

    /// Steam's cover for one game, if it has been fetched and is still in the
    /// atlas.
    ///
    /// By app id rather than by a path, because unlike a file of the user's
    /// own, a game is not a thing on this disk: it may not be installed at
    /// all, and where its picture ended up — Valve's cache or this shell's —
    /// is [`crate::art`]'s business and nothing the layout should have an
    /// opinion about.
    ///
    /// Answers `None` far more often than not, exactly as [`Self::thumbnail`]
    /// does, and for the same reason: the row is drawn at the same size and in
    /// the same place either way, so a cover fades into a card that was
    /// already there.
    fn cover(&self, _app_id: u32) -> Option<crate::gpu::Thumb> {
        None
    }

    /// How much of the colour has drained out of that cover: 0 for a game that
    /// is on this disk and can be started, 1 for one that is not.
    ///
    /// A game is not a thing the layout can look up — it is told whether this
    /// one is installed, which is the whole of the question — so what the shell
    /// adds on top is only *when* the answer arrives: a download finishing
    /// gives a cover its colour back over a moment rather than between two
    /// frames. Every other caller takes the plain answer from here.
    fn drain(&self, _app_id: u32, installed: bool) -> f32 {
        if installed {
            0.0
        } else {
            1.0
        }
    }
}

/// The cards a column of pictures is drawn on: their shape, and how tall they
/// stand chosen and unchosen.
///
/// One description rather than three constants because two kinds of column
/// show pictures now and they are not the same shape — a frame out of a film
/// is wide, a Steam cover is tall — and everything that lays a column out has
/// to be told which.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Cards {
    /// Width over height — of the *picture*, not of the card. The card is that
    /// plus its mount, which is a little squarer; see [`Cards::width`].
    aspect: f32,
    height: f32,
    focused: f32,
}

impl Cards {
    /// Cards of `aspect`, taking up the room a picture card takes.
    ///
    /// What is held constant across shapes is *area*, and that is the only
    /// choice here that needed making. Holding the height would make covers
    /// small — a portrait picture as tall as a landscape one is half the
    /// picture — and holding the width would make them enormous, half a screen
    /// each. Equal area is the two of them weighing the same on the glass,
    /// which is what a column of rows is asking for.
    fn of(aspect: f32) -> Cards {
        let grow = (CARD_ASPECT / aspect).sqrt();
        Cards {
            aspect,
            height: CARD_HEIGHT * grow,
            focused: CARD_HEIGHT_FOCUSED * grow,
        }
    }

    /// The mount round a card drawn `height` tall: the same width of glass on
    /// all four sides. See [`CARD_MOUNT`].
    ///
    /// Solved rather than measured, because the border is a share of the
    /// picture and the picture is what is left of the card once the border is
    /// taken off it. With `c` the share and `s` the picture's shorter side as a
    /// multiple of its height — 1 for anything wide, the aspect itself for
    /// anything tall — the border `m` satisfies `m = c·s·(height − 2m)`, and
    /// this is that rearranged.
    fn mount(&self, height: f32) -> f32 {
        let shorter = self.aspect.min(1.0);
        CARD_MOUNT * shorter * height / (1.0 + 2.0 * CARD_MOUNT * shorter)
    }

    /// How wide a card drawn `height` tall is: its picture, plus that mount
    /// either side of it.
    ///
    /// So the card is very slightly squarer than the picture it carries, and
    /// deliberately: a mount of even width round a picture of a given shape
    /// *is* a slightly different shape, and the alternative — a card at the
    /// picture's own aspect — is what leaves the border wider on one pair of
    /// sides than the other.
    fn width(&self, height: f32) -> f32 {
        let mount = self.mount(height);
        (height - mount * 2.0) * self.aspect + mount * 2.0
    }

    /// The clear glass between two unchosen cards. See [`CARD_AIR`].
    fn air(&self) -> f32 {
        self.width(self.height) * CARD_AIR
    }

    /// How far apart two unchosen rows stand, centre to centre.
    ///
    /// Added up rather than chosen, for the reason [`ITEM_GAP_BELOW`] is: a
    /// card, and the air after it.
    ///
    /// The chosen row is *not* in this: it is taller than the rest by
    /// [`Self::swell`] and is pushed clear of its neighbours by half of that on
    /// each side, so that the glass between any two rows is the same width
    /// whichever of them is chosen. A single pitch covering both was the whole
    /// of what was wrong with the column this replaced — sized so the chosen
    /// card cleared its neighbours, it left the unchosen ones half a card
    /// apart, and a library read as a stack of islands with wallpaper running
    /// between them.
    fn spacing(&self) -> f32 {
        self.height + self.air()
    }

    /// How much taller the chosen card is than its neighbours — which is how
    /// much a column has to push the rows either side of it away by.
    fn swell(&self) -> f32 {
        self.focused - self.height
    }

    /// The width of the chosen card, which is what the labels beside it and
    /// the strip a click lands in are both measured from.
    fn reach(&self) -> f32 {
        self.width(self.focused)
    }
}

/// The cards a column shows, or `None` for a column of icons.
///
/// A property of the *column* and not of what has loaded: the rows have to
/// stand in the same places from the first frame, or arriving pictures would
/// walk the list up and down under the cursor.
///
/// Asked of the first row that knows — which on a shelf is the search at the
/// head of it, and that one knows without a single file under it. So a column
/// searched down to nothing is still a column of photographs, and the rows do
/// not change shape underneath the user as a match arrives. A Steam library
/// answers the same way: the first game says the column is one of covers, and
/// it says so before a single cover has been fetched.
fn cards_in(entries: &[Entry]) -> Option<Cards> {
    for entry in entries {
        // Whichever way it answers, the first row that knows is the last one
        // asked: a shelf of music says "no cards" on its first song rather
        // than on its twenty-thousandth, and walking a whole library once per
        // column per frame is exactly the cost this shell does not pay.
        match entry {
            Entry::Media(file) => return file.kind.has_picture().then(|| Cards::of(CARD_ASPECT)),
            // A folder's field says nothing about cards, and says it without
            // stopping the walk: what a directory holds is folders, documents
            // and photographs together, so its column is rows — see the round
            // preview an explorer row draws instead.
            Entry::Search(search) => {
                let kind = search.of.shelf()?;
                return kind.has_picture().then(|| Cards::of(CARD_ASPECT));
            }
            Entry::Game(_) => return Some(Cards::of(COVER_ASPECT)),
            _ => {}
        }
    }
    None
}

/// The square out of the middle of a picture that a round hole shows, as
/// fractions of the whole picture: left, top, right, bottom.
///
/// A photograph is almost never square and a hole always is, so something has
/// to give. Stretching gives the wrong face; fitting gives a picture with two
/// empty crescents round it, which in a *column* of them reads as pictures of
/// several different sizes. What is left is the middle, which is where the
/// subject of a photograph is in the overwhelming majority of photographs — and
/// it is what every gallery on every phone does with a thumbnail grid.
///
/// The middle and not the top: a portrait picture cropped from the top would be
/// a column of foreheads.
fn round_crop(aspect: f32) -> [f32; 4] {
    // The fraction of the long side the short side is worth.
    let (keep_x, keep_y) = if aspect >= 1.0 {
        (1.0 / aspect, 1.0)
    } else {
        (1.0, aspect)
    };
    let (edge_x, edge_y) = ((1.0 - keep_x) / 2.0, (1.0 - keep_y) / 2.0);
    [edge_x, edge_y, 1.0 - edge_x, 1.0 - edge_y]
}

/// Lay out one display's bar.
///
/// `focused` is whether this is the display the controller and keyboard are
/// driving. Every display draws its own [`Cursor`], so the others show what
/// they are pointing at, dimmed, rather than a copy of this one.
///
/// `time` runs the selection pulse.
///
/// `typing` is whether the on-screen keyboard is typing into this display's
/// bar, which happens in exactly one place: the search field at the head of a
/// column of the user's own files. It is what puts the caret on that row, and
/// it is a per-display answer rather than something the row itself carries —
/// the board is up on one screen, and a caret blinking away on the other would
/// be a field nobody is typing into.
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
    typing: bool,
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
            clip: None,
            halo: 0.0,
            lines: 1,
        });
    }

    // Nothing found to launch. Said at the foot of the display rather than in
    // place of the bar, which is where it used to be said: the shell's own
    // Settings column has rows in it now, and a machine with no applications
    // on it is exactly the machine whose settings the user has come looking
    // for. With nothing at all in the catalogue this is still the only thing
    // drawn, because there is then no bar to say it under.
    if xmb.is_empty() {
        texts.push(Text {
            content: "No applications found".to_string(),
            x: cross_x,
            y: height - 72.0 * scale,
            size: 24.0 * scale,
            color: theme.text_soft.a(0.7 * attention),
            bold: false,
            max_width: width,
            align: TextAlign::Left,
            clip: None,
            halo: 0.0,
            lines: 1,
        });
    }

    let category_spacing = CATEGORY_SPACING * scale;
    let item_spacing = ITEM_SPACING * scale;
    // The note a column with nothing in it draws sits at the icon's distance,
    // because a column with no rows has no cards in it either. Every column
    // that does have rows takes its own; see [`gap_below`].
    let empty_gap_below = ITEM_GAP_BELOW * scale;

    // While the row glides sideways the column belongs to nobody: fade it out
    // with the old category and back in with the new one, as the original bar
    // does, instead of teleporting its contents.
    let category_travel = (cursor.category_position - cursor.selected_category as f32)
        .abs()
        .min(1.0);
    let column_alpha = (1.0 - category_travel) * attention;

    // Where each column of the path stands: the open one on the cross, and one
    // step to the left for every column it was opened through. A step in
    // carries the whole chain along by one, so a column takes the place of the
    // one it came out of and the far end of a long path leaves the screen
    // rather than the near end running off it.
    let columns = cursor.columns(xmb);
    let depth = cursor.depth_position();
    let column_x = |level: usize| bar_column_x(level as f32, depth, width, height);

    // --- the columns of the path taken -----------------------------------
    // Drawn first so the category row overlaps them, as on the real bar.
    if let Some(category) = cursor.current_category(xmb) {
        if category.entries.is_empty() && column_alpha > 0.01 {
            texts.push(Text {
                content: category.empty_note().to_string(),
                x: column_x(0) + ITEM_ICON_FOCUSED * ITEM_DISC * scale / 2.0 + 12.0 * scale,
                y: cross_y + empty_gap_below - 14.0 * scale,
                size: 22.0 * scale,
                color: theme.text_soft.a(0.7 * column_alpha),
                bold: false,
                max_width: width,
                align: TextAlign::Left,
                clip: None,
                halo: 0.0,
                lines: 1,
            });
        }
    }

    for (level, column) in columns.iter().enumerate() {
        if column_alpha <= 0.01 {
            break;
        }
        // How much this column is the one the user is standing in. It owns the
        // focus: the light, the glass, and the rows either side of the one
        // chosen all belong to whichever column this is 1 for.
        let active = 1.0 - (depth - level as f32).abs().min(1.0);
        // And how much of it is on screen at all. A column arrives as the bar
        // travels to it and then stays, however many are opened in front of
        // it; only one being stepped back out of drops away — and that one
        // drops away on a ramp of its own, so it is out of the way of the
        // column it is standing over rather than fading in step with the
        // slide. See [`departing`].
        let present = (1.0 - (level as f32 - depth)).clamp(0.0, 1.0);
        let present = match column.standing {
            Standing::Leaving => departing(present),
            Standing::Open | Standing::Behind => present,
        };
        if present <= 0.01 {
            continue;
        }

        // And how far back it stands: one step per column opened in front of
        // it, so the trail falls away behind the one the user is in rather
        // than lying flat behind it.
        let (near, clarity) = receded(depth - level as f32);

        let x = column_x(level);
        // What it has left of itself by the time it reaches the near edge.
        let clarity = clarity * leaving(x, scale);

        // A column of pictures is measured differently from a column of
        // applications: taller rows, further apart, and a text column that
        // clears a card rather than a disc.
        let cards = cards_in(column.entries);
        let item_spacing = match cards {
            Some(cards) => cards.spacing() * scale,
            None => item_spacing,
        };
        let gap_above = gap_above(cards) * scale;
        let gap_below = gap_below(cards) * scale;
        // Fixed text column: anchored to the focused entry's extent so labels
        // do not shuffle sideways as focus (and therefore icon size) moves
        // around.
        //
        // Its disc's extent, not its icon's — the same distinction the
        // category's label is measured with. The glass is half again the icon
        // standing on it, and a column measured from the icon puts the names
        // over the rim.
        //
        // The whole column recedes about its own icons, this offset included:
        // a label that kept its distance from an icon half the size would read
        // as a name that had drifted off the row it belongs to.
        let text_x = match cards {
            Some(cards) => x + (cards.reach() / 2.0 + 14.0) * scale * near,
            None => x + (ITEM_ICON_FOCUSED * ITEM_DISC / 2.0 + 12.0) * scale * near,
        };
        // A column gives up its half of the screen to the one opened in front
        // of it, over the same glide: a label that snapped to the shorter box
        // the instant a subcategory opened would re-wrap in front of the user.
        let text_max = lerp(
            (width - text_x - 48.0 * scale).max(0.0),
            (column_x(level + 1) - COLUMN_CLEAR * scale - text_x).max(0.0),
            depth - level as f32,
        );

        // Only the rows that can be on screen are looked at at all. A shelf of
        // the user's own files is as long as their home directory is full, and
        // a column is a screen tall: reading the whole list to throw all but
        // twenty of it away would put the size of somebody's photograph
        // collection into the cost of every frame the shell draws.
        let shown = rows_in_view(
            column.position,
            column.entries.len(),
            item_spacing * near,
            height,
        );
        for index in shown {
            let entry = &column.entries[index];
            let offset = index as f32 - column.position;
            // Its rows close on the one the column was opened from as it goes
            // back, since that is the row that stays: a column receding with
            // its spacing untouched would be a stack of shrinking icons that
            // had all drifted apart from one another.
            let y = if level == 0 {
                item_y(
                    offset,
                    cross_y,
                    gap_above,
                    gap_below,
                    item_spacing * near,
                    row_swell(cards, scale) * near,
                )
            } else {
                nested_y(
                    offset,
                    cross_y,
                    gap_below,
                    item_spacing * near,
                    row_swell(cards, scale) * near,
                )
            };

            // Skip rows that cannot be on screen.
            if y < -item_spacing || y > height + item_spacing {
                continue;
            }

            let distance = offset.abs();
            let focus = (1.0 - distance.min(1.0)) * active;
            // The row a column was opened from is what that column leaves
            // behind — the trail. Every other row of it exists only while the
            // column is the one in front.
            let trail = index == column.selected;
            // What each row has left of itself. Everything on its way off the
            // screen takes the leaving ramp, whichever way the bar is going:
            // a column stepped back out of goes whole, and a column stepped
            // *past* gives up everything but its trail. Both are lying over
            // the rows arriving in their place, and `active` — the pace of the
            // slide — is the pace they have to beat.
            let presence = match column.standing {
                _ if trail => present,
                Standing::Leaving => present,
                Standing::Behind => departing(active),
                Standing::Open => active,
            } * clarity;
            // Fade with distance so the column dissolves instead of ending
            // abruptly.
            let mut alpha = (1.0 - (distance / 6.0)).clamp(0.0, 1.0) * column_alpha * presence;
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
            // A column of pictures runs off the bottom of the display rather
            // than stopping short of it. An icon is a mark that has to be whole
            // to be read, so it dissolves while it is still clear of the edge;
            // a card cut off by the edge is a photograph with more underneath,
            // which is both true and the plainest way of saying it. Stopping
            // short cost the best part of a row, in the column that has the
            // most rows to show.
            let bottom_clearance = match cards {
                Some(cards) => -cards.height / 2.0,
                None => ITEM_ICON / 2.0 + 16.0,
            } * scale;
            alpha *= ((height - bottom_clearance - y) / fade_range).clamp(0.0, 1.0);
            if alpha <= 0.01 {
                continue;
            }

            // Chosen, rather than merely the row a column was opened from: the
            // trail keeps its rows but hands the light forward, so exactly one
            // row on screen is lit however deep the path runs.
            let selected = distance < 0.5 && active > 0.5;
            let icon_size = lerp(ITEM_ICON, ITEM_ICON_FOCUSED, focus) * scale * near;
            // A value set on a scale is drawn as the scale, in the room the
            // icon would have had — and the scale is a tall capsule where an
            // icon is a small square, so the light behind it and the glass
            // under it have to be that shape too. Worked out here rather than
            // in [`column_bar`] because the bloom is drawn before the row and
            // the groove *is* the row's glass: there is no disc under a bar.
            let groove = entry
                .bar()
                .map(|bar| (bar, column_bar_box(x, y, height, scale * near)));
            // The card a picture stands on, when this column is one of
            // pictures. It exists whether or not the picture has arrived, so
            // nothing moves when one does.
            let card = cards.map(|cards| {
                let h = lerp(cards.height, cards.focused, focus) * scale * near;
                let w = cards.width(h);
                [x - w / 2.0, y - h / 2.0, w, h]
            });

            if distance < 0.5 && active > 0.01 {
                // The breathing bloom behind the focused entry.
                //
                // It rides `active`, so as a subcategory opens the light
                // crosses from the row it was opened from to the row it opened
                // on instead of appearing twice or blinking once.
                //
                // Behind a card it is stretched to the card's own shape rather
                // than drawn as the square a disc wants. The bloom is one
                // radial drawing either way, and a square one big enough to
                // reach past a card's long side stands half its own height
                // above and below it — which is not a light behind an object,
                // it is a haze the object is somewhere inside.
                //
                // A bar takes the same treatment for the same reason, off its
                // own groove: a square light big enough to reach past a track
                // that is most of the screen tall would be a haze over the
                // whole column rather than a light behind one control. It is
                // spread on the short side, where the room is, so the groove
                // stands in a soft vertical band instead of a hard-edged one.
                let (glow_w, glow_h) = match (card, groove) {
                    (Some([_, _, w, h]), _) => {
                        let out = h * (0.30 + 0.04 * pulse);
                        (w + out * 2.0, h + out * 2.0)
                    }
                    (None, Some((_, [_, _, w, h]))) => {
                        let out = w * (3.4 + 0.4 * pulse);
                        (w + out * 2.0, h + out)
                    }
                    (None, None) => {
                        let glow = icon_size * (2.3 + 0.2 * pulse);
                        (glow, glow)
                    }
                };
                // And it is quieter behind a bar. The same light spread over a
                // shape this narrow arrives as a bright lamp with a control
                // standing in front of it: the groove goes on showing what is
                // behind it, and what is behind it would be the accent at full
                // strength rather than the wallpaper — so the one pane whose
                // whole job is to show a *colour* would be showing the shell's.
                let behind_bar = if groove.is_some() { 0.55 } else { 1.0 };
                quads.push(Quad {
                    x: x - glow_w / 2.0,
                    y: y - glow_h / 2.0,
                    w: glow_w,
                    h: glow_h,
                    slot: GLOW_SLOT,
                    color: theme
                        .accent
                        .a((0.34 + 0.26 * pulse) * alpha * active * behind_bar),
                    ..Quad::default()
                });
            }

            // The glass the row stands on: a disc under an icon, a card under
            // a picture, of the same material the menu's buttons are cut from,
            // so "this is the thing you have chosen" looks the same everywhere
            // in the shell.
            //
            // Under the chosen row only, where it is a disc — an unfocused
            // application is its icon and nothing else. Under every row where
            // it is a card, because a card is not decoration there: it is the
            // mount the picture is on, and a photograph with a transparent
            // corner or a drawing on a white ground needs something to sit on
            // whether or not it is the one being looked at.
            //
            // Never under a bar. A bar's groove is already a pane of this same
            // glass, cut to the shape of the control — see [`column_bar`] — and
            // a disc behind it would be a second, rounder object sitting under
            // the first with nothing in it: a button that had been pressed,
            // with a track lying across it.
            let plate = if groove.is_some() {
                None
            } else if card.is_some() {
                card
            } else if distance < 0.5 && active > 0.01 {
                let disc = icon_size * ITEM_DISC;
                Some([x - disc / 2.0, y - disc / 2.0, disc, disc])
            } else {
                None
            };
            if let Some([px, py, pw, ph]) = plate {
                // The chosen row's glass is fuller; the rest of a column of
                // pictures is a quieter mount so the pictures are what the
                // column is made of.
                let lit = if card.is_some() {
                    lerp(0.45, 1.0, focus * active)
                } else {
                    active
                };
                quads.push(Quad {
                    x: px,
                    y: py,
                    w: pw,
                    h: ph,
                    slot: SOLID_SLOT,
                    // Lightly stained, because what is behind it is already
                    // the accent: the bloom above shows *through* this, and
                    // tinting a purple bloom purple is how a pane stops
                    // looking like glass and starts looking like paint.
                    color: theme.accent.a(0.13 * lit),
                    radius: if card.is_some() {
                        ph * CARD_CORNER
                    } else {
                        ph / 2.0
                    },
                    thickness: DEPTH_CONTROL * scale,
                    frost: FROST_CONTROL,
                    gloss: GLOSS_FULL,
                    fade: alpha * lit,
                    ..Quad::default()
                });
            }

            // The picture itself, on the card, at its own shape. An unfocused
            // row gets one too — a column of pictures where only the chosen
            // one is a picture would be a column of empty cards.
            let held = match entry {
                // Steam's own cover, which is to a game exactly what a frame
                // is to a film: the thing the row is, rather than a mark
                // standing in for it.
                Entry::Game(game) => slots.cover(game.app_id),
                _ => entry.media().and_then(|file| slots.thumbnail(&file.path)),
            };
            // And whether that cover is drawn in colour. A game that is not on
            // this disk is drawn colourless — the same picture, with nothing in
            // it that says "now": the top half of the column, which is what can
            // be played, is the only part of it in colour. Nothing else in the
            // shell is drained; a photograph the user owns is not a photograph
            // they are missing.
            let colourless = match entry {
                Entry::Game(game) => slots.drain(game.app_id, game.installed),
                _ => 0.0,
            };
            let picture =
                card.zip(held.filter(|thumb| thumb.aspect.is_finite() && thumb.aspect > 0.0));
            if let Some(([cx, cy, cw, ch], thumb)) = picture {
                // The same border on all four sides, which is what the card
                // was sized around: a picture at the column's own shape fills
                // the room exactly and the mount is even the whole way round.
                // Anything else — a portrait photograph in a column of films —
                // is fitted inside that room and centred, and shows more glass
                // on the two sides it does not reach.
                let mount = cards.map_or(0.0, |cards| cards.mount(ch));
                let (room_w, room_h) = (cw - mount * 2.0, ch - mount * 2.0);
                // Fitted rather than filled: a photograph cropped to the
                // card's shape is a photograph with its subject cut off, and
                // the shell has no way of knowing which half mattered.
                let (w, h) = if thumb.aspect >= room_w / room_h {
                    (room_w, room_w / thumb.aspect)
                } else {
                    (room_h * thumb.aspect, room_h)
                };
                quads.push(Quad {
                    x: cx + (cw - w) / 2.0,
                    y: cy + (ch - h) / 2.0,
                    w,
                    h,
                    slot: thumb.slot,
                    color: [1.0, 1.0, 1.0, alpha],
                    radius: (ch * CARD_CORNER - mount).max(0.0),
                    drain: colourless,
                    ..Quad::default()
                });
            }

            // The picture a file in a folder is *of*, in the round hole its glyph
            // would have had.
            //
            // Round, and cut from the middle, because this column is not a
            // column of pictures: a folder holds folders, documents and
            // photographs together, so the rows have to stay rows — one shape,
            // one rhythm, a picture where the mark of a kind would otherwise
            // be. The shelves answer the same question the other way round,
            // with cards, because everything on one of those *is* a picture and
            // the column can be made of them.
            let preview = entry
                .file()
                .and_then(|file| slots.thumbnail(&file.path))
                .filter(|thumb| thumb.aspect.is_finite() && thumb.aspect > 0.0);
            if let Some(thumb) = preview {
                let size = icon_size * ITEM_PREVIEW;
                quads.push(Quad {
                    x: x - size / 2.0,
                    y: y - size / 2.0,
                    w: size,
                    h: size,
                    slot: thumb.slot,
                    color: [1.0, 1.0, 1.0, alpha],
                    radius: size / 2.0,
                    crop: round_crop(thumb.aspect),
                    ..Quad::default()
                });
            }

            // A row standing for a colour is drawn *in* that colour: the atlas
            // multiplies the quad's colour into the texel, so the swatch is
            // one white drawing tinted rather than a drawing per colour. It
            // stands in for the missing-icon tint too — a colour with no
            // drawing behind it is still that colour.
            let tint = entry.swatch().map(|swatch| swatch.a(alpha));
            // The scale itself, in the room the icon would have had. It is the
            // whole of the row: there is no glyph beside it, because the name
            // of the setting is on the row this column was opened from and is
            // still on screen to the left.
            if let Some((bar, box_)) = groove {
                quads.extend(column_bar(box_, bar, alpha, active));
            } else if picture.is_none() && preview.is_none() {
                // Only where there is no picture. A row that drew both would be
                // a film strip stamped over the frame it stands for.
                let mut icon = icon_quad(
                    entry_slot(entry, slots),
                    x - icon_size / 2.0,
                    y - icon_size / 2.0,
                    icon_size,
                    alpha,
                    tint.unwrap_or_else(|| theme.accent_deep.a(alpha * 0.75)),
                );
                if let Some(tint) = tint {
                    icon.color = tint;
                }
                quads.push(icon);
            }

            // The mark that says this row is the value in force. On the icon
            // rather than beside the label, because what it is marking is the
            // swatch: the colour is the answer, and the word beside it is only
            // its name.
            if entry.chosen() {
                let badge = icon_size * CHOSEN_BADGE;
                quads.push(icon_quad(
                    slots.glyph(icons::CHOSEN),
                    x + icon_size * CHOSEN_BADGE_AT - badge / 2.0,
                    y + icon_size * CHOSEN_BADGE_AT - badge / 2.0,
                    badge,
                    alpha,
                    theme.rim.a(alpha),
                ));
            }

            // The row being typed into, if this is it. The open column only:
            // the same field is drawn on the trail behind a step further in,
            // and a caret back there would say the keyboard was going to that
            // one.
            let field = (typing && selected && column.standing == Standing::Open)
                .then(|| entry.search())
                .flatten()
                .filter(|search| search.role == Role::Field);

            if selected {
                let name_size = 30.0 * scale * near;
                let comment = entry.comment();
                // With a comment the pair straddles the icon's centre line;
                // without one the name alone sits on it.
                let name_y = if comment.is_some() {
                    y - 40.0 * scale * near
                } else {
                    y - name_size * 0.62
                };
                texts.push(Text {
                    // The query and the caret, rather than what the row is
                    // called. They part company on an empty field: the row is
                    // called "Search" while nobody is in it, and a field being
                    // typed into that still said so would be a field with a
                    // word already in it.
                    content: match field {
                        Some(search) => format!("{}{CARET}", search.query),
                        None => entry.title().to_string(),
                    },
                    x: text_x,
                    y: name_y,
                    size: name_size,
                    color: theme.text.a(alpha),
                    bold: true,
                    max_width: text_max,
                    align: TextAlign::Left,
                    clip: None,
                    halo: 0.0,
                    lines: 1,
                });
                if let Some(comment) = comment {
                    texts.push(Text {
                        content: comment.to_string(),
                        x: text_x,
                        y: y + 4.0 * scale * near,
                        size: 19.0 * scale * near,
                        color: theme.text_soft.a(alpha * 0.85),
                        bold: false,
                        max_width: text_max,
                        align: TextAlign::Left,
                        clip: None,
                        halo: 0.0,
                        lines: 1,
                    });
                }
            } else {
                let text_size = 22.0 * scale * near;
                // A row on the trail is read rather than skimmed: it is the
                // one thing on screen saying how the user got here, so it
                // keeps more ink than a neighbour in the open column.
                let ink = if trail { 0.85 } else { 0.62 };
                texts.push(Text {
                    content: entry.title().to_string(),
                    x: text_x,
                    y: y - text_size * 0.62,
                    size: text_size,
                    color: theme.text.a(alpha * ink),
                    bold: false,
                    max_width: text_max,
                    align: TextAlign::Left,
                    clip: None,
                    halo: 0.0,
                    lines: 1,
                });
            }
        }
    }

    // --- the category row ------------------------------------------------
    // It rides the same slide the columns do, so the category a path was
    // opened from stays directly above the head of that path.
    let inside = depth.clamp(0.0, 1.0);
    // How much of the way out of the row the bar has come, for the purpose of
    // taking the row's ink away: the ramp a column leaves on rather than the
    // slide itself, and for exactly the reason [`departing`] exists.
    //
    // The row used to fade in step with the move, which put it at half
    // strength across the middle of the step in — while the column being
    // opened was at half strength coming up over it. That is two rows of
    // things printed over one another, and a shelf is the worst case of it:
    // the pictures are wide, they cross the whole row, and what they land on
    // is a rank of half-lit icons rather than the wallpaper.
    let stepped_in = 1.0 - departing(1.0 - inside);
    // The row stands a step behind the outermost column, which is a step
    // behind the one in front of that, and so on to the column the user is
    // in — so at two subcategories deep the row is three steps back. The extra
    // step arrives over the first move rather than the instant one begins,
    // because at the top of a column the row is not behind anything: it is
    // the thing the cursor is on.
    let (row_near, row_clarity) = category_row_recession(depth);
    let category_spacing = category_spacing * row_near;
    for (index, category) in xmb.categories.iter().enumerate() {
        let offset = index as f32 - cursor.category_position;
        let x = bar_category_x(offset, depth, width, height);

        if x < -category_spacing || x > width + category_spacing {
            continue;
        }

        let distance = offset.abs();
        let focus = 1.0 - distance.min(1.0);
        let selected = distance < 0.5;
        // Fades over the half-step where selection hands over, so during a
        // glide the glow and label cross-fade between neighbours instead
        // of blinking.
        let handover = (1.0 - distance * 2.0).clamp(0.0, 1.0);
        // Unlike the item column, the category row never fades a category out:
        // the row is the map of where everything lives, so every category that
        // fits on screen has to stay readable. Distance only dims it, down to a
        // floor, to keep the selected one obviously selected.
        //
        // Inside a subcategory it does fade, and to nothing: the map is not
        // where the user is standing, and the row's other columns are not
        // reachable from in there — Left comes back out first. What is left is
        // the one category the path hangs off. It goes on the way *in*, not
        // over the whole slide — see `stepped_in` — so the column opening has
        // clear ground to arrive on.
        //
        // And it dissolves at the near edge as a deepening path carries it
        // off, for the same reason a column does — but only for that reason:
        // at the top of a column the categories before the first stand where
        // they have always stood, half off the screen and none the worse.
        //
        // The button, the icon on it and the name under it all take this one
        // number. They are one object: a cog that had gone while the word
        // "Settings" was still sitting under it would not read as a category
        // leaving, it would read as a missing icon.
        let gone = lerp(1.0, leaving(x, scale), inside);
        let alpha = (1.0 - distance * 0.10).clamp(CATEGORY_MIN_ALPHA, 1.0)
            * attention
            * (1.0 - stepped_in * (1.0 - handover))
            * row_clarity
            * gone;
        let icon_size = lerp(CATEGORY_ICON, CATEGORY_ICON_FOCUSED, focus) * scale * row_near;

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
            // The light on the button goes as the path leaves it: what the
            // user is pointing at is a row in a column now, and two lit things
            // on screen would be two answers to the same question. The icon
            // and its label stay — that is the head of the trail — but the
            // button underneath stops being the thing chosen.
            let lit = handover * attention * (1.0 - inside);

            let glow = icon_size * (2.0 + 0.2 * pulse);
            quads.push(Quad {
                x: x - glow / 2.0,
                y: cross_y - glow / 2.0,
                w: glow,
                h: glow,
                slot: GLOW_SLOT,
                color: theme.accent.a((0.28 + 0.24 * pulse) * lit),
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
                fade: lit,
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
            //
            // Its distance below the button recedes with the button, for the
            // same reason a row's label keeps its distance from its own icon:
            // a name left where a full-size button's would have been is a name
            // that has come away from the thing it names.
            let label_size = CATEGORY_LABEL * scale * row_near;
            let box_w = category_spacing * 1.7;
            texts.push(Text {
                content: category.title.to_string(),
                x: x - box_w / 2.0,
                y: cross_y
                    + (CATEGORY_ICON_FOCUSED * CATEGORY_DISC / 2.0 + CATEGORY_LABEL_ABOVE)
                        * scale
                        * row_near,
                size: label_size,
                color: theme.text.a(handover * attention * row_clarity * gone),
                bold: true,
                max_width: box_w,
                align: TextAlign::Center,
                clip: None,
                halo: 0.0,
                lines: 1,
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

/// How far apart one column of a path stands from the next, on a display this
/// size.
fn subcolumn_step(height: f32) -> f32 {
    SUBCOLUMN_STEP * guide_scale(height)
}

/// Where the column at `level` of the open path stands.
///
/// The whole chain slides one step left per column opened, so this is the cross
/// less however far in the bar has travelled — which is why the level is taken
/// as a float: nothing here is at a whole number of steps while a path is being
/// walked into or out of.
fn bar_column_x(level: f32, depth: f32, width: f32, height: f32) -> f32 {
    width * BAR_CROSS_X + (level - depth) * subcolumn_step(height)
}

/// How far back the category row stands, and what that leaves of it: one step
/// behind the outermost column, and another once a path has been opened at all.
fn category_row_recession(depth: f32) -> (f32, f32) {
    receded(depth + depth.clamp(0.0, 1.0))
}

/// Where a category `offset` places along the row from the one selected stands.
fn bar_category_x(offset: f32, depth: f32, width: f32, height: f32) -> f32 {
    let (row_near, _) = category_row_recession(depth);
    bar_column_x(0.0, depth, width, height)
        + offset * CATEGORY_SPACING * guide_scale(height) * row_near
}

/// The vertical centre of the row `offset` item-units from a column's selection,
/// when that column stands `near` of full size.
///
/// `level` is how far along the path the column stands, and only the outermost
/// — level 0 — is laid out around the category row. See [`nested_y`].
fn bar_item_y(offset: f32, level: usize, near: f32, height: f32, cards: Option<Cards>) -> f32 {
    let scale = guide_scale(height);
    let cross_y = height * BAR_CROSS_Y;
    let spacing = cards.map_or(ITEM_SPACING, |cards| cards.spacing()) * scale * near;
    if level == 0 {
        item_y(
            offset,
            cross_y,
            gap_above(cards) * scale,
            gap_below(cards) * scale,
            spacing,
            row_swell(cards, scale) * near,
        )
    } else {
        nested_y(
            offset,
            cross_y,
            gap_below(cards) * scale,
            spacing,
            row_swell(cards, scale) * near,
        )
    }
}

/// What a click at `(x, y)` on the start screen has landed on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BarSpot {
    /// A button on the category row.
    Category(usize),
    /// A row of the column the user is standing in. `level` is where along the
    /// groove the point fell, for a row that is a bar — the one row in the tree
    /// a press says something *with* rather than merely to, exactly as the
    /// guide's own bars and the mixer's rows do. `None` for every other row,
    /// and for a point that was on a bar's row but not on its track.
    Item { row: usize, level: Option<f32> },
    /// The row a column further back was opened from — the trail. Carries how
    /// many columns back it is, which is how many steps out reach it.
    Trail(usize),
}

/// What is under `(x, y)` on the start screen, if anything.
///
/// The order is the order the bar is drawn in, back to front: the category row
/// overlaps the columns, so it is asked first, and a click in the band it
/// occupies belongs to it even where a column's rows are gliding through.
///
/// Bands rather than the drawn discs. Every icon on the bar has a name beside
/// it and air around it, and a user aiming at a row is aiming at the row, not
/// at the circle of glass under its picture — so a column's rows divide the
/// height between them and a category's buttons divide the width, and there is
/// nowhere on the cross that belongs to nothing.
pub fn bar_hit(
    xmb: &Xmb,
    cursor: &Cursor,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
) -> Option<BarSpot> {
    if xmb.is_empty() {
        return None;
    }
    let depth = cursor.depth_position();
    category_row_hit(xmb, cursor, x, y, width, height, depth)
        .or_else(|| column_hit(xmb, cursor, x, y, width, height, depth))
}

/// The category row's half of [`bar_hit`].
#[allow(clippy::too_many_arguments)]
fn category_row_hit(
    xmb: &Xmb,
    cursor: &Cursor,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    depth: f32,
) -> Option<BarSpot> {
    let scale = guide_scale(height);
    let cross_y = height * BAR_CROSS_Y;
    let (row_near, _) = category_row_recession(depth);
    // The band the eye reads as "the row": the height a focused button takes,
    // held to that whatever size the one being pointed at currently is, so the
    // row does not grow and shrink under the hand crossing it.
    let band = CATEGORY_ICON_FOCUSED * CATEGORY_DISC * scale * row_near;
    if (y - cross_y).abs() > band * 0.5 {
        return None;
    }

    let spacing = CATEGORY_SPACING * scale * row_near;
    let offset = (x - bar_column_x(0.0, depth, width, height)) / spacing;
    let index = (cursor.category_position + offset).round();
    if index < 0.0 || index as usize >= xmb.categories.len() {
        return None;
    }
    let index = index as usize;

    // Inside a path the row has faded out but for the one category the path
    // hangs off, which stays as the head of the trail. So that button is the
    // only thing up here a click can still be about, and what it means is the
    // same as clicking any other row of the trail: back out to it.
    let steps_back = depth.round() as usize;
    if steps_back > 0 {
        return (index == cursor.selected_category).then_some(BarSpot::Trail(steps_back));
    }
    Some(BarSpot::Category(index))
}

/// The columns' half of [`bar_hit`], front to back — the order they stand in.
#[allow(clippy::too_many_arguments)]
fn column_hit(
    xmb: &Xmb,
    cursor: &Cursor,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    depth: f32,
) -> Option<BarSpot> {
    let scale = guide_scale(height);
    let columns = cursor.columns(xmb);
    for (level, column) in columns.iter().enumerate().rev() {
        // A column the bar has stepped out of is not something the hand can be
        // pointing at. It is on its way off the screen — and once the step has
        // settled it is not on the screen at all, only still held so that going
        // straight back in returns to the row it was left on. Answering from it
        // meant a click on the right-hand side of the open column's names
        // coming back as a row of the column the user had just left.
        if column.standing == Standing::Leaving {
            continue;
        }
        let (near, _) = receded(depth - level as f32);
        // The strip this column has to itself: from its own glass across to
        // where the next one's begins, or to the far side of the display for
        // the one in front. That is the box its labels are laid into, and a
        // name is as much the row as its icon is.
        // A column of pictures is wider and taller than one of applications,
        // and the hand has to land where the eye says the row is.
        let cards = cards_in(column.entries);
        let row_reach =
            cards.map_or(ITEM_ICON_FOCUSED * ITEM_DISC, |cards| cards.reach()) * scale * near;
        let column_x = bar_column_x(level as f32, depth, width, height);
        let left = column_x - row_reach * 0.5;
        // The strip ends where the next column *the user can act on* begins, so
        // a column stepped out of does not go on holding back the one it came
        // out of: half the open column would answer to nothing until the next
        // move dropped the one being kept.
        let right = match columns
            .get(level + 1)
            .filter(|next| next.standing != Standing::Leaving)
        {
            Some(_) => {
                bar_column_x(level as f32 + 1.0, depth, width, height) - COLUMN_CLEAR * scale
            }
            None => width,
        };
        if x < left || x > right {
            continue;
        }

        let band = cards.map_or(ITEM_SPACING, |cards| cards.spacing()) * scale * near;
        let row_at =
            |index: usize| bar_item_y(index as f32 - column.position, level, near, height, cards);

        // A column behind the open one shows one row and no more — the row it
        // was opened from — so that is the only thing in it a click can mean,
        // and what it means is stepping back out to it.
        let steps_back = (depth - level as f32).round();
        if steps_back >= 1.0 {
            if (y - row_at(column.selected)).abs() <= band * 0.5 {
                return Some(BarSpot::Trail(steps_back as usize));
            }
            continue;
        }

        // The open column: the row whose band `y` falls in. Walked rather than
        // solved, because neither layout is one line — an application column is
        // two, either side of the gap the category row sits in, and a column of
        // pictures has no single pitch at all — and what is walked is a
        // screen's worth of rows rather than the list, which is what keeps a
        // click off the size of the user's collection.
        //
        // Each row reaches half the way to the row on either side of it, which
        // is the only description that leaves nothing between two rows
        // belonging to neither. The pitch would do where every row is the same
        // distance from the next; in a column of pictures the chosen card
        // pushes its neighbours out, so the pair either side of it stand
        // further apart than the pitch and a strip of each of those gaps would
        // answer to nothing.
        for index in rows_in_view(column.position, column.entries.len(), band, height) {
            let row_y = row_at(index);
            if row_y < -band || row_y > height + band {
                continue;
            }
            // A bar is not a row-sized thing. Its groove is a track most of the
            // column tall — see [`column_bar`] — and the whole of that track is
            // the control: what a press on it asks for is decided by *where*
            // along it the press was made, the way it is on the guide's bars and
            // on the mixer's rows. So the track is asked before the row's own
            // band, which is one icon high and would answer for the middle of a
            // track that reaches most of the way up the screen.
            //
            // It cannot take a click off the row above or below it: a bar is the
            // whole of its column, which is what makes the number on it readable
            // as the value — the name of the setting is on the row the column
            // was opened from. See [`crate::apps::Bar`].
            if column.entries[index].bar().is_some() {
                let track = column_bar_box(column_x, row_y, height, scale * near);
                if let Some(level) = column_bar_level_at(track, row_reach, x, y) {
                    return Some(BarSpot::Item {
                        row: index,
                        level: Some(level),
                    });
                }
            }
            let reach = |neighbour: f32| {
                let step = (neighbour - row_y).abs();
                // The ends of the column have a row on one side only, and there
                // the band is the one it would have had on the other.
                if step > 0.0 {
                    step * 0.5
                } else {
                    band * 0.5
                }
            };
            let above = reach(row_at(index.saturating_sub(1)));
            let below = reach(row_at((index + 1).min(column.entries.len() - 1)));
            if y >= row_y - above && y <= row_y + below {
                return Some(BarSpot::Item {
                    row: index,
                    level: None,
                });
            }
        }
    }
    None
}

/// How much of something standing at `x` has not yet left the screen.
///
/// The chain slides one step left for every subcategory opened, so the far end
/// of a long path eventually reaches the edge and goes. It dissolves as it
/// crosses rather than sliding out under the boundary: a row is an icon with a
/// *label* beside it, and text cut off at the edge reads as a fault in the
/// layout rather than as something leaving.
///
/// It holds its full strength right up to that edge, though. This used to
/// begin fading a whole [`COLUMN_MARGIN`] before it, which is the distance the
/// outermost column stands at when a path is one subcategory deep — so on any
/// display narrower than the 16:9 the step is measured against, the first step
/// in already had the category half faded out while it was still sitting in
/// plain sight. Hanging over the side is ordinary; the bar is wider than the
/// screen and always has been. Only what has actually gone past is worth
/// taking away, and one icon's width past centre is where that has happened.
fn leaving(x: f32, scale: f32) -> f32 {
    let band = ITEM_ICON * scale;
    ((x + band) / band).clamp(0.0, 1.0)
}

/// Vertical centre of the item at `offset` (in item units from the selection).
///
/// Offsets at or past 0 stack below the category row from `gap_below`;
/// offsets at or past −1 stack above it from `gap_above`. In between — an
/// entry mid-scroll — the position blends linearly, so the entry glides
/// through the row's gap instead of jumping over it.
///
/// `swell` is how much taller the chosen row is drawn than the rest, and it
/// pushes the row below it down by half of that — the same correction
/// [`nested_y`] makes, and for the same reason: without it the chosen card
/// nearly touches the row under it while every other pair stands well apart.
///
/// Only downwards, though, and that is the whole difference between the two.
/// Here the row *above* the chosen one is on the other side of the category
/// row, which is a hundred and fifty pixels of glass, a label and its air. It
/// is not a neighbour of the chosen card in any sense the eye recognises, and
/// pushing it further away would only take a row off the top of the screen.
fn item_y(
    offset: f32,
    cross_y: f32,
    gap_above: f32,
    gap_below: f32,
    spacing: f32,
    swell: f32,
) -> f32 {
    let pushed = swell * 0.5 * offset.clamp(0.0, 1.0);
    if offset >= 0.0 {
        cross_y + gap_below + offset * spacing + pushed
    } else if offset <= -1.0 {
        cross_y - gap_above + (offset + 1.0) * spacing
    } else {
        cross_y + gap_below + offset * (gap_below + gap_above)
    }
}

/// Where a row of a column that is *not* the outermost sits, `offset` rows from
/// the chosen one — which stands where [`item_y`] would put it, so the row a
/// column was opened from and the row it opened on are level.
///
/// One step throughout, about the chosen row, instead of two lines either side
/// of the gap the category row sits in. Only the outermost column straddles
/// that row. Step into a subcategory and the category row recedes to the far
/// left, keeping the one it was opened from and nothing else, and the band
/// across the middle of the display is empty — but every column went on holding
/// it open, at a cost of two and a half rows in a library and three in a
/// column of settings.
///
/// `swell` is how much taller the chosen row is drawn than its neighbours, and
/// it pushes what is above it up and what is below it down by half each, so
/// that the clear space between any two neighbours is the same whichever row is
/// chosen. A column of pictures needs this and a column of icons does not: a
/// card is drawn under every row, and the chosen one is nearly twice the rest,
/// so on a plain step it eats the gap on both sides and sits almost touching
/// them while the pairs further off stand well apart. An unfocused application
/// is its icon and nothing else, which leaves the room already.
///
/// The clamp is what makes that exact rather than approximate. A row two or
/// more away is pushed by the full half — the swelling between them is over and
/// done with — and one part-way there is pushed in proportion, which is exactly
/// how much the row between them has swollen by. So the gap holds not only when
/// the cursor is settled on a row but all the way through the glide to the next.
fn nested_y(offset: f32, cross_y: f32, gap_below: f32, spacing: f32, swell: f32) -> f32 {
    cross_y + gap_below + offset * spacing + swell * 0.5 * offset.clamp(-1.0, 1.0)
}

/// Which rows of a column can be on the display, when it is drawn at
/// `position` with `pitch` between one row and the next.
///
/// The bound the drawing and the hit test are both cut down to, and the reason
/// neither of them costs anything in proportion to how much music somebody
/// owns. A column is a screen tall however long its list is, so the rows that
/// can be seen are the ones within a screen's worth of the position it is
/// drawn at, and everything else is a row whose `y` the caller would work out
/// only to throw away.
///
/// Generous rather than exact: both [`item_y`] and [`nested_y`] step by the
/// pitch and then move a row or so about — the gap the category row sits in,
/// the swell of a chosen card — so this answers with three rows more than the
/// arithmetic needs at either end, and the callers still ask of each row it
/// returns whether that row is really on screen. What it must never do is
/// leave out a row that is, which is why it errs the way it does.
fn rows_in_view(position: f32, rows: usize, pitch: f32, height: f32) -> std::ops::Range<usize> {
    if pitch <= 0.0 || !pitch.is_finite() || !position.is_finite() {
        return 0..rows;
    }
    let reach = height / pitch + 3.0;
    let from = (position - reach).clamp(0.0, rows as f32) as usize;
    let to = ((position + reach).clamp(0.0, rows as f32) as usize + 1).min(rows);
    from..to.max(from)
}

/// How much taller a column's chosen row is drawn than the rest — see
/// [`nested_y`]. Only a column of pictures has any.
fn row_swell(cards: Option<Cards>, scale: f32) -> f32 {
    cards.map_or(0.0, |cards| cards.swell() * scale)
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
    /// Whether announcements are being kept out of the corner of the screen.
    /// The other switch on the tile line, and the one that is about the
    /// session rather than about what is in front of it.
    pub do_not_disturb: bool,
    /// How far the unread mark on the bell has arrived: 0 for a bell with
    /// nothing to say, 1 for one carrying it. A position rather than a flag
    /// because it grows on and goes out again, and the tile is in plain sight
    /// while it does both.
    pub unread: f32,
    /// The foreground application's title, for the sidebar's header.
    pub app: Option<&'a str>,
    /// Name of the application the Close entry would end — the one behind the
    /// selected card, and the application rather than the window, because that
    /// is what Close acts on. `None` when the card is the start screen, which
    /// cannot be closed and so is why the entry is absent rather than merely
    /// unlabelled.
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
            Item::DoNotDisturb => self.do_not_disturb,
            // The other two raise a panel rather than holding a state, so
            // there is nothing for them to be *on*: what a press does is
            // visible in the panel that grows out of them.
            _ => false,
        }
    }

    /// How far this tile's unread mark has arrived. Only the bell has one:
    /// it is the only tile that holds anything that can be missed.
    fn tile_badge(&self, item: Item) -> f32 {
        match item {
            Item::Notifications => self.unread,
            _ => 0.0,
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
    let sidebar_x = sidebar_slide_x(age, width);
    let [panel_x, panel_y, panel_w, panel_h] = sidebar_panel_rect(width, height);
    let padding = GUIDE_PADDING * scale;
    let text_x = panel_x + padding * 0.8;
    let text_w = panel_w - padding * 1.6;

    // --- the sidebar -------------------------------------------------------
    // A clearer cut of glass over two restrained pools of the active accent.
    // The wallpaper's current now remains visible across the broad face, and
    // the light under it gives the header and foot depth without turning the
    // entire column into one bright, predetermined slab.
    quads.extend(sidebar_surface(
        [sidebar_x + panel_x, panel_y, panel_w, panel_h],
        scale,
        view.behind,
        slide,
    ));

    let title_size = 34.0 * scale;
    let subtitle_size = 18.0 * scale;
    texts.push(Text {
        content: match &view.clock {
            Some(clock) => clock.time.to_string(),
            None => "LineXinBar".to_string(),
        },
        x: sidebar_x + text_x,
        y: 52.0 * scale,
        size: title_size,
        color: theme.text.a(0.95 * slide),
        bold: true,
        max_width: text_w,
        align: TextAlign::Left,
        clip: None,
        halo: 0.0,
        lines: 1,
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
            clip: None,
            halo: 0.0,
            lines: 1,
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
        clip: None,
        halo: 0.0,
        lines: 1,
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
            clip: None,
            halo: 0.0,
            lines: 1,
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
                    badge: view.tile_badge(*item),
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
        let label_padding = GUIDE_LABEL_PADDING * scale;
        texts.push(Text {
            content: item.label(view.close_target),
            x: sidebar_x + rx + label_padding + drift,
            // Centred in its chip now that no description shares the row.
            y: ry + rh * 0.5 - label_size * 0.62,
            size: label_size,
            color: theme
                .text
                .a(if focused { 1.0 } else { 0.78 } * appear * slide),
            bold: focused,
            max_width: (rw - label_padding * 2.0).max(0.0),
            align: TextAlign::Left,
            clip: None,
            halo: 0.0,
            lines: 1,
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
            clip: None,
            halo: 0.0,
            lines: 1,
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
    let panel_w = (POWER_WIDTH * scale).min(width * 0.8);
    let panel_h = (78.0 + 14.0) * scale + POWER_ROW * scale * rows as f32;
    [
        (width - panel_w) * 0.5,
        (height - panel_h) * 0.5,
        panel_w,
        panel_h,
    ]
}

/// The capsule of power choice `index`, in the panel's settled coordinates.
///
/// Shared by the drawing and by the hit test, for the reason every rectangle
/// here is shared: two places computing one shape have to agree, and a row that
/// answers to a click a few pixels away from where it is drawn is worse than a
/// row that cannot be clicked at all.
pub fn power_dialog_row_rect(width: f32, height: f32, rows: usize, index: usize) -> [f32; 4] {
    let scale = guide_scale(height);
    let [panel_x, panel_y, panel_w, _] = power_dialog_rect(width, height, rows);
    let row_h = POWER_ROW * scale;
    let title_h = 78.0 * scale;
    let inset = 12.0 * scale;
    let capsule = row_h - 8.0 * scale;
    [
        panel_x + inset,
        panel_y + title_h + index as f32 * row_h + 4.0 * scale,
        panel_w - inset * 2.0,
        capsule,
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
pub fn recede_behind_power_dialog(
    scene: &mut Scene,
    width: f32,
    height: f32,
    rows: usize,
    progress: f32,
) {
    scene.fade(lerp(1.0, POWER_DIM, progress));
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
    recede_behind_power_dialog(scene, width, height, items.len(), open);

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
    let row_h = POWER_ROW * scale;
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
        clip: None,
        halo: 0.0,
        lines: 1,
    });

    let label_size = 22.0 * scale;
    for (index, item) in items.iter().enumerate() {
        let y = panel_y + title_h + index as f32 * row_h;
        let focused = index == selected;
        let [row_x, row_y, row_w, capsule] =
            power_dialog_row_rect(width, height, items.len(), index);
        // Warm for the two choices there is no coming back from, so the
        // difference is visible before the label is read.
        let tint = if item.is_grave() {
            theme.danger
        } else {
            theme.accent
        };
        inside.quads.push(Quad {
            x: row_x,
            y: row_y,
            w: row_w,
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
            clip: None,
            halo: 0.0,
            lines: 1,
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
        inside.fade(ease((open - POWER_CONTENT_IN) / (1.0 - POWER_CONTENT_IN)));
    }
    scene.quads.extend(inside.quads);
    scene.texts.extend(inside.texts);
}

// --- the context menu ------------------------------------------------------

/// The panel's width, and the line each row is given, against the same 1080p
/// reference as everything else.
///
/// Wide enough that a command reads as a sentence rather than as a word that
/// had to be shortened — "Move to Other Screen" is the sort of thing that goes
/// on one of these — and no wider, because the menu is a note attached to an
/// object on screen and must not read as a screen of its own.
const CONTEXT_WIDTH: f32 = 440.0;
/// What the announcement panels ask for on top of it — see [`Menu::widen`].
///
/// They are the exception the width was written against: their rows carry
/// somebody else's sentences rather than a command the shell named to fit, and
/// each of them a button as well. The extra hundred is the difference between
/// a summary that wraps to three lines and one that fits on two.
pub const NOTIFICATION_EXTRA_WIDTH: f32 = 100.0;
const CONTEXT_ROW: f32 = 64.0;
/// What a row that carries a track is given instead.
///
/// Half again as tall, because it holds two things stacked rather than one:
/// what the sound is, and how loud it is. Squeezing both onto a command's row
/// would put a name and a track on one line, and the track is the thing being
/// aimed at — it wants the width of the panel, not whatever is left after a
/// name has taken its share.
const MIXER_ROW: f32 = 96.0;
/// The application's icon at the head of such a row, as a share of the chip,
/// and the air between it and what it labels.
const MIXER_ICON: f32 = 0.62;
const MIXER_ICON_GAP: f32 = 14.0;
/// The same on a plain command row, for a row that is about an application
/// rather than about an act — the Open with list is a column of programs.
///
/// Larger than a glyph on the same row, and deliberately: one of the shell's
/// own marks is a symbol standing for a verb, and this is a picture of a
/// particular program, which is the thing the user is picking between. Below
/// [`MIXER_ICON`] because the row it is on is two thirds as tall.
const CONTEXT_ICON: f32 = 0.7;
/// One of the shell's own marks at the head of a row instead, as a share of the
/// chip — smaller than a picture, being a symbol rather than a portrait.
const CONTEXT_GLYPH: f32 = 0.46;
/// The button on the right-hand end of a row — see [`crate::menu::Aside`]: how
/// wide it is as a share of the row's settled chip, the air between it and the
/// writing, and the mark on it as a share of its own width.
///
/// Square on a row that has not opened out, and the full height of the row
/// whether it has or not — see [`context_menu_aside_rect`]. It is a button
/// rather than a mark on the row, and the shell draws a button as something
/// with a face that can be aimed at — the same reason the guide's tiles are
/// squares.
const CONTEXT_ASIDE: f32 = 1.0;
const CONTEXT_ASIDE_GAP: f32 = 12.0;
const CONTEXT_ASIDE_GLYPH: f32 = 0.42;
/// Where the name and the track sit inside the chip, as shares of its height:
/// the middle of the line the name is on, and the middle of the track's own.
const MIXER_NAME_LINE: f32 = 0.32;
const MIXER_TRACK_LINE: f32 = 0.70;
/// The speaker at the head of the track, as a share of the chip's height, and
/// the air after it.
const MIXER_GLYPH: f32 = 0.24;
const MIXER_GLYPH_GAP: f32 = 10.0;
/// The header, when the menu was raised with a title: the name of the thing
/// being acted on, the air under it, and the hairline that separates it from
/// the commands.
const CONTEXT_TITLE: f32 = 62.0;
/// What that name is set at, and what each line of it takes — the same 1.25
/// the renderer spaces lines by, because a header that wraps has to be given
/// exactly what the words will occupy.
const CONTEXT_TITLE_SIZE: f32 = 24.0;
const CONTEXT_TITLE_LINE: f32 = CONTEXT_TITLE_SIZE * 1.25;
/// The space a change of band opens between two rows, and where the rule in it
/// is drawn. The guide's own separator gap would be generous here: that column
/// is the height of a display and this one is a handful of rows.
const CONTEXT_GROUP_GAP: f32 = 20.0;
/// How far the panel floats clear of the control it is about.
const CONTEXT_GAP: f32 = 16.0;
/// How far the selection's halo reaches beyond the chip it is under, as a share
/// of a command's row. What a chip 2.6 times its own height gave a command row,
/// held as a distance so that a taller row is not given a taller glow.
const CONTEXT_GLOW_REACH: f32 = 1.35;
/// The strip reserved at each end of the column for the arrow that says the
/// list carries on. Reserved at *both* ends as soon as the list scrolls at
/// all, so that scrolling moves the rows and never resizes the panel.
const CONTEXT_SCROLL_STRIP: f32 = 22.0;
/// The arrow drawn in it.
const CONTEXT_SCROLL_ARROW: f32 = 14.0;
/// How far the rest of the display is dimmed behind the menu. Lighter than the
/// power dialog's: that is a question about ending the session, this is a note
/// pinned to something the user can still see.
const CONTEXT_DIM: f32 = 0.42;
/// How dark the scrim over a running application is, for the same reason —
/// the shell's own surfaces are dimmed by the line above, and this is what
/// dims whatever the compositor is drawing *under* this surface.
const CONTEXT_SCRIM: f32 = 0.55;
/// How much of its size the start screen gives up while a panel stands over
/// it, as a share — see [`recede_into_depth`].
///
/// Dimming on its own says the light went out; a panel held in front of the
/// screen says the screen is further away, and what is further away is
/// smaller. The two together are the difference between a bar that has been
/// turned down and a bar that has been stepped back from.
///
/// Deeper than the lean the bar gives an application opening off it, which is
/// the same gesture the other way round but a different job: that one is a
/// flourish under a splash that is about to cover the whole display, gone
/// before it can be looked at, while this is a distance the screen *sits* at
/// for as long as the user takes to read a menu. A push that only reads while
/// it is moving is one that has stopped saying anything by the time it
/// matters.
///
/// And no deeper. The menu is *about* the tile it came out of, and the bar
/// still has to be read past the panel while it is up; far enough back and the
/// step stops being the screen receding and starts being the screen shrinking,
/// which is a thing happening to the subject rather than to the ground behind
/// it.
const CONTEXT_DEPTH: f32 = 0.1;
/// The share of the panel's growth that passes before its contents begin to
/// appear. A menu's rows do not read at the size of an icon, and letting them
/// try makes the whole thing look like a shrunken panel being enlarged rather
/// than something opening out of the control it belongs to.
const CONTEXT_CONTENT_IN: f32 = 0.45;

/// How tall one row's own line is, in reference pixels.
///
/// A track needs more of the column than a command does — see [`MIXER_ROW`] —
/// and so does a row with a second line under its label, for the plainer
/// reason that there are two lines to fit. The same number as a track's, not
/// because the two have anything to do with each other but because both are a
/// label plus one more thing the same size, and a panel that mixed them would
/// otherwise have three row heights in one column.
fn context_row_height(entry: &MenuEntry) -> f32 {
    let stacked = if entry.level.is_some() || entry.detail.is_some() {
        MIXER_ROW
    } else {
        CONTEXT_ROW
    };
    // And one line more where there is a line above the label — see
    // [`crate::menu::Entry::stamp`]. Added rather than folded into a third
    // height because it is exactly that: the row it would have been, with a
    // line put on top of it. Everything below keeps the placement it had, and
    // a list of announcements is a list of three-line rows, of which fewer fit
    // on a display than four — which is the price of the line.
    stacked
        + if entry.stamp.is_some() {
            CONTEXT_STAMP_ROOM
        } else {
            0.0
        }
}

/// The sizes a row's three runs are set at, in reference pixels, and the room
/// the one above the label is given.
///
/// The stamp is the smallest of the three and the quietest, because it is the
/// least of what the row says: it is a note about when, over a summary somebody
/// else wrote. Its room is its own line spacing — one line, and it never asks
/// for another.
const CONTEXT_LABEL_SIZE: f32 = 23.0;
const CONTEXT_DETAIL_SIZE: f32 = 20.0;
const CONTEXT_STAMP_SIZE: f32 = 18.0;
const CONTEXT_STAMP_ROOM: f32 = CONTEXT_STAMP_SIZE * 1.25;

/// What a row grows by for each line of its label, and of the line under it,
/// beyond the first. The factor is the renderer's line spacing — see the
/// metrics `Gpu` shapes with — because a row has to open out by exactly what
/// the text will take.
const CONTEXT_LABEL_LINE: f32 = CONTEXT_LABEL_SIZE * 1.25;
const CONTEXT_DETAIL_LINE: f32 = CONTEXT_DETAIL_SIZE * 1.25;

/// The most either of a row's two runs will open out to, in lines.
///
/// A cap rather than however many the sentence takes, because the panel grows
/// with the row and the display does not. Three lines is a long summary read
/// in full; past that the row would be eating the list it belongs to, and what
/// the user wants is the announcement itself rather than a taller row.
pub const CONTEXT_MAX_LINES: u8 = 3;

/// How wide the panel is drawn, in reference pixels.
fn context_panel_width(extra: f32) -> f32 {
    CONTEXT_WIDTH + extra
}

/// The height of a row's chip when nothing is opening out, in reference pixels
/// — what sizes everything on the row that is not writing.
fn context_chip_height(entry: &MenuEntry) -> f32 {
    context_row_settled(entry) - GUIDE_ROW_PADDING * 2.0
}

/// How wide the button on the right-hand end of a row is, in reference pixels,
/// or nought where the row has none.
fn context_aside_width(entry: &MenuEntry) -> f32 {
    match entry.aside {
        Some(_) => context_chip_height(entry) * CONTEXT_ASIDE,
        None => 0.0,
    }
}

/// How wide a row's writing is, in reference pixels — the same for both runs,
/// because the second line starts under the first rather than beside it.
///
/// Everything that takes width off the line is asked here and nowhere else:
/// the picture or the mark at the head of the row, and the button at the end of
/// it. Measured against the row's *settled* height, which is what sizes all
/// three — a row that has opened out keeps the furniture it had.
fn context_text_width(entry: &MenuEntry, extra: f32) -> f32 {
    let tall = context_row_height(entry) > CONTEXT_ROW;
    let chip = context_chip_height(entry);
    let mut width = context_panel_width(extra) - GUIDE_MARGIN * 2.0 - GUIDE_LABEL_PADDING * 2.0;
    if entry.icon.is_some() {
        width -= chip * if tall { MIXER_ICON } else { CONTEXT_ICON } + GUIDE_LABEL_PADDING * 0.5;
    } else if entry.glyph.is_some() {
        width -= chip * CONTEXT_GLYPH + GUIDE_LABEL_PADDING * 0.5;
    }
    if entry.aside.is_some() {
        width -= context_aside_width(entry) + CONTEXT_ASIDE_GAP;
    }
    width.max(1.0)
}

/// The box a row's label is set in, in reference pixels: how wide, and at what
/// size.
///
/// For whoever is building the rows to ask the renderer how many lines a label
/// will take — see [`crate::menu::Entry::lines`]. It is here rather than at the
/// caller because these are the numbers the row is *drawn* with, and a
/// measurement taken against different ones would grow the row by the wrong
/// amount or not at all. `extra` is how much wider than usual the panel the row
/// is bound for will be — see [`Menu::widen`].
pub fn context_label_box(entry: &MenuEntry, extra: f32) -> (f32, f32) {
    (context_text_width(entry, extra), CONTEXT_LABEL_SIZE)
}

/// The same for the line under the label — see [`crate::menu::Entry::detail`].
/// The width is the label's; the size is not.
pub fn context_detail_box(entry: &MenuEntry, extra: f32) -> (f32, f32) {
    (context_text_width(entry, extra), CONTEXT_DETAIL_SIZE)
}

/// And the same for the header — see [`crate::menu::Title`].
///
/// The whole width of the panel inside its margins, because the header is the
/// one run on the panel with nothing beside it: no picture at its head and no
/// button at its end.
pub fn context_title_box(extra: f32) -> (f32, f32) {
    (
        (context_panel_width(extra) - (GUIDE_MARGIN + GUIDE_LABEL_PADDING) * 2.0).max(1.0),
        CONTEXT_TITLE_SIZE,
    )
}

/// The most lines a header will open out to, on a display this tall.
///
/// A header is a heading and not a paragraph, so this is a quarter of the
/// display where a row raised to be read gets half — see
/// [`context_read_lines`]. But it is a real number of lines and not one,
/// because the header over an announcement is a sentence the program that sent
/// it wrote, and a shell that cut it to fit would be hiding the one line the
/// user opened the announcement to read in full. What is left over goes to the
/// list underneath, which scrolls; the heading does not.
pub fn context_title_lines(height: f32) -> u8 {
    let scale = guide_scale(height);
    ((height * 0.25) / (CONTEXT_TITLE_LINE * scale)).clamp(1.0, u8::MAX as f32) as u8
}

/// How tall the header is, in reference pixels: nought where there is none,
/// and one line's worth more for every line past the first.
fn context_title_height(menu: &Menu) -> f32 {
    match menu.title() {
        Some(_) => CONTEXT_TITLE + context_title_growth(menu),
        None => 0.0,
    }
}

/// How much of that is the header having opened out — what a panel with a
/// one-line header, which is every other panel in the shell, does not pay.
fn context_title_growth(menu: &Menu) -> f32 {
    match menu.title() {
        Some(_) => (menu.title_lines().max(1) - 1) as f32 * CONTEXT_TITLE_LINE,
        None => 0.0,
    }
}

/// The most lines a row that is there to be read will take, on a display this
/// tall — see [`crate::menu::Entry::reading`].
///
/// Not a fixed number like [`CONTEXT_MAX_LINES`], because this row is not
/// sharing a panel with a list somebody is scanning: it is the reason the panel
/// was raised, and the only thing that has any business cutting it short is the
/// edge of the screen. Half the display, so that whatever buttons the program
/// offered underneath it, and the way out below them, are still on the panel.
pub fn context_read_lines(height: f32) -> u8 {
    let scale = guide_scale(height);
    ((height * 0.5) / (CONTEXT_LABEL_LINE * scale)).clamp(1.0, u8::MAX as f32) as u8
}

/// How far a row would open out if it were selected and all the way out, in
/// reference pixels: what the label wants, and what the line under it wants.
///
/// The two are kept apart all the way to the drawing because they are taken in
/// different places — the label's room opens under the label, and the detail's
/// under the detail — and a single total could not say where.
fn entry_opening(entry: &MenuEntry) -> (f32, f32) {
    // A row that is there to be read is capped by whoever counted its lines,
    // against the display it is bound for — see [`context_read_lines`]. The
    // three-line cap is for rows sharing a panel with a list, which this is
    // not: cutting the paragraph short here would be cutting it short on the
    // one panel raised to show it.
    let cap = if entry_always_open(entry) {
        u8::MAX
    } else {
        CONTEXT_MAX_LINES
    };
    let extra = |lines: u8| lines.min(cap).saturating_sub(1) as f32;
    (
        extra(entry.lines) * CONTEXT_LABEL_LINE,
        extra(entry.detail_lines) * CONTEXT_DETAIL_LINE,
    )
}

/// Whether a row is open all the time rather than only under the highlight.
///
/// A row that cannot be chosen never comes under the highlight, so it would
/// never open — and a row put on a panel purely to be read is exactly the one
/// that has to be all the way out from the moment the panel arrives. That is
/// what an opened announcement's body is: a row the highlight steps over.
fn entry_always_open(entry: &MenuEntry) -> bool {
    entry.reading
}

/// How far row `index` has opened out to show writing too long for one line,
/// in reference pixels: the label's share and the detail's.
///
/// Only the selected row, and only one at a time: this is the row being read,
/// not a column that has decided to lay itself out differently. Everything
/// else in the panel keeps the height it had, so the list a user was scanning
/// does not rearrange itself under them.
fn context_row_opening(menu: &Menu, index: usize) -> (f32, f32) {
    let Some(entry) = menu.entries().get(index) else {
        return (0.0, 0.0);
    };
    let out = if entry_always_open(entry) {
        1.0
    } else if index == menu.selected() {
        menu.expansion()
    } else {
        return (0.0, 0.0);
    };
    let (label, detail) = entry_opening(entry);
    (label * out, detail * out)
}

/// How much taller row `index` is for having opened out, in reference pixels.
pub fn context_row_growth(menu: &Menu, index: usize) -> f32 {
    let (label, detail) = context_row_opening(menu, index);
    label + detail
}

/// A row's height with nothing opening out under the highlight — which for a
/// row that is always open includes the room its writing takes.
fn context_row_settled(entry: &MenuEntry) -> f32 {
    let base = context_row_height(entry);
    if !entry_always_open(entry) {
        return base;
    }
    let (label, detail) = entry_opening(entry);
    base + label + detail
}

/// How many lines a run may take, given the room it has been given.
///
/// Floored, so that a run never draws a line the row has not opened out far
/// enough to hold: the two runs sit one under the other, and a label that grew
/// its third line half-way through the movement put it straight through the
/// first line of the body underneath. The nudge is for the arithmetic alone —
/// at rest the room is a whole number of lines, and a whisker under it is
/// still that number.
fn lines_in(room: f32, line: f32) -> u8 {
    1 + (room / line + 1e-3).floor().max(0.0) as u8
}

/// A row's height including however far it has opened out.
fn context_row_height_at(menu: &Menu, index: usize) -> f32 {
    let Some(entry) = menu.entries().get(index) else {
        return CONTEXT_ROW;
    };
    context_row_height(entry) + context_row_growth(menu, index)
}

/// How much of the column a row and whatever precedes it take up, in reference
/// pixels: the row itself, plus the gap a change of band opens above it.
fn context_row_span(menu: &Menu, index: usize, first: usize) -> f32 {
    let entries = menu.entries();
    let gap = index > first && entries[index].group != entries[index - 1].group;
    context_row_height_at(menu, index) + if gap { CONTEXT_GROUP_GAP } else { 0.0 }
}

/// How tall the drawn rows are altogether, in reference pixels.
fn context_body_height(menu: &Menu) -> f32 {
    let first = menu.first_visible();
    (first..first + menu.visible_rows())
        .map(|index| context_row_span(menu, index, first))
        .sum()
}

/// How far down the panel the first row starts, in reference pixels: the
/// margin, the header if there is one, and the strip the upper arrow lives in.
fn context_rows_top(menu: &Menu) -> f32 {
    GUIDE_MARGIN
        + context_title_height(menu)
        + if context_scrolls(menu) {
            CONTEXT_SCROLL_STRIP
        } else {
            0.0
        }
}

/// Whether the list is longer than the panel is drawing.
fn context_scrolls(menu: &Menu) -> bool {
    menu.visible_rows() < menu.entries().len()
}

/// How many rows a display `height` tall has room for.
///
/// What the shell tells the menu with [`Menu::set_window`], and the only thing
/// the model has to know about the size of a screen. Never zero: a panel with
/// one row that runs off the bottom of a very short display is still better
/// than a panel with nothing in it.
pub fn context_menu_rows_that_fit(height: f32) -> usize {
    rows_that_fit(height, CONTEXT_ROW)
}

/// The same for a panel of tracks, which are taller.
///
/// Asked separately because the answer goes *in* with the entries, before the
/// menu holds any: whoever is about to raise a mixer is the only one who knows
/// it is one.
pub fn mixer_rows_that_fit(height: f32) -> usize {
    rows_that_fit(height, MIXER_ROW)
}

/// And for a menu that already has its rows, which is what the shell asks every
/// frame — a display can change size under an open panel.
pub fn menu_rows_that_fit(menu: &Menu, height: f32) -> usize {
    // Room for the one row that will open out, held back from the count.
    //
    // Once, not per row: only the selected row grows — see
    // [`context_row_opening`] — so the panel is at its tallest with the
    // longest row in the list selected and every other row settled. Without
    // this the panel, which is sized to its rows, would grow past the bottom
    // of the display the moment somebody stopped on a long announcement, and
    // cut off the row at the end of the list to pay for it.
    let opening = menu
        .entries()
        .iter()
        .filter(|entry| !entry_always_open(entry))
        .map(|entry| {
            let (label, detail) = entry_opening(entry);
            label + detail
        })
        .fold(0.0, f32::max);

    // Measured against the rows this list actually has, rather than against
    // the tallest of them counted over and over. A panel raised to show one
    // announcement is a paragraph several inches deep with three buttons under
    // it, and measuring those buttons as though each were the paragraph would
    // answer that one row fits — leaving the reader the words and no way to
    // act on them.
    // And against this panel's own header, which is the other thing that can be
    // taller here than the worst case [`column_room`] allows for: a heading
    // somebody else wrote, opened out to be read in full. The room it takes
    // comes off the rows, so a long one costs the list its last row rather than
    // pushing it off the bottom of the display.
    let scale = guide_scale(height);
    let mut room = column_room(height, opening + context_title_growth(menu));
    let mut rows = 0;
    for entry in menu.entries() {
        let span = (context_row_settled(entry) + CONTEXT_GROUP_GAP) * scale;
        if rows > 0 && span > room {
            break;
        }
        room -= span;
        rows += 1;
    }
    rows.max(1)
}

fn rows_that_fit(height: f32, row: f32) -> usize {
    let scale = guide_scale(height);
    ((column_room(height, 0.0) / ((row + CONTEXT_GROUP_GAP) * scale)) as usize).max(1)
}

/// How much of a display this tall is left for rows, once the panel's own
/// furniture and `reserve` reference pixels have been taken out of it.
///
/// The worst case, so the answer holds however the entries turn out to be
/// grouped: a titled menu whose list is long enough to scroll, with a band
/// change above every row.
fn column_room(height: f32, reserve: f32) -> f32 {
    let scale = guide_scale(height);
    let furniture = (GUIDE_MARGIN * 2.0
        + CONTEXT_TITLE
        + CONTEXT_SCROLL_STRIP * 2.0
        + CONTEXT_GROUP_GAP
        + reserve)
        * scale;
    height - PANEL_INSET * scale * 2.0 - furniture
}

/// Where the menu's panel settles: beside the control it is about, on whichever
/// side of it there is room for, and never off the display.
///
/// Beside rather than over. The anchor is the whole reason the menu is where it
/// is — it says *this is what these commands are about* — so covering it with
/// the answer would throw away the only context the panel has.
pub fn context_menu_rect(width: f32, height: f32, menu: &Menu) -> [f32; 4] {
    let scale = guide_scale(height);
    let inset = PANEL_INSET * scale;
    let gap = CONTEXT_GAP * scale;

    let panel_w =
        (context_panel_width(menu.extra_width()) * scale).min((width - inset * 2.0).max(0.0));
    let panel_h = ((context_rows_top(menu)
        + context_body_height(menu)
        + if context_scrolls(menu) {
            CONTEXT_SCROLL_STRIP
        } else {
            0.0
        }
        + GUIDE_MARGIN)
        * scale)
        .min((height - inset * 2.0).max(0.0));

    let [ax, ay, aw, ah] = menu.anchor();
    // To the right of the anchor by preference, because the bar and the guide's
    // sidebar both put what can be selected on the left of the display. Flipped
    // to the other side when the panel would run off the edge, which is what
    // makes one function serve a tile near the left margin and a card near the
    // right one.
    let right = ax + aw + gap;
    let x = if right + panel_w <= width - inset {
        right
    } else {
        (ax - gap - panel_w).max(inset)
    };
    // Centred on the anchor vertically, so the panel reads as hanging off it
    // rather than as having been dropped beside it.
    let y = (ay + ah * 0.5 - panel_h * 0.5).clamp(inset, (height - inset - panel_h).max(inset));
    [
        x.min((width - inset - panel_w).max(inset)),
        y,
        panel_w,
        panel_h,
    ]
}

/// The whole line entry `index` occupies, in settled display coordinates, or
/// `None` when that entry is scrolled out of the panel.
///
/// The line and not the chip: on a row with a button on its end the two are
/// different things — see [`context_menu_chip_rect`] — and this is the one a
/// press anywhere on the row lands in.
///
/// Shared by the drawing below and by the caller easing the selection, for the
/// same reason [`menu_item_rect`] is: two places computing one rectangle have
/// to agree.
pub fn context_menu_row_rect(
    width: f32,
    height: f32,
    menu: &Menu,
    index: usize,
) -> Option<[f32; 4]> {
    let scale = guide_scale(height);
    let [panel_x, panel_y, panel_w, _] = context_menu_rect(width, height, menu);
    let margin = GUIDE_MARGIN * scale;
    let padding = GUIDE_ROW_PADDING * scale;
    let first = menu.first_visible();
    let last = first + menu.visible_rows();
    if index < first || index >= last {
        return None;
    }

    let mut y = panel_y + context_rows_top(menu) * scale;
    for row in first..=index {
        let line = context_row_height_at(menu, row);
        y += (context_row_span(menu, row, first) - line) * scale;
        if row == index {
            return Some([
                panel_x + margin,
                y + padding,
                panel_w - margin * 2.0,
                line * scale - padding * 2.0,
            ]);
        }
        y += line * scale;
    }
    None
}

/// The face of row `index`: the whole line, less the button on its end and the
/// air between them where it has one.
///
/// The row and its button are two chips side by side rather than one chip with
/// a second laid over it, and this is the first of the two. A button drawn
/// *inside* the row's own face is a button on top of a button — and worse, the
/// light that says what the next press will reach has to fall on one of them:
/// with the button standing on the row, a lit row lights the button as well and
/// stops answering the only question the pair asks, which of the two is being
/// aimed at.
///
/// So the row gives up the width instead. Everything on the row that is
/// measured — see [`context_text_width`] — was already measured against this,
/// because the writing always stopped before the button; what changes at the
/// chip is only where the glass ends.
pub fn context_menu_chip_rect(
    width: f32,
    height: f32,
    menu: &Menu,
    index: usize,
) -> Option<[f32; 4]> {
    let entry = menu.entries().get(index)?;
    let [rx, ry, rw, rh] = context_menu_row_rect(width, height, menu, index)?;
    if entry.aside.is_none() {
        return Some([rx, ry, rw, rh]);
    }
    let taken = (context_aside_width(entry) + CONTEXT_ASIDE_GAP) * guide_scale(height);
    Some([rx, ry, (rw - taken).max(0.0), rh])
}

/// The button on the right-hand end of row `index`, in settled display
/// coordinates, or `None` where that row has none or is scrolled off the panel.
///
/// As wide as the row settles at is tall — see [`context_aside_width`] — and as
/// tall as the row it is on, whatever the row has opened out to. The width is
/// held off the settled row on purpose, because it is the width that takes room
/// from the writing and a line that reflowed as the row grew would be a
/// paragraph rearranging itself under the reader. The height is not: the button
/// and the row's own face are a pair set side by side, and a pair whose halves
/// are different heights reads as a control that has been dropped onto a row
/// rather than as one half of it.
pub fn context_menu_aside_rect(
    width: f32,
    height: f32,
    menu: &Menu,
    index: usize,
) -> Option<[f32; 4]> {
    let entry = menu.entries().get(index)?;
    entry.aside?;
    let [rx, ry, rw, rh] = context_menu_row_rect(width, height, menu, index)?;
    let button = context_aside_width(entry) * guide_scale(height);
    Some([rx + rw - button, ry, button, rh])
}

/// Where the highlight stands: the selected row's chip, or its button when the
/// user has stepped sideways onto it.
///
/// One place answers it, because two would be the light and the press landing
/// on different rectangles the moment they disagreed.
pub fn context_menu_highlight_rect(width: f32, height: f32, menu: &Menu) -> Option<[f32; 4]> {
    let index = menu.selected();
    if menu.on_aside() {
        if let Some(rect) = context_menu_aside_rect(width, height, menu, index) {
            return Some(rect);
        }
    }
    // The row's face and not its whole line: the light lands on exactly what
    // the next press will reach, and on a row with a button that is everything
    // up to the button.
    context_menu_chip_rect(width, height, menu, index)
}

/// The rules between the menu's bands: one wherever two drawn rows disagree
/// about which group they are in, centred in the gap that disagreement opened.
fn context_separator_rects(width: f32, height: f32, menu: &Menu) -> Vec<[f32; 4]> {
    let scale = guide_scale(height);
    let [panel_x, _, panel_w, _] = context_menu_rect(width, height, menu);
    let side = GUIDE_MARGIN * scale;
    let rule_h = (1.0 * scale).max(1.0);
    let entries = menu.entries();
    let first = menu.first_visible();

    (first + 1..first + menu.visible_rows())
        .filter(|index| entries[*index].group != entries[index - 1].group)
        .filter_map(|index| {
            let [_, above_y, _, above_h] = context_menu_row_rect(width, height, menu, index - 1)?;
            let [_, below_y, _, _] = context_menu_row_rect(width, height, menu, index)?;
            // Centred between the *visible* chip edges, exactly as the guide's
            // column does it — the chips carry their own row padding, so a rule
            // placed from the lower row alone sits visibly high.
            let y = (above_y + above_h + below_y - rule_h) * 0.5;
            Some([panel_x + side, y, panel_w - side * 2.0, rule_h])
        })
        .collect()
}

/// Where the panel is when it is `progress` of the way out of its anchor.
///
/// The power dialog's flight, and for the same reason: it leaves as one shape
/// rather than growing into its proportions, so the contents can ride out on
/// the very same factor and nothing inside the panel moves relative to anything
/// else on the way.
pub fn context_menu_bounds(width: f32, height: f32, menu: &Menu, progress: f32) -> [f32; 4] {
    let [px, py, pw, ph] = context_menu_rect(width, height, menu);
    let [ax, ay, aw, ah] = menu.anchor();
    if pw <= 0.0 {
        return [px, py, pw, ph];
    }
    let factor = lerp((aw / pw).min(1.0), 1.0, progress);
    let cx = lerp(ax + aw * 0.5, px + pw * 0.5, progress);
    let cy = lerp(ay + ah * 0.5, py + ph * 0.5, progress);
    [
        cx - pw * factor * 0.5,
        cy - ph * factor * 0.5,
        pw * factor,
        ph * factor,
    ]
}

/// Push a scene behind the context menu: dim it, and drop the text the panel
/// would otherwise print through.
///
/// The same treatment the power dialog gets, and needed for the same reason —
/// every quad in a scene is drawn before every text run, so a panel laid over
/// the bar does not hide the bar's labels unless they are taken away.
pub fn recede_behind_context_menu(
    scene: &mut Scene,
    width: f32,
    height: f32,
    menu: &Menu,
    progress: f32,
) {
    scene.fade(lerp(1.0, CONTEXT_DIM, progress));
    scene.hide_text_behind(context_menu_bounds(width, height, menu, progress));
}

/// Step the start screen back from the viewer while a panel stands over it.
///
/// The bar shrinks towards the crossing point of its own cross — where the
/// focused entry sits, and where [`launch_origin`] anchors a menu — so the one
/// tile the panel is about does not move at all while everything around it
/// draws away from it. Shrinking towards the middle of the display instead
/// would slide that tile out from under the very panel growing out of it,
/// which reads as the bar sliding rather than as the bar receding.
///
/// Every quad is scaled, glass included: a pane's depth is a length like any
/// other, and a slab left at full thickness on a scene that has moved back
/// would be a bevel that grew as the screen shrank.
///
/// `depth` is how far back, 0 flat against the glass and 1 fully stepped away.
/// It is one number for however many panels are stacked up and it runs on
/// [`crate::menu::FLIGHT`], a panel's own arrival, so the screen goes back
/// over exactly the span the first one comes forward in and stays there until
/// the last has gone. The shell holds it; see `Panel::depth_linear`, and the
/// reason it is not simply read off whichever panel is open.
///
/// The mirror of the lean the bar gives an application opening off it, which
/// zooms *towards* the tile by the same share: something coming out of the
/// screen pulls it forward, something standing over it pushes it back.
///
/// Called before the panel's own [`recede_behind_context_menu`], which fades
/// what is left, and before the start card's [`Scene::place_into`], which is
/// about the guide taking the display rather than about anything standing on
/// it.
pub fn recede_into_depth(scene: &mut Scene, width: f32, height: f32, depth: f32) {
    let depth = depth.clamp(0.0, 1.0);
    if depth <= 0.0 {
        return;
    }
    let [ax, ay, aw, ah] = launch_origin(width, height);
    let factor = lerp(1.0, 1.0 - CONTEXT_DEPTH, depth);
    // Scaling about a point rather than about the origin: everything keeps its
    // distance from the cross in the same proportion, and the cross itself
    // stays exactly where it was.
    scene.scale_by(
        factor,
        [
            (ax + aw * 0.5) * (1.0 - factor),
            (ay + ah * 0.5) * (1.0 - factor),
        ],
    );
}

/// How long the start screen takes to arrive when the shell is started, in
/// seconds.
///
/// Exactly what the start screen takes to fly into its overview card, which is
/// the shell's other whole-screen move and the longest thing in it that is not
/// a wallpaper crossfade. Nothing here should outlast that.
///
/// It was nearly a second to begin with, on the reasoning that an arrival
/// nobody asked for should be given time to be read — and that was the wrong
/// way round. Nobody asked for it, so it is in the way: it stands between a
/// person who has just started the machine and the bar they started it to use,
/// and every frame of it past the point the move has been understood is a frame
/// of waiting.
///
/// The whole of it is spent moving, too, which was not true of the ramp this
/// once ran on: [`ease`] is gentle at both ends but never still, so three
/// tenths of a second here is three tenths of a second of screen coming forward
/// rather than a wait with a sweep on the end of it.
pub const ARRIVAL: f32 = 0.3;

/// How far back it starts, in the steps of recession a column of an open path
/// stands in — see [`receded`].
///
/// Four, which is as far as the shell's depth goes: past about that the
/// recession stops and everything sits at [`DEPTH_FLOOR_SCALE`], because
/// another step tells the eye nothing it has not been told. So the screen comes
/// in from the back of the same depth the trail is read in rather than from a
/// distance invented for the occasion, and the size it starts at is the size a
/// column four deep would be drawn at.
const ARRIVAL_STEPS: f32 = 4.0;

/// The share of the arrival by which the screen is fully lit.
///
/// The ink leads the movement, which is the mirror of what [`departing`] does
/// for a column on its way out: what is leaving has to clear the ground for
/// what replaces it, and what is arriving has nothing in its way at all, so it
/// can be all the way up before it has finished travelling — and it should be.
/// A screen still gaining opacity as it settles reads as one that has not
/// finished loading. A screen that is fully there and merely still coming
/// forward reads as the shell arriving, which is what this is.
const ARRIVAL_LIT_BY: f32 = 0.7;

/// Bring the start screen forward out of the depth the shell starts it in.
///
/// The first thing a session shows is a display with nothing of the shell on
/// it: the wallpaper is drawn, and the bar is far enough back to be both small
/// and unlit — which is to say invisible. Over [`ARRIVAL`] it travels the whole
/// of [`ARRIVAL_STEPS`] home, growing to its settled size and taking its ink on
/// the way, and lands exactly where a bar that never animated would have been.
///
/// On [`ease`], like everything else in the shell. This ran on an exponential
/// for a while — speed rising the whole way, on the reasoning that a thing
/// crossing a distance towards the viewer covers ground faster the nearer it
/// gets — and the reasoning is sound about the world and wrong about the
/// screen. A ramp that ends at full speed stops dead on the mark, and the eye
/// reads a halt as something having been interrupted rather than as something
/// having arrived; it also spends so much of its clock nearly still that the
/// first half of the animation looks like a shell that has hung. Gentle at both
/// ends is the shell's shape for a reason, and an arrival is no exception to
/// it: the screen leaves the distance softly, crosses at its quickest through
/// the middle, and settles onto the mark.
///
/// About the middle of the display, and this is the one depth move in the shell
/// that is not anchored on the cross. Every other one is about something
/// staying put — a tile a menu is growing out of, a row the trail hangs off —
/// and there is nothing on screen yet for this one to hold still against. What
/// is left is the plain reading of a thing approaching: it comes up the axis
/// the viewer is looking down, which is the centre of the glass, and the whole
/// screen swells out of it evenly. Anchored at the cross instead, the far side
/// of the row would sweep across half the display and read as a slide.
///
/// `arrival` is how far through that it is, 0 at the back and 1 arrived; 1
/// leaves the scene untouched rather than scaling it by one, for the reason
/// [`recede_into_depth`] leaves a screen with nothing over it alone.
///
/// Applied to the whole scene, clock included. The screen is one object coming
/// forward, not a bar with furniture arranged around it, and something that
/// held its place while everything else approached would read as a hole in the
/// picture rather than as part of the wallpaper.
pub fn arrive_from_depth(scene: &mut Scene, width: f32, height: f32, arrival: f32) {
    let arrival = arrival.clamp(0.0, 1.0);
    if arrival >= 1.0 {
        return;
    }
    let near = arrival_near(arrival);
    scene.scale_by(
        near,
        [width * 0.5 * (1.0 - near), height * 0.5 * (1.0 - near)],
    );
    // The ink rides the same ramp as the movement, only finished sooner: a fade
    // held to a straight line under an eased approach is a screen whose opacity
    // and whose distance disagree about where it has got to.
    scene.fade(ease(arrival / ARRIVAL_LIT_BY));
}

/// How large the arriving screen is drawn, as a share of its settled size.
fn arrival_near(arrival: f32) -> f32 {
    let travelled = ease(arrival);
    let (near, _) = receded(ARRIVAL_STEPS * (1.0 - travelled));
    near
}

/// Where a point on the display falls on a start screen that is still arriving.
///
/// The hit test measures the bar where it has settled, because that is where
/// every layout function puts it — so a click landing while the screen is still
/// coming forward has to be carried back through the same move the drawing was
/// carried through, or it answers with whatever is at that point of the settled
/// bar rather than with what the user is looking at. Three tenths of a second
/// is short, and a session whose very first click goes to the wrong category is
/// not something to leave to the user being quick.
pub fn arrival_point(x: f32, y: f32, width: f32, height: f32, arrival: f32) -> (f32, f32) {
    let near = arrival_near(arrival);
    if near <= 0.0 {
        return (x, y);
    }
    let (cx, cy) = (width * 0.5, height * 0.5);
    (cx + (x - cx) / near, cy + (y - cy) / near)
}

/// How long the display takes to come up out of black when an application has
/// walked out of it, in seconds.
///
/// A tenth, which is a blink and is meant to be. This is not an effect; it is a
/// join. An application that ends gives its last frame back to the compositor
/// mid-composite, and what stands behind that frame — the next window down, the
/// shell's wallpaper coming back — arrives on its own schedule rather than on
/// the one the shell is drawing to. A cut straight from one to the other shows
/// whatever happened to be underneath for the frame or two before the shell has
/// the display, which reads as a flicker of somebody else's window. Black
/// covers the join, and being over in a tenth of a second is what keeps it a
/// join rather than a fade to black — a transition the user waits through would
/// be the shell making an occasion out of a program exiting.
///
/// A third of the arrival it runs under, so the screen is clear well before the
/// bar has finished coming forward. See [`cover_with_black`].
pub const BLACK_HANDOVER: f32 = 0.1;

/// Lay black over a whole display, for coming up out of it.
///
/// The same sheet the launch splash dips a game through — see
/// [`crate::launch::Launch::blackout`] — and it is drawn the same way, as one
/// quad over everything the scene has put down.
///
/// A sheet cannot reach the scene's *text*, though: every quad is drawn before
/// every run, so the labels would print straight through it — which on a screen
/// meant to be black is the one thing on it anybody would see. The splash
/// answers that by timing its title to be gone before the screen is fully
/// black. There is no timing this one: the runs underneath belong to a start
/// screen that is arriving, and it is arriving *because* of the black.
///
/// So the runs are taken down by hand, to exactly what the sheet would have
/// left of them. That is not a second effect, it is the same one finished off,
/// and it is why this dims rather than dropping: a run cut at a threshold would
/// snap back the frame the black passed it, and what is wanted is the label
/// coming up out of the dark with everything else.
///
/// Laid on after everything that moves the scene about, never before: this is a
/// sheet over a *display*, and one that had been through the arrival's own
/// shrink would leave the edges of the screen showing.
pub fn cover_with_black(scene: &mut Scene, width: f32, height: f32, black: f32) {
    let black = black.clamp(0.0, 1.0);
    if black <= 0.0 {
        return;
    }
    scene.quads.push(Quad {
        x: 0.0,
        y: 0.0,
        w: width,
        h: height,
        slot: SOLID_SLOT,
        color: [0.0, 0.0, 0.0, black],
        ..Quad::default()
    });
    for text in &mut scene.texts {
        text.color[3] *= 1.0 - black;
    }
}

/// Everything `build_context_menu` draws from.
pub struct ContextMenuView<'a> {
    pub menu: &'a Menu,
    /// Eased rectangle of the selected row's chip, in settled display
    /// coordinates — see [`context_menu_row_rect`]. `None` snaps it to the
    /// selection.
    pub highlight: Option<[f32; 4]>,
    /// How far the panel is out of its anchor, 0 shut and 1 open. Already
    /// eased: a dismissed menu is still falling back into the control it came
    /// from, so this is a position rather than a state.
    pub open: f32,
    /// How softly the wallpaper behind the overlay is being drawn, so the
    /// panel's glass bends the same wallpaper the layer below is showing.
    pub behind: f32,
    /// The global clock, for the selection pulse.
    pub time: f32,
    /// For the entries' own glyphs, and the scroll arrows.
    pub slots: &'a dyn SlotLookup,
}

/// How round a row's chip is.
///
/// A capsule for a command, like every other pressable thing in the shell. A
/// row half again as tall, holding two lines, gets the rounded square instead:
/// a capsule that deep reads as a lozenge with something printed inside it
/// rather than as a row — the same reason the guide's tiles are rounded
/// squares.
///
/// Asked of the row's *height* rather than of what it happens to carry. It
/// used to test for a level, which is one of the two things that makes a row
/// tall — see [`context_row_height`] — and so an announcement, which is tall
/// because it has a second line under its title, came out a deep capsule
/// beside a mixer's rounded squares. Two functions asking "is this the tall
/// kind of row" in two different ways is a difference waiting to be noticed on
/// screen, and it was.
///
/// And of the height the row *settles* at rather than the height it has opened
/// out to, which is why the scale is wanted: a radius taken off a growing chip
/// would make the one row somebody is reading visibly rounder than the rows
/// either side of it, which is the very inconsistency the shared question was
/// written to end. Never more than would round the chip away altogether, so
/// that a row drawn shorter than it settles at — one being pressed — is still
/// a chip.
/// How round the button on the end of a row is.
///
/// The rounded square a tall row wears, on a chip that is square to begin with:
/// the button belongs to the row it sits on and has to read as part of it.
fn context_aside_radius(entry: &MenuEntry, scale: f32) -> f32 {
    context_chip_height(entry) * scale * 0.32
}

fn context_chip_radius(entry: &MenuEntry, height: f32, scale: f32) -> f32 {
    if context_row_settled(entry) > CONTEXT_ROW {
        (((context_row_settled(entry) - GUIDE_ROW_PADDING * 2.0) * scale) * 0.32).min(height * 0.5)
    } else {
        height * 0.5
    }
}

/// One mixer row, laid into the chip at `chip`: what is making the sound, what
/// it is called, and how loud it is.
///
/// The icon is the application's own, at the size the bar draws one, because
/// this is a list the user reads by picture — the point of a mixer is to find
/// the game among the browsers at a glance, and every one of these rows is
/// otherwise a name and a track.
///
/// Everything on it is white rather than accent-coloured, for the reason
/// [`quick_bar`] is: the lit capsule glides onto this row when it is selected,
/// and a track tinted with the accent would disappear underneath it exactly
/// when it is being aimed at.
fn mixer_row(
    scene: &mut Scene,
    chip: [f32; 4],
    entry: &MenuEntry,
    level: Level,
    scale: f32,
    focused: bool,
    slots: &dyn SlotLookup,
) {
    let theme = theme();
    let [x, y, w, h] = chip;
    let padding = GUIDE_LABEL_PADDING * scale * 0.6;

    // The icon, and where what it labels begins. A row whose icon the machine
    // could not produce starts at its name instead of leaving a hole.
    let icon = h * MIXER_ICON;
    let mut text_x = x + padding;
    if let Some(slot) = entry
        .icon
        .as_deref()
        .and_then(|name| slots.slot_for(Some(name)))
    {
        scene.quads.push(Quad {
            x: text_x,
            y: y + (h - icon) * 0.5,
            w: icon,
            h: icon,
            slot,
            color: [1.0, 1.0, 1.0, 1.0],
            ..Quad::default()
        });
        text_x += icon + MIXER_ICON_GAP * scale;
    }
    let text_w = (x + w - padding - text_x).max(0.0);

    let name_size = 22.0 * scale;
    scene.texts.push(Text {
        content: entry.label.clone(),
        x: text_x,
        y: y + h * MIXER_NAME_LINE - name_size * 0.62,
        size: name_size,
        color: theme.text.a(if focused { 1.0 } else { 0.86 }),
        bold: focused,
        max_width: text_w,
        align: TextAlign::Left,
        clip: None,
        halo: 0.0,
        lines: 1,
    });

    // The speaker says which way the track runs and whether the sound is on at
    // all — a row turned all the way down and a row silenced are the same
    // picture without it.
    let glyph = h * MIXER_GLYPH;
    let middle = y + h * MIXER_TRACK_LINE;
    if let Some(slot) = slots.glyph(mixer_glyph(level)) {
        scene.quads.push(Quad {
            x: text_x,
            y: middle - glyph * 0.5,
            w: glyph,
            h: glyph,
            slot,
            color: [1.0, 1.0, 1.0, if level.muted { 0.55 } else { 0.9 }],
            ..Quad::default()
        });
    }
    scene.quads.extend(track(
        mixer_track_line(chip, entry, level, scale, slots),
        level,
        scale,
        1.0,
    ));
}

/// Which speaker a row at this level carries.
fn mixer_glyph(level: Level) -> &'static str {
    if level.muted {
        icons::VOLUME_MUTED
    } else {
        icons::VOLUME
    }
}

/// Where the name of a mixer row begins, which is also where its speaker does:
/// past the application's icon, or at the padding when the machine could not
/// produce one.
fn mixer_text_x(chip: [f32; 4], entry: &MenuEntry, scale: f32, slots: &dyn SlotLookup) -> f32 {
    let [x, _, _, h] = chip;
    let padding = GUIDE_LABEL_PADDING * scale * 0.6;
    let icon = h * MIXER_ICON;
    let drawn = entry
        .icon
        .as_deref()
        .and_then(|name| slots.slot_for(Some(name)))
        .is_some();
    x + padding
        + if drawn {
            icon + MIXER_ICON_GAP * scale
        } else {
            0.0
        }
}

/// The line a mixer row's groove runs along, in the shape [`track`] takes.
///
/// Shared with the hit test, and it has to be: a row's track begins after an
/// icon and a speaker that the atlas may or may not have been able to produce,
/// so where it starts is not something a second reading of the layout could
/// work out for itself.
fn mixer_track_line(
    chip: [f32; 4],
    entry: &MenuEntry,
    level: Level,
    scale: f32,
    slots: &dyn SlotLookup,
) -> [f32; 4] {
    let [x, y, w, h] = chip;
    let padding = GUIDE_LABEL_PADDING * scale * 0.6;
    let mut track_x = mixer_text_x(chip, entry, scale, slots);
    if slots.glyph(mixer_glyph(level)).is_some() {
        track_x += h * MIXER_GLYPH + MIXER_GLYPH_GAP * scale;
    }
    [
        track_x,
        y + h * MIXER_TRACK_LINE,
        x + w - padding - track_x,
        0.0,
    ]
}

/// What a mixer row in the chip at `chip` is being set to by a click at `x`, or
/// `None` when the click was on the speaker at the groove's head rather than on
/// the groove — which is the button that silences it, exactly as it is on
/// every mixer ever drawn.
pub fn mixer_level_at(
    chip: [f32; 4],
    entry: &MenuEntry,
    level: Level,
    height: f32,
    slots: &dyn SlotLookup,
    x: f32,
) -> Option<f32> {
    let [track_x, _, track_w, _] = mixer_track_line(chip, entry, level, guide_scale(height), slots);
    (x >= track_x && track_w > 0.0).then(|| ((x - track_x) / track_w).clamp(0.0, 1.0))
}

/// Draw the context menu: a panel out of the control it is about, and a short
/// column of commands on it.
///
/// The panel is cut from the guide sidebar's glass — literally, through
/// [`sidebar_surface`] — because it is the same kind of object: a large quiet
/// pane that things the user can press are laid on. Reaching for the modal
/// recipe instead would have given it the power dialog's near-opaque slab,
/// which is right for a question about ending the session and wrong for a note
/// attached to a tile.
///
/// Everything is laid out at the size the panel settles at and then carried out
/// of the anchor whole, so nothing inside moves relative to anything else on
/// the way.
pub fn build_context_menu(view: ContextMenuView, width: f32, height: f32) -> Scene {
    let theme = theme();
    let scale = guide_scale(height);
    let menu = view.menu;
    let open = view.open.clamp(0.0, 1.0);
    let pulse = 0.5 + 0.5 * (view.time * std::f32::consts::TAU / PULSE_PERIOD).sin();

    let mut scene = Scene::default();
    // The scrim dims what the *compositor* is drawing under this surface — a
    // running application, or the guide's live window cards. The shell's own
    // scenes are dimmed by `recede_behind_context_menu` instead, which can also
    // take their text away.
    scene.quads.push(Quad {
        x: 0.0,
        y: 0.0,
        w: width,
        h: height,
        slot: SOLID_SLOT,
        color: theme.glass.a(CONTEXT_SCRIM * open),
        ..Quad::default()
    });

    let [panel_x, panel_y, panel_w, panel_h] = context_menu_rect(width, height, menu);
    if panel_w <= 0.0 || panel_h <= 0.0 {
        return scene;
    }

    // The panel and what is on it are two scenes so they can arrive at
    // different moments: the glass as soon as it is on its way, the commands
    // once there is enough of it to read them on.
    let mut panel = Scene::default();
    panel.quads.extend(sidebar_surface(
        [panel_x, panel_y, panel_w, panel_h],
        scale,
        view.behind,
        1.0,
    ));

    let mut inside = Scene::default();
    let label_padding = GUIDE_LABEL_PADDING * scale;
    let text_x = panel_x + GUIDE_MARGIN * scale + label_padding;
    let text_w = (panel_w - (GUIDE_MARGIN * scale + label_padding) * 2.0).max(0.0);

    // The header names what the commands are about. The rule under it is the
    // same barely-there hairline the guide rules its bands with: a grouping,
    // not a border.
    if let Some(title) = menu.title() {
        let title_size = CONTEXT_TITLE_SIZE * scale;
        inside.texts.push(Text {
            content: title.to_string(),
            x: text_x,
            // The first line stays where a one-line header has always sat, and
            // the rest of it opens out underneath — the same bargain a row that
            // opens out makes, and for the same reason: the line the reader is
            // already looking at must not move to make room for the ones under
            // it.
            y: panel_y + (GUIDE_MARGIN + CONTEXT_TITLE * 0.42) * scale - title_size * 0.5,
            size: title_size,
            color: theme.text_soft.a(0.85),
            bold: true,
            max_width: text_w,
            align: TextAlign::Left,
            clip: None,
            halo: 0.0,
            // However many the panel was built to hold — see
            // [`crate::menu::Title`]. A header is what the panel is *about*, so
            // one that does not fit on a line is wrapped rather than cut: the
            // announcement this panel was raised over is titled with a sentence
            // its own program wrote, and an ellipsis there hides the thing the
            // user pressed the row to read.
            lines: menu.title_lines(),
        });
        inside.quads.push(Quad {
            x: panel_x + GUIDE_MARGIN * scale,
            y: panel_y + (GUIDE_MARGIN + CONTEXT_TITLE * 0.78 + context_title_growth(menu)) * scale,
            w: panel_w - GUIDE_MARGIN * scale * 2.0,
            h: (1.0 * scale).max(1.0),
            slot: SOLID_SLOT,
            color: theme.accent_soft.a(0.16),
            ..Quad::default()
        });
    }

    for [sx, sy, sw, sh] in context_separator_rects(width, height, menu) {
        inside.quads.push(Quad {
            x: sx,
            y: sy,
            w: sw,
            h: sh,
            slot: SOLID_SLOT,
            color: theme.accent_soft.a(0.16),
            ..Quad::default()
        });
    }

    // The selection: one lit capsule that glides between rows, drawn before the
    // chips so an outgoing row can hand its own over as the light arrives.
    let entries = menu.entries();
    let selected = menu.selected();
    let selected_rect = view
        .highlight
        .or_else(|| context_menu_highlight_rect(width, height, menu));
    if let Some([hx, hy, hw, hh]) = selected_rect {
        // Warm rather than the accent's violet where the row cannot be taken
        // back. This light is the whole of what says so — the labels are all
        // one white — which is why it is the row's own colour and not a wash
        // over the panel: what turns red is exactly what is about to be pressed.
        let tint = if entries.get(selected).is_some_and(|entry| entry.grave) {
            theme.danger
        } else {
            theme.accent
        };
        // How far the halo reaches past the chip is a distance rather than a
        // share of it. A track's row is half again as tall as a command's, and
        // a glow that grew with the chip washed the row underneath brightly
        // enough to read as a second selection — so every row is haloed by the
        // reach a command row has always had, whatever its own height.
        let glow_h = hh + CONTEXT_ROW * scale * CONTEXT_GLOW_REACH;
        inside.quads.push(Quad {
            x: hx + hw * 0.5 - panel_w * 0.62,
            y: hy + hh * 0.5 - glow_h * 0.5,
            w: panel_w * 1.24,
            h: glow_h,
            slot: GLOW_SLOT,
            color: tint.a(0.13 + 0.05 * pulse),
            ..Quad::default()
        });
        // It goes down with the row under it when that row is pressed. It has
        // to: it is drawn *over* the chip, so a press that sank only what was
        // underneath would happen entirely behind the thing being looked at.
        let press = if menu.on_aside() {
            menu.aside_press_progress(selected)
        } else {
            menu.press_progress(selected)
        };
        let [lx, ly, lw, lh] =
            scaled_about_centre([hx, hy, hw, hh], press.map_or(1.0, press_scale));
        inside.quads.push(Quad {
            x: lx,
            y: ly,
            w: lw,
            h: lh,
            slot: SOLID_SLOT,
            color: tint.a(0.46 + 0.05 * pulse),
            // As round as the chip it is arriving over, not always a capsule.
            // The light is the row being lit rather than a second object laid
            // on it, so a stadium sitting on a track's rounded square would
            // read as the selected row having changed shape.
            radius: match entries.get(selected) {
                Some(entry) if menu.on_aside() => context_aside_radius(entry, scale),
                Some(entry) => context_chip_radius(entry, lh, scale),
                None => lh * 0.5,
            },
            thickness: DEPTH_CONTROL * scale,
            behind: view.behind,
            frost: FROST_CONTROL,
            gloss: GLOSS_FULL,
            ..Quad::default()
        });
    }

    let first = menu.first_visible();
    for (index, entry) in entries
        .iter()
        .enumerate()
        .skip(first)
        .take(menu.visible_rows())
    {
        // The row's own face, which stops where its button begins — the two are
        // drawn side by side, and the light above has already been told to land
        // on one or the other.
        let Some([rx, ry, rw, rh]) = context_menu_chip_rect(width, height, menu, index) else {
            continue;
        };
        let focused = index == selected;
        // The row only gives its own chip up once the lit capsule has arrived
        // over it. Handing it over the instant the selection changed leaves a
        // hole in the column for the length of the glide, with the light still
        // crossing the gap to fill it.
        let handed_over = match (focused, selected_rect) {
            (true, Some(highlight)) => highlight_arrival(highlight, [rx, ry, rw, rh]),
            _ => 0.0,
        };
        let press = menu.press_progress(index);
        let chip = scaled_about_centre([rx, ry, rw, rh], press.map_or(1.0, press_scale));

        let radius = context_chip_radius(entry, chip[3], scale);
        if entry.enabled {
            inside.quads.push(Quad {
                x: chip[0],
                y: chip[1],
                w: chip[2],
                h: chip[3],
                slot: SOLID_SLOT,
                color: theme.glass_raised.a(0.10),
                radius,
                thickness: DEPTH_CONTROL * scale,
                behind: view.behind,
                frost: FROST_CONTROL,
                gloss: GLOSS_QUIET,
                fade: 1.0 - handed_over,
                ..Quad::default()
            });
        } else {
            // A row that cannot be chosen is outlined where its chip would be
            // rather than given a dimmer one. Every chip here is a slab of
            // glass over a dark panel, so a dimmer one is a chip in slightly
            // less light — which is also what an unselected chip looks like. A
            // hairline is a different kind of thing rather than a quieter one.
            inside.quads.push(Quad {
                x: chip[0],
                y: chip[1],
                w: chip[2],
                h: chip[3],
                slot: SOLID_SLOT,
                color: theme.text_soft.a(0.22),
                radius,
                border: (1.5 * scale).max(1.0),
                ..Quad::default()
            });
        }

        // A row that carries a level is laid out as its own thing: what the
        // sound is, and how loud it is, one above the other.
        if let Some(level) = entry.level {
            mixer_row(&mut inside, chip, entry, level, scale, focused, view.slots);
            continue;
        }

        // The button on the end of the row, drawn from the settled rectangle
        // rather than from the pressed chip: it is beside the row and not on
        // it, so a press on the row must not carry it down as well.
        //
        // Where the writing stops is the row's own end, which the chip has
        // already given up to the button and the air between them — see
        // [`context_menu_chip_rect`]. Taken from the settled row rather than
        // from `chip`, so a line does not reflow while the row is going down.
        let aside_end = rx + rw;
        if let Some(aside) = entry.aside {
            let Some(rect) = context_menu_aside_rect(width, height, menu, index) else {
                continue;
            };
            let sunk = menu.aside_press_progress(index);
            let [bx, by, bw, bh] = scaled_about_centre(rect, sunk.map_or(1.0, press_scale));
            let on_it = focused && menu.on_aside();
            inside.quads.push(Quad {
                x: bx,
                y: by,
                w: bw,
                h: bh,
                slot: SOLID_SLOT,
                color: theme.glass_raised.a(0.16),
                radius: context_aside_radius(entry, scale),
                thickness: DEPTH_CONTROL * scale,
                behind: view.behind,
                frost: FROST_CONTROL,
                gloss: GLOSS_QUIET,
                // It gives its face up to the light exactly as a row does.
                fade: 1.0
                    - match (on_it, selected_rect) {
                        (true, Some(highlight)) => highlight_arrival(highlight, rect),
                        _ => 0.0,
                    },
                ..Quad::default()
            });
            if let Some(slot) = view.slots.glyph(aside.glyph) {
                // Off the button's width, which is the one side of it that does
                // not grow with the row: a mark drawn as a share of a button
                // that has opened out with a paragraph would be an inch of
                // dustbin.
                let mark = bw * CONTEXT_ASIDE_GLYPH;
                inside.quads.push(Quad {
                    x: bx + (bw - mark) * 0.5,
                    y: by + (bh - mark) * 0.5,
                    w: mark,
                    h: mark,
                    slot,
                    // Brighter under the highlight than beside it, which is the
                    // only thing on the row that says which of the two the next
                    // press will reach. The two are a label's own two weights,
                    // and for the same reason: quieter must still be legible,
                    // and this glass has the wallpaper coming through it.
                    color: [1.0, 1.0, 1.0, if on_it { 1.0 } else { 0.82 }],
                    ..Quad::default()
                });
            }
        }

        // How much of the row this entry's two runs have been given beyond one
        // line each, and therefore how many lines each may take. Needed before
        // anything is placed: the row's writing is laid out against the height
        // the row settles at, and the room it has opened out is taken from
        // under each run rather than being shared out over the whole chip.
        let (label_grown, detail_grown) = context_row_opening(menu, index);
        let (label_grown, detail_grown) = (label_grown * scale, detail_grown * scale);
        let grown = label_grown + detail_grown;
        // The chip as it stands when it is not being read. Everything but the
        // writing is sized and centred on this, so that a row opening out does
        // not inflate its own picture.
        let settled = chip[3] - grown;

        // What is at the head of the row: a picture of the thing it is about
        // where it has one, and one of the shell's own marks otherwise. A row
        // without either simply starts at its label, so a menu can mix all
        // three without leaving a column of holes.
        // The line stops where the button starts, not where the chip does.
        let mut label_x = chip[0] + label_padding;
        let mut label_w = aside_end - label_padding - label_x;
        let pictured = entry
            .icon
            .as_deref()
            .and_then(|name| view.slots.slot_for(Some(name)));
        if let Some(slot) = pictured {
            let icon = settled * CONTEXT_ICON;
            let top = chip[1] + (settled - icon) * 0.5;
            inside.quads.push(Quad {
                x: label_x,
                y: top,
                w: icon,
                h: icon,
                slot,
                color: [1.0, 1.0, 1.0, if entry.enabled { 1.0 } else { 0.4 }],
                ..Quad::default()
            });
            // A row with a picture *and* a mark wears the mark as a badge on
            // the corner of it — the tick on the value in force, exactly as the
            // Settings column draws it, and for the same reason: the mark is
            // saying something about the picture rather than standing in for
            // it.
            if let Some(slot) = entry.glyph.and_then(|name| view.slots.glyph(name)) {
                let badge = icon * CHOSEN_BADGE;
                inside.quads.push(Quad {
                    x: label_x + icon * (0.5 + CHOSEN_BADGE_AT) - badge * 0.5,
                    y: top + icon * (0.5 + CHOSEN_BADGE_AT) - badge * 0.5,
                    w: badge,
                    h: badge,
                    slot,
                    color: theme.rim.a(if entry.enabled { 1.0 } else { 0.4 }),
                    ..Quad::default()
                });
            }
            label_x += icon + label_padding * 0.5;
            label_w -= icon + label_padding * 0.5;
        } else if let Some(slot) = entry.glyph.and_then(|name| view.slots.glyph(name)) {
            let glyph = settled * CONTEXT_GLYPH;
            inside.quads.push(Quad {
                x: label_x,
                y: chip[1] + (settled - glyph) * 0.5,
                w: glyph,
                h: glyph,
                slot,
                color: [1.0, 1.0, 1.0, if entry.enabled { 0.9 } else { 0.35 }],
                ..Quad::default()
            });
            label_x += glyph + label_padding * 0.5;
            label_w -= glyph + label_padding * 0.5;
        }

        let label_size = CONTEXT_LABEL_SIZE * scale;
        // Every label is the same white, the grave rows included. What a row
        // does is said by the light that lands on it — the warm capsule chosen
        // above — and never by the colour of its letters: warm text on the
        // panel's dark glass is a dim red on a dark ground, which makes the one
        // row that most wants reading the hardest to read.
        // Dim is how the shell says *not for you*, so it is kept for the rows
        // that are not: a row put on the panel to be read is set in the same
        // white as any other, because it is the reason the panel is up. Its
        // outline has already said it cannot be pressed, and saying so twice
        // costs the reader the contrast they need for a paragraph.
        let color = if entry.enabled {
            theme.text.a(if focused { 1.0 } else { 0.82 })
        } else if entry.reading {
            theme.text.a(0.92)
        } else {
            theme.text_soft.a(0.38)
        };
        let label_lines = lines_in(label_grown, CONTEXT_LABEL_LINE * scale);

        // The row's writing, as a stack of line boxes centred on the chip.
        //
        // One rule for one, two or three runs, and it is the reason the air
        // above the first line matches the air under the last: the stack is
        // measured and the leftover is halved, rather than each run being
        // placed at a share of the row that happened to read well for the
        // number of runs it was written for. The stamp arrived as a fourth
        // such share and had almost no room over it — it sat against the top
        // of its own line box while the body below kept a whole margin.
        //
        // The chip and the stack grow by the same amount when a row opens
        // out — the room a run takes is inside its own box — so the margins
        // do not move while it does, and neither does anything above the run
        // that grew. That is what keeps the line somebody is already reading
        // where they are looking.
        let detail_size = CONTEXT_DETAIL_SIZE * scale;
        let stamp_size = CONTEXT_STAMP_SIZE * scale;
        let stamp_box = if entry.stamp.is_some() {
            CONTEXT_STAMP_ROOM * scale
        } else {
            0.0
        };
        let label_box = CONTEXT_LABEL_LINE * scale + label_grown;
        let detail_box = match &entry.detail {
            Some(_) => CONTEXT_DETAIL_LINE * scale + detail_grown,
            None => 0.0,
        };
        let stack_top = chip[1] + (chip[3] - (stamp_box + label_box + detail_box)) * 0.5;
        if let Some(stamp) = &entry.stamp {
            inside.texts.push(Text {
                content: stamp.clone(),
                x: label_x,
                y: stack_top,
                size: stamp_size,
                // Quieter than the body under it, and quieter still beside the
                // highlight. It is the one run on the row that nobody came to
                // read: it answers *when*, which is a question asked of a whole
                // column at a glance rather than of one row.
                color: theme.text_soft.a(if focused { 0.85 } else { 0.6 }),
                bold: false,
                max_width: label_w.max(0.0),
                align: TextAlign::Left,
                clip: None,
                halo: 0.0,
                // One, always. It is "now" or "3m", and a row that opened out
                // to hold more of it would be opening out for nothing.
                lines: 1,
            });
        }

        inside.texts.push(Text {
            content: entry.label.clone(),
            x: label_x,
            y: stack_top + stamp_box,
            size: label_size,
            color,
            bold: focused,
            max_width: label_w.max(0.0),
            align: TextAlign::Left,
            clip: None,
            halo: 0.0,
            lines: label_lines,
        });
        if let Some(detail) = &entry.detail {
            // The whole of it, wrapped into however many lines the row has
            // opened out to hold — one, until the highlight stops here. A
            // notification body is a sentence written by the program that sent
            // it, and the row somebody is reading is the only place in the
            // shell where there is room to read one.
            inside.texts.push(Text {
                content: detail.clone(),
                x: label_x,
                // Under the label's whole box, however many lines that has
                // opened out to — and no further. Its own lines open out
                // beneath it, so it stays under the last line of the label
                // rather than drifting away from it.
                y: stack_top + stamp_box + label_box,
                size: detail_size,
                // White under the highlight, and quiet everywhere else. This
                // line is the announcement itself rather than a note about the
                // row, and the row the highlight has stopped on is the one
                // being read — the same row that has just opened out to show
                // the rest of it.
                color: match (entry.enabled, focused) {
                    (true, true) => theme.text.a(0.94),
                    (true, false) => theme.text_soft.a(0.72),
                    (false, _) => theme.text_soft.a(0.3),
                },
                bold: false,
                max_width: label_w.max(0.0),
                align: TextAlign::Left,
                clip: None,
                halo: 0.0,
                lines: lines_in(detail_grown, CONTEXT_DETAIL_LINE * scale),
            });
        }
    }

    // The arrows that say the list carries on past what is drawn. Only where
    // it does: an arrow at an end the column has reached is a control that
    // lies about there being somewhere else to go.
    let arrow = CONTEXT_SCROLL_ARROW * scale;
    let strip = CONTEXT_SCROLL_STRIP * scale;
    let rows_top = panel_y + context_rows_top(menu) * scale;
    for (showing, name, y) in [
        (
            menu.scrolled_above(),
            icons::ARROW_UP,
            rows_top - strip * 0.5 - arrow * 0.5,
        ),
        (
            menu.scrolled_below(),
            icons::ARROW_DOWN,
            panel_y + panel_h - GUIDE_MARGIN * scale - strip * 0.5 - arrow * 0.5,
        ),
    ] {
        let Some(slot) = showing.then(|| view.slots.glyph(name)).flatten() else {
            continue;
        };
        inside.quads.push(Quad {
            x: panel_x + (panel_w - arrow) * 0.5,
            y,
            w: arrow,
            h: arrow,
            slot,
            color: [1.0, 1.0, 1.0, 0.55],
            ..Quad::default()
        });
    }

    // Out of the anchor, both halves on the one factor, so the panel and what
    // is on it cannot drift apart on the way.
    let [grown_x, grown_y, grown_w, _] = context_menu_bounds(width, height, menu, open);
    let factor = grown_w / panel_w;
    let offset = [grown_x - panel_x * factor, grown_y - panel_y * factor];
    panel.scale_by(factor, offset);
    panel.fade(ease(open / 0.25));
    inside.scale_by(factor, offset);
    inside.fade(ease(
        (open - CONTEXT_CONTENT_IN) / (1.0 - CONTEXT_CONTENT_IN),
    ));

    scene.quads.extend(panel.quads);
    scene.quads.extend(inside.quads);
    scene.texts.extend(inside.texts);
    scene
}

// --- the centred panel -----------------------------------------------------

/// How wide the panel is, against the same 1080p reference.
///
/// Wider than the context menu by half again, because this one carries
/// sentences rather than commands: an application's own description of itself,
/// and the name of the application in a question about destroying it. Every run
/// the shell draws is one line with an ellipsis where the rest would have been,
/// so width is the only thing standing between a name and being cut in half.
const DIALOG_WIDTH: f32 = 680.0;
/// One answer, on the context menu's row height so the two columns of pressable
/// things are the same size.
const DIALOG_BUTTON: f32 = CONTEXT_ROW;
/// The block the application's icon sits in at the head of the panel, and the
/// icon inside it.
const DIALOG_ICON_BAND: f32 = 96.0;
const DIALOG_ICON: f32 = 72.0;
/// What each kind of line is given: the name of the thing, a sentence about it,
/// a named value, a field being typed into, and the band a rule is centred in.
const DIALOG_HEADING: f32 = 44.0;
const DIALOG_NOTE: f32 = 36.0;
const DIALOG_FIELD: f32 = 42.0;
const DIALOG_SECRET: f32 = 78.0;
const DIALOG_RULE: f32 = 24.0;
/// The mark one typed character is drawn as, and how far apart they sit.
const SECRET_MARK: f32 = 10.0;
const SECRET_MARK_GAP: f32 = 8.0;
/// The band the sign-in code stands in, and how much of it is the quiet zone
/// every reader needs around a code to find its edges.
///
/// Square, and as large as a 680-pixel panel can carry: a code is read by a
/// phone camera held at arm's length in front of a television, and the one
/// thing that decides whether that works is how many pixels a module gets.
const DIALOG_QR: f32 = 360.0;
const DIALOG_QR_QUIET: f32 = 4.0;
/// The two colours the code is drawn in.
///
/// The one thing this shell draws that is not in its own palette, and it
/// cannot be: what reads a QR code is a phone camera held at arm's length in
/// front of a television, and every reader ever written looks for dark modules
/// on a light field. A code drawn in glass over an accent would be a picture of
/// a code. Not quite pure black on pure white — a white card at full intensity
/// on an OLED beside a dark panel is a lamp pointed at the room — but far
/// beyond the contrast any reader asks for.
const QR_LIGHT: crate::theme::Color = crate::theme::Color(0xf4f6f8);
const QR_DARK: crate::theme::Color = crate::theme::Color(0x0a0d12);
/// The row of lights that stands where an answer will be. Three of them, on
/// the height of a field so that a panel does not change size when the thing
/// it was waiting for arrives.
const DIALOG_WAITING: f32 = DIALOG_FIELD;
const WAITING_LIGHTS: usize = 3;
const WAITING_LIGHT: f32 = 12.0;
const WAITING_LIGHT_GAP: f32 = 14.0;
/// How long one full pass of that row takes, in seconds.
const WAITING_CYCLE: f32 = 1.4;
/// How far the rest of the display is dimmed behind it. Deeper than the context
/// menu's: a menu is a note pinned to something the user can still see, and this
/// has taken the screen.
const DIALOG_DIM: f32 = 0.3;
/// How dark the scrim over whatever the compositor is drawing under this surface
/// is, for the same reason.
const DIALOG_SCRIM: f32 = 0.68;
/// The share of the panel's growth that passes before its contents appear.
const DIALOG_CONTENT_IN: f32 = 0.45;
/// And the share over which the glass itself arrives. Everything behind the
/// panel is taken away on exactly this ramp, so nothing is hidden before the
/// thing hiding it can be seen — see [`Scene::dim_text_behind`].
const DIALOG_PANEL_IN: f32 = 0.25;
/// How tall one line of the panel is, in reference pixels.
fn dialog_line_height(line: &Line) -> f32 {
    match line {
        Line::Heading(_) => DIALOG_HEADING,
        Line::Note(_) => DIALOG_NOTE,
        Line::Field { .. } => DIALOG_FIELD,
        Line::Secret { .. } => DIALOG_SECRET,
        // A field with its contents shown stands in the same well as one that
        // hides them, and is therefore exactly as tall: the two are the same
        // control, and a panel whose field changed height between asking for
        // an account name and asking for its password would be two panels.
        Line::Entry(_) => DIALOG_SECRET,
        Line::Qr(_) => DIALOG_QR,
        Line::Waiting => DIALOG_WAITING,
        Line::Rule => DIALOG_RULE,
    }
}

/// Everything on the panel, in settled display coordinates, worked out in one
/// place.
///
/// One function rather than a rectangle each, because the panel is a column:
/// where the buttons are depends on how much was said above them, so two halves
/// computing it separately is two halves that can disagree about a line's
/// height. The drawing and the caller easing the highlight both come through
/// here.
struct DialogLayout {
    icon: Option<[f32; 4]>,
    lines: Vec<[f32; 4]>,
    buttons: Vec<[f32; 4]>,
}

/// How tall the panel is altogether, in reference pixels.
fn dialog_body_height(dialog: &Dialog) -> f32 {
    let said: f32 = dialog.lines().iter().map(dialog_line_height).sum();
    let icon = if dialog.icon().is_some() {
        DIALOG_ICON_BAND
    } else {
        0.0
    };
    GUIDE_MARGIN * 2.0 + icon + said + DIALOG_BUTTON * dialog.buttons.entries().len() as f32
}

/// Where the panel settles: the middle of the display, on both axes.
///
/// Unlike the context menu there is no anchor to stand beside. This one is not
/// *about* something still on screen — it carries its subject inside it — so the
/// middle is the honest place for it, and it is where the eye already is after
/// the menu that raised it folded away.
///
/// The middle of what is *left*, when the keyboard has the foot of the display:
/// see [`Dialog::set_footer`]. It rides down to the true centre again as the
/// board falls away, which is the same movement in reverse.
pub fn dialog_rect(width: f32, height: f32, dialog: &Dialog) -> [f32; 4] {
    let scale = guide_scale(height);
    let inset = PANEL_INSET * scale;
    let panel_w = (DIALOG_WIDTH * scale).min((width - inset * 2.0).max(0.0));
    // Never more than half the display given away: a panel squeezed into a
    // sliver above the keyboard is worse than one the keyboard overlaps, and a
    // very short screen would otherwise leave nothing at all.
    let footer = dialog.footer().min(height * 0.5);
    let room = height - footer;
    let panel_h = (dialog_body_height(dialog) * scale).min((room - inset * 2.0).max(0.0));
    [
        (width - panel_w) * 0.5,
        ((room - panel_h) * 0.5).max(inset),
        panel_w,
        panel_h,
    ]
}

fn dialog_layout(width: f32, height: f32, dialog: &Dialog) -> DialogLayout {
    let scale = guide_scale(height);
    let [panel_x, panel_y, panel_w, _] = dialog_rect(width, height, dialog);
    let margin = GUIDE_MARGIN * scale;
    let padding = GUIDE_ROW_PADDING * scale;
    let mut y = panel_y + margin;

    let icon = dialog.icon().is_some().then(|| {
        let size = DIALOG_ICON * scale;
        let band = DIALOG_ICON_BAND * scale;
        let rect = [
            panel_x + (panel_w - size) * 0.5,
            y + (band - size) * 0.5,
            size,
            size,
        ];
        y += band;
        rect
    });

    let lines = dialog
        .lines()
        .iter()
        .map(|line| {
            let band = dialog_line_height(line) * scale;
            let rect = [panel_x + margin, y, panel_w - margin * 2.0, band];
            y += band;
            rect
        })
        .collect();

    // The answers are chips on the same measure the menu's rows are, so the
    // press, the glide and the lit capsule all land on a rectangle of the size
    // they were designed against.
    let buttons = (0..dialog.buttons.entries().len())
        .map(|_| {
            let band = DIALOG_BUTTON * scale;
            let rect = [
                panel_x + margin,
                y + padding,
                panel_w - margin * 2.0,
                band - padding * 2.0,
            ];
            y += band;
            rect
        })
        .collect();

    DialogLayout {
        icon,
        lines,
        buttons,
    }
}

/// The chip for answer `index`, in settled display coordinates.
///
/// Shared by the drawing below and by the caller easing the selection, for the
/// same reason [`context_menu_row_rect`] is: two places computing one rectangle
/// have to agree.
pub fn dialog_button_rect(
    width: f32,
    height: f32,
    dialog: &Dialog,
    index: usize,
) -> Option<[f32; 4]> {
    dialog_layout(width, height, dialog)
        .buttons
        .get(index)
        .copied()
}

/// Where the panel is when it is `progress` of the way out of the control that
/// opened it.
///
/// The context menu's flight, and the power dialog's before that: one shape
/// carried on a travelling centre, so the contents can ride out on the very same
/// factor and nothing inside moves relative to anything else on the way.
pub fn dialog_bounds(width: f32, height: f32, dialog: &Dialog, progress: f32) -> [f32; 4] {
    let [px, py, pw, ph] = dialog_rect(width, height, dialog);
    let [ax, ay, aw, ah] = dialog.buttons.anchor();
    if pw <= 0.0 {
        return [px, py, pw, ph];
    }
    let factor = lerp((aw / pw).min(1.0), 1.0, progress);
    let cx = lerp(ax + aw * 0.5, px + pw * 0.5, progress);
    let cy = lerp(ay + ah * 0.5, py + ph * 0.5, progress);
    [
        cx - pw * factor * 0.5,
        cy - ph * factor * 0.5,
        pw * factor,
        ph * factor,
    ]
}

/// Push a scene behind the panel: dim it, and drop the text it would otherwise
/// print through.
///
/// Called for every scene drawn before this one — including the context menu
/// that raised it, which is still folding back into its own anchor and whose
/// rows would otherwise show through the middle of the answer.
pub fn recede_behind_dialog(
    scene: &mut Scene,
    width: f32,
    height: f32,
    dialog: &Dialog,
    progress: f32,
) {
    scene.fade(lerp(1.0, DIALOG_DIM, progress));
    scene.dim_text_behind(
        dialog_bounds(width, height, dialog, progress),
        ease(progress / DIALOG_PANEL_IN),
    );
}

/// Everything [`build_dialog`] draws from.
pub struct DialogView<'a> {
    pub dialog: &'a Dialog,
    /// Eased rectangle of the selected answer's chip, in settled display
    /// coordinates — see [`dialog_button_rect`]. `None` snaps it to the
    /// selection.
    pub highlight: Option<[f32; 4]>,
    /// How far the panel is out of the control it came from, 0 shut and 1 open.
    /// Already eased.
    pub open: f32,
    /// How softly the wallpaper behind the overlay is being drawn, so the
    /// panel's glass bends the same wallpaper the layer below is showing.
    pub behind: f32,
    /// The global clock, for the selection pulse.
    pub time: f32,
    /// For the application's icon.
    pub slots: &'a dyn SlotLookup,
}

/// Draw the centred panel: what the shell has to say, and the answers to it.
///
/// Cut from the guide sidebar's glass through [`sidebar_surface`], exactly as
/// the context menu is and for the same reason — it comes out of that menu, and
/// two panels one press apart that were made of different material would read as
/// two different shells.
pub fn build_dialog(view: DialogView, width: f32, height: f32) -> Scene {
    let theme = theme();
    let scale = guide_scale(height);
    let dialog = view.dialog;
    let open = view.open.clamp(0.0, 1.0);
    let pulse = 0.5 + 0.5 * (view.time * std::f32::consts::TAU / PULSE_PERIOD).sin();

    let mut scene = Scene::default();
    // The scrim dims what the *compositor* is drawing under this surface. The
    // shell's own scenes are dimmed by `recede_behind_dialog` instead, which can
    // also take their text away.
    scene.quads.push(Quad {
        x: 0.0,
        y: 0.0,
        w: width,
        h: height,
        slot: SOLID_SLOT,
        color: theme.glass.a(DIALOG_SCRIM * open),
        ..Quad::default()
    });

    let [panel_x, panel_y, panel_w, panel_h] = dialog_rect(width, height, dialog);
    if panel_w <= 0.0 || panel_h <= 0.0 {
        return scene;
    }

    let mut panel = Scene::default();
    panel.quads.extend(sidebar_surface(
        [panel_x, panel_y, panel_w, panel_h],
        scale,
        view.behind,
        1.0,
    ));

    let mut inside = Scene::default();
    let layout = dialog_layout(width, height, dialog);
    let label_padding = GUIDE_LABEL_PADDING * scale;

    // The application's own icon, at the head of the panel. Full size and
    // untinted: this is the one place in the shell where an application is
    // being *described* rather than listed, so it is worth the room.
    if let Some([ix, iy, iw, ih]) = layout.icon {
        inside.quads.push(icon_quad(
            view.slots.slot_for(dialog.icon()),
            ix,
            iy,
            iw.min(ih),
            1.0,
            theme.accent_deep.a(0.75),
        ));
    }

    for (line, [lx, ly, lw, lh]) in dialog.lines().iter().zip(&layout.lines) {
        match line {
            Line::Heading(text) => {
                let size = 26.0 * scale;
                inside.texts.push(Text {
                    content: text.clone(),
                    x: *lx,
                    y: ly + (lh - size) * 0.5 - size * 0.12,
                    size,
                    color: theme.text.a(0.98),
                    bold: true,
                    max_width: *lw,
                    align: TextAlign::Center,
                    clip: None,
                    halo: 0.0,
                    lines: 1,
                });
            }
            Line::Note(text) => {
                let size = 21.0 * scale;
                inside.texts.push(Text {
                    content: text.clone(),
                    x: *lx,
                    y: ly + (lh - size) * 0.5 - size * 0.12,
                    size,
                    color: theme.text_soft.a(0.82),
                    bold: false,
                    max_width: *lw,
                    align: TextAlign::Center,
                    clip: None,
                    halo: 0.0,
                    lines: 1,
                });
            }
            // The label and the value are one row read across, so they sit on
            // one baseline with the width of the panel between them: the names
            // line up down the left and the answers down the right, which is
            // what makes two of these readable as a table rather than as four
            // separate sentences.
            Line::Field { label, value } => {
                let size = 21.0 * scale;
                let y = ly + (lh - size) * 0.5 - size * 0.12;
                let inner = (lw - label_padding * 2.0).max(0.0);
                inside.texts.push(Text {
                    content: label.clone(),
                    x: lx + label_padding,
                    y,
                    size,
                    color: theme.text_soft.a(0.68),
                    bold: false,
                    max_width: inner * 0.5,
                    align: TextAlign::Left,
                    clip: None,
                    halo: 0.0,
                    lines: 1,
                });
                inside.texts.push(Text {
                    content: value.clone(),
                    x: lx + label_padding + inner * 0.5,
                    y,
                    size,
                    color: theme.text.a(0.94),
                    bold: false,
                    max_width: inner * 0.5,
                    align: TextAlign::Right,
                    clip: None,
                    halo: 0.0,
                    lines: 1,
                });
            }
            // A field being typed into: a well sunk into the panel, with one
            // mark per character. Sunk rather than raised — every other control
            // in the shell is a slab lying on the glass, and this is the one
            // thing that is a hole in it, which is what says it takes what the
            // user types instead of doing something when pressed.
            Line::Secret { typed } => {
                let well_h = (DIALOG_SECRET - 24.0) * scale;
                let well = [
                    lx + label_padding,
                    ly + (lh - well_h) * 0.5,
                    (lw - label_padding * 2.0).max(0.0),
                    well_h,
                ];
                inside.quads.push(Quad {
                    x: well[0],
                    y: well[1],
                    w: well[2],
                    h: well[3],
                    slot: SOLID_SLOT,
                    color: theme.glass.a(0.5),
                    radius: well_h * 0.5,
                    ..Quad::default()
                });
                inside.quads.push(Quad {
                    x: well[0],
                    y: well[1],
                    w: well[2],
                    h: well[3],
                    slot: SOLID_SLOT,
                    color: theme.accent_soft.a(0.3 + 0.12 * pulse),
                    radius: well_h * 0.5,
                    border: (1.5 * scale).max(1.0),
                    ..Quad::default()
                });

                // The marks, centred as a group, and capped at what the well
                // holds — a long password must not run out of the panel, and
                // the count is not something the shell should be showing off
                // about anyway.
                let mark = SECRET_MARK * scale;
                let pitch = mark + SECRET_MARK_GAP * scale;
                let room = ((well[2] - label_padding) / pitch).floor().max(0.0) as usize;
                let drawn = (*typed).min(room);
                let run = drawn as f32 * pitch - SECRET_MARK_GAP * scale;
                let start = well[0] + (well[2] - run.max(0.0)) * 0.5;
                for index in 0..drawn {
                    inside.quads.push(Quad {
                        x: start + index as f32 * pitch,
                        y: well[1] + (well[3] - mark) * 0.5,
                        w: mark,
                        h: mark,
                        slot: SOLID_SLOT,
                        color: theme.text.a(0.88),
                        radius: mark * 0.5,
                        ..Quad::default()
                    });
                }
                // The caret, so an empty field is visibly a field waiting for
                // something rather than an empty box.
                let caret_h = well[3] * 0.46;
                inside.quads.push(Quad {
                    x: start + drawn as f32 * pitch + if drawn > 0 { 0.0 } else { -scale },
                    y: well[1] + (well[3] - caret_h) * 0.5,
                    w: (2.0 * scale).max(1.0),
                    h: caret_h,
                    slot: SOLID_SLOT,
                    color: theme.text.a(0.35 + 0.45 * pulse),
                    ..Quad::default()
                });
            }
            // A field whose contents are meant to be read: the same well the
            // password stands in, because it is the same control, with the
            // characters in it instead of one mark each.
            Line::Entry(text) => {
                let well_h = (DIALOG_SECRET - 24.0) * scale;
                let well = [
                    lx + label_padding,
                    ly + (lh - well_h) * 0.5,
                    (lw - label_padding * 2.0).max(0.0),
                    well_h,
                ];
                inside.quads.push(Quad {
                    x: well[0],
                    y: well[1],
                    w: well[2],
                    h: well[3],
                    slot: SOLID_SLOT,
                    color: theme.glass.a(0.5),
                    radius: well_h * 0.5,
                    ..Quad::default()
                });
                inside.quads.push(Quad {
                    x: well[0],
                    y: well[1],
                    w: well[2],
                    h: well[3],
                    slot: SOLID_SLOT,
                    color: theme.accent_soft.a(0.3 + 0.12 * pulse),
                    radius: well_h * 0.5,
                    border: (1.5 * scale).max(1.0),
                    ..Quad::default()
                });

                let size = 26.0 * scale;
                let inset = well_h * 0.5;
                inside.texts.push(Text {
                    content: text.clone(),
                    x: well[0] + inset,
                    y: well[1] + (well[3] - size) * 0.5 - size * 0.12,
                    size,
                    color: theme.text.a(0.94),
                    bold: false,
                    max_width: (well[2] - inset * 2.0).max(0.0),
                    align: TextAlign::Left,
                    clip: None,
                    halo: 0.0,
                    lines: 1,
                });

                // The caret, after what has been typed. Placed from an
                // estimate of the run's width for the reason the keyboard
                // hint's chip is sized from one — the shell cannot measure a
                // run before the GPU shapes it — and clamped inside the well,
                // so an estimate that drifts on a long name puts the caret at
                // the end of the field rather than outside it.
                let run = text.chars().count() as f32 * size * HINT_ADVANCE;
                let caret_h = well[3] * 0.46;
                inside.quads.push(Quad {
                    x: (well[0] + inset + run).min(well[0] + well[2] - inset),
                    y: well[1] + (well[3] - caret_h) * 0.5,
                    w: (2.0 * scale).max(1.0),
                    h: caret_h,
                    slot: SOLID_SLOT,
                    color: theme.text.a(0.35 + 0.45 * pulse),
                    ..Quad::default()
                });
            }
            // The sign-in code, on a white card.
            //
            // The one thing this shell draws that is not in its own palette,
            // and it cannot be: what reads a QR code is a phone camera held at
            // arm's length in front of a television, and every reader ever
            // written looks for dark modules on a light field. A code drawn in
            // glass over an accent would be a picture of a code.
            Line::Qr(code) => {
                // A module is a whole number of pixels, which is the one thing
                // that decides whether a camera can resolve the grid at all.
                // Everything else about the size follows from it: the card is
                // as many whole modules as the band can carry, and the leftover
                // fraction of a pixel becomes margin rather than a row of
                // modules a pixel wider than their neighbours.
                let across = code.width as f32 + DIALOG_QR_QUIET * 2.0;
                let module = (lw.min(*lh) / across).floor().max(1.0);
                let card = module * across;
                let ox = lx + (lw - card) * 0.5;
                let oy = ly + (lh - card) * 0.5;

                inside.quads.push(Quad {
                    x: ox,
                    y: oy,
                    w: card,
                    h: card,
                    slot: SOLID_SLOT,
                    color: QR_LIGHT.a(1.0),
                    radius: module,
                    ..Quad::default()
                });

                let quiet = DIALOG_QR_QUIET * module;
                let dark = QR_DARK.a(1.0);
                for row in 0..code.width {
                    for column in 0..code.width {
                        if !code.at(column, row) {
                            continue;
                        }
                        inside.quads.push(Quad {
                            x: ox + quiet + column as f32 * module,
                            y: oy + quiet + row as f32 * module,
                            w: module,
                            h: module,
                            slot: SOLID_SLOT,
                            color: dark,
                            ..Quad::default()
                        });
                    }
                }
            }
            // Something is happening elsewhere: a row of lights, each coming up
            // a little after the one before it.
            //
            // A row rather than a ring because it stands in a line of the panel
            // — where the code will be, where the answer will be — and because
            // three quads moving in sequence say "still going" without the
            // shell having to draw a spinner it has nowhere else to use.
            Line::Waiting => {
                let light = WAITING_LIGHT * scale;
                let pitch = light + WAITING_LIGHT_GAP * scale;
                let run = WAITING_LIGHTS as f32 * pitch - WAITING_LIGHT_GAP * scale;
                let start = lx + (lw - run) * 0.5;
                for index in 0..WAITING_LIGHTS {
                    // Each light is the same wave a third of a cycle behind its
                    // neighbour, so the row reads as a travelling swell rather
                    // than as three things blinking together.
                    let phase = view.time / WAITING_CYCLE - index as f32 / WAITING_LIGHTS as f32;
                    let lit = 0.5 + 0.5 * (phase * std::f32::consts::TAU).sin();
                    inside.quads.push(Quad {
                        x: start + index as f32 * pitch,
                        y: ly + (lh - light) * 0.5,
                        w: light,
                        h: light,
                        slot: SOLID_SLOT,
                        color: theme.accent.a(0.25 + 0.55 * lit),
                        radius: light * 0.5,
                        ..Quad::default()
                    });
                }
            }
            // The same barely-there hairline the guide rules its bands with,
            // and the context menu its groups.
            Line::Rule => {
                let rule_h = (1.0 * scale).max(1.0);
                inside.quads.push(Quad {
                    x: *lx,
                    y: ly + (lh - rule_h) * 0.5,
                    w: *lw,
                    h: rule_h,
                    slot: SOLID_SLOT,
                    color: theme.accent_soft.a(0.16),
                    ..Quad::default()
                });
            }
        }
    }

    // The selection, drawn before the chips so an outgoing answer can hand its
    // own over as the light arrives.
    let buttons = dialog.buttons.entries();
    let selected = dialog.buttons.selected();
    let selected_rect = view
        .highlight
        .or_else(|| layout.buttons.get(selected).copied());
    if let Some([hx, hy, hw, hh]) = selected_rect {
        // Warm rather than the accent's violet on the answer there is no coming
        // back from — the same light a grave menu row takes, because it is the
        // same fact about a control and the shell says it one way.
        let tint = |alpha: f32| match buttons.get(selected) {
            Some(button) if button.grave => theme.danger.a(alpha),
            _ => theme.accent.a(alpha),
        };
        let glow_h = hh * 2.6;
        inside.quads.push(Quad {
            x: hx + hw * 0.5 - panel_w * 0.62,
            y: hy + hh * 0.5 - glow_h * 0.5,
            w: panel_w * 1.24,
            h: glow_h,
            slot: GLOW_SLOT,
            color: tint(0.13 + 0.05 * pulse),
            ..Quad::default()
        });
        let press = dialog.buttons.press_progress(selected);
        let [lx, ly, lw, lh] =
            scaled_about_centre([hx, hy, hw, hh], press.map_or(1.0, press_scale));
        inside.quads.push(Quad {
            x: lx,
            y: ly,
            w: lw,
            h: lh,
            slot: SOLID_SLOT,
            color: tint(0.5 + 0.05 * pulse),
            radius: lh * 0.5,
            thickness: DEPTH_CONTROL * scale,
            behind: view.behind,
            frost: FROST_CONTROL,
            gloss: GLOSS_FULL,
            ..Quad::default()
        });
    }

    for (index, button) in buttons.iter().enumerate() {
        let Some([rx, ry, rw, rh]) = layout.buttons.get(index).copied() else {
            continue;
        };
        let focused = index == selected;
        let handed_over = match (focused, selected_rect) {
            (true, Some(highlight)) => highlight_arrival(highlight, [rx, ry, rw, rh]),
            _ => 0.0,
        };
        let press = dialog.buttons.press_progress(index);
        let chip = scaled_about_centre([rx, ry, rw, rh], press.map_or(1.0, press_scale));

        // Every answer rests as the same slab of glass, the one that destroys
        // something included. It used to carry its red standing still, on the
        // grounds that a Yes which only turned red once highlighted would be an
        // ordinary button right up to the moment that stopped mattering. What
        // that argument missed is that a panel with one button already lit has
        // answered its own question before the user has — and it left this
        // panel disagreeing with the one Steam's uninstall raises, which is the
        // same question about a different thing. The warmth is on the light
        // now, in both, and the order of the two answers says the rest: the
        // harmless one is first and is the one being stood on.
        inside.quads.push(Quad {
            x: chip[0],
            y: chip[1],
            w: chip[2],
            h: chip[3],
            slot: SOLID_SLOT,
            color: theme.glass_raised.a(0.10),
            radius: chip[3] * 0.5,
            thickness: DEPTH_CONTROL * scale,
            behind: view.behind,
            frost: FROST_CONTROL,
            gloss: GLOSS_QUIET,
            fade: 1.0 - handed_over,
            ..Quad::default()
        });

        let label_size = 23.0 * scale;
        inside.texts.push(Text {
            content: button.label.clone(),
            x: chip[0] + label_padding,
            y: chip[1] + chip[3] * 0.5 - label_size * 0.62,
            size: label_size,
            color: theme.text.a(if focused { 1.0 } else { 0.84 }),
            bold: focused,
            max_width: (chip[2] - label_padding * 2.0).max(0.0),
            align: TextAlign::Center,
            clip: None,
            halo: 0.0,
            lines: 1,
        });
    }

    // Out of the control it came from, both halves on the one factor.
    let [grown_x, grown_y, grown_w, _] = dialog_bounds(width, height, dialog, open);
    let factor = grown_w / panel_w;
    let offset = [grown_x - panel_x * factor, grown_y - panel_y * factor];
    panel.scale_by(factor, offset);
    panel.fade(ease(open / DIALOG_PANEL_IN));
    inside.scale_by(factor, offset);
    inside.fade(ease((open - DIALOG_CONTENT_IN) / (1.0 - DIALOG_CONTENT_IN)));

    scene.quads.extend(panel.quads);
    scene.quads.extend(inside.quads);
    scene.texts.extend(inside.texts);
    scene
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
    /// How far it is out of the display's bottom edge: 0 below it, 1 in place.
    /// Eased already, the way the power dialog's growth is — see
    /// [`keyboard::Osk::animate`], which is what carries it back down again.
    pub arrived: f32,
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

    // It rises from under the display's edge rather than fading in, and leaves
    // the same way. A keyboard that appeared on the spot over a running
    // application reads as the application having done something; one that
    // slides up reads as the shell putting it there — and one that vanished
    // on the keystroke that dismissed it would read as the shell having lost
    // it, so the same travel carries it back off the bottom.
    let arrived = view.arrived;
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
                clip: None,
                halo: 0.0,
                lines: 1,
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
                clip: None,
                halo: 0.0,
                lines: 1,
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
        clip: None,
        halo: 0.0,
        lines: 1,
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
    /// How far its unread mark has arrived, 0 for a tile that has none.
    badge: f32,
}

/// The unread mark on the bell: how wide the bead is as a share of the tile,
/// how far its centre sits in from the tile's top right corner, and how much
/// wider than the bead the dark moat around it is.
///
/// In the corner rather than on the glyph. A bell already says *announcements*,
/// and a mark laid over it would be a second thing to read inside one shape;
/// a bead at the corner is the badge every phone in the user's pocket puts
/// there, which is what makes it legible without a legend.
///
/// The inset is measured so the bead clears both the chip's rounded corner and
/// the glyph inside it, which reaches [`GUIDE_TILE_GLYPH`] of the tile: the
/// bead's own edge stops short of the bell's shoulder rather than resting on
/// it.
const TILE_BADGE: f32 = 0.21;
const TILE_BADGE_INSET: f32 = 0.245;
const TILE_BADGE_MOAT: f32 = 1.34;

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

    // A glyph the shell could not rasterise leaves the chip empty rather than
    // a wrong drawing in its place — but not the mark below, which is the one
    // thing on this tile that is news rather than decoration.
    if let Some(slot) = item.glyph().and_then(|name| slots.glyph(name)) {
        let glyph = h * GUIDE_TILE_GLYPH;
        // Four depths, and the order is the point: a switch that is on is the
        // brightest thing in the column, a switch that is merely selected is
        // next, and one that cannot be reached at all is a long way behind
        // both.
        let lit = match (state.live, state.on, state.focused) {
            // A ghost of a glyph, inside the hairline the caller drew instead
            // of a chip. Three things say the same thing about a tile nothing
            // can be done with — no chip, no light in the glyph, and a
            // highlight that refuses to stop on it — because on a panel of
            // five lit controls no one of them is enough on its own.
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
    }

    // Last, so it is over the glyph rather than under it: it is a thing sitting
    // on the tile, and the corner it sits in belongs to it.
    quads.extend(unread_badge([x, y, w, h], state.badge, alpha));
    quads
}

/// The unread mark, laid into the tile at `[x, y, w, h]`, `arrived` of the way
/// on.
///
/// A bead and the dark moat it stands in. The moat is what makes it *sit on*
/// the tile rather than be painted into it, and it is what the mark needs to
/// survive both backgrounds it is drawn against: the plain glass chip, and the
/// lit accent capsule that glides onto that chip when the tile is selected.
///
/// White rather than accent-coloured for the reason the quick-settings bars
/// are — see [`quick_bar`]. A mark tinted with the accent would disappear into
/// the selection the moment the user walked the highlight onto the very tile
/// they were being told to open.
///
/// No number on it, at the user's direction, and it is the better answer
/// anyway: a count is a thing to read, and this is a thing to notice. What is
/// worth knowing from across a room is *something arrived*; how many there are
/// is a question the list itself answers, one row per line, a press away.
fn unread_badge([x, y, w, h]: [f32; 4], arrived: f32, alpha: f32) -> Vec<Quad> {
    let arrived = arrived.clamp(0.0, 1.0);
    if arrived <= 0.0 {
        return Vec::new();
    }
    let theme = theme();
    // It grows out of its own centre, which stays put: a bead that also
    // travelled would be a second movement to follow in a corner the eye is
    // only glancing at.
    let centre = [x + w * (1.0 - TILE_BADGE_INSET), y + h * TILE_BADGE_INSET];
    let bead = w * TILE_BADGE * arrived;
    let moat = bead * TILE_BADGE_MOAT;
    let disc = |size: f32, color: [f32; 4]| Quad {
        x: centre[0] - size * 0.5,
        y: centre[1] - size * 0.5,
        w: size,
        h: size,
        slot: SOLID_SLOT,
        color,
        radius: size * 0.5,
        ..Quad::default()
    };
    vec![
        disc(moat, theme.glass.a(0.5 * arrived * alpha)),
        disc(bead, theme.rim.a(arrived * alpha)),
    ]
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
    let [x, y, _, h] = chip;
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

    let [track_x, middle, track_w, _] = bar_track_line(chip, scale);
    quads.extend(track([track_x, middle, track_w, 0.0], level, scale, alpha));
    quads
}

/// The line a quick-settings bar's groove runs along inside the chip at `chip`:
/// `[x, middle, width, 0]`, in the same shape [`track`] takes.
///
/// Shared with the hit test, because a bar is the one control in the sidebar
/// where *where* it was clicked is the whole of the answer.
fn bar_track_line(chip: [f32; 4], scale: f32) -> [f32; 4] {
    let [x, y, w, h] = chip;
    let glyph = h * BAR_GLYPH;
    let track_x = x + BAR_INSET * scale + glyph + BAR_GAP * scale;
    [
        track_x,
        y + h * 0.5,
        (x + w - BAR_INSET * scale) - track_x,
        0.0,
    ]
}

/// What a quick-settings bar in the chip at `chip` is being set to by a click
/// at `x`, or `None` when the click was on the glyph at the groove's head
/// rather than on the groove itself.
///
/// The head is not part of the value. On the volume bar it is the crossed-out
/// speaker, and a click there means silence — which is what pressing the bar
/// has always meant — rather than "as quiet as this display can express".
pub fn bar_level_at(chip: [f32; 4], height: f32, x: f32) -> Option<f32> {
    let [track_x, _, track_w, _] = bar_track_line(chip, guide_scale(height));
    (x >= track_x && track_w > 0.0).then(|| ((x - track_x) / track_w).clamp(0.0, 1.0))
}

/// A level drawn as a track: the groove, the part of it that is filled, and the
/// handle on the end of the fill.
///
/// `band` is `[x, middle, width, _]` — the line it is centred on rather than a
/// box, because the track is always [`BAR_TRACK`] thick and what a caller knows
/// is where it should run. Shared by the sidebar's quick-settings bars and by
/// the mixer's rows, so a volume is the same picture wherever it is being set.
fn track([x, middle, w, _]: [f32; 4], level: Level, scale: f32, alpha: f32) -> Vec<Quad> {
    let theme = theme();
    if w <= 0.0 {
        return Vec::new();
    }
    let mut quads = Vec::with_capacity(3);
    let track_h = BAR_TRACK * scale;
    let track_y = middle - track_h * 0.5;

    quads.push(Quad {
        x,
        y: track_y,
        w,
        h: track_h,
        slot: SOLID_SLOT,
        color: theme.rim.a(0.20 * alpha),
        radius: track_h * 0.5,
        ..Quad::default()
    });

    // A muted session is still at whatever volume it was left at, so the fill
    // stays where it is and goes quiet instead of emptying — turning the sound
    // back on must not look like it also turned it up.
    let filled = w * level.value.clamp(0.0, 1.0);
    let lit = if level.muted { 0.30 } else { 0.95 };
    if filled > 0.0 {
        quads.push(Quad {
            x,
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
    //
    // It is kept inside the ends of its own groove rather than centred on the
    // fill wherever that reaches: a control at the top of its range would
    // otherwise hang half a handle past the track, over whatever the track was
    // laid on and, on a narrow row, out through the chip's rounded corner. The
    // fill is still the reading — this moves the dot by half its own width at
    // the two extremes and by nothing anywhere else.
    let handle = track_h * BAR_HANDLE;
    let at = (x + filled).clamp(x + handle * 0.5, x + (w - handle * 0.5).max(handle * 0.5));
    quads.push(Quad {
        x: at - handle * 0.5,
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

/// The drawing a row stands on.
///
/// Applications are looked up with the theme's fallback behind them, because a
/// launcher showing a hole where an icon should be is worse than showing the
/// generic executable. The shell's own rows are not: a subcategory or a colour
/// that came out as that same generic icon would read as an application in the
/// column, which is the one thing it is not.
fn entry_slot(entry: &Entry, slots: &impl SlotLookup) -> Option<u32> {
    match entry {
        Entry::App(app) => slots.slot_for(app.icon.as_deref()),
        _ => entry.icon().and_then(|name| slots.glyph(name)),
    }
}

/// The vertical bar a value on a scale is set on, in the room a row's icon
/// would have had.
///
/// How tall the track is as a share of the display, how wide it is drawn, and
/// how much bigger than that the handle on the end of the fill is.
///
/// The height is a share rather than a fixed size because it is the *reach* of
/// the control: what makes a bar worth having over a list is being able to see
/// the whole range at once, and on a taller screen there is more room to see it
/// in. The width is not, because it is a *thickness* — and because it has to be
/// several times the bevel this is cut with, or the groove is all rim and no
/// face and reads as a painted stripe rather than as a pane with an edge.
const COLUMN_BAR_HEIGHT: f32 = 0.40;
const COLUMN_BAR_WIDTH: f32 = 40.0;
const COLUMN_BAR_HANDLE: f32 = 1.2;

/// How far inside the groove the filled part is drawn, as a share of the
/// groove's width.
///
/// The fill is a pane in its own right, laid *in* the channel rather than over
/// it, so the groove keeps a rim of its own glass all the way round the light.
/// Without it the two panes share an edge and the bevel of the one underneath
/// has nowhere to be, which is a coloured stripe with a line drawn round it.
///
/// A rim, though, and not a margin: the light is what the control is *for*, and
/// a fill held well inside its groove reads as two bars, a thin one drawn down
/// the middle of a fat one.
const COLUMN_BAR_INSET: f32 = 0.10;

/// Where a bar's groove is drawn, given the room the row's icon would have had.
///
/// It is worked out apart from the drawing because two other things are cut to
/// this shape before the bar is reached: the light behind the row, and the
/// decision not to put a disc under it.
fn column_bar_box(x: f32, y: f32, height: f32, scale: f32) -> [f32; 4] {
    let w = COLUMN_BAR_WIDTH * scale;
    let h = height * COLUMN_BAR_HEIGHT;
    [x - w / 2.0, y - h / 2.0, w, h]
}

/// What a press at `(x, y)` sets a bar whose groove is drawn at `track`, or
/// `None` where the press was not on the control at all.
///
/// The other half of [`column_bar`], and read off the same arithmetic: the
/// level is measured against the channel the fill is laid in rather than
/// against the whole groove, so a press on the handle is the value the handle
/// is already standing on and a press at either end reaches that end exactly.
///
/// `reach` is how wide the control answers, and it is the room the row's icon
/// would have had rather than the width of the drawn groove. A track is aimed
/// at across its width and pressed along its length: the groove is a pane
/// [`COLUMN_BAR_WIDTH`] across where an icon is a hundred-odd, and a bar the
/// hand had to be threaded into would be the one control in the shell that
/// wanted care. It is the same width the column measures its own strip from,
/// which is what keeps a press on the track from being one on the wallpaper
/// beside it. Past the ends of the track there is no value to be asking for,
/// so the length is the track itself.
fn column_bar_level_at([left, top, w, h]: [f32; 4], reach: f32, x: f32, y: f32) -> Option<f32> {
    if (x - (left + w / 2.0)).abs() > reach / 2.0 || y < top || y > top + h {
        return None;
    }
    let inset = w * COLUMN_BAR_INSET;
    let room = h - inset * 2.0;
    (room > 0.0).then(|| ((top + inset + room - y) / room).clamp(0.0, 1.0))
}

/// The bar itself: the groove, the part of it that is filled, and the handle
/// on the end of the fill.
///
/// The same three marks the guide's quick-settings bars are made of — see
/// [`track`], which is this lying down — because a level is one picture and
/// the shell should not have two of it. What differs is the axis, the material
/// and what the fill is *coloured*.
///
/// The material, because of where each of them stands. The guide's bar is one
/// control among several already lying on a pane of glass, and putting glass on
/// glass would only blur the pane twice; this one stands on the wallpaper, in
/// the place the chosen row's glass disc would have been, and it *is* that
/// glass — every one of these three marks is a slab of the material the rest of
/// the shell's controls are cut from, so a level set in a column is the same
/// object as a button, not a diagram of one. That is also why nothing here is
/// drawn on: the rim light, the sheen down the bevel and the colour split at
/// the edge are what a slab of this shape does with the one lamp the shell is
/// lit by, worked out in the shader.
///
/// The colour, because a volume has none of its own and a colour temperature is
/// nothing but one: where the guide's bar fills with the rim light this one
/// fills with the light it stands for.
///
/// The fill grows upward from the foot, which is the way a quantity is read on
/// anything vertical, and it is what Up adds to.
fn column_bar(
    [x, top, track_w, track_h]: [f32; 4],
    bar: &crate::apps::Bar,
    alpha: f32,
    lit: f32,
) -> Vec<Quad> {
    let theme = theme();
    let scale = track_w / COLUMN_BAR_WIDTH;
    let depth = DEPTH_CONTROL * scale;
    let mut quads = Vec::with_capacity(3);

    // The groove: the row's glass, cut to the shape of the control. Stained
    // with the accent as the disc under a chosen icon is, and *less* than it,
    // because what is behind this is already the accent bloom and because the
    // empty part of a bar has to read as empty — a groove tinted as strongly as
    // a disc is a second fill above the real one, in a colour that means
    // nothing. Lightly enough that the pane is mostly what it is transmitting,
    // which is how a pane stays glass instead of turning into paint.
    quads.push(Quad {
        x,
        y: top,
        w: track_w,
        h: track_h,
        slot: SOLID_SLOT,
        color: theme.accent.a(0.08 * lit.clamp(0.0, 1.0) + 0.05),
        radius: track_w / 2.0,
        thickness: depth,
        frost: FROST_CONTROL,
        gloss: GLOSS_FULL,
        fade: alpha,
        ..Quad::default()
    });

    // The light in the channel. Held clear of the groove's rim on both sides so
    // the two panes each keep a bevel of their own, and the same at the foot:
    // the fill is lying in the groove, not filling it in.
    let inset = track_w * COLUMN_BAR_INSET;
    let (lit_w, room) = (track_w - inset * 2.0, track_h - inset * 2.0);
    let filled = room * bar.fill.clamp(0.0, 1.0);
    // The colour the value stands for, where it stands for one. Its own colour
    // rather than the shell's: what this is showing is what the *screen* is
    // about to look like, and a preview stained with the accent would be a
    // preview of the wrong thing. Stained to the full, which on a slab this
    // shallow still leaves the rim clear enough to read as an edge — see the
    // stain term in the shader, which thins towards the bevel by itself.
    let colour = match bar.swatch {
        Some(colour) => colour.a(alpha),
        None => theme.rim.a(0.92 * alpha),
    };
    // A fill shorter than it is wide has no length left to be a capsule of, and
    // is drawn as the round end alone rather than as a squashed one.
    if filled > 0.0 {
        let height = filled.max(lit_w);
        quads.push(Quad {
            x: x + inset,
            y: top + inset + room - height,
            w: lit_w,
            h: height,
            slot: SOLID_SLOT,
            color: colour,
            radius: lit_w / 2.0,
            thickness: depth,
            gloss: GLOSS_FULL,
            fade: alpha,
            ..Quad::default()
        });
    }

    // The handle. Only there to say that the end of the fill is a place the
    // value *is* rather than where a drawing happens to stop — so it is drawn
    // across the groove, wider than the channel it marks, the way the pointer
    // on any scale overhangs the scale.
    //
    // Kept inside the ends of its own groove, exactly as the guide's is: a
    // control at the top of its range would otherwise hang half a handle past
    // the track, over whatever the track was laid on.
    let handle = track_w * COLUMN_BAR_HANDLE;
    let travel = top + inset + room - filled;
    let at = travel.clamp(top + handle / 2.0, top + track_h - handle / 2.0);
    quads.push(Quad {
        x: x + track_w / 2.0 - handle / 2.0,
        y: at - handle / 2.0,
        w: handle,
        h: handle,
        slot: SOLID_SLOT,
        color: theme.rim.a(0.55),
        radius: handle / 2.0,
        thickness: depth,
        frost: FROST_CONTROL,
        gloss: GLOSS_FULL,
        fade: alpha,
        ..Quad::default()
    });
    quads
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
///
/// However deep the path, too. Opening a subcategory slides the whole chain
/// along rather than adding to the end of it, so the column being browsed is
/// always the one standing on the cross — which is what makes this one
/// rectangle rather than one per depth.
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
        0.0,
    );
    let size = ITEM_ICON_FOCUSED * scale * ITEM_DISC;
    [cross_x - size * 0.5, y - size * 0.5, size, size]
}

/// The same, for a row that stands on a Steam cover rather than on a disc.
///
/// A library is a column of pictures and the picture *is* the row — see
/// [`Cards`] — so this is the chosen card at the cross, at the shape Valve's
/// portrait capsule comes in. A game brought back out of [`launch_origin`]
/// would grow out of a small square hole in the middle of the cover the user
/// pressed, which reads as the window arriving from somewhere else.
///
/// Derived from the same numbers the column is laid out with, and held to the
/// drawing by `a_game_opens_out_of_the_cover_it_was_chosen_from`.
pub fn cover_origin(width: f32, height: f32) -> [f32; 4] {
    let scale = guide_scale(height);
    let cards = Cards::of(COVER_ASPECT);
    let y = height * BAR_CROSS_Y + gap_below(Some(cards)) * scale;
    let h = cards.focused * scale;
    let w = cards.width(h);
    [width * BAR_CROSS_X - w * 0.5, y - h * 0.5, w, h]
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
    /// Whether this is a game opening out of the Steam library, which is drawn
    /// a different way entirely — see [`build_launch`].
    pub game: bool,
    /// Steam's own logo for that game, if there is one and it has arrived.
    ///
    /// Its own picture rather than an atlas slot, because unlike an icon a
    /// logo has a shape of its own: a title set in one line is six times as
    /// wide as it is tall and a stacked one is square, and the splash fits
    /// each into the same box rather than squashing either.
    pub logo: Option<crate::gpu::Thumb>,
    /// Which step of starting it the line beside the indicator says it is on.
    pub doing: Option<crate::launch::Doing>,
    /// How black the display is on its way to the game — see
    /// [`crate::launch::Launch::blackout`]. Unshaped.
    pub blackout: f32,
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

/// The corner of the screen where announcements arrive, against 1080p like
/// every other metric here.
///
/// A bubble is [`CONTEXT_WIDTH`] across — the width every panel in the shell
/// is, and deliberately: the corner and the list are the same announcement seen
/// twice, and two pieces of furniture carrying the same words at two widths
/// read as unrelated.
///
/// It does *not* follow the panel's extra hundred — see
/// [`NOTIFICATION_EXTRA_WIDTH`]. The list took that width for the two things a
/// bubble does not have: a button on the end of every row, and a summary given
/// room to wrap. This is the one thing the shell draws over an application that
/// nobody asked for, so it takes a corner and not a column — a bubble a third
/// of the screen across would be the shell deciding that its news matters more
/// than the game underneath it.
const TOAST_WIDTH: f32 = CONTEXT_WIDTH;
/// How far the stack stands off the display's corner, and the air between one
/// bubble and the next.
const TOAST_INSET: f32 = 30.0;
const TOAST_GAP: f32 = 14.0;
/// The air to the left and right of what is written on a bubble.
///
/// [`GUIDE_MARGIN`], because that is what every panel in this shell insets its
/// contents by — the guide's column, the context menu's rows, its title. A
/// bubble is a panel and inherits the number rather than choosing one.
const TOAST_PAD: f32 = GUIDE_MARGIN;

/// How tall a bubble is.
///
/// Sized to the two lines it carries and not much more. It was [`MIXER_ROW`]
/// plus a full [`TOAST_PAD`] top and bottom, on the reasoning that a two-line
/// row is worth the same everywhere — and that was wrong twice over. A mixer
/// row is that tall because it holds a name *and a track*; two lines of
/// writing need far less. And a panel margin is what a panel puts around a
/// column of rows, where it separates the glass from things that have their
/// own edges; here it was simply air, doubled, around a block of text that
/// already has its own.
///
/// The stack is what makes the difference matter. Three of these at the old
/// height reached nearly half the depth of a 1080p display, which for
/// something nobody asked for is a great deal of somebody's game to be
/// standing on.
const TOAST_HEIGHT: f32 = 104.0;

/// Where the two lines sit inside a bubble, as shares of its height: the
/// middle of the summary's line, and the middle of the one under it.
///
/// Their own numbers rather than the panel's [`MIXER_NAME_LINE`], because the
/// thing they are shares *of* is no longer the same height. Set so the pair is
/// optically centred: the gap above the summary and the gap below the body are
/// equal, which is what stops a short bubble reading as top-heavy.
const TOAST_TITLE_LINE: f32 = 0.36;
const TOAST_BODY_LINE: f32 = 0.68;

/// The picture at the head of a bubble, as a share of the pane's height.
///
/// Smaller than the [`MIXER_ICON`] a row gets. An icon on a row is sharing its
/// height with nothing else and can fill it; this one sits beside two lines of
/// writing in a pane sized to them, and at a row's proportion it grows taller
/// than the text it is labelling and starts to read as the subject rather than
/// the mark.
const TOAST_ICON: f32 = 0.46;

/// How strongly a bubble's writing is ringed in shade.
///
/// The one place in the shell where a run needs it — see [`crate::gpu::Text`]'s
/// `halo`. Everywhere else the shell chose what is behind the words; here it
/// did not. A pane is nearly all borrowed light, and a bubble stands wherever
/// the corner of the screen happens to be: over a wallpaper the writing is
/// clear, over the bar's white category icons it is grey on white.
///
/// The alternative was to take the glass down behind the text until it read,
/// and that is the wrong trade. It darkens a rectangle of somebody's game for
/// the sake of a sentence, and it makes the panel look like the flat slab this
/// one was rewritten to stop being. A ring costs the letters' own outline and
/// nothing else.
const TOAST_HALO: f32 = 1.0;

/// The same for the line under it, and stronger — the one number here that is
/// not the same for both lines.
///
/// It reads worse than the summary and every reason is stacked the same way:
/// it is smaller, so its ring is thinner in real pixels; it is not bold, so
/// there is less letter to carry the contrast; and it is drawn in the soft
/// grey that makes it the second line rather than the first, which is the
/// least of the three but the one that costs it against a white icon
/// underneath. Matching the summary's ring leaves it the harder of the two to
/// read, which is backwards — the line that is quieter *by design* still has
/// to be legible, or it is not a second line, it is a smudge.
const TOAST_BODY_HALO: f32 = 1.75;

/// One bubble, as the corner needs it: what it says, what it came from, and
/// where it has got to in its own life.
///
/// The words are borrowed rather than owned because the store already holds
/// them, and the stage and progress come from [`crate::notify::Toast`] rather
/// than being worked out here — the corner draws what the store says is
/// happening, and does not keep a second opinion about when a bubble is due to
/// leave.
pub struct ToastCard<'a> {
    pub title: &'a str,
    pub body: &'a str,
    pub icon: Option<u32>,
    pub stage: crate::notify::Stage,
    pub progress: f32,
}

/// Where a bubble stands and how solid it is, from the stage it is in.
///
/// One function for all three stages so that a bubble caught mid-flight and
/// sent away — which is what dismissing its row does — carries on from where
/// it is rather than snapping to the start of the exit.
///
/// Returns `(x offset from resting, alpha, how much of its slot it still
/// occupies)`. The third is what makes the stack close up: a bubble on its way
/// out gives its slot back as it goes, so the ones below have finished
/// climbing by the time it is swept, and nothing jumps.
fn toast_flight(stage: crate::notify::Stage, progress: f32, travel: f32) -> (f32, f32, f32) {
    match stage {
        crate::notify::Stage::In => {
            let arrived = ease(progress);
            (travel * (1.0 - arrived), arrived, 1.0)
        }
        crate::notify::Stage::Sitting => (0.0, 1.0, 1.0),
        crate::notify::Stage::Out => {
            let gone = ease(progress);
            (travel * gone, 1.0 - gone, 1.0 - gone)
        }
    }
}

/// The corner of the screen: bubbles stacked down from the top right.
///
/// Oldest at the top, which is the opposite of the panel behind the bell and
/// deliberately so. The list is read from the top and the newest row is the
/// one somebody opened it for; the corner is *watched*, and a stack that
/// reordered itself as things arrived would move the bubble somebody was
/// half-way through reading. New ones come in underneath.
pub fn build_toasts(cards: &[ToastCard], width: f32, height: f32, behind: f32) -> Scene {
    let theme = theme();
    let scale = guide_scale(height);
    let mut scene = Scene::default();
    let pad = TOAST_PAD * scale;
    let label_padding = GUIDE_LABEL_PADDING * scale;

    for (card, (rect, alpha)) in cards.iter().zip(toast_layout(cards, width, height)) {
        if alpha <= 0.0 {
            continue;
        }

        // The pane, and nothing else: this is the context menu's panel with
        // the rows left off it.
        //
        // A chip was tried here and is wrong twice over. A chip is what a *row*
        // looks like, and a row is a thing that can be moved onto and pressed —
        // the corner is neither, so a bubble wearing one is inviting a press
        // that has nowhere to land. And a chip is a second slab of glass laid
        // on the first, which on a panel this small leaves the pane showing as
        // a rim around it: a bordered box rather than a panel with something
        // written on it.
        //
        // So the words sit directly on the glass, at the panel's own margin —
        // exactly where a context menu puts its title.
        scene
            .quads
            .extend(sidebar_surface(rect, scale, behind, alpha));

        let icon = rect[3] * TOAST_ICON;
        let icon_x = rect[0] + pad;
        scene.quads.push(icon_quad(
            card.icon,
            icon_x,
            rect[1] + (rect[3] - icon) * 0.5,
            icon,
            alpha,
            theme.glass_raised.a(0.5 * alpha),
        ));

        let text_x = icon_x + icon + label_padding;
        let text_w = (rect[0] + rect[2] - text_x - pad).max(0.0);
        // Two lines in the panel's own places — see MIXER_NAME_LINE, which is
        // where a row puts the name it has something under. The second one
        // only if the program had anything more to say: a bubble with an empty
        // line under its sentence is a sentence sitting above the middle of
        // the chip for no reason, so it is centred instead when it stands
        // alone.
        let title_size = 23.0 * scale;
        let body_size = 20.0 * scale;
        let has_body = !card.body.trim().is_empty();
        let title_y = if has_body {
            rect[1] + rect[3] * TOAST_TITLE_LINE - title_size * 0.5
        } else {
            rect[1] + rect[3] * 0.5 - title_size * 0.62
        };
        scene.texts.push(Text {
            content: card.title.to_string(),
            x: text_x,
            y: title_y,
            size: title_size,
            color: theme.text.a(0.97 * alpha),
            bold: true,
            max_width: text_w,
            align: TextAlign::Left,
            clip: None,
            // Faded with the bubble, because the ring is no longer thinned by
            // the run's own colour — see `halo_copies`. A bubble that flew off
            // the display leaving its outlines standing would be the worst of
            // both.
            halo: TOAST_HALO * alpha,
            lines: 1,
        });
        if has_body {
            // One line of it. A notification body can be a paragraph — some
            // programs put a whole email in one — and a bubble that grew to
            // fit would be a program deciding how much of the screen it may
            // have. The rest is on the row behind the bell, which is a list
            // somebody chose to open.
            scene.texts.push(Text {
                content: card.body.lines().next().unwrap_or_default().to_string(),
                x: text_x,
                y: rect[1] + rect[3] * TOAST_BODY_LINE - body_size * 0.5,
                size: body_size,
                color: theme.text_soft.a(0.72 * alpha),
                bold: false,
                max_width: text_w,
                align: TextAlign::Left,
                clip: None,
                halo: TOAST_BODY_HALO * alpha,
                lines: 1,
            });
        }
    }

    scene
}

/// Where each bubble stands and how solid it is, in order.
///
/// Shared by the drawing above and by the caller cutting the runs underneath
/// them, for the same reason the card layout is shared with the compositor:
/// two places computing one rectangle have to agree, and a bubble that cut its
/// hole somewhere other than where it landed is worse than one that cut none.
fn toast_layout(cards: &[ToastCard], width: f32, height: f32) -> Vec<([f32; 4], f32)> {
    let scale = guide_scale(height);
    let card_w = TOAST_WIDTH * scale;
    let card_h = TOAST_HEIGHT * scale;
    let inset = TOAST_INSET * scale;
    let gap = TOAST_GAP * scale;
    let rest_x = width - inset - card_w;
    // Far enough to be off the display entirely, so a bubble is never seen
    // cut in half by the edge it came in through.
    let travel = card_w + inset * 2.0;

    let mut placed = Vec::with_capacity(cards.len());
    let mut y = inset;
    for card in cards {
        let (offset, alpha, slot) = toast_flight(card.stage, card.progress, travel);
        placed.push(([rest_x + offset, y, card_w, card_h], alpha));
        // Its own top is taken before its slot shrinks, so a bubble on its way
        // out slides sideways from where it was standing while the ones under
        // it climb into the room it is giving back.
        y += (card_h + gap) * slot;
    }
    placed
}

/// The rectangles alone, for whatever the bubbles have landed on top of.
///
/// One per card and in the same order, including the ones that are invisible:
/// a bubble is fully off the display at both ends of its flight, so a rect
/// nothing can be seen in cuts nothing out of what is underneath, and an
/// answer that quietly dropped entries would stop lining up with the cards it
/// was asked about.
pub fn toast_rects(cards: &[ToastCard], width: f32, height: f32) -> Vec<[f32; 4]> {
    toast_layout(cards, width, height)
        .into_iter()
        .map(|(rect, _)| rect)
        .collect()
}

// --- the volume control the keys raise -------------------------------------

/// How tall the control the volume keys raise is.
///
/// Added up rather than chosen, out of the one thing standing in it: the glyph
/// at its head is [`BAR_GLYPH`] of the pane's height and stands [`BAR_INSET`]
/// in from its left edge, and this is the height at which the air under that
/// glyph is the same as the air beside it. Anything else reads as a bar that
/// has slipped up or down inside its own panel.
const VOLUME_HEIGHT: f32 = BAR_INSET * 2.0 / (1.0 - BAR_GLYPH);

/// How far it stands off the foot of the display.
///
/// The corner's own distance — see [`TOAST_INSET`] — because two pieces of this
/// shell's furniture standing different distances from the edge of one screen
/// is one of them looking misplaced.
const VOLUME_INSET: f32 = TOAST_INSET;

/// How far below where it settles it comes up from.
///
/// A short way, and not the whole panel: this is not something arriving from
/// off the display like a bubble is — it belongs to a key that has already
/// taken effect, and the movement is only there to say which direction it came
/// from. Far enough to be a rise, near enough that the eye has nothing to
/// follow.
const VOLUME_TRAVEL: f32 = 26.0;

/// The volume, as the control the keys raise needs it: where it stands, and how
/// far through its own arrival the panel is.
pub struct VolumeView<'a> {
    pub level: Level,
    /// 0 to 1, and linear — the easing is applied here, so that a showing
    /// turned around half-way through carries on from where it was.
    pub fade: f32,
    /// The blur the pane bends what is behind it by, as every other pane in
    /// the shell does.
    pub behind: f32,
    pub slots: &'a dyn SlotLookup,
}

/// Where the control stands: centred at the foot of the display, risen by
/// however much of its arrival is done.
///
/// Its own function because two other things are cut to this shape — the runs
/// underneath it, which a pane in front of them has to cut, and the input
/// region, which must not be cut at all: this is drawn over an application
/// somebody is still using and takes neither its keys nor its clicks.
pub fn volume_rect(width: f32, height: f32, fade: f32) -> [f32; 4] {
    let scale = guide_scale(height);
    let w = CONTEXT_WIDTH * scale;
    let h = VOLUME_HEIGHT * scale;
    let rest = height - VOLUME_INSET * scale - h;
    [
        (width - w) * 0.5,
        rest + VOLUME_TRAVEL * scale * (1.0 - ease(fade)),
        w,
        h,
    ]
}

/// The control itself: one pane, with the sidebar's own volume bar laid on it.
///
/// Deliberately the same bar, drawn by the same code — see [`quick_bar`]. The
/// keys and the row in the guide set one thing, and a shell that drew a
/// different picture for each would be telling the user they are two controls.
/// What the pane adds is somewhere for it to stand: the row has the sidebar's
/// glass under it, and this one is over a game.
pub fn build_volume(view: VolumeView, width: f32, height: f32) -> Scene {
    let mut scene = Scene::default();
    let alpha = ease(view.fade.clamp(0.0, 1.0));
    if alpha <= 0.0 {
        return scene;
    }
    let scale = guide_scale(height);
    let rect = volume_rect(width, height, view.fade);

    scene
        .quads
        .extend(sidebar_surface(rect, scale, view.behind, alpha));
    scene.quads.extend(quick_bar(
        rect,
        Bar::Volume,
        view.level,
        scale,
        alpha,
        view.slots,
    ));
    scene
}

/// Draw the splash — one of two, depending on what is starting.
///
/// An application gets a panel out of the tile it was chosen from, with its
/// icon and name on it and something that says the wait is expected.
/// Everything on it arrives *after* the panel has, so nothing is legible while
/// it is still tile-sized — the same reason the power dialog's rows hold back,
/// and the difference between an application opening and a screenful of
/// furniture being scaled up.
///
/// A game gets no panel at all: see [`build_game_launch`].
pub fn build_launch(view: LaunchView, width: f32, height: f32) -> Scene {
    if view.game {
        return build_game_launch(view, width, height);
    }
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
        clip: None,
        halo: 0.0,
        lines: 1,
    });

    // The ring, round the icon it is waiting on.
    scene.quads.extend(launch_ring(
        [icon_x, icon_y],
        icon_size * 0.5 + 34.0 * scale,
        9.0 * scale,
        view.time,
        view.waiting,
        arrived,
    ));

    scene.fade(view.fade);
    scene
}

/// Draw a game starting: its own picture left exactly where it is, its title
/// coming forward in the middle of it, and the ring down in a corner.
///
/// No panel, which is the whole of the difference. A game chosen in the
/// library already has Steam's own picture of it standing behind the entire
/// display — the shell fades it in as the cursor arrives on the row — and
/// growing an opaque panel over that would be the shell throwing the picture
/// away at the exact moment it stopped being decoration and became the thing
/// somebody is waiting in front of. So the bar goes, the picture stays, and
/// the press is answered by the game's own title in its own lettering.
///
/// The title is Valve's `logo.png` where there is one. Where there is not —
/// an old title, a tool, a game whose logo has not come down the wire yet —
/// the name is written in its place, at the same size and with the same move,
/// because a splash with nothing in the middle of it is a picture with no
/// answer on it.
fn build_game_launch(view: LaunchView, width: f32, height: f32) -> Scene {
    let theme = theme();
    let scale = guide_scale(height);
    let mut scene = Scene::default();

    // The title holds back while the bar is still on screen and then comes
    // forward on its own, so the two never share the middle of the display.
    // Both are over the same picture, and a name crossing a category row on
    // its way in would read as part of the bar rather than as its answer.
    let arrived = ease(((view.open - 0.35) / 0.65).clamp(0.0, 1.0));
    let zoom = lerp(LAUNCH_LOGO_FROM, 1.0, arrived);
    let room = [width * LAUNCH_LOGO_WIDTH, height * LAUNCH_LOGO_HEIGHT];
    let middle = [width * 0.5, height * 0.5];

    match view
        .logo
        .filter(|logo| logo.aspect.is_finite() && logo.aspect > 0.0)
    {
        Some(logo) => {
            // Fitted into the room rather than filled into it: a wordmark is
            // the one picture in the shell that must not be cropped, since
            // what would be cropped is the end of the game's name.
            let (w, h) = if logo.aspect >= room[0] / room[1] {
                (room[0], room[0] / logo.aspect)
            } else {
                (room[1] * logo.aspect, room[1])
            };
            let (w, h) = (w * zoom, h * zoom);
            scene.quads.push(Quad {
                x: middle[0] - w * 0.5,
                y: middle[1] - h * 0.5,
                w,
                h,
                slot: logo.slot,
                // Drawn as it was made. A logo is artwork Valve drew to stand
                // on this very picture, so anything the shell tinted it with
                // would be the shell correcting an artist.
                color: [1.0, 1.0, 1.0, arrived],
                ..Quad::default()
            });
        }
        None => {
            // Wider than the logo's room, and deliberately: that box is
            // proportioned for a *picture* standing in the middle of a
            // display, and a game's name is a sentence. Held to the same
            // width would put an ellipsis in the middle of half the titles in
            // a library, which on the one thing the screen is saying is worse
            // than a line that runs a little wide.
            let size = LAUNCH_NAME * scale * zoom;
            let room = width * LAUNCH_NAME_WIDTH;
            scene.texts.push(Text {
                content: view.name.to_string(),
                x: middle[0] - room * 0.5,
                y: middle[1] - size * 0.62,
                size,
                color: theme.text.a(0.97 * arrived),
                bold: true,
                max_width: room,
                align: TextAlign::Center,
                clip: None,
                // Ringed in shade, unlike the name on an application's panel —
                // see [`TOAST_HALO`], which is here for the same reason. That
                // one is written on glass the shell chose; this one is written
                // on a photograph Valve chose, and a white title over the
                // bright half of a hero is not a title.
                //
                // Faded by hand, twice over: a ring is not part of a run's
                // colour, so neither the title arriving nor the whole splash
                // handing the screen over would take it — and a shadow left
                // standing over the game that has just appeared is the worst
                // outline the shell could leave behind.
                halo: LAUNCH_NAME_HALO * arrived * view.fade,
                lines: 1,
            });
        }
    }

    // And the ring, in the bottom-right corner, arriving with the title.
    let inset = LAUNCH_RING_INSET * scale;
    let ring = [width - inset, height - inset];
    let orbit = LAUNCH_RING_ORBIT * scale;

    // On a shade of its own, because the corner of a hero is whatever Valve
    // painted there. The dots are pale, and half the pictures in a library
    // have a bright sky in the corner they land on — a ring that cannot be
    // seen is worse than none, since the shell has then said nothing about
    // the minutes it is asking somebody to wait.
    //
    // A bloom rather than a panel: it is the [`GLOW_SLOT`] this shell already
    // lights its cursor with, in the glass's own near-black, and it has no
    // edge anywhere. A pane in the corner would be furniture stamped on
    // somebody's artwork; this is the artwork itself, a little further away.
    let shade = (orbit + 9.0 * scale) * LAUNCH_RING_SHADE;
    scene.quads.push(Quad {
        x: ring[0] - shade * 0.5,
        y: ring[1] - shade * 0.5,
        w: shade,
        h: shade,
        slot: GLOW_SLOT,
        color: theme.glass.a(0.55 * arrived),
        ..Quad::default()
    });
    scene.quads.extend(launch_ring(
        ring,
        orbit,
        9.0 * scale,
        view.time,
        view.waiting,
        arrived,
    ));

    // And what it is waiting for, in words, beside it.
    //
    // Beside rather than under, because the ring is already as far into the
    // corner as it can stand and a line under it would be off the display.
    // Right up against it and running back towards the middle of the screen:
    // the pair reads as one thing that way round, and a line of writing
    // stranded in the middle of the bottom edge would read as a caption on the
    // picture instead.
    if let Some(doing) = view.doing {
        let size = LAUNCH_DOING * scale;
        let room = width * LAUNCH_DOING_WIDTH;
        let right = ring[0] - orbit - 9.0 * scale - LAUNCH_DOING_GAP * scale;
        scene.texts.push(Text {
            content: launch_caption(doing).to_string(),
            x: right - room,
            // Centred on the ring rather than sharing its baseline: what it is
            // beside is a circle, and a circle has no baseline to share.
            y: ring[1] - size * 0.62,
            size,
            color: theme.text.a(0.9 * arrived),
            bold: false,
            max_width: room,
            align: TextAlign::Right,
            clip: None,
            // Its own shade, for the reason the fallback title has one: the
            // ring got a bloom it can sit in and this runs out past the edge
            // of that, onto whatever Valve painted along the bottom.
            halo: LAUNCH_NAME_HALO * arrived * view.fade,
            lines: 1,
        });
    }

    scene.fade(view.fade);

    // The dip to black, over everything above and over the picture under it.
    //
    // Added after the fade rather than before it because it is the one thing
    // here that is not fading *with* the splash — it is what the splash is
    // fading into, and it outlives everything else on the screen by the length
    // of the hold and the way up. See [`crate::launch::Launch::blackout`].
    //
    // It does not cover the text runs above it, and cannot: every quad in a
    // scene is drawn before every run — see [`Scene::hide_text_behind`]. That
    // is why the title and the line under the ring are timed to be gone by the
    // moment the screen is fully black rather than merely being stood over.
    let black = ease(view.blackout);
    if black > 0.0 {
        scene.quads.push(Quad {
            x: 0.0,
            y: 0.0,
            w: width,
            h: height,
            slot: SOLID_SLOT,
            color: [0.0, 0.0, 0.0, black],
            ..Quad::default()
        });
    }

    scene
}

/// What a game's splash says it is waiting for, beside the indicator.
///
/// Named steps rather than a spinner alone, and this is a reversal: the shell
/// used to say nothing here on the reasoning that "starting Steam" is its own
/// plumbing and not something anybody asked about. That holds for a wait of a
/// second or two. It does not hold for this one — a cold client is the better
/// part of a minute before the game is so much as asked for, and a spinner
/// alone over a picture for that long does not read as *working*, it reads as
/// stuck. Naming the step is what tells somebody the wait is going somewhere.
///
/// Neither line names the game. The whole middle of the display is the game's
/// own title in its own lettering, and repeating it in grey along the bottom
/// would be the shell saying the one thing already said largest.
fn launch_caption(doing: crate::launch::Doing) -> &'static str {
    match doing {
        crate::launch::Doing::Steam => "Launching Steam",
        crate::launch::Doing::Game => "Starting the game",
    }
}

/// A ring of dots lighting in turn, centred at `at`.
///
/// Not a bar: nothing here knows how far along the application is, and a bar
/// that does not measure anything is a lie about how much longer this will
/// take.
fn launch_ring(
    at: [f32; 2],
    orbit: f32,
    dot: f32,
    time: f32,
    waiting: bool,
    alpha: f32,
) -> Vec<Quad> {
    let theme = theme();
    let phase = time / LAUNCH_SPIN;
    (0..LAUNCH_DOTS)
        .map(|index| {
            let turn = index as f32 / LAUNCH_DOTS as f32;
            let angle = turn * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
            // Each dot is brightest as the sweep passes it and dims behind it.
            let behind = (phase - turn).rem_euclid(1.0);
            let lit = if waiting {
                0.2 + 0.8 * (1.0 - behind).powi(3)
            } else {
                // Nothing left to wait for: the ring settles rather than
                // spinning on over an application that is already there.
                0.2
            };
            Quad {
                x: at[0] + angle.cos() * orbit - dot * 0.5,
                y: at[1] + angle.sin() * orbit - dot * 0.5,
                w: dot,
                h: dot,
                slot: SOLID_SLOT,
                color: theme.accent_soft.a(lit * alpha),
                radius: dot * 0.5,
                ..Quad::default()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::{App, Category, Choice, Folder};
    use crate::menu::Title;
    use crate::model::Action;
    use std::path::{Path, PathBuf};

    /// A round hole shows the middle of a picture, and the middle of a square
    /// one is all of it. What must never happen is the crop reaching outside
    /// the picture, which would sample whatever of the atlas is next door.
    #[test]
    fn a_round_preview_takes_the_middle_of_a_picture() {
        assert_eq!(
            round_crop(1.0),
            [0.0, 0.0, 1.0, 1.0],
            "a square is all of it"
        );

        let wide = round_crop(2.0);
        assert_eq!(wide, [0.25, 0.0, 0.75, 1.0], "half the width of a 2:1");
        let tall = round_crop(0.5);
        assert_eq!(tall, [0.0, 0.25, 1.0, 0.75], "and half the height of a 1:2");

        // Centred in both, whatever the shape: a portrait cropped from the top
        // would be a column of foreheads.
        for aspect in [0.2, 0.9, 1.6, 4.0] {
            let [x0, y0, x1, y1] = round_crop(aspect);
            assert!(
                (x0 + x1 - 1.0).abs() < 1e-6,
                "{aspect} is off-centre across"
            );
            assert!((y0 + y1 - 1.0).abs() < 1e-6, "{aspect} is off-centre down");
            assert!(
                (0.0..=1.0).contains(&x0) && (0.0..=1.0).contains(&y1),
                "{aspect} reaches outside its own picture"
            );
            // And what is kept is square: the same fraction of the picture's
            // width and height, once the shape is taken into account.
            let kept = ((x1 - x0) * aspect, y1 - y0);
            assert!(
                (kept.0 - kept.1).abs() < 1e-6,
                "{aspect} keeps {kept:?}, which is not a square"
            );
        }
    }

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
                icons::SETTING_APPEARANCE => 23,
                icons::SETTING_ACCENT => 24,
                icons::SWATCH => 25,
                icons::CHOSEN => 26,
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

    /// A row that launches something — the ordinary contents of a column.
    fn app(name: &str) -> Entry {
        Entry::App(App {
            name: name.into(),
            comment: Some("does a thing".into()),
            icon: Some("icon".into()),
            exec: "true".into(),
            terminal: false,
            categories: Vec::new(),
            mime_types: Vec::new(),
            path: PathBuf::from("/tmp/x.desktop"),
            wm_class: None,
        })
    }

    /// A row that stands for one of the user's own pictures.
    fn picture(path: &str) -> Entry {
        Entry::Media(std::sync::Arc::new(
            crate::media::File::at(Path::new(path)).expect("a listable file"),
        ))
    }

    /// Slots for everything, and a picture of a given shape for every file —
    /// so a test can see what the layout does with one it has and one it has
    /// not yet been given.
    struct Pictures(f32);
    impl SlotLookup for Pictures {
        fn slot_for(&self, _icon: Option<&str>) -> Option<u32> {
            Some(7)
        }
        fn thumbnail(&self, _path: &Path) -> Option<crate::gpu::Thumb> {
            Some(crate::gpu::Thumb {
                slot: THUMB_SLOT,
                aspect: self.0,
                // Not the layout's business — it places the card and fits the
                // picture from the aspect, and how much of an atlas block the
                // picture happens to occupy is the renderer's problem.
                covers: [1.0, 1.0],
            })
        }
    }

    /// The same for a Steam library: every game has its cover, so what a row
    /// draws can be told from what it would have drawn without one.
    struct Covers;
    impl SlotLookup for Covers {
        fn slot_for(&self, _icon: Option<&str>) -> Option<u32> {
            Some(7)
        }
        fn cover(&self, _app_id: u32) -> Option<crate::gpu::Thumb> {
            Some(crate::gpu::Thumb {
                slot: THUMB_SLOT,
                aspect: COVER_ASPECT,
                covers: [1.0, 1.0],
            })
        }
    }

    /// A slot no icon uses, so a quad drawn with it is a thumbnail and nothing
    /// else.
    const THUMB_SLOT: u32 = 4242;

    /// One title in a Steam library, as the shell's own Steam column holds it.
    fn game(app_id: u32, name: &str) -> Entry {
        owned(app_id, name, true)
    }

    /// The same, saying whether it is on this disk.
    fn owned(app_id: u32, name: &str, installed: bool) -> Entry {
        Entry::Game(crate::apps::Game {
            app_id,
            name: name.to_string(),
            note: if installed {
                "Installed"
            } else {
                "Not installed"
            }
            .to_string(),
            installed,
            updating: false,
            steam_client: true,
        })
    }

    /// A library on the bar, in the column of its own the shell hangs it in.
    fn library(games: &[(u32, &str)]) -> Xmb {
        shelved_games(games.iter().map(|(id, name)| game(*id, name)).collect())
    }

    /// The same, when the test cares which of them are on the disk.
    fn part_owned(games: &[(u32, &str, bool)]) -> Xmb {
        shelved_games(
            games
                .iter()
                .map(|(id, name, installed)| owned(*id, name, *installed))
                .collect(),
        )
    }

    fn shelved_games(entries: Vec<Entry>) -> Xmb {
        Xmb::new(vec![Category {
            id: "graphics",
            title: "Graphics",
            icon: "g",
            entries: vec![folder("Steam", entries)],
        }])
    }

    /// The answer a point on an ordinary row gives: that row, and no value —
    /// which is every row in the tree but a bar's track, the one place a press
    /// says something with where it landed.
    fn row_hit(row: usize) -> Option<BarSpot> {
        Some(BarSpot::Item { row, level: None })
    }

    /// A row that opens a column of its own.
    fn folder(title: &str, entries: Vec<Entry>) -> Entry {
        Entry::Folder(Folder {
            title: title.into(),
            comment: None,
            icon: Some("folder".into()),
            entries,
            place: None,
        })
    }

    /// A column that is a value on a scale rather than a list of them.
    ///
    /// Its steps are a colour temperature's, because that is the one bar the
    /// tree has: a test that presses the track is then pressing what the shell
    /// draws, and gets back the setting the shell would apply.
    fn bar(fill: f32) -> Entry {
        Entry::Bar(crate::apps::Bar {
            title: "4000 K".into(),
            comment: None,
            fill,
            swatch: Some(crate::theme::Color(0xFFB46B)),
            up: None,
            down: None,
            steps: (2000..=6500)
                .step_by(100)
                .map(|kelvin| crate::settings::Setting::Display {
                    display: "TEST-OUT-1",
                    value: crate::settings::DisplayValue::NightLightTemperature(kelvin),
                })
                .collect(),
        })
    }

    /// A row that is one of a set of values, `chosen` if it is the one the
    /// shell is set to.
    fn choice(title: &str, chosen: bool) -> Entry {
        Entry::Choice(Choice {
            title: title.into(),
            comment: None,
            icon: Some(icons::SWATCH.into()),
            swatch: Some(crate::theme::Color(0x8B5CF6)),
            chosen,
            setting: None,
        })
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
        build(xmb, cursor, width, height, focused, None, 0.0, slots, false)
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
            entries: vec![app("first")],
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
            entries: vec![app("first"), app("second")],
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
                entries: vec![app("first"), app("second")],
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

    /// And a game grows out of its cover, which is a different rectangle
    /// entirely.
    ///
    /// The one that matters most, because it is the one the *window itself*
    /// flies out of: a game already running is not loaded again, it is brought
    /// back — see `lxb_shell_v1.activate_window_from` — and what the user
    /// pressed to get it is a tall picture, not the small disc every other row
    /// stands on. Out of the disc it would grow from a square hole in the
    /// middle of the cover, which reads as a window arriving from nowhere.
    #[test]
    fn a_game_opens_out_of_the_cover_it_was_chosen_from() {
        for (width, height) in [(1280.0, 800.0), (1920.0, 1080.0), (3840.0, 2160.0)] {
            let xmb = library(&[(504230, "Celeste"), (367520, "Hollow Knight")]);
            let scene = opened(&xmb, width, height, &Covers);
            let tile = cover_origin(width, height);

            // The chosen game's own cover, which is the tallest picture drawn:
            // the card under it is what `cover_origin` describes, and the
            // picture stands inside that card's mount.
            let cover = scene
                .quads
                .iter()
                .filter(|quad| quad.slot == THUMB_SLOT)
                .max_by(|a, b| a.h.total_cmp(&b.h))
                .expect("a library draws a cover on every row");
            let mount = Cards::of(COVER_ASPECT).mount(tile[3]);
            for (name, drawn, want) in [
                ("left", cover.x, tile[0] + mount),
                ("top", cover.y, tile[1] + mount),
                ("width", cover.w, tile[2] - mount * 2.0),
                ("height", cover.h, tile[3] - mount * 2.0),
            ] {
                assert!(
                    (drawn - want).abs() < 0.5,
                    "{width}x{height}: the cover's {name} is {drawn}, not {want}"
                );
            }

            // And it is the cover it grows out of rather than the disc, which
            // is neither where it stands nor the shape it is.
            let disc = launch_origin(width, height);
            assert!(
                tile[3] > disc[3] * 1.5 && tile[2] < tile[3],
                "{width}x{height}: a portrait capsule, not a disc: {tile:?}"
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
                    game: false,
                    logo: None,
                    doing: None,
                    blackout: 0.0,
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

    /// A game is answered on the picture it is already standing on. Nothing
    /// covers the display, the title grows into the middle of it, and the ring
    /// waits down in the bottom-right corner.
    ///
    /// The panel is what is being pinned out, not merely something that has
    /// gone: a single opaque quad the size of the screen is the one thing that
    /// would throw away the hero the shell spent the browse fetching, and it
    /// is invisible to every other test here because a splash covering
    /// everything looks perfectly deliberate.
    #[test]
    fn a_game_opens_on_its_own_picture_with_no_panel_over_it() {
        let (width, height) = (1920.0, 1080.0);
        let tile = launch_origin(width, height);
        // A one-line wordmark, which is the shape that tests the fitting: it
        // is far wider than the room is proportioned, so the width binds.
        let logo = crate::gpu::Thumb {
            slot: 4242,
            aspect: 640.0 / 113.0,
            covers: [1.0, 1.0],
        };
        let at = |open: f32, fade: f32, logo| {
            build_launch(
                LaunchView {
                    name: "Hollow Knight",
                    icon: Some(7),
                    game: true,
                    logo,
                    doing: Some(crate::launch::Doing::Steam),
                    blackout: 0.0,
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

        let scene = at(1.0, 1.0, Some(logo));
        assert!(
            !scene
                .quads
                .iter()
                .any(|quad| quad.w >= width * 0.9 && quad.h >= height * 0.9),
            "something is covering the game's own picture"
        );
        // The only writing on it is the line beside the indicator. The middle
        // of the display is the logo, and a name written under artwork that is
        // the name would be the shell saying it twice.
        assert_eq!(
            scene
                .texts
                .iter()
                .map(|text| text.content.as_str())
                .collect::<Vec<_>>(),
            vec!["Launching Steam"]
        );

        // The logo, settled: fitted into its room at its own shape, centred.
        let mark = scene
            .quads
            .iter()
            .find(|quad| quad.slot == logo.slot)
            .expect("the game's logo");
        assert!((mark.w - width * LAUNCH_LOGO_WIDTH).abs() < 0.5, "{mark:?}");
        assert!((mark.w / mark.h - logo.aspect).abs() < 0.01, "{mark:?}");
        assert!(
            (mark.x + mark.w * 0.5 - width * 0.5).abs() < 0.5,
            "{mark:?}"
        );
        assert!(
            (mark.y + mark.h * 0.5 - height * 0.5).abs() < 0.5,
            "{mark:?}"
        );

        // And it got there by growing, from the middle rather than from the
        // tile: a logo is not what the row it was chosen from was showing.
        let opening = at(0.6, 1.0, Some(logo));
        let growing = opening
            .quads
            .iter()
            .find(|quad| quad.slot == logo.slot)
            .expect("the game's logo");
        assert!(growing.w < mark.w, "the logo does not grow");
        assert!(growing.w > mark.w * LAUNCH_LOGO_FROM * 0.99);
        assert!(growing.color[3] < 1.0, "it does not fade in");
        assert!(
            (growing.x + growing.w * 0.5 - width * 0.5).abs() < 0.5,
            "it grew from somewhere other than the middle: {growing:?}"
        );

        // The ring is in the bottom-right corner, clear of both edges — not
        // round the title, which on a game is standing on somebody's artwork.
        let dots: Vec<&Quad> = scene
            .quads
            .iter()
            .filter(|quad| quad.slot == SOLID_SLOT)
            .collect();
        assert_eq!(dots.len(), LAUNCH_DOTS);
        let far = |pick: fn(&Quad) -> f32| dots.iter().map(|dot| pick(dot)).fold(0.0, f32::max);
        assert!(
            far(|dot| dot.x + dot.w) < width && far(|dot| dot.x) > width * 0.9,
            "the ring is not in the right of the display"
        );
        assert!(
            far(|dot| dot.y + dot.h) < height && far(|dot| dot.y) > height * 0.9,
            "the ring is not at the foot of the display"
        );

        // And the line saying what is being waited for sits beside the ring,
        // clear of it and running back towards the middle rather than off the
        // edge of the display.
        let said = scene.texts.first().expect("the line beside the indicator");
        let ring_left = far(|dot| dot.x);
        assert!(
            said.x + said.max_width < ring_left,
            "the writing runs into the ring"
        );
        assert!(said.x > 0.0, "the writing runs off the left of the display");
        let middle_of_dots = (far(|dot| dot.y + dot.h)
            + dots.iter().map(|dot| dot.y).fold(f32::MAX, f32::min))
            * 0.5;
        assert!(
            (said.y + said.size * 0.62 - middle_of_dots).abs() < said.size * 0.3,
            "the writing is not level with the ring"
        );

        // Nothing outlasts the hand-over: the game's own window is behind all
        // of this, and a title left over it belongs to the shell, not the game.
        assert!(at(1.0, 0.0, Some(logo))
            .quads
            .iter()
            .all(|quad| quad.fade <= 0.001));
    }

    /// The line beside the indicator names the step, and changes when the step
    /// does. A loading screen still saying "Steam" a minute into a shader
    /// cache is one lying about what it is waiting for.
    #[test]
    fn the_line_beside_the_indicator_says_which_step_this_is() {
        let (width, height) = (1920.0, 1080.0);
        let said = |doing| {
            build_launch(
                LaunchView {
                    name: "Hollow Knight",
                    icon: Some(7),
                    game: true,
                    logo: Some(crate::gpu::Thumb {
                        slot: 4242,
                        aspect: 2.0,
                        covers: [1.0, 1.0],
                    }),
                    doing,
                    blackout: 0.0,
                    from: launch_origin(width, height),
                    open: 1.0,
                    fade: 1.0,
                    waiting: true,
                    time: 0.0,
                },
                width,
                height,
            )
            .texts
            .first()
            .map(|text| text.content.clone())
        };

        assert_eq!(
            said(Some(crate::launch::Doing::Steam)).as_deref(),
            Some("Launching Steam")
        );
        assert_eq!(
            said(Some(crate::launch::Doing::Game)).as_deref(),
            Some("Starting the game")
        );
        // Neither repeats the title, which is already the whole middle of the
        // display in the game's own lettering.
        for doing in [crate::launch::Doing::Steam, crate::launch::Doing::Game] {
            assert!(!said(Some(doing)).expect("a line").contains("Hollow Knight"));
        }
        assert_eq!(said(None), None, "nothing to say, so nothing written");
    }

    /// The screen dips through black on its way to the game, and the dip is a
    /// sheet over the whole display — not a fade of the splash, which would
    /// show the game through the gaps as it went.
    ///
    /// The sheet also has to survive the splash's own fade. It is what the
    /// splash fades *into*, and it outlives everything else on screen by the
    /// length of the hold and the way up; a black added before the fade would
    /// be taken out by it and the screen would cut straight to the game.
    #[test]
    fn a_game_is_reached_through_a_black_screen() {
        let (width, height) = (1920.0, 1080.0);
        let at = |fade: f32, blackout: f32| {
            build_launch(
                LaunchView {
                    name: "Hollow Knight",
                    icon: Some(7),
                    game: true,
                    logo: None,
                    doing: Some(crate::launch::Doing::Game),
                    blackout,
                    from: launch_origin(width, height),
                    open: 1.0,
                    fade,
                    waiting: false,
                    time: 0.0,
                },
                width,
                height,
            )
        };
        let sheet = |scene: &Scene| {
            scene
                .quads
                .iter()
                .find(|quad| quad.w >= width && quad.h >= height)
                .map(|quad| (quad.color, quad.fade))
        };

        // Nothing while the splash is simply waiting: the picture behind it is
        // the whole point of the splash.
        assert_eq!(sheet(&at(1.0, 0.0)), None);

        // Coming on, over everything.
        let (colour, faded) = sheet(&at(0.5, 0.5)).expect("the black");
        assert_eq!([colour[0], colour[1], colour[2]], [0.0, 0.0, 0.0]);
        assert!(colour[3] > 0.0 && colour[3] < 1.0, "{colour:?}");
        assert_eq!(faded, 1.0, "the splash's own fade took the black with it");

        // Full black: nothing of the shell's is still being drawn on it, and
        // the sheet is solid.
        let black = at(0.0, 1.0);
        assert_eq!(sheet(&black).expect("the black").0[3], 1.0);
        assert!(
            black.texts.iter().all(|text| text.color[3] <= 0.001),
            "writing left standing on a black screen"
        );

        // And coming off again with the splash long gone, which is the half
        // that shows the game.
        let (colour, faded) = sheet(&at(0.0, 0.4)).expect("the black");
        assert!(colour[3] > 0.0 && colour[3] < 1.0, "{colour:?}");
        assert_eq!(faded, 1.0);
    }

    /// Not every game has a logo — an old title, a tool, a demo, or one whose
    /// artwork has not come down the wire yet — and a splash with nothing in
    /// the middle of it is a picture with no answer on it. So the name is
    /// written where the logo would have been, and moves as the logo would.
    #[test]
    fn a_game_with_no_logo_is_still_named() {
        let (width, height) = (1920.0, 1080.0);
        let at = |open: f32| {
            build_launch(
                LaunchView {
                    name: "Hollow Knight",
                    icon: Some(7),
                    game: true,
                    logo: None,
                    doing: Some(crate::launch::Doing::Game),
                    blackout: 0.0,
                    from: launch_origin(width, height),
                    open,
                    fade: 1.0,
                    waiting: true,
                    time: 0.0,
                },
                width,
                height,
            )
        };
        let title = |scene: &Scene| {
            scene
                .texts
                .iter()
                .find(|text| text.content == "Hollow Knight")
                .cloned()
                .expect("the game's name")
        };

        let settled = title(&at(1.0));
        assert!(settled.color[3] > 0.9);
        assert!(settled.halo > 0.0, "a name over a hero needs its own edge");
        assert!(
            (settled.x + settled.max_width * 0.5 - width * 0.5).abs() < 0.5,
            "the name is not centred: {} wide at {}",
            settled.max_width,
            settled.x
        );

        // The same move the logo makes, so the two cases are one animation.
        let growing = title(&at(0.6));
        assert!(growing.size < settled.size);
        assert!(growing.color[3] < settled.color[3]);
        assert!(
            growing.halo < settled.halo,
            "the shadow arrived before the letters did"
        );
    }

    /// A ring is not part of a run's colour, so nothing that fades a scene
    /// touches it — which means the one thing on this splash that could
    /// outlive the hand-over is the shadow under the name. A black outline of
    /// a title left standing over the game that has just appeared is the worst
    /// thing the shell could leave on somebody's screen.
    #[test]
    fn a_games_name_takes_its_shadow_with_it() {
        let (width, height) = (1920.0, 1080.0);
        let scene = build_launch(
            LaunchView {
                name: "Hollow Knight",
                icon: Some(7),
                game: true,
                logo: None,
                doing: Some(crate::launch::Doing::Game),
                blackout: 0.0,
                from: launch_origin(width, height),
                open: 1.0,
                fade: 0.0,
                waiting: false,
                time: 0.0,
            },
            width,
            height,
        );
        for text in &scene.texts {
            assert_eq!(text.color[3], 0.0, "a title outlasted the handover");
            assert_eq!(text.halo, 0.0, "its shadow outlasted the handover");
        }
        assert!(scene.quads.iter().all(|quad| quad.fade <= 0.001));
    }

    #[test]
    fn empty_model_still_renders_a_message() {
        let xmb = Xmb::new(Vec::new());
        let scene = focused(&xmb, 1920.0, 1080.0, &NoSlots);
        assert!(scene.quads.is_empty());
        assert_eq!(scene.texts.len(), 1);
    }

    /// A catalogue with nothing to launch in it still has the shell's own
    /// column, and that column has to be reachable: the machine with no
    /// applications on it is exactly the one whose settings are wanted.
    #[test]
    fn a_catalogue_with_nothing_to_launch_still_draws_the_shells_own_column() {
        let xmb = Xmb::new(vec![Category {
            id: "settings",
            title: "Settings",
            icon: "settings",
            entries: vec![folder("Appearance", vec![choice("Purple", true)])],
        }]);
        assert!(xmb.is_empty(), "nothing here starts a process");

        let scene = focused(&xmb, 1920.0, 1080.0, &AllSlots);
        assert!(scene.texts.iter().any(|text| text.content == "Appearance"));
        assert!(scene.texts.iter().any(|text| text.content == "Settings"));

        // And the note says so, out of the bar's way rather than in place of it.
        let note = scene
            .texts
            .iter()
            .find(|text| text.content == "No applications found")
            .expect("an empty scan is still worth saying");
        assert!(note.y > 1080.0 * 0.6);
    }

    /// Two categories with rows in them, for the pointing tests: the shape of
    /// the cross needs an arm in each direction to be aimed at.
    fn crossed() -> Xmb {
        Xmb::new(vec![
            Category {
                id: "play",
                title: "Play",
                icon: "play",
                entries: vec![app("one"), app("two"), app("three")],
            },
            Category {
                id: "settings",
                title: "Settings",
                icon: "settings",
                entries: vec![app("only")],
            },
        ])
    }

    /// The hit test and the layout are one answer read two ways, the same way
    /// the on-screen keyboard's are: the row under a point has to be the row
    /// drawn at that point, or a click launches something other than what it
    /// was aimed at.
    #[test]
    fn the_row_under_the_pointer_is_the_row_drawn_there() {
        let xmb = crossed();
        let (width, height) = (1920.0, 1080.0);
        let cursor = Cursor::new(xmb.categories.len());
        let scene = build_with(&xmb, &cursor, width, height, true, &AllSlots);

        // The icons of the open column, top to bottom. Nothing is scrolled, so
        // they are its rows in order — the selection is the first of them.
        let column_x = width * BAR_CROSS_X;
        let mut rows: Vec<[f32; 2]> = scene
            .quads
            .iter()
            .filter(|quad| is_icon(quad) && (quad.x + quad.w * 0.5 - column_x).abs() < 1.0)
            .map(|quad| [quad.x + quad.w * 0.5, quad.y + quad.h * 0.5])
            .filter(|[_, y]| *y > height * BAR_CROSS_Y)
            .collect();
        rows.sort_by(|a, b| a[1].total_cmp(&b[1]));
        assert_eq!(rows.len(), 3, "every row of the column is drawn");

        for (index, [x, y]) in rows.iter().enumerate() {
            assert_eq!(
                bar_hit(&xmb, &cursor, *x, *y, width, height),
                row_hit(index),
                "the icon drawn at ({x}, {y}) is row {index}"
            );
        }

        // And the row the selection is on is the one at the cross, which is
        // where the launch splash grows out of.
        let [ox, oy, ow, oh] = launch_origin(width, height);
        assert_eq!(
            bar_hit(&xmb, &cursor, ox + ow * 0.5, oy + oh * 0.5, width, height),
            row_hit(0)
        );
    }

    /// The two arms of the cross do not answer for one another: the category
    /// row is drawn over the columns, and the gap it sits in is wide enough
    /// that neither reaches the other.
    #[test]
    fn the_category_row_takes_the_line_it_is_drawn_on() {
        let xmb = crossed();
        let (width, height) = (1920.0, 1080.0);
        let cursor = Cursor::new(xmb.categories.len());
        let (cross_x, cross_y) = (width * BAR_CROSS_X, height * BAR_CROSS_Y);
        let spacing = CATEGORY_SPACING * guide_scale(height);

        assert_eq!(
            bar_hit(&xmb, &cursor, cross_x, cross_y, width, height),
            Some(BarSpot::Category(0))
        );
        assert_eq!(
            bar_hit(&xmb, &cursor, cross_x + spacing, cross_y, width, height),
            Some(BarSpot::Category(1)),
            "the next button along the row"
        );
        // Past the last category there is no button, and nothing invented.
        assert_eq!(
            bar_hit(
                &xmb,
                &cursor,
                cross_x + spacing * 3.0,
                cross_y,
                width,
                height
            ),
            None
        );
    }

    /// A row of the trail is a column the path was opened through, and pointing
    /// at it is asking to walk back out to it — which is what the user can see
    /// it is, because it is the only row of that column still drawn.
    #[test]
    fn pointing_at_the_trail_is_asking_to_step_back_out() {
        let xmb = nested();
        let (width, height) = (1920.0, 1080.0);
        let cursor = stepped(&xmb);

        // The trail's row and the open column's row sit on the same line; the
        // trail is the one further left.
        let icons = trail_icons(
            &build_with(&xmb, &cursor, width, height, true, &AllSlots),
            height,
        );
        let line = launch_origin(width, height);
        let middle = line[1] + line[3] * 0.5;
        assert_eq!(
            bar_hit(&xmb, &cursor, icons[0][0], middle, width, height),
            Some(BarSpot::Trail(1))
        );
        assert_eq!(
            bar_hit(&xmb, &cursor, icons[1][0], middle, width, height),
            row_hit(0),
            "and the column in front of it is still being browsed"
        );

        // The category button at the head of the trail is the same journey,
        // all the way out.
        let category_x = bar_category_x(0.0, cursor.depth_position(), width, height);
        assert_eq!(
            bar_hit(
                &xmb,
                &cursor,
                category_x,
                height * BAR_CROSS_Y,
                width,
                height
            ),
            Some(BarSpot::Trail(1))
        );
    }

    /// A column with a path through it: one plain row, then a subcategory two
    /// levels down whose innermost column is a list of values. Every column
    /// along it has a second row, so a test can tell a column that is keeping
    /// only the row it was opened from apart from one that has nothing else.
    fn nested() -> Xmb {
        Xmb::new(vec![Category {
            id: "settings",
            title: "Settings",
            icon: "settings",
            entries: vec![
                app("plain"),
                folder(
                    "Appearance",
                    vec![
                        folder(
                            "Accent color",
                            vec![choice("Green", false), choice("Purple", true)],
                        ),
                        // A subcategory rather than a value, so a swatch on
                        // screen means the list of colours is open and nothing
                        // else.
                        folder("Wallpaper", vec![app("a picture")]),
                    ],
                ),
            ],
        }])
    }

    /// Stand the cursor one subcategory in, settled: the whole shape of a path
    /// is on screen there — the category, the row it was opened from, and the
    /// column that opened.
    fn stepped(xmb: &Xmb) -> Cursor {
        let mut cursor = Cursor::new(1);
        cursor.navigate(Action::Down, xmb);
        assert!(cursor.enter(xmb));
        settle(&mut cursor);
        cursor
    }

    /// And at the end of the path, settled.
    fn walked(xmb: &Xmb) -> Cursor {
        let mut cursor = stepped(xmb);
        assert!(cursor.navigate(Action::Right, xmb));
        settle(&mut cursor);
        cursor
    }

    /// The centre of every icon on the row the cursor sits on, left to right.
    fn trail_icons(scene: &Scene, height: f32) -> Vec<[f32; 3]> {
        let row = launch_origin(1.0, height);
        let centre = row[1] + row[3] / 2.0;
        let mut icons: Vec<[f32; 3]> = scene
            .quads
            .iter()
            .filter(|quad| is_icon(quad) && ((quad.y + quad.h / 2.0) - centre).abs() < 1.0)
            .map(|quad| [quad.x + quad.w / 2.0, quad.w, quad.color[3]])
            .collect();
        icons.sort_by(|a, b| a[0].total_cmp(&b[0]));
        icons
    }

    /// The shape of the thing: category above, and the path taken reading left
    /// to right beneath it on one line — which is what a trail is for, and the
    /// only arrangement in which the row a column was opened from is still
    /// next to the column it opened.
    #[test]
    fn a_path_reads_left_to_right_under_the_category_it_hangs_off() {
        let xmb = nested();
        let cursor = stepped(&xmb);
        let (width, height) = (1920.0, 1080.0);
        let scene = build_with(&xmb, &cursor, width, height, true, &AllSlots);

        let icons = trail_icons(&scene, height);
        assert_eq!(icons.len(), 2, "the open column, and the one behind it");
        let [behind, open] = [icons[0][0], icons[1][0]];

        // One step apart, with the open column on the cross and the one it was
        // opened from standing where the slide left it.
        let step = subcolumn_step(height);
        assert!((open - behind - step).abs() < 0.5);
        assert!((open - width * BAR_CROSS_X).abs() < 0.5);
        // And nothing has run off the edge to get there.
        assert!(
            behind - icons[0][1] / 2.0 > 0.0,
            "the trail starts on screen"
        );

        // The category the path hangs off stands over the head of it.
        let category = scene
            .quads
            .iter()
            .filter(|quad| is_icon(quad))
            .find(|quad| ((quad.y + quad.h / 2.0) - height * BAR_CROSS_Y).abs() < 1.0)
            .expect("the category row is still drawn");
        assert!((category.x + category.w / 2.0 - behind).abs() < 0.5);

        // Left to right, the labels of the path.
        let at = |name: &str| {
            scene
                .texts
                .iter()
                .find(|text| text.content == name)
                .unwrap_or_else(|| panic!("{name} should be on screen"))
                .x
        };
        assert!(at("Settings") < at("Appearance"));
        assert!(at("Appearance") < at("Accent color"));
    }

    /// A row hanging over the near edge is still a row. It is drawn where it
    /// stands, clipped by the screen like anything else, and only what has
    /// genuinely gone past is taken away.
    ///
    /// And whatever is taken is taken from the whole of it. The bug this
    /// guards against: one step in, on any display narrower than the 16:9 the
    /// layout's step is measured against, the category stands nearer the edge
    /// than the reference display ever puts it — and the fade that was meant
    /// for a column *leaving* had already emptied the cog while the word
    /// "Settings" underneath it was still at full strength. An icon that goes
    /// without its own name does not read as something leaving. It reads as a
    /// missing icon.
    #[test]
    fn a_row_at_the_edge_keeps_its_name_and_its_name_keeps_it() {
        let xmb = nested();
        let cursor = stepped(&xmb);

        for (width, height) in [
            (960.0, 600.0),   // 16:10, and small
            (1440.0, 1080.0), // 4:3: one step in puts the row past the edge
            (1920.0, 1200.0), // 16:10
            (1920.0, 1080.0), // the reference
            (2560.0, 1080.0), // wider than the step was measured for
        ] {
            let scene = build_with(&xmb, &cursor, width, height, true, &AllSlots);
            let category = scene
                .quads
                .iter()
                .filter(|quad| is_icon(quad))
                .find(|quad| ((quad.y + quad.h / 2.0) - height * BAR_CROSS_Y).abs() < 1.0)
                .unwrap_or_else(|| panic!("{width}x{height}: the category is still drawn"));
            let label = scene
                .texts
                .iter()
                .find(|text| text.content == "Settings")
                .unwrap_or_else(|| panic!("{width}x{height}: it still has its name"));

            // Whatever the edge has taken, it has taken from both.
            assert!(
                (category.color[3] - label.color[3]).abs() < 0.01,
                "{width}x{height}: cog at {:.2} under a name at {:.2}",
                category.color[3],
                label.color[3]
            );
            // And one step in is not far enough for the edge to have taken
            // anything worth speaking of: it is still a category on screen.
            assert!(
                category.color[3] > 0.4,
                "{width}x{height}: the category faded to {:.2} one step in",
                category.color[3]
            );
            // Some of it may well be over the side. That is ordinary — the
            // category row runs off both edges at the top of a column too.
            assert!(category.x + category.w > 0.0);
        }
    }

    /// A column opened from another takes that one's place, and everything
    /// behind moves along by one.
    ///
    /// What it buys is a bar that costs the same at any depth: the column being
    /// browsed is always on the cross and the one it came from always a step to
    /// the left of that, so a path four subcategories deep takes no more of the
    /// screen than a path one deep — the far end of it has gone off the side
    /// instead of the near end running off the other.
    #[test]
    fn a_column_takes_the_place_of_the_one_it_was_opened_from() {
        let xmb = nested();
        let (width, height) = (1920.0, 1080.0);
        let mut cursor = stepped(&xmb);

        let places = |cursor: &Cursor| {
            let scene = build_with(&xmb, cursor, width, height, true, &AllSlots);
            trail_icons(&scene, height)
                .iter()
                .map(|icon| icon[0])
                .collect::<Vec<f32>>()
        };
        let before = places(&cursor);
        assert_eq!(before.len(), 2);

        assert!(cursor.navigate(Action::Right, &xmb));
        settle(&mut cursor);
        let after = places(&cursor);

        assert_eq!(after.len(), 2, "still two columns on screen, not three");
        for (was, now) in before.iter().zip(&after) {
            assert!(
                (was - now).abs() < 0.5,
                "a step in should hand the same places on: {was} then {now}"
            );
        }
    }

    /// A column behind the open one keeps the row it was opened from and
    /// nothing else. Its neighbours are not context — they are a second list
    /// of choices beside the real one.
    #[test]
    fn a_column_behind_the_open_one_keeps_only_its_own_row() {
        let xmb = nested();
        let scene = build_with(&xmb, &stepped(&xmb), 1920.0, 1080.0, true, &AllSlots);

        let shown = |scene: &Scene, name: &str| scene.texts.iter().any(|text| text.content == name);
        assert!(shown(&scene, "Appearance"), "the row it was opened from");
        assert!(!shown(&scene, "plain"), "its neighbour is behind with it");
        assert!(shown(&scene, "Accent color"), "the open column, in full");
        assert!(shown(&scene, "Wallpaper"), "every row of it");

        // A step further in and the same is true one column along, while the
        // column that has now fallen two back is off the screen entirely.
        let scene = build_with(&xmb, &walked(&xmb), 1920.0, 1080.0, true, &AllSlots);
        assert!(shown(&scene, "Accent color"), "the row it was opened from");
        assert!(
            !shown(&scene, "Wallpaper"),
            "its neighbour is behind with it"
        );
        assert!(shown(&scene, "Green") && shown(&scene, "Purple"));
        assert!(!shown(&scene, "Appearance"), "two columns back has gone");
        assert!(!shown(&scene, "Settings"), "and so has the category");
    }

    /// A trail label has room to be a word.
    ///
    /// The regression this guards against, seen on a 960x600 display: the
    /// step between two columns was being narrowed to whatever room the bar
    /// had to slide left in, and on a wide, short display that is not much —
    /// the layout scales with height and the cross sits by width. The trail
    /// came out as "Appe / aranc" over "Acce / nt", which is a breadcrumb that
    /// cannot be read, which is a breadcrumb for nothing.
    #[test]
    fn a_trail_label_has_room_to_be_read_on_any_display() {
        let xmb = nested();
        // One step in and two: the row standing behind the open column is a
        // different one each time, and both have to be readable where they
        // stand.
        let paths = [
            (stepped(&xmb), "Appearance"),
            (walked(&xmb), "Accent color"),
        ];

        for (width, height) in [
            (960.0, 600.0),
            (1280.0, 720.0),
            (1920.0, 1080.0),
            (2560.0, 1080.0),
            (3840.0, 2160.0),
        ] {
            for (cursor, name) in &paths {
                let scene = build_with(&xmb, cursor, width, height, true, &AllSlots);
                let label = scene
                    .texts
                    .iter()
                    .find(|text| &text.content == name)
                    .unwrap_or_else(|| panic!("{width}x{height}: {name} should be drawn"));
                // Roboto averages around half an em per character, so a dozen
                // of them want six ems of box.
                assert!(
                    label.max_width > label.size * 6.0,
                    "{width}x{height}: {name} has {:.0}px for a {:.0}px font",
                    label.max_width,
                    label.size
                );
            }
        }
    }

    /// One lit row on screen, however deep the path runs. The glow is the
    /// answer to "what am I pointing at", and a trail that kept its own would
    /// be three answers to one question.
    #[test]
    fn only_the_open_column_is_lit() {
        let xmb = nested();
        let cursor = walked(&xmb);
        let scene = build_with(&xmb, &cursor, 1920.0, 1080.0, true, &AllSlots);

        let glows: Vec<&Quad> = scene
            .quads
            .iter()
            .filter(|quad| quad.slot == GLOW_SLOT && quad.color[3] > 0.01)
            .collect();
        assert_eq!(glows.len(), 1, "only the chosen row breathes");
        assert!(
            (glows[0].x + glows[0].w / 2.0 - 1920.0 * BAR_CROSS_X).abs() < 0.5,
            "and it is in the open column, which is always the one on the cross"
        );

        // The category button is unlit too: the light has gone down into the
        // column, and its icon stays only as the head of the trail.
        assert!(scene
            .quads
            .iter()
            .filter(|quad| quad.gloss > 0.0 && (quad.y + quad.h / 2.0 - 1080.0 * 0.30).abs() < 1.0)
            .all(|quad| quad.fade < 0.9));
    }

    /// The step in is one continuous move of the whole bar. Nothing may jump:
    /// not on the frame the button is pressed, not at the handover halfway
    /// through, and not on the frame it arrives.
    #[test]
    fn stepping_into_a_subcategory_never_jumps() {
        let xmb = nested();
        let (width, height) = (1920.0, 1080.0);
        let mut cursor = Cursor::new(1);
        cursor.navigate(Action::Down, &xmb);
        settle(&mut cursor);

        let head = |cursor: &Cursor| {
            let scene = build_with(&xmb, cursor, width, height, true, &AllSlots);
            // Where the row being opened from stands, and how big it is: the
            // trail's head is on screen for the whole move, so it is the one
            // mark that can be followed across the handover.
            trail_icons(&scene, height)[0]
        };

        let start = head(&cursor);
        assert!((start[0] - width * BAR_CROSS_X).abs() < 0.5);
        cursor.enter(&xmb);
        assert_eq!(
            head(&cursor)[0],
            start[0],
            "the press itself must move nothing"
        );

        let mut previous = start;
        let mut frames = 0;
        while cursor.animate(1.0 / 60.0) {
            let now = head(&cursor);
            assert!(now[0] < previous[0] + 0.001, "it must not double back");
            assert!(
                previous[0] - now[0] < subcolumn_step(height) * 0.2,
                "a frame moved it {} px",
                previous[0] - now[0]
            );
            assert!(
                (previous[1] - now[1]).abs() < ITEM_ICON_FOCUSED * 0.2,
                "and it may not resize in one frame either"
            );
            assert!(now[2] > 0.2, "nor blink out on the way");
            previous = now;
            frames += 1;
            assert!(frames < 600, "the step never settled");
        }

        // Arrived: one full step left of where it started, and shrunk to the
        // size of a row that is no longer the one being pointed at.
        assert!((start[0] - previous[0] - subcolumn_step(height)).abs() < 0.5);
        assert!(previous[1] < start[1]);
    }

    /// Each column behind the open one stands one step further back than the
    /// column in front of it, and the category row stands a step behind the
    /// outermost column of all — so two subcategories deep the row is three
    /// steps back, its column two, and the one opened from that one.
    ///
    /// Measured on the icons, the labels and the ink of both, because a
    /// recession that moved only one of them is not a recession: an icon that
    /// shrank under a label that did not has come away from its own name.
    #[test]
    fn the_trail_falls_away_a_step_at_a_time() {
        let xmb = nested();
        let (width, height) = (1920.0, 1080.0);
        let scene = build_with(&xmb, &stepped(&xmb), width, height, true, &AllSlots);

        // Three planes, front to back: the open column, the row it was opened
        // from, and the category row behind that.
        let icons = trail_icons(&scene, height);
        let [behind, open] = [icons[0], icons[1]];
        let category = scene
            .quads
            .iter()
            .filter(|quad| is_icon(quad))
            .find(|quad| ((quad.y + quad.h / 2.0) - height * BAR_CROSS_Y).abs() < 1.0)
            .expect("the category row is still drawn");

        // Each of them at the size its own distance gives it. The open
        // column's row is the focused one and the trail's is not, so between
        // those two the step rides on top of a size difference they would have
        // had anyway; between the trail and the category it is all there is.
        let (one_back, one_back_haze) = receded(1.0);
        let (two_back, two_back_haze) = receded(2.0);
        assert!((behind[1] - ITEM_ICON * one_back).abs() < 0.5);
        assert!(behind[1] < open[1]);
        assert!((category.w - CATEGORY_ICON_FOCUSED * two_back).abs() < 0.5);

        // And dimmer by the same ladder: both rows sit on the same line and
        // are the chosen row of their own column, so what is left between them
        // is the distance.
        assert!((behind[2] / open[2] - one_back_haze).abs() < 0.01);
        assert!(category.color[3] < behind[2], "the row behind them all");

        let title = |name: &str| {
            scene
                .texts
                .iter()
                .find(|text| text.content == name)
                .unwrap_or_else(|| panic!("{name} should be drawn"))
        };
        assert!((title("Settings").size - CATEGORY_LABEL * two_back).abs() < 0.5);
        assert!(title("Settings").color[3] < two_back_haze + 0.01);

        // The labels go back with the icons they belong to — in size, in ink,
        // and in how close they stand to their own icon. A label that kept its
        // distance from one half the size has come away from its own name.
        let (outer, inner) = (title("Appearance"), title("Accent color"));
        assert!((outer.size / (22.0 * one_back)).abs() - 1.0 < 0.01);
        assert!(outer.color[3] < inner.color[3]);
        assert!(((outer.x - behind[0]) / (inner.x - open[0]) - one_back).abs() < 0.01);

        // A step further in does not push the trail further back: what is one
        // column behind is always one step away, however long the path is.
        let deeper = build_with(&xmb, &walked(&xmb), width, height, true, &AllSlots);
        let deeper_icons = trail_icons(&deeper, height);
        assert!((deeper_icons[0][1] - behind[1]).abs() < 0.5);
        assert!((deeper_icons[0][2] - behind[2]).abs() < 0.01);
    }

    /// At the top of a column nothing has receded: the row the cursor is on is
    /// not behind anything, and a bar that opened already pushed back would be
    /// answering a question nobody had asked.
    #[test]
    fn the_top_of_a_column_stands_at_the_front() {
        let xmb = nested();
        let (width, height) = (1920.0, 1080.0);
        let mut cursor = Cursor::new(1);
        cursor.navigate(Action::Down, &xmb);
        settle(&mut cursor);

        let scene = build_with(&xmb, &cursor, width, height, true, &AllSlots);
        let category = scene
            .quads
            .iter()
            .filter(|quad| is_icon(quad))
            .find(|quad| ((quad.y + quad.h / 2.0) - height * BAR_CROSS_Y).abs() < 1.0)
            .expect("the category row is drawn");
        assert!((category.w - CATEGORY_ICON_FOCUSED).abs() < 0.5);

        let label = scene
            .texts
            .iter()
            .find(|text| text.content == "Settings")
            .expect("the category is named");
        assert!((label.size - CATEGORY_LABEL).abs() < 0.5);
        assert!(label.color[3] > 0.99, "at full ink: {}", label.color[3]);
    }

    /// The recession arrives over the move rather than on the frame it starts.
    #[test]
    fn falling_back_is_something_that_happens_over_the_step() {
        let xmb = nested();
        let (width, height) = (1920.0, 1080.0);
        let mut cursor = Cursor::new(1);
        cursor.navigate(Action::Down, &xmb);
        settle(&mut cursor);

        let category_size = |cursor: &Cursor| {
            build_with(&xmb, cursor, width, height, true, &AllSlots)
                .quads
                .iter()
                .filter(|quad| is_icon(quad))
                .find(|quad| ((quad.y + quad.h / 2.0) - height * BAR_CROSS_Y).abs() < 1.0)
                .map(|quad| quad.w)
                .expect("the category row is drawn")
        };

        let front = category_size(&cursor);
        cursor.enter(&xmb);
        assert_eq!(category_size(&cursor), front, "not on the press itself");

        let mut previous = front;
        while cursor.animate(1.0 / 60.0) {
            let now = category_size(&cursor);
            assert!(now <= previous + 0.001, "it must not come back forward");
            assert!(
                previous - now < CATEGORY_ICON_FOCUSED * 0.1,
                "a frame took {}px off it",
                previous - now
            );
            previous = now;
        }
        let (near, _) = receded(2.0);
        assert!((previous - CATEGORY_ICON_FOCUSED * near).abs() < 0.5);
    }

    /// A column arrives from somewhere, at every depth: every step in slides
    /// the whole chain along, so the one opening comes in from a full step to
    /// the right of where it will stop rather than fading up in place.
    #[test]
    fn a_column_opened_below_the_first_still_arrives_from_somewhere() {
        let xmb = nested();
        let (width, height) = (1920.0, 1080.0);
        let mut cursor = Cursor::new(1);
        cursor.navigate(Action::Down, &xmb);
        cursor.enter(&xmb);
        settle(&mut cursor);

        let swatch_x = |cursor: &Cursor| {
            let scene = build_with(&xmb, cursor, width, height, true, &Named);
            scene
                .quads
                .iter()
                .find(|quad| quad.slot == Named::slot_of(icons::SWATCH))
                .map(|quad| quad.x + quad.w / 2.0)
        };
        assert_eq!(swatch_x(&cursor), None, "the values column is not open yet");

        cursor.navigate(Action::Right, &xmb);
        let mut travelled: Vec<f32> = Vec::new();
        while cursor.animate(1.0 / 60.0) {
            if let Some(x) = swatch_x(&cursor) {
                travelled.push(x);
            }
        }
        let settled = swatch_x(&cursor).expect("it is open once the step is done");

        let first = *travelled.first().expect("it should be drawn on the way in");
        assert!(
            first - settled > subcolumn_step(height) * 0.5,
            "it appeared {:.0}px from where it stops",
            first - settled
        );
        for pair in travelled.windows(2) {
            assert!(pair[1] <= pair[0] + 0.001, "it must not double back");
        }
        assert!((travelled[travelled.len() - 1] - settled).abs() < 1.0);
    }

    /// What the layout gives a named row, or nothing where it has faded out
    /// altogether. Names rather than icons because a name is the half of a row
    /// that lands on the column beside it.
    fn row_ink(xmb: &Xmb, cursor: &Cursor, name: &str) -> Option<f32> {
        build_with(xmb, cursor, 1920.0, 1080.0, true, &Named)
            .texts
            .iter()
            .find(|text| text.content == name)
            .map(|text| text.color[3])
    }

    /// One step of the bar, watched as a handover between two rows laid over
    /// one another: `going` is the row the step takes away, `coming` the row
    /// taking its place.
    struct Handover {
        /// What `going` had before the step, and then frame by frame for as
        /// long as it was still drawn.
        going: Vec<f32>,
        /// What `coming` had on the frame `going` had gone, and what it
        /// settles at once the step is over.
        coming: f32,
        settled: f32,
        /// How many frames the whole step took.
        frames: usize,
    }

    /// Take `step`, and watch the two rows trade places over it.
    ///
    /// Every step through a path is this same shape — one list laid over
    /// another with one of them on its way out — so a step in and a step out
    /// are measured the same way and held to the same thing.
    fn handover(
        xmb: &Xmb,
        cursor: &mut Cursor,
        step: impl FnOnce(&mut Cursor) -> bool,
        going: &str,
        coming: &str,
    ) -> Handover {
        let mut inks = vec![row_ink(xmb, cursor, going).expect("the row the step takes away")];
        assert_eq!(row_ink(xmb, cursor, coming), None, "and the one it brings");

        assert!(step(cursor), "the step should move the bar");
        let (mut frames, mut arriving) = (0, None);
        while cursor.animate(1.0 / 60.0) {
            frames += 1;
            match row_ink(xmb, cursor, going) {
                Some(alpha) => inks.push(alpha),
                None if arriving.is_none() => arriving = row_ink(xmb, cursor, coming),
                None => {}
            }
        }

        Handover {
            going: inks,
            coming: arriving.expect("the row taking its place is on screen by then"),
            settled: row_ink(xmb, cursor, coming).expect("and it is there at the end"),
            frames,
        }
    }

    impl Handover {
        /// What both directions have to do.
        fn is_clean(&self) {
            // It leans out rather than blinking off on the frame the button
            // lands, and it never comes back.
            let [at_rest, first] = [self.going[0], self.going[1]];
            assert!(
                first > at_rest * 0.8,
                "it went from {at_rest:.2} to {first:.2} in one frame"
            );
            for pair in self.going.windows(2) {
                assert!(pair[1] <= pair[0] + 0.001, "it must not come back");
            }

            // It is out of the way inside the first half of the step, which is
            // the whole point: what is left of the journey belongs to the row
            // taking its place.
            assert!(
                self.going.len() * 2 < self.frames,
                "it held the screen for {} of the {} frames of the step",
                self.going.len(),
                self.frames
            );
            // Gone, but not before that row can be read — what leaves must not
            // leave a hole where what replaces it has not arrived.
            assert!(
                self.coming > self.settled / 3.0,
                "it left at {:.2} against the {:.2} it settles at",
                self.coming,
                self.settled
            );
        }
    }

    /// Stepping into a subcategory, the column it opens over keeps the row it
    /// was opened from — the trail — and gives up the rest of itself before
    /// the column arriving is strong enough to read.
    ///
    /// The complaint this answers, in the direction it was raised second: a
    /// shelf of film cards came up over a list of applications while every
    /// name in that list was still at full strength, so the cards were laid
    /// across the words.
    #[test]
    fn a_column_opening_clears_the_one_it_stands_over() {
        let xmb = nested();
        let mut cursor = Cursor::new(1);
        cursor.navigate(Action::Down, &xmb);
        settle(&mut cursor);

        // "plain" is the row this column is losing — the cursor is on
        // "Appearance", so that one stays as the trail and "plain" does not.
        // "Wallpaper" is a row of the column about to open over it.
        let step = handover(
            &xmb,
            &mut cursor,
            |cursor| cursor.enter(&xmb),
            "plain",
            "Wallpaper",
        );
        step.is_clean();
    }

    /// And coming back out is not that move played backwards: there the whole
    /// column is what leaves, standing over the one it came out of the entire
    /// way home.
    ///
    /// The complaint this answers, in the direction it was raised first: a
    /// subcategory's rows sat at half strength across the middle of the
    /// journey back, printed over the names of the column underneath them.
    #[test]
    fn a_column_stepped_out_of_is_gone_before_the_bar_has_finished_leaving_it() {
        let xmb = nested();
        let mut cursor = walked(&xmb);

        // Two deep: "Purple" is a row of the column standing open, and
        // "Wallpaper" a row of the one it was opened out of — which is the
        // column being come back to, and is not on screen until that starts.
        let step = handover(
            &xmb,
            &mut cursor,
            |cursor| cursor.navigate(Action::Left, &xmb),
            "Purple",
            "Wallpaper",
        );
        step.is_clean();
    }

    /// A column that is a bar is drawn as a bar and nothing else: no disc
    /// behind it, and every mark of it a pane of the shell's own glass.
    ///
    /// The disc is the part worth pinning down. It is drawn under the row the
    /// cursor is on everywhere else in the bar, and behind a track it is a
    /// round pane with a tall thin one lying across it — which reads as a
    /// button that has been pressed rather than as a control.
    #[test]
    fn a_bar_stands_on_its_own_glass_rather_than_on_a_disc() {
        let xmb = Xmb::new(vec![Category {
            id: "settings",
            title: "Settings",
            icon: "settings",
            entries: vec![app("plain"), folder("Color temperature", vec![bar(0.4)])],
        }]);
        // One step in is the whole path: the bar is the only row of the column
        // it opens onto, and there is nowhere further to go.
        let cursor = stepped(&xmb);
        let (width, height) = (1920.0, 1080.0);
        let scene = build_with(&xmb, &cursor, width, height, true, &Named);

        // The groove is the one tall narrow pane the shell draws, and the rest
        // of the bar is what stands in the same channel.
        let groove = scene
            .quads
            .iter()
            .filter(|quad| quad.slot == SOLID_SLOT && quad.h > quad.w * 3.0)
            .max_by(|a, b| a.h.total_cmp(&b.h))
            .expect("the groove");
        let (track_w, track_h) = (groove.w, groove.h);
        let middle = groove.x + track_w / 2.0;
        let marks: Vec<&Quad> = scene
            .quads
            .iter()
            .filter(|quad| quad.slot == SOLID_SLOT && (quad.x + quad.w / 2.0 - middle).abs() < 1.0)
            .collect();
        assert_eq!(marks.len(), 3, "a groove, the light in it, and a handle");
        // Nothing in the channel is wider than the handle — which is what says
        // the disc is gone rather than merely under something.
        assert!(marks
            .iter()
            .all(|quad| quad.w <= track_w * COLUMN_BAR_HANDLE + 1.0));
        for mark in &marks {
            assert!(mark.thickness > 0.0, "every mark is a slab of glass");
            assert_eq!(mark.gloss, GLOSS_FULL, "and takes the shell's own light");
            assert!(
                mark.radius >= mark.w / 2.0 - 0.01,
                "and is a capsule, not a box"
            );
        }

        // No disc. Every round pane the shell draws under a chosen row is as
        // wide as it is tall; the bar's marks are not, but for the handle,
        // which is far smaller than a disc would be.
        let discs = marks
            .iter()
            .filter(|quad| (quad.w - quad.h).abs() < 1.0)
            .count();
        assert_eq!(discs, 1, "the handle, and nothing else round");
        assert!(marks.iter().all(|quad| quad.w < track_h * 0.5));

        // The light is inside the groove, growing up from the foot: its foot is
        // the groove's, its head is not, and it is held clear of the groove's
        // rim on both sides.
        let fill = marks
            .iter()
            .filter(|quad| quad.h < groove.h && (quad.w - quad.h).abs() >= 1.0)
            .max_by(|a, b| a.h.total_cmp(&b.h))
            .expect("the light in it");
        assert!(track_h > height * 0.2, "the bar is the reach of the column");
        assert!(fill.w < groove.w && fill.x > groove.x);
        assert!(fill.x + fill.w < groove.x + groove.w);
        assert!(fill.y + fill.h <= groove.y + groove.h + 0.01);
        assert!(
            fill.y > groove.y + groove.h * 0.4,
            "it stands at 0.4 filled"
        );
    }

    /// And the whole of that track is the control: a press anywhere along it
    /// asks for the value drawn at that point, exactly as a press along the
    /// guide's own bars does.
    ///
    /// The track is most of the column tall and the band a row would otherwise
    /// have is one icon high, so a bar measured as a row answers on a strip
    /// through its middle and nowhere else — which leaves the one control in
    /// the tree that says what it is set to by *where* it is drawn unable to be
    /// set by pointing at it.
    #[test]
    fn a_bar_is_set_by_where_along_its_track_the_press_landed() {
        let fill = 0.4;
        let xmb = Xmb::new(vec![Category {
            id: "settings",
            title: "Settings",
            icon: "settings",
            entries: vec![app("plain"), folder("Color temperature", vec![bar(fill)])],
        }]);
        let cursor = stepped(&xmb);
        let (width, height) = (1920.0, 1080.0);
        let scene = build_with(&xmb, &cursor, width, height, true, &Named);

        // The groove and the handle as they were drawn, because what the hand
        // aims at is what the eye can see — the same reading of the scene the
        // drawing test makes.
        let groove = scene
            .quads
            .iter()
            .filter(|quad| quad.slot == SOLID_SLOT && quad.h > quad.w * 3.0)
            .max_by(|a, b| a.h.total_cmp(&b.h))
            .expect("the groove");
        let middle = groove.x + groove.w / 2.0;
        let handle = scene
            .quads
            .iter()
            .filter(|quad| quad.slot == SOLID_SLOT && (quad.w - quad.h).abs() < 1.0)
            .find(|quad| (quad.x + quad.w / 2.0 - middle).abs() < 1.0)
            .expect("the handle on the end of the fill");

        let pressed = |x: f32, y: f32| match bar_hit(&xmb, &cursor, x, y, width, height) {
            Some(BarSpot::Item { row: 0, level }) => level,
            other => panic!("{other:?} at ({x}, {y})"),
        };

        // The value the handle stands at is the value pressing the handle asks
        // for: the drawing and the hit test are one answer read two ways here
        // as much as anywhere else on the bar.
        let at_handle =
            pressed(middle, handle.y + handle.h / 2.0).expect("the handle is the track");
        assert!((at_handle - fill).abs() < 0.01, "{at_handle} for {fill}");

        // Both ends of the groove reach both ends of the range. Up the track is
        // more, which is the way a quantity is read on anything vertical.
        let head = pressed(middle, groove.y + 1.0).expect("the head of the track");
        let foot = pressed(middle, groove.y + groove.h - 1.0).expect("the foot of it");
        assert!(head > 0.99, "{head}");
        assert!(foot < 0.01, "{foot}");

        // And every point between them is somewhere on the range, in order.
        // This is what the row's own band could not do: outside the one icon it
        // is high, the length of the track answered to nothing at all.
        let mut last = f32::INFINITY;
        let mut step = groove.y + 1.0;
        while step < groove.y + groove.h {
            let level = pressed(middle, step).unwrap_or_else(|| panic!("nothing at {step}"));
            assert!(level <= last + 1e-6, "{level} below {last} at {step}");
            last = level;
            step += 1.0;
        }

        // The name beside the track is the row rather than a place on it, which
        // is the ordinary press — and on a bar that does nothing at all.
        let beside = middle + ITEM_ICON_FOCUSED * ITEM_DISC * guide_scale(height);
        assert_eq!(
            pressed(beside, groove.y + groove.h * 0.5),
            None,
            "the label is the row, not a value on it"
        );

        // And the track answers for its own length only. Past either end of it
        // there is no value to be asking for, and it does not reach across the
        // column at the height it happens to be drawn at.
        for at in [groove.y - 2.0, groove.y + groove.h + 2.0] {
            assert_eq!(bar_hit(&xmb, &cursor, middle, at, width, height), None);
        }
        assert_eq!(
            bar_hit(&xmb, &cursor, beside, groove.y + 1.0, width, height),
            None,
            "the head of the track is not the whole of that line"
        );
    }

    /// The value a setting is set to is marked, and a colour is drawn in
    /// itself: the swatch is the answer, and the word beside it is its name.
    #[test]
    fn the_value_in_force_is_marked_and_drawn_in_its_own_colour() {
        let xmb = nested();
        let cursor = walked(&xmb);
        let scene = build_with(&xmb, &cursor, 1920.0, 1080.0, true, &Named);

        let purple = crate::theme::Color(0x8B5CF6).rgb();
        let swatches: Vec<&Quad> = scene
            .quads
            .iter()
            .filter(|quad| is_icon(quad) && quad.color[..3] == purple)
            .collect();
        assert_eq!(swatches.len(), 2, "every colour is drawn in itself");
        // The chosen one is also the one the cursor is on, so it is the larger.
        let swatch = swatches
            .iter()
            .max_by(|a, b| a.w.total_cmp(&b.w))
            .expect("the chosen colour is drawn in itself");

        // The mark sits on the swatch's own corner rather than beside it.
        let badge = scene
            .quads
            .iter()
            .find(|quad| quad.slot == Named::slot_of(icons::CHOSEN))
            .expect("the value in force is marked");
        assert!(badge.w < swatch.w * 0.6);
        assert!(badge.x > swatch.x + swatch.w * 0.5);
        assert!(badge.y > swatch.y + swatch.h * 0.5);
        assert!(badge.x < swatch.x + swatch.w);

        // Only the one in force: the other colour in the list is unmarked.
        assert_eq!(
            scene
                .quads
                .iter()
                .filter(|quad| quad.slot == Named::slot_of(icons::CHOSEN))
                .count(),
            1
        );
    }

    /// An application launched from inside a path opens out of the tile it was
    /// chosen from, which is not where the tile is at the top level.
    #[test]
    fn a_launch_from_inside_a_path_opens_from_the_right_tile() {
        let xmb = nested();
        let cursor = walked(&xmb);
        let (width, height) = (1920.0, 1080.0);
        let scene = build_with(&xmb, &cursor, width, height, true, &AllSlots);

        let tile = launch_origin(width, height);
        let centre = [tile[0] + tile[2] * 0.5, tile[1] + tile[3] * 0.5];
        let lit = scene
            .quads
            .iter()
            .find(|quad| quad.slot == GLOW_SLOT && quad.color[3] > 0.01)
            .expect("the open column has a chosen row");
        assert!((lit.x + lit.w * 0.5 - centre[0]).abs() < 0.5);
        assert!((lit.y + lit.h * 0.5 - centre[1]).abs() < 0.5);
    }

    /// A column of the user's own pictures, for the card tests.
    ///
    /// Inside a subcategory, which is the only place one is ever reached: the
    /// user's files hang on a row of their own — Music, Video, Images — under
    /// the column that owns them, so a library is always a column stepped into
    /// and never the outermost. Which is what lets it use the band the category
    /// row would otherwise be holding open, so a fixture that stood one at the
    /// top level would be testing a layout the shell never draws.
    fn album(paths: &[&str]) -> Xmb {
        Xmb::new(vec![Category {
            id: "graphics",
            title: "Graphics",
            icon: "g",
            entries: vec![folder(
                "Images",
                paths.iter().map(|path| picture(path)).collect(),
            )],
        }])
    }

    /// That album, open, with the cursor settled on its first picture.
    fn opened(xmb: &Xmb, width: f32, height: f32, slots: &impl SlotLookup) -> Scene {
        let mut cursor = Cursor::new(xmb.categories.len());
        assert!(cursor.enter(xmb));
        settle(&mut cursor);
        build_with(xmb, &cursor, width, height, true, slots)
    }

    /// A shelf as the worker hands it over: the field, whatever the search has
    /// left, and the row that puts the rest back.
    fn shelf(kind: crate::media::Kind, query: &str, paths: &[&str], found: usize) -> Xmb {
        let listing = paths
            .iter()
            .map(|path| {
                std::sync::Arc::new(crate::media::File::at(Path::new(path)).expect("a file"))
            })
            .collect();
        Xmb::new(vec![Category {
            id: "graphics",
            title: "Graphics",
            icon: "g",
            entries: vec![folder(
                "Images",
                crate::apps::media_rows(listing, kind, query, found),
            )],
        }])
    }

    /// The field at the head of a shelf is an ordinary row until the board
    /// comes up over it, and then it is a field: what it says is what has been
    /// typed, with the caret on the end of it.
    #[test]
    fn only_the_field_being_typed_into_carries_the_caret() {
        let xmb = shelf(
            crate::media::Kind::Image,
            "sun",
            &["/home/x/Pictures/sunset.jpg"],
            4,
        );
        let mut cursor = Cursor::new(xmb.categories.len());
        assert!(cursor.enter(&xmb));
        // Stepping in lands on the pictures; the field is the row above them.
        while cursor.navigate(Action::Up, &xmb) {}
        settle(&mut cursor);
        let says = |typing: bool| {
            build(
                &xmb, &cursor, 1920.0, 1080.0, true, None, 0.0, &AllSlots, typing,
            )
            .texts
            .into_iter()
            .map(|text| text.content)
            .collect::<Vec<String>>()
        };

        // Nobody typing: the row reads as what the column is filtered by.
        let quiet = says(false);
        assert!(quiet.iter().any(|said| said == "sun"), "{quiet:?}");
        assert!(!quiet.iter().any(|said| said == "sun|"), "{quiet:?}");

        // Typing: the caret is on the end of the query, and on nothing else.
        // The row below offers to clear the search and is not a field, so a
        // caret on it would be a second place the letters might be going.
        let typed = says(true);
        assert!(typed.iter().any(|said| said == "sun|"), "{typed:?}");
        assert!(typed.iter().any(|said| said == "Clear search"), "{typed:?}");
        assert!(
            !typed
                .iter()
                .any(|said| said.ends_with("|") && said != "sun|"),
            "{typed:?}"
        );
    }

    /// An empty field being typed into is empty, rather than holding the word
    /// that stands in for a field nobody is in.
    #[test]
    fn a_field_opened_on_nothing_holds_only_the_caret() {
        let xmb = shelf(
            crate::media::Kind::Image,
            "",
            &["/home/x/Pictures/sunset.jpg"],
            1,
        );
        let mut cursor = Cursor::new(xmb.categories.len());
        assert!(cursor.enter(&xmb));
        while cursor.navigate(Action::Up, &xmb) {}
        settle(&mut cursor);
        let said: Vec<String> = build(
            &xmb, &cursor, 1920.0, 1080.0, true, None, 0.0, &AllSlots, true,
        )
        .texts
        .into_iter()
        .map(|text| text.content)
        .collect();
        assert!(said.iter().any(|said| said == "|"), "{said:?}");
        assert!(!said.iter().any(|said| said == "Search"), "{said:?}");
    }

    /// A column of photographs searched down to nothing is still a column of
    /// photographs, so the rows do not change shape underneath the user as the
    /// first match arrives.
    #[test]
    fn a_shelf_narrowed_to_nothing_still_knows_what_it_is_a_shelf_of() {
        let pictures = crate::apps::media_rows(Vec::new(), crate::media::Kind::Image, "zzz", 12);
        assert_eq!(cards_in(&pictures), Some(Cards::of(CARD_ASPECT)));
        let songs = crate::apps::media_rows(Vec::new(), crate::media::Kind::Audio, "zzz", 12);
        assert_eq!(cards_in(&songs), None, "music has no picture in it to show");
    }

    /// A Steam library is a column of covers, and a cover is a different shape
    /// from a film frame. What the two hold in common is how much of the glass
    /// a row takes up — see [`Cards::of`] — because that is what makes a
    /// column of either read as the same list.
    #[test]
    fn a_library_is_a_column_of_covers_weighing_what_a_picture_weighs() {
        let games = vec![game(2835570, "Buckshot Roulette"), game(504230, "Celeste")];
        let cards = cards_in(&games).expect("a column of covers");
        let pictures = Cards::of(CARD_ASPECT);

        assert!(
            (cards.aspect - 600.0 / 900.0).abs() < 0.001,
            "a cover is Valve's portrait capsule: {}",
            cards.aspect
        );
        assert!(cards.focused > pictures.focused, "taller than a film frame");
        assert!(cards.reach() < pictures.reach(), "and narrower");
        for (cover, picture) in [
            (cards.height, pictures.height),
            (cards.focused, pictures.focused),
        ] {
            let area = |height: f32, cards: Cards| height * height * cards.aspect;
            let (cover, picture) = (area(cover, cards), area(picture, pictures));
            assert!(
                (cover / picture - 1.0).abs() < 0.001,
                "a row of either takes the same room: {cover} against {picture}"
            );
        }
    }

    /// The cover is the row, exactly as a photograph is: it stands on the card
    /// in the game's place, and the Steam mark that stood in for it goes.
    #[test]
    fn a_game_wears_its_cover_instead_of_the_steam_mark() {
        let xmb = library(&[(2835570, "Buckshot Roulette"), (504230, "Celeste")]);
        let marks = |scene: &Scene| scene.quads.iter().filter(|q| q.slot == 7).count();

        let waiting = opened(&xmb, 1920.0, 1080.0, &AllSlots);
        let arrived = opened(&xmb, 1920.0, 1080.0, &Covers);
        assert!(
            marks(&waiting) > 0,
            "the Steam mark stands in until there is a cover"
        );

        let drawn: Vec<&Quad> = arrived
            .quads
            .iter()
            .filter(|quad| quad.slot == THUMB_SLOT)
            .collect();
        assert_eq!(drawn.len(), 2, "every row of it, not only the chosen one");
        assert_eq!(
            marks(&arrived),
            marks(&waiting) - 2,
            "and steps aside once each has one"
        );

        // Filling its card, with an even border round it: a capsule is the
        // shape the card was built around, so what is left over is the mount
        // and nothing else. Letterboxed, or wider on two sides than the other
        // two, and it reads as the wrong picture rather than as a frame.
        let cards = Cards::of(COVER_ASPECT);
        let scale = guide_scale(1080.0);
        let cover = drawn
            .iter()
            .max_by(|a, b| a.h.total_cmp(&b.h))
            .expect("a cover");
        let height = cards.focused * scale;
        let mount = cards.mount(height);
        assert!(
            (cover.h - (height - mount * 2.0)).abs() < 0.5,
            "inside the card's mount: {} of {height}",
            cover.h
        );
        assert!(
            (cover.w - (cards.width(height) - mount * 2.0)).abs() < 0.5,
            "and across it: {}",
            cover.w
        );
        assert!(
            (cover.w / cover.h - COVER_ASPECT).abs() < 0.01,
            "at its own shape: {}x{}",
            cover.w,
            cover.h
        );
    }

    /// A game that is not on this disk wears the same cover with the colour
    /// taken out of it.
    ///
    /// The library is installed-first, so this is what tells the two halves of
    /// it apart at a glance and from across a room: the games that can be
    /// played are the ones in colour. Nothing else in the shell is drained —
    /// a photograph the user owns is not a photograph they are missing.
    #[test]
    fn a_game_that_is_not_here_wears_a_colourless_cover() {
        let xmb = part_owned(&[(1, "Here", true), (2, "Not here", false)]);
        let scene = opened(&xmb, 1920.0, 1080.0, &Covers);
        let mut covers: Vec<&Quad> = scene
            .quads
            .iter()
            .filter(|quad| quad.slot == THUMB_SLOT)
            .collect();
        assert_eq!(covers.len(), 2, "both rows wear one");
        // Down the column, which is the order the library is in: the chosen
        // row is the first of them and stands taller than the rest.
        covers.sort_by(|a, b| a.y.total_cmp(&b.y));
        assert_eq!(
            covers[0].drain, 0.0,
            "on the disk, so drawn as Steam made it"
        );
        assert_eq!(covers[1].drain, 1.0, "not on it, so drawn colourless");

        let photographs = opened(
            &shelf(crate::media::Kind::Image, "", &["/p/a.jpg", "/p/b.jpg"], 2),
            1920.0,
            1080.0,
            &Pictures(3.0 / 2.0),
        );
        assert!(
            photographs.quads.iter().all(|quad| quad.drain == 0.0),
            "and nothing else in the shell is drained of anything"
        );
    }

    /// A column of covers stands evenly apart and clear of the category row it
    /// hangs under — and a column of icons is exactly where it always was.
    ///
    /// Three complaints in one, all of them the same mistake: a distance to a
    /// row's *centre* that was measured when every row was an icon. A cover is
    /// two and a half times as tall as that, so it reached up through the
    /// category's name, sat on the glass of the row above, and left its
    /// neighbours nearly touching it while the pairs further down the column
    /// stood half a card apart.
    #[test]
    fn a_library_stands_evenly_apart_and_clear_of_the_category_row() {
        let height = 1080.0;
        let scale = guide_scale(height);
        let cross_y = height * BAR_CROSS_Y;
        let cards = Cards::of(COVER_ASPECT);
        let y = |offset: f32| bar_item_y(offset, 0, 1.0, height, Some(cards));
        let half = |offset: f32| {
            let card = if offset == 0.0 {
                cards.focused
            } else {
                cards.height
            };
            card * scale / 2.0
        };

        // The glass between the chosen card and the row under it is the same
        // width as the glass between any other two.
        let below_the_chosen = (y(1.0) - half(1.0)) - (y(0.0) + half(0.0));
        let further_down = (y(2.0) - half(2.0)) - (y(1.0) + half(1.0));
        assert!(
            (below_the_chosen - further_down).abs() < 0.5,
            "{below_the_chosen} under the chosen card and {further_down} between the rest"
        );
        assert!(
            (further_down - cards.air() * scale).abs() < 0.5,
            "and it is the air the cards were spaced by: {further_down}"
        );

        // Above the category row, the same air an icon column has always had.
        let above = (cross_y - CATEGORY_HALF * scale) - (y(-1.0) + half(-1.0));
        assert!(
            (above - CATEGORY_AIR_ABOVE * scale).abs() < 0.5,
            "{above} between the row above and the category's glass"
        );

        // And below its label, more of it — see `CATEGORY_LABEL_TO_CARDS`.
        let under_the_label = (y(0.0) - half(0.0)) - (cross_y + CATEGORY_TO_LABEL * scale);
        assert!(
            (under_the_label - CATEGORY_LABEL_TO_CARDS * scale).abs() < 0.5,
            "{under_the_label} between the category's name and the first cover"
        );
        assert!(
            under_the_label > CATEGORY_LABEL_TO_ICONS * scale,
            "which is more than an icon column asks for"
        );

        // None of which has moved a column of icons by a pixel.
        let icons = |offset: f32| bar_item_y(offset, 0, 1.0, height, None);
        assert!((icons(0.0) - (cross_y + ITEM_GAP_BELOW * scale)).abs() < 0.001);
        assert!((icons(-1.0) - (cross_y - ITEM_GAP_ABOVE * scale)).abs() < 0.001);
        assert!((icons(2.0) - icons(1.0) - ITEM_SPACING * scale).abs() < 0.001);
    }

    /// The glass round a picture is the same width on all four sides, whatever
    /// shape the cards in that column are.
    ///
    /// The eye is very good at this: a border half again as wide above as
    /// beside reads as a picture that has slipped in its frame. It was, on the
    /// first pass at covers — the card was cut to the picture's own aspect and
    /// then inset by a share of each side, which is a different number of
    /// pixels on each.
    #[test]
    fn the_mount_is_the_same_width_all_the_way_round() {
        /// The glass left showing beside the chosen row's picture, and above
        /// it.
        fn mount_of(xmb: &Xmb, slots: &impl SlotLookup) -> (f32, f32) {
            let scene = opened(xmb, 1920.0, 1080.0, slots);
            let card = scene
                .quads
                .iter()
                .filter(|quad| quad.thickness > 0.0 && quad.h > 0.0)
                .max_by(|a, b| a.h.total_cmp(&b.h))
                .expect("the chosen card");
            let picture = scene
                .quads
                .iter()
                .filter(|quad| quad.slot == THUMB_SLOT)
                .max_by(|a, b| a.h.total_cmp(&b.h))
                .expect("the picture on it");
            ((card.w - picture.w) / 2.0, (card.h - picture.h) / 2.0)
        }

        let films = mount_of(
            &album(&["/home/x/a.jpg", "/home/x/b.jpg"]),
            &Pictures(CARD_ASPECT),
        );
        let covers = mount_of(
            &library(&[(2835570, "Buckshot Roulette"), (504230, "Celeste")]),
            &Covers,
        );
        for (beside, above) in [films, covers] {
            assert!(
                (beside - above).abs() < 0.5,
                "{beside} beside the picture and {above} above it"
            );
            assert!(above > 1.0, "and there is a mount at all: {above}");
        }
    }

    /// The picture is the row. It stands on the card at its own shape, fitted
    /// rather than filled, and the glyph the column is marked with is not
    /// drawn over it.
    #[test]
    fn a_picture_stands_on_its_card_at_its_own_shape() {
        let xmb = album(&["/home/x/a.jpg", "/home/x/b.jpg"]);
        let wide = opened(&xmb, 1920.0, 1080.0, &Pictures(16.0 / 9.0));
        let scale = guide_scale(1080.0);

        let drawn: Vec<&Quad> = wide
            .quads
            .iter()
            .filter(|quad| quad.slot == THUMB_SLOT)
            .collect();
        assert_eq!(drawn.len(), 2, "every row of it, not only the chosen one");

        // The focused one is the taller, and it is a card rather than a disc.
        let card = drawn
            .iter()
            .max_by(|a, b| a.h.total_cmp(&b.h))
            .expect("a picture");
        assert!(
            (card.w / card.h - 16.0 / 9.0).abs() < 0.01,
            "drawn at its own shape: {}x{}",
            card.w,
            card.h
        );
        let cards = Cards::of(CARD_ASPECT);
        let height = CARD_HEIGHT_FOCUSED * scale;
        assert!(
            (card.h - (height - cards.mount(height) * 2.0)).abs() < 0.5,
            "inside the card's mount: {}",
            card.h
        );

        // A tall picture is fitted into the same card, so the row keeps its
        // height and the labels keep their column.
        let tall = opened(&xmb, 1920.0, 1080.0, &Pictures(0.5));
        let portrait = tall
            .quads
            .iter()
            .filter(|quad| quad.slot == THUMB_SLOT)
            .max_by(|a, b| a.h.total_cmp(&b.h))
            .expect("a picture");
        assert!((portrait.h - card.h).abs() < 1.0, "the same height");
        assert!(portrait.w < card.w * 0.4, "and much narrower");
        assert!(
            (portrait.w / portrait.h - 0.5).abs() < 0.01,
            "still its own shape"
        );
    }

    /// Until the picture arrives the row is the column's glyph, and the moment
    /// it does the glyph goes: a film strip stamped over a frame is two marks
    /// for one thing.
    #[test]
    fn the_glyph_gives_way_to_the_picture() {
        let xmb = album(&["/home/x/a.jpg"]);

        // Slot 7 is every glyph these doubles hand out, the category button's
        // included, so what says the row's own glyph has gone is one fewer of
        // them rather than none.
        let glyphs = |scene: &Scene| scene.quads.iter().filter(|q| q.slot == 7).count();

        let waiting = opened(&xmb, 1920.0, 1080.0, &AllSlots);
        let arrived = opened(&xmb, 1920.0, 1080.0, &Pictures(1.5));
        assert!(
            glyphs(&waiting) > 0,
            "the glyph stands in until there is a picture"
        );
        assert!(arrived.quads.iter().any(|quad| quad.slot == THUMB_SLOT));
        assert_eq!(
            glyphs(&arrived),
            glyphs(&waiting) - 1,
            "and steps aside once it has"
        );
    }

    /// The rows have to stand in the same places whether or not any picture
    /// has loaded — otherwise a column would walk up and down under the cursor
    /// as the workers finished.
    #[test]
    fn a_picture_arriving_moves_nothing() {
        let xmb = album(&["/home/x/a.jpg", "/home/x/b.jpg", "/home/x/c.jpg"]);
        // Where each row's own name was drawn. The titles are one letter each,
        // so nothing else in the scene can be mistaken for one.
        let rows = |scene: &Scene| -> Vec<f32> {
            let mut ys: Vec<f32> = scene
                .texts
                .iter()
                .filter(|text| matches!(text.content.as_str(), "a" | "b" | "c"))
                .map(|text| text.y)
                .collect();
            ys.sort_by(f32::total_cmp);
            ys
        };

        let waiting = opened(&xmb, 1920.0, 1080.0, &AllSlots);
        let arrived = opened(&xmb, 1920.0, 1080.0, &Pictures(1.5));
        assert_eq!(rows(&waiting).len(), 3);
        assert_eq!(rows(&waiting), rows(&arrived));

        // And a column of pictures stands further apart than a column of
        // applications, which is what makes room for them.
        let apps = Xmb::new(vec![Category {
            id: "a",
            title: "A",
            icon: "a",
            entries: vec![app("a"), app("b"), app("c")],
        }]);
        let icons = focused(&apps, 1920.0, 1080.0, &AllSlots);
        let ys = rows(&arrived);
        assert!(
            ys[2] - ys[1] > rows(&icons)[2] - rows(&icons)[1],
            "a picture needs the room"
        );

        // Every card, top to bottom, as where its middle is and how tall it is.
        //
        // Measured from the pictures rather than from the labels beside them:
        // the chosen row's title sits above its own centre to leave room for
        // the line under it, so a column of labels is not a straight ruler. A
        // picture is fitted inside its card's mount, which is what turns its
        // height back into the card's.
        let mut cards: Vec<(f32, f32)> = arrived
            .quads
            .iter()
            .filter(|quad| quad.slot == THUMB_SLOT)
            .map(|quad| (quad.y + quad.h / 2.0, quad.h / (1.0 - 2.0 * CARD_MOUNT)))
            .collect();
        cards.sort_by(|a, b| a.0.total_cmp(&b.0));
        assert_eq!(cards.len(), 3);

        // The clear glass between two of them. The chosen card is nearly twice
        // its neighbours, so these two gaps come out equal only if the *pitch*
        // does not — which is the whole of why a column of pictures is laid out
        // about its chosen row rather than on one step.
        let air = |a: (f32, f32), b: (f32, f32)| (b.0 - a.0) - (a.1 + b.1) / 2.0;
        let beside_chosen = air(cards[0], cards[1]);
        let between_rest = air(cards[1], cards[2]);

        // Under nothing, the chosen picture is drawn over the one after it.
        assert!(beside_chosen > 0.0, "rows overlap: {beside_chosen}");
        // Equal, or the chosen card has eaten its neighbour's air: on one step
        // it sat almost touching the rows either side of it while every other
        // pair in the column stood well apart.
        assert!(
            (beside_chosen - between_rest).abs() < 1.0,
            "uneven: {beside_chosen} beside the chosen row, {between_rest} below it"
        );
        // And the air stays shorter than the cards. It is the one number a
        // column of pictures is judged by from across the room, and at the
        // pitch this started out with it was a dead heat — as much wallpaper as
        // picture, with a display's worth of room going to neither.
        assert!(
            between_rest < cards[2].1,
            "more gap than card: {between_rest} against {}",
            cards[2].1
        );
    }

    /// And a click has to land where the eye says the row is, which for a card
    /// column is a different band from an icon column's.
    #[test]
    fn a_card_is_pointed_at_where_it_is_drawn() {
        let xmb = album(&["/home/x/a.jpg", "/home/x/b.jpg", "/home/x/c.jpg"]);
        let mut cursor = Cursor::new(xmb.categories.len());
        assert!(cursor.enter(&xmb));
        settle(&mut cursor);
        let (width, height) = (1920.0, 1080.0);

        // The library stands one column in, which is where the hand finds it.
        let depth = cursor.depth_position();
        let x = bar_column_x(1.0, depth, width, height);
        let row_y = |row: usize| {
            bar_item_y(
                row as f32 - cursor.item_position,
                1,
                1.0,
                height,
                Some(Cards::of(CARD_ASPECT)),
            )
        };
        for row in 0..3 {
            assert_eq!(
                bar_hit(&xmb, &cursor, x, row_y(row), width, height),
                row_hit(row),
                "row {row}"
            );
        }

        // And every point between the first row and the last belongs to one of
        // them. The rows of a column of pictures are not evenly spaced — the
        // chosen card pushes the two beside it out — so bands cut from the
        // pitch fall short of meeting either side of it, and the strip left
        // over answers to nothing and swallows the click.
        let (top, bottom) = (row_y(0), row_y(2));
        let mut step = top;
        while step <= bottom {
            assert!(
                matches!(
                    bar_hit(&xmb, &cursor, x, step, width, height),
                    Some(BarSpot::Item { .. })
                ),
                "nothing at {step}, between {top} and {bottom}"
            );
            step += 1.0;
        }
    }

    #[test]
    fn offscreen_rows_are_culled() {
        // A long list must not emit a quad per entry.
        let entries: Vec<Entry> = (0..500).map(|i| app(&format!("app{i}"))).collect();
        let xmb = Xmb::new(vec![Category {
            id: "a",
            title: "A",
            icon: "a",
            entries,
        }]);

        let scene = focused(&xmb, 1920.0, 1080.0, &NoSlots);
        assert!(
            scene.quads.len() < 40,
            "expected culling, got {} quads",
            scene.quads.len()
        );
    }

    /// And they are culled without being *looked at*: the drawing and the hit
    /// test walk [`rows_in_view`] rather than the list, so a shelf holding
    /// everything under somebody's home directory costs a screen's worth of
    /// rows per frame and no more.
    ///
    /// The window has to be generous, though. Every row it leaves out is a row
    /// that will not be drawn however plainly it is on screen, so this asserts
    /// the whole of what the layout would have put on the display is inside it.
    #[test]
    fn only_a_screens_worth_of_a_long_column_is_ever_looked_at() {
        let (height, rows) = (1080.0, 4_000);
        let scale = guide_scale(height);

        // Every pitch the shell lays a column out at: icons, the wide cards a
        // film stands on, and the tall ones a game's cover does.
        for cards in [
            None,
            Some(Cards::of(CARD_ASPECT)),
            Some(Cards::of(COVER_ASPECT)),
        ] {
            let pitch = cards.map_or(ITEM_SPACING, |cards| cards.spacing()) * scale;
            // The outermost column straddles the category row; every column
            // opened out of one is laid out about its own chosen row.
            for level in [0, 1] {
                for position in [0.0, 3.5, 1_500.0, (rows - 1) as f32] {
                    let shown = rows_in_view(position, rows, pitch, height);
                    assert!(
                        shown.len() < 40,
                        "{} rows for one screen at {position}",
                        shown.len()
                    );
                    for index in 0..rows {
                        let y = bar_item_y(index as f32 - position, level, 1.0, height, cards);
                        // The same bound the callers cull against, one row
                        // either side of the display.
                        if y < -pitch || y > height + pitch {
                            continue;
                        }
                        assert!(
                            shown.contains(&index),
                            "row {index} is on screen at {y} and would not be drawn"
                        );
                    }
                }
            }
        }
    }

    /// A row deep inside a long column is still the row the hand lands on.
    #[test]
    fn a_click_finds_a_row_far_down_a_long_list() {
        let paths: Vec<String> = (0..5_000).map(|i| format!("/home/x/{i:05}.jpg")).collect();
        let xmb = album(&paths.iter().map(String::as_str).collect::<Vec<&str>>());
        let mut cursor = Cursor::new(xmb.categories.len());
        assert!(cursor.enter(&xmb));
        assert!(cursor.point_at_row(2_500, &xmb));
        settle(&mut cursor);

        let (width, height) = (1920.0, 1080.0);
        let x = bar_column_x(1.0, cursor.depth_position(), width, height);
        let position = cursor.columns(&xmb)[1].position;
        for row in 2_499..=2_501 {
            let y = bar_item_y(
                row as f32 - position,
                1,
                1.0,
                height,
                Some(Cards::of(CARD_ASPECT)),
            );
            assert_eq!(
                bar_hit(&xmb, &cursor, x, y, width, height),
                row_hit(row),
                "row {row}"
            );
        }
    }

    #[test]
    fn a_display_that_is_not_taking_input_is_dimmed_but_still_legible() {
        let xmb = Xmb::new(vec![Category {
            id: "a",
            title: "A",
            icon: "a",
            entries: vec![app("first"), app("second")],
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
            entries: vec![app("first"), app("second"), app("third"), app("fourth")],
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
                entries: vec![app("only")],
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
            entries: vec![app("first"), app("second")],
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
            entries: vec![app("first"), app("second"), app("third")],
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
            entries: vec![app("first")],
        }]);
        let cursor = Cursor::new(1);

        let glow_alpha = |time: f32| {
            let scene = build(
                &xmb, &cursor, 1920.0, 1080.0, true, None, time, &AllSlots, false,
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

    /// An invented display. The guide only ever prints the name it is given,
    /// so nothing here should be tied to a connector on any one machine.
    const TEST_SCREEN: &str = "TEST-OUT-1";

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
                do_not_disturb: false,
                unread: 0.0,
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

        let among_others = guide_scene(&guide, Some("Celeste"), Some(TEST_SCREEN), &[], None);
        assert!(among_others
            .texts
            .iter()
            .any(|t| t.content == format!("Screen {TEST_SCREEN}")));
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
        let scene = guide_scene(&guide, Some("Celeste"), Some(TEST_SCREEN), &cards, None);

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
        // Measured against a row and not against the tile line, which is
        // centred on the panel rather than started at the margin.
        let resume = items
            .iter()
            .position(|item| *item == Item::Resume)
            .expect("Resume is always present");
        assert!((px - menu_item_rect(&items, resume, 1920.0, 1080.0)[0]).abs() < 0.5);

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
            !scene.texts.iter().any(|text| text.content == "LineXinBar"),
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
            do_not_disturb: false,
            unread: 0.0,
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
        assert!(scene.texts.iter().any(|text| text.content == "LineXinBar"));
    }

    /// A bar is a groove with a value in it, so where it was pressed is the
    /// answer — and its head is not part of that. On the volume bar the head is
    /// the crossed-out speaker, and a click there means silence rather than
    /// "as quiet as this groove can express".
    #[test]
    fn a_bar_is_set_by_where_along_it_the_press_landed() {
        let (width, height) = (1920.0, 1080.0);
        let mut guide = Guide::default();
        guide.set_bars(BOTH_BARS);
        let items = guide.items(true);
        let row = items
            .iter()
            .position(|item| *item == Item::Volume)
            .expect("the volume bar is in the column");
        let chip = menu_item_rect(&items, row, width, height);
        let [track_x, _, track_w, _] = bar_track_line(chip, guide_scale(height));

        assert_eq!(bar_level_at(chip, height, track_x), Some(0.0));
        assert_eq!(bar_level_at(chip, height, track_x + track_w), Some(1.0));
        let half = bar_level_at(chip, height, track_x + track_w * 0.5).unwrap();
        assert!((half - 0.5).abs() < 0.001, "{half}");

        // The glyph at its head, and the chip's own padding before that.
        assert_eq!(bar_level_at(chip, height, chip[0] + 1.0), None);
        assert_eq!(bar_level_at(chip, height, track_x - 1.0), None);

        // Past the far end is the top of the range rather than nothing: a
        // finger that overshoots the groove's last pixel meant "all the way".
        assert_eq!(
            bar_level_at(chip, height, chip[0] + chip[2] * 2.0),
            Some(1.0)
        );
    }

    /// The mixer's rows are the same instrument: a groove to set, and a speaker
    /// at its head to silence. Its start depends on what the atlas could draw,
    /// which is exactly why the drawing and the hit test read it from one
    /// place.
    #[test]
    fn a_mixer_rows_groove_starts_after_whatever_was_drawn_before_it() {
        let chip = [100.0, 100.0, 400.0, 90.0];
        let height = 1080.0;
        let level = Level {
            value: 0.5,
            muted: false,
        };
        let entry = MenuEntry::new(crate::menu::Command::MuteShell, "System")
            .icon("icon")
            .level(level);

        let with_icons = mixer_level_at(chip, &entry, level, height, &Named, chip[0] + 1.0);
        assert_eq!(with_icons, None, "the icon and the speaker come first");
        let far = mixer_level_at(chip, &entry, level, height, &Named, chip[0] + chip[2]);
        assert_eq!(far, Some(1.0));

        // With nothing in the atlas the groove starts further left, because
        // nothing is drawn in front of it — and the hit test has to know that
        // or every press lands short of where the user aimed.
        let bare = mixer_track_line(chip, &entry, level, guide_scale(height), &NoSlots);
        let drawn = mixer_track_line(chip, &entry, level, guide_scale(height), &Named);
        assert!(bare[0] < drawn[0]);
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
                Item::DoNotDisturb,
                Item::Notifications,
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

        // Rows in order, none overlapping, whatever heights they are. Every
        // tile but the first is skipped: they are beside it rather than under
        // it, which is exactly what the assertion above checks.
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
                Item::DoNotDisturb,
                Item::Notifications,
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
        let resume_row = plain_items
            .iter()
            .position(|item| *item == Item::Resume)
            .unwrap();
        let button = menu_item_rect(&plain_items, resume_row, 1920.0, 1080.0);
        let bar = menu_item_rect(&items, index_of(Item::Volume), 1920.0, 1080.0);
        assert_eq!(button[0], bar[0], "and every row is inset the same");
        assert!(bar[3] < button[3], "a bar's row is shorter than a button's");
    }

    /// Controls sit on the broad face of the sidebar rather than on its
    /// rounded rim. A labelled button then repeats that same amount of air
    /// inside itself, on both sides of the line available to its text.
    #[test]
    fn the_buttons_have_balanced_padding_inside_the_sidebar() {
        let mut fullest = Guide::default();
        fullest.set_pointer_control(true);
        fullest.set_bars(BOTH_BARS);

        for [width, height] in [
            [960.0, 600.0],
            [1000.0, 742.0],
            [1280.0, 720.0],
            // The narrowest sidebar this shell draws against tiles at nearly
            // their full size: four of them do not fit between the margins
            // here, and the line is taken in rather than allowed to overhang
            // them. It is also the size the nested session runs at, which is
            // where the overhang was seen.
            [1280.0, 800.0],
            [1920.0, 1080.0],
            [3440.0, 1440.0],
        ] {
            let scale = guide_scale(height);
            let expected = GUIDE_MARGIN * scale;
            let [panel_x, panel_y, panel_w, panel_h] = sidebar_panel_rect(width, height);
            assert!(
                expected > DEPTH_SIDEBAR * scale,
                "the controls must clear the sidebar bevel"
            );

            let items = fullest.items(true);
            for (index, item) in items.iter().enumerate() {
                let [x, y, w, h] = menu_item_rect(&items, index, width, height);
                if *item == Item::Power {
                    let left = x - panel_x;
                    let bottom = panel_y + panel_h - (y + h);
                    assert!((left - expected).abs() < 0.01, "{width}x{height}");
                    assert!((bottom - expected).abs() < 0.01, "{width}x{height}");
                    continue;
                }

                // The tile line is measured as a line, below: it is centred
                // on the panel, so no one tile on it is at the margin.
                if item.is_tile() {
                    continue;
                }
                assert!((x - panel_x - expected).abs() < 0.01, "{width}x{height}");
                let right = panel_x + panel_w - (x + w);
                assert!((right - expected).abs() < 0.01, "{width}x{height}");
            }

            // The tiles are centred rather than inset, so what has to balance
            // is the run: the air to the left of the first equals the air to
            // the right of the last, and neither is inside the margin the rows
            // keep.
            let tiles: Vec<[f32; 4]> = items
                .iter()
                .enumerate()
                .filter(|(_, item)| item.is_tile())
                .map(|(index, _)| menu_item_rect(&items, index, width, height))
                .collect();
            let first = tiles.first().expect("the column always carries tiles");
            let last = tiles.last().expect("the column always carries tiles");
            let left = first[0] - panel_x;
            let right = panel_x + panel_w - (last[0] + last[2]);
            assert!(
                (left - right).abs() < 0.01,
                "{width}x{height}: {left} {right}"
            );
            assert!(left >= expected - 0.01, "{width}x{height}: {left}");
        }

        // The glass and its label have the same air on every horizontal side
        // once the settled scene has removed entrance drift.
        let mut guide = Guide::default();
        guide.open();
        guide.backdate_open(2.0);
        let scene = guide_scene(&guide, Some("Celeste"), None, &[], None);
        let items = guide.items(false);
        let resume = items
            .iter()
            .position(|item| *item == Item::Resume)
            .expect("Resume is always present");
        let [x, _, w, _] = menu_item_rect(&items, resume, 1920.0, 1080.0);
        let label = scene
            .texts
            .iter()
            .find(|text| text.content == "Resume")
            .expect("Resume should label its button");
        let [panel_x, _, _, _] = sidebar_panel_rect(1920.0, 1080.0);
        let outside = x - panel_x;
        let inside_left = label.x - x;
        let inside_right = x + w - (label.x + label.max_width);
        assert!((outside - inside_left).abs() < 0.01);
        assert!((outside - inside_right).abs() < 0.01);
    }

    /// A separator belongs equally to the bands on both sides of it. Measure
    /// from the visible glass edges rather than from either line's layout box,
    /// because tiles, bars and ordinary buttons have different inner padding.
    #[test]
    fn separators_have_equal_padding_above_and_below() {
        let plain = Guide::default();
        let mut bars = Guide::default();
        bars.set_bars(BOTH_BARS);
        let mut pointer = Guide::default();
        pointer.set_pointer_control(true);
        let mut fullest = Guide::default();
        fullest.set_pointer_control(true);
        fullest.set_bars(BOTH_BARS);

        let cases = [
            plain.items(false),
            plain.items(true),
            bars.items(false),
            bars.items(true),
            pointer.items(true),
            fullest.items(true),
        ];
        for [width, height] in [
            [960.0, 600.0],
            [1000.0, 742.0],
            [1280.0, 720.0],
            [1920.0, 1080.0],
            [3440.0, 1440.0],
        ] {
            for items in &cases {
                let rows = separator_rows(items);
                let rules = menu_separator_rects(items, width, height);
                assert_eq!(rules.len(), rows.len());

                for (row, rule) in rows.into_iter().zip(rules) {
                    let above = (0..row)
                        .rev()
                        .find(|index| items[*index] != Item::Power)
                        .expect("a separator must have a control above it");
                    let upper = menu_item_rect(items, above, width, height);
                    let lower = menu_item_rect(items, row, width, height);
                    let above_gap = rule[1] - (upper[1] + upper[3]);
                    let below_gap = lower[1] - (rule[1] + rule[3]);
                    assert!(above_gap > 0.0 && below_gap > 0.0);
                    assert!(
                        (above_gap - below_gap).abs() < 0.01,
                        "{width}x{height}: {above_gap} above, {below_gap} below"
                    );
                }
            }
        }
    }

    /// The tiles at the head of the column are square, side by side on one
    /// line, and centred on the panel rather than inset at its left edge like
    /// the rows under them.
    ///
    /// Centred because the run is shorter than the panel and how much shorter
    /// depends on the session: a machine with no pointer control carries three
    /// tiles where this one carries four. Left-aligned, that difference lands
    /// entirely on the right-hand side, and the line reads as a rank of
    /// buttons that stopped early rather than as a line of its own.
    #[test]
    fn the_tiles_sit_side_by_side_at_the_head_of_the_column() {
        let mut guide = Guide::default();
        guide.set_pointer_control(true);
        guide.set_bars(BOTH_BARS);
        let items = guide.items(true);

        let pointer = menu_item_rect(&items, 0, 1920.0, 1080.0);
        let mixer = menu_item_rect(&items, 1, 1920.0, 1080.0);
        let moon = menu_item_rect(&items, 2, 1920.0, 1080.0);
        let bell = menu_item_rect(&items, 3, 1920.0, 1080.0);
        assert_eq!(items[0], Item::Pointer);
        assert_eq!(items[1], Item::Mixer);
        assert_eq!(items[2], Item::DoNotDisturb);
        assert_eq!(items[3], Item::Notifications);

        // Square, all of them, and the same square.
        assert!((pointer[2] - pointer[3]).abs() < 0.01, "{pointer:?}");
        for tile in [mixer, moon, bell] {
            assert_eq!([pointer[2], pointer[3]], [tile[2], tile[3]]);
            // Beside one another on one line.
            assert_eq!(pointer[1], tile[1]);
        }
        // With air between, and evenly spaced: one gap, used three times,
        // rather than a row that drifts.
        for pair in [[pointer, mixer], [mixer, moon], [moon, bell]] {
            assert!(pair[1][0] > pair[0][0] + pair[0][2], "{pair:?}");
            assert!(((pair[1][0] - pair[0][0]) - (mixer[0] - pointer[0])).abs() < 0.01);
        }

        // Centred on the row below rather than sharing its left edge: the same
        // air at both ends of the line, and more of it than the rows keep.
        let volume = menu_item_rect(&items, 4, 1920.0, 1080.0);
        let left = pointer[0] - volume[0];
        let right = (volume[0] + volume[2]) - (bell[0] + bell[2]);
        assert!(
            (left - right).abs() < 0.01,
            "{left} at one end, {right} at the other"
        );
        assert!(left > 0.0, "the line is shorter than the rows under it");
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
                    do_not_disturb: false,
                    unread: 0.0,
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

    /// The second switch on the line says which state it is in the same way
    /// the first does — by being filled — and, unlike the first, it says it on
    /// a machine with nothing running.
    ///
    /// That is the whole difference between the two switches and it is worth a
    /// test of its own: the pointer tile is about the application in front, so
    /// with none it is drawn inert; this one is about the session, which is
    /// there whether or not anything is.
    #[test]
    fn the_do_not_disturb_tile_is_a_switch_the_empty_session_can_still_throw() {
        let scene = |quiet: bool| {
            let mut guide = Guide::default();
            // No pointer control and nothing in front: the barest session
            // there is, and the switch still has to work on it.
            guide.open();
            guide.backdate_open(2.0);
            build_guide(
                GuideView {
                    guide: &guide,
                    clock: None,
                    volume: None,
                    brightness: None,
                    stick_pointer: false,
                    do_not_disturb: quiet,
                    unread: 0.0,
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

        let items = Guide::default().items(false);
        let index = items
            .iter()
            .position(|item| *item == Item::DoNotDisturb)
            .expect("the tile is on every session");
        let tile = menu_item_rect(&items, index, 1920.0, 1080.0);
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
                .filter(|q| q.slot == Named::slot_of(icons::DO_NOT_DISTURB))
                .find(in_tile)
                .copied()
                .expect("the moon on its own tile")
        };
        let panes = |scene: &Scene| {
            scene
                .quads
                .iter()
                .filter(|q| q.slot == SOLID_SLOT && q.thickness > 0.0)
                .filter(in_tile)
                .count()
        };

        assert_eq!(panes(&scene(true)), 2, "on: the fill, over the chip");
        assert_eq!(panes(&scene(false)), 1, "off: the chip alone");
        assert!(glyph(&scene(true)).color[3] > glyph(&scene(false)).color[3]);
        // Never the hairline the pointer tile is drawn as when it has nothing
        // to act on: there is always a session to quieten.
        assert!(
            !scene(false)
                .quads
                .iter()
                .filter(|q| q.slot == SOLID_SLOT && q.border > 0.0)
                .any(|q| in_tile(&q)),
            "the switch is never drawn out of reach"
        );
    }

    /// The unread mark: a bead in the bell's own corner, on no other tile, and
    /// still there when the highlight is sitting on the tile it marks.
    ///
    /// That last part is the whole of why it is white rather than accent
    /// coloured. The moment the user walks the selection onto the bell — which
    /// is exactly what the mark is telling them to do — the chip under it fills
    /// with the accent, and a mark tinted with the accent would choose that
    /// moment to disappear.
    #[test]
    fn the_unread_mark_sits_in_the_bells_corner_and_survives_the_selection() {
        let scene = |unread: f32, on_the_bell: bool| {
            let mut guide = Guide::default();
            guide.open();
            guide.backdate_open(2.0);
            if on_the_bell {
                // Up from Resume to the tile line, then along it to the bell.
                guide.move_selection(-1, false);
                while guide.selected_item(false) != Some(Item::Notifications) {
                    assert!(guide.move_in_line(1, false), "the bell is on this line");
                }
            }
            build_guide(
                GuideView {
                    guide: &guide,
                    clock: None,
                    volume: None,
                    brightness: None,
                    stick_pointer: false,
                    do_not_disturb: false,
                    unread,
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

        let items = Guide::default().items(false);
        let corner = |item: Item| {
            let index = items.iter().position(|held| *held == item).unwrap();
            let [x, y, w, h] = menu_item_rect(&items, index, 1920.0, 1080.0);
            // The quadrant the mark lives in, and nothing else does.
            [x + w * 0.5, y, w * 0.5, h * 0.5]
        };
        let beads = |scene: &Scene, item: Item| {
            let [cx, cy, cw, ch] = corner(item);
            scene
                .quads
                .iter()
                .filter(|q| {
                    q.slot == SOLID_SLOT
                        && q.radius > 0.0
                        && (q.radius - q.w * 0.5).abs() < 0.01
                        && q.x >= cx
                        && q.y >= cy
                        && q.x + q.w <= cx + cw + 0.01
                        && q.y + q.h <= cy + ch + 0.01
                })
                .count()
        };

        // Nothing unread, nothing drawn — on any tile.
        for item in [Item::Mixer, Item::DoNotDisturb, Item::Notifications] {
            assert_eq!(beads(&scene(0.0, false), item), 0, "{item:?}");
        }

        // Unread: the bead and the moat it stands in, on the bell alone. The
        // other tiles hold nothing that can be missed.
        assert_eq!(beads(&scene(1.0, false), Item::Notifications), 2);
        assert_eq!(beads(&scene(1.0, false), Item::Mixer), 0);
        assert_eq!(beads(&scene(1.0, false), Item::DoNotDisturb), 0);

        // And it is still there under the selection, at the same size.
        let lit = scene(1.0, true);
        assert_eq!(beads(&lit, Item::Notifications), 2);
        let widest = |scene: &Scene| {
            let [cx, cy, cw, ch] = corner(Item::Notifications);
            scene
                .quads
                .iter()
                .filter(|q| {
                    q.slot == SOLID_SLOT
                        && q.radius > 0.0
                        && (q.radius - q.w * 0.5).abs() < 0.01
                        && q.x >= cx
                        && q.y >= cy
                        && q.x + q.w <= cx + cw + 0.01
                        && q.y + q.h <= cy + ch + 0.01
                })
                .map(|q| q.w)
                .fold(0.0f32, f32::max)
        };
        assert!((widest(&lit) - widest(&scene(1.0, false))).abs() < 0.01);

        // Part-way on, it is smaller and fainter rather than half-drawn.
        let growing = widest(&scene(0.4, false));
        assert!(growing > 0.0 && growing < widest(&scene(1.0, false)));
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

    /// The mixer tile carries a glyph and never a label: it is the one tile
    /// that opens a panel rather than turning something over, and what it opens
    /// is what says which applications are playing.
    #[test]
    fn the_mixer_tile_is_drawn_as_a_glyph_alone() {
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
                    do_not_disturb: false,
                    unread: 0.0,
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
            "Exit LineXinBar Shell",
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
                    do_not_disturb: false,
                    unread: 0.0,
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
            clip: None,
            halo: 0.0,
            lines: 1,
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
            clip: None,
            halo: 0.0,
            lines: 1,
        };

        let mut scene = Scene {
            quads: Vec::new(),
            texts: vec![under, beside],
        };
        recede_behind_power_dialog(&mut scene, 1920.0, 1080.0, rows, 1.0);

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
        let scene = guide_scene(&guide, Some("Celeste"), Some(TEST_SCREEN), &[], None);

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
                do_not_disturb: false,
                unread: 0.0,
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

    /// The sidebar keeps one refractive surface over the wallpaper, and it has
    /// to bend the wallpaper *as the layer below is drawing it*. Its broad
    /// face is deliberately clearer than a modal panel and optically curved;
    /// otherwise that much heavily frosted, level glass becomes a flat block.
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
                do_not_disturb: false,
                unread: 0.0,
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
            .find(|q| (q.w - pw).abs() < 1.0 && (q.h - ph).abs() < 1.0 && q.thickness > 0.0)
            .expect("the sidebar should have one live pane");
        assert!(
            panel.thickness > 0.0,
            "and it should be a slab, not a rectangle"
        );
        assert!(
            panel.frost > 0.0 && panel.frost < FROST_PANEL,
            "clearer than modal glass, while retaining enough frost for labels"
        );
        assert_eq!(panel.behind, 0.8, "bending the wallpaper it is actually on");
        assert!(panel.gloss > 0.0, "with a lit edge");
        assert!(
            panel.face_curve > 0.0,
            "and a broad reflection over the face, not only its rim"
        );

        // Floating, not pinned: glass only reads as a layer if what it is laid
        // over runs past it.
        assert!(px > 0.0 && py > 0.0);
        assert!(px + pw < overview::sidebar_width(1920.0) as f32);
        assert!(py + ph < 1080.0);
    }

    /// The atmosphere belongs below the glass, follows the active accent, and
    /// never becomes another refractive pane stacked over the first one.
    #[test]
    fn the_sidebar_layers_accent_light_under_one_glass_surface() {
        crate::theme::with_accent("Green", || {
            let rect = sidebar_panel_rect(1920.0, 1080.0);
            let [header, foot, panel, rim] = sidebar_surface(rect, guide_scale(1080.0), 0.8, 0.7);
            let active = theme();

            assert_eq!(header.slot, GLOW_SLOT);
            assert_eq!(foot.slot, GLOW_SLOT);
            assert_eq!(
                [header.color[0], header.color[1], header.color[2]],
                active.accent_soft.rgb()
            );
            assert_eq!(
                [foot.color[0], foot.color[1], foot.color[2]],
                active.accent.rgb()
            );
            assert_eq!(header.color[3], SIDEBAR_HEADER_LIGHT);
            assert_eq!(foot.color[3], SIDEBAR_FOOT_LIGHT);
            assert_eq!(header.fade, 0.7);
            assert_eq!(foot.fade, 0.7);
            assert_eq!(header.thickness, 0.0);
            assert_eq!(foot.thickness, 0.0);
            assert_eq!(header.gloss, 0.0);
            assert_eq!(foot.gloss, 0.0);

            assert_eq!(panel.behind, 0.8);
            assert!(panel.thickness > 0.0);
            assert!(panel.face_curve > 0.0);
            assert!(rim.border > 0.0);
            assert_eq!(rim.thickness, 0.0);
            assert_eq!(
                [rim.color[0], rim.color[1], rim.color[2]],
                active.accent_soft.rgb()
            );
            assert_eq!(
                [header, foot, panel, rim]
                    .iter()
                    .filter(|quad| quad.thickness > 0.0)
                    .count(),
                1,
                "decorative layers must not stack more glass over the panel"
            );
        });
    }

    /// The soft atlas glows are not clipped as rounded shapes, so their own
    /// transparent edges have to remain wholly inside the sidebar at every
    /// display size the layout is exercised with.
    #[test]
    fn the_sidebar_lighting_stays_inside_the_panel() {
        for [width, height] in [
            [960.0, 600.0],
            [1280.0, 720.0],
            [1920.0, 1080.0],
            [3440.0, 1440.0],
        ] {
            let [x, y, w, h] = sidebar_panel_rect(width, height);
            let surface = sidebar_surface([x, y, w, h], guide_scale(height), 0.8, 1.0);
            for light in &surface[..2] {
                assert!(light.w > 0.0 && light.h > 0.0);
                assert!(light.x >= x && light.y >= y, "{width}x{height}: {light:?}");
                assert!(
                    light.x + light.w <= x + w && light.y + light.h <= y + h,
                    "{width}x{height}: {light:?}"
                );
            }
        }
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
            entries: vec![app("first"), app("second"), app("third")],
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

    /// The map gets out of the way of the column it opens.
    ///
    /// Stepping into a subcategory slides a new column up over the category
    /// row. The row used to give up its ink at the pace of that slide, which
    /// left it at a third of its strength while the column arriving over it
    /// was at a third of its own — two things printed over one another across
    /// the middle of the move, and neither of them readable while it lasted.
    /// A shelf is the worst of it: the pictures are wide, they cross the whole
    /// row, and what they landed on was a rank of half-lit icons.
    #[test]
    fn the_category_row_goes_before_the_column_it_opens_arrives() {
        let xmb = Xmb::new(vec![
            Category {
                id: "media",
                title: "Multimedia",
                icon: "media",
                entries: vec![folder("Video", vec![app("first"), app("second")])],
            },
            Category {
                id: "apps",
                title: "Apps",
                icon: "apps",
                entries: vec![app("Files")],
            },
        ]);
        let (width, height) = (1920.0, 1080.0);
        let cross_y = height * BAR_CROSS_Y;

        // What is left of the categories the path did not open — the map,
        // which is the part that has to go. The one it did open is the head of
        // the trail and stays: its icon is the larger of the two on the row.
        let map_ink = |scene: &Scene| {
            let mut row: Vec<&Quad> = scene
                .quads
                .iter()
                .filter(|quad| is_icon(quad) && ((quad.y + quad.h / 2.0) - cross_y).abs() < 1.0)
                .collect();
            row.sort_by(|a, b| b.w.total_cmp(&a.w));
            row.iter().skip(1).map(|q| q.color[3]).fold(0.0, f32::max)
        };
        // And how far up the column that opened has come, read off the row it
        // opened on.
        let column_ink = |scene: &Scene| {
            scene
                .texts
                .iter()
                .find(|text| text.content == "first")
                .map_or(0.0, |text| text.color[3])
        };

        let mut cursor = Cursor::new(xmb.categories.len());
        assert!(cursor.enter(&xmb), "the row opens a column of its own");
        let mut halfway = None;
        loop {
            let scene = build_with(&xmb, &cursor, width, height, true, &AllSlots);
            let (map, column) = (map_ink(&scene), column_ink(&scene));
            let inside = cursor.depth_position();

            // Gone outright, and with the slide still running: the same ramp a
            // column being stepped past leaves on, and the same reason for it.
            if inside >= 1.0 - COLUMN_GONE_BY {
                assert_eq!(map, 0.0, "the row still has ink at {inside} of the way in");
            }
            // And gone by the time there is anything to read over it.
            if column >= 0.5 && halfway.is_none() {
                halfway = Some((inside, map));
            }
            if !cursor.animate(1.0 / 60.0) {
                break;
            }
        }

        let (inside, map) = halfway.expect("the column has to arrive");
        assert!(
            map < 0.1,
            "the column was half up at {inside} in with {map} of the map still over it",
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
            entries: vec![app("first")],
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
                    do_not_disturb: false,
                    unread: 0.0,
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
            .filter(|q| {
                q.border > 0.0
                    && q.x < x - 6.0
                    && q.y < y - 6.0
                    && q.x + q.w > x + w
                    && q.y + q.h > y + h
            })
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
                .filter(|q| {
                    q.border > 0.0
                        && q.x < x - 6.0
                        && q.y < y - 6.0
                        && q.x + q.w > x + w
                        && q.y + q.h > y + h
                })
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
                    do_not_disturb: false,
                    unread: 0.0,
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
            entries: vec![app("Celeste")],
        }]);
        let cursor = Cursor::new(1);
        let full = focused(&xmb, 1920.0, 1080.0, &AllSlots);
        // Everything the display puts a pixel of on screen — including the
        // ones it only puts part of, which the card keeps that same part of.
        let on_screen: Vec<&Quad> = full
            .quads
            .iter()
            .filter(|q| q.x < 1920.0 && q.y < 1080.0 && q.x + q.w > 0.0 && q.y + q.h > 0.0)
            .collect();
        assert!(on_screen.len() > 1, "the bar should have icons to shrink");

        let mut mini = build(
            &xmb, &cursor, 1920.0, 1080.0, true, None, 0.0, &AllSlots, false,
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
        // And every pixel of it lands inside the card, so nothing pokes out
        // past the frame drawn around it: a pane may reach beyond the card, as
        // one reaching past the display's edge does, but only inside the cut it
        // now carries.
        for quad in &mini.quads {
            let ink = match quad.clip {
                Some(clip) => intersection([quad.x, quad.y, quad.w, quad.h], clip),
                None => [quad.x, quad.y, quad.w, quad.h],
            };
            assert!(ink[0] >= card[0] - 0.5 && ink[0] + ink[2] <= card[0] + card[2] + 0.5);
            assert!(ink[1] >= card[1] - 0.5 && ink[1] + ink[3] <= card[1] + card[3] + 0.5);
        }
        for text in &mini.texts {
            assert!(text.x >= card[0] - 0.5 && text.y >= card[1] - 0.5);
        }
    }

    /// A miniature of a display shows what the display shows, and only that.
    ///
    /// The bar walks off its own left edge as a path opens: the category row
    /// goes with the slide, and every category's name is centred on a box
    /// wider than the button it sits under, so the name of the one the path
    /// hangs off reaches past the edge before the button does. On a display
    /// narrower than 16:9 the button goes past it too. The screen cuts all of
    /// that at its edge and thinks no more about it; a card has no edge of its
    /// own to cut against, and the run was drawn whole — the word "Multimedia"
    /// printed on the wallpaper *beside* the guide's card, at the size and
    /// place a screen a fifth off to the left would have put it.
    #[test]
    fn a_card_draws_nothing_the_display_it_pictures_would_have_cut() {
        let xmb = Xmb::new(vec![
            Category {
                id: "media",
                title: "Multimedia",
                icon: "media",
                entries: vec![folder("Video", vec![app("first"), app("second")])],
            },
            Category {
                id: "apps",
                title: "Apps",
                icon: "apps",
                entries: vec![app("Files")],
            },
        ]);
        // Standing inside the subcategory, where the row has been carried off
        // the edge — the state the guide is opened over.
        let mut cursor = Cursor::new(xmb.categories.len());
        assert!(cursor.enter(&xmb), "the row opens a column of its own");
        settle(&mut cursor);

        // Whether any of the screens tried actually cuts the run — a wide one
        // has room for the whole box, and a test where nothing is cut proves
        // nothing at all.
        let mut cut = false;
        for [width, height] in SCREENS {
            let mut mini = build_with(&xmb, &cursor, width, height, true, &AllSlots);
            // A card at the display's own shape, as the overview lays one out.
            let card = [width * 0.5, height * 0.2, width * 0.4, height * 0.4];
            mini.place_into(card, width, height);

            let name = mini
                .texts
                .iter()
                .find(|text| text.content == "Multimedia")
                .expect("the head of the trail is named");
            cut |= name.x < card[0];

            for text in &mini.texts {
                // Where the run may put pixels: its own box, cut down by
                // whatever it is clipped to — the same pair the renderer hands
                // glyphon, and the only thing that decides where ink lands.
                let box_of = [text.x, text.y, text.max_width, text.size * 2.0];
                let ink = match text.clip {
                    Some(clip) => intersection(box_of, clip),
                    None => box_of,
                };
                if ink[2] <= 0.0 || ink[3] <= 0.0 {
                    continue;
                }
                assert!(
                    ink[0] >= card[0] - 0.5
                        && ink[1] >= card[1] - 0.5
                        && ink[0] + ink[2] <= card[0] + card[2] + 0.5
                        && ink[1] + ink[3] <= card[1] + card[3] + 0.5,
                    "{width}x{height}: {:?} draws at {ink:?}, outside a card at {card:?}",
                    text.content,
                );
            }
        }
        assert!(cut, "no screen here put a run past its own edge to cut");
    }

    /// And it draws everything the display *does* show, half an icon included.
    ///
    /// The other half of the same rule, and the bug the user reported: an icon
    /// the path had carried partly off the left edge was thrown away outright
    /// on the way into the card. Press the guide from inside a subcategory and
    /// the category's own mark and the one on the trail beside it simply went
    /// out — from a card that is supposed to be a picture of the screen it was
    /// pressed on — and came back whole when the guide closed. Nothing in this
    /// shell may vanish at the start of a transition and reappear at the end of
    /// it. A pane that ran off the edge is cut to the edge, here as there.
    #[test]
    fn a_card_keeps_the_half_of_an_icon_the_display_still_shows() {
        let xmb = Xmb::new(vec![
            Category {
                id: "media",
                title: "Multimedia",
                icon: "media",
                entries: vec![folder("Video", vec![app("first"), app("second")])],
            },
            Category {
                id: "apps",
                title: "Apps",
                icon: "apps",
                entries: vec![app("Files")],
            },
        ]);
        let mut cursor = Cursor::new(xmb.categories.len());
        assert!(cursor.enter(&xmb), "the row opens a column of its own");
        settle(&mut cursor);

        let mut straddled = false;
        for [width, height] in SCREENS {
            let full = build_with(&xmb, &cursor, width, height, true, &AllSlots);
            // The panes the display cuts rather than hides: part on the screen
            // and part past its edge.
            let hanging: Vec<&Quad> = full
                .quads
                .iter()
                .filter(|q| {
                    let ink = intersection([q.x, q.y, q.w, q.h], [0.0, 0.0, width, height]);
                    let shown = ink[2] > 0.0 && ink[3] > 0.0;
                    shown && (ink[2] < q.w - 0.5 || ink[3] < q.h - 0.5)
                })
                .collect();
            if hanging.is_empty() {
                continue;
            }
            straddled = true;

            let mut mini = build_with(&xmb, &cursor, width, height, true, &AllSlots);
            let card = [width * 0.5, height * 0.2, width * 0.4, height * 0.4];
            mini.place_into(card, width, height);
            let scale = card[2] / width;

            for hung in hanging {
                let small = mini
                    .quads
                    .iter()
                    .find(|q| {
                        (q.x - (card[0] + hung.x * scale)).abs() < 0.5
                            && (q.y - (card[1] + hung.y * scale)).abs() < 0.5
                            && (q.w - hung.w * scale).abs() < 0.5
                    })
                    .unwrap_or_else(|| {
                        panic!(
                            "{width}x{height}: a pane at {}x{} was dropped rather than cut",
                            hung.x, hung.y
                        )
                    });
                // Cut to where the display's own edge was, once that edge has
                // travelled into the card with everything else.
                let clip = small.clip.expect("a cut pane carries its cut");
                let ink = intersection([small.x, small.y, small.w, small.h], clip);
                assert!(
                    ink[2] > 0.0 && ink[3] > 0.0,
                    "{width}x{height}: the half of the pane the screen shows is cut away too",
                );
                assert!(
                    ink[0] >= card[0] - 0.5
                        && ink[1] >= card[1] - 0.5
                        && ink[0] + ink[2] <= card[0] + card[2] + 0.5
                        && ink[1] + ink[3] <= card[1] + card[3] + 0.5,
                    "{width}x{height}: it draws at {ink:?}, outside a card at {card:?}",
                );
            }
        }
        assert!(
            straddled,
            "no screen here put a pane across its own edge to cut"
        );
    }

    /// The bar the panels of two categories, for the depth tests: enough of a
    /// cross that there is something out at the edges to watch draw inward.
    fn cross() -> Xmb {
        Xmb::new(vec![
            Category {
                id: "games",
                title: "Games",
                icon: "games",
                entries: vec![app("Celeste"), app("Hades")],
            },
            Category {
                id: "apps",
                title: "Apps",
                icon: "apps",
                entries: vec![app("Files")],
            },
        ])
    }

    /// A panel standing over the start screen pushes the whole of it back —
    /// and it goes back *around the tile the panel came out of*, which is the
    /// one thing that does not move. A menu grows out of that tile for a fifth
    /// of a second; a tile that slid away underneath it would be a menu coming
    /// out of nothing.
    #[test]
    fn the_start_screen_steps_back_around_the_tile_a_menu_opens_from() {
        for [width, height] in SCREENS {
            let xmb = cross();
            let flat = focused(&xmb, width, height, &AllSlots);
            let mut back = focused(&xmb, width, height, &AllSlots);
            recede_into_depth(&mut back, width, height, 1.0);

            let [ax, ay, aw, ah] = launch_origin(width, height);
            let (cx, cy) = (ax + aw * 0.5, ay + ah * 0.5);
            let factor = 1.0 - CONTEXT_DEPTH;

            assert_eq!(
                back.quads.len(),
                flat.quads.len(),
                "stepping back drops nothing"
            );
            let mut moved = 0;
            for (pushed, still) in back.quads.iter().zip(&flat.quads) {
                assert!((pushed.w - still.w * factor).abs() < 1e-3);
                assert!((pushed.h - still.h * factor).abs() < 1e-3);
                // Glass depth is a length like any other. A slab that kept its
                // thickness on a scene that moved away would be a bevel that
                // grew as the screen shrank.
                assert!((pushed.thickness - still.thickness * factor).abs() < 1e-3);
                assert!((pushed.x - (cx + (still.x - cx) * factor)).abs() < 1e-3);
                assert!((pushed.y - (cy + (still.y - cy) * factor)).abs() < 1e-3);
                if (pushed.x - still.x).abs() > 1.0 {
                    moved += 1;
                }
            }
            assert!(moved > 0, "the bar should have something to push back");
            for (pushed, still) in back.texts.iter().zip(&flat.texts) {
                assert!((pushed.size - still.size * factor).abs() < 1e-3);
                assert!((pushed.x - (cx + (still.x - cx) * factor)).abs() < 1e-3);
            }

            // And the rectangle it all draws towards is a tile that is really
            // there: the focused entry's own disc, which is what `launch_origin`
            // hands a menu as its anchor.
            let tile = flat
                .quads
                .iter()
                .position(|quad| {
                    (quad.x - ax).abs() < 0.5
                        && (quad.y - ay).abs() < 0.5
                        && (quad.w - aw).abs() < 0.5
                        && (quad.h - ah).abs() < 0.5
                })
                .expect("the focused entry stands on its anchor");
            let disc = &back.quads[tile];
            assert!((disc.x + disc.w * 0.5 - cx).abs() < 1e-3);
            assert!((disc.y + disc.h * 0.5 - cy).abs() < 1e-3);
        }
    }

    /// With nothing over it the screen is where it always was — not scaled by
    /// one, which would put every coordinate through a multiply for nothing
    /// and leave the resting bar to the mercy of rounding.
    #[test]
    fn a_start_screen_with_nothing_over_it_is_left_exactly_where_it_is() {
        let xmb = cross();
        let flat = focused(&xmb, 1920.0, 1080.0, &AllSlots);
        let mut untouched = focused(&xmb, 1920.0, 1080.0, &AllSlots);
        recede_into_depth(&mut untouched, 1920.0, 1080.0, 0.0);
        for (quad, want) in untouched.quads.iter().zip(&flat.quads) {
            assert_eq!(
                [quad.x, quad.y, quad.w, quad.h],
                [want.x, want.y, want.w, want.h]
            );
        }
        for (text, want) in untouched.texts.iter().zip(&flat.texts) {
            assert_eq!([text.x, text.y, text.size], [want.x, want.y, want.size]);
        }
    }

    /// The session opens on a display with nothing of the shell on it. The bar
    /// is as far back as the shell's depth goes and has no ink at all, which
    /// between them is invisible — and it is the *whole* bar, categories and
    /// the column hanging off them alike, because a category row arriving over
    /// a list that was already there would read as half a screen.
    #[test]
    fn the_start_screen_starts_at_the_back_of_the_shells_own_depth() {
        for [width, height] in SCREENS {
            let xmb = cross();
            let flat = focused(&xmb, width, height, &AllSlots);
            let mut far = focused(&xmb, width, height, &AllSlots);
            arrive_from_depth(&mut far, width, height, 0.0);

            assert_eq!(far.quads.len(), flat.quads.len(), "arriving drops nothing");
            assert_eq!(far.texts.len(), flat.texts.len());
            for quad in &far.quads {
                assert_eq!(quad.fade, 0.0, "{width}x{height}: a pane is showing");
            }
            for text in &far.texts {
                assert_eq!(
                    text.color[3], 0.0,
                    "{width}x{height}: {:?} is showing",
                    text.content
                );
            }

            // And it stands where a column four deep would stand: smaller by
            // the shell's own recession, and gathered in towards the middle of
            // the display — the axis the viewer is looking down, which is what
            // makes it read as distance rather than as a bar sliding.
            let (near, _) = receded(ARRIVAL_STEPS);
            let (cx, cy) = (width * 0.5, height * 0.5);
            let mut gathered = 0;
            for (there, here) in far.quads.iter().zip(&flat.quads) {
                assert!((there.w - here.w * near).abs() < 1e-3);
                assert!((there.h - here.h * near).abs() < 1e-3);
                // Glass depth travels with everything else, for the reason it
                // does on the way back — see the step-back test above.
                assert!((there.thickness - here.thickness * near).abs() < 1e-3);
                assert!((there.x - (cx + (here.x - cx) * near)).abs() < 1e-3);
                assert!((there.y - (cy + (here.y - cy) * near)).abs() < 1e-3);
                if (there.x - here.x).abs() > 1.0 {
                    gathered += 1;
                }
            }
            assert!(
                gathered > 0,
                "{width}x{height}: nothing had any way to come"
            );
            for (there, here) in far.texts.iter().zip(&flat.texts) {
                assert!((there.size - here.size * near).abs() < 1e-3);
                assert!((there.x - (cx + (here.x - cx) * near)).abs() < 1e-3);
                assert!((there.y - (cy + (here.y - cy) * near)).abs() < 1e-3);
            }
        }
    }

    /// It comes the whole way forward and stops: an arrived screen is the bar
    /// as it would have been drawn had nothing animated at all, exactly — not
    /// scaled by one, for the reason a screen with nothing over it is left
    /// alone rather than pushed back by nothing.
    #[test]
    fn an_arrived_start_screen_is_left_exactly_where_it_is() {
        let xmb = cross();
        let flat = focused(&xmb, 1920.0, 1080.0, &AllSlots);
        let mut landed = focused(&xmb, 1920.0, 1080.0, &AllSlots);
        arrive_from_depth(&mut landed, 1920.0, 1080.0, 1.0);
        for (quad, want) in landed.quads.iter().zip(&flat.quads) {
            assert_eq!(
                [quad.x, quad.y, quad.w, quad.h, quad.fade],
                [want.x, want.y, want.w, want.h, want.fade]
            );
        }
        for (text, want) in landed.texts.iter().zip(&flat.texts) {
            assert_eq!(
                [text.x, text.y, text.size, text.color[3]],
                [want.x, want.y, want.size, want.color[3]]
            );
        }
    }

    /// One journey forward, never a step back — and the ink leads the
    /// movement, so the screen is fully lit while it is still travelling
    /// rather than still thickening as it settles.
    #[test]
    fn the_arrival_only_ever_comes_closer_and_is_lit_before_it_lands() {
        let xmb = cross();
        let (width, height) = (1920.0, 1080.0);
        let flat = focused(&xmb, width, height, &AllSlots);
        // The widest pane on the bar, which is the one with the most room to
        // grow: a category's own glass.
        let widest = |scene: &Scene| scene.quads.iter().map(|quad| quad.w).fold(0.0f32, f32::max);
        let lit = |scene: &Scene| {
            scene
                .quads
                .iter()
                .map(|quad| quad.fade)
                .fold(0.0f32, f32::max)
        };

        let mut last = (0.0f32, 0.0f32);
        for step in 0..=20 {
            let arrival = step as f32 / 20.0;
            let mut scene = focused(&xmb, width, height, &AllSlots);
            arrive_from_depth(&mut scene, width, height, arrival);
            let (size, ink) = (widest(&scene), lit(&scene));
            assert!(size >= last.0, "the screen went back at {arrival}");
            assert!(ink >= last.1, "the screen dimmed at {arrival}");
            assert!(size <= widest(&flat) + 1e-3, "it overshot at {arrival}");
            last = (size, ink);
        }
        assert!((last.0 - widest(&flat)).abs() < 1e-3, "it never landed");

        // Fully lit with the last of the travelling still to do.
        let mut early = focused(&xmb, width, height, &AllSlots);
        arrive_from_depth(&mut early, width, height, ARRIVAL_LIT_BY);
        assert!((lit(&early) - lit(&flat)).abs() < 1e-3);
        assert!(widest(&early) < widest(&flat), "and still on its way");
    }

    /// The arrival leaves the distance at rest and settles onto the mark at
    /// rest, quickest through the middle, and is the same shape read from
    /// either end — the shell's own ramp, which this deliberately does not have
    /// a ramp of its own instead of.
    #[test]
    fn the_arrival_runs_on_the_shells_own_symmetric_ramp() {
        let (width, height) = (1920.0, 1080.0);
        let xmb = cross();
        let settled = focused(&xmb, width, height, &AllSlots);
        let widest = |scene: &Scene| scene.quads.iter().map(|quad| quad.w).fold(0.0f32, f32::max);
        // How much of the journey is behind it, read off the drawing rather
        // than off the ramp — and read in *steps of depth*, which is what the
        // journey is measured in. The size those steps are drawn at is a
        // perspective curve laid over the ramp: a screen crossing the last step
        // swells far more than one crossing the first, exactly as an approach
        // at a steady speed does. Mistaking that swelling for the ramp is how
        // an even move gets read as an accelerating one.
        let travelled = |arrival: f32| {
            let mut scene = focused(&xmb, width, height, &AllSlots);
            arrive_from_depth(&mut scene, width, height, arrival);
            let near = widest(&scene) / widest(&settled);
            let steps = near.ln() / DEPTH_SHRINK.ln();
            1.0 - steps / ARRIVAL_STEPS
        };

        // Halfway through the clock is halfway home: the mark of a ramp with
        // no lean either way.
        assert!(
            (travelled(0.5) - 0.5).abs() < 0.02,
            "half the clock had it {} of the way there",
            travelled(0.5)
        );
        // And what it has done by any point, it has left to do by the mirror of
        // that point.
        for step in 1..10 {
            let t = step as f32 / 10.0;
            assert!(
                (travelled(t) + travelled(1.0 - t) - 1.0).abs() < 0.02,
                "{t} and its mirror are not one journey: {} and {}",
                travelled(t),
                travelled(1.0 - t)
            );
        }

        // Nothing bolts off the mark, and nothing arrives still at speed: a
        // tenth of the clock at either end covers almost none of the ground.
        assert!(
            travelled(0.1) < 0.05,
            "it left at a bolt: {}",
            travelled(0.1)
        );
        assert!(
            travelled(0.9) > 0.95,
            "it was still travelling: {}",
            travelled(0.9)
        );
        // The quickest part is the middle, by some way.
        let middle = travelled(0.6) - travelled(0.4);
        assert!(middle > (travelled(0.1) - travelled(0.0)) * 3.0);
        assert!(middle > (travelled(1.0) - travelled(0.9)) * 3.0);
    }

    /// A click that lands while the screen is still arriving is about what the
    /// user can see, not about where that button will end up: the point is
    /// carried back through the arrival before the settled bar is asked.
    ///
    /// Taken off the drawn scene rather than worked out from the ramp, so it is
    /// the picture the user is pressing on that is being tested and not this
    /// test's own copy of the arithmetic.
    #[test]
    fn a_click_on_an_arriving_bar_is_about_where_it_looks() {
        for [width, height] in SCREENS {
            let xmb = cross();
            let cursor = Cursor::new(xmb.categories.len());

            let flat = focused(&xmb, width, height, &AllSlots);
            // The second category's own glass, where the settled bar puts it.
            let settled = (
                bar_category_x(1.0, 0.0, width, height),
                height * BAR_CROSS_Y,
            );
            let button = flat
                .quads
                .iter()
                .position(|quad| {
                    (quad.x + quad.w * 0.5 - settled.0).abs() < 0.5
                        && (quad.y + quad.h * 0.5 - settled.1).abs() < 0.5
                        && quad.w > 0.0
                })
                .expect("the second category stands on a tile of its own");

            // Nothing is dropped on the way in, so the same pane is at the same
            // index in a scene caught halfway through the arrival.
            for arrival in [0.2, 0.5, 0.8] {
                let mut coming = focused(&xmb, width, height, &AllSlots);
                arrive_from_depth(&mut coming, width, height, arrival);
                let drawn = &coming.quads[button];
                let (x, y) = arrival_point(
                    drawn.x + drawn.w * 0.5,
                    drawn.y + drawn.h * 0.5,
                    width,
                    height,
                    arrival,
                );
                assert!((x - settled.0).abs() < 1e-2 && (y - settled.1).abs() < 1e-2);
                assert_eq!(
                    bar_hit(&xmb, &cursor, x, y, width, height),
                    Some(BarSpot::Category(1)),
                    "{width}x{height} at {arrival}: a press on the second category missed it"
                );
            }
        }
    }

    /// The black an application walking out leaves behind covers the display —
    /// the whole of it, and everything the scene had put down.
    #[test]
    fn the_handover_lays_black_over_the_whole_display() {
        for [width, height] in SCREENS {
            let xmb = cross();
            let mut scene = focused(&xmb, width, height, &AllSlots);
            let quads = scene.quads.len();
            cover_with_black(&mut scene, width, height, 1.0);

            assert_eq!(scene.quads.len(), quads + 1, "one sheet, over everything");
            let sheet = scene.quads.last().expect("the black");
            assert_eq!(
                [sheet.x, sheet.y, sheet.w, sheet.h],
                [0.0, 0.0, width, height]
            );
            assert_eq!(sheet.color, [0.0, 0.0, 0.0, 1.0]);
            // Flat: a rounded sheet would show the display at its corners, and
            // a pane of glass would show what it was covering through it.
            assert_eq!(sheet.radius, 0.0);
            assert_eq!(sheet.thickness, 0.0);
            assert_eq!(sheet.fade, 1.0);
        }

        // And a display with none left over it is left exactly as it was.
        let xmb = cross();
        let mut clear = focused(&xmb, 1920.0, 1080.0, &AllSlots);
        let quads = clear.quads.len();
        cover_with_black(&mut clear, 1920.0, 1080.0, 0.0);
        assert_eq!(clear.quads.len(), quads);
    }

    /// Nothing of the shell reads through the black, and text is the case that
    /// could: every quad is drawn before every run, so a label on a screen
    /// meant to be black would be the one thing on it anybody could see.
    #[test]
    fn nothing_of_the_shell_reads_through_the_black() {
        let xmb = cross();
        let (width, height) = (1920.0, 1080.0);

        let mut dark = focused(&xmb, width, height, &AllSlots);
        assert!(
            dark.texts.iter().any(|text| text.color[3] > 0.0),
            "the bar this is covering has writing on it"
        );
        cover_with_black(&mut dark, width, height, 1.0);
        for text in &dark.texts {
            assert_eq!(
                text.color[3], 0.0,
                "{:?} is legible on a black screen",
                text.content
            );
        }

        // Dimmed to what the sheet leaves, not cut at a threshold: a run that
        // came back whole the frame the black passed it would snap into place
        // instead of coming up out of the dark with the bar it belongs to.
        let lit = focused(&xmb, width, height, &AllSlots);
        let mut half = focused(&xmb, width, height, &AllSlots);
        cover_with_black(&mut half, width, height, 0.5);
        for (dimmed, whole) in half.texts.iter().zip(&lit.texts) {
            assert!((dimmed.color[3] - whole.color[3] * 0.5).abs() < 1e-6);
        }

        // And the black is gone well before the bar has finished arriving,
        // rather than the two ending together — the join is over, and what is
        // left is the shell's own animation.
        const {
            assert!(BLACK_HANDOVER < ARRIVAL * 0.5);
        }
    }

    /// And once it has landed the point is the point, untouched.
    #[test]
    fn a_click_on_a_settled_bar_is_not_moved_at_all() {
        let (x, y) = arrival_point(640.0, 400.0, 1920.0, 1080.0, 1.0);
        assert_eq!([x, y], [640.0, 400.0]);
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
                    clip: None,
                    halo: 0.0,
                    lines: 1,
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
                    clip: None,
                    halo: 0.0,
                    lines: 1,
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
                arrived: 1.0,
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
    fn the_board_travels_through_the_screens_edge_in_both_directions() {
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
        let at = |arrived: f32| {
            slab_top(&build_keyboard(
                KeyboardView {
                    board: &model,
                    slots: &Named,
                    arrived,
                    behind: 0.0,
                    time: 0.0,
                },
                1920.0,
                1080.0,
            ))
        };

        let away = at(0.0);
        assert!(
            away > settled,
            "the board should start below where it settles: {away} vs {settled}"
        );
        assert!(away >= 1080.0, "and off the bottom of the display entirely");

        assert!((at(1.0) - settled).abs() < 0.01, "it must land on its mark");

        // The same road back. Halfway out is halfway down, which is what makes
        // putting the board away a departure rather than a disappearance.
        let half = at(0.5);
        assert!(
            half > settled && half < away,
            "a half-drawn board should be between its two ends: {half}"
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

    // --- the context menu --------------------------------------------------

    use crate::menu::{Command, Menu};

    /// The display sizes the menu is checked at: a small 16:10 panel, 720p,
    /// 1080p, and an ultrawide — the same spread the sidebar is held to.
    const SCREENS: [[f32; 2]; 4] = [
        [960.0, 600.0],
        [1280.0, 720.0],
        [1920.0, 1080.0],
        [3440.0, 1440.0],
    ];

    /// A pane of glass, as the renderer counts them for its snapshot budget:
    /// something rounded and thick that is not merely an outline.
    fn is_glass(quad: &Quad) -> bool {
        quad.radius > 0.0 && quad.border <= 0.0 && quad.thickness > 0.0
    }

    fn menu_entries(count: usize) -> Vec<MenuEntry> {
        (0..count)
            .map(|index| {
                MenuEntry::new(Command::Placeholder("row"), format!("Command {index}"))
                    // Two bands, so a rule falls somewhere in every list long
                    // enough to have one.
                    .group(u8::from(index * 2 >= count))
            })
            .collect()
    }

    /// A menu raised over `anchor`, open, on a display of that size.
    fn raised(anchor: [f32; 4], count: usize, height: f32) -> Menu {
        let mut menu = Menu::default();
        assert!(menu.open_at(
            anchor,
            Some(Title::new("Celeste")),
            menu_entries(count),
            context_menu_rows_that_fit(height),
        ));
        while menu.animate(0.05) < 1.0 {}
        menu
    }

    fn context_scene(menu: &Menu, width: f32, height: f32) -> Scene {
        build_context_menu(
            ContextMenuView {
                menu,
                highlight: None,
                open: 1.0,
                behind: 0.6,
                time: 0.0,
                slots: &Named,
            },
            width,
            height,
        )
    }

    /// The panel is the guide sidebar's material, not a second recipe that
    /// happens to look like it.
    ///
    /// Asserted against `sidebar_surface` itself rather than against the
    /// numbers, so the two cannot drift: whatever the sidebar's glass becomes,
    /// this is cut from the same stuff.
    #[test]
    fn the_context_menu_is_cut_from_the_sidebars_glass() {
        let menu = raised([200.0, 400.0, 160.0, 160.0], 4, 1080.0);
        let scene = context_scene(&menu, 1920.0, 1080.0);
        let rect = context_menu_rect(1920.0, 1080.0, &menu);
        let expected = sidebar_surface(rect, guide_scale(1080.0), 0.6, 1.0);

        // The scrim is drawn first and is not part of the surface.
        let surface = &scene.quads[1..5];
        for (drawn, want) in surface.iter().zip(&expected) {
            assert_eq!(drawn.slot, want.slot);
            assert_eq!(drawn.color, want.color);
            assert_eq!(
                [drawn.x, drawn.y, drawn.w, drawn.h],
                [want.x, want.y, want.w, want.h]
            );
            assert_eq!(drawn.thickness, want.thickness);
            assert_eq!(drawn.frost, want.frost);
            assert_eq!(drawn.gloss, want.gloss);
            assert_eq!(drawn.face_curve, want.face_curve);
        }
        // Two lights under one refractive pane and a hairline over it — the
        // sidebar's four layers, and no second pane to cost another snapshot.
        assert_eq!(surface.iter().filter(|quad| is_glass(quad)).count(), 1);
    }

    /// The bug this replaced: a menu raised over a card landed across the
    /// guide's sidebar and every button it touched lost its label — Resume,
    /// Close and Dashboard all went blank, while their chips stayed lit.
    ///
    /// A run the panel only reaches into keeps the part of itself that is still
    /// in the open, cut at the panel's edge. One it covers outright is still
    /// dropped: there is nothing left of that to draw.
    #[test]
    fn a_panel_only_takes_the_part_of_a_label_it_covers() {
        let (width, height) = (1920.0, 1080.0);
        let mut guide = Guide::default();
        guide.set_bars(BOTH_BARS);
        guide.open();
        guide.backdate_open(2.0);
        let cards = [card("Celeste")];
        let mut scene = guide_scene(&guide, Some("Celeste"), None, &cards, None);
        let before: Vec<String> = scene.texts.iter().map(|t| t.content.clone()).collect();

        // A menu out of the selected card. A card that wide has no room for a
        // panel to its right, so the panel flips to its left and lands across
        // the column — which is the frame the bug was reported from.
        let menu = raised([800.0, 300.0, 700.0, 400.0], 4, height);
        let panel = context_menu_rect(width, height, &menu);
        let [sidebar_x, _, sidebar_w, _] = sidebar_panel_rect(width, height);
        assert!(
            panel[0] < sidebar_x + sidebar_w && panel[0] + panel[2] > sidebar_x,
            "this test is only about a panel that reaches over the sidebar"
        );
        recede_behind_context_menu(&mut scene, width, height, &menu, 1.0);

        // Every label is still here. Not one for one, because a run the panel
        // crosses is now drawn as its two ends and arrives twice — what must
        // not happen is a name going missing.
        let after: Vec<String> = scene.texts.iter().map(|t| t.content.clone()).collect();
        for name in &before {
            assert!(
                after.contains(name),
                "{name:?} vanished from under the panel"
            );
        }
        for name in &after {
            assert!(before.contains(name), "{name:?} was invented by the cut");
        }
        let mut cut = 0;
        for text in &scene.texts {
            // A run the panel is nowhere near is left alone entirely — a clip
            // on one of those would be a label being cut for nothing.
            let Some([x, _, w, _]) = text.clip else {
                assert!(
                    !behind(text, panel),
                    "{:?} is under the panel and was not cut",
                    text.content
                );
                continue;
            };
            assert!(
                behind(text, panel),
                "{:?} was cut for nothing",
                text.content
            );
            assert!(w > 0.0, "{:?} was cut down to nothing", text.content);
            assert!(
                x + w <= panel[0] + 0.5 || x >= panel[0] + panel[2] - 0.5,
                "{:?} is still allowed to draw under the panel",
                text.content
            );
            cut += 1;
        }
        assert!(cut > 0, "the panel covered none of the column");

        // A run wholly inside the panel has nothing left and goes, which is
        // what stops the bar printing through a menu raised over it.
        let mut covered = Scene::default();
        covered.texts.push(Text {
            content: "Buried".to_string(),
            x: panel[0] + 10.0,
            y: panel[1] + 10.0,
            size: 20.0,
            color: [1.0; 4],
            bold: false,
            max_width: 40.0,
            align: TextAlign::Left,
            clip: None,
            halo: 0.0,
            lines: 1,
        });
        covered.hide_text_behind(panel);
        assert!(covered.texts.is_empty());
    }

    /// It stands beside the control it is about, wholly on the display, from
    /// wherever on that display the control happens to be.
    #[test]
    fn it_stands_beside_its_anchor_and_never_off_the_display() {
        for [width, height] in SCREENS {
            let inset = PANEL_INSET * guide_scale(height);
            // A tile at each corner and one in the middle: the two horizontal
            // extremes are what make the panel flip to the other side, and the
            // vertical ones are what make it stop sliding.
            for anchor in [
                [inset, inset, 140.0, 140.0],
                [width - inset - 140.0, inset, 140.0, 140.0],
                [inset, height - inset - 140.0, 140.0, 140.0],
                [width - inset - 140.0, height - inset - 140.0, 140.0, 140.0],
                [width * 0.5, height * 0.5, 140.0, 140.0],
            ] {
                let menu = raised(anchor, 4, height);
                let [x, y, w, h] = context_menu_rect(width, height, &menu);
                assert!(
                    x >= inset - 0.01 && y >= inset - 0.01,
                    "{width}x{height} at {anchor:?}: panel starts at {x},{y}"
                );
                assert!(
                    x + w <= width - inset + 0.01 && y + h <= height - inset + 0.01,
                    "{width}x{height} at {anchor:?}: panel ends at {},{}",
                    x + w,
                    y + h
                );
                // Beside the anchor, never over it: the anchor is the only
                // context the panel has, and covering it throws that away.
                assert!(
                    x >= anchor[0] + anchor[2] || x + w <= anchor[0],
                    "{width}x{height} at {anchor:?}: panel {x}..{} overlaps it",
                    x + w
                );
            }
        }
    }

    /// Every row is drawn inside the panel, and clear of its bevel — the
    /// sidebar's own spacing rule, which the chips here inherit by being laid
    /// out from the same margin.
    #[test]
    fn every_row_sits_inside_the_panel_and_clears_its_bevel() {
        for [width, height] in SCREENS {
            let scale = guide_scale(height);
            let menu = raised([300.0, 300.0, 160.0, 160.0], 5, height);
            let [px, py, pw, ph] = context_menu_rect(width, height, &menu);
            let mut previous_bottom = py;

            for index in 0..menu.entries().len() {
                let [x, y, w, h] = context_menu_row_rect(width, height, &menu, index)
                    .expect("a list this short is drawn whole");
                assert!(
                    x - px >= DEPTH_SIDEBAR * scale,
                    "{width}x{height}: row {index} rests on the panel's rim"
                );
                assert!(
                    (x - px - (px + pw - (x + w))).abs() < 0.01,
                    "row {index} is off centre"
                );
                assert!(
                    y >= previous_bottom && y + h <= py + ph,
                    "{width}x{height}: row {index} runs from {y} to {} in a panel {py}..{}",
                    y + h,
                    py + ph
                );
                previous_bottom = y + h;
            }
        }
    }

    /// The rule between two bands is centred between the chips it separates,
    /// not hung off the lower one — the guide's separator rule, which this
    /// column has to follow for the same reason: the chips carry their own row
    /// padding, so a rule placed from one side alone sits visibly high.
    #[test]
    fn a_bands_rule_is_centred_between_the_rows_it_parts() {
        for [width, height] in SCREENS {
            let menu = raised([300.0, 300.0, 160.0, 160.0], 6, height);
            let rules = context_separator_rects(width, height, &menu);
            assert_eq!(rules.len(), 1, "{width}x{height}: two bands, one rule");

            let above = context_menu_row_rect(width, height, &menu, 2).unwrap();
            let below = context_menu_row_rect(width, height, &menu, 3).unwrap();
            let [_, ry, _, rh] = rules[0];
            let over = ry - (above[1] + above[3]);
            let under = below[1] - (ry + rh);
            assert!(
                (over - under).abs() < 0.5,
                "{width}x{height}: {over} of air above the rule and {under} below"
            );
        }
    }

    /// A list longer than the display can hold scrolls under a fixed panel
    /// rather than running off the bottom of the screen, and says so at both
    /// ends — but only at an end it has not reached.
    #[test]
    fn a_long_list_scrolls_inside_a_panel_that_fits() {
        for [width, height] in SCREENS {
            let inset = PANEL_INSET * guide_scale(height);
            let mut menu = raised([300.0, 300.0, 160.0, 160.0], 40, height);
            let drawn = menu.visible_rows();
            assert!(drawn < 40, "{width}x{height}: nothing was left to scroll");

            let [_, y, _, h] = context_menu_rect(width, height, &menu);
            assert!(
                y >= inset - 0.01 && y + h <= height - inset + 0.01,
                "{width}x{height}: a {drawn}-row panel runs from {y} to {}",
                y + h
            );

            // At the top: an arrow below and none above.
            let arrows = |menu: &Menu| {
                let scene = context_scene(menu, width, height);
                (
                    scene
                        .quads
                        .iter()
                        .any(|quad| quad.slot == Named::slot_of(icons::ARROW_UP)),
                    scene
                        .quads
                        .iter()
                        .any(|quad| quad.slot == Named::slot_of(icons::ARROW_DOWN)),
                )
            };
            assert_eq!(arrows(&menu), (false, true), "{width}x{height}: at the top");

            // Walking down takes the window with it, and the panel does not
            // change size as it goes.
            let settled = context_menu_rect(width, height, &menu);
            for _ in 0..drawn {
                menu.move_selection(1);
            }
            assert!(menu.first_visible() > 0);
            assert_eq!(context_menu_rect(width, height, &menu), settled);
            assert_eq!(
                arrows(&menu),
                (true, true),
                "{width}x{height}: in the middle"
            );

            // And at the foot there is nowhere further down to point.
            while menu.selected() + 1 < 40 {
                menu.move_selection(1);
            }
            assert_eq!(
                arrows(&menu),
                (true, false),
                "{width}x{height}: at the foot"
            );
        }
    }

    /// The panel leaves its anchor before anything on it is legible, and the
    /// two arrive as one shape rather than as a panel with contents drifting
    /// inside it.
    #[test]
    fn the_menu_grows_out_of_the_control_it_is_about() {
        let anchor = [400.0, 500.0, 160.0, 160.0];
        let (width, height) = (1920.0, 1080.0);
        let menu = raised(anchor, 4, height);

        let shut = context_menu_bounds(width, height, &menu, 0.0);
        assert!(
            (shut[0] + shut[2] * 0.5 - (anchor[0] + anchor[2] * 0.5)).abs() < 0.01
                && (shut[1] + shut[3] * 0.5 - (anchor[1] + anchor[3] * 0.5)).abs() < 0.01,
            "shut, it is centred on the anchor: {shut:?}"
        );
        assert_eq!(
            context_menu_bounds(width, height, &menu, 1.0),
            context_menu_rect(width, height, &menu)
        );

        // A fifth of the way out there is glass on its way but nothing to read
        // on it yet.
        let early = build_context_menu(
            ContextMenuView {
                menu: &menu,
                highlight: None,
                open: 0.2,
                behind: 0.0,
                time: 0.0,
                slots: &Named,
            },
            width,
            height,
        );
        assert!(early
            .quads
            .iter()
            .any(|quad| is_glass(quad) && quad.fade > 0.0));
        assert!(
            early.texts.iter().all(|text| text.color[3] < 0.01),
            "the commands were legible at a fifth of the panel's size"
        );

        // And the whole of it keeps the panel's proportions on the way, so
        // nothing inside can drift out of it.
        let [_, _, gw, gh] = context_menu_bounds(width, height, &menu, 0.5);
        let [_, _, pw, ph] = context_menu_rect(width, height, &menu);
        assert!((gw / gh - pw / ph).abs() < 0.001);
    }

    // --- the volume mixer --------------------------------------------------

    // --- the corner of the screen -------------------------------------------

    fn bubble(stage: crate::notify::Stage, progress: f32) -> ToastCard<'static> {
        ToastCard {
            title: "Download finished",
            body: "linux-6.9.tar.xz",
            icon: None,
            stage,
            progress,
        }
    }

    /// Announcements arrive in the top right, stacked downwards, and every one
    /// of them is inside the display: a bubble that hangs off the edge it came
    /// in through is one nobody can read the end of.
    #[test]
    fn bubbles_stack_down_from_the_top_right_corner() {
        use crate::notify::Stage;
        let (w, h) = (1920.0, 1080.0);
        let cards = [
            bubble(Stage::Sitting, 0.0),
            bubble(Stage::Sitting, 0.0),
            bubble(Stage::Sitting, 0.0),
        ];
        let rects = toast_rects(&cards, w, h);
        assert_eq!(rects.len(), 3);

        for rect in &rects {
            assert!(
                rect[0] > w * 0.5,
                "in the right half of the display: {rect:?}"
            );
            assert!(rect[0] + rect[2] < w, "and clear of the edge: {rect:?}");
            assert!(rect[1] > 0.0 && rect[1] + rect[3] < h, "{rect:?}");
        }
        // One column, in order, with air between and never overlapping.
        for pair in rects.windows(2) {
            assert_eq!(pair[0][0], pair[1][0], "one column");
            assert!(
                pair[1][1] > pair[0][1] + pair[0][3],
                "{:?} runs into {:?}",
                pair[0],
                pair[1]
            );
        }
    }

    /// A bubble comes in from beyond the right-hand edge and goes back out the
    /// same way, and at both ends of its life it is somewhere other than where
    /// it sits. Which is the whole reason the corner is laid out from the
    /// stage rather than from a row number.
    #[test]
    fn a_bubble_flies_in_from_outside_the_display_and_back_out_of_it() {
        use crate::notify::Stage;
        let (w, h) = (1920.0, 1080.0);

        let arriving = toast_rects(&[bubble(Stage::In, 0.0)], w, h);
        assert!(
            arriving[0][0] >= w,
            "it starts off the display entirely: {arriving:?}"
        );
        let settled = toast_rects(&[bubble(Stage::Sitting, 0.0)], w, h);
        let leaving = toast_rects(&[bubble(Stage::Out, 0.5)], w, h);
        assert!(
            leaving[0][0] > settled[0][0],
            "and leaves the way it came: {leaving:?} vs {settled:?}"
        );
        // Part-way in it is between the two, and it eases rather than
        // travelling at a constant speed — nothing in this shell moves
        // linearly. A quarter of the way through the flight it has barely
        // left, which is what makes a bubble lean into its arrival.
        let quarter = toast_rects(&[bubble(Stage::In, 0.25)], w, h);
        assert!(quarter[0][0] > settled[0][0] && quarter[0][0] < arriving[0][0]);
        let linear = arriving[0][0] + (settled[0][0] - arriving[0][0]) * 0.25;
        assert!(
            quarter[0][0] > linear + 1.0,
            "a quarter of the way in it should still be near the edge: \
             {quarter:?} against a linear {linear}"
        );
    }

    /// A bubble on its way out gives its slot back as it goes, so the ones
    /// under it have finished climbing by the time it is swept off the list.
    ///
    /// Without this the stack is correct in every frame and still jumps: three
    /// bubbles hold three slots for the whole of the first one's exit, and
    /// then the other two snap up a row in one frame, some tens of pixels,
    /// after the thing that moved them has already gone.
    #[test]
    fn the_stack_closes_up_while_the_bubble_above_is_still_leaving() {
        use crate::notify::Stage;
        let (w, h) = (1920.0, 1080.0);

        let settled = toast_rects(
            &[bubble(Stage::Sitting, 0.0), bubble(Stage::Sitting, 0.0)],
            w,
            h,
        );
        let closing = toast_rects(
            &[bubble(Stage::Out, 0.5), bubble(Stage::Sitting, 0.0)],
            w,
            h,
        );
        assert!(
            closing[1][1] < settled[1][1],
            "the one below climbs while the one above is still going"
        );

        // And by the end of the flight it is standing where the departing one
        // stood, so the sweep changes nothing.
        let closed = toast_rects(
            &[bubble(Stage::Out, 1.0), bubble(Stage::Sitting, 0.0)],
            w,
            h,
        );
        assert!(
            (closed[1][1] - settled[0][1]).abs() < 0.01,
            "{:?} should have reached {:?}",
            closed[1],
            settled[0]
        );
    }

    /// The glass and the words are one object: a bubble drawn with nothing on
    /// it, or words with no pane under them, is a failure either way.
    #[test]
    fn a_bubble_carries_what_it_has_to_say() {
        use crate::notify::Stage;
        let scene = build_toasts(&[bubble(Stage::Sitting, 0.0)], 1920.0, 1080.0, 0.0);
        let said: Vec<&str> = scene.texts.iter().map(|t| t.content.as_str()).collect();
        assert_eq!(said, ["Download finished", "linux-6.9.tar.xz"]);
        assert!(!scene.quads.is_empty(), "and stands on something");

        // A program that sent no body gets one line, centred, rather than a
        // sentence sitting above the middle of an empty pane.
        let bare = ToastCard {
            body: "",
            ..bubble(Stage::Sitting, 0.0)
        };
        let scene = build_toasts(std::slice::from_ref(&bare), 1920.0, 1080.0, 0.0);
        assert_eq!(scene.texts.len(), 1);
        let [_, y, _, h] = toast_rects(std::slice::from_ref(&bare), 1920.0, 1080.0)[0];
        let line = &scene.texts[0];
        assert!(
            (line.y + line.size * 0.5 - (y + h * 0.5)).abs() < h * 0.08,
            "the lone line sits on the pane's middle"
        );

        // A body of several lines is cut to one. A bubble that grew to fit
        // would be a program deciding how much of the screen it may have.
        let wordy = ToastCard {
            body: "first line\nsecond line\nthird",
            ..bubble(Stage::Sitting, 0.0)
        };
        let scene = build_toasts(&[wordy], 1920.0, 1080.0, 0.0);
        assert_eq!(scene.texts[1].content, "first line");
    }

    /// Both lines are ringed in shade, and the glass behind them is left
    /// alone. A bubble stands wherever the corner of the screen happens to be
    /// and shows whatever is under it, so its writing is the one text in the
    /// shell with nothing chosen behind it.
    #[test]
    fn a_bubbles_writing_carries_its_own_contrast() {
        use crate::notify::Stage;
        let card = bubble(Stage::Sitting, 0.0);
        let scene = build_toasts(std::slice::from_ref(&card), 1920.0, 1080.0, 0.0);
        assert_eq!(scene.texts.len(), 2);
        for line in &scene.texts {
            assert!(
                line.halo > 0.0,
                "{:?} stands on borrowed light",
                line.content
            );
        }

        // The second line gets the *stronger* ring, not the weaker one. It is
        // smaller, lighter and drawn in the soft grey that makes it second —
        // every one of which costs it contrast, so matching the summary leaves
        // it the harder of the two to read, which is backwards.
        assert!(
            scene.texts[1].halo > scene.texts[0].halo,
            "the quieter line needs the most help: {} against {}",
            scene.texts[1].halo,
            scene.texts[0].halo
        );

        // And both rings leave with the bubble. Nothing thins them now that
        // the run's own colour does not — see `gpu::halo_copies` — so a bubble
        // that flew off leaving its outlines standing is the failure to watch
        // for, and it is this line that would catch it.
        let leaving = build_toasts(&[bubble(Stage::Out, 0.85)], 1920.0, 1080.0, 0.0);
        for (going, sitting) in leaving.texts.iter().zip(&scene.texts) {
            assert!(
                going.halo > 0.0 && going.halo < sitting.halo,
                "{:?} is on its way out",
                going.content
            );
        }

        // And nothing else is drawn to get that contrast. The bubble is the
        // pane the context menu is made of and the picture at its head, and
        // that is the whole of it: taking the glass down behind the words was
        // tried, and a panel with a darker patch in the middle of it is the
        // flat, cheap slab this one was rewritten to stop being.
        let rect = toast_rects(std::slice::from_ref(&card), 1920.0, 1080.0)[0];
        assert_eq!(
            scene.quads.len(),
            sidebar_surface(rect, guide_scale(1080.0), 0.0, 1.0).len() + 1,
            "one pane's worth of glass, and the icon"
        );
    }

    /// Nothing is drawn for a bubble that has finished leaving, and nothing is
    /// cut out from under one either — a hole in the labels underneath, left
    /// where a bubble used to be, is the same bug seen from the other side.
    #[test]
    fn a_bubble_that_has_gone_leaves_no_hole_behind_it() {
        use crate::notify::Stage;
        let width = 1920.0;
        for card in [bubble(Stage::Out, 1.0), bubble(Stage::In, 0.0)] {
            let rect = toast_rects(std::slice::from_ref(&card), width, 1080.0)[0];
            assert!(rect[0] >= width, "still off the display: {rect:?}");
            let scene = build_toasts(std::slice::from_ref(&card), width, 1080.0, 0.0);
            assert!(scene.quads.is_empty() && scene.texts.is_empty());
        }
    }

    // --- the volume the keys raise -------------------------------------------

    fn volume_control(level: Level, fade: f32) -> Scene {
        build_volume(
            VolumeView {
                level,
                fade,
                behind: 0.0,
                slots: &Named,
            },
            1920.0,
            1080.0,
        )
    }

    /// It stands at the foot of the display, clear of every other thing this
    /// shell puts on one: the clock and the bubbles in the top right, the
    /// cross and the guide's sidebar down the left.
    #[test]
    fn the_volume_control_stands_at_the_foot_of_the_display() {
        let (w, h) = (1920.0, 1080.0);
        let [x, y, cw, ch] = volume_rect(w, h, 1.0);
        assert!(x > 0.0 && x + cw < w, "inside the display: {x} {cw}");
        assert!(
            (x + cw * 0.5 - w * 0.5).abs() < 0.01,
            "centred: {x} + {cw} on {w}"
        );
        assert!(y > h * 0.75, "at the foot of it: {y}");
        assert!(y + ch < h, "and clear of the edge: {y} + {ch}");

        // It comes up from below rather than appearing, and has arrived by the
        // time it is solid — nothing that has finished arriving is still on
        // its way somewhere.
        let arriving = volume_rect(w, h, 0.0);
        assert!(arriving[1] > y, "it rises: {} to {y}", arriving[1]);
        assert_eq!(volume_rect(w, h, 1.0), [x, y, cw, ch]);
    }

    /// One pane, with the sidebar's own volume bar laid on it — the same
    /// picture, because a volume set two ways is one control.
    #[test]
    fn the_volume_control_wears_the_speaker_of_the_level_it_shows() {
        let glyph = |level: Level| {
            volume_control(level, 1.0)
                .quads
                .iter()
                .find(|quad| quad.slot != SOLID_SLOT && quad.slot != GLOW_SLOT)
                .map(|quad| quad.slot)
                .expect("the speaker")
        };
        assert_eq!(
            glyph(Level {
                value: 0.4,
                muted: false
            }),
            Named::slot_of(icons::VOLUME)
        );
        assert_eq!(
            glyph(Level {
                value: 0.4,
                muted: true
            }),
            Named::slot_of(icons::VOLUME_MUTED),
            "a silenced session says so, as the row in the guide does"
        );
    }

    /// The fill is the reading, and it is the one thing on the control that
    /// changes with the value.
    #[test]
    fn the_volume_controls_fill_follows_the_level() {
        let filled = |value: f32| {
            let scene = volume_control(
                Level {
                    value,
                    muted: false,
                },
                1.0,
            );
            // The groove and what is lying in it: the two flat marks the
            // track is made of, which are the only things on the control as
            // thin as `BAR_TRACK`. The pane behind them is the height of the
            // whole panel, and the handle is twice the track.
            let parts: Vec<&Quad> = scene
                .quads
                .iter()
                .filter(|quad| quad.slot == SOLID_SLOT && quad.h <= BAR_TRACK + 0.5)
                .collect();
            let track = parts
                .iter()
                .max_by(|a, b| a.w.total_cmp(&b.w))
                .expect("a groove");
            parts
                .iter()
                .filter(|quad| (quad.x - track.x).abs() < 0.5 && quad.w < track.w)
                .map(|quad| quad.w / track.w)
                .fold(0.0f32, f32::max)
        };
        let quiet = filled(0.2);
        let loud = filled(0.8);
        assert!(quiet > 0.0 && quiet < loud && loud < 1.0, "{quiet} {loud}");
        assert!(
            (loud / quiet - 4.0).abs() < 0.2,
            "four times as loud is four times as much of the groove: {quiet} {loud}"
        );
    }

    /// Nothing at all is drawn once it has gone — a control that left its
    /// glass standing over somebody's game is the failure to watch for.
    #[test]
    fn a_volume_control_that_has_gone_draws_nothing() {
        let scene = volume_control(
            Level {
                value: 0.5,
                muted: false,
            },
            0.0,
        );
        assert!(scene.quads.is_empty() && scene.texts.is_empty());
    }

    /// The notification list as the guide raises it: a way to clear the lot,
    /// and one announcement whose summary is longer than a row.
    ///
    /// Three runs, as the real rows have: when it arrived, what it is called,
    /// and what it went on to say.
    fn announcements(lines: u8, detail_lines: u8, height: f32) -> Menu {
        announcements_stamped(Some("now"), lines, detail_lines, height)
    }

    /// The same, with the time made optional — for the one test that has to
    /// weigh a row carrying it against the same row without.
    fn announcements_stamped(
        stamp: Option<&str>,
        lines: u8,
        detail_lines: u8,
        height: f32,
    ) -> Menu {
        let mut menu = Menu::default();
        let mut long = MenuEntry::new(Command::DismissNotification(1), "Hello World".repeat(6))
            .icon("mail")
            .stamp(stamp.unwrap_or_default())
            .detail("the rest of what it said ".repeat(6))
            .aside(Command::DismissNotification(1), icons::UNINSTALL);
        long.lines = lines;
        long.detail_lines = detail_lines;
        assert!(menu.open_selecting(
            [40.0, 200.0, 68.0, 68.0],
            Some(Title::new("Notifications")),
            vec![
                MenuEntry::new(Command::DismissNotifications, "Clear All").glyph(icons::UNINSTALL),
                long.group(1),
            ],
            mixer_rows_that_fit(height),
            1,
        ));
        menu.widen(NOTIFICATION_EXTRA_WIDTH);
        while menu.animate(0.05) < 1.0 {}
        menu
    }

    /// Where a row's runs are drawn, top first: how far down the row's own chip
    /// each one starts, what size it is set at, and how many lines it may take.
    ///
    /// Measured from the chip and not from the top of the display, because the
    /// panel hangs centred off the tile it was raised from: it slides up by
    /// half of whatever a row on it grows by, and every absolute reading inside
    /// it moves with it.
    fn row_writing(menu: &Menu, index: usize, width: f32, height: f32) -> Vec<(f32, f32, u8)> {
        let chip = context_menu_row_rect(width, height, menu, index).unwrap();
        let mut runs: Vec<(f32, f32, u8)> = context_scene(menu, width, height)
            .texts
            .iter()
            .filter(|text| text.y >= chip[1] - 1.0 && text.y <= chip[1] + chip[3])
            .map(|text| (text.y - chip[1], text.size, text.lines))
            .collect();
        runs.sort_by(|a, b| a.0.total_cmp(&b.0));
        runs
    }

    /// A stacked row is cornered like the mixer's — a rounded square, cut to a
    /// share of its own height — and not like a command, which is a capsule.
    ///
    /// The two were decided by different questions, one asking whether the row
    /// carried a level and the other how tall it was, and a row that is tall
    /// for the *other* reason fell between them: a deep capsule sitting beside
    /// the mixer's rounded squares in a panel raised from the tile next door.
    ///
    /// One rule, not one number: an announcement's row is taller than a track
    /// by the line above its summary, so it is a little rounder than one, and
    /// nowhere near a capsule. Pinning the two to the same radius would make
    /// the corner a fact about tracks rather than about tall rows.
    #[test]
    fn a_stacked_row_is_cornered_like_a_track_and_not_like_a_button() {
        let height = 1080.0;
        let scale = guide_scale(height);
        let announced = announcements(1, 1, height);
        let tracks = mixer([0.5; 3], height);

        let row = |menu: &Menu, index: usize| {
            let rect = context_menu_row_rect(1920.0, height, menu, index).unwrap();
            (
                context_chip_radius(&menu.entries()[index], rect[3], scale),
                rect[3],
            )
        };

        // Both are cut to the same share of themselves, and the taller of the
        // two is the rounder by exactly that much.
        let (announcement, announcement_h) = row(&announced, 1);
        let (track, track_h) = row(&tracks, 0);
        assert!(announcement_h > track_h, "the extra line: {announcement_h}");
        assert!(
            (announcement / announcement_h - track / track_h).abs() < 0.01,
            "{announcement} of {announcement_h} against {track} of {track_h}"
        );
        // And neither is a capsule, which is what this test exists to catch.
        assert!(announcement < announcement_h * 0.5 - 1.0);
        assert!(track < track_h * 0.5 - 1.0);

        // A one-line command still is one, here and everywhere else in the
        // shell.
        let (clear, clear_h) = row(&announced, 0);
        assert!((clear - clear_h * 0.5).abs() < 1e-3);
    }

    /// The time stands above the summary, in its own smaller and quieter run,
    /// and the row is one line taller for it.
    ///
    /// Above rather than sharing the body's line, which is where it was until
    /// the user asked for this: the two ran together into one sentence
    /// beginning with a time, and reading either meant finding the separator
    /// first. The cost is the third line, and it is charged to the row rather
    /// than taken out of the two runs that were already there — everything
    /// under the time sits exactly where it sat before.
    #[test]
    fn an_announcement_says_when_above_its_summary() {
        let height = 1080.0;
        let width = 1920.0;
        let scale = guide_scale(height);
        let menu = announcements(1, 1, height);
        let runs = row_writing(&menu, 1, width, height);
        assert_eq!(runs.len(), 3, "a time, a summary and a body: {runs:?}");

        // Top to bottom, and each smaller than the one above it: the time is
        // the least of what the row says and is set as the least of it.
        assert!(runs[0].0 < runs[1].0 && runs[1].0 < runs[2].0, "{runs:?}");
        assert!(
            (runs[0].1 - CONTEXT_STAMP_SIZE * scale).abs() < 1e-3,
            "{runs:?}"
        );
        assert!(runs[0].1 < runs[2].1 && runs[2].1 < runs[1].1, "{runs:?}");

        // The row is exactly one of the time's lines taller than the same row
        // without one, and the two runs below it have not moved within what is
        // left: the line was added over the row rather than squeezed into it.
        let plain = announcements_stamped(None, 1, 1, height);
        let shorter = row_writing(&plain, 1, width, height);
        assert_eq!(shorter.len(), 2, "{shorter:?}");
        let tall = context_menu_row_rect(width, height, &menu, 1).unwrap();
        let short = context_menu_row_rect(width, height, &plain, 1).unwrap();
        assert!(
            (tall[3] - short[3] - CONTEXT_STAMP_ROOM * scale).abs() < 1.0,
            "{tall:?} against {short:?}"
        );
        for (with, without) in runs[1..].iter().zip(&shorter) {
            assert!(
                (with.0 - without.0 - CONTEXT_STAMP_ROOM * scale).abs() < 1.0,
                "moved down by the time's line and no more: {runs:?} against {shorter:?}"
            );
        }
    }

    /// The air over a row's first line is the air under its last, whether the
    /// row carries one run, two or three.
    ///
    /// The bug: the time was given a line box at the very top of the chip and
    /// sat against the border, while the body below it kept a whole margin —
    /// visibly lopsided, and reported. It came of each run being placed at a
    /// share of the row chosen to read well for the number of runs there were
    /// at the time, so a run added later had nowhere balanced to go. The runs
    /// are now a stack of line boxes with the leftover halved above and below,
    /// which is one rule for any number of them.
    #[test]
    fn the_air_above_a_rows_writing_matches_the_air_under_it() {
        let height = 1080.0;
        let width = 1920.0;
        // Every shape of row the panel has: a bare command, an announcement
        // with nothing more to say, and one with all three runs.
        let three = announcements(1, 1, height);
        let two = announcements_stamped(None, 1, 1, height);
        for (menu, index, runs_wanted) in [(&three, 0, 1), (&two, 1, 2), (&three, 1, 3)] {
            let runs = row_writing(menu, index, width, height);
            assert_eq!(runs.len(), runs_wanted, "{runs:?}");
            let chip = context_menu_row_rect(width, height, menu, index).unwrap();

            let above = runs[0].0;
            let last = runs[runs.len() - 1];
            // A run's box is its own line height, once per line it may take.
            let below = chip[3] - (last.0 + last.2 as f32 * last.1 * 1.25);
            assert!(
                (above - below).abs() < 0.01,
                "{runs_wanted} runs: {above} over, {below} under"
            );
            assert!(above > 0.0, "{runs_wanted} runs sit against the border");
        }
    }

    /// A row whose label will not fit opens out under the highlight, and takes
    /// the rows below it down with it.
    #[test]
    fn a_row_too_long_to_read_opens_out_when_it_is_selected() {
        let height = 1080.0;
        let width = 1920.0;
        let mut menu = announcements(3, 1, height);
        assert_eq!(menu.selected(), 1, "opened on the announcement");

        let row = |menu: &Menu| context_menu_row_rect(width, height, menu, 1).unwrap();
        let shut = row(&menu);
        assert_eq!(context_row_growth(&menu, 1), 0.0, "nothing has moved yet");

        // It opens over several frames rather than jumping.
        let mut seen = Vec::new();
        for _ in 0..40 {
            menu.animate(0.05);
            seen.push(row(&menu)[3]);
        }
        assert!(
            seen.windows(2).all(|pair| pair[1] >= pair[0] - 1e-3),
            "it only ever grows on the way out: {seen:?}"
        );
        assert!(
            seen.iter()
                .any(|h| *h > shut[3] + 1.0 && *h < seen[seen.len() - 1] - 1.0),
            "and is caught part-way there: {seen:?}"
        );

        let open = row(&menu);
        let scale = guide_scale(height);
        assert!(
            (open[3] - shut[3] - 2.0 * CONTEXT_LABEL_LINE * scale).abs() < 1.0,
            "two lines taller: {} against {}",
            open[3],
            shut[3]
        );

        // Only that row. The one above it has not moved, and the panel behind
        // them has grown to hold what has.
        let above = context_menu_row_rect(width, height, &menu, 0).unwrap();
        assert_eq!(context_row_growth(&menu, 0), 0.0);
        assert!((above[3] - CONTEXT_ROW * scale + GUIDE_ROW_PADDING * 2.0 * scale).abs() < 1.0);
        assert!(
            open[1] >= above[1] + above[3],
            "still below the row above it"
        );

        // Moving the highlight away closes it again.
        menu.move_selection(-1);
        for _ in 0..40 {
            menu.animate(0.05);
        }
        assert!(
            (row(&menu)[3] - shut[3]).abs() < 1e-3,
            "shut behind the highlight"
        );
    }

    /// And so does the line under it, which on an announcement is the longer of
    /// the two: the summary is a headline and the body is the sentence.
    ///
    /// The two runs open out separately. The label's room is taken under the
    /// label, so the body follows it down; the body's is taken under the body,
    /// so the label stays where it was. A row that shared one total between
    /// them would push the body away from the heading it belongs to.
    #[test]
    fn a_body_too_long_to_read_opens_out_under_its_own_heading() {
        let height = 1080.0;
        let width = 1920.0;
        let scale = guide_scale(height);
        let row = |menu: &Menu| context_menu_row_rect(width, height, menu, 1).unwrap();
        let writing = |menu: &Menu| row_writing(menu, 1, width, height);

        // Nothing but the body is too long. The row still grows, and by the
        // body's line rather than the label's — they are different sizes, and
        // opening by the wrong one leaves a gap or clips a line.
        let mut menu = announcements(1, 3, height);
        let shut = row(&menu);
        let settled = writing(&menu);
        for _ in 0..40 {
            menu.animate(0.05);
        }
        let open = row(&menu);
        assert!(
            (open[3] - shut[3] - 2.0 * CONTEXT_DETAIL_LINE * scale).abs() < 1.0,
            "two lines of body taller: {} against {}",
            open[3],
            shut[3]
        );

        // The body of the row being read is white, and every other row's stays
        // quiet: the announcement under the highlight is the one being read,
        // and it is the row that has just opened out to show the rest of it.
        let body = |menu: &Menu, row: usize| {
            let chip = context_menu_row_rect(width, height, menu, row).unwrap();
            context_scene(menu, width, height)
                .texts
                .iter()
                .filter(|text| text.y > chip[1] && text.y < chip[1] + chip[3])
                .map(|text| text.color)
                .next_back()
                .unwrap()
        };
        let lit = body(&menu, 1);
        menu.move_selection(-1);
        for _ in 0..40 {
            menu.animate(0.05);
        }
        assert!(
            lit[3] > body(&menu, 1)[3] + 0.1,
            "the row being read is the bright one: {lit:?}"
        );
        menu.move_selection(1);
        for _ in 0..40 {
            menu.animate(0.05);
        }

        let opened = writing(&menu);
        assert_eq!(opened.len(), 3, "a time, a heading and a body: {opened:?}");
        assert_eq!(opened[2].2, 3, "the body may take three lines");
        assert_eq!(opened[1].2, 1, "the heading still takes one");
        assert_eq!(opened[0].2, 1, "and the time is always one");
        // Neither run has moved: the label's room is nil and the body's opens
        // out beneath it.
        for (was, now) in settled.iter().zip(&opened) {
            assert!(
                (was.0 - now.0).abs() < 1e-3,
                "the writing stayed put: {settled:?} against {opened:?}"
            );
        }

        // With a long label as well, the body is pushed down by exactly the
        // room the label took above it — no further, or it would drift away
        // from the heading it belongs to.
        let mut both = announcements(3, 3, height);
        let before = writing(&both);
        for _ in 0..40 {
            both.animate(0.05);
        }
        let after = writing(&both);
        for (was, now) in before.iter().zip(&after).take(2) {
            assert!(
                (was.0 - now.0).abs() < 1e-3,
                "the time and the heading are where they were: {before:?} against {after:?}"
            );
        }
        assert!(
            (after[2].0 - before[2].0 - 2.0 * CONTEXT_LABEL_LINE * scale).abs() < 1.0,
            "the body dropped by the label's two lines: {before:?} against {after:?}"
        );
        assert!(
            (row(&both)[3] - shut[3] - 2.0 * (CONTEXT_LABEL_LINE + CONTEXT_DETAIL_LINE) * scale)
                .abs()
                < 1.0,
            "and the row holds both"
        );
    }

    /// Neither run ever draws a line the row has not yet opened out far enough
    /// to hold.
    ///
    /// Caught on screen. The label took its third line as soon as the room for
    /// it was more than half there, while the body underneath was still on its
    /// way down, and for a third of a second every time the highlight arrived
    /// the two were drawn straight through each other.
    #[test]
    fn a_line_is_drawn_only_once_there_is_room_for_it() {
        let height = 1080.0;
        let width = 1920.0;
        let scale = guide_scale(height);
        let mut menu = announcements(3, 3, height);

        // From the last line of the label to the top of the body, which is the
        // distance that closes up when a line arrives early.
        let gap = |menu: &Menu| {
            let runs = row_writing(menu, 1, width, height);
            assert_eq!(runs.len(), 3, "a time, a heading and a body: {runs:?}");
            runs[2].0 - (runs[1].0 + (runs[1].2 - 1) as f32 * CONTEXT_LABEL_LINE * scale)
        };

        let settled = gap(&menu);
        for _ in 0..40 {
            menu.animate(0.05);
            assert!(
                gap(&menu) >= settled - 1e-3,
                "the body stays clear of the label: {} against {settled}",
                gap(&menu)
            );
        }
        // And at rest they are exactly as far apart as they were with one line
        // each: the room went underneath them both, and never between them.
        assert!((gap(&menu) - settled).abs() < 1e-3);
    }

    /// A row that is there to be read is open before the highlight reaches it —
    /// which it never does.
    ///
    /// The panel behind an announcement is a paragraph on a row the highlight
    /// steps over. Left to wait for one it would show a single line of the body
    /// and an ellipsis, on the one panel whose whole purpose is the rest of it.
    #[test]
    fn a_row_that_is_there_to_be_read_is_open_before_it_is_reached() {
        let height = 1080.0;
        let width = 1920.0;
        let scale = guide_scale(height);
        let mut menu = Menu::default();
        let mut body =
            MenuEntry::new(Command::DismissNotification(1), "a paragraph ".repeat(20)).reading();
        body.lines = 3;
        assert!(menu.open_at(
            [40.0, 200.0, 68.0, 68.0],
            Some(Title::new("Hello World")),
            vec![
                body,
                MenuEntry::new(Command::DismissNotification(1), "Dismiss").group(1),
                MenuEntry::new(Command::Dismiss, "Not now")
                    .group(1)
                    .disabled(),
            ],
            mixer_rows_that_fit(height),
        ));
        while menu.animate(0.05) < 1.0 {}

        assert_ne!(menu.selected(), 0, "the highlight stepped over it");
        let read = context_menu_row_rect(width, height, &menu, 0).unwrap();
        let runs = row_writing(&menu, 0, width, height);
        assert_eq!(runs.len(), 1, "the paragraph and nothing else: {runs:?}");
        assert_eq!(runs[0].2, 3, "all three lines of it");
        assert!(
            (read[3] - (CONTEXT_ROW + 2.0 * CONTEXT_LABEL_LINE - GUIDE_ROW_PADDING * 2.0) * scale)
                .abs()
                < 1.0,
            "and the row is tall enough to hold them: {read:?}"
        );

        // It does not move, because there is nothing for it to wait for.
        for _ in 0..40 {
            menu.animate(0.05);
        }
        assert_eq!(
            context_menu_row_rect(width, height, &menu, 0).unwrap(),
            read
        );

        // And it is set in the shell's white, not in the grey that means a
        // control is not for you. Its outline has already said it cannot be
        // pressed; saying so a second time costs a paragraph its contrast.
        let scene = context_scene(&menu, width, height);
        let paragraph = scene
            .texts
            .iter()
            .find(|text| text.content.starts_with("a paragraph"))
            .expect("the paragraph is drawn");
        let unavailable = scene
            .texts
            .iter()
            .find(|text| text.content == "Not now")
            .expect("and so is the row that is genuinely not for them");
        assert!(
            paragraph.color[3] > unavailable.color[3] + 0.3,
            "not dimmed the way an unavailable row is: {:?} against {:?}",
            paragraph.color,
            unavailable.color
        );
        assert!(
            paragraph.color[3] >= 0.9,
            "and read at the shell's own white: {:?}",
            paragraph.color
        );
    }

    /// A row keeps the corners of the rows around it while it opens out.
    ///
    /// The radius follows the row's height, and the height is now something
    /// that moves: taken off the grown one, the single row somebody is reading
    /// would be visibly rounder than its neighbours for as long as they read it
    /// — the very inconsistency that having one question decide the corners was
    /// written to end.
    #[test]
    fn a_row_that_opens_out_keeps_the_corners_of_the_rows_around_it() {
        let height = 1080.0;
        let scale = guide_scale(height);
        let mut menu = announcements(3, 3, height);
        let radius = |menu: &Menu| {
            let rect = context_menu_row_rect(1920.0, height, menu, 1).unwrap();
            context_chip_radius(&menu.entries()[1], rect[3], scale)
        };

        let shut = radius(&menu);
        for _ in 0..40 {
            menu.animate(0.05);
        }
        assert!(
            (radius(&menu) - shut).abs() < 1e-3,
            "the same corners open as shut"
        );

        // And still cut to the share of itself a track is cut to — see
        // `a_stacked_row_is_cornered_like_a_track_and_not_like_a_button`.
        let tracks = mixer([0.5; 3], height);
        let track = context_menu_row_rect(1920.0, height, &tracks, 0).unwrap();
        let settled =
            context_menu_row_rect(1920.0, height, &announcements(3, 3, height), 1).unwrap()[3];
        assert!(
            (shut / settled
                - context_chip_radius(&tracks.entries()[0], track[3], scale) / track[3])
                .abs()
                < 0.01
        );
    }

    /// The announcement panel is wider than every other one, and only it.
    #[test]
    fn only_the_announcement_panel_takes_the_extra_width() {
        let (width, height) = (1920.0, 1080.0);
        let scale = guide_scale(height);
        let announced = context_menu_rect(width, height, &announcements(1, 1, height));
        let commands = context_menu_rect(width, height, &raised([300.0; 4], 4, height));
        assert!(
            (announced[2] - commands[2] - NOTIFICATION_EXTRA_WIDTH * scale).abs() < 1.0,
            "{announced:?} against {commands:?}"
        );
        // And the bubble in the corner keeps the ordinary width — it has
        // neither of the two things the panel took the room for.
        assert!((TOAST_WIDTH - CONTEXT_WIDTH).abs() < 1e-3);
    }

    /// Every announcement carries a button on its right-hand end, reached by
    /// stepping sideways onto it, and a press there throws that one away
    /// without putting the panel down.
    #[test]
    fn a_row_carries_its_own_way_of_being_thrown_away() {
        let (width, height) = (1920.0, 1080.0);
        let mut menu = announcements(1, 1, height);
        assert_eq!(menu.selected(), 1, "opened on the announcement");
        assert!(!menu.on_aside(), "and on the row, not on its button");

        // The button takes the right-hand end of the row's line, top to bottom.
        let row = context_menu_row_rect(width, height, &menu, 1).unwrap();
        let button = context_menu_aside_rect(width, height, &menu, 1).unwrap();
        assert!(
            button[0] >= row[0] && button[0] + button[2] <= row[0] + row[2] + 0.01,
            "{button:?} inside {row:?}"
        );
        assert!(
            (button[0] + button[2] - row[0] - row[2]).abs() < 0.01,
            "at the end"
        );
        assert_eq!(
            (button[1], button[3]),
            (row[1], row[3]),
            "the button and the row it is on are the same height"
        );

        // And the row's own face stops before it, with the air between them
        // showing: they are two chips side by side rather than a button drawn
        // on top of a row.
        let chip = context_menu_chip_rect(width, height, &menu, 1).unwrap();
        assert!(
            (button[0] - (chip[0] + chip[2]) - CONTEXT_ASIDE_GAP * guide_scale(height)).abs()
                < 0.01,
            "the row's face {chip:?} runs into the button at {button:?}"
        );
        // The writing stops with it.
        let runs = context_scene(&menu, width, height).texts;
        for text in runs
            .iter()
            .filter(|text| text.y >= row[1] - 1.0 && text.y <= row[1] + row[3] && text.x >= row[0])
        {
            assert!(
                text.x + text.max_width <= chip[0] + chip[2] + 0.01,
                "{:?} runs past the row's face at {chip:?}",
                text.content
            );
        }

        // Right steps onto it and the highlight goes with it; Left comes back.
        // The light lands on one of the two and never on both, which is the
        // whole of what says which the next press will reach.
        assert!(menu.move_aside(1));
        assert!(menu.on_aside());
        assert_eq!(menu.selected(), 1, "still the same row underneath");
        assert_eq!(
            context_menu_highlight_rect(width, height, &menu),
            Some(button)
        );
        assert!(!menu.move_aside(1), "and there is nowhere further to go");
        assert!(menu.move_aside(-1));
        assert!(!menu.on_aside());
        assert_eq!(
            context_menu_highlight_rect(width, height, &menu),
            Some(chip),
            "the light on the row stops short of its button"
        );

        // A press on the button carries the row's *other* command, and leaves
        // the panel standing so the next one can be thrown away too.
        menu.move_aside(1);
        assert_eq!(menu.choose(), Some(Command::DismissNotification(1)));
        assert!(menu.is_open(), "the panel stayed up");
        assert!(
            menu.aside_press_progress(1).is_some(),
            "the button went down"
        );
        assert!(
            menu.press_progress(1).is_none(),
            "and the row underneath did not"
        );

        // Nothing to step onto on a row without one.
        let mut plain = raised([300.0; 4], 4, height);
        assert!(!plain.move_aside(1));
        assert!(!plain.on_aside());
    }

    /// The button on the end of a row is as tall as the row, and stays as tall
    /// as it while the row opens out under the highlight.
    ///
    /// It used to be a square cut from the height the row settles at, which on
    /// an announcement opened out to six lines of somebody's sentence left a
    /// stub of a button at the top of a deep row with nothing under it. Its
    /// *width* is still the settled one, because that is the side that takes
    /// room from the writing.
    #[test]
    fn the_button_on_a_row_is_as_tall_as_the_row_however_far_it_opens() {
        let (width, height) = (1920.0, 1080.0);
        let mut menu = announcements(3, 3, height);
        let settled = context_menu_aside_rect(width, height, &menu, 1).unwrap();
        assert!(
            (settled[2] - settled[3]).abs() < 0.01,
            "square on a row that has not opened out: {settled:?}"
        );

        for _ in 0..40 {
            menu.animate(0.05);
        }
        let row = context_menu_row_rect(width, height, &menu, 1).unwrap();
        let opened = context_menu_aside_rect(width, height, &menu, 1).unwrap();
        assert!(
            opened[3] > settled[3] + 1.0,
            "the row opened out and the button did not: {opened:?}"
        );
        assert_eq!((opened[1], opened[3]), (row[1], row[3]));
        assert!(
            (opened[2] - settled[2]).abs() < 0.01,
            "and it is no wider for it: {opened:?} against {settled:?}"
        );

        // The mark on it is the same size on both, being drawn off the width.
        let mark = |menu: &Menu| {
            let button = context_menu_aside_rect(width, height, menu, 1).unwrap();
            context_scene(menu, width, height)
                .quads
                .iter()
                .find(|quad| quad.x > button[0] && quad.x + quad.w < button[0] + button[2])
                .map(|quad| quad.h)
                .expect("the mark on the button")
        };
        assert!((mark(&menu) - mark(&announcements(3, 3, height))).abs() < 0.01);
    }

    /// Nothing of the row is drawn under its button.
    ///
    /// The button used to stand on the row's own face, which is a button on top
    /// of a button — and under the highlight it was worse than untidy: the lit
    /// capsule ran the whole width of the row, the button sat in the middle of
    /// that light, and the pair stopped saying which of the two the next press
    /// would reach.
    #[test]
    fn a_row_draws_nothing_under_its_own_button() {
        let (width, height) = (1920.0, 1080.0);
        let menu = announcements(1, 1, height);
        let row = context_menu_row_rect(width, height, &menu, 1).unwrap();
        let chip = context_menu_chip_rect(width, height, &menu, 1).unwrap();
        let button = context_menu_aside_rect(width, height, &menu, 1).unwrap();
        let scene = context_scene(&menu, width, height);
        for quad in scene.quads.iter().filter(|quad| {
            // What is on this row's own line and belongs to the row rather than
            // to the button: the panel's glass and the selection's bloom are
            // both taller than a row, and neither is the row.
            quad.y >= row[1] - 1.0
                && quad.y + quad.h <= row[1] + row[3] + 1.0
                && quad.x < button[0] - 0.01
        }) {
            assert!(
                quad.x + quad.w <= chip[0] + chip[2] + 0.01,
                "{quad:?} reaches under the button at {button:?}"
            );
        }
    }

    /// One announcement opened out of the list: the paragraph it sent, a button
    /// it offered and a way out — under a heading of its own that takes
    /// `title_lines` lines to say.
    fn opened_announcement(title_lines: u8, height: f32) -> Menu {
        let mut menu = Menu::default();
        let mut body =
            MenuEntry::new(Command::DismissNotification(1), "a paragraph ".repeat(20)).reading();
        body.lines = 3;
        assert!(menu.open_at(
            [40.0, 200.0, 68.0, 68.0],
            Some(Title::new("Everything the program had to say in its heading").lines(title_lines)),
            vec![
                body,
                MenuEntry::new(Command::InvokeNotification(1, 0), "Open File"),
                MenuEntry::new(Command::DismissNotification(1), "Dismiss").group(1),
            ],
            context_menu_rows_that_fit(height),
        ));
        menu.widen(NOTIFICATION_EXTRA_WIDTH);
        menu.set_window(menu_rows_that_fit(&menu, height));
        while menu.animate(0.05) < 1.0 {}
        menu
    }

    /// A heading too long for one line is wrapped rather than cut.
    ///
    /// The header of this panel is the announcement's own summary — a sentence
    /// the program that sent it wrote, and the thing the user pressed the row
    /// to read. It used to be set on one line and ellipsised, which hid the end
    /// of the very line the panel was raised to show.
    #[test]
    fn a_heading_too_long_for_one_line_opens_out_instead_of_being_cut() {
        let (width, height) = (1920.0, 1080.0);
        let scale = guide_scale(height);
        let one = opened_announcement(1, height);
        let three = opened_announcement(3, height);
        let grown = 2.0 * CONTEXT_TITLE_LINE * scale;
        assert_eq!(
            one.visible_rows(),
            three.visible_rows(),
            "the same rows are on both panels"
        );

        // The panel is taller by exactly the two lines the heading took...
        let short = context_menu_rect(width, height, &one);
        let long = context_menu_rect(width, height, &three);
        assert!(
            (long[3] - short[3] - grown).abs() < 1.0,
            "{long:?} against {short:?}"
        );

        // ...and the rows start that much further down it, so the heading is
        // read in the room the panel grew rather than over the paragraph.
        let under = |menu: &Menu, panel: [f32; 4]| {
            context_menu_row_rect(width, height, menu, 0).unwrap()[1] - panel[1]
        };
        assert!(
            (under(&three, long) - under(&one, short) - grown).abs() < 1.0,
            "the first row did not move down with the heading"
        );

        // The heading itself is drawn with room for all three lines, and it is
        // still the first thing on the panel.
        let scene = context_scene(&three, width, height);
        let heading = scene
            .texts
            .iter()
            .find(|text| text.content.starts_with("Everything"))
            .expect("the heading is drawn");
        assert_eq!(heading.lines, 3, "all three lines of it");
        assert!(
            heading.y + heading.size * 3.0
                <= context_menu_row_rect(width, height, &three, 0).unwrap()[1],
            "the heading runs into the paragraph under it"
        );

        // And a panel the shell titled itself is untouched: one line, and the
        // same header it always had.
        let plain = raised([300.0; 4], 4, height);
        assert_eq!(plain.title_lines(), 1);
    }

    /// A panel raised to show one announcement keeps the buttons under it,
    /// however long the announcement is.
    ///
    /// The row count used to measure the tallest row and then count that height
    /// over and over, which for this panel — a paragraph several inches deep
    /// with three buttons under it — answered that one row fits. That leaves
    /// the reader the words and no way to act on them.
    #[test]
    fn a_paragraph_does_not_push_the_buttons_off_its_own_panel() {
        for [width, height] in SCREENS {
            let mut menu = Menu::default();
            let mut body =
                MenuEntry::new(Command::DismissNotification(1), "a paragraph ".repeat(60))
                    .reading();
            body.lines = context_read_lines(height);
            assert!(menu.open_at(
                [40.0, 200.0, 68.0, 68.0],
                Some(Title::new("Hello World")),
                vec![
                    body,
                    MenuEntry::new(Command::InvokeNotification(1, 0), "Install now"),
                    MenuEntry::new(Command::InvokeNotification(1, 1), "Remind me later"),
                    MenuEntry::new(Command::DismissNotification(1), "Dismiss").group(1),
                ],
                context_menu_rows_that_fit(height),
            ));
            menu.widen(NOTIFICATION_EXTRA_WIDTH);
            menu.set_window(menu_rows_that_fit(&menu, height));
            while menu.animate(0.05) < 1.0 {}

            assert_eq!(
                menu.visible_rows(),
                4,
                "{width}x{height}: the paragraph and all three buttons"
            );
            let panel = context_menu_rect(width, height, &menu);
            let last = context_menu_row_rect(width, height, &menu, 3).unwrap();
            assert!(
                last[1] + last[3] <= panel[1] + panel[3] + 1.0,
                "{width}x{height}: the way out is still on the panel"
            );
            assert!(
                panel[1] >= PANEL_INSET * guide_scale(height) - 0.01
                    && panel[1] + panel[3] <= height - PANEL_INSET * guide_scale(height) + 0.01,
                "{width}x{height}: and the panel is still on the display"
            );
        }
    }

    /// A row that is there to be read is not cut at three lines, because the
    /// panel it is on was raised to show it.
    #[test]
    fn what_a_panel_was_raised_to_show_is_cut_only_by_the_display() {
        for [_, height] in SCREENS {
            let allowed = context_read_lines(height);
            assert!(
                allowed > CONTEXT_MAX_LINES,
                "{height}: a paragraph gets more than a row in a list does"
            );
            // And it leaves at least half the display for the buttons under it
            // and the way out below them.
            let scale = guide_scale(height);
            assert!(
                allowed as f32 * CONTEXT_LABEL_LINE * scale <= height * 0.5 + 1.0,
                "{height}: {allowed} lines is more than half the screen"
            );
        }
    }

    /// The panel is built to hold the row that opens out, on a display it only
    /// just fits on.
    ///
    /// It is sized to its rows, so a row growing under the highlight grows the
    /// panel — and a panel already reaching the bottom of the display would
    /// have nowhere to put the growth but over the edge, cutting off the row at
    /// the end of the list for as long as somebody stood on a long one.
    #[test]
    fn the_panel_keeps_room_for_the_row_that_will_open_out() {
        let width = 1920.0;
        for height in [1080.0, 900.0, 768.0, 600.0] {
            let mut menu = announcements(3, 3, height);
            let rows = menu.visible_rows();
            for _ in 0..40 {
                menu.animate(0.05);
            }
            assert_eq!(rows, menu.visible_rows(), "the same rows are drawn");
            let panel = context_menu_rect(width, height, &menu);
            let last = context_menu_row_rect(width, height, &menu, rows - 1).unwrap();
            assert!(
                last[1] + last[3] <= panel[1] + panel[3] + 1.0,
                "the last row is still on the panel at {height}: {last:?} in {panel:?}"
            );
            assert!(
                panel[1] + panel[3] <= height - PANEL_INSET * guide_scale(height) + 1.0,
                "and the panel is still on the display at {height}: {panel:?}"
            );
        }
    }

    /// The mixer as the guide raises it: two applications and the session's own
    /// output under them, out of the tile in the column.
    fn mixer(levels: [f32; 3], height: f32) -> Menu {
        let mut menu = Menu::default();
        let level = |value: f32| Level {
            value,
            muted: false,
        };
        assert!(menu.open_at(
            [40.0, 200.0, 68.0, 68.0],
            None,
            vec![
                MenuEntry::new(Command::MuteApplication(1), "A Game")
                    .icon("game")
                    .level(level(levels[0])),
                MenuEntry::new(Command::MuteApplication(2), "A Browser")
                    .icon("browser")
                    .level(level(levels[1])),
                MenuEntry::new(Command::MuteShell, "System")
                    .icon(icons::CATEGORY_SYSTEM)
                    .level(level(levels[2]))
                    .group(1),
            ],
            mixer_rows_that_fit(height),
        ));
        while menu.animate(0.05) < 1.0 {}
        menu
    }

    /// A track's row is taller than a command's, and the panel is built to hold
    /// it: a mixer raised on a display too short for its rows would run off the
    /// bottom of the screen, which is the one thing the row count exists to
    /// stop.
    #[test]
    fn a_mixer_row_is_given_more_of_the_column_than_a_command() {
        for [width, height] in SCREENS {
            let inset = PANEL_INSET * guide_scale(height);
            let menu = mixer([0.3, 0.6, 0.9], height);
            let [_, y, _, h] = context_menu_rect(width, height, &menu);
            assert!(
                y >= inset - 0.01 && y + h <= height - inset + 0.01,
                "{width}x{height}: the panel runs from {y} to {}",
                y + h
            );

            let track = context_menu_row_rect(width, height, &menu, 0).unwrap();
            let command = raised([300.0, 300.0, 160.0, 160.0], 4, height);
            let plain = context_menu_row_rect(width, height, &command, 0).unwrap();
            assert!(
                track[3] > plain[3],
                "{width}x{height}: a track got a command's row"
            );
            assert!(
                mixer_rows_that_fit(height) <= context_menu_rows_that_fit(height),
                "{width}x{height}: taller rows were said to fit as well"
            );
            // And the answer the shell asks for every frame follows the rows
            // the menu actually holds — it walks them, so it never claims room
            // for rows that are not there.
            assert_eq!(
                menu_rows_that_fit(&menu, height),
                mixer_rows_that_fit(height).min(menu.entries().len())
            );
            assert_eq!(
                menu_rows_that_fit(&command, height),
                context_menu_rows_that_fit(height).min(command.entries().len())
            );
        }
    }

    /// Each row carries the application's own picture, its name, and a track
    /// filled to where that application stands — the three things the panel is
    /// for, and nothing that says the level twice.
    #[test]
    fn every_mixer_row_shows_what_is_playing_and_how_loud() {
        let (width, height) = (1920.0, 1080.0);
        let menu = mixer([0.25, 0.5, 1.0], height);
        let scene = context_scene(&menu, width, height);

        let said: Vec<&str> = scene
            .texts
            .iter()
            .map(|text| text.content.as_str())
            .collect();
        assert_eq!(said, vec!["A Game", "A Browser", "System"]);

        for (index, level) in [0.25, 0.5, 1.0].into_iter().enumerate() {
            let [rx, ry, rw, rh] = context_menu_row_rect(width, height, &menu, index).unwrap();
            let inside = |quad: &&Quad| {
                quad.x >= rx - 0.5
                    && quad.y >= ry - 0.5
                    && quad.x + quad.w <= rx + rw + 0.5
                    && quad.y + quad.h <= ry + rh + 0.5
            };
            let parts: Vec<&Quad> = scene.quads.iter().filter(inside).collect();

            // The icon: square, and the largest picture on the row.
            let icon = parts
                .iter()
                .find(|quad| quad.slot == Named::slot_of("game") && quad.w > rh * 0.4)
                .or_else(|| {
                    parts.iter().find(|quad| {
                        quad.w > rh * 0.4 && quad.h > rh * 0.4 && quad.slot != SOLID_SLOT
                    })
                })
                .expect("an icon");
            assert!((icon.w - icon.h).abs() < 0.5, "the icon is not square");

            // The speaker at the head of the track says the sound is on.
            assert!(
                parts
                    .iter()
                    .any(|quad| quad.slot == Named::slot_of(icons::VOLUME)),
                "row {index} has no speaker"
            );

            // The track, and the part of it that is filled: the widest run on
            // the row, and the shorter one starting at the same place.
            let groove = parts
                .iter()
                .filter(|quad| quad.slot == SOLID_SLOT && quad.h < rh * 0.2)
                .max_by(|a, b| a.w.total_cmp(&b.w))
                .expect("a track");
            let filled = parts
                .iter()
                .filter(|quad| {
                    quad.slot == SOLID_SLOT
                        && (quad.x - groove.x).abs() < 0.5
                        && quad.h == groove.h
                        && quad.w <= groove.w
                })
                .min_by(|a, b| a.w.total_cmp(&b.w))
                .expect("a fill");
            assert!(
                (filled.w / groove.w - level).abs() < 0.02,
                "row {index} stands at {} and is filled to {}",
                level,
                filled.w / groove.w
            );
            assert!(
                groove.x > icon.x + icon.w,
                "row {index} draws its track over its icon"
            );
        }
    }

    /// The dot on the end of the fill stays inside the groove it runs along, at
    /// both ends of the range.
    ///
    /// A control at the top of its range would otherwise hang half a handle
    /// past its own track — over whatever the track was laid on, and out
    /// through the rounded corner of the chip on a row that carries one.
    #[test]
    fn a_handle_never_leaves_the_end_of_its_track() {
        for value in [0.0, 0.05, 0.5, 0.95, 1.0] {
            let drawn = track(
                [100.0, 50.0, 200.0, 0.0],
                Level {
                    value,
                    muted: false,
                },
                1.0,
                1.0,
            );
            let groove = drawn[0];
            let handle = drawn.last().unwrap();
            assert!(
                handle.x >= groove.x - 0.01 && handle.x + handle.w <= groove.x + groove.w + 0.01,
                "at {value} the handle runs {}..{} in a track {}..{}",
                handle.x,
                handle.x + handle.w,
                groove.x,
                groove.x + groove.w
            );
            // And the fill is still the reading: keeping the dot in is worth
            // nothing if it moves the answer the row is giving.
            if let Some(fill) = drawn.get(1).filter(|_| drawn.len() > 2) {
                assert!(
                    (fill.w / groove.w - value).abs() < 0.001,
                    "at {value} the track is filled to {}",
                    fill.w / groove.w
                );
            }
        }
    }

    /// The light that glides onto a row is the shape of that row.
    ///
    /// It is the row being lit rather than a second object laid over it, so a
    /// capsule sitting on a track's rounded square reads as the selected row
    /// having changed shape — which is what it looked like.
    #[test]
    fn the_light_takes_the_shape_of_the_row_it_lands_on() {
        let (width, height) = (1920.0, 1080.0);
        let roundness = |menu: &Menu| {
            let scene = context_scene(menu, width, height);
            let round = |gloss: f32| {
                scene
                    .quads
                    .iter()
                    .find(|quad| quad.gloss == gloss && quad.thickness > 0.0)
                    .map(|quad| quad.radius / quad.h)
                    .expect("a chip")
            };
            (round(GLOSS_FULL), round(GLOSS_QUIET))
        };

        let (lit, chip) = roundness(&mixer([0.5, 0.5, 0.5], height));
        assert!((lit - chip).abs() < 0.001, "lit {lit} against chip {chip}");
        assert!(lit < 0.5 - 0.01, "a track's row is not a stadium: {lit}");

        // And a column of commands is capsules, as it always was.
        let (lit, chip) = roundness(&raised([300.0, 300.0, 160.0, 160.0], 4, height));
        assert!((lit - 0.5).abs() < 0.001 && (chip - 0.5).abs() < 0.001);
    }

    /// Silenced, the row says so where the reading is: the speaker is struck
    /// out and the fill stays where it was, because turning the sound back on
    /// must not look like it also turned it up.
    #[test]
    fn a_silenced_application_keeps_its_reading_and_says_it_is_silent() {
        let (width, height) = (1920.0, 1080.0);
        let mut menu = mixer([0.7, 0.5, 0.9], height);
        let loud = context_scene(&menu, width, height);

        let entries: Vec<MenuEntry> = menu
            .entries()
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let mut entry = entry.clone();
                if index == 0 {
                    entry.level = Some(Level {
                        value: 0.7,
                        muted: true,
                    });
                }
                entry
            })
            .collect();
        assert!(menu.refresh(entries));
        let quiet = context_scene(&menu, width, height);

        let struck = |scene: &Scene| {
            scene
                .quads
                .iter()
                .any(|quad| quad.slot == Named::slot_of(icons::VOLUME_MUTED))
        };
        assert!(!struck(&loud) && struck(&quiet));

        let fill_of = |scene: &Scene| {
            let [rx, ry, rw, rh] = context_menu_row_rect(width, height, &menu, 0).unwrap();
            scene
                .quads
                .iter()
                .filter(|quad| {
                    quad.slot == SOLID_SLOT
                        && quad.h < rh * 0.2
                        && quad.x >= rx
                        && quad.y >= ry
                        && quad.x + quad.w <= rx + rw + 0.5
                })
                .map(|quad| quad.w)
                .fold(0.0_f32, f32::min)
        };
        assert!(
            (fill_of(&loud) - fill_of(&quiet)).abs() < 0.5,
            "silencing it emptied the track"
        );
    }

    /// The panel grows out of the tile the user pressed, not out of the middle
    /// of the sidebar — the same promise every other menu makes about the
    /// control it was raised from.
    #[test]
    fn the_mixer_grows_out_of_its_tile_in_the_column() {
        let (width, height) = (1920.0, 1080.0);
        let items = [Item::Pointer, Item::Mixer, Item::Volume, Item::Resume];
        let tile = menu_item_rect(&items, 1, width, height);

        let mut menu = Menu::default();
        assert!(menu.open_at(
            tile,
            None,
            vec![MenuEntry::new(Command::MuteShell, "System").level(Level {
                value: 0.5,
                muted: false,
            })],
            mixer_rows_that_fit(height),
        ));
        while menu.animate(0.05) < 1.0 {}

        let shut = context_menu_bounds(width, height, &menu, 0.0);
        assert!(
            (shut[0] + shut[2] * 0.5 - (tile[0] + tile[2] * 0.5)).abs() < 0.01
                && (shut[1] + shut[3] * 0.5 - (tile[1] + tile[3] * 0.5)).abs() < 0.01,
            "shut, it is centred on the tile: {shut:?} against {tile:?}"
        );
        // And it stands beside the sidebar's column rather than over the tile.
        let [x, _, w, _] = context_menu_rect(width, height, &menu);
        assert!(x >= tile[0] + tile[2] || x + w <= tile[0]);
    }

    /// A row that cannot be chosen is a different kind of thing rather than a
    /// quieter one — an outline where its chip would be, exactly as an
    /// unreachable tile is drawn in the guide.
    #[test]
    fn a_row_that_cannot_be_chosen_is_outlined_rather_than_dimmed() {
        let mut menu = Menu::default();
        menu.open_at(
            [300.0, 300.0, 160.0, 160.0],
            None,
            vec![
                MenuEntry::new(Command::Placeholder("a"), "Live"),
                MenuEntry::new(Command::Placeholder("b"), "Not from here").disabled(),
            ],
            8,
        );
        while menu.animate(0.05) < 1.0 {}
        let (width, height) = (1920.0, 1080.0);
        let scene = context_scene(&menu, width, height);

        let dead = context_menu_row_rect(width, height, &menu, 1).unwrap();
        let chip = scene
            .quads
            .iter()
            .find(|quad| (quad.y - dead[1]).abs() < 0.5 && (quad.h - dead[3]).abs() < 0.5)
            .expect("the row is drawn");
        assert!(chip.border > 0.0, "it was given a chip after all");
        assert_eq!(chip.thickness, 0.0, "and glass with it");

        // The highlight cannot reach it, so it is never the one lit.
        assert!(!menu.move_selection(1));
        assert_eq!(menu.selected(), 0);
    }

    /// A row there is no coming back from says so with the light on it and
    /// never with its letters.
    ///
    /// Both halves matter and they are the same fact from either end: the
    /// label is the plain white every other row has, whether or not the
    /// highlight is on it, and the capsule that arrives is warm rather than the
    /// accent's. A red label on the panel's dark glass was the way this used to
    /// be said, and it was the least readable text in the shell.
    #[test]
    fn a_grave_row_is_warned_of_by_its_light_and_not_by_its_label() {
        let (width, height) = (1920.0, 1080.0);
        let ordinary = MenuEntry::new(Command::Placeholder("keep"), "Keep");
        let grave = MenuEntry::new(Command::Placeholder("remove"), "Uninstall").grave();
        let label = |scene: &Scene, of: &str| {
            scene
                .texts
                .iter()
                .find(|text| text.content == of)
                .map(|text| text.color)
                .expect("the row is drawn")
        };
        let lit = |scene: &Scene, row: [f32; 4]| {
            scene
                .quads
                .iter()
                .find(|quad| {
                    quad.slot == SOLID_SLOT
                        && quad.thickness > 0.0
                        && (quad.y - row[1]).abs() < 0.5
                        && (quad.h - row[3]).abs() < 0.5
                })
                .map(|quad| quad.color)
                .expect("the selected row is lit")
        };

        // On the ordinary row first, so the grave one is drawn unselected.
        let mut menu = Menu::default();
        menu.open_at([300.0, 300.0, 160.0, 160.0], None, vec![ordinary, grave], 8);
        while menu.animate(0.05) < 1.0 {}
        let resting = context_scene(&menu, width, height);
        // The colour, not the strength: an unselected row of any kind is drawn
        // a little softer than the one the highlight is on.
        assert_eq!(
            label(&resting, "Uninstall")[..3],
            label(&resting, "Keep")[..3],
            "the grave row is written in a colour of its own"
        );

        // And then on it.
        assert!(menu.move_selection(1));
        while menu.animate(0.05) < 1.0 {}
        let selected = context_scene(&menu, width, height);
        assert_eq!(
            label(&selected, "Uninstall")[..3],
            label(&resting, "Keep")[..3],
            "selecting the grave row turned its label a colour of its own"
        );

        let row = context_menu_row_rect(width, height, &menu, 1).unwrap();
        let theme = crate::theme::theme();
        assert_eq!(
            lit(&selected, row)[..3],
            theme.danger.a(1.0)[..3],
            "the light that reached the grave row is the ordinary accent"
        );
        assert_ne!(theme.danger.a(1.0)[..3], theme.accent.a(1.0)[..3]);
    }

    /// The busiest screen a context menu can appear on still fits inside the
    /// renderer's snapshot budget.
    ///
    /// The guide's, which is already the deepest composition the shell draws,
    /// with a menu raised over a window card on top of it. Running out does not
    /// fail, it *degrades* — panes past the budget refract the frame as it
    /// stood earlier in the draw order — so it is asserted rather than left to
    /// be noticed.
    #[test]
    fn a_context_menu_over_the_guide_fits_inside_the_snapshot_budget() {
        let xmb = Xmb::new(vec![Category {
            id: "a",
            title: "Settings",
            icon: "a",
            entries: vec![app("first"), app("second"), app("third")],
        }]);
        let (width, height) = (1920.0, 1080.0);
        let mut scene = build_with(&xmb, &Cursor::new(1), width, height, true, &AllSlots);
        scene.place_into([900.0, 200.0, 900.0, 520.0], width, height);

        let mut guide = Guide::default();
        guide.set_bars(BOTH_BARS);
        guide.open();
        guide.backdate_open(2.0);
        let cards = [card("Celeste")];
        let over = guide_scene(&guide, Some("Celeste"), None, &cards, None);
        scene.quads.extend(over.quads);

        let menu = raised([900.0, 200.0, 900.0, 520.0], 5, height);
        scene
            .quads
            .extend(context_scene(&menu, width, height).quads);

        let needed = crate::gpu::snapshots_needed(&scene.quads);
        assert!(
            needed <= crate::gpu::MAX_GLASS_BATCHES,
            "the guide with a context menu over it needs {needed} snapshots \
             and the budget is {}",
            crate::gpu::MAX_GLASS_BATCHES,
        );
    }

    // --- the centred panel -------------------------------------------------

    /// The information panel for an application, open, raised out of `from`.
    fn informed(from: [f32; 4]) -> Dialog {
        let mut dialog = Dialog::default();
        assert!(dialog.ask(
            from,
            Some("an-application".to_string()),
            vec![
                Line::Heading("Example Editor".to_string()),
                Line::Note("An editor that does not exist".to_string()),
                Line::Rule,
                Line::field("Version", "3.4.1-2"),
                Line::field("Size", "15.7 MiB"),
                Line::Rule,
            ],
            vec![MenuEntry::new(Command::Dismiss, "Close")],
            0,
        ));
        while dialog.buttons.animate(0.05) < 1.0 {}
        dialog
    }

    /// The uninstall question, open on the answer that declines it — which is
    /// also the one drawn first, as it is in every question the shell asks.
    fn questioned(from: [f32; 4]) -> Dialog {
        let mut dialog = Dialog::default();
        assert!(dialog.ask(
            from,
            Some("an-application".to_string()),
            vec![
                Line::Note("Do you want to uninstall the".to_string()),
                Line::Heading("Example Editor?".to_string()),
                Line::Rule,
            ],
            vec![
                MenuEntry::new(Command::Dismiss, "No"),
                MenuEntry::new(Command::ConfirmUninstall, "Yes").grave(),
            ],
            0,
        ));
        while dialog.buttons.animate(0.05) < 1.0 {}
        dialog
    }

    fn dialog_scene(dialog: &Dialog, width: f32, height: f32) -> Scene {
        build_dialog(
            DialogView {
                dialog,
                highlight: None,
                open: 1.0,
                behind: 0.6,
                time: 0.0,
                slots: &Named,
            },
            width,
            height,
        )
    }

    /// Everything the panel was given is on it, in the order it was given, and
    /// each named value is a pair read across the panel rather than two runs
    /// that happen to be near each other.
    #[test]
    fn the_information_panel_says_the_four_things_it_was_given() {
        let (width, height) = (1920.0, 1080.0);
        let dialog = informed([200.0, 400.0, 160.0, 160.0]);
        let scene = dialog_scene(&dialog, width, height);

        let said: Vec<&str> = scene
            .texts
            .iter()
            .map(|text| text.content.as_str())
            .collect();
        assert_eq!(
            said,
            vec![
                "Example Editor",
                "An editor that does not exist",
                "Version",
                "3.4.1-2",
                "Size",
                "15.7 MiB",
                "Close",
            ]
        );

        let run = |content: &str| {
            scene
                .texts
                .iter()
                .find(|text| text.content == content)
                .unwrap_or_else(|| panic!("{content:?} should be on the panel"))
        };
        for (label, value) in [("Version", "3.4.1-2"), ("Size", "15.7 MiB")] {
            let (label, value) = (run(label), run(value));
            assert!(
                (label.y - value.y).abs() < 0.01,
                "the label and its value are not on one baseline"
            );
            assert_eq!(label.align, TextAlign::Left);
            assert_eq!(value.align, TextAlign::Right);
            assert!(value.x > label.x, "the value should be the far column");
        }

        // The application's own icon, at the head of it.
        let [ix, iy, _, _] = dialog_rect(width, height, &dialog);
        assert!(scene.quads.iter().any(|quad| quad.slot
            == Named.slot_for(Some("an-application")).unwrap()
            && quad.x > ix
            && quad.y > iy));
    }

    /// It settles in the middle of the display, whatever it was raised from and
    /// whatever the display is.
    #[test]
    fn the_panel_settles_in_the_middle_of_the_display() {
        for [width, height] in SCREENS {
            for anchor in [
                [40.0, 40.0, 90.0, 90.0],
                [width - 200.0, height - 300.0, 90.0, 90.0],
            ] {
                let dialog = informed(anchor);
                let [x, y, w, h] = dialog_rect(width, height, &dialog);
                assert!(
                    ((x + w * 0.5) - width * 0.5).abs() < 0.01
                        && ((y + h * 0.5) - height * 0.5).abs() < 0.01,
                    "{width}x{height}: {:?} is not centred",
                    [x, y, w, h]
                );
                assert!(x >= 0.0 && y >= 0.0 && x + w <= width && y + h <= height);
            }
        }
    }

    /// It grows out of the row that was pressed, on the menu's own flight: one
    /// shape from there to the middle, with nothing legible on it until there
    /// is enough panel to read it on.
    #[test]
    fn the_panel_grows_out_of_the_row_that_opened_it() {
        let from = [400.0, 500.0, 380.0, 54.0];
        let (width, height) = (1920.0, 1080.0);
        let dialog = questioned(from);

        let shut = dialog_bounds(width, height, &dialog, 0.0);
        assert!(
            (shut[0] + shut[2] * 0.5 - (from[0] + from[2] * 0.5)).abs() < 0.01
                && (shut[1] + shut[3] * 0.5 - (from[1] + from[3] * 0.5)).abs() < 0.01,
            "shut, it is centred on the row it came from: {shut:?}"
        );
        assert_eq!(
            dialog_bounds(width, height, &dialog, 1.0),
            dialog_rect(width, height, &dialog)
        );
        let [_, _, gw, gh] = dialog_bounds(width, height, &dialog, 0.5);
        let [_, _, pw, ph] = dialog_rect(width, height, &dialog);
        assert!(
            (gw / gh - pw / ph).abs() < 0.001,
            "it changed shape on the way"
        );

        let early = build_dialog(
            DialogView {
                dialog: &dialog,
                highlight: None,
                open: 0.2,
                behind: 0.0,
                time: 0.0,
                slots: &Named,
            },
            width,
            height,
        );
        assert!(early
            .quads
            .iter()
            .any(|quad| is_glass(quad) && quad.fade > 0.0));
        assert!(
            early.texts.iter().all(|text| text.color[3] < 0.01),
            "the question was legible at a fifth of the panel's size"
        );
    }

    /// The panel is the guide sidebar's material, the same way the menu that
    /// raised it is — asserted against `sidebar_surface` itself so the three
    /// cannot drift apart.
    #[test]
    fn the_centred_panel_is_cut_from_the_sidebars_glass() {
        let (width, height) = (1920.0, 1080.0);
        let dialog = questioned([200.0, 400.0, 380.0, 54.0]);
        let scene = dialog_scene(&dialog, width, height);
        let expected = sidebar_surface(
            dialog_rect(width, height, &dialog),
            guide_scale(height),
            0.6,
            1.0,
        );

        // The scrim is drawn first and is not part of the surface.
        let surface = &scene.quads[1..5];
        for (drawn, want) in surface.iter().zip(&expected) {
            assert_eq!(drawn.slot, want.slot);
            assert_eq!(drawn.color, want.color);
            assert_eq!(
                [drawn.x, drawn.y, drawn.w, drawn.h],
                [want.x, want.y, want.w, want.h]
            );
            assert_eq!(drawn.thickness, want.thickness);
            assert_eq!(drawn.frost, want.frost);
            assert_eq!(drawn.face_curve, want.face_curve);
        }
        assert_eq!(surface.iter().filter(|quad| is_glass(quad)).count(), 1);
    }

    /// A question wears its warning on the light, not on a button.
    ///
    /// Three things at once, because they are one decision. Both answers rest
    /// as the same glass — the panel does not answer itself before the user
    /// has. The answer that destroys something is the one *under* the harmless
    /// one, which is where every question in the shell puts it, and the
    /// harmless one is what the question opens standing on. And walking onto
    /// the grave answer lights it warm rather than in the accent, which is the
    /// same light a grave menu row takes.
    #[test]
    fn a_question_rests_cold_and_lights_its_grave_answer_warm() {
        let (width, height) = (1920.0, 1080.0);
        let dialog = questioned([200.0, 400.0, 380.0, 54.0]);
        // The *last* quad on the button's rectangle, not the first: the lit
        // capsule is drawn under the chips and sits on exactly the same
        // rectangle as the one it has arrived over.
        let chip = |scene: &Scene, index: usize| {
            let [x, y, _, h] = dialog_button_rect(width, height, &dialog, index).unwrap();
            scene
                .quads
                .iter()
                .rfind(|quad| {
                    (quad.x - x).abs() < 0.5 && (quad.y - y).abs() < 0.5 && (quad.h - h).abs() < 0.5
                })
                .map(|quad| quad.color)
                .expect("both answers are drawn")
        };

        // The order, and where the highlight starts.
        let labels: Vec<&str> = dialog
            .buttons
            .entries()
            .iter()
            .map(|entry| entry.label.as_str())
            .collect();
        assert_eq!(
            labels,
            ["No", "Yes"],
            "the destroying answer is drawn first"
        );
        assert_eq!(dialog.buttons.selected(), 0);
        assert!(dialog.buttons.entries()[1].grave);

        let resting = dialog_scene(&dialog, width, height);
        assert_eq!(
            chip(&resting, 0),
            chip(&resting, 1),
            "one of the answers is coloured before it has been chosen"
        );

        // And the light that arrives on Yes is the warm one, where the light on
        // No is the accent's. The lit capsule is the *first* quad on the
        // button's rectangle, being the one the chip is then drawn over.
        let theme = crate::theme::theme();
        let lit = |dialog: &Dialog, index: usize| {
            let [x, y, _, h] = dialog_button_rect(width, height, dialog, index).unwrap();
            dialog_scene(dialog, width, height)
                .quads
                .iter()
                .find(|quad| {
                    (quad.x - x).abs() < 0.5 && (quad.y - y).abs() < 0.5 && (quad.h - h).abs() < 0.5
                })
                .map(|quad| quad.color)
                .expect("the selection is lit")
        };
        let on_no = lit(&dialog, 0);
        let mut dialog = dialog;
        assert!(dialog.buttons.move_selection(1));
        while dialog.buttons.animate(0.05) < 1.0 {}
        let on_yes = lit(&dialog, 1);

        assert_eq!(on_no[..3], theme.accent.a(1.0)[..3]);
        assert_eq!(on_yes[..3], theme.danger.a(1.0)[..3]);
        assert_ne!(theme.accent.a(1.0)[..3], theme.danger.a(1.0)[..3]);
    }

    /// The sign-in code: dark modules on a light card, every module a whole
    /// number of pixels, and the whole thing inside the band it was given.
    ///
    /// Whole pixels are the only thing here that decides whether a phone
    /// camera can read the code at all, and it is the one property of this
    /// drawing that cannot be seen by looking at a screenshot — a grid whose
    /// modules are 7.4 pixels wide looks fine and scans badly.
    #[test]
    fn the_sign_in_code_is_drawn_in_whole_pixels_on_a_light_card() {
        let code =
            lxb_steam::qr::encode("https://s.team/q/1/1234567890123456789").expect("a short URL");
        let mut dialog = Dialog::default();
        assert!(dialog.ask(
            [200.0, 400.0, 380.0, 54.0],
            None,
            vec![
                Line::Note("Scan this in the Steam app on your phone.".to_string()),
                Line::Qr(code.clone()),
            ],
            vec![MenuEntry::new(Command::SteamCancel, "Cancel")],
            0,
        ));
        while dialog.animate(0.05) < 1.0 {}

        for [width, height] in SCREENS {
            let scene = dialog_scene(&dialog, width, height);
            // The card: the one opaque near-white quad on a panel drawn
            // entirely in glass.
            let cards: Vec<&Quad> = scene
                .quads
                .iter()
                .filter(|quad| quad.color[3] > 0.99 && quad.color[0] > 0.8)
                .collect();
            assert_eq!(cards.len(), 1, "{width}x{height}: not one card");
            let card = cards[0];
            assert!(
                card.w >= 1.0 && (card.w - card.h).abs() < 0.01,
                "not square"
            );

            // Every module is the same whole number of pixels, and there are
            // as many dark ones as the code has.
            let modules: Vec<&Quad> = scene
                .quads
                .iter()
                .filter(|quad| {
                    quad.border <= 0.0
                        && quad.radius <= 0.0
                        && quad.w > 0.0
                        && (quad.w - quad.h).abs() < 0.01
                        && quad.w < card.w
                })
                .collect();
            let dark = code.dark.iter().filter(|dark| **dark).count();
            assert_eq!(
                modules.len(),
                dark,
                "{width}x{height}: not one quad per module"
            );
            let module = modules[0].w;
            assert_eq!(module, module.floor(), "a module is {module} pixels wide");
            for quad in &modules {
                assert!(
                    (quad.w - module).abs() < 0.001,
                    "the modules are not one size"
                );
                // And every one of them is on the card rather than beside it.
                assert!(
                    quad.x >= card.x
                        && quad.y >= card.y
                        && quad.x + quad.w <= card.x + card.w + 0.01
                        && quad.y + quad.h <= card.y + card.h + 0.01,
                    "a module fell off the card"
                );
            }

            // The card is lighter than every module on it, by a long way:
            // a reader looks for dark on light and nothing else.
            for quad in &modules {
                assert!(
                    card.color[0] - quad.color[0] > 0.5,
                    "the code has no contrast to read"
                );
            }
        }
    }

    /// A field whose contents are shown stands in the same well and on the
    /// same line as one that hides them, so the panel does not change size
    /// between asking for an account name and asking for its password.
    #[test]
    fn a_shown_field_is_the_same_control_as_a_hidden_one() {
        assert_eq!(
            dialog_line_height(&Line::Entry("someone".to_string())),
            dialog_line_height(&Line::Secret { typed: 7 })
        );

        let (width, height) = (1920.0, 1080.0);
        let panel = |line: Line| {
            let mut dialog = Dialog::default();
            assert!(dialog.ask(
                [200.0, 400.0, 380.0, 54.0],
                None,
                vec![
                    Line::Note("Enter your Steam account name.".to_string()),
                    line
                ],
                vec![MenuEntry::new(Command::SteamSubmit, "Next")],
                0,
            ));
            while dialog.animate(0.05) < 1.0 {}
            dialog
        };

        let shown = panel(Line::Entry("someone".to_string()));
        let hidden = panel(Line::Secret { typed: 7 });
        assert_eq!(
            dialog_rect(width, height, &shown),
            dialog_rect(width, height, &hidden)
        );

        // Unlike the password, this one really does write what was typed:
        // a code nobody can read back is a code nobody can check.
        let scene = dialog_scene(&shown, width, height);
        let said: Vec<&str> = scene
            .texts
            .iter()
            .map(|text| text.content.as_str())
            .collect();
        assert!(said.contains(&"someone"), "{said:?}");
    }

    /// The password field, and the one rule that matters about it: what is
    /// typed is never on the panel. The line carries a count, the drawing turns
    /// that into marks, and no text run appears at all.
    #[test]
    fn the_password_field_draws_marks_and_never_characters() {
        let (width, height) = (1920.0, 1080.0);
        let field = |typed: usize| {
            let mut dialog = Dialog::default();
            assert!(dialog.ask(
                [200.0, 400.0, 380.0, 54.0],
                None,
                vec![
                    Line::Note("Enter your password to allow this.".to_string()),
                    Line::Secret { typed },
                ],
                vec![MenuEntry::new(Command::SubmitPassword, "Uninstall").grave()],
                0,
            ));
            while dialog.animate(0.05) < 1.0 {}
            dialog
        };

        let empty = dialog_scene(&field(0), width, height);
        let filled = dialog_scene(&field(7), width, height);
        for scene in [&empty, &filled] {
            let said: Vec<&str> = scene
                .texts
                .iter()
                .map(|text| text.content.as_str())
                .collect();
            assert_eq!(
                said,
                vec!["Enter your password to allow this.", "Uninstall"],
                "the field wrote something it was typed"
            );
        }
        // Seven characters are seven more round marks than none. The well and
        // its outline are the same in both, so the difference is the marks.
        let dots = |scene: &Scene| {
            scene
                .quads
                .iter()
                .filter(|quad| quad.border <= 0.0 && quad.w > 0.0 && (quad.w - quad.h).abs() < 0.01)
                .count()
        };
        assert_eq!(
            dots(&filled) - dots(&empty),
            7,
            "one mark per character, and no more"
        );

        // A password longer than the well is wide stops at the well rather
        // than running out of the panel, and stops in the same place however
        // much longer it gets — a field that grew a mark per keystroke would
        // be counting the password out loud.
        let long = dots(&dialog_scene(&field(400), width, height));
        let longer = dots(&dialog_scene(&field(4000), width, height));
        assert_eq!(long, longer, "the marks kept coming");
        assert!(long < 400, "{long} marks is not a capped field");

        let held = field(4000);
        let [panel_x, _, panel_w, _] = dialog_rect(width, height, &held);
        let marks = dialog_scene(&held, width, height);
        for mark in marks
            .quads
            .iter()
            .filter(|quad| quad.radius > 0.0 && quad.border <= 0.0 && quad.w < panel_w * 0.5)
        {
            assert!(
                mark.x >= panel_x && mark.x + mark.w <= panel_x + panel_w,
                "a mark at {} ran out of the panel",
                mark.x
            );
        }
    }

    /// It centres itself in what the keyboard has left, and comes back to the
    /// middle of the display as the board falls away.
    #[test]
    fn the_panel_lifts_clear_of_the_keyboard() {
        let (width, height) = (1920.0, 1080.0);
        let mut dialog = informed([200.0, 400.0, 380.0, 54.0]);
        let centred = dialog_rect(width, height, &dialog);

        let board = height - keyboard_panel_rect(width, height)[1];
        let mut previous = centred[1];
        // Every step of the board's rise lifts the panel a little further, so
        // the two read as one movement rather than as a panel that jumped.
        for step in 1..=8 {
            dialog.set_footer(board * step as f32 / 8.0);
            let lifted = dialog_rect(width, height, &dialog);
            assert!(
                lifted[1] < previous,
                "the panel did not move at step {step}: {lifted:?}"
            );
            assert!(lifted[1] >= 0.0);
            previous = lifted[1];
        }
        // Clear of the keys, which is the whole point.
        let lifted = dialog_rect(width, height, &dialog);
        assert!(
            lifted[1] + lifted[3] <= height - board + 0.01,
            "{lifted:?} still runs into the keyboard"
        );

        // And back again.
        dialog.set_footer(0.0);
        assert_eq!(dialog_rect(width, height, &dialog), centred);
    }

    /// The deepest thing the shell can draw: a panel over the menu that raised
    /// it, over the guide, over the bar — all still inside the snapshot budget.
    #[test]
    fn a_panel_over_a_menu_over_the_guide_fits_inside_the_snapshot_budget() {
        let xmb = Xmb::new(vec![Category {
            id: "a",
            title: "Settings",
            icon: "a",
            entries: vec![app("first"), app("second"), app("third")],
        }]);
        let (width, height) = (1920.0, 1080.0);
        let mut scene = build_with(&xmb, &Cursor::new(1), width, height, true, &AllSlots);
        scene.place_into([900.0, 200.0, 900.0, 520.0], width, height);

        let mut guide = Guide::default();
        guide.set_bars(BOTH_BARS);
        guide.open();
        guide.backdate_open(2.0);
        let cards = [card("Celeste")];
        scene
            .quads
            .extend(guide_scene(&guide, Some("Celeste"), None, &cards, None).quads);

        // The menu is still folding away underneath, which is the frame this
        // is really about: both panels are on screen at once.
        let mut menu = raised([900.0, 200.0, 900.0, 520.0], 5, height);
        menu.close();
        menu.animate(0.05);
        scene
            .quads
            .extend(context_scene(&menu, width, height).quads);

        let dialog = informed([900.0, 400.0, 380.0, 54.0]);
        scene
            .quads
            .extend(dialog_scene(&dialog, width, height).quads);

        let needed = crate::gpu::snapshots_needed(&scene.quads);
        assert!(
            needed <= crate::gpu::MAX_GLASS_BATCHES,
            "a panel over a menu over the guide needs {needed} snapshots \
             and the budget is {}",
            crate::gpu::MAX_GLASS_BATCHES,
        );
    }

    /// A scene is all its quads and then all its text, so anything drawn before
    /// the panel would print straight through it unless it is taken away — the
    /// menu the panel came out of most of all, since that one is directly
    /// underneath.
    #[test]
    fn text_behind_the_panel_is_dropped_not_merely_dimmed() {
        let (width, height) = (1920.0, 1080.0);
        let anchor = [200.0, 400.0, 380.0, 54.0];
        let dialog = informed(anchor);
        let [px, py, pw, ph] = dialog_rect(width, height, &dialog);

        let label = |content: &str, x: f32, y: f32| Text {
            content: content.into(),
            x,
            y,
            size: 20.0,
            color: [1.0; 4],
            bold: false,
            max_width: 100.0,
            align: TextAlign::Left,
            clip: None,
            halo: 0.0,
            lines: 1,
        };
        let behind = || Scene {
            quads: Vec::new(),
            texts: vec![
                label("under the panel", px + pw * 0.5, py + ph * 0.5),
                label("clear of it", 10.0, 10.0),
                // Beside the row the panel came out of, which is where the
                // menu's own labels are.
                label("beside the anchor", anchor[0] + 10.0, anchor[1] + 4.0),
            ],
        };

        let mut settled = behind();
        recede_behind_dialog(&mut settled, width, height, &dialog, 1.0);
        let left: Vec<&str> = settled
            .texts
            .iter()
            .map(|text| text.content.as_str())
            .collect();
        assert_eq!(
            left,
            // The settled panel is in the middle of the display; the row it
            // came out of is not, so a label beside that row is now clear of
            // it and is merely dimmed with everything else.
            ["clear of it", "beside the anchor"],
            "a label showed through the panel"
        );
        assert!(
            settled
                .texts
                .iter()
                .all(|text| text.color[3] > 0.0 && text.color[3] < 1.0),
            "text beside the panel should be dimmed, not taken away"
        );

        // The bug this pairing exists for. The panel travels as one whole
        // rectangle, so at the moment of the press it already *spans* the menu
        // that raised it while being invisible. Nothing may be taken away yet.
        let mut arriving = behind();
        recede_behind_dialog(&mut arriving, width, height, &dialog, 0.0);
        assert_eq!(
            arriving.texts.len(),
            3,
            "a label went before anything covered it"
        );
        assert!(
            arriving
                .texts
                .iter()
                .all(|text| (text.color[3] - 1.0).abs() < 1e-6),
            "and none of them was dimmed either"
        );
    }

    /// The bug this replaced, reported off a photograph of a television: the
    /// panel offering to install a game took every name on the bar with it,
    /// leaving a screen of unlabelled icons behind a dialog.
    ///
    /// A bar label is laid out in a box that runs to the far edge of the
    /// display and printed at the left of it, so a panel in the middle leaves
    /// more *box* on its right than there is *label* on its left — and keeping
    /// whichever end had more room kept the empty one. Both ends survive now,
    /// so a label is cut where the panel actually is.
    #[test]
    fn a_label_the_panel_crosses_keeps_the_end_its_words_are_on() {
        for [width, height] in SCREENS {
            let xmb = cross();
            let dialog = informed([width * 0.22, height * 0.5, 300.0, 120.0]);
            let panel = dialog_rect(width, height, &dialog);

            // The runs that reach clear across the panel, which are the ones
            // the old rule got wrong: it is the far end of those that has the
            // room and the near end that has the words.
            let straddling: Vec<(String, f32)> = focused(&xmb, width, height, &AllSlots)
                .texts
                .iter()
                .filter(|text| {
                    behind(text, panel)
                        && text.x < panel[0]
                        && text.x + text.max_width > panel[0] + panel[2]
                })
                .map(|text| (text.content.clone(), text.x))
                .collect();
            assert!(
                !straddling.is_empty(),
                "{width}x{height}: no label of this bar reaches across the panel"
            );

            let mut scene = focused(&xmb, width, height, &AllSlots);
            recede_behind_dialog(&mut scene, width, height, &dialog, 1.0);
            for (name, from) in straddling {
                assert!(
                    scene.texts.iter().any(|text| {
                        text.content == name
                            && text
                                .clip
                                .is_some_and(|[x, _, w, _]| x <= from + 0.5 && x + w > from)
                    }),
                    "{width}x{height}: {name:?} lost the end its words are printed on"
                );
            }
            // And nothing of the bar is still allowed to print through the
            // panel, which is what the cutting was for in the first place.
            for text in &scene.texts {
                let Some([x, _, w, _]) = text.clip else {
                    assert!(!behind(text, panel), "{:?} prints through", text.content);
                    continue;
                };
                assert!(
                    x + w <= panel[0] + 0.5 || x >= panel[0] + panel[2] - 0.5,
                    "{:?} still draws under the panel",
                    text.content
                );
            }
        }
    }
}
