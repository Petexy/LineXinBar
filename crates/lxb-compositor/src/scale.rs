//! How much larger than life an application draws itself.
//!
//! One number per display — see `lxb_shell_v1.set_output_application_scale`,
//! and `set_application_scale` beside it, which is what a screen nobody has
//! named draws at — and it is not a magnification. A screen looked at from a
//! sofa wants an application's *interface* larger, not its pixels larger, and
//! those are two different things: the first is what a high-density laptop
//! panel does to every toolkit on it, and the second is what a projector out of
//! focus does.
//!
//! Per display because a screen is looked at from where it stands. The number
//! answers how far away the user is sitting, and a person at a desk with a
//! television behind them is sitting two distances at once — so a window
//! carries the answer of the display it is on, and a window moved between two
//! screens is configured again for the one it lands on.
//!
//! So this is carried out the way a dense panel carries it out, in three parts
//! that have to agree:
//!
//! 1. The window is configured with a logical size this factor *smaller* than
//!    the display — see [`crate::outputs::OutputManager::tile_window_on_output`]
//!    — so its interface has fewer logical pixels to lay itself out in and each
//!    of them is worth more of the screen.
//! 2. Its surfaces are told, over `wp_fractional_scale_v1`, that their scale is
//!    that much higher — see [`preferred_scale`] — so a client that honours it
//!    draws a buffer with exactly as many pixels as the screen has.
//! 3. That buffer is drawn back out over the whole display, by scaling the
//!    window's render elements about its own corner — see [`crate::render`].
//!
//! With all three, a factor of 1.5 on a 1920×1080 screen configures the window
//! at 1280×720, is handed a 1920×1080 buffer, and puts it on the display one
//! pixel for one pixel. Nothing is resampled and nothing is soft; the
//! application's own text is simply drawn at 1.5× the size it chose. A client
//! that ignores the scale sends a 1280×720 buffer instead and is enlarged into
//! the same place, which is soft — and is the same answer every other
//! compositor gives such a client, rather than a window three quarters of the
//! way across the screen.
//!
//! Two things are deliberately left out of it.
//!
//! The shell is not an application. It draws itself in layer surfaces, which
//! are never [`Window`]s and so are never asked about here: LineXinBar is sized
//! against the display it was given whatever this is set to. That is the whole
//! point of doing it per window instead of by moving the output's own scale,
//! which would have taken the Settings page the user is looking at with it.
//!
//! Xwayland windows are left out too, and that is a limit rather than a
//! preference. X11 has no per-surface scale to tell a client about, so part 2
//! is unavailable there and all that could be done is part 3 — magnifying
//! pixels that have already been drawn. A blurred window is not what somebody
//! asking for a larger one asked for, so an X11 window keeps its own size.

//! And one application at a time can be given a *resolution* instead — see
//! [`Resolution`], and `lxb_shell_v1.set_application_resolution`. It is the
//! same three parts with the second one dropped: the window is configured at
//! the number of pixels that were asked for, told nothing about scale, and its
//! picture is drawn back out over the display. So the two cannot both be in
//! force on one window, and the resolution wins where a shell has asked for
//! both — a number of pixels is an answer to "how hard is this to draw", and a
//! percentage is an answer to "how far away am I sitting", and only the first
//! of those can be said about one application.

use smithay::desktop::Window;
use smithay::utils::{Logical, Point, Rectangle};

/// Natural size: what an application draws at when nothing has been asked of
/// it, and the floor of the range.
///
/// The floor because below it an application would be given *more* logical
/// pixels than the display has, drawing its interface smaller than it chose.
/// That is a legitimate thing to want on a desk two feet from a 4K panel, and
/// it is not what this setting is for — a shell driven from a sofa with a pad
/// has one direction to go — so the range starts here.
pub const NATURAL_PERCENT: u32 = 100;

/// As far as this goes.
///
/// Three times is already an interface with a third of the room it was designed
/// for, which is the point where an application's own dialogs start arriving
/// larger than the screen that has to hold them. Past it the setting stops
/// making windows readable and starts making them unusable.
pub const LARGEST_PERCENT: u32 = 300;

/// The factor every application's own drawing is enlarged by.
///
/// Held as per cent rather than as a float, so that two of these compare
/// exactly: what the shell asks for is a whole number of per cent, and a
/// setting the compositor cannot tell it has already applied is one that
/// re-tiles every window on the session every time it is re-sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppScale(u32);

impl Default for AppScale {
    fn default() -> Self {
        Self(NATURAL_PERCENT)
    }
}

impl AppScale {
    /// The scale a shell asked for, brought into the range this compositor
    /// will drive.
    ///
    /// Clamped rather than refused, for the reason a colour temperature out of
    /// range is clamped: a shell asking for something outside it has a bug, and
    /// applications left at some unrelated size are a worse way to report that
    /// than the nearest size that means something.
    pub fn from_percent(percent: u32) -> Self {
        Self(percent.clamp(NATURAL_PERCENT, LARGEST_PERCENT))
    }

    pub fn percent(self) -> u32 {
        self.0
    }

    /// The factor itself, which is what every calculation actually wants.
    pub fn factor(self) -> f64 {
        self.0 as f64 / NATURAL_PERCENT as f64
    }
}

/// How many pixels one application draws its picture at, whatever the display
/// it lands on is showing.
///
/// The other half of this module, and the opposite bargain from [`AppScale`].
/// That one is a *larger interface at the display's own sharpness*: the window
/// is made smaller and the client is told over `wp_fractional_scale_v1` to fill
/// it with as many pixels as the screen has, so nothing is resampled. This one
/// is *fewer pixels*: the window is made smaller and the client is told
/// nothing extra, so it draws exactly the picture that was asked for and that
/// picture is enlarged onto the screen.
///
/// Which is to say the two share parts 1 and 3 above and differ in part 2, and
/// that difference is the whole of what a console means by a game's
/// resolution: the work of drawing a frame is cut to a quarter and the frame
/// still covers the television. A setting that did part 2 as well would cut
/// nothing at all — the client would be asked for the same number of pixels it
/// was drawing before, laid out in a smaller interface, which is the one thing
/// nobody asks for a resolution in order to get.
///
/// Xwayland windows are included here, and left out of [`AppScale`]. The
/// exclusion there is honest: X11 has no per-surface scale, so all that could
/// be done is to magnify pixels that were already drawn, and a blurred window
/// is not a larger one. Here magnifying *is* the request — an X11 client
/// configured at 1280×720 really does draw 1280×720 pixels — so the thing that
/// made it wrong there is what makes it right here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolution {
    size: smithay::utils::Size<i32, Logical>,
}

impl Resolution {
    /// The size a shell asked for, or `None` for the display's own.
    ///
    /// `None` for a zero in either direction, which is how the protocol says
    /// "the display's own" and so how a choice is taken back. A negative size
    /// cannot arrive — the request carries two unsigned numbers — and one past
    /// what a signed pixel count can hold is refused here rather than wrapped,
    /// because the arithmetic below it is the arithmetic nobody tests.
    pub fn new(width: u32, height: u32) -> Option<Self> {
        let (width, height) = (i32::try_from(width).ok()?, i32::try_from(height).ok()?);
        (width > 0 && height > 0).then_some(Self {
            size: smithay::utils::Size::from((width, height)),
        })
    }

    /// The size itself, which is what a window carrying this is configured at.
    pub fn size(self) -> smithay::utils::Size<i32, Logical> {
        self.size
    }

    /// How far a picture this size is enlarged to cover a display of `screen`,
    /// or `None` where it does not fit inside one.
    ///
    /// Whichever of the two directions runs out first, so a size that is not
    /// the shape of the screen keeps the shape the application drew it in and
    /// leaves the rest of the display uncovered. A shell is expected to offer
    /// sizes that share the shape of the screen they will be drawn on — see
    /// the shell's own `resolution` module — so this is the answer to a
    /// mistake rather than a feature, and the thing worth protecting in a
    /// mistake is the picture's proportions.
    ///
    /// `None` rather than a factor below one for a size larger than the
    /// screen. Shrinking a picture to fit is a different setting from this one,
    /// every path below [`Mapping`] and [`visual_geometry`] is written for a
    /// factor of one or more, and a display that has since been put into a
    /// smaller mode has to fall back to its own size rather than to arithmetic
    /// nothing was written for.
    pub fn factor(self, screen: smithay::utils::Size<i32, Logical>) -> Option<f64> {
        if screen.w <= 0 || screen.h <= 0 || screen.w < self.size.w || screen.h < self.size.h {
            return None;
        }
        let factor = (screen.w as f64 / self.size.w as f64)
            .min(screen.h as f64 / self.size.h as f64)
            .max(1.0);
        Some(factor)
    }
}

/// The factor `window`'s own drawing is enlarged by: its display's, for a
/// Wayland application, and one to one for anything else.
///
/// Anything else is an Xwayland window, for the reason this module's own
/// documentation gives. A layer surface never reaches here — the shell's
/// surfaces are not windows — so nothing has to exclude it.
pub fn window_scale(scale: AppScale, window: &Window) -> f64 {
    match window.toplevel().is_some() {
        true => scale.factor(),
        false => 1.0,
    }
}

/// The size to configure a window at, on a display with `area` to give it.
///
/// Rounded down, so a window is never configured larger than the display can
/// show once it is drawn back out: at 1.5 on a 1080p screen the last row of a
/// 720.5-pixel window would be off the bottom of it.
///
/// At least one pixel each way. A window of no size is one a client cannot
/// draw into at all, and the arithmetic that gets there — an enormous scale on
/// a tiny nested window — is exactly the arithmetic nobody tests.
pub fn configured_size(
    area: smithay::utils::Size<i32, Logical>,
    factor: f64,
) -> smithay::utils::Size<i32, Logical> {
    if factor <= 1.0 {
        return area;
    }
    smithay::utils::Size::from((
        ((area.w as f64 / factor).floor() as i32).max(1),
        ((area.h as f64 / factor).floor() as i32).max(1),
    ))
}

/// What a window covers on screen, as opposed to what it was configured at.
///
/// The window's own rectangle grown about its top-left corner, which is where
/// the render anchors the same growth. Everything that asks whether a window
/// *fills* something — the frame throttle, tearing, HDR passthrough — has to
/// ask it of this: a window configured at two thirds of the display is a window
/// covering all of it, and one asked the other question would decide that no
/// application ever fills a screen the moment scaling is switched on.
pub fn visual_geometry(geometry: Rectangle<i32, Logical>, factor: f64) -> Rectangle<i32, Logical> {
    if factor <= 1.0 {
        return geometry;
    }
    Rectangle::new(
        geometry.loc,
        smithay::utils::Size::from((
            (geometry.size.w as f64 * factor).round() as i32,
            (geometry.size.h as f64 * factor).round() as i32,
        )),
    )
}

/// The step between the screen's coordinates and one window's own.
///
/// A window drawing larger than life works in a smaller space than the screen
/// does: a press two thirds of the way across a display at 150% is two thirds of
/// the way across a window that is only two thirds as wide, and the number the
/// client has to be given is the second one. This is that step, in both
/// directions, carried alongside every hit test so that nothing has to
/// rediscover which window a coordinate came through.
///
/// [`Mapping::none`] is the screen's own space, and is what the shell's layer
/// surfaces, X11 windows and every window on an unscaled session get: both
/// directions are then the identity, and the arithmetic below is skipped
/// outright rather than multiplied by one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mapping {
    /// The point both spaces agree about: the window's own corner, which is
    /// where the render anchors the same growth.
    anchor: Point<f64, Logical>,
    factor: f64,
}

impl Mapping {
    /// The screen's own coordinates, unchanged.
    pub fn none() -> Self {
        Self {
            anchor: Point::from((0.0, 0.0)),
            factor: 1.0,
        }
    }

    /// The space of a window whose corner is at `anchor`, drawn `factor` larger
    /// than it was configured.
    pub fn of(anchor: Point<f64, Logical>, factor: f64) -> Self {
        match factor == 1.0 {
            true => Self::none(),
            false => Self { anchor, factor },
        }
    }

    /// Whether this is the screen's own space, so nothing has to be converted.
    pub fn is_none(self) -> bool {
        self.factor == 1.0
    }

    /// A point on the screen, in the window's own coordinates.
    pub fn into_window(self, point: Point<f64, Logical>) -> Point<f64, Logical> {
        match self.is_none() {
            true => point,
            false => self.anchor + (point - self.anchor).downscale(self.factor),
        }
    }

    /// And back: a point in the window's coordinates, on the screen.
    ///
    /// The inverse of [`Self::into_window`], and it has to be exactly that:
    /// this is how a client's own cursor hint — a position it gives in its own
    /// coordinates, when it lets a locked pointer go — is put back where the
    /// pointer really is.
    pub fn onto_screen(self, point: Point<f64, Logical>) -> Point<f64, Logical> {
        match self.is_none() {
            true => point,
            false => self.anchor + (point - self.anchor).upscale(self.factor),
        }
    }

    /// A movement, in the window's coordinates.
    ///
    /// Relative pointer motion is measured in the same space as the ordinary
    /// motion beside it — that is what `wp_relative_pointer_v1` says it is — so
    /// a game reading its camera off this stream has to be given the same
    /// smaller numbers its `wl_pointer` events carry.
    pub fn delta_into_window(self, delta: Point<f64, Logical>) -> Point<f64, Logical> {
        match self.is_none() {
            true => delta,
            false => delta.downscale(self.factor),
        }
    }
}

/// The scale to tell a surface of `window` about: its display's density,
/// multiplied by how much larger than life that display draws applications.
///
/// Both halves matter and neither is optional. The display's is what a client
/// on a dense panel needs to know; the application's is the whole of this
/// setting. A client told only the first draws a buffer for the logical size it
/// was configured at, which is the one that has to be enlarged.
pub fn preferred_scale(output_scale: f64, factor: f64) -> f64 {
    output_scale * factor
}

/// Tell every surface of `window` the scale it should draw at.
///
/// The finished scale, not the two halves of it: an application given a
/// resolution is told exactly one whatever the display's density is, because
/// the whole of that setting is the number of pixels in the buffer and a
/// density multiplied into it would be the display's pixel count back again.
/// See [`crate::outputs::OutputManager::told_scale_on`], which is the one place
/// that decides.
///
/// Every surface, not the toplevel alone: a menu is a popup with a surface of
/// its own and a text field may be a subsurface, and a window whose menus drew
/// at a different scale from the window they hang off would be worse than one
/// that ignored the setting altogether.
///
/// Called from where the window is tiled rather than only from
/// `wp_fractional_scale_v1`'s own handler, and that is what makes the setting
/// arrive at all: a client creates its scale object as soon as it has a
/// surface, which is before the window is mapped and before the compositor
/// knows which display it belongs to. The handler's answer is the best one
/// available at that moment; this is the one that is right, and it is sent
/// alongside the configure that carries the matching size.
pub fn tell(window: &Window, scale: f64) {
    window.with_surfaces(|_, states| {
        smithay::wayland::fractional_scale::with_fractional_scale(states, |fractional| {
            // Idempotent inside smithay: a surface already drawing at this
            // scale is not sent the event again, which is what makes this safe
            // to call from every relayout.
            fractional.set_preferred_scale(scale);
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::utils::Size;

    /// Where a press on the screen lands inside the window it hit.
    ///
    /// The property that matters is the corner: a window is drawn over the whole
    /// display, so the far corner of the *display* has to arrive at the far
    /// corner of the *window*. A transform that was off by the scale would put
    /// every click a proportion of the screen away from what the user aimed at,
    /// and the further from the window's own corner they aimed the worse it
    /// would be.
    #[test]
    fn a_press_lands_where_it_was_aimed() {
        let display = Size::<i32, Logical>::from((1280, 800));
        for percent in [100, 125, 150, 200, 300] {
            let factor = AppScale::from_percent(percent).factor();
            let corner = Point::<f64, Logical>::from((240.0, 90.0));
            let mapping = Mapping::of(corner, factor);
            let configured = configured_size(display, factor);

            // The window's own corner is the point both spaces agree about.
            assert_eq!(mapping.into_window(corner), corner);

            // And the opposite corner of what is drawn is the opposite corner of
            // what the client laid out, to within the pixel the size was floored
            // by.
            let drawn = visual_geometry(Rectangle::new(corner.to_i32_round(), configured), factor);
            let far = corner + Point::from((drawn.size.w as f64, drawn.size.h as f64));
            let inside = mapping.into_window(far) - corner;
            assert!(
                (inside.x - configured.w as f64).abs() <= 1.0
                    && (inside.y - configured.h as f64).abs() <= 1.0,
                "{percent}%: the far corner of the screen lands at {inside:?} in a {configured:?} window"
            );
        }
    }

    /// The whole of what a resolution asks for, in one line of arithmetic: a
    /// picture of that size, enlarged by exactly enough to cover the screen.
    #[test]
    fn a_resolution_is_enlarged_to_exactly_cover_the_screen() {
        let screen = Size::<i32, Logical>::from((1920, 1080));
        for (size, expected) in [
            ([1600, 900], 1.2),
            ([1280, 720], 1.5),
            ([960, 540], 2.0),
            ([640, 360], 3.0),
        ] {
            let resolution = Resolution::new(size[0], size[1]).expect("a real size");
            let factor = resolution.factor(screen).expect("it fits");
            assert!((factor - expected).abs() < 1e-9, "{size:?}: {factor}");
            let drawn = visual_geometry(
                Rectangle::new(Point::from((0, 0)), resolution.size()),
                factor,
            );
            assert_eq!(drawn.size, screen, "{size:?} does not cover the screen");
        }
    }

    /// A size the display cannot show is not answered with arithmetic written
    /// for the other direction. It happens without anybody choosing it — a
    /// display put into a smaller mode, a window carried to a smaller screen —
    /// and the honest answer there is the display's own size.
    #[test]
    fn a_picture_larger_than_the_screen_is_refused_rather_than_shrunk() {
        let resolution = Resolution::new(2560, 1440).expect("a real size");
        assert_eq!(resolution.factor(Size::from((1920, 1080))), None);
        assert_eq!(resolution.factor(Size::from((2560, 1080))), None);
        assert_eq!(resolution.factor(Size::from((0, 0))), None);
        // Its own size is not larger than itself, and is drawn one to one.
        assert_eq!(resolution.factor(Size::from((2560, 1440))), Some(1.0));
    }

    /// A size that is not the shape of the screen keeps the shape the
    /// application drew it in. The shell offers no such size — see the shell's
    /// own `resolution` module — so this is what a mistake comes to, and what
    /// it must not come to is a stretched picture.
    #[test]
    fn a_picture_of_the_wrong_shape_keeps_its_proportions() {
        let resolution = Resolution::new(1280, 960).expect("a real size");
        let factor = resolution
            .factor(Size::from((1920, 1080)))
            .expect("it fits inside");
        // The direction that runs out first is the height: 1080 / 960.
        assert!((factor - 1.125).abs() < 1e-9, "{factor}");
        let drawn = visual_geometry(
            Rectangle::new(Point::from((0, 0)), resolution.size()),
            factor,
        );
        assert_eq!(drawn.size.h, 1080, "the picture should fill the height");
        assert!(drawn.size.w < 1920, "and leave the width uncovered");
    }

    /// Nothing is a size. Zero in either direction is how the protocol says
    /// "the display's own", and it has to come back as the absence of a
    /// setting rather than as a window of no width a client cannot draw into.
    #[test]
    fn nothing_is_not_a_size() {
        assert_eq!(Resolution::new(0, 0), None);
        assert_eq!(Resolution::new(1280, 0), None);
        assert_eq!(Resolution::new(0, 720), None);
        assert_eq!(Resolution::new(u32::MAX, u32::MAX), None);
    }

    /// And a press still lands where it was aimed, which is the one property
    /// every factor in this module has to keep.
    #[test]
    fn a_press_lands_where_it_was_aimed_at_a_resolution_too() {
        let screen = Size::<i32, Logical>::from((1920, 1080));
        let resolution = Resolution::new(1280, 720).expect("a real size");
        let factor = resolution.factor(screen).expect("it fits");
        let corner = Point::<f64, Logical>::from((0.0, 0.0));
        let mapping = Mapping::of(corner, factor);
        let far = Point::from((screen.w as f64, screen.h as f64));
        let inside = mapping.into_window(far);
        assert!(
            (inside.x - 1280.0).abs() < 1e-9 && (inside.y - 720.0).abs() < 1e-9,
            "the far corner of the screen lands at {inside:?}"
        );
    }

    /// And it comes back out again: a client's own cursor hint is a point in its
    /// space that has to be put back on the screen exactly.
    #[test]
    fn the_two_spaces_are_each_other_s_inverse() {
        let mapping = Mapping::of(Point::from((17.0, 23.0)), 1.5);
        for point in [(17.0, 23.0), (0.0, 0.0), (640.0, 400.0), (1279.0, 799.0)] {
            let point = Point::<f64, Logical>::from(point);
            let round_trip = mapping.onto_screen(mapping.into_window(point));
            assert!(
                (round_trip.x - point.x).abs() < 1e-9 && (round_trip.y - point.y).abs() < 1e-9,
                "{point:?} came back as {round_trip:?}"
            );
        }
    }

    /// A movement is measured in the same space as the position it moves, which
    /// is what `wp_relative_pointer_v1` says it is — and unlike a position it
    /// does not depend on where the window's corner is.
    #[test]
    fn a_movement_is_scaled_but_not_moved() {
        let mapping = Mapping::of(Point::from((100.0, 200.0)), 2.0);
        let delta = Point::<f64, Logical>::from((10.0, -6.0));
        assert_eq!(mapping.delta_into_window(delta), Point::from((5.0, -3.0)));
        assert_eq!(Mapping::none().delta_into_window(delta), delta);
    }

    /// The screen's own space, which is what the shell's surfaces, X11 windows
    /// and every window of an unscaled session are hit-tested in, changes
    /// nothing whatever.
    #[test]
    fn the_screen_s_own_space_is_the_identity() {
        let point = Point::<f64, Logical>::from((613.5, 42.25));
        for mapping in [
            Mapping::none(),
            // A factor of one is the screen's own space however it was built,
            // so nothing has to remember to check for it.
            Mapping::of(Point::from((9.0, 9.0)), 1.0),
        ] {
            assert!(mapping.is_none());
            assert_eq!(mapping.into_window(point), point);
            assert_eq!(mapping.onto_screen(point), point);
        }
    }

    #[test]
    fn nothing_below_natural_size() {
        assert_eq!(AppScale::from_percent(0).percent(), NATURAL_PERCENT);
        assert_eq!(AppScale::from_percent(50).percent(), NATURAL_PERCENT);
        assert_eq!(AppScale::from_percent(99).percent(), NATURAL_PERCENT);
    }

    #[test]
    fn nothing_past_the_ceiling() {
        assert_eq!(AppScale::from_percent(4000).percent(), LARGEST_PERCENT);
        assert_eq!(AppScale::from_percent(u32::MAX).percent(), LARGEST_PERCENT);
    }

    #[test]
    fn the_default_is_one_to_one() {
        let scale = AppScale::default();
        assert_eq!(scale.percent(), NATURAL_PERCENT);
        assert_eq!(scale.factor(), 1.0);
    }

    /// The three parts have to agree, and this is that agreement in numbers: a
    /// window configured at the smaller size, told the higher scale, draws a
    /// buffer of exactly the display's pixels.
    #[test]
    fn the_buffer_comes_back_the_size_of_the_screen() {
        let display = Size::<i32, Logical>::from((1920, 1080));
        let factor = AppScale::from_percent(150).factor();
        let configured = configured_size(display, factor);
        assert_eq!(configured, Size::from((1280, 720)));
        let buffer = (
            configured.w as f64 * preferred_scale(1.0, factor),
            configured.h as f64 * preferred_scale(1.0, factor),
        );
        assert_eq!(buffer, (1920.0, 1080.0));
    }

    /// And on a doubled screen the two multiply rather than replacing one
    /// another: 1.5× the interface on a 2× panel is a buffer three times the
    /// window's logical size.
    #[test]
    fn a_dense_panel_multiplies_rather_than_replaces() {
        assert_eq!(preferred_scale(2.0, 1.5), 3.0);
        assert_eq!(preferred_scale(2.0, 1.0), 2.0);
    }

    #[test]
    fn a_window_never_reaches_past_the_display_it_was_sized_for() {
        for percent in [100, 105, 137, 150, 175, 250, 300] {
            let factor = AppScale::from_percent(percent).factor();
            let display = Size::<i32, Logical>::from((1366, 768));
            let configured = configured_size(display, factor);
            let drawn = visual_geometry(Rectangle::from_size(configured), factor);
            assert!(
                drawn.size.w <= display.w && drawn.size.h <= display.h,
                "{percent}% draws {:?} onto {display:?}",
                drawn.size
            );
        }
    }

    /// A window put fullscreen is divided like any other, which is not a
    /// restatement of the test above it: fullscreen is the one size a client
    /// asks for rather than being given, and it is asked for against the whole
    /// display rather than the area left over.
    ///
    /// The bug, in numbers. Until this division was made on that path too, a
    /// fullscreen configure carried the display's own size: the client filled
    /// it at the scale it had been told, and the growth that lays an ordinary
    /// window over the whole display laid that one a factor past it. A video
    /// put fullscreen from a browser on a session at 150% came back with a
    /// third of its width and a third of its height off the screen.
    #[test]
    fn a_fullscreen_window_is_divided_like_any_other() {
        let display = Size::<i32, Logical>::from((1280, 800));
        let factor = AppScale::from_percent(150).factor();

        let room = configured_size(display, factor);
        assert_eq!(room, Size::from((853, 533)));
        assert_eq!(
            visual_geometry(Rectangle::from_size(room), factor).size,
            display,
            "the display is what a fullscreen window has to end up covering"
        );

        // And what the display's own size comes to when it is handed over
        // undivided and then drawn out: half as much again in each direction,
        // which is the cropping itself.
        assert_eq!(
            visual_geometry(Rectangle::from_size(display), factor).size,
            Size::from((1920, 1200))
        );
    }

    #[test]
    fn no_window_is_configured_away_to_nothing() {
        let tiny = Size::<i32, Logical>::from((2, 1));
        let configured = configured_size(tiny, AppScale::from_percent(300).factor());
        assert_eq!(configured, Size::from((1, 1)));
    }

    /// A natural session is left alone exactly, rather than multiplied by a
    /// factor that happens to be one: these are the paths every frame and every
    /// press of an ordinary session goes through.
    #[test]
    fn natural_size_changes_nothing() {
        let area = Size::<i32, Logical>::from((1920, 1080));
        assert_eq!(configured_size(area, 1.0), area);
        let geometry = Rectangle::new((7, 11).into(), area);
        assert_eq!(visual_geometry(geometry, 1.0), geometry);
    }
}
