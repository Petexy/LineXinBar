//! Where a picture-in-picture window goes, and what shape it is drawn in.
//!
//! A browser's picture-in-picture window is the one window on this session that
//! is neither maximized nor hidden: it floats in a corner, over everything,
//! while the user does something else. The compositor places it and draws the
//! rounded mat around it; the shell offers the three choices that decide where
//! and how large. Neither of them can work the answer out alone — the shell
//! knows the setting and not the display, the compositor knows the display and
//! not the setting — so the arithmetic between the two lives here, in the crate
//! they both already depend on, exactly as the overview's card slots do.
//!
//! Everything is in logical coordinates, like [`crate::overview`].

use crate::overview::Rect;

/// The corner radius the shell draws its menus and panels with, against a
/// 1080p reference.
///
/// It is the shell's number, kept here because it is now two processes'
/// number: it is the air the compositor holds a floating window off the edges
/// of the screen by, and off the window above it in the column — see
/// [`margin`]. `lxb-desktop`'s `PANEL_RADIUS` reads it from here so there is one
/// place for it to be wrong.
///
/// It is *not* what that window's own corners are rounded at, though it was
/// until the mat became a hairline. See [`FRAME_RADIUS`], which is what a mat
/// [`BORDER`] thick is able to hide a square corner behind.
pub const MENU_RADIUS: f64 = 30.0;

/// How thick the mat around the window is, in logical pixels.
///
/// Not a decoration — it is what makes the corner round at all. The window
/// inside is a rectangle with square corners, as every client's buffer is, and
/// nothing in a compositor's renderer can cut a curve out of one. So the mat is
/// laid over its edges: rounded on the outside at [`FRAME_RADIUS`], rounded on
/// the inside at what is left of it, and opaque in between, which is what covers
/// the four square corners the client drew.
///
/// **Three pixels, fixed, and not a share of anything.** It used to be a third
/// of the radius, which is a tenth of a small window's height on a 1080p panel
/// and twice that on a 4K one — a picture frame where a hairline was wanted. A
/// thin line reads as the edge of the window; a thick one reads as furniture
/// around it, and the thing in the corner is a video, not an exhibit. Fixed
/// rather than scaled with the display for the same reason a hairline is a
/// hairline on every screen.
pub const BORDER: f64 = 3.0;

/// The radius the floating window's own corners are rounded at, in logical
/// pixels.
///
/// **Not the shell's menu radius, and it cannot be.** The two numbers are tied:
/// the client's square corner sits √2·(r − b) from the centre of the outer arc,
/// so it is hidden by the mat only where √2·(r − b) ≤ r — and a mat as thin as
/// [`BORDER`] can only hide a corner this round. Eight leaves the corner very
/// nearly a whole pixel inside the curve, which is the antialiased edge's own
/// width and as much slack as there is to have. Round it further at this
/// thickness and the four corners of the client's buffer come out through the
/// curve, which is a rounded window with points on it.
///
/// The air around the window is still measured in [`MENU_RADIUS`] — see
/// [`margin`]. That is the number the shell's own shapes are spaced by, and it
/// is what the corner it sits in is worth; the rounding of this one frame is a
/// separate question and now has a separate answer.
pub const FRAME_RADIUS: f64 = 8.0;

/// How far the mat's shadow reaches past it, against the same 1080p reference.
///
/// A window with nothing behind it needs no shadow; this one has an
/// application behind it always, and without one it reads as a hole cut in the
/// screen rather than as something lying on top of it.
pub const SHADOW: f64 = 18.0;

/// The aspect a floating window is given before its client has said what shape
/// it wants to be.
///
/// Sixteen to nine, because almost everything that is ever put into
/// picture-in-picture is video of that shape. It is only ever a *starting*
/// answer: a window whose video is four to three, or a phone's video stood on
/// end, is that shape and letterboxing it into a widescreen box would be the
/// compositor deciding what the picture looks like. What the client says is
/// asked for once and then followed — see the compositor's `pip` module, which
/// is where that conversation lives.
pub const DEFAULT_ASPECT: f64 = 16.0 / 9.0;

/// The narrowest and widest shape a floating window is given, whatever its
/// client asks for.
///
/// A window is a rectangle in a corner of somebody's screen, and past these it
/// stops being one: a slit and a strip are both shapes a client can ask for by
/// accident — a buffer it has not laid out yet, a size of zero in one direction
/// — and neither is a window anybody can see anything in.
pub const NARROWEST_ASPECT: f64 = 1.0 / 3.0;
pub const WIDEST_ASPECT: f64 = 3.0;

/// How large the floating window is, as the shell offers it.
///
/// A share of the display's width rather than a number of pixels: the same
/// choice has to mean the same thing on a laptop panel and on a television
/// across the room, and only one of those is measured in pixels the user can
/// see from where they are sitting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Size {
    Small,
    #[default]
    Medium,
    Large,
}

impl Size {
    /// The wire value, which is `lxb_shell_v1.pip_size`.
    pub fn wire(self) -> u32 {
        match self {
            Self::Small => 0,
            Self::Medium => 1,
            Self::Large => 2,
        }
    }

    /// The wire value read back, or `None` for a number this enum has no
    /// meaning for — which a compositor answers by leaving the size alone
    /// rather than by inventing one.
    pub fn from_wire(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::Small),
            1 => Some(Self::Medium),
            2 => Some(Self::Large),
            _ => None,
        }
    }

    /// The name this size is written down under, in the shell's settings file.
    pub fn key(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large",
        }
    }

    /// A written name read back, folded the way every other key in that file is.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|size| size.key().eq_ignore_ascii_case(key.trim()))
    }

    /// What the row is called on the page.
    pub fn title(self) -> &'static str {
        match self {
            Self::Small => "Small",
            Self::Medium => "Medium",
            Self::Large => "Large",
        }
    }

    /// How much of the display's width it takes.
    pub fn share(self) -> f64 {
        match self {
            Self::Small => 1.0 / 6.0,
            Self::Medium => 1.0 / 4.0,
            Self::Large => 1.0 / 3.0,
        }
    }

    pub const ALL: [Size; 3] = [Size::Small, Size::Medium, Size::Large];
}

/// Which corner it sits in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Place {
    TopLeft,
    /// The corner it starts in, and the one the feature is named after
    /// everywhere it exists: a video parked out of the way is parked where the
    /// eye is not.
    #[default]
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Place {
    /// The wire value, which is `lxb_shell_v1.pip_place`.
    pub fn wire(self) -> u32 {
        match self {
            Self::TopLeft => 0,
            Self::TopRight => 1,
            Self::BottomLeft => 2,
            Self::BottomRight => 3,
        }
    }

    pub fn from_wire(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::TopLeft),
            1 => Some(Self::TopRight),
            2 => Some(Self::BottomLeft),
            3 => Some(Self::BottomRight),
            _ => None,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::TopLeft => "top-left",
            Self::TopRight => "top-right",
            Self::BottomLeft => "bottom-left",
            Self::BottomRight => "bottom-right",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|place| place.key().eq_ignore_ascii_case(key.trim()))
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::TopLeft => "Top left",
            Self::TopRight => "Top right",
            Self::BottomLeft => "Bottom left",
            Self::BottomRight => "Bottom right",
        }
    }

    fn at_the_right(self) -> bool {
        matches!(self, Self::TopRight | Self::BottomRight)
    }

    fn at_the_bottom(self) -> bool {
        matches!(self, Self::BottomLeft | Self::BottomRight)
    }

    pub const ALL: [Place; 4] = [
        Place::TopLeft,
        Place::TopRight,
        Place::BottomLeft,
        Place::BottomRight,
    ];
}

/// How much larger than the 1080p reference a display of this height is.
///
/// The shell's own `guide_scale`, clamped at both ends for the same reasons: a
/// nested window a few hundred pixels tall must not be given a radius that
/// swallows the picture, and a 4K panel must not be given one four times over.
pub fn scale(height: f64) -> f64 {
    (height / 1080.0).clamp(0.6, 2.5)
}

/// How far a floating window is held off the display's edges, and how far the
/// next one down the column stands off it.
///
/// The shell's menu radius on this display, which is the number every shape in
/// the session is spaced by. Deliberately *not* the window's own corner radius —
/// those were one number while the mat was a share of it, and a hairline frame
/// would have parked the window three pixels off the edge of the screen.
pub fn margin(height: f64) -> f64 {
    MENU_RADIUS * scale(height)
}

/// Everything the compositor needs to place and draw one floating window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    /// The whole shape, mat included. What the shadow is cast from and what
    /// the corner margin is measured to.
    pub outer: Rect,
    /// The rectangle the client is configured at and drawn into, which is the
    /// mat's opening: `outer` inset by [`Frame::border`] on every side.
    pub inner: Rect,
    /// The outer corner radius — [`FRAME_RADIUS`], which is as round as a mat
    /// this thin can hide the client's own square corner behind.
    pub radius: f64,
    /// How thick the mat is.
    pub border: f64,
    /// How far its shadow reaches past `outer`.
    pub shadow: f64,
}

impl Frame {
    /// The radius the mat's opening is cut with, which is what the client's
    /// square corners are hidden behind.
    pub fn inner_radius(&self) -> f64 {
        (self.radius - self.border).max(0.0)
    }
}

/// Where a floating window goes on a display `width` × `height` logical pixels.
///
/// The size is a share of the width, at whatever shape the window's own client
/// asked to be — see [`DEFAULT_ASPECT`] for what stands in until it has said —
/// held off both edges by exactly the radius it is rounded with. One number for
/// the corner and for the air around it, because they are the same statement
/// about how far this shape stands off the screen's edge.
///
/// Floors and ceilings on the size, for the two displays that break the share.
/// A window smaller than [`SMALLEST`] is one nobody can see what is happening
/// in; one taller than half the display is not floating over the session any
/// more, which is what a very tall or very narrow screen — or a video stood on
/// its end — would otherwise ask for.
///
/// `offset` is how far in from that corner the window starts, which is what
/// stands a second floating window below the first instead of on top of it —
/// see [`frames`], which is what works it out.
pub fn frame(width: f64, height: f64, size: Size, place: Place, aspect: f64, offset: f64) -> Frame {
    let margin = margin(height);
    // A shape a client cannot have meant: a buffer it has not laid out, a size
    // of nothing in one direction, or arithmetic that produced neither.
    let aspect = match aspect.is_finite() {
        true => aspect.clamp(NARROWEST_ASPECT, WIDEST_ASPECT),
        false => DEFAULT_ASPECT,
    };

    // The room a window may take, after the margins are paid on both sides.
    let across = (width - margin * 2.0).max(0.0);
    let down = (height - margin * 2.0).max(0.0);
    let border = BORDER;
    // The mat is drawn *round* the window, so it is not part of the shape the
    // client asked for. The aspect belongs to the opening, and the two sides of
    // the mat are added back afterwards — take it off the whole shape instead
    // and the opening comes out a different shape from the picture in it, which
    // is a gap down one edge of the window for any client that keeps its own.
    let mat = border * 2.0;

    let mut w = (width * size.share()).clamp(SMALLEST.min(across), across);
    let mut h = (w - mat).max(1.0) / aspect + mat;
    // Half the display's height is the ceiling: past it the window stops being
    // something in a corner. Taken on the height because that is the dimension
    // a share runs out of first — on a tall screen, and on any screen once the
    // client asks to be taller than it is wide.
    let tallest = down.min(height * 0.5);
    if h > tallest {
        h = tallest;
        w = (h - mat).max(1.0) * aspect + mat;
    }
    // And the width again, for the shape that has just been made wider: a very
    // wide video on a narrow display can leave the screen this way round.
    if w > across {
        w = across;
        h = (w - mat).max(1.0) / aspect + mat;
    }
    // Whatever the two above settled on, there has to be something left inside
    // the mat to be a window.
    let w = w.max(mat + 1.0).min(across.max(mat + 1.0));
    let h = h.max(mat + 1.0).min(down.max(mat + 1.0));

    let x = match place.at_the_right() {
        true => width - margin - w,
        false => margin,
    };
    // Always in from the corner, and clamped at the far margin: a session with
    // more floating windows than the screen has room for piles them up rather
    // than losing them off the edge.
    let y = match place.at_the_bottom() {
        true => (height - margin - h - offset).max(margin.min(height - h)),
        false => (margin + offset).min((height - margin - h).max(margin)),
    };

    framed(Rect { x, y, w, h }, height)
}

/// The frame around one outer rectangle, wherever that rectangle came from.
///
/// The mat, the radius and the shadow are the same for a window the layout put
/// in a corner and for one the user dragged into the middle of the screen:
/// what differs between them is only which rectangle it is. So there is one
/// place a [`Frame`] is built, and both doors — [`frame`] and [`frame_at`] —
/// come through it.
fn framed(outer: Rect, height: f64) -> Frame {
    let border = BORDER;
    Frame {
        outer,
        inner: Rect {
            x: outer.x + border,
            y: outer.y + border,
            w: (outer.w - border * 2.0).max(1.0),
            h: (outer.h - border * 2.0).max(1.0),
        },
        radius: FRAME_RADIUS,
        border,
        shadow: SHADOW * scale(height),
    }
}

/// Where every floating window on one display goes, in the order they started
/// floating — one shape per aspect handed in.
///
/// A browser that opens a second picture-in-picture while the first is still up
/// gets a second window, and two of them in the same corner is one window with
/// something wrong with it: the corner is what says *this is a video parked out
/// of the way*, and two things saying it in the same place say neither. So they
/// stand in a column from that corner, each below the one before.
///
/// Laid out together rather than one at a time, because they are not the same
/// height: each is the shape its own client asked to be, so how far down the
/// second one starts is a fact about the *first* one. Stepping by a window's own
/// height — which is what this did before there was anything to hand it but a
/// count — puts a tall window through the bottom of a squat one.
pub fn frames(width: f64, height: f64, size: Size, place: Place, aspects: &[f64]) -> Vec<Frame> {
    let margin = margin(height);
    let mut offset = 0.0;
    aspects
        .iter()
        .map(|aspect| {
            let frame = frame(width, height, size, place, *aspect, offset);
            offset += frame.outer.h + margin;
            frame
        })
        .collect()
}

/// The frame for a window the user has put somewhere by hand, on a display
/// `width` × `height` logical pixels.
///
/// The same shape the layout draws — same mat, same radius, same shadow — in a
/// rectangle nothing but the user's hand decided. The rectangle is brought back
/// to something the display can hold first: see [`hand_placed`], which is where
/// every rule that still applies to a window somebody dragged is written down.
pub fn frame_at(outer: Rect, width: f64, height: f64, aspect: f64) -> Frame {
    framed(hand_placed(outer, width, height, aspect), height)
}

/// A rectangle the user has dragged or resized, brought back to one this
/// display can hold.
///
/// Three rules survive a hand on the window, and only three:
///
/// - **It is still the shape its client asked to be.** Dragging a corner
///   scales the window; it does not restretch the video. That is the same
///   promise [`frame`] makes, asked of the opening rather than of the whole
///   shape for the same reason — the mat is drawn *round* the picture, so the
///   two sides of it are added back after the shape is settled.
/// - **It is still a window.** No smaller than [`SMALLEST`] across, which is
///   the floor the layout draws at, and no larger than the screen it is on.
/// - **It is still on the screen.** Wholly, not partly: a video dragged half
///   off the edge is a video with half of it missing, and there is nothing
///   over there to have dragged it towards.
///
/// What does *not* survive is the margin. [`margin`] is the air the layout
/// holds a window off the edges by, and a window the user pushed into the
/// corner themselves has been told where it goes.
pub fn hand_placed(outer: Rect, width: f64, height: f64, aspect: f64) -> Rect {
    let aspect = match aspect.is_finite() {
        true => aspect.clamp(NARROWEST_ASPECT, WIDEST_ASPECT),
        false => DEFAULT_ASPECT,
    };
    let mat = BORDER * 2.0;
    // The shape is locked, so the size is one number and not two: the width is
    // what the hand said, and the height is what the client's shape makes of
    // it. Then the same pair of ceilings [`frame`] takes, in the same order and
    // for the same reason — a window taller than the display is one a very tall
    // video asks for, and taking the height back makes it wider again.
    let smallest = SMALLEST.min(width.max(1.0));
    let mut w = outer.w.clamp(smallest, width.max(smallest));
    let mut h = (w - mat).max(1.0) / aspect + mat;
    if h > height {
        h = height;
        w = (h - mat).max(1.0) * aspect + mat;
    }
    if w > width {
        w = width;
        h = (w - mat).max(1.0) / aspect + mat;
    }
    let w = w.max(mat + 1.0).min(width.max(mat + 1.0));
    let h = h.max(mat + 1.0).min(height.max(mat + 1.0));
    Rect {
        x: outer.x.clamp(0.0, (width - w).max(0.0)),
        y: outer.y.clamp(0.0, (height - h).max(0.0)),
        w,
        h,
    }
}

/// The smallest a floating window is drawn, in logical pixels across.
///
/// A sixth of a small nested window is a few dozen pixels — a stamp, not a
/// picture — and the point of the feature is that the user can still follow
/// what is in it.
pub const SMALLEST: f64 = 240.0;

/// How far inside the mat a pixel at `(x, y)` is, in logical pixels, measured
/// from the mat's outer silhouette.
///
/// Positive inside the shape, negative outside it, and zero on the curve, which
/// is what lets a painter turn it into coverage: the mat's edge is one pixel of
/// distance wide and antialiases itself. `(x, y)` is relative to the outer
/// rectangle's own corner.
///
/// Shared rather than left to the compositor because it is also what says
/// whether the rounding *works*: the test below asks it about the client's own
/// corner, which is the whole argument for [`BORDER`].
pub fn inside_rounded(x: f64, y: f64, w: f64, h: f64, radius: f64) -> f64 {
    let radius = radius.min(w / 2.0).min(h / 2.0).max(0.0);
    // Distance to the rounded rectangle, the standard way: fold the point into
    // one quadrant, measure to the corner arc's centre, and take the straight
    // edges where the point is beside the arc rather than diagonal from it.
    let dx = (x - w / 2.0).abs() - (w / 2.0 - radius);
    let dy = (y - h / 2.0).abs() - (h / 2.0 - radius);
    let outside = dx.max(0.0).hypot(dy.max(0.0));
    let inside = dx.max(dy).min(0.0);
    radius - (outside + inside)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_wire_value_survives_the_round_trip() {
        for size in Size::ALL {
            assert_eq!(Size::from_wire(size.wire()), Some(size));
            assert_eq!(Size::from_key(size.key()), Some(size));
        }
        for place in Place::ALL {
            assert_eq!(Place::from_wire(place.wire()), Some(place));
            assert_eq!(Place::from_key(place.key()), Some(place));
        }
        assert_eq!(Size::from_wire(3), None);
        assert_eq!(Place::from_wire(4), None);
        assert_eq!(Size::from_key("enormous"), None);
    }

    #[test]
    fn a_written_name_is_read_however_it_was_capitalised() {
        assert_eq!(Place::from_key(" Top-Right "), Some(Place::TopRight));
        assert_eq!(Size::from_key("LARGE"), Some(Size::Large));
    }

    #[test]
    fn each_corner_is_the_same_distance_from_its_two_edges() {
        let (w, h) = (1920.0, 1080.0);
        for place in Place::ALL {
            let frame = frame(w, h, Size::Medium, place, DEFAULT_ASPECT, 0.0);
            let left = frame.outer.x;
            let right = w - frame.outer.right();
            let top = frame.outer.y;
            let bottom = h - frame.outer.bottom();
            assert!(
                (left.min(right) - margin(h)).abs() < 0.001,
                "{place:?} is {left}/{right} from the sides"
            );
            assert!(
                (top.min(bottom) - margin(h)).abs() < 0.001,
                "{place:?} is {top}/{bottom} from the top and bottom"
            );
        }
    }

    #[test]
    fn the_corner_it_is_put_in_is_the_corner_it_lands_in() {
        let (w, h) = (1920.0, 1080.0);
        let top_right = frame(w, h, Size::Medium, Place::TopRight, DEFAULT_ASPECT, 0.0);
        let bottom_left = frame(w, h, Size::Medium, Place::BottomLeft, DEFAULT_ASPECT, 0.0);
        assert!(top_right.outer.x > w / 2.0 && top_right.outer.y < h / 2.0);
        assert!(bottom_left.outer.x < w / 2.0 && bottom_left.outer.y > h / 2.0);
    }

    #[test]
    fn a_larger_choice_is_a_larger_window() {
        let (w, h) = (1920.0, 1080.0);
        let widths: Vec<f64> = Size::ALL
            .iter()
            .map(|size| {
                frame(w, h, *size, Place::TopRight, DEFAULT_ASPECT, 0.0)
                    .outer
                    .w
            })
            .collect();
        assert!(widths[0] < widths[1] && widths[1] < widths[2], "{widths:?}");
    }

    /// The shape is the client's, not this module's: a window that says it is
    /// four to three is drawn four to three, and one stood on its end is drawn
    /// standing on its end rather than letterboxed into a widescreen box.
    ///
    /// Asked of the **opening**, which is the rectangle the client is given and
    /// the only one whose shape it can see. Taking the mat off a shape that was
    /// already the right one leaves an opening that is not, and the window ends
    /// up with a strip of somebody's start screen down one side of it — which is
    /// what this looked like on screen before it was written down.
    #[test]
    fn a_window_is_the_shape_its_client_asked_to_be() {
        let (w, h) = (1920.0, 1080.0);
        for aspect in [16.0 / 9.0, 4.0 / 3.0, 1.0, 9.0 / 16.0, 2.76] {
            for size in Size::ALL {
                let frame = frame(w, h, size, Place::TopRight, aspect, 0.0);
                assert!(
                    (frame.inner.w / frame.inner.h - aspect).abs() < 0.001,
                    "{aspect} at {size:?} came out as {:?}",
                    frame.inner
                );
                // And the mat is still the same thickness all the way round it.
                assert!((frame.outer.w - frame.inner.w - frame.border * 2.0).abs() < 0.001);
                assert!((frame.outer.h - frame.inner.h - frame.border * 2.0).abs() < 0.001);
            }
        }
    }

    /// A shape no client can have meant is answered with one that means
    /// something, rather than with a slit down the side of the screen.
    #[test]
    fn a_shape_that_is_not_a_shape_is_not_taken_literally() {
        let (w, h) = (1920.0, 1080.0);
        for aspect in [0.0, -4.0, f64::NAN, f64::INFINITY, 200.0, 0.0001] {
            let frame = frame(w, h, Size::Medium, Place::TopRight, aspect, 0.0);
            let ratio = frame.outer.w / frame.outer.h;
            assert!(
                (NARROWEST_ASPECT - 0.001..=WIDEST_ASPECT + 0.001).contains(&ratio),
                "{aspect} came out at {ratio}"
            );
            assert!(frame.outer.w > 0.0 && frame.outer.h > 0.0);
        }
    }

    /// A column, not a pile: floating windows of any shapes stand clear of one
    /// another, in the order they started floating, from the corner inwards —
    /// for as long as the display has room for them, which is what the test
    /// below this one is about.
    ///
    /// The shapes are the point. Stepping by each window's own height puts a
    /// tall one through the bottom of a squat one, which is what this looked
    /// like on screen before the column was laid out as a whole.
    #[test]
    fn floating_windows_stand_in_a_column_rather_than_a_pile() {
        let (w, h) = (1920.0, 1080.0);
        let shapes = [2.76, 16.0 / 9.0, 1.0];
        for place in Place::ALL {
            let frames = frames(w, h, Size::Small, place, &shapes);
            assert_eq!(frames.len(), shapes.len());
            for (index, frame) in frames.iter().enumerate() {
                assert_eq!(frame.outer.x, frames[0].outer.x, "{place:?} moved sideways");
                assert!(
                    (frame.inner.w / frame.inner.h - shapes[index]).abs() < 0.001,
                    "{place:?} lost the shape of window {index}"
                );
                for other in frames.iter().skip(index + 1) {
                    assert!(
                        frame.outer.bottom() <= other.outer.y
                            || other.outer.bottom() <= frame.outer.y,
                        "{place:?} put two windows through each other: {:?} {:?}",
                        frame.outer,
                        other.outer
                    );
                }
            }
            // From the corner inwards, whichever corner that is.
            let first = frames[0].outer;
            let last = frames[shapes.len() - 1].outer;
            match place {
                Place::BottomLeft | Place::BottomRight => assert!(last.y < first.y),
                _ => assert!(last.y > first.y),
            }
        }
    }

    /// More windows than the display has room for pile up at the far edge
    /// rather than walking off it.
    #[test]
    fn a_column_longer_than_the_screen_stops_at_the_far_edge() {
        let (w, h) = (1920.0, 1080.0);
        let shapes = [16.0 / 9.0; 8];
        for place in Place::ALL {
            for frame in frames(w, h, Size::Large, place, &shapes) {
                assert!(
                    frame.outer.y >= 0.0 && frame.outer.bottom() <= h + 0.001,
                    "{place:?} put a window at {:?}",
                    frame.outer
                );
            }
        }
    }

    #[test]
    fn nothing_ever_hangs_off_the_display() {
        // Including the shapes that break the share: a nested window barely
        // larger than the smallest size, and a screen far taller than it is
        // wide.
        for (w, h) in [
            (1920.0, 1080.0),
            (3840.0, 2160.0),
            (1280.0, 720.0),
            (600.0, 400.0),
            (1080.0, 1920.0),
        ] {
            for size in Size::ALL {
                for place in Place::ALL {
                    for (aspect, offset) in [
                        (DEFAULT_ASPECT, 0.0),
                        (4.0 / 3.0, 260.0),
                        (9.0 / 16.0, 900.0),
                        (WIDEST_ASPECT, 4000.0),
                    ] {
                        let frame = frame(w, h, size, place, aspect, offset);
                        assert!(
                            frame.outer.x >= 0.0
                                && frame.outer.y >= 0.0
                                && frame.outer.right() <= w + 0.001
                                && frame.outer.bottom() <= h + 0.001,
                            "{size:?} {place:?} on {w}×{h} is {:?}",
                            frame.outer
                        );
                        assert!(frame.inner.w > 0.0 && frame.inner.h > 0.0);
                    }
                }
            }
        }
    }

    #[test]
    fn the_mat_covers_the_square_corner_the_client_drew() {
        // The argument that ties BORDER to FRAME_RADIUS, asked of the geometry
        // rather than of the prose: the client's own corner has to fall inside
        // the outer curve, or the window is a rounded shape with four points
        // sticking out of it. Since the mat is now a fixed hairline, this is
        // what says how round the corner is allowed to be.
        let frame = frame(
            1920.0,
            1080.0,
            Size::Medium,
            Place::TopRight,
            DEFAULT_ASPECT,
            0.0,
        );
        let corner = frame.border;
        let inside = inside_rounded(corner, corner, frame.outer.w, frame.outer.h, frame.radius);
        // And with a whole pixel to spare, which is what the antialiased edge of
        // the curve is drawn over: a corner that only just clears it is a corner
        // half-blended into the shape it is meant to be hidden behind.
        assert!(
            inside >= 0.9,
            "the client's corner is only {inside} inside the curve"
        );
        // And the opening it is cut from does *not* contain it, which is what
        // makes the mat cover it rather than merely reach it.
        assert!(
            inside_rounded(0.0, 0.0, frame.inner.w, frame.inner.h, frame.inner_radius()) < 0.0,
            "the mat's opening leaves the client's corner uncovered"
        );
    }

    /// A hand on the window moves it and scales it. It does not restretch the
    /// video: the opening is the client's shape before the drag and after it,
    /// which is the same promise the corner layout makes.
    #[test]
    fn a_window_the_user_placed_is_still_the_shape_its_client_asked_to_be() {
        let (w, h) = (1920.0, 1080.0);
        for aspect in [16.0 / 9.0, 4.0 / 3.0, 1.0, 9.0 / 16.0, 2.76] {
            for dragged in [
                Rect {
                    x: 40.0,
                    y: 40.0,
                    w: 300.0,
                    h: 700.0,
                },
                Rect {
                    x: 900.0,
                    y: 500.0,
                    w: 640.0,
                    h: 100.0,
                },
                Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 481.0,
                    h: 271.0,
                },
            ] {
                let frame = frame_at(dragged, w, h, aspect);
                assert!(
                    (frame.inner.w / frame.inner.h - aspect).abs() < 0.001,
                    "{aspect} came out as {:?}",
                    frame.inner
                );
                assert!((frame.outer.w - frame.inner.w - frame.border * 2.0).abs() < 0.001);
            }
        }
    }

    /// Wholly on the screen, however far the hand went — and still a window
    /// when it gets there.
    #[test]
    fn a_window_the_user_placed_stays_on_the_screen() {
        for (w, h) in [(1920.0, 1080.0), (3840.0, 2160.0), (600.0, 400.0)] {
            for dragged in [
                Rect {
                    x: -900.0,
                    y: -900.0,
                    w: 480.0,
                    h: 270.0,
                },
                Rect {
                    x: w + 100.0,
                    y: h + 100.0,
                    w: 480.0,
                    h: 270.0,
                },
                Rect {
                    x: 10.0,
                    y: 10.0,
                    w: 1.0,
                    h: 1.0,
                },
                Rect {
                    x: 10.0,
                    y: 10.0,
                    w: w * 4.0,
                    h: h * 4.0,
                },
            ] {
                let placed = hand_placed(dragged, w, h, DEFAULT_ASPECT);
                assert!(
                    placed.x >= 0.0
                        && placed.y >= 0.0
                        && placed.right() <= w + 0.001
                        && placed.bottom() <= h + 0.001,
                    "{dragged:?} on {w}×{h} came out at {placed:?}"
                );
                assert!(placed.w > BORDER * 2.0 && placed.h > BORDER * 2.0);
                assert!(placed.w >= SMALLEST.min(w) - 0.001, "{placed:?} is a stamp");
            }
        }
    }

    /// The margin is the layout's, not the window's. A window dragged into the
    /// corner sits in the corner — the air [`margin`] holds the *placed* ones
    /// off the edges by is not a wall the hand has to stop at.
    #[test]
    fn a_window_the_user_placed_is_not_held_off_the_edges() {
        let (w, h) = (1920.0, 1080.0);
        let placed = hand_placed(
            Rect {
                x: -5.0,
                y: -5.0,
                w: 480.0,
                h: 273.0,
            },
            w,
            h,
            DEFAULT_ASPECT,
        );
        assert_eq!((placed.x, placed.y), (0.0, 0.0));
        assert!(margin(h) > 0.0, "the layout still has its own margin");
    }

    #[test]
    fn the_distance_field_is_zero_on_the_curve() {
        let (w, h, r) = (400.0, 225.0, 30.0);
        // Straight down the middle of each edge, where the shape is a plain
        // rectangle.
        assert!((inside_rounded(0.0, h / 2.0, w, h, r)).abs() < 0.001);
        assert!((inside_rounded(w, h / 2.0, w, h, r)).abs() < 0.001);
        // And on the corner arc itself.
        let on_the_arc = r - r / 2.0_f64.sqrt();
        assert!((inside_rounded(on_the_arc, on_the_arc, w, h, r)).abs() < 0.001);
        // The very corner of the bounding box is outside the shape.
        assert!(inside_rounded(0.0, 0.0, w, h, r) < 0.0);
    }
}
