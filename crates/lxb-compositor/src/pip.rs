//! The one window on this session that floats: a browser's picture-in-picture.
//!
//! Everything else here fills the display it opened on. That is the whole
//! layout — a console has one thing on screen at a time, and
//! [`crate::outputs::OutputManager::tile_window_on_output`] enforces it against
//! clients that ask for anything else. A picture-in-picture window is the one
//! honest exception: it exists precisely to be *not* what the user is doing,
//! kept in a corner while they do something else, and a maximized one is not
//! picture-in-picture at all. So it is taken out of the layout and given a
//! corner of its own.
//!
//! # What says a window is one
//!
//! Its title, and nothing else. Every browser that has the feature gives that
//! window the same title — "Picture-in-Picture" — and none of them gives it a
//! separate app_id: the window belongs to the browser and calls itself by the
//! browser's name, which is also what the window the video came out of calls
//! itself. There is no window type to read either; it is an ordinary toplevel.
//!
//! Matching on a title is matching on a string a client can put anything it
//! likes into, and that is worth saying out loud rather than hiding. What it
//! buys is a window drawn small in a corner. What it costs, if some program
//! were to name a window that on purpose, is the same: a small window in a
//! corner, still running, still clickable, still closable by the application
//! that made it. That is a bargain worth taking for a feature the user can
//! switch off from the page that describes it.
//!
//! # How it is drawn round
//!
//! With a mat, not with a mask. A client's buffer is a rectangle with four
//! square corners, and nothing in the renderer this compositor is generic over
//! can cut a curve out of one: the interface it is written against draws
//! textures and solid colours, and the shader that could do it belongs to one
//! of the three backends rather than to all of them.
//!
//! So the corner is covered instead of cut. A painted image is laid over the
//! window's own edges — rounded on the outside at `lxb_protocol::pip`'s frame
//! radius, rounded on the inside at what is left of it after the mat's
//! thickness, and opaque in between — and the window is configured to the
//! opening rather than to the whole shape. What the user sees is a rounded
//! window with a hairline surround and a shadow under it; what the corners
//! really are is a few hundred pixels of paint over four right angles. The
//! arithmetic that makes that hold — why a mat three pixels thick can only
//! round a corner so far — is `lxb_protocol::pip`, where the shell can see it
//! too.

use std::cell::RefCell;

use lxb_protocol::overview::Rect;
use lxb_protocol::pip;
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::element::texture::TextureRenderElement;
use smithay::backend::renderer::element::{Id, Kind};
use smithay::backend::renderer::utils::RendererSurfaceState;
use smithay::backend::renderer::{ContextId, ImportMem, Renderer, Texture};
use smithay::desktop::Window;
use smithay::input::pointer::CursorIcon;
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::Resource;
use smithay::utils::{IsAlive, Logical, Physical, Point, Rectangle, Size, Transform};

/// The title a browser gives the window, and the whole of the test.
pub const TITLE: &str = "Picture-in-Picture";

/// How many painted mats are kept.
///
/// One per floating window on the busiest session anybody has, and one spare for
/// the shape a window has just left — a video being resized walks through a run
/// of them, and repainting the one it came from is what a cache is for. Twice
/// that, because a selected window is two pictures: its mat, and the accent mark
/// laid over it. See [`Pip::mark`].
const MATS: usize = 8;

/// The mat's own colour, sRGB, and the hairline that catches the light at its
/// outer edge.
///
/// Near black and deliberately quiet. It is a frame around somebody's picture:
/// the accent belongs to the things the user is choosing between, and a frame
/// painted in it would be the brightest object on a screen the user is not
/// looking at. The rim is the one concession — without it the mat has no edge
/// at all against a dark game, and the window's shape stops being legible.
const MAT: [f32; 3] = [0.055, 0.055, 0.065];

/// The same colour, opaque and premultiplied, as the renderer draws a solid: it
/// is what fills the mat's opening behind the window. See
/// [`crate::render::push_floating_windows`], where the argument for it is.
pub const BACKDROP: [f32; 4] = [MAT[0], MAT[1], MAT[2], 1.0];
const RIM: [f32; 3] = [0.30, 0.30, 0.34];

/// How far the rim reaches in from the outer curve, in logical pixels, and how
/// much of the lamp it is allowed to carry.
///
/// The same lamp every glyph in the shell is lit by — up and to the left — so
/// the light on this shape agrees with the light on the marks drawn over it.
const RIM_WIDTH: f64 = 1.0;
const RIM_LIGHT: f32 = 0.9;

/// How dark the shadow is directly under the mat, and how far the shape is
/// dropped before it is cast, as a share of the shadow's reach.
///
/// Something that floats has to be seen to be floating; over a dark game a
/// window with no shadow is a hole in the picture rather than a thing on top of
/// it.
const SHADOW_DEPTH: f32 = 0.42;
const SHADOW_DROP: f64 = 0.25;

/// How long the mark on the selected window takes to breathe out and back, in
/// seconds.
///
/// The shell's own `PULSE_PERIOD`, transcribed. Everything the user is choosing
/// between in this session breathes at this rate — a tile of the bar, a row of
/// a menu, a card in the guide — and a video marked at any other rate would be
/// the one selected thing on the screen keeping its own time. The two clocks
/// are not in step and cannot be, but the tempo is the same.
const MARK_PERIOD: f64 = 1.8;

/// How much of the accent the mark carries at the bottom of its breath and at
/// the top of it.
///
/// Never nothing: the surround of a selected window is accent for as long as it
/// is selected, and a pulse that went out entirely would be a window that looked
/// unselected twice a second. The glow around it is what the rest of the travel
/// is spent on.
const MARK_LOW: f32 = 0.55;
const MARK_HIGH: f32 = 1.0;

/// How far the glow reaches out from the shape, as a share of the shadow the
/// mat is already drawn with, and how bright it is where it leaves the frame.
///
/// Inside the shadow rather than past it: the image the mat is painted into is
/// the shape plus its shadow, so a glow that reached further would need a larger
/// picture, a different origin and a window that moved a pixel when it was
/// selected. What is there is enough — the shadow is generous, and a glow is
/// read by its brightest edge rather than by where it ends.
const GLOW_REACH: f64 = 0.85;
const GLOW_LIGHT: f32 = 0.55;

/// How long a floating window takes to arrive in its corner, and how long it
/// takes to go.
///
/// Going is the quicker of the two, as it is everywhere else in this session:
/// arriving is something the user is being shown and wants to watch land, and
/// leaving is something they have already decided on and are waiting through.
/// Both are short enough that a video is *playing* rather than opening — a
/// window that took half a second to appear would read as the browser being
/// slow rather than as the session being smooth.
const ARRIVES_OVER: std::time::Duration = std::time::Duration::from_millis(240);
const LEAVES_OVER: std::time::Duration = std::time::Duration::from_millis(180);

/// How long a floating window takes to settle into a new place, or a new shape.
///
/// Longer than either the arrival or the departure, because this one is a
/// *spring* and a bounce nobody can see is a jump with extra steps. This is the
/// column closing up behind a window somebody has just pulled out of it, and
/// the whole column changing size because somebody moved a slider on the
/// Settings page — both of them things the user did on purpose and is watching
/// the result of.
const SETTLES_OVER: std::time::Duration = std::time::Duration::from_millis(480);

/// The two numbers that spring is made of: how quickly the wobble dies away,
/// and how much of a wobble there is.
///
/// A decaying cosine — `1 − e^(−decay·t)·cos(wobble·t)`. It leaves nought at
/// rest, goes about a tenth past its destination a third of the way through,
/// comes back a hundredth short of it, and is there. One good bounce and the
/// ghost of a second, which is what reads as something soft rather than as
/// something sprung.
///
/// The wobble is three and a half half-turns for one specific reason: a cosine
/// is exactly zero there, so the curve *lands* on its destination at the end of
/// [`SETTLES_OVER`] rather than a fraction of a percent short of it. A fraction
/// of a percent of a five-hundred-pixel window is two pixels, and two pixels
/// appearing on the last frame of an animation is the jump this is here to
/// remove.
const SPRING_DECAY: f64 = 8.0;
const SPRING_WOBBLE: f64 = 3.5 * std::f64::consts::PI;

/// The fraction of its own size a floating window starts at and falls back to
/// — the depth in the two directions the whole animation is made of.
///
/// Slight, and deliberately so. This window arrives over whatever the user is
/// actually doing, so it has to announce itself without taking the screen: a
/// tenth of its own size is a step forward out of the display, and anything
/// more is a thing thrown at the viewer. It is also as far as the frame can be
/// scaled before the mat's hairline stops being a hairline — see
/// [`crate::render`], where the mat, the picture and the backing are all put
/// through the one transform so that none of them can be scaled without the
/// others.
const DEPTH: f64 = 0.88;

/// What the shell has asked for: whether such a window floats at all, how large
/// it is drawn and which corner it sits in.
///
/// Not remembered anywhere. The shell says what this is as soon as it connects,
/// which is long before any application exists to put a video in — the bargain
/// [`crate::scale::AppScale`] is under, and for the same reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    /// Off, and a window titled this is an application window like any other:
    /// maximized, listed to the shell, given the keyboard. That is what this
    /// compositor did before it could be asked, and it is the right answer for
    /// somebody who does not want a video following them around.
    pub floating: bool,
    pub size: pip::Size,
    pub place: pip::Place,
}

impl Default for Settings {
    /// On, a quarter of the display's width, upper right — the corner the
    /// feature is named after everywhere it exists.
    fn default() -> Self {
        Self {
            floating: true,
            size: pip::Size::default(),
            place: pip::Place::default(),
        }
    }
}

/// What one floating window's layout has settled, kept on the window itself.
///
/// Three things that the two halves of drawing it have to agree about, and one
/// conversation.
///
/// The **aspect** is the client's, and it takes a question to get: a
/// picture-in-picture window is not born floating. A browser maps it, this
/// compositor tiles it to the whole display like every other window, and only
/// then does the browser set the title that says what it is — so by the time it
/// starts floating, the only shape it has is the shape *we* gave it, and asking
/// its geometry would be asking for our own answer back. So it is asked
/// outright: the first configure it gets as a floating window carries no size at
/// all, which is xdg-shell for "choose one", and the next size it draws that we
/// did not tell it to draw is the shape it wanted all along. Until then,
/// [`lxb_protocol::pip`]'s default stands in.
///
/// **The answer is read for as long as the window floats**, not once. A client
/// says one thing about itself and then draws what it is told, so this normally
/// happens exactly once; but a browser puts a video of another shape into the
/// same window, and — the reason this stopped being a single reading — the first
/// size a client draws is very often not a window at all. See
/// [`A_WINDOW_AT_LEAST`], which is the whole of that bug.
///
/// The **frame** is where the layout put it, recorded so the render draws its
/// mat around exactly the rectangle the client was configured into. The two used
/// to work it out separately from the same inputs, which is a way of saying they
/// could disagree.
#[derive(Debug, Default)]
pub struct Floating {
    /// The shape the client asked to be, once it has said.
    aspect: std::cell::Cell<Option<f64>>,
    /// Whether the question has been put at all.
    asked: std::cell::Cell<bool>,
    /// The last size this window was *told* to be — the shape it drawing is no
    /// news, because it is our own shape read back. Set to whatever it happened
    /// to be showing when the question went out, which is the answer to some
    /// configure of ours from when it was an ordinary window.
    told: std::cell::Cell<Option<Size<i32, Logical>>>,
    /// How many times it has changed its mind. See [`ANSWERS`].
    answers: std::cell::Cell<u32>,
    /// The identity the mat's backing colour is drawn under, made once so the
    /// damage tracker sees one long-lived element rather than a new one every
    /// frame.
    backdrop: std::cell::OnceCell<Id>,
    /// The shape the layout last placed it in — what the mat is painted for.
    frame: std::cell::Cell<Option<pip::Frame>>,
    /// Where the user put it, if they ever have: the outer rectangle, in its
    /// display's own logical coordinates.
    ///
    /// `None` is the ordinary case — the window stands in the column the
    /// settings describe and moves when they change. A window that has been
    /// dragged or resized is *detached*: it keeps this rectangle, it is no
    /// longer counted in that column, and the place it used to stand in is free
    /// for the next video that starts floating. See
    /// [`crate::outputs::OutputManager::floating_column`], which is where a
    /// window leaving the column is a window the next one can stand where.
    placed: std::cell::Cell<Option<Rect>>,
    /// When this window started floating, counted in the order they did.
    ///
    /// What decides which of two floating windows keeps the corner, and it is
    /// deliberately *not* the stack: a window is raised by being clicked on, and
    /// a second video that jumped into the corner because somebody pressed pause
    /// on the first one would be two windows swapping places under the hand
    /// doing it. First one there keeps it; the next stands below.
    since: std::cell::Cell<Option<u64>>,
    /// The moment this window was first *drawn* in its corner, which is when
    /// its arrival started.
    ///
    /// Not the moment it started floating, which is a different instant and the
    /// wrong one: a window starts floating when its client names it, and is
    /// drawn some frames later — once the layout has a rectangle for it, and
    /// once the client has answered what shape it wants to be. Started from the
    /// naming, the fade would be over before there was anything on screen to
    /// fade, and the video would appear at full size in a corner exactly as it
    /// did before any of this was written.
    arrived: std::cell::Cell<Option<std::time::Instant>>,
    /// The shape it was standing in when the layout last moved it, and the
    /// moment that happened — what the spring runs *from*.
    ///
    /// `None` once it has settled, which is every frame of a video in a corner
    /// nobody has touched. What it is set by is a *layout*: the column closing
    /// up behind a window somebody pulled out of it, a second video arriving
    /// under the first, or a press on the Settings page changing how large
    /// these windows are and which corner they sit in. Never by a hand: a
    /// window under the pointer is where the pointer is, and a window that
    /// sprang along behind the hand dragging it would be a window that had
    /// stopped being dragged.
    settling: std::cell::Cell<Option<(Rect, std::time::Instant)>>,
    /// What the *user* said this window is, if they have ever said: floating,
    /// or an ordinary window like every other one.
    ///
    /// `None` is every window on an ordinary session — the title is the whole
    /// answer, which is what [`can_float`] reads. A menu row is what fills this
    /// in, in either direction: a video told to fill the display it is in the
    /// corner of, and an application told to go and sit in that corner. From
    /// then on it outranks the title, and it has to: the whole point of the
    /// first is a window that goes on calling itself Picture-in-Picture and is
    /// no longer treated as one.
    ///
    /// Kept on the window, so it dies with it and no two windows can inherit
    /// each other's answer — and deliberately **not cleared by
    /// [`Floating::forget`]**, which runs on the very change this records. A
    /// wish forgotten there would put the window straight back where it came
    /// from, once per pass of the event loop, forever.
    wish: std::cell::Cell<Option<bool>>,
    /// The corner a window was standing in when the user asked for it to fill
    /// the display, in that display's own coordinates.
    ///
    /// The window has stopped floating by the time this is read, so nothing
    /// else remembers the shape it left: the flight out to the whole display is
    /// grown from this. Taken up by the first commit that draws at a different
    /// size, which is the frame the window has actually become large — see
    /// [`crate::state::LxbState::fly_out_of_the_corner`], where the argument for
    /// waiting for it is. Not cleared by [`Floating::forget`], for the reason
    /// [`Floating::wish`] is not: it is set in the same breath as the change
    /// that runs it.
    left: std::cell::Cell<Option<(Rect, Size<i32, Logical>)>>,
    /// Whether a hand is on this window at this moment — a pointer drag or a
    /// controller one, between one movement of it and the next.
    ///
    /// The one thing that stops a window springing. It is deliberately not
    /// *whether it has been dragged*: a window let go of somewhere, or a drag
    /// cancelled and the window put back where it started, is a window nobody
    /// is holding any more, and both of those should glide rather than jump.
    /// What must not spring is a window under a hand that is still moving it.
    held: std::cell::Cell<bool>,
}

impl Floating {
    /// The shape to lay this window out at.
    pub fn aspect(&self) -> f64 {
        self.aspect.get().unwrap_or(pip::DEFAULT_ASPECT)
    }

    /// The rectangle the layout placed this window in, if it has been laid out
    /// since it started floating.
    pub fn frame(&self) -> Option<pip::Frame> {
        self.frame.get()
    }

    /// Take the rectangle the layout settled — and, when it is a change worth
    /// watching, start this window springing towards it.
    ///
    /// Three windows never spring, and each of them for its own reason:
    ///
    /// - One the user is **holding**. A window being dragged is placed where
    ///   their hand is and is re-laid-out on every movement of it; a spring
    ///   there would be a window lagging behind the pointer dragging it, which
    ///   is a window that has stopped being dragged. Only while the hand is on
    ///   it: letting go somewhere, or cancelling the drag and putting it back,
    ///   both glide. See [`Floating::held`].
    /// - One that has **not finished arriving**. It is already animating, from
    ///   nothing and from behind the screen, and a client answering what shape
    ///   it wants to be — which it does a few frames in, and which moves the
    ///   whole column — must not turn that into two animations at once.
    /// - One being laid out for the **first** time. There is no shape for it to
    ///   spring *from*: it has never been anywhere.
    ///
    /// Where it springs from is where it is standing at this moment rather than
    /// where the layout last put it. Those are the same thing for a window at
    /// rest and quite different for one already in the middle of a spring —
    /// which is the ordinary case when somebody is dragging the Size slider on
    /// the Settings page, and which without this would have every step of that
    /// drag jump back to the shape before it.
    pub fn placed_in(&self, frame: pip::Frame, now: std::time::Instant) {
        let was = self.frame.get();
        let standing = self.standing_in(now);
        self.frame.set(Some(frame));
        if self.held.get() || !self.has_arrived(now) {
            self.settling.set(None);
            return;
        }
        let Some(was) = was else {
            return;
        };
        if was.outer == frame.outer {
            return;
        }
        self.settling
            .set(Some((standing.unwrap_or(was.outer), now)));
    }

    /// The rectangle this window is *drawn* in `now`, while that is not the one
    /// the layout settled — and nothing when it is, which is nearly always.
    ///
    /// The outer shape, mat included: everything else about the window is
    /// measured from it, so one rectangle is one answer for all of it.
    pub fn standing_in(&self, now: std::time::Instant) -> Option<Rect> {
        let (from, at) = self.settling.get()?;
        let to = self.frame.get()?.outer;
        let through = settling(now.saturating_duration_since(at))?;
        Some(between(from, to, through))
    }

    /// Take, or give back, the hand that is on this window. See
    /// [`Floating::held`].
    pub fn hold(&self, held: bool) {
        self.held.set(held);
    }

    /// Whether it is still on its way somewhere, which is what asks for the
    /// next frame to be drawn.
    pub fn is_settling(&self, now: std::time::Instant) -> bool {
        self.standing_in(now).is_some()
    }

    /// Whether this window has been drawn at all and has finished arriving.
    ///
    /// Asked without starting the arrival's clock, which is why it reads the
    /// cell rather than going through [`Floating::arriving`]: this is asked
    /// from the *layout*, and a window whose arrival began when it was laid out
    /// would have faded in before it was ever on screen.
    fn has_arrived(&self, now: std::time::Instant) -> bool {
        self.arrived
            .get()
            .is_some_and(|at| arrival(now.saturating_duration_since(at)).is_none())
    }

    /// What the mat's own colour is drawn under, behind the window.
    ///
    /// Made once and kept, so the damage tracker sees one long-lived element
    /// rather than a new one every frame.
    pub fn backdrop(&self) -> Id {
        self.backdrop.get_or_init(Id::new).clone()
    }

    /// Everything this window said about itself, forgotten — for a window that
    /// has stopped floating, so that a video put back into a corner asks its
    /// question again rather than answering with the shape of the last one.
    ///
    /// Everything the *client* said. What the **user** said is not here:
    /// [`Floating::wish`] and [`Floating::left`] are both written in the same
    /// breath as the change that calls this, and forgetting either one here
    /// would undo the very press that made it.
    pub fn forget(&self) {
        self.aspect.set(None);
        self.asked.set(false);
        self.told.set(None);
        self.answers.set(0);
        self.frame.set(None);
        self.placed.set(None);
        self.since.set(None);
        self.arrived.set(None);
        self.settling.set(None);
        self.held.set(false);
    }

    /// Where the user put this window, if they have — see [`Floating::placed`].
    pub fn placed(&self) -> Option<Rect> {
        self.placed.get()
    }

    /// Put it there, which is also what takes it out of the column.
    pub fn place_at(&self, outer: Rect) {
        self.placed.set(Some(outer));
    }

    /// Back into the column, wherever it had been dragged to.
    ///
    /// What the settings do. Somebody who has just chosen a corner on the
    /// Settings page has said where they want their videos, and a window left
    /// in the middle of the screen because it was once dragged there would be
    /// the session ignoring them. Every other change leaves a placed window
    /// alone.
    pub fn reattach(&self) {
        self.placed.set(None);
    }

    /// What the user said this window is, if they have said. See
    /// [`Floating::wish`].
    pub fn wished(&self) -> Option<bool> {
        self.wish.get()
    }

    /// Say it. Nothing else ever writes this: it is one menu row, in one
    /// direction or the other.
    pub fn wish(&self, floating: bool) {
        self.wish.set(Some(floating));
    }

    /// Whether this window is on its way out of a corner at all, which is the
    /// question every commit on the session asks and all but a handful answer
    /// no to.
    pub fn is_leaving(&self) -> bool {
        self.left.get().is_some()
    }

    /// The corner this window left to fill its display, and the size it was
    /// drawing at when it left — until something takes them. See
    /// [`Floating::left`].
    pub fn take_the_corner_it_left(&self, drawn: Size<i32, Logical>) -> Option<Rect> {
        let (corner, was) = self.left.get()?;
        if was == drawn {
            return None;
        }
        self.left.set(None);
        Some(corner)
    }

    /// Write them down on the way out of the corner, or take them back — a
    /// window sent back to a corner before it ever finished leaving the last
    /// one has nothing left to fly out of.
    pub fn leaving(&self, corner: Option<(Rect, Size<i32, Logical>)>) {
        self.left.set(corner);
    }

    /// When this window started floating, taking the next place in the order
    /// the first time it is asked.
    pub fn since(&self, next: impl FnOnce() -> u64) -> u64 {
        match self.since.get() {
            Some(since) => since,
            None => {
                let since = next();
                self.since.set(Some(since));
                since
            }
        }
    }

    /// The same, without taking a place — for a window being counted rather
    /// than placed.
    pub fn floating_since(&self) -> Option<u64> {
        self.since.get()
    }

    /// How this window is to be drawn `now` — how much of it there is, and the
    /// fraction of its own size it stands at — while it is still arriving, and
    /// nothing once it has.
    ///
    /// Asking is what starts the clock, the first time, so the animation begins
    /// on the frame the window is first drawn on and not before. See
    /// [`Floating::arrived`] and [`arrival`].
    pub fn arriving(&self, now: std::time::Instant) -> Option<(f32, f64)> {
        let started = match self.arrived.get() {
            Some(started) => started,
            None => {
                self.arrived.set(Some(now));
                now
            }
        };
        arrival(now.saturating_duration_since(started))
    }

    /// Put the question, once. `true` while it is going out, which is what says
    /// this configure carries no size.
    ///
    /// `drawn` is what the window is showing at that moment. That is an answer
    /// to some configure of ours from back when it was an ordinary window, so it
    /// is written down as *ours* rather than read as its own.
    pub fn ask(&self, drawn: Size<i32, Logical>) -> bool {
        if self.asked.get() {
            return false;
        }
        self.asked.set(true);
        self.told.set(Some(drawn));
        true
    }

    /// The size this window has just been sent, which is the one shape its
    /// drawing tells us nothing by.
    pub fn told(&self, size: Size<i32, Logical>) {
        self.told.set(Some(size));
    }

    /// Read what the client has drawn, and take it as its answer if it is one.
    /// `true` when the shape changed, which is what asks for the column to be
    /// laid out again.
    ///
    /// Asked at the commit, because the answer *is* the size the client has just
    /// drawn and that is the only moment it exists. Five things are not answers:
    /// a window that has not been asked yet, a size that is not a window at all
    /// (see [`A_WINDOW_AT_LEAST`]), the size we ourselves last sent, a shape we
    /// are already drawing it at *to within the whole pixels a client is
    /// configured in* — which is the common case, and the one that keeps this
    /// to a subtraction per commit — and one more mind-change than [`ANSWERS`]
    /// allows.
    pub fn read_answer(&self, drawn: Size<i32, Logical>) -> bool {
        if !self.asked.get() {
            return false;
        }
        if drawn.w < A_WINDOW_AT_LEAST || drawn.h < A_WINDOW_AT_LEAST {
            return false;
        }
        if self.told.get() == Some(drawn) {
            return false;
        }
        // A size within a pixel of the shape it is already being drawn at is
        // that shape read back, not a new one. Asked in pixels rather than as a
        // ratio because that is what the difference is made of: a client is
        // configured at whole pixels, so the height it draws is our own shape
        // rounded, and at a couple of hundred pixels tall half a pixel is
        // already a quarter of a percent of the ratio. Comparing ratios put
        // that noise over any fixed threshold and read it as the video changing
        // shape — which cost three of this window's [`ANSWERS`] during one drag
        // of its corner, and walked its aspect a little further from the video's
        // with each one.
        let aspect = drawn.w as f64 / drawn.h as f64;
        if (drawn.h as f64 - drawn.w as f64 / self.aspect()).abs() <= 1.0 {
            return false;
        }
        if self.answers.get() >= ANSWERS {
            return false;
        }
        self.answers.set(self.answers.get() + 1);
        self.aspect.set(Some(aspect));
        tracing::debug!(
            width = drawn.w,
            height = drawn.h,
            answer = self.answers.get(),
            "a floating window said what shape it wants to be"
        );
        true
    }
}

/// The smallest thing that is a window, in logical pixels each way.
///
/// A client's first commit is very often not a window: Firefox's
/// picture-in-picture window draws **one pixel by one** before it has laid
/// anything out. A placeholder has no shape to say anything with, and read as an
/// answer that one says *square* — which is what put a sixteen-to-nine video in
/// a square frame with the session showing through underneath it, on the day
/// this was written. Nothing this small is a picture of anything, so nothing
/// this small is an answer.
const A_WINDOW_AT_LEAST: i32 = 32;

/// How many times a client may change its mind about its own shape.
///
/// A client says one thing and then draws what it is told, so one answer is the
/// ordinary case and a second is a video swapped for one of another shape in the
/// same window. A client that answers a *different* shape to every size it is
/// sent is arguing with the layout, and that argument has a fixed point: the
/// width is the setting's share of the display and never moves, so every round
/// drags the height towards it and the two meet at a square. A handful of rounds
/// covers everything honest and leaves the rest of the session's frames alone.
const ANSWERS: u32 = 8;

/// The floating state hanging off one window, made the first time it is asked
/// for.
pub fn floating_state(window: &Window) -> &Floating {
    window.user_data().insert_if_missing(Floating::default);
    window
        .user_data()
        .get::<Floating>()
        .expect("the floating state was just inserted")
}

/// One surface of a floating window as the renderer last had it: everything
/// needed to put that picture back on the screen, and nothing at all that
/// belongs to the client — which by then has gone.
///
/// This is the whole trick behind a window that fades out. A picture-in-picture
/// window does not stop floating: it is *destroyed*, because that is what a
/// browser does when the video goes back into its page or the user closes it.
/// A destroyed surface has no buffer, no texture and no state, so a fade drawn
/// from the window itself would have nothing to draw — the video would vanish
/// on the first frame of the animation and the mat would fade out around a
/// hole. What is kept instead is the picture: the texture handle the renderer
/// already made, which outlives the client that gave it to us, together with
/// the numbers that say where on the screen it goes.
struct KeptSurface<T> {
    /// The same identity the live surface was drawn under, so the damage
    /// tracker sees one element carrying on rather than a new one appearing
    /// where a window has just been destroyed.
    id: Id,
    /// Where this surface sits inside the window's picture, in that display's
    /// logical coordinates.
    ///
    /// Logical rather than physical so the same note can be drawn at any scale:
    /// it is asked for again while the small picture behind the shell's glass
    /// is being built, which is this frame at another size.
    offset: Point<i32, Logical>,
    texture: T,
    scale: i32,
    transform: Transform,
    /// What part of the buffer is shown and how large it is drawn — the
    /// client's own viewport, exactly as it last set it.
    src: Rectangle<f64, Logical>,
    dst: Size<i32, Logical>,
}

impl<T: Texture + Clone + 'static> KeptSurface<T> {
    /// Take a note of one surface as the renderer has it now.
    ///
    /// Nothing for a surface with no picture in it yet, and nothing for one
    /// this renderer has never imported — both of which are frames there would
    /// be nothing to keep from.
    fn of(
        id: Id,
        offset: Point<i32, Logical>,
        context: ContextId<T>,
        data: &RendererSurfaceState,
    ) -> Option<Self> {
        let view = data.view()?;
        Some(Self {
            id,
            offset,
            texture: data.texture(context)?.clone(),
            scale: data.buffer_scale(),
            transform: data.buffer_transform(),
            src: view.src,
            dst: view.dst,
        })
    }
}

/// A floating window's last picture: every surface of it, and the renderer
/// context those textures belong to.
pub struct KeptPicture<T: Texture> {
    context: ContextId<T>,
    surfaces: Vec<KeptSurface<T>>,
}

impl<T: Texture + Clone + 'static> KeptPicture<T> {
    pub fn new(context: ContextId<T>) -> Self {
        Self {
            context,
            surfaces: Vec::new(),
        }
    }

    /// Keep one more surface of the tree, if there is anything of it to keep.
    pub fn keep(&mut self, id: Id, offset: Point<i32, Logical>, data: &RendererSurfaceState) {
        if let Some(surface) = KeptSurface::of(id, offset, self.context.clone(), data) {
            self.surfaces.push(surface);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.surfaces.is_empty()
    }

    /// The picture again: drawn from `at` — the physical point the window's
    /// surface tree starts at — with `alpha` of it left.
    ///
    /// Front to back, as every element list in this compositor is: the tree was
    /// walked back to front when it was kept, so it is handed back reversed.
    ///
    /// Nothing at all to a renderer that is not the one these textures were
    /// made by. The type is no guarantee on its own: the udev backend has a
    /// renderer per GPU and they all speak in the same kind of texture, so a
    /// second card asked to draw the first card's picture would draw whatever
    /// happened to be at that name in its own context. That is what a context
    /// identity is for, and this is the one place it can be checked.
    pub fn elements(
        &self,
        context: ContextId<T>,
        at: Point<f64, Physical>,
        scale: smithay::utils::Scale<f64>,
        alpha: f32,
    ) -> Vec<TextureRenderElement<T>> {
        if self.context.clone().erased() != context.erased() {
            return Vec::new();
        }
        self.surfaces
            .iter()
            .rev()
            .map(|surface| {
                TextureRenderElement::from_static_texture(
                    surface.id.clone(),
                    self.context.clone(),
                    at + surface.offset.to_f64().to_physical(scale),
                    surface.texture.clone(),
                    surface.scale,
                    surface.transform,
                    Some(alpha),
                    Some(surface.src),
                    Some(surface.dst),
                    // Never opaque, whatever the client said about its buffer:
                    // this picture is drawn at a fading alpha, and the mat
                    // behind it has to show through as it goes.
                    None,
                    Kind::Unspecified,
                )
            })
            .collect()
    }
}

/// Where a floating window was drawn: the shape the layout settled, the two
/// corners that shape put it at, and how much its own drawing had to be shrunk
/// to fit.
///
/// One value rather than four loose numbers because they are one answer, worked
/// out once per frame and used twice: to draw the window, and to write down
/// where it was in case that was the last frame of it.
#[derive(Debug, Clone, Copy)]
pub struct Placed {
    /// The shape it was drawn in, which is what the mat is painted for.
    pub frame: pip::Frame,
    /// Where its *picture* starts: the opening, plus whatever centring a client
    /// drawing smaller than its opening was given.
    pub corner: Point<f64, Logical>,
    /// Where its *surface tree* starts, which is before that by however much of
    /// itself a client draws outside its own geometry.
    pub origin: Point<f64, Logical>,
    /// How much its own drawing had to be shrunk to fit the opening.
    pub factor: f64,
}

/// A floating window's last picture and everything about where it was drawn,
/// held without knowing which renderer drew it.
///
/// The texture type belongs to the renderer, and the compositor's state is not
/// generic over one — there are three backends and the udev one has a renderer
/// per GPU. So the picture itself is carried type-erased and asked for by the
/// renderer that comes to draw it; one that does not recognise it draws the mat
/// and the frame without it, which is the same fade with nothing inside it.
pub struct LastPicture {
    /// Which display it was drawn on, by name.
    output: String,
    placed: Placed,
    /// What the colour behind it was drawn under, carried across because the
    /// window whose user data held it has gone: a new identity every frame
    /// would have the damage tracker redrawing the whole rectangle for a fade
    /// that only changes its alpha.
    backdrop: Id,
    /// The surfaces themselves — a `KeptPicture<R::TextureId>`.
    picture: Box<dyn std::any::Any>,
}

impl std::fmt::Debug for LastPicture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LastPicture")
            .field("output", &self.output)
            .field("placed", &self.placed)
            .finish_non_exhaustive()
    }
}

impl LastPicture {
    pub fn new<T: Texture + Clone + 'static>(
        output: String,
        placed: Placed,
        backdrop: Id,
        picture: KeptPicture<T>,
    ) -> Self {
        Self {
            output,
            placed,
            backdrop,
            picture: Box::new(picture),
        }
    }

    /// Where the window was when this was taken.
    pub fn placed(&self) -> Placed {
        self.placed
    }

    /// What the colour behind it is drawn under.
    pub fn backdrop(&self) -> Id {
        self.backdrop.clone()
    }

    /// The picture, if it was this renderer that took it.
    pub fn picture<T: Texture + Clone + 'static>(&self) -> Option<&KeptPicture<T>> {
        self.picture.downcast_ref::<KeptPicture<T>>()
    }
}

/// A floating window on its way out: the last picture of it, and when it
/// started leaving.
#[derive(Debug)]
struct Going {
    /// The window it was, so that a video put straight back into a corner
    /// replaces the one still fading rather than being drawn over it.
    id: u32,
    kept: LastPicture,
    started: std::time::Instant,
}

/// The floating window's settings, the painted mat that goes round it, and
/// which windows were floating the last time anything looked.
#[derive(Debug, Default)]
pub struct Pip {
    settings: Settings,
    /// How many windows have started floating on this session, which is what
    /// gives each of them its place in the order — see [`Floating::since`].
    order: std::cell::Cell<u64>,
    /// The windows that were floating at the last pass, by the id the overview
    /// knows them under.
    ///
    /// A window does not arrive floating: a browser maps it and titles it a
    /// moment later, and puts the video back by retitling it again. So what
    /// makes this work is noticing that the answer *changed* — see
    /// [`crate::state::LxbState::refresh_floating_windows`], which is the one
    /// place that writes this.
    floating: std::collections::HashSet<u32>,
    /// The mats painted so far, newest first. Behind a cell because they are
    /// asked for while a frame is being assembled, which has the compositor only
    /// by shared reference.
    ///
    /// More than one, because there can be more than one shape on screen: two
    /// floating windows are two sizes now that each is the shape its own client
    /// asked to be, and one image would be repainted twice a frame — a hundred
    /// milliseconds of processor on a 4K panel, for a picture that was already
    /// drawn. Kept short and dropped from the end: the shapes in play are the
    /// windows on screen, and a session with more than a few is a session with
    /// something else wrong with it.
    mats: RefCell<Vec<Mat>>,
    /// Which floating window the shell has handed its directions to, and the
    /// colour to mark it in — 0xRRGGBB, straight from the shell's accent.
    ///
    /// One per session rather than one per display: the guide the selection is
    /// offered from is only ever on the screen being driven, so there is only
    /// ever one window it could be about. See
    /// [`crate::state::LxbState::select_floating_window`].
    selected: Option<(u32, u32)>,
    /// The surface the shell is drawing a context menu on, and nothing else:
    /// the one thing this session draws *in front of* these windows.
    ///
    /// One per session, for the reason the selection is: a context menu is one
    /// panel on one display, and the shell says which surface it is on for
    /// exactly as long as it is up. Nothing at all the rest of the time, which
    /// is nearly always — and that is what keeps the whole of this free on a
    /// session where nobody has opened one. See
    /// [`crate::state::LxbState::set_menu_surface`].
    menu: Option<WlSurface>,
    /// The last picture drawn of each floating window on the session, taken
    /// afresh every frame.
    ///
    /// Behind a cell for the reason the mats are: this is written while a frame
    /// is being assembled, which has the compositor only by shared reference.
    /// It costs one texture handle cloned per surface of a floating window per
    /// frame — a reference count, not a picture — and nothing at all on a
    /// session with no video parked in a corner.
    ///
    /// The window that is *drawn* is the one kept, not the one that exists:
    /// what this is for is the moment after the client has gone, and by then
    /// the only true thing left about that window is what was last on screen.
    last: RefCell<std::collections::HashMap<u32, LastPicture>>,
    /// The windows that have gone and are still being drawn while they fade.
    ///
    /// Empty nearly always, and at most a handful for a fifth of a second when
    /// it is not. See [`Going`] and [`departure`].
    going: Vec<Going>,
}

/// A painted mat, and the shape it was painted for.
#[derive(Debug)]
struct Mat {
    /// Physical pixels across and down, the logical shape and scale they were
    /// painted from, and the accent this is the selection mark for — `None` for
    /// the mat itself. Compared rather than the whole frame, because these are
    /// what change the picture: a window moved from one corner to another is the
    /// same mat in a different place.
    key: (i32, i32, u64, Rectangle<i32, Physical>, Option<u32>),
    buffer: MemoryRenderBuffer,
    /// How large the image is drawn, which is the shape with its shadow.
    size: Size<i32, Logical>,
}

impl Pip {
    pub fn settings(&self) -> Settings {
        self.settings
    }

    /// Which windows were floating at the last pass.
    pub fn were_floating(&self) -> &std::collections::HashSet<u32> {
        &self.floating
    }

    /// The next place in the order windows started floating in.
    pub fn next_in_order(&self) -> u64 {
        let next = self.order.get().wrapping_add(1);
        self.order.set(next);
        next
    }

    /// Take a fresh note of it.
    pub fn now_floating(&mut self, windows: std::collections::HashSet<u32>) {
        self.floating = windows;
    }

    /// Take what the shell asked for. `true` when it is a change, which is what
    /// says whether every window has to be laid out again.
    pub fn set(&mut self, settings: Settings) -> bool {
        if self.settings == settings {
            return false;
        }
        self.settings = settings;
        true
    }

    /// Where every floating window on a display this size goes, one per shape
    /// handed in and in the order they started floating. Empty while the
    /// feature is switched off.
    ///
    /// The whole column at once, because where the second window starts is a
    /// fact about the first one's height: see [`lxb_protocol::pip::frames`].
    pub fn frames(&self, output: Size<i32, Logical>, aspects: &[f64]) -> Vec<pip::Frame> {
        if !self.settings.floating || output.w <= 0 || output.h <= 0 {
            return Vec::new();
        }
        pip::frames(
            output.w as f64,
            output.h as f64,
            self.settings.size,
            self.settings.place,
            aspects,
        )
    }

    /// Which floating window the shell has the directions on, and the colour it
    /// asked for that window to be marked in.
    pub fn selected(&self) -> Option<(u32, u32)> {
        self.selected
    }

    /// Take the shell's word for it. `true` when it is a change, which is what
    /// says whether the screen has to be drawn again.
    pub fn select(&mut self, selected: Option<(u32, u32)>) -> bool {
        if self.selected == selected {
            return false;
        }
        self.selected = selected;
        true
    }

    /// Whether this surface is the one the shell is drawing a context menu on,
    /// and so the one thing drawn in front of a floating window.
    pub fn is_a_menu(&self, surface: &WlSurface) -> bool {
        self.menu.as_ref() == Some(surface)
    }

    /// Whether there is a menu up at all, which is what the render and the
    /// pointer both ask before doing any of the work below.
    pub fn has_a_menu(&self) -> bool {
        self.menu.as_ref().is_some_and(|menu| menu.is_alive())
    }

    /// Take the shell's word for it. `true` when it is news.
    ///
    /// A surface that has gone is forgotten in the same breath: a shell that
    /// stopped without saying so leaves its own behind, and nothing else would
    /// ever clear it.
    pub fn name_a_menu(&mut self, surface: Option<WlSurface>) -> bool {
        self.menu.take_if(|menu| !menu.is_alive());
        if self.menu == surface {
            return false;
        }
        self.menu = surface;
        true
    }

    /// Take a note of what a floating window last looked like, so that it can
    /// still be drawn once its client has gone. See [`LastPicture`].
    pub fn keep(&self, id: u32, picture: LastPicture) {
        self.last.borrow_mut().insert(id, picture);
    }

    /// Start a window that has stopped floating on its way out, and answer
    /// whether there is anything to draw.
    ///
    /// Nothing for a window nothing ever drew — one that was named
    /// picture-in-picture and renamed before the layout ever placed it, which
    /// no browser does but a client is free to. There is no picture of such a
    /// window and no shape it was ever in, so there is nothing for a fade to be
    /// a fade of, and it simply is not there.
    pub fn let_go(&mut self, id: u32, now: std::time::Instant) -> bool {
        let Some(kept) = self.last.borrow_mut().remove(&id) else {
            return false;
        };
        // A window leaving twice is the same window: replace it rather than
        // stacking a second fade on the first.
        self.going.retain(|going| going.id != id);
        self.going.push(Going {
            id,
            kept,
            started: now,
        });
        true
    }

    /// Forget a window entirely — both the picture kept of it and any fade it
    /// is in the middle of.
    ///
    /// What a window that starts floating *again* asks for: the same id is back
    /// in the corner, and a copy of the last one still fading over it would be
    /// the video appearing twice.
    pub fn forget_a_window(&mut self, id: u32) {
        self.last.borrow_mut().remove(&id);
        self.going.retain(|going| going.id != id);
    }

    /// Drop every window that has finished leaving. `true` when one has, which
    /// is what says the screen has to be drawn again without it.
    pub fn forget_the_gone(&mut self, now: std::time::Instant) -> bool {
        let before = self.going.len();
        self.going
            .retain(|going| departure(now.saturating_duration_since(going.started)).is_some());
        before != self.going.len()
    }

    /// Whether anything is on its way out at all, which is what the render asks
    /// before doing any of the work below.
    pub fn anything_going(&self) -> bool {
        !self.going.is_empty()
    }

    /// Every window leaving `output`, with how much of it is left and how large
    /// it is drawn — oldest first, so two of them going at once keep the order
    /// they stood in.
    pub fn going_on(&self, output: &str, now: std::time::Instant) -> Vec<(&LastPicture, f32, f64)> {
        self.going
            .iter()
            .filter(|going| going.kept.output == output)
            .filter_map(|going| {
                let (alpha, depth) = departure(now.saturating_duration_since(going.started))?;
                Some((&going.kept, alpha, depth))
            })
            .collect()
    }

    /// The mat for `frame`, ready to draw at `at` — the top left of the shape
    /// *and its shadow*, relative to the output.
    ///
    /// Painted at the display's own pixels and drawn at the size it was painted
    /// for, so the curve is as sharp as the panel allows rather than being a
    /// small picture stretched over a large window. Repainted only when the
    /// shape or the display changes, which is a press on the Settings page and
    /// nothing else.
    ///
    /// `alpha` is how much of it there is, which is 1.0 for every frame of a
    /// window that is simply there and less only while one is arriving or
    /// leaving. It is a number handed to the renderer and never a repaint: the
    /// picture that fades is the picture that was already in hand, for the same
    /// reason the selection mark's breath is — see [`Pip::mark`].
    pub fn mat<R>(
        &self,
        renderer: &mut R,
        frame: &pip::Frame,
        scale: f64,
        opening: Rectangle<i32, Physical>,
        alpha: f32,
    ) -> Option<MemoryRenderBufferRenderElement<R>>
    where
        R: Renderer + ImportMem,
        R::TextureId: Send + Clone + 'static,
    {
        self.picture(renderer, frame, scale, opening, None, Some(alpha))
    }

    /// The mark that says this is the window the user's directions are on: the
    /// same shape as the mat, in the shell's accent, drawn over it at `alpha`.
    ///
    /// A second picture rather than a mat painted in the accent, and that is the
    /// whole of why the pulse costs nothing. A colour that breathes is a
    /// different colour sixty times a second, and a mat is a few hundred
    /// thousand distance fields to paint — so a session with a video selected
    /// would spend a mat's worth of processor on every frame of it. Painted once
    /// and faded up and down instead, the breath is a number handed to the
    /// renderer, and the picture behind it is the one that was already there.
    ///
    /// `accent` is packed 0xRRGGBB, exactly as the shell sent it, and is part of
    /// the cache key: the user changing accent repaints this and nothing else.
    pub fn mark<R>(
        &self,
        renderer: &mut R,
        frame: &pip::Frame,
        scale: f64,
        opening: Rectangle<i32, Physical>,
        accent: u32,
        alpha: f32,
    ) -> Option<MemoryRenderBufferRenderElement<R>>
    where
        R: Renderer + ImportMem,
        R::TextureId: Send + Clone + 'static,
    {
        self.picture(renderer, frame, scale, opening, Some(accent), Some(alpha))
    }

    /// One of the two pictures drawn round a floating window, painted if it is
    /// not already in hand.
    fn picture<R>(
        &self,
        renderer: &mut R,
        frame: &pip::Frame,
        scale: f64,
        opening: Rectangle<i32, Physical>,
        accent: Option<u32>,
        alpha: Option<f32>,
    ) -> Option<MemoryRenderBufferRenderElement<R>>
    where
        R: Renderer + ImportMem,
        R::TextureId: Send + Clone + 'static,
    {
        let mut held = self.mats.borrow_mut();
        // Where the image lands on the display, and therefore where the opening
        // falls *inside* it. Both in whole physical pixels, and both worked out
        // from the same two functions the render uses to place the window
        // itself — a mat three pixels thick has nothing to spare for the two
        // halves disagreeing about which pixel an edge is on.
        let at = origin(frame, scale);
        let hole = Rectangle::<i32, Physical>::new(opening.loc - at, opening.size);
        let logical = Size::<i32, Logical>::from((
            (frame.outer.w + frame.shadow * 2.0).ceil() as i32,
            (frame.outer.h + frame.shadow * 2.0).ceil() as i32,
        ));
        let width = ((logical.w as f64) * scale).ceil() as i32;
        let height = ((logical.h as f64) * scale).ceil() as i32;
        // The radius carries the whole of the shape's identity that the two
        // sizes above do not: a window of the same size on a display of a
        // different height is rounded differently. And so does where the
        // opening falls, which moves with the fraction of a pixel the window's
        // corner lands on.
        let key = (width, height, frame.radius.to_bits(), hole, accent);
        if width <= 0 || height <= 0 {
            return None;
        }
        match held.iter().position(|mat| mat.key == key) {
            // Newest first, so the shapes on screen stay in front of the ones
            // that were on screen a moment ago.
            Some(found) => {
                let mat = held.remove(found);
                held.insert(0, mat);
            }
            None => {
                let pixels = match accent {
                    Some(accent) => paint_mark(frame, scale, width, height, hole, accent),
                    None => paint(frame, scale, width, height, hole),
                };
                held.insert(
                    0,
                    Mat {
                        key,
                        buffer: MemoryRenderBuffer::from_slice(
                            &pixels,
                            Fourcc::Abgr8888,
                            (width, height),
                            1,
                            Transform::Normal,
                            None,
                        ),
                        size: logical,
                    },
                );
                held.truncate(MATS);
            }
        }

        let mat = held.first()?;
        // The source is the whole painted image, in the buffer's own logical
        // units — which are its pixels, because it is a scale-one buffer — and
        // the size is the shape it was painted for. Together they put every
        // texel of it on exactly one pixel of the display it was painted at.
        match MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            at.to_f64(),
            &mat.buffer,
            alpha,
            Some(Rectangle::from_size(Size::from((
                width as f64,
                height as f64,
            )))),
            Some(mat.size),
            Kind::Unspecified,
        ) {
            Ok(element) => Some(element),
            // The window is still drawn — square-cornered, and better than not
            // at all. Nothing else in the frame depends on this.
            Err(err) => {
                tracing::warn!(?err, "could not upload the picture-in-picture mat");
                None
            }
        }
    }
}

/// Whether this window is the one a browser has put a video into.
///
/// The title, trimmed and folded: a client that capitalises it differently
/// means the same thing by it, and one that pads it means the same thing too.
/// Note that [`crate::shell_control::window_title`] answers "Application" for a
/// window whose client set no title at all, so a nameless window can never
/// match this by accident.
pub fn titled_picture_in_picture(window: &Window) -> bool {
    title_says_picture_in_picture(&crate::shell_control::window_title(window))
}

/// Whether this window is one this compositor may float.
///
/// The title — or, where the user has said, what *they* said instead. A row of
/// the floating window's own menu takes a video out of its corner and gives it
/// the whole display; a row of the guide's window card menu sends an
/// application the other way. Either one outranks the title from then on, which
/// is the whole of what those rows are for: a browser goes on calling a window
/// Picture-in-Picture after the user has said they want to watch it properly.
/// See [`Floating::wish`] and `lxb_shell_v1.set_window_floating`.
///
/// And one exception to both: a window whose geometry is its client's own
/// business. An X11 menu, notification or splash — and any override-redirect
/// window — is placed where its client asked and is never re-tiled, so it is the
/// one kind of window a corner cannot be given to. Left out here rather than in
/// the layout alone, because being floated is two decisions taken in two places:
/// where the window goes, and where it is drawn with its mat. A window the first
/// refuses and the second accepts would be drawn with a rounded surround sized
/// for a rectangle it is not in.
pub fn can_float(window: &Window) -> bool {
    floating_state(window)
        .wished()
        .unwrap_or_else(|| titled_picture_in_picture(window))
        && !crate::input::window_is_x11_chrome(window)
        && !window
            .x11_surface()
            .is_some_and(|surface| surface.is_override_redirect())
}

/// The same question of the title itself, so it can be asked without a window
/// — which is the only way it can be asked in a test.
pub fn title_says_picture_in_picture(title: &str) -> bool {
    title.trim().eq_ignore_ascii_case(TITLE)
}

// -- the hand on the window -------------------------------------------------
//
// A floating window is the one window on this session that is not simply where
// the layout put it, and so the only one there is anything to point at: every
// other window fills a display and has nowhere else to be. It has edges to pull
// and a middle to carry it by, and this is the arithmetic of both.
//
// Compositor-only, unlike the shape arithmetic in `lxb_protocol::pip`: the
// shell has no pointer and no part in a drag. What it does own is where a
// window goes before any hand touches one — which is why a press on the
// Settings page puts every window back. See [`Floating::reattach`].

/// How wide the band along each edge of a floating window is that resizes it
/// rather than reaching the video, in logical pixels.
///
/// Wider than the mat is thick, on purpose. Three pixels of frame is a hairline
/// to look at and nothing at all to hit — a hand would miss it more often than
/// not — so the band reaches inside the picture. What that costs is the
/// outermost few pixels of the client's own window, which on a video is video
/// and around a browser's controls is the air they sit in.
pub const GRAB: f64 = 8.0;

/// How far the pointer travels with the button held before a press inside the
/// window stops being a click and starts carrying it, in logical pixels.
///
/// The press itself reaches the client the moment it happens, so a play button
/// answers instantly; this only decides whether what happened *was* a click.
/// Small enough that carrying feels like it started under the hand, large
/// enough that a press on a small window's own button does not tow the window
/// across the screen with it.
pub const TAKES: f64 = 4.0;

/// Which edge of the window a hand has hold of, in one dimension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pull {
    /// The left edge, or the top: the near one, which moves while the far side
    /// of the window stays where it is.
    Start,
    /// The right edge, or the bottom.
    End,
}

/// What a press on a floating window does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handle {
    /// Anywhere else: the window is carried.
    Move,
    /// An edge — or a corner, which is two edges at once. `None` in a dimension
    /// means neither of that dimension's edges is being pulled, which is what
    /// makes an edge and a corner one case here instead of eight.
    Edge { x: Option<Pull>, y: Option<Pull> },
}

/// What a press at `point` on this window would do, or `None` where the point
/// is not on the window at all.
///
/// `point` is in the display's own logical coordinates, as the frame is.
///
/// The band is measured *inwards* from the outer edge and never outwards. The
/// shadow reaches eighteen pixels past the frame and belongs to no window: a
/// band out there would swallow presses meant for the application the video is
/// lying on top of, which is the one thing a window in a corner must never do.
pub fn handle_at(frame: &pip::Frame, point: Point<f64, Logical>) -> Option<Handle> {
    let outer = frame.outer;
    if point.x < outer.x || point.y < outer.y || point.x > outer.right() || point.y > outer.bottom()
    {
        return None;
    }
    // Never more than a third of the window each way, so that a video dragged
    // down to the smallest size the layout draws still has a middle to be
    // carried by rather than being all edges.
    let band = GRAB.min(outer.w / 3.0).min(outer.h / 3.0);
    let pull = |near: f64, far: f64| match (near <= band, far <= band) {
        (true, false) => Some(Pull::Start),
        (false, true) => Some(Pull::End),
        // Both, on a window narrower than two bands: whichever edge is nearer.
        (true, true) => Some(match near <= far {
            true => Pull::Start,
            false => Pull::End,
        }),
        (false, false) => None,
    };
    let x = pull(point.x - outer.x, outer.right() - point.x);
    let y = pull(point.y - outer.y, outer.bottom() - point.y);
    Some(match (x, y) {
        (None, None) => Handle::Move,
        (x, y) => Handle::Edge { x, y },
    })
}

/// The pointer's shape over a handle, or `None` where the client's own cursor
/// stands.
///
/// The middle of the window is the client's. A browser's picture-in-picture
/// draws its own pointer over its own controls, and painting a hand over it
/// would be this compositor claiming a window it is not carrying — it only
/// starts carrying if the hand goes on to move. The edges are ours from the
/// moment the pointer is on them, so they say so.
pub fn icon(handle: Handle) -> Option<CursorIcon> {
    let Handle::Edge { x, y } = handle else {
        return None;
    };
    Some(match (x, y) {
        (Some(Pull::Start), Some(Pull::Start)) => CursorIcon::NwResize,
        (Some(Pull::End), Some(Pull::Start)) => CursorIcon::NeResize,
        (Some(Pull::Start), Some(Pull::End)) => CursorIcon::SwResize,
        (Some(Pull::End), Some(Pull::End)) => CursorIcon::SeResize,
        (Some(Pull::Start), None) => CursorIcon::WResize,
        (Some(Pull::End), None) => CursorIcon::EResize,
        (None, Some(Pull::Start)) => CursorIcon::NResize,
        (None, Some(Pull::End)) => CursorIcon::SResize,
        (None, None) => return None,
    })
}

/// Where a window carried by `delta` ends up on a display this size.
pub fn carried(
    origin: Rect,
    delta: Point<f64, Logical>,
    display: Size<i32, Logical>,
    aspect: f64,
) -> Rect {
    pip::hand_placed(
        Rect {
            x: origin.x + delta.x,
            y: origin.y + delta.y,
            ..origin
        },
        display.w as f64,
        display.h as f64,
        aspect,
    )
}

/// Where a window dragged by one of its edges — or by a corner, which is two of
/// them — ends up.
///
/// **The shape is the client's throughout.** A hand on a corner scales the
/// window; it does not restretch the video. So only one of the two dimensions
/// is ever what the hand said and the other is what the client's shape makes of
/// it, and a corner takes whichever of the two was pulled further, so the window
/// follows the diagonal rather than one arbitrary side of it.
///
/// **The edges the hand did not touch stay where they were.** Pulling the left
/// edge moves the left edge: the right one has not been touched and must not
/// wander. The dimension nobody has hold of keeps its middle instead, because
/// there is no edge in it to have anchored — a window made taller by its top
/// edge grows the same amount either side rather than lurching right.
///
/// Every rule about how large a window may be lives in
/// [`lxb_protocol::pip::hand_placed`] and is applied here twice: once to settle
/// the size before the anchored edges are put back, and once at the end, where
/// it is the clamp that keeps the whole thing on the screen.
pub fn resized(
    origin: Rect,
    x: Option<Pull>,
    y: Option<Pull>,
    delta: Point<f64, Logical>,
    display: Size<i32, Logical>,
    aspect: f64,
) -> Rect {
    let (width, height) = (display.w as f64, display.h as f64);
    let asked_w = match x {
        Some(Pull::Start) => origin.w - delta.x,
        Some(Pull::End) => origin.w + delta.x,
        None => origin.w,
    };
    let asked_h = match y {
        Some(Pull::Start) => origin.h - delta.y,
        Some(Pull::End) => origin.h + delta.y,
        None => origin.h,
    };
    // The height read as a width, so that the two can be compared at all: the
    // mat is drawn round the picture, so it is the *opening* that has the
    // client's shape and the two sides of the frame are outside it.
    let mat = pip::BORDER * 2.0;
    let from_h = (asked_h - mat).max(1.0) * aspect + mat;
    let wanted = match (x, y) {
        (Some(_), Some(_)) => asked_w.max(from_h),
        (Some(_), None) => asked_w,
        (None, Some(_)) => from_h,
        (None, None) => origin.w,
    };
    let sized = pip::hand_placed(
        Rect {
            w: wanted,
            ..origin
        },
        width,
        height,
        aspect,
    );
    let (w, h) = (sized.w, sized.h);
    let at = |pull: Option<Pull>, near: f64, was: f64, now: f64| match pull {
        Some(Pull::Start) => near + was - now,
        Some(Pull::End) => near,
        None => near + (was - now) / 2.0,
    };
    pip::hand_placed(
        Rect {
            x: at(x, origin.x, origin.w, w),
            y: at(y, origin.y, origin.h, h),
            w,
            h,
        },
        width,
        height,
        aspect,
    )
}

/// Which corner to resize a floating window from, when nobody has taken hold of
/// one: the corner with the most room to grow into.
///
/// Room is the rectangle between that corner of the window and the same corner
/// of the display — the space the window has to grow into if it is pulled that
/// way — and the largest of the four wins. A window parked in the top right is
/// pulled from the bottom left, because that is where the screen is; one in the
/// middle is pulled from whichever way it has the most room.
///
/// Ties go to the bottom right, which is the corner everything else is resized
/// from and the one a hand goes to first.
pub fn resize_corner(outer: Rect, display: Size<i32, Logical>) -> (Pull, Pull) {
    let (width, height) = (display.w as f64, display.h as f64);
    let left = outer.x.max(0.0);
    let right = (width - outer.right()).max(0.0);
    let top = outer.y.max(0.0);
    let bottom = (height - outer.bottom()).max(0.0);
    let corners = [
        (Pull::End, Pull::End, right * bottom),
        (Pull::Start, Pull::End, left * bottom),
        (Pull::End, Pull::Start, right * top),
        (Pull::Start, Pull::Start, left * top),
    ];
    let best = corners
        .iter()
        .copied()
        .fold(corners[0], |best, corner| match corner.2 > best.2 {
            true => corner,
            false => best,
        });
    (best.0, best.1)
}

/// Where a corner of the window is, for putting the pointer on it.
pub fn corner_point(outer: Rect, x: Pull, y: Pull) -> Point<f64, Logical> {
    Point::from((
        match x {
            Pull::Start => outer.x,
            Pull::End => outer.right(),
        },
        match y {
            Pull::Start => outer.y,
            Pull::End => outer.bottom(),
        },
    ))
}

/// The middle of the window, for the same reason.
pub fn middle(outer: Rect) -> Point<f64, Logical> {
    Point::from((outer.x + outer.w / 2.0, outer.y + outer.h / 2.0))
}

/// What ends a drag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Until {
    /// The button that started it comes back up: a hand holding the window.
    Released(u32),
    /// A button is clicked. Nothing is held down — the drag was started from
    /// the window's own menu, and the button that chose the row was let go of
    /// before the window ever moved. A left click leaves the window where it
    /// is; any other button puts it back where it was.
    Clicked,
    /// The shell says so. No pointer is involved at any point: this is a
    /// controller's hand on the window, and what moves it is
    /// [`crate::state::LxbState::drag_floating_window`] rather than any motion
    /// the seat ever hears about. A click ends it too — a session can have both
    /// controls plugged in, and a mouse that could not put down what a pad had
    /// picked up would be a window stuck to the stick.
    Told,
}

/// A hand on a floating window: which window it has hold of, by what, and
/// where it all started.
///
/// Kept on the compositor rather than in a smithay pointer grab, because this
/// is not the client's drag: no client asked for it, no client is told about
/// it, and the pointer goes on being this compositor's own throughout — see
/// [`crate::input::LxbState::carry_a_floating_window`], which is the whole of
/// what a drag does.
#[derive(Debug, Clone)]
pub struct Drag {
    pub window: Window,
    pub handle: Handle,
    /// The display it started on, and the only one it can end on: a window
    /// belongs to the screen it opened on here, and a hand does not change
    /// that any more than a launch does.
    pub output: Output,
    /// What ends it, and what a button pressed in the middle of it means.
    pub until: Until,
    /// Where the window was before the drag: its own rectangle if it had
    /// already been placed by hand, and `None` if it was standing in the
    /// column. What a cancelled drag is put back to — which is a state and not
    /// only a rectangle, because a window that was in the column belongs back
    /// in the column.
    pub was: Option<Rect>,
    /// Where the pointer was when the button went down, in the display's own
    /// coordinates.
    ///
    /// Meaningless under [`Until::Told`], which has no pointer to have been
    /// anywhere: a controller's drag measures each step from where the window is
    /// now rather than from where anything started. See
    /// [`crate::state::LxbState::drag_floating_window`].
    pub from: Point<f64, Logical>,
    /// The window's outer rectangle at that moment, in the same coordinates.
    pub origin: Rect,
    /// Whether the window is actually following the hand yet. An edge is from
    /// the first moment — the press never reaches the client at all. The middle
    /// waits for the pointer to travel [`TAKES`], because until it has, what is
    /// happening is a click on somebody's play button.
    pub carrying: bool,
}

/// Paint the mat: `width` × `height` physical pixels of it, at `scale`.
///
/// One pass, in logical coordinates taken back to the shape's own corner. The
/// two rounded rectangles are distance fields — see
/// [`lxb_protocol::pip::inside_rounded`] — so every edge in the picture
/// antialiases itself over exactly one physical pixel, and the corner is a
/// curve rather than a staircase.
fn paint(
    frame: &pip::Frame,
    scale: f64,
    width: i32,
    height: i32,
    hole: Rectangle<i32, Physical>,
) -> Vec<u8> {
    let mut pixels = vec![0_u8; width as usize * height as usize * 4];
    let shadow = frame.shadow;
    // The opening, in this image's own pixels rather than in logical units:
    // that is the whole point of being handed it. Its edges then fall on whole
    // pixels — the same ones the client's own buffer is snapped to — so a mat
    // three pixels thick is three pixels and not two and two halves.
    let hole_x = hole.loc.x as f64 + OVERLAP;
    let hole_y = hole.loc.y as f64 + OVERLAP;
    let hole_w = (hole.size.w as f64 - OVERLAP * 2.0).max(1.0);
    let hole_h = (hole.size.h as f64 - OVERLAP * 2.0).max(1.0);
    let inner_radius = (frame.inner_radius() * scale - OVERLAP).max(0.0);
    // One physical pixel, in the logical units the shape itself is measured in.
    // It is what an edge is allowed to be soft over.
    let pixel = 1.0 / scale.max(0.001);

    // The middle of the opening is not painted at all. Every pixel a whole one
    // inside it comes out transparent — it is neither mat nor shadow, and the
    // buffer starts transparent — and it is the great majority of the image: a
    // mat is a ring and a shadow is a band around one. Skipping it is what
    // keeps a window being dragged by its corner from repainting a couple of
    // million distance fields a frame to arrive at nothing. The span is the
    // opening's own rectangle, inset past the antialiased edge *and* past the
    // corner arcs, which are the only part of it that is not a rectangle.
    let clear_top = hole_y + inner_radius + 1.0;
    let clear_bottom = hole_y + hole_h - inner_radius - 1.0;
    let (clear_left, clear_right) = (hole_x + 1.0, hole_x + hole_w - 1.0);

    for (row, line) in pixels.chunks_mut(width as usize * 4).enumerate() {
        // Pixel centres, so the shape is not shifted half a pixel up and left.
        let y = (row as f64 + 0.5) * pixel - shadow;
        let clear = (row as f64 + 0.5 > clear_top) && ((row as f64 + 0.5) < clear_bottom);
        for (column, pixel_out) in line.chunks_exact_mut(4).enumerate() {
            if clear && column as f64 + 0.5 > clear_left && (column as f64 + 0.5) < clear_right {
                continue;
            }
            let x = (column as f64 + 0.5) * pixel - shadow;

            let outside = pip::inside_rounded(x, y, frame.outer.w, frame.outer.h, frame.radius);
            // The opening, in physical pixels, and drawn a half of one *inside*
            // the rectangle the client was configured at — see [`OVERLAP`],
            // which is why a mat lies over the edge of a picture rather than
            // beside it.
            let opening = pip::inside_rounded(
                column as f64 + 0.5 - hole_x,
                row as f64 + 0.5 - hole_y,
                hole_w,
                hole_h,
                inner_radius,
            );
            // Inside the shape, and outside the window's own opening: that is
            // the mat, and it is the only opaque thing here.
            let mat = coverage(outside, pixel) * (1.0 - coverage(opening, 1.0));

            // The lamp, up and to the left, over the outermost sliver of the
            // mat. `outside` falls off in every direction from the shape's
            // edge, so how far in a pixel is says how much rim it takes, and
            // which way it faces says how much light.
            let lit = ((shadow - x + shadow - y) / (frame.outer.w + frame.outer.h)).clamp(0.0, 1.0);
            let rim = (1.0 - (outside / RIM_WIDTH).clamp(0.0, 1.0)) as f32 * RIM_LIGHT * lit as f32;
            let body = [
                MAT[0] + (RIM[0] - MAT[0]) * rim,
                MAT[1] + (RIM[1] - MAT[1]) * rim,
                MAT[2] + (RIM[2] - MAT[2]) * rim,
            ];

            // And the shadow, cast by the same shape dropped a little, over
            // whatever is behind the window. Deepest against the mat's own edge
            // and gone a shadow's reach out from it.
            //
            // Nothing of it is drawn where the shape itself is — the last
            // factor — because the shape is the mat and the mat's opening, and
            // an alpha in the opening would be a veil over the video.
            let dropped = pip::inside_rounded(
                x,
                y - shadow * SHADOW_DROP,
                frame.outer.w,
                frame.outer.h,
                frame.radius,
            );
            let reach = ((shadow + dropped) / shadow.max(0.001)).clamp(0.0, 1.0);
            let cast = (reach * reach * (1.0 - coverage(outside, pixel))) as f32 * SHADOW_DEPTH;

            let mat = mat as f32;
            let alpha = mat + cast * (1.0 - mat);
            // Premultiplied, which is what the renderer blends: the shadow's
            // own colour is black, so only the mat contributes any.
            for channel in 0..3 {
                pixel_out[channel] = (body[channel].clamp(0.0, 1.0) * mat * 255.0).round() as u8;
            }
            pixel_out[3] = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }
    pixels
}

/// How much of a pixel a distance field covers: all of it a pixel inside the
/// edge, none of it a pixel outside, and a straight ramp across the edge
/// itself.
fn coverage(distance: f64, pixel: f64) -> f64 {
    (distance / pixel + 0.5).clamp(0.0, 1.0)
}

/// Paint the mark that says a floating window is the selected one: the mat's own
/// ring in `accent`, and a glow of it reaching out into the shadow.
///
/// The same image as [`paint`] in every dimension — same pixels, same origin,
/// same opening — so the two lie exactly on top of one another and the mark can
/// simply be drawn over the mat. And transparent inside the opening for the
/// reason the shadow is: an alpha over the picture would be a veil over
/// somebody's video, and this one would be a coloured veil.
///
/// `accent` is packed 0xRRGGBB. It is unpacked here rather than at the protocol
/// edge so that what crosses the compositor is the one number the shell sent,
/// all the way to the pixel it becomes.
fn paint_mark(
    frame: &pip::Frame,
    scale: f64,
    width: i32,
    height: i32,
    hole: Rectangle<i32, Physical>,
    accent: u32,
) -> Vec<u8> {
    let mut pixels = vec![0_u8; width as usize * height as usize * 4];
    let shadow = frame.shadow;
    let tint = [
        ((accent >> 16) & 0xff) as f32 / 255.0,
        ((accent >> 8) & 0xff) as f32 / 255.0,
        (accent & 0xff) as f32 / 255.0,
    ];
    let hole_x = hole.loc.x as f64 + OVERLAP;
    let hole_y = hole.loc.y as f64 + OVERLAP;
    let hole_w = (hole.size.w as f64 - OVERLAP * 2.0).max(1.0);
    let hole_h = (hole.size.h as f64 - OVERLAP * 2.0).max(1.0);
    let inner_radius = (frame.inner_radius() * scale - OVERLAP).max(0.0);
    let pixel = 1.0 / scale.max(0.001);
    // How far out the glow goes, and never nothing: a window on a display with
    // no room for a shadow still has to be markable.
    let reach = (shadow * GLOW_REACH).max(pixel);

    // The middle of the opening is skipped, exactly as it is in [`paint`] and
    // for the same reason — it is the great majority of the image and none of it
    // is drawn.
    let clear_top = hole_y + inner_radius + 1.0;
    let clear_bottom = hole_y + hole_h - inner_radius - 1.0;
    let (clear_left, clear_right) = (hole_x + 1.0, hole_x + hole_w - 1.0);

    for (row, line) in pixels.chunks_mut(width as usize * 4).enumerate() {
        let y = (row as f64 + 0.5) * pixel - shadow;
        let clear = (row as f64 + 0.5 > clear_top) && ((row as f64 + 0.5) < clear_bottom);
        for (column, pixel_out) in line.chunks_exact_mut(4).enumerate() {
            if clear && column as f64 + 0.5 > clear_left && (column as f64 + 0.5) < clear_right {
                continue;
            }
            let x = (column as f64 + 0.5) * pixel - shadow;

            let outside = pip::inside_rounded(x, y, frame.outer.w, frame.outer.h, frame.radius);
            let opening = pip::inside_rounded(
                column as f64 + 0.5 - hole_x,
                row as f64 + 0.5 - hole_y,
                hole_w,
                hole_h,
                inner_radius,
            );
            // The ring: the mat's own footprint, and the accent goes over all of
            // it. What the user has to be able to see at a glance is which of
            // two windows in a corner is the one their next press is about, and
            // a tint over a near-black frame is not that.
            let ring = (coverage(outside, pixel) * (1.0 - coverage(opening, 1.0))) as f32;

            // And the glow, out from the shape's own edge rather than from a
            // shape dropped below it: this is light coming off the frame, not
            // something the frame is casting. Squared, so it leaves the frame
            // bright and is gone well before the end of its reach.
            let out_by = (-outside).max(0.0);
            let fall = (1.0 - out_by / reach).clamp(0.0, 1.0);
            let glow = (fall * fall * (1.0 - coverage(outside, pixel))) as f32 * GLOW_LIGHT;

            let alpha = ring + glow * (1.0 - ring);
            // Premultiplied, as the mat is. Every pixel here is the one colour,
            // so the only thing that varies across the picture is how much of it
            // there is.
            for channel in 0..3 {
                pixel_out[channel] = (tint[channel].clamp(0.0, 1.0) * alpha * 255.0).round() as u8;
            }
            pixel_out[3] = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }
    pixels
}

/// How much of the accent the selection mark carries `at` seconds into a
/// session.
///
/// A breath rather than a blink: it never leaves, and it never reaches the same
/// value twice in a row. See [`MARK_PERIOD`], [`MARK_LOW`] and [`MARK_HIGH`].
pub fn mark_alpha(at: std::time::Duration) -> f32 {
    let phase = at.as_secs_f64() * std::f64::consts::TAU / MARK_PERIOD;
    let breath = (0.5 + 0.5 * phase.sin()) as f32;
    MARK_LOW + (MARK_HIGH - MARK_LOW) * breath
}

/// How a floating window is drawn `elapsed` into its arrival: how much of it
/// there is, and how large it is drawn as a fraction of its own size.
///
/// `None` once it has arrived, which is every frame of a video but the first
/// handful — and that is the point of answering it this way. A window that is
/// simply *there* costs one subtraction and a comparison per frame, and takes
/// none of the paths below.
///
/// Eased out rather than in: the window is already on its way when the user
/// first sees it and slows into its corner, which is what makes something
/// arriving read as arriving rather than as being pushed. See
/// [`ARRIVES_OVER`] and [`DEPTH`].
pub fn arrival(elapsed: std::time::Duration) -> Option<(f32, f64)> {
    let through = elapsed.as_secs_f64() / ARRIVES_OVER.as_secs_f64();
    if through >= 1.0 {
        return None;
    }
    let eased = ease_out_cubic(through);
    Some((eased as f32, DEPTH + (1.0 - DEPTH) * eased))
}

/// And how it is drawn `elapsed` into its departure, which is the same two
/// numbers read the other way round.
///
/// `None` once it has gone, which is what says the picture can be dropped.
///
/// Smoothstepped rather than eased out, which is what the rest of this
/// compositor fades with — see [`crate::curtain`] and [`crate::blackout`].
///
/// The difference is the *last* frame. Something disappearing has to land on
/// nothing at the moment it stops being drawn, and a curve that is still
/// falling when it runs out leaves the window blinking off at whatever it had
/// got down to: eased in, the last frame a sixty-hertz panel draws of this is
/// still a fifth of a window, and a fifth of a window vanishing is the pop this
/// whole animation exists to remove. Smoothstep is flat at both ends, so the
/// frames at either end of it are worth nothing and nothing respectively.
pub fn departure(elapsed: std::time::Duration) -> Option<(f32, f64)> {
    let through = elapsed.as_secs_f64() / LEAVES_OVER.as_secs_f64();
    if through >= 1.0 {
        return None;
    }
    let eased = smoothstep(through);
    Some(((1.0 - eased) as f32, 1.0 - (1.0 - DEPTH) * eased))
}

/// How far along a floating window is `elapsed` into settling into a new place
/// or a new shape — which is a number that goes past 1.0 and comes back, that
/// being the whole of the spring.
///
/// `None` once it has settled, which is every frame of a video standing in a
/// corner nobody has touched. See [`SETTLES_OVER`], [`SPRING_DECAY`] and
/// [`SPRING_WOBBLE`].
pub fn settling(elapsed: std::time::Duration) -> Option<f64> {
    let through = elapsed.as_secs_f64() / SETTLES_OVER.as_secs_f64();
    if through >= 1.0 {
        return None;
    }
    Some(1.0 - (-SPRING_DECAY * through).exp() * (SPRING_WOBBLE * through).cos())
}

/// `from` to `to`, `through` of the way — asked with a `through` past 1.0 and
/// under it, which is what puts the rectangle outside both of them.
///
/// Nothing is ever interpolated to nothing: an overshoot on the way from a
/// large shape to a much smaller one could in principle take an edge through
/// zero, and a rectangle of no width is not a smaller rectangle.
fn between(from: Rect, to: Rect, through: f64) -> Rect {
    let at = |from: f64, to: f64| from + (to - from) * through;
    Rect {
        x: at(from.x, to.x),
        y: at(from.y, to.y),
        w: at(from.w, to.w).max(1.0),
        h: at(from.h, to.h).max(1.0),
    }
}

/// `rect` drawn at `depth` of its own size, about its own middle.
pub fn deepened(rect: Rect, depth: f64) -> Rect {
    Rect {
        x: rect.x + rect.w * (1.0 - depth) / 2.0,
        y: rect.y + rect.h * (1.0 - depth) / 2.0,
        w: rect.w * depth,
        h: rect.h * depth,
    }
}

/// Quick at the start and settling at the end.
fn ease_out_cubic(through: f64) -> f64 {
    let through = through.clamp(0.0, 1.0);
    1.0 - (1.0 - through).powi(3)
}

/// Flat at both ends and quickest in the middle.
fn smoothstep(through: f64) -> f64 {
    let through = through.clamp(0.0, 1.0);
    through * through * (3.0 - 2.0 * through)
}

/// How far the mat is painted *over* the picture it surrounds, in physical
/// pixels.
///
/// A mat lies on the edge of a photograph; it does not stop where the paper
/// starts. Half a pixel is all it takes now that the opening is painted at the
/// same whole pixels the client's own buffer is snapped to — see [`paint`] and
/// [`backing`], which are handed the same rectangle. What it buys is the last
/// thing that can still slip: this image is drawn at a whole logical size, and
/// on a display at a fractional scale that is not exactly the pixels it was
/// painted at, so it is resampled by a fraction of one.
///
/// It is also what makes the frame's inner edge antialias itself, which the
/// outer edge has always done. Half a pixel of a hairline is not a cost worth
/// counting; a hairline with a hard edge on one side and a soft edge on the
/// other is.
const OVERLAP: f64 = 0.5;

/// Where the painted mat lands on the display, in whole physical pixels.
///
/// Whole, because the image is one texel per physical pixel and a texel grid
/// laid down half a pixel out is a soft edge on every straight in the picture.
/// Both the render and [`Pip::mat`] ask this rather than working it out, for
/// the reason everything else about this shape is asked once: three pixels of
/// mat cannot absorb a disagreement.
pub fn origin(frame: &pip::Frame, scale: f64) -> Point<i32, Physical> {
    Point::from((
        (((frame.outer.x - frame.shadow) * scale).round()) as i32,
        (((frame.outer.y - frame.shadow) * scale).round()) as i32,
    ))
}

/// The rectangle to fill with the mat's own colour behind a floating window, in
/// physical pixels, given the opening the layout settled and the display's
/// scale.
///
/// This is for the *other* way a hole can open in the frame: a client that does
/// not cover its opening at all — one still starting up, or one that will not
/// take the size it was given. The mat's [`OVERLAP`] answers for the seam at the
/// edge; this answers for everything inside it.
///
/// Rounded to the nearest pixel, which is what the client's own rectangle is
/// rounded to, and no further: this is a rectangle with four square corners, and
/// pushing one out towards a curve it is meant to be hidden behind is how the
/// rounding stops being round. [`lxb_protocol::pip::BORDER`] leaves r·0.057 of
/// slack over the client's corner — under two physical pixels on a 1080p panel —
/// and the half a pixel this can gain fits under it at every radius this
/// compositor draws.
pub fn backing(opening: Rectangle<f64, Logical>, scale: f64) -> Rectangle<i32, Physical> {
    opening.to_physical_precise_round(scale)
}

/// A row of the floating window's menu that only the compositor can carry out.
///
/// The two rows that move the window to another display are not here: those are
/// `move_window_to_output`, which already means exactly this for every other
/// window, and a second way of saying it would be a second way of getting it
/// wrong. Nor is the row that gives the window the whole display: that one is
/// `set_window_floating`, which is one sentence said in both directions and is
/// the same request the guide's window card menu sends to put an application
/// *into* a corner. Nor is the row that dismisses the menu — that one never
/// leaves the shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuCommand {
    Move,
    Resize,
    Close,
    Realign,
}

impl crate::state::LxbState {
    /// Carry out a row of the floating window's menu.
    ///
    /// Move and Resize both hand the pointer to the compositor: it is put on
    /// the window — in the middle of it, or on the corner about to be pulled —
    /// and the window follows it until the next click. That is a drag like any
    /// other from there on, settled by the same arithmetic; what differs is
    /// only what ends it, which is [`Until`].
    ///
    /// Close *asks* the window to close, and is deliberately not the shell's
    /// own Close: that one ends the application behind the window, and the
    /// application behind this one is a browser with the rest of somebody's
    /// session in it. Asked, a browser answers by putting the video back in the
    /// page it came from, which is exactly what the row means.
    pub(crate) fn pip_menu_command(&mut self, id: u32, command: MenuCommand) {
        let Some(window) = self.window_by_overview_id(id) else {
            tracing::debug!(
                id,
                ?command,
                "a floating window command for a window that is gone"
            );
            return;
        };
        // A video the user put back while the menu was up is an ordinary window
        // again, and none of these rows means anything to one.
        if !self.lxb.floating(&window) {
            tracing::debug!(id, ?command, "a command for a window that stopped floating");
            return;
        }
        if command == MenuCommand::Close {
            tracing::info!(id, "asking a floating window to close");
            self.request_window_close(&window);
            return;
        }
        // Back into the column, at the size and in the corner the Settings page
        // asks for. The whole of "detached" is the rectangle the user put it in
        // — see [`Floating::placed`] — so giving that up is the whole of coming
        // back, and the spring the layout starts is what carries it there. The
        // rest of the column is laid out with it, because the place this window
        // is going back to is one the windows still standing in it are holding
        // open.
        if command == MenuCommand::Realign {
            tracing::info!(id, "a floating window was put back in the column");
            floating_state(&window).reattach();
            self.relayout_floating_windows();
            self.queue_redraw();
            return;
        }

        let Some(frame) = floating_state(&window).frame() else {
            return;
        };
        let Some(output) = self.lxb.outputs.window_display(&self.lxb.space, &window) else {
            return;
        };
        let Some(display) = self.lxb.space.output_geometry(&output) else {
            return;
        };
        let (handle, at) = match command {
            MenuCommand::Resize => {
                let (x, y) = resize_corner(frame.outer, display.size);
                (
                    Handle::Edge {
                        x: Some(x),
                        y: Some(y),
                    },
                    corner_point(frame.outer, x, y),
                )
            }
            // Move. Close returned above.
            _ => (Handle::Move, middle(frame.outer)),
        };
        tracing::info!(id, ?handle, "a floating window was given to the pointer");
        self.take_a_floating_window(&window, &output, handle, display.loc.to_f64() + at);
    }

    /// Take the user's word for whether one window floats.
    ///
    /// Two menu rows, one request, because they are one sentence read in either
    /// direction: a video in a corner told to fill the display it is in the
    /// corner of, and an application told to go and sit in that corner instead.
    /// Everything that being floating *means* — the corner, the mat, the column,
    /// being left out of the deck, off the keyboard and on top of everything —
    /// is settled in one place ([`crate::state::Lxb::floating`]) and asks one
    /// question ([`can_float`]), so all either direction has to do here is
    /// answer that question differently and let the pass that notices do the
    /// rest.
    ///
    /// **Refused while the feature is switched off.** A window given a corner on
    /// a session that has no floating windows is a window with no menu to be got
    /// out of the corner with: the only door back is the row on the floating
    /// window's own menu, and that menu is raised by the compositor on a window
    /// the compositor is not floating.
    ///
    /// The flight out of the corner is not started here. See
    /// [`crate::state::LxbState::fly_out_of_the_corner`].
    pub(crate) fn set_window_floating(&mut self, id: u32, floating: bool) {
        if !self.lxb.outputs.pip().settings().floating {
            tracing::debug!(
                id,
                floating,
                "a window was told to float with the feature off"
            );
            return;
        }
        let Some(window) = self.window_by_overview_id(id) else {
            tracing::debug!(id, floating, "which a window that is gone floats");
            return;
        };
        // The one kind of window that can never be given a corner, whoever
        // asks: it is placed where its client put it and is never re-tiled, so
        // it would be drawn with a surround sized for a rectangle it is not in.
        // Answered here as well as in [`can_float`], so that the wish is not
        // written down where it could never be acted on.
        if crate::input::window_is_x11_chrome(&window)
            || window
                .x11_surface()
                .is_some_and(|surface| surface.is_override_redirect())
        {
            tracing::debug!(id, "chrome cannot be given a corner");
            return;
        }
        let was = self.lxb.floating(&window);
        let state = floating_state(&window);
        state.wish(floating);
        if was == floating {
            // Already what it was asked to be. The wish is still written down —
            // the user has said it, and a browser retitling the window later is
            // not them changing their mind — but nothing moves.
            return;
        }
        match floating {
            // Out of the corner. The shape it is standing in is remembered
            // before the layout takes it away, because nothing else will have
            // it once this window has stopped being a floating one: the frame
            // is the *floating* state, and that is forgotten on the same pass.
            false => {
                let corner = state.frame().map(|frame| frame.inner);
                let drawn = self
                    .lxb
                    .space
                    .element_geometry(&window)
                    .map(|geometry| geometry.size)
                    .unwrap_or_default();
                state.leaving(corner.map(|corner| (corner, drawn)));
                // And the picture kept of it in that corner is thrown away
                // *before* the pass that would set it fading: a window that
                // stops floating is ordinarily one a browser has destroyed, and
                // what is drawn for it is the last frame taken of it falling
                // out of the corner. This one has not gone anywhere. Left in,
                // the fade would be a second copy of the video shrinking in the
                // corner while the window itself grew out of it.
                self.lxb.outputs.pip_mut().forget_a_window(id);
            }
            // And into it: a window on its way out of one has nothing left to
            // fly out of, and a flight begun after it has gone back would grow
            // the corner out of the corner.
            true => state.leaving(None),
        }
        tracing::info!(
            id,
            floating,
            title = crate::shell_control::window_title(&window),
            "the user said what this window is"
        );
        // Which is all of it: the pass that notices a window has changed what it
        // is re-tiles it, lays the column out around it, hands the keyboard to
        // whatever should have it now, and asks for a frame.
        self.refresh_floating_windows();
    }

    /// Start a window that has just left its corner growing out to fill the
    /// display, once it is actually large enough to be growing from anything.
    ///
    /// Asked at the commit, and that is the whole reason this is not simply done
    /// where the window was told to stop floating: it is *told* there, and it
    /// becomes large some frames later, when its client has taken the size and
    /// drawn it. The flight is drawn between where the window began and where it
    /// really is — see [`crate::render::push_restoring_windows`] — so one begun
    /// while the window was still the size of a corner would be a flight from a
    /// rectangle to itself, standing still for as long as the client took and
    /// then jumping into the middle of the journey.
    ///
    /// The first size it draws that is not the size it left at is the frame it
    /// has become large on, which is the same reading [`Floating::read_answer`]
    /// takes of a client's shape and for the same reason: what a client draws is
    /// the only thing it ever says. A client that never takes the size simply
    /// never flies; it appears where it was put, which is what it did before any
    /// of this existed.
    ///
    /// The same flight the guide's tiles use, deliberately: what the user asked
    /// for is this window opened full screen, and that is what opening one full
    /// screen looks like on this session.
    pub(crate) fn fly_out_of_the_corner(&mut self, window: &Window) {
        if self.lxb.floating(window) {
            return;
        }
        // Before anything is measured: this is asked at every commit of every
        // window on the session, and all but the handful of frames after a
        // press on that row answer it with a cell read.
        if !floating_state(window).is_leaving() {
            return;
        }
        let drawn = self
            .lxb
            .space
            .element_geometry(window)
            .map(|geometry| geometry.size)
            .unwrap_or_default();
        let Some(corner) = floating_state(window).take_the_corner_it_left(drawn) else {
            return;
        };
        // The display it left the corner of, which is the display it is on: a
        // window here is pinned to the screen it opened on, and this one has not
        // moved.
        let Some(output) = self.lxb.outputs.window_display(&self.lxb.space, window) else {
            return;
        };
        let id = crate::overview::window_id(window);
        tracing::debug!(id, ?corner, "a window grew out of its corner");
        self.lxb
            .restores
            .begin(id, &output, corner, std::time::Instant::now());
        self.queue_redraw();
    }

    /// Mark the floating window the shell has handed its own directions to, or
    /// mark none.
    ///
    /// The shell keeps this: it is the shell that reads the controller, that
    /// knows its overlay is up, and that knows which of several windows the user
    /// walked to. All the compositor does is draw the answer — which only the
    /// compositor can, because a floating window is drawn in front of every
    /// surface the shell owns and a mark drawn by the shell would be behind the
    /// thing it marks.
    ///
    /// A selection on a window that is gone, or one that has stopped floating,
    /// selects nothing rather than being refused: the shell is told about such a
    /// window going away in the same breath, and a stale mark on a corner of the
    /// screen would outlive both.
    pub(crate) fn select_floating_window(&mut self, id: u32, accent: u32) {
        let selected = self
            .window_by_overview_id(id)
            .filter(|window| self.lxb.floating(window))
            .map(|_| (id, accent));
        if selected.is_none() && id != 0 {
            tracing::debug!(id, "a floating window was selected that is not there");
        }
        // A hand the pad still has on some *other* window is let go of first,
        // and left where it was put: the user has walked away from that window,
        // which is not the same as taking back what they did to it.
        if let Some(drag) = self.lxb.pip_drag.clone() {
            let held = crate::overview::window_id(&drag.window);
            if drag.until == Until::Told && Some(held) != selected.map(|(id, _)| id) {
                self.let_go_of_a_floating_window(&drag, true);
            }
        }
        if !self.lxb.outputs.pip_mut().select(selected) {
            return;
        }
        tracing::debug!(?selected, "the shell marked a floating window");
        self.queue_redraw();
    }

    /// Take the shell's word for which of its surfaces is a context menu, so
    /// that one surface can be drawn in front of the windows every other one is
    /// drawn behind.
    ///
    /// The one exception to "a floating window is over everything", and it
    /// exists because the alternative is a dead end: a window made large enough
    /// covers the very menu that offers to make it small again, and a console
    /// has no pointer to find an unseen row with.
    ///
    /// **A surface, not a rectangle.** A panel is rounded and a rectangle is
    /// not, so lifting a panel's bounding box puts four square corners of the
    /// shell over the video — which is exactly what it looked like the first
    /// time this was built. A surface of its own is exactly its own shape.
    pub(crate) fn set_menu_surface(&mut self, surface: Option<WlSurface>) {
        if !self.lxb.outputs.pip_mut().name_a_menu(surface) {
            return;
        }
        tracing::debug!("the shell named the surface it draws its menus on");
        self.queue_redraw();
    }

    /// Take hold of a floating window on a controller's behalf.
    ///
    /// The same drag a hand makes, with the pointer taken out of it: nothing is
    /// warped, nothing is pressed, and whatever the pointer happens to be over
    /// goes on hearing about it. What the two share is everything that decides
    /// where the window ends up — see [`carried`] and [`resized`] — so a window
    /// moved with a stick and one moved with a mouse obey the same rules about
    /// edges, sizes and the screen they have to stay on.
    ///
    /// One hand at a time. A grab arriving while anything already has hold of a
    /// window is dropped: the alternative is two controls pulling one window in
    /// two directions, which is not a state this session can be in.
    pub(crate) fn grab_floating_window(&mut self, id: u32, resize: bool) {
        if self.lxb.pip_drag.is_some() {
            tracing::debug!(id, "a floating window is already being held");
            return;
        }
        let Some(window) = self.window_by_overview_id(id) else {
            tracing::debug!(id, "a grab on a floating window that is gone");
            return;
        };
        if !self.lxb.floating(&window) {
            tracing::debug!(id, "a grab on a window that is not floating");
            return;
        }
        let Some(frame) = floating_state(&window).frame() else {
            return;
        };
        let Some(output) = self.lxb.outputs.window_display(&self.lxb.space, &window) else {
            return;
        };
        let Some(display) = self.lxb.space.output_geometry(&output) else {
            return;
        };
        // Which corner a resize grows from is the compositor's answer rather
        // than the shell's, for the reason the pointer's is: it is a fact about
        // where the window is standing on a display the compositor laid out.
        let handle = match resize {
            true => {
                let (x, y) = resize_corner(frame.outer, display.size);
                Handle::Edge {
                    x: Some(x),
                    y: Some(y),
                }
            }
            false => Handle::Move,
        };
        tracing::info!(id, ?handle, "a floating window was given to a controller");
        self.lxb.pip_drag = Some(Drag {
            window: window.clone(),
            handle,
            output,
            until: Until::Told,
            was: floating_state(&window).placed(),
            from: Point::default(),
            origin: frame.outer,
            carrying: true,
        });
        self.raise_window(&window, false);
        self.queue_redraw();
    }

    /// Move the window a controller has hold of.
    ///
    /// **Measured from where the window is now**, which is the one thing this
    /// does differently from the pointer's drag. A pointer is itself clamped to
    /// the display, so a hand pushed into the edge of the screen has nowhere
    /// further to go and the drag it is measured from cannot run past the
    /// window; a stick has no such limit, and a delta accumulated against a
    /// fixed origin would build up a debt the user then has to push back out of
    /// before the window moved at all.
    pub(crate) fn drag_floating_window(&mut self, delta: Point<f64, Logical>) {
        let Some(drag) = self.lxb.pip_drag.clone() else {
            return;
        };
        if drag.until != Until::Told {
            // A pointer has hold of it. Not a case a shell can reach honestly,
            // and the answer to it is to leave the hand that is actually on the
            // window alone.
            return;
        }
        // A window that has died under the hand, or that the user has just put
        // back into its page, is not being held any more.
        if !drag.window.alive() || !self.lxb.floating(&drag.window) {
            self.lxb.pip_drag = None;
            return;
        }
        let Some(display) = self.lxb.space.output_geometry(&drag.output) else {
            return;
        };
        let state = floating_state(&drag.window);
        let aspect = state.aspect();
        // Where it is now: its own rectangle once it has been moved by hand, and
        // the one the column gave it until then.
        let Some(from) = state.placed().or(Some(drag.origin)) else {
            return;
        };
        let rect = match drag.handle {
            Handle::Move => carried(from, delta, display.size, aspect),
            Handle::Edge { x, y } => resized(from, x, y, delta, display.size, aspect),
        };
        if state.placed() == Some(rect) {
            return;
        }
        // The first movement is what takes it out of the column, exactly as the
        // pointer's first movement is, and the column has to close up behind it.
        let leaving = state.placed().is_none();
        state.hold(true);
        state.place_at(rect);
        match leaving {
            true => self.relayout_floating_windows(),
            false => self.lxb.outputs.tile_window_on_output(
                &mut self.lxb.space,
                &drag.window,
                &drag.output,
            ),
        }
        self.queue_redraw();
    }

    /// Let go of the window a controller had hold of.
    pub(crate) fn drop_floating_window(&mut self, keep: bool) {
        let Some(drag) = self.lxb.pip_drag.clone() else {
            return;
        };
        if drag.until != Until::Told {
            return;
        }
        self.let_go_of_a_floating_window(&drag, keep);
    }

    /// End a drag nothing is holding down: `keep` to leave the window where the
    /// drag left it, and otherwise to put it back.
    ///
    /// Back to the *state* it was in and not merely to the rectangle it had: a
    /// window that was standing in the column belongs back in the column, where
    /// it will move again the next time the settings do.
    ///
    /// One answer for the menu's drags and the controller's, because letting go
    /// is the same act whichever of them started it. What differs is only what
    /// the caller has to do about the pointer afterwards.
    pub(crate) fn let_go_of_a_floating_window(&mut self, drag: &Drag, keep: bool) {
        self.lxb.pip_drag = None;
        let state = floating_state(&drag.window);
        // The hand is off it, whichever way this drag ended. A window put back
        // where it started springs back rather than jumping there — see
        // [`Floating::held`].
        state.hold(false);
        if !keep {
            match drag.was {
                Some(rect) => state.place_at(rect),
                None => state.reattach(),
            }
            self.relayout_floating_windows();
        }
        tracing::debug!(
            handle = ?drag.handle,
            kept = keep,
            at = ?state.placed(),
            "a floating window was let go of"
        );
        self.queue_redraw();
    }

    /// Notice a window that has started floating, or stopped.
    ///
    /// Asked once a pass from the event loop, beside
    /// [`crate::state::LxbState::refresh_foreground`] and for exactly the same
    /// reason: this is a fact about a window's *title*, and a title changes
    /// without mapping, unmapping or focusing anything. A browser maps the
    /// picture-in-picture window first and names it a moment afterwards, so a
    /// decision taken when it mapped would be taken before there was anything
    /// to decide from — and puts the video back by renaming the same window,
    /// which has to put it back into the ordinary layout.
    ///
    /// Cheap when nothing changed: on a session with the feature switched off
    /// it is one empty set compared against another, and on one with it on it
    /// is a title read per window.
    pub(crate) fn refresh_floating_windows(&mut self) {
        let now: std::collections::HashSet<u32> = self
            .lxb
            .space
            .elements()
            .filter(|window| self.lxb.floating(window))
            .map(crate::overview::window_id)
            .collect();
        // Before anything else, and whether or not the set has changed: the
        // window that is *drawn* over everything has to *be* over everything in
        // the stack as well. Nothing else keeps it there — an application
        // launched afterwards is mapped on top, as every new window is — and a
        // pointer finds windows in stack order, so a video drawn over a game
        // whose window is above it in the stack is one whose own play button
        // cannot be pressed.
        if !now.is_empty() {
            self.keep_the_floating_windows_on_top(&now);
        }

        // A mark on a window that has stopped floating — a video the user put
        // back into its page while the guide was open on it — is a mark on a
        // corner of the screen with nothing under it. Dropped here rather than
        // left to the shell to notice: the shell is told in the same breath, but
        // the frame in between is drawn from this.
        if let Some((selected, _)) = self.lxb.outputs.pip().selected() {
            if !now.contains(&selected) {
                self.lxb.outputs.pip_mut().select(None);
            }
        }

        // Nothing is being held except by a drag that is actually in flight.
        // Cleared here as well as where a drag ends, because a window left
        // marked as held is a window that never springs again — and the two
        // ways a drag can end without going through the door that clears it
        // are the window dying and the window ceasing to float, neither of
        // which is a thing that door is asked about.
        let held = self.lxb.pip_drag.as_ref().map(|drag| drag.window.clone());
        for window in self.lxb.space.elements() {
            if self.lxb.floating(window) && Some(window) != held.as_ref() {
                floating_state(window).hold(false);
            }
        }

        // Whatever else is happening: the windows on their way out are drawn
        // for as long as they are leaving, and the ones springing into a new
        // place in the column for as long as they are springing. Both ask for
        // the frames they are drawn on, because a video moving in a corner is
        // very often the only thing changing on the session and nothing else
        // would ask.
        let at = std::time::Instant::now();
        let going = self.lxb.outputs.pip().anything_going();
        if going {
            self.lxb.outputs.pip_mut().forget_the_gone(at);
        }
        let settling = self
            .lxb
            .space
            .elements()
            .any(|window| self.lxb.floating(window) && floating_state(window).is_settling(at));
        if going || settling {
            self.queue_redraw();
        }

        if &now == self.lxb.outputs.pip().were_floating() {
            return;
        }

        // Both directions: a window that has just started floating has to be
        // put in its corner, and one that has stopped has to go back to filling
        // its display. `tile_window` asks the same question again and answers
        // whichever of the two this is.
        let changed: Vec<Window> = self
            .lxb
            .space
            .elements()
            .filter(|window| {
                let id = crate::overview::window_id(window);
                now.contains(&id) != self.lxb.outputs.pip().were_floating().contains(&id)
            })
            .cloned()
            .collect();
        // Every window that has stopped floating starts fading out of its
        // corner, drawn from the last picture taken of it — which is the only
        // thing left of it, because a browser closing a picture-in-picture
        // window *destroys* it. And every window that has just started floating
        // forgets whatever was kept of it, so that a video put straight back
        // into a corner replaces the copy still fading rather than arriving
        // behind it. See [`LastPicture`].
        let at = std::time::Instant::now();
        let were = self.lxb.outputs.pip().were_floating();
        let gone: Vec<u32> = were.difference(&now).copied().collect();
        let arrived: Vec<u32> = now.difference(were).copied().collect();
        for id in gone {
            if self.lxb.outputs.pip_mut().let_go(id, at) {
                tracing::debug!(window = id, "a floating window started leaving its corner");
            }
        }
        for id in arrived {
            self.lxb.outputs.pip_mut().forget_a_window(id);
        }
        self.lxb.outputs.pip_mut().now_floating(now);

        // A window that has stopped floating forgets what it said about itself,
        // so that a video put back into a corner is asked its shape again
        // rather than answering with the shape of the last one.
        for window in &changed {
            if !self.lxb.floating(window) {
                floating_state(window).forget();
            }
        }

        for window in &changed {
            tracing::info!(
                title = crate::shell_control::window_title(window),
                floating = self.lxb.floating(window),
                "a window changed what it is"
            );
            self.lxb.outputs.tile_window(&mut self.lxb.space, window);
        }
        if changed.is_empty() {
            return;
        }
        // And then every floating window again, because one arriving or leaving
        // moves the column the rest of them stand in.
        self.relayout_floating_windows();
        // A window that has just started floating must not still be holding the
        // keyboard: it is a corner of the screen now, and whatever is behind it
        // is what the user is actually driving.
        self.focus_topmost_window();
        self.queue_redraw();
    }

    /// Lay a floating window out again when its client has said — or changed its
    /// mind about — what shape it wants to be.
    ///
    /// Asked from the commit, which is the moment the answer arrives and the
    /// only moment it can be read: the answer *is* the size the client has just
    /// drawn at. Everything else about the conversation is in
    /// [`Floating::read_answer`]; this is only where it is listened for.
    ///
    /// Costs a size comparison per commit of a floating window, and nothing at
    /// all for every other window on the session: a client drawing the shape it
    /// is already being drawn at — which is every frame of a video once the
    /// shape is settled — turns back at the comparison and lays out nothing.
    pub(crate) fn settle_a_floating_window(&mut self, window: &Window) {
        if !self.lxb.floating(window) {
            return;
        }
        let drawn = self
            .lxb
            .space
            .element_geometry(window)
            .map(|geometry| geometry.size)
            .unwrap_or_default();
        if !floating_state(window).read_answer(drawn) {
            return;
        }
        // Every floating window, not only this one: they stand in a column, and
        // this one having just become a different shape is what says where the
        // ones below it start. See [`lxb_protocol::pip::frames`].
        self.relayout_floating_windows();
    }

    /// Lay out every floating window again, because they are laid out as one
    /// column and any of them changing moves the rest.
    pub(crate) fn relayout_floating_windows(&mut self) {
        let floating: Vec<Window> = self
            .lxb
            .space
            .elements()
            .filter(|window| self.lxb.floating(window))
            .cloned()
            .collect();
        for window in &floating {
            self.lxb.outputs.tile_window(&mut self.lxb.space, window);
        }
    }

    /// Raise every floating window to the top of the stack, if it is not
    /// already there.
    ///
    /// Checked before it is done, because it is done on every pass of the event
    /// loop and re-ordering the stack for nothing would be re-ordering it sixty
    /// times a second. The check is the cheap half: whether the windows at the
    /// top of the stack are exactly the floating ones.
    ///
    /// Never activated. Raising a window normally hands it the keyboard, which
    /// is the one thing this window must not be given — see
    /// [`crate::state::Lxb::takes_the_keyboard`].
    fn keep_the_floating_windows_on_top(&mut self, floating: &std::collections::HashSet<u32>) {
        let already = self
            .lxb
            .space
            .elements()
            .rev()
            .take_while(|window| floating.contains(&crate::overview::window_id(window)))
            .count();
        if already == floating.len() {
            return;
        }
        let windows: Vec<Window> = self
            .lxb
            .space
            .elements()
            .filter(|window| floating.contains(&crate::overview::window_id(window)))
            .cloned()
            .collect();
        for window in windows {
            self.lxb.space.raise_element(&window, false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A texture that is not one, for the bookkeeping tests: what is kept of a
    /// window that has gone is a handle and some numbers, and none of these
    /// tests is about the handle.
    #[derive(Debug, Clone)]
    struct NoTexture;

    impl smithay::backend::renderer::Texture for NoTexture {
        fn width(&self) -> u32 {
            0
        }
        fn height(&self) -> u32 {
            0
        }
        fn format(&self) -> Option<Fourcc> {
            None
        }
    }

    fn a_picture_of(output: &str) -> LastPicture {
        LastPicture::new(
            output.to_string(),
            Placed {
                frame: frame(),
                corner: Point::from((0.0, 0.0)),
                origin: Point::from((0.0, 0.0)),
                factor: 1.0,
            },
            Id::new(),
            KeptPicture::<NoTexture>::new(ContextId::new()),
        )
    }

    /// Both ends of the arrival, and the fact that it has two ends at all.
    ///
    /// A window starts invisible and small and finishes whole and its own size,
    /// and stops being animated at all once it has — which is what keeps every
    /// other frame of a video free.
    #[test]
    fn a_floating_window_arrives_out_of_nothing_and_settles_at_its_own_size() {
        let (alpha, depth) = arrival(std::time::Duration::ZERO).expect("it starts");
        assert!(alpha < 0.01, "it starts invisible: {alpha}");
        assert!(
            (depth - DEPTH).abs() < 0.001,
            "it starts behind the screen: {depth}"
        );

        let (alpha, depth) = arrival(ARRIVES_OVER - std::time::Duration::from_millis(1))
            .expect("it is still arriving a millisecond before it lands");
        assert!(alpha > 0.99, "it ends whole: {alpha}");
        assert!(depth > 0.99, "it ends its own size: {depth}");

        assert_eq!(
            arrival(ARRIVES_OVER),
            None,
            "a window that has arrived is not animated"
        );
        assert_eq!(arrival(ARRIVES_OVER * 100), None);
    }

    /// And the departure, which is the same two numbers read the other way.
    #[test]
    fn a_floating_window_leaves_from_its_own_size_and_fades_out_of_the_corner() {
        let (alpha, depth) = departure(std::time::Duration::ZERO).expect("it starts");
        assert!(alpha > 0.99, "it starts whole: {alpha}");
        assert!(depth > 0.99, "it starts its own size: {depth}");

        let (alpha, depth) = departure(LEAVES_OVER - std::time::Duration::from_millis(1))
            .expect("it is still leaving a millisecond before it goes");
        assert!(alpha < 0.01, "it ends invisible: {alpha}");
        assert!(
            (depth - DEPTH).abs() < 0.001,
            "it ends behind the screen: {depth}"
        );

        assert_eq!(departure(LEAVES_OVER), None, "and then it is gone");
    }

    /// Never linear, in either direction.
    ///
    /// The one rule this whole session's motion is held to. Both curves move
    /// the whole way without a step backwards and without a stall, and neither
    /// of them is a straight ramp: asked a quarter and three quarters of the
    /// way through, a straight ramp would answer a quarter and three quarters.
    #[test]
    fn neither_animation_is_a_straight_ramp() {
        for (name, over, curve) in [
            (
                "arrival",
                ARRIVES_OVER,
                arrival as fn(std::time::Duration) -> Option<(f32, f64)>,
            ),
            ("departure", LEAVES_OVER, departure),
        ] {
            // How far *through* the animation is, whichever way round it runs:
            // the arrival's alpha rises and the departure's falls, and both of
            // them are one number going from nothing to everything.
            let through = |at: f64| {
                let (alpha, _) = curve(over.mul_f64(at)).expect(name);
                match name {
                    "arrival" => alpha as f64,
                    _ => 1.0 - alpha as f64,
                }
            };

            let steps = 64;
            let mut last = curve(std::time::Duration::ZERO).expect(name);
            let mut moved = 0;
            for step in 1..steps {
                let now = curve(over.mul_f64(step as f64 / steps as f64)).expect(name);
                let forward = match name {
                    "arrival" => now.0 >= last.0 && now.1 >= last.1,
                    _ => now.0 <= last.0 && now.1 <= last.1,
                };
                assert!(
                    forward,
                    "{name} turned back at step {step}: {last:?} {now:?}"
                );
                if now != last {
                    moved += 1;
                }
                last = now;
            }
            assert!(moved > steps / 2, "{name} stalls");

            for at in [0.25, 0.75] {
                let reached = through(at);
                assert!(
                    (reached - at).abs() > 0.05,
                    "{name} is a straight ramp: {reached} of the way through at {at}"
                );
            }
        }
    }

    /// A window that has gone is still drawn, on its own display, until it has
    /// finished going — and then it is not drawn at all.
    #[test]
    fn a_window_that_has_gone_is_drawn_until_its_fade_is_over() {
        let mut pip = Pip::default();
        let at = std::time::Instant::now();
        pip.keep(7, a_picture_of("one"));

        assert!(pip.let_go(7, at), "there was a picture of it to draw");
        assert!(pip.anything_going());
        assert_eq!(pip.going_on("one", at).len(), 1);
        assert_eq!(
            pip.going_on("two", at).len(),
            0,
            "it is leaving the display it was on and no other"
        );

        assert_eq!(pip.going_on("one", at + LEAVES_OVER).len(), 0);
        assert!(
            pip.forget_the_gone(at + LEAVES_OVER),
            "and is dropped once it has gone"
        );
        assert!(!pip.anything_going());
    }

    /// A window nothing ever drew has nothing to fade.
    ///
    /// A client is free to name a window picture-in-picture and rename it in
    /// the same breath, before the layout has ever placed it. There is no
    /// picture of such a window and no shape it was ever in, so there is
    /// nothing for a fade to be a fade of.
    #[test]
    fn a_window_that_was_never_drawn_does_not_fade() {
        let mut pip = Pip::default();
        assert!(!pip.let_go(3, std::time::Instant::now()));
        assert!(!pip.anything_going());
    }

    /// A video put straight back into the corner it just left replaces the copy
    /// still fading there rather than arriving behind it.
    #[test]
    fn a_window_that_comes_straight_back_does_not_arrive_behind_itself() {
        let mut pip = Pip::default();
        let at = std::time::Instant::now();
        pip.keep(7, a_picture_of("one"));
        assert!(pip.let_go(7, at));

        pip.forget_a_window(7);
        assert!(!pip.anything_going());
        assert_eq!(pip.going_on("one", at).len(), 0);
    }

    /// And a window that leaves twice is one window leaving, not two.
    #[test]
    fn a_window_cannot_be_leaving_twice_over() {
        let mut pip = Pip::default();
        let at = std::time::Instant::now();
        pip.keep(7, a_picture_of("one"));
        assert!(pip.let_go(7, at));
        pip.keep(7, a_picture_of("one"));
        assert!(pip.let_go(7, at + std::time::Duration::from_millis(10)));
        assert_eq!(pip.going_on("one", at + LEAVES_OVER / 2).len(), 1);
    }

    fn a_frame_at(x: f64, y: f64, w: f64, h: f64) -> pip::Frame {
        pip::Frame {
            outer: Rect { x, y, w, h },
            inner: Rect {
                x: x + 3.0,
                y: y + 3.0,
                w: w - 6.0,
                h: h - 6.0,
            },
            radius: 12.0,
            border: 3.0,
            shadow: 24.0,
        }
    }

    /// A window that has been drawn and has finished arriving, which is what
    /// the spring is only ever started on.
    fn a_settled_window(at: std::time::Instant) -> Floating {
        let state = Floating::default();
        state.arriving(at);
        state.placed_in(a_frame_at(100.0, 100.0, 400.0, 225.0), at + ARRIVES_OVER);
        state
    }

    /// The spring: out of nothing, past its destination, back, and *exactly*
    /// onto it at the end.
    ///
    /// The last of those is the one that matters. A curve that is still a
    /// fraction of a percent short when it runs out puts the window a couple of
    /// pixels from where it belongs on the last frame of the animation, and a
    /// couple of pixels appearing from nowhere is the jump the spring is here
    /// to remove.
    /// What the *user* said about a window outlives the window changing what it
    /// is — which is the whole of what the two rows are for, and the one way
    /// this could quietly undo itself.
    #[test]
    fn a_window_the_user_has_spoken_for_does_not_forget_it() {
        let state = a_settled_window(std::time::Instant::now());
        assert_eq!(state.wished(), None, "the title is the whole answer");

        // A video told to fill the display. `forget` runs on the very pass that
        // notices it has stopped floating, and a wish cleared there would be
        // put straight back by the next one — once per pass of the event loop,
        // forever.
        state.wish(false);
        state.forget();
        assert_eq!(state.wished(), Some(false));

        // And the other direction: an application sent to a corner keeps being
        // one, whatever it calls itself.
        state.wish(true);
        state.forget();
        assert_eq!(state.wished(), Some(true));
    }

    /// A window grows out of the corner it left on the first frame it draws at
    /// a size it was not drawing at in the corner — not on the frame it was
    /// told to, which is some frames earlier and is a flight from a rectangle
    /// to itself.
    #[test]
    fn a_window_flies_out_of_its_corner_once_it_is_big_enough_to_fly() {
        let corner = Rect {
            x: 100.0,
            y: 100.0,
            w: 400.0,
            h: 225.0,
        };
        let was = Size::<i32, Logical>::from((400, 225));
        let state = a_settled_window(std::time::Instant::now());
        state.leaving(Some((corner, was)));

        // The client goes on drawing what it was drawing for as long as it
        // takes to take the new size, and every one of those frames is not the
        // answer.
        assert_eq!(state.take_the_corner_it_left(was), None);
        assert_eq!(state.take_the_corner_it_left(was), None);

        // The first size that is not that one is.
        assert_eq!(
            state.take_the_corner_it_left((2560, 1440).into()),
            Some(corner)
        );
        // And it is spent: a window flies out of a corner once.
        assert_eq!(state.take_the_corner_it_left((2560, 1440).into()), None);

        // A window sent back to a corner before it ever finished leaving the
        // last one has nothing left to fly out of.
        state.leaving(Some((corner, was)));
        state.leaving(None);
        assert_eq!(state.take_the_corner_it_left((2560, 1440).into()), None);
    }

    #[test]
    fn the_spring_goes_past_where_it_is_going_and_lands_exactly_on_it() {
        assert_eq!(settling(std::time::Duration::ZERO), Some(0.0));

        let mut furthest: f64 = 0.0;
        let mut crossings = 0;
        let mut last: f64 = 0.0;
        let steps = 480;
        for step in 0..steps {
            let through = settling(SETTLES_OVER.mul_f64(step as f64 / steps as f64))
                .expect("it is still settling");
            furthest = furthest.max(through);
            if (last - 1.0).signum() != (through - 1.0).signum() {
                crossings += 1;
            }
            last = through;
        }
        assert!(
            (1.05..1.20).contains(&furthest),
            "it goes about a tenth past: {furthest}"
        );
        assert!(
            crossings >= 2,
            "it crosses its destination and comes back: {crossings}"
        );

        let landing = settling(SETTLES_OVER - std::time::Duration::from_millis(1))
            .expect("a millisecond before it is done");
        assert!(
            (landing - 1.0).abs() < 0.002,
            "it lands on its destination: {landing}"
        );
        assert_eq!(settling(SETTLES_OVER), None, "and then it is settled");
    }

    /// A window the layout moves springs from where it was to where it is going,
    /// and is drawn nowhere else once it has arrived there.
    #[test]
    fn a_window_the_layout_moves_springs_to_its_new_place() {
        let at = std::time::Instant::now();
        let state = a_settled_window(at);
        assert_eq!(
            state.standing_in(at + ARRIVES_OVER),
            None,
            "it starts still"
        );

        let moved = at + ARRIVES_OVER;
        state.placed_in(a_frame_at(100.0, 400.0, 400.0, 225.0), moved);
        let start = state.standing_in(moved).expect("it is on its way");
        assert!(
            (start.y - 100.0).abs() < 1.0,
            "from where it was: {start:?}"
        );

        let middle = state
            .standing_in(moved + SETTLES_OVER / 3)
            .expect("still on its way");
        assert!(
            middle.y > 100.0 && middle.y != start.y,
            "and on its way: {middle:?}"
        );

        assert_eq!(
            state.standing_in(moved + SETTLES_OVER),
            None,
            "and then it is simply where the layout put it"
        );
    }

    /// The same when it is the *shape* that changed, which is the Settings page
    /// moving every window in the column at once.
    #[test]
    fn a_window_the_settings_resize_springs_past_its_new_shape() {
        let at = std::time::Instant::now();
        let state = a_settled_window(at);
        let moved = at + ARRIVES_OVER;
        state.placed_in(a_frame_at(100.0, 100.0, 600.0, 338.0), moved);

        let widest = (0..96)
            .filter_map(|step| state.standing_in(moved + SETTLES_OVER.mul_f64(step as f64 / 96.0)))
            .map(|rect| rect.w)
            .fold(f64::MIN, f64::max);
        assert!(
            widest > 600.0,
            "it goes past the shape it is growing into: {widest}"
        );
        assert!(widest < 640.0, "and not far past it: {widest}");
    }

    /// Three windows never spring, and each for its own reason. See
    /// [`Floating::placed_in`].
    #[test]
    fn a_window_under_the_hand_a_window_arriving_and_a_new_one_never_spring() {
        let at = std::time::Instant::now();

        // One being laid out for the first time: there is nowhere to spring
        // from.
        let fresh = Floating::default();
        fresh.arriving(at);
        fresh.placed_in(a_frame_at(100.0, 100.0, 400.0, 225.0), at + ARRIVES_OVER);
        assert_eq!(fresh.standing_in(at + ARRIVES_OVER), None);

        // One still arriving: it is already animating, and a client answering
        // what shape it wants to be must not start a second animation over the
        // first.
        let arriving = Floating::default();
        arriving.arriving(at);
        arriving.placed_in(a_frame_at(100.0, 100.0, 400.0, 225.0), at);
        arriving.placed_in(
            a_frame_at(100.0, 400.0, 400.0, 225.0),
            at + ARRIVES_OVER / 2,
        );
        assert_eq!(arriving.standing_in(at + ARRIVES_OVER / 2), None);

        // And one the user is holding, which is where their hand is.
        let held = a_settled_window(at);
        held.hold(true);
        held.place_at(Rect {
            x: 300.0,
            y: 300.0,
            w: 400.0,
            h: 225.0,
        });
        held.placed_in(a_frame_at(320.0, 300.0, 400.0, 225.0), at + ARRIVES_OVER);
        assert_eq!(
            held.standing_in(at + ARRIVES_OVER),
            None,
            "a window under the hand is under the hand"
        );

        // But only while the hand is on it. A drag cancelled puts the window
        // back where it started, and it goes back the way everything else
        // moves rather than jumping there.
        held.hold(false);
        held.placed_in(a_frame_at(100.0, 100.0, 400.0, 225.0), at + ARRIVES_OVER);
        assert!(
            held.standing_in(at + ARRIVES_OVER).is_some(),
            "a window let go of springs back"
        );
    }

    /// A window moved again mid-spring carries on from where it is, rather than
    /// jumping back to where the layout last put it.
    ///
    /// Which is the ordinary case, not an edge one: somebody dragging the Size
    /// slider on the Settings page moves every window in the column several
    /// times a second.
    #[test]
    fn a_window_moved_again_mid_spring_carries_on_from_where_it_is() {
        let at = std::time::Instant::now();
        let state = a_settled_window(at);
        let first = at + ARRIVES_OVER;
        state.placed_in(a_frame_at(100.0, 400.0, 400.0, 225.0), first);

        let part = first + SETTLES_OVER / 4;
        let midway = state.standing_in(part).expect("mid-spring");
        state.placed_in(a_frame_at(100.0, 700.0, 400.0, 225.0), part);
        let restarted = state.standing_in(part).expect("on its way again");
        assert!(
            (restarted.y - midway.y).abs() < 1.0,
            "it carries on from {midway:?}, not from {restarted:?}"
        );
    }

    /// The clock starts when the window is first drawn, and not when it is
    /// first asked about.
    #[test]
    fn the_arrival_is_timed_from_the_first_frame_the_window_is_drawn_on() {
        let state = Floating::default();
        let at = std::time::Instant::now();
        let (first, _) = state.arriving(at).expect("it has only just started");
        assert!(first < 0.01, "the first frame it is drawn on is its first");

        let (later, _) = state
            .arriving(at + ARRIVES_OVER / 2)
            .expect("it is still arriving");
        assert!(later > first);
        assert_eq!(state.arriving(at + ARRIVES_OVER), None);

        // And a window put back into a corner arrives again rather than
        // appearing at once, because it forgets it was ever there.
        state.forget();
        let (again, _) = state
            .arriving(at + ARRIVES_OVER * 10)
            .expect("it starts over");
        assert!(again < 0.01, "it arrives again: {again}");
    }

    /// What a window has to be called, and what it must not be called. The
    /// last two are the ones that matter: a title is a document, a track, a
    /// page — and a browser window showing an article *about* the feature must
    /// not be swept into a corner.
    #[test]
    fn only_the_title_a_browser_gives_that_window_counts() {
        for title in [
            "Picture-in-Picture",
            "picture-in-picture",
            "  Picture-in-Picture  ",
            "PICTURE-IN-PICTURE",
        ] {
            assert!(title_says_picture_in_picture(title), "{title}");
        }
        for title in [
            "Picture in Picture",
            "Picture-in-Picture — Mozilla Firefox",
            "How to use Picture-in-Picture",
            "Application",
            "",
        ] {
            assert!(!title_says_picture_in_picture(title), "{title}");
        }
    }

    /// Switched off, there is no rectangle to put anything in — which is what
    /// leaves such a window tiled to its display like every other one.
    #[test]
    fn a_session_that_does_not_want_a_floating_window_is_given_no_shape() {
        let mut pip = Pip::default();
        let shapes = [pip::DEFAULT_ASPECT];
        assert_eq!(pip.frames(Size::from((1920, 1080)), &shapes).len(), 1);
        assert!(pip.set(Settings {
            floating: false,
            ..Settings::default()
        }));
        assert!(pip.frames(Size::from((1920, 1080)), &shapes).is_empty());
        // And saying the same thing twice is not a change, which is what keeps
        // a shell re-sending its settings from re-laying-out every window.
        assert!(!pip.set(Settings {
            floating: false,
            ..Settings::default()
        }));
    }

    /// The conversation with the client about what shape it wants to be.
    ///
    /// Every step of it, because each one is a case that was got wrong before it
    /// was written down: the question is put once and carries no size, the shape
    /// we told it to be is not news when it draws it, and the first size it
    /// draws of its own is the answer.
    #[test]
    fn a_floating_window_is_asked_what_shape_it_wants_to_be() {
        let state = Floating::default();
        assert_eq!(state.aspect(), pip::DEFAULT_ASPECT, "nothing said yet");
        // Nothing is read from a window that has not been asked: it is showing
        // whatever it was tiled at, which is our shape and not its own.
        assert!(!state.read_answer(Size::from((1920, 1080))));

        // The question, put once — and with it, a note of what it was showing
        // when it went out, for the same reason.
        assert!(state.ask(Size::from((1920, 1080))));
        assert!(!state.ask(Size::from((1920, 1080))), "asked twice");
        assert!(!state.read_answer(Size::from((1920, 1080))));
        assert_eq!(state.aspect(), pip::DEFAULT_ASPECT);

        // And then it draws itself the shape it wants.
        assert!(state.read_answer(Size::from((640, 480))));
        assert!((state.aspect() - 4.0 / 3.0).abs() < 0.001);
        // Which it goes on drawing, sixty times a second, saying nothing new.
        assert!(!state.read_answer(Size::from((640, 480))));

        // The size it is then told to be is our shape read back, whatever it
        // is, and never a second answer.
        state.told(Size::from((460, 345)));
        assert!(!state.read_answer(Size::from((460, 345))));
        assert!((state.aspect() - 4.0 / 3.0).abs() < 0.001);

        // A video swapped for one of another shape in the same window is one,
        // though — that is why this is read for as long as the window floats.
        assert!(state.read_answer(Size::from((360, 640))));
        assert!((state.aspect() - 0.5625).abs() < 0.001);

        // Put back into the ordinary layout, it forgets everything — so the
        // next corner it is given starts by asking again.
        state.forget();
        assert_eq!(state.aspect(), pip::DEFAULT_ASPECT);
        assert!(!state.read_answer(Size::from((640, 480))), "not asked yet");
    }

    /// The bug this was written for: **a placeholder is not an answer.**
    ///
    /// Firefox's picture-in-picture window commits one pixel by one before it
    /// has laid anything out. Read as a shape that says *square*, and what was on
    /// screen was a sixteen-to-nine video drawn in a square frame with the
    /// session showing through underneath it.
    #[test]
    fn the_one_pixel_a_client_starts_with_is_not_a_shape() {
        let state = Floating::default();
        assert!(state.ask(Size::from((1920, 1080))));
        for placeholder in [(0, 0), (1, 1), (1, 200), (31, 31)] {
            assert!(
                !state.read_answer(Size::from(placeholder)),
                "{placeholder:?} was taken for a window"
            );
            assert_eq!(state.aspect(), pip::DEFAULT_ASPECT);
        }
        // And the real answer, when it comes, is still heard. A shape, not a
        // rounding: 460 × 259 would be sixteen to nine to within a quarter of a
        // pixel, which is the shape it is already being drawn at.
        assert!(state.read_answer(Size::from((460, 345))));
        assert!((state.aspect() - 460.0 / 345.0).abs() < 0.001);
    }

    /// A client that keeps whatever size it has, whatever it is told, says its
    /// shape once and is never read again — it is not arguing, it is repeating
    /// itself, and laying the column out on every frame of it would be this
    /// compositor arguing with itself.
    #[test]
    fn a_client_that_will_not_take_its_size_is_read_once() {
        let state = Floating::default();
        assert!(state.ask(Size::from((1920, 1080))));
        assert!(state.read_answer(Size::from((372, 230))));
        for _ in 0..100 {
            state.told(Size::from((460, 284)));
            assert!(!state.read_answer(Size::from((372, 230))));
        }
        assert!((state.aspect() - 372.0 / 230.0).abs() < 0.001);
    }

    /// And a client that answers a *different* shape to every size it is sent is
    /// stopped after [`ANSWERS`] of them.
    ///
    /// The argument has a fixed point, which is the reason for the cap rather
    /// than a footnote to it: the width is the setting's share of the display
    /// and never moves, so a client that draws what it is told plus a constant
    /// drags the height up towards it, and the two meet at a square. Left
    /// running, that is a video that goes square over a couple of seconds.
    #[test]
    fn a_client_that_argues_about_its_shape_is_stopped() {
        let state = Floating::default();
        let width = 460.0;
        assert!(state.ask(Size::from((1920, 1080))));
        let mut told = Size::<i32, Logical>::from((460, 259));
        for _ in 0..100 {
            state.told(told);
            // The client draws what it was told, plus its own furniture.
            state.read_answer(Size::from((told.w + 32, told.h + 32)));
            told = Size::from((460, (width / state.aspect()).round() as i32));
        }
        // It stopped, and it stopped somewhere that is still a picture rather
        // than at the square the argument was heading for.
        let settled = state.aspect();
        state.told(told);
        assert!(!state.read_answer(Size::from((told.w + 32, told.h + 32))));
        assert_eq!(state.aspect(), settled);
        assert!(
            settled > 1.2,
            "it argued its way to {settled} — a square is 1"
        );
    }

    /// A display with no size yet — one being brought up, or one that has just
    /// gone — has nowhere to put a window either.
    #[test]
    fn a_display_of_no_size_is_not_a_corner() {
        let pip = Pip::default();
        let shapes = [pip::DEFAULT_ASPECT];
        assert!(pip.frames(Size::from((0, 0)), &shapes).is_empty());
        assert!(pip.frames(Size::from((1920, 0)), &shapes).is_empty());
    }

    /// The opening in the painted image's own pixels, worked out the way the
    /// render works it out: the rectangle the client's buffer is snapped to,
    /// less where the image lands.
    fn hole(frame: &pip::Frame, scale: f64) -> Rectangle<i32, Physical> {
        let opening = Rectangle::<f64, Logical>::new(
            (frame.inner.x, frame.inner.y).into(),
            (frame.inner.w, frame.inner.h).into(),
        );
        let backing = backing(opening, scale);
        Rectangle::new(backing.loc - origin(frame, scale), backing.size)
    }

    fn frame() -> pip::Frame {
        pip::frame(
            1920.0,
            1080.0,
            pip::Size::Medium,
            pip::Place::TopRight,
            pip::DEFAULT_ASPECT,
            0.0,
        )
    }

    /// Where the mat is opaque and where it is not, which is the whole of what
    /// makes the window look round.
    #[test]
    fn the_mat_is_solid_at_the_corner_and_clear_in_the_middle() {
        let frame = frame();
        let scale = 1.0;
        let width = (frame.outer.w + frame.shadow * 2.0).ceil() as i32;
        let height = (frame.outer.h + frame.shadow * 2.0).ceil() as i32;
        let pixels = paint(&frame, scale, width, height, hole(&frame, scale));
        let at = |x: f64, y: f64| {
            let column = (x + frame.shadow) as usize;
            let row = (y + frame.shadow) as usize;
            let start = (row * width as usize + column) * 4;
            pixels[start + 3]
        };

        // The middle of the opening is the video, untouched.
        assert_eq!(at(frame.outer.w / 2.0, frame.outer.h / 2.0), 0);
        // The middle of the top edge is mat.
        assert_eq!(at(frame.outer.w / 2.0, frame.border / 2.0), 255);
        // The client's own square corner is covered — the argument for the
        // mat's thickness, asked of the painted pixels this time.
        assert_eq!(at(frame.border + 0.5, frame.border + 0.5), 255);
        // And the corner of the bounding box is not: that is the rounding.
        assert!(at(0.5, 0.5) < 255);
    }

    /// A shadow under the shape, and none over the video.
    #[test]
    fn the_shadow_falls_outside_the_shape_only() {
        let frame = frame();
        let width = (frame.outer.w + frame.shadow * 2.0).ceil() as i32;
        let height = (frame.outer.h + frame.shadow * 2.0).ceil() as i32;
        let pixels = paint(&frame, 1.0, width, height, hole(&frame, 1.0));
        let alpha = |x: usize, y: usize| pixels[(y * width as usize + x) * 4 + 3];

        // Just under the bottom edge, in the middle: shadow.
        let under = alpha(
            (frame.shadow + frame.outer.w / 2.0) as usize,
            (frame.shadow + frame.outer.h + 2.0) as usize,
        );
        assert!(under > 0 && under < 255, "{under}");
        // The far corner of the image, a whole shadow's reach away: nothing.
        assert_eq!(alpha(0, 0), 0);
    }

    /// **Nothing shows through the edge of the opening.**
    ///
    /// The bug this was written for, asked of the painted pixels: a one-pixel
    /// line of the application behind, down one side of the video. The mat's
    /// opening antialiases itself over the pixel its edge falls in, so that
    /// pixel is part mat and part nothing — and the colour standing behind the
    /// window was rounded to the *nearest* pixel, which on a fractional opening
    /// lands inside it and leaves the "nothing" half showing the session.
    ///
    /// So: every pixel of the mat that is not fully opaque, and is not out at
    /// the shape's own edge where the shadow starts, has to be inside the
    /// rectangle [`backing`] returns.
    #[test]
    fn nothing_shows_through_the_edge_of_the_opening() {
        for (display, scale) in [
            ((1920.0, 1080.0), 1.0),
            ((2560.0, 1440.0), 1.0),
            // The odd sizes are the point: a quarter of the width inset by a
            // third of a radius that is a share of the height is rarely whole.
            ((2515.0, 1396.0), 1.0),
            ((1280.0, 800.0), 1.5),
        ] {
            for size in pip::Size::ALL {
                for place in pip::Place::ALL {
                    let frame =
                        pip::frame(display.0, display.1, size, place, pip::DEFAULT_ASPECT, 0.0);
                    let opening = Rectangle::<f64, Logical>::new(
                        (frame.inner.x, frame.inner.y).into(),
                        (frame.inner.w, frame.inner.h).into(),
                    );
                    let backing = backing(opening, scale);

                    // Where the painted image lands, which is where the render
                    // puts it: one texel per physical pixel, from a corner
                    // rounded to a whole one.
                    let width = ((frame.outer.w + frame.shadow * 2.0).ceil() * scale).ceil() as i32;
                    let height =
                        ((frame.outer.h + frame.shadow * 2.0).ceil() * scale).ceil() as i32;
                    let pixels = paint(&frame, scale, width, height, hole(&frame, scale));
                    let at_x = ((frame.outer.x - frame.shadow) * scale).round() as i32;
                    let at_y = ((frame.outer.y - frame.shadow) * scale).round() as i32;

                    for row in 0..height {
                        for column in 0..width {
                            let alpha = pixels[((row * width + column) * 4 + 3) as usize];
                            if alpha == 255 {
                                continue;
                            }
                            let x = (column as f64 + 0.5) / scale - frame.shadow;
                            let y = (row as f64 + 0.5) / scale - frame.shadow;
                            // A pixel out at the outer curve is the shape's own
                            // edge against the session, and is meant to be soft.
                            let depth = pip::inside_rounded(
                                x,
                                y,
                                frame.outer.w,
                                frame.outer.h,
                                frame.radius,
                            );
                            if depth <= 1.0 / scale {
                                continue;
                            }
                            let at = Point::<i32, Physical>::from((at_x + column, at_y + row));
                            assert!(
                                backing.contains(at),
                                "{display:?} at {scale}, {size:?} {place:?}: the pixel at \
                                 {at:?} is {alpha}/255 of mat and nothing is behind it \
                                 ({backing:?})"
                            );
                        }
                    }
                }
            }
        }
    }

    /// And the rectangle that backs it is still hidden by the curve.
    ///
    /// It is a rectangle with four square corners, like the client's own, and it
    /// is pushed *outwards* — so the argument for [`lxb_protocol::pip::BORDER`]
    /// has to survive it. There is r·0.057 of slack over the client's corner,
    /// which is under two physical pixels on a 1080p panel: [`BLEED`] fits and
    /// twice it would not.
    #[test]
    fn what_stands_behind_the_window_is_still_under_the_curve() {
        for (display, scale) in [
            ((1920.0, 1080.0), 1.0),
            ((2560.0, 1440.0), 1.0),
            ((1280.0, 800.0), 1.0),
            ((1280.0, 800.0), 2.0),
            ((3840.0, 2160.0), 1.0),
        ] {
            for size in pip::Size::ALL {
                for place in pip::Place::ALL {
                    let frame =
                        pip::frame(display.0, display.1, size, place, pip::DEFAULT_ASPECT, 0.0);
                    let opening = Rectangle::<f64, Logical>::new(
                        (frame.inner.x, frame.inner.y).into(),
                        (frame.inner.w, frame.inner.h).into(),
                    );
                    let backing = backing(opening, scale);
                    let corners = [
                        (backing.loc.x, backing.loc.y),
                        (backing.loc.x + backing.size.w, backing.loc.y),
                        (backing.loc.x, backing.loc.y + backing.size.h),
                        (
                            backing.loc.x + backing.size.w,
                            backing.loc.y + backing.size.h,
                        ),
                    ];
                    for (x, y) in corners {
                        let inside = pip::inside_rounded(
                            x as f64 / scale - frame.outer.x,
                            y as f64 / scale - frame.outer.y,
                            frame.outer.w,
                            frame.outer.h,
                            frame.radius,
                        );
                        assert!(
                            inside > 0.0,
                            "{display:?} at {scale}, {size:?} {place:?}: a corner of what \
                             stands behind the window pokes {inside} out of the curve"
                        );
                    }
                }
            }
        }
    }

    /// A window being dragged by its corner draws every size it is sent, a few
    /// frames behind the hand — and each of those is our own shape rounded to
    /// the whole pixels it is configured in. None of it is the video changing
    /// shape. Reading it as one walks the window away from the shape it really
    /// is, a fraction of a percent at a time, and spends the answers a real
    /// change of video would need: three of them went in one drag of one corner,
    /// measured in a nested session on 2026-08-25.
    #[test]
    fn a_client_catching_up_with_a_resize_is_not_changing_its_shape() {
        let state = Floating::default();
        state.ask(Size::from((1280, 720)));
        state.told(Size::from((354, 199)));
        assert!(!state.read_answer(Size::from((354, 199))));
        for lagged in [(422, 238), (566, 319), (619, 349)] {
            assert!(
                !state.read_answer(Size::from(lagged)),
                "{lagged:?} was read as a new shape"
            );
        }
        assert!((state.aspect() - pip::DEFAULT_ASPECT).abs() < 0.0001);
        // And a video really of another shape still says so.
        assert!(state.read_answer(Size::from((400, 300))));
        assert!((state.aspect() - 4.0 / 3.0).abs() < 0.001);
    }

    // -- the hand on the window ---------------------------------------

    /// A rectangle in the middle of a 1080p display, at the size the layout
    /// draws and at whatever shape is being asked about, with room on every
    /// side for a drag to move it without being clamped.
    fn loose(aspect: f64) -> Rect {
        Rect {
            x: 600.0,
            y: 300.0,
            w: 320.0,
            h: (320.0 - pip::BORDER * 2.0) / aspect + pip::BORDER * 2.0,
        }
    }

    fn display() -> Size<i32, Logical> {
        Size::from((1920, 1080))
    }

    #[test]
    fn the_edges_of_a_floating_window_are_grabbed_and_the_middle_is_carried() {
        let frame = frame();
        let outer = frame.outer;
        let at = |x: f64, y: f64| handle_at(&frame, Point::from((x, y)));
        let (mid_x, mid_y) = (outer.x + outer.w / 2.0, outer.y + outer.h / 2.0);
        let edge = |x, y| Some(Handle::Edge { x, y });

        // The four corners, each of which is two edges at once.
        assert_eq!(
            at(outer.x + 1.0, outer.y + 1.0),
            edge(Some(Pull::Start), Some(Pull::Start))
        );
        assert_eq!(
            at(outer.right() - 1.0, outer.y + 1.0),
            edge(Some(Pull::End), Some(Pull::Start))
        );
        assert_eq!(
            at(outer.x + 1.0, outer.bottom() - 1.0),
            edge(Some(Pull::Start), Some(Pull::End))
        );
        assert_eq!(
            at(outer.right() - 1.0, outer.bottom() - 1.0),
            edge(Some(Pull::End), Some(Pull::End))
        );

        // The four edges, each in the middle of its own side.
        assert_eq!(at(outer.x + 1.0, mid_y), edge(Some(Pull::Start), None));
        assert_eq!(at(outer.right() - 1.0, mid_y), edge(Some(Pull::End), None));
        assert_eq!(at(mid_x, outer.y + 1.0), edge(None, Some(Pull::Start)));
        assert_eq!(at(mid_x, outer.bottom() - 1.0), edge(None, Some(Pull::End)));

        // The band is what it says it is, to the pixel.
        assert_eq!(
            at(outer.x + GRAB - 0.5, mid_y),
            edge(Some(Pull::Start), None)
        );
        assert_eq!(at(outer.x + GRAB + 0.5, mid_y), Some(Handle::Move));

        // The middle carries the window, and the shadow belongs to nobody:
        // a press out there is the application's, not this window's.
        assert_eq!(at(mid_x, mid_y), Some(Handle::Move));
        assert_eq!(at(outer.x - 1.0, mid_y), None);
        assert_eq!(at(outer.right() + 4.0, mid_y), None);
        assert_eq!(at(mid_x, outer.bottom() + 4.0), None);
    }

    /// Pulling one edge moves that edge. The others have not been touched and
    /// must not wander — a window that jumped sideways under the hand pulling
    /// its left edge is a window that cannot be resized to anything on purpose.
    #[test]
    fn a_window_pulled_by_an_edge_leaves_the_other_edges_where_they_were() {
        let origin = loose(pip::DEFAULT_ASPECT);
        let pull = |x, y, dx: f64, dy: f64| {
            resized(
                origin,
                x,
                y,
                Point::from((dx, dy)),
                display(),
                pip::DEFAULT_ASPECT,
            )
        };

        let wider = pull(Some(Pull::Start), None, -60.0, 0.0);
        assert!((wider.right() - origin.right()).abs() < 0.001);
        assert!(wider.w > origin.w);

        let wider = pull(Some(Pull::End), None, 60.0, 0.0);
        assert!((wider.x - origin.x).abs() < 0.001);
        assert!(wider.w > origin.w);

        // A horizontal edge changes the height, and the width follows it,
        // because the shape is the client's. There is no vertical edge being
        // held, so the width grows evenly either side rather than lurching.
        let taller = pull(None, Some(Pull::End), 0.0, 40.0);
        assert!((taller.y - origin.y).abs() < 0.001);
        assert!(taller.h > origin.h && taller.w > origin.w);
        assert!(
            ((taller.x + taller.w / 2.0) - (origin.x + origin.w / 2.0)).abs() < 0.001,
            "the window lurched sideways: {taller:?}"
        );
    }

    /// A hand on a corner scales the window. It does not restretch the video:
    /// the opening is the shape the client asked to be, before the drag and
    /// after it, which is the same promise the corner layout makes.
    #[test]
    fn a_window_pulled_by_a_corner_is_still_the_shape_its_client_asked_to_be() {
        for aspect in [16.0 / 9.0, 4.0 / 3.0, 1.0, 9.0 / 16.0] {
            let origin = loose(aspect);
            for (dx, dy) in [(60.0, 10.0), (10.0, 60.0), (-60.0, -60.0), (60.0, -60.0)] {
                let rect = resized(
                    origin,
                    Some(Pull::End),
                    Some(Pull::End),
                    Point::from((dx, dy)),
                    display(),
                    aspect,
                );
                let mat = pip::BORDER * 2.0;
                assert!(
                    ((rect.w - mat) / (rect.h - mat) - aspect).abs() < 0.001,
                    "{aspect} pulled by ({dx}, {dy}) came out as {rect:?}"
                );
                // And the corner opposite the one being pulled has not moved.
                assert!((rect.x - origin.x).abs() < 0.001);
                assert!((rect.y - origin.y).abs() < 0.001);
            }
        }
    }

    /// However far the hand goes: the window is still on the screen, still a
    /// window, and — being carried rather than pulled — still the size it was.
    #[test]
    fn a_window_carried_off_the_screen_is_brought_back_onto_it() {
        let origin = loose(pip::DEFAULT_ASPECT);
        for (dx, dy) in [
            (-4000.0, -4000.0),
            (4000.0, 4000.0),
            (0.0, 900.0),
            (1400.0, 0.0),
        ] {
            let rect = carried(
                origin,
                Point::from((dx, dy)),
                display(),
                pip::DEFAULT_ASPECT,
            );
            assert!(
                rect.x >= 0.0 && rect.y >= 0.0 && rect.right() <= 1920.0 && rect.bottom() <= 1080.0,
                "carried by ({dx}, {dy}) to {rect:?}"
            );
            assert!((rect.w - origin.w).abs() < 0.001 && (rect.h - origin.h).abs() < 0.001);
        }
    }

    /// A stick held into the edge of the screen builds up no travel to be
    /// pushed back out of.
    ///
    /// The whole reason a controller's drag is measured from where the window is
    /// *now* rather than from where the grab started. A pointer cannot go past
    /// the edge of the display, so a hand pushed into one has nowhere further to
    /// go and the drag it is measured from stops growing with it; a stick has no
    /// such limit, and a delta accumulated against a fixed origin would leave
    /// the window pinned to the edge for as long as it took to push the debt
    /// back off.
    #[test]
    fn a_window_pushed_into_the_edge_by_a_stick_comes_straight_back_off_it() {
        let mut rect = loose(pip::DEFAULT_ASPECT);
        // A second's worth of stick, held right and down into the corner.
        for _ in 0..60 {
            rect = carried(
                rect,
                Point::from((40.0, 40.0)),
                display(),
                pip::DEFAULT_ASPECT,
            );
        }
        // Hard against the edge, margin and all: the margin holds the *column*
        // off the edges, and a window somebody put somewhere by hand is where
        // they put it. See [`lxb_protocol::pip::hand_placed`].
        assert!(
            (rect.right() - 1920.0).abs() < 0.001,
            "pinned against the right edge, not {rect:?}"
        );
        // And one step the other way moves it exactly one step.
        let back = carried(
            rect,
            Point::from((-40.0, -40.0)),
            display(),
            pip::DEFAULT_ASPECT,
        );
        assert!((back.x - (rect.x - 40.0)).abs() < 0.001, "{back:?}");
        assert!((back.y - (rect.y - 40.0)).abs() < 0.001, "{back:?}");
    }

    /// The same for a resize, which is the other thing a stick can be doing:
    /// pulled down to the smallest a window may be and then one step larger.
    #[test]
    fn a_window_pulled_smaller_by_a_stick_grows_again_at_once() {
        let mut rect = loose(pip::DEFAULT_ASPECT);
        let (x, y) = (Some(Pull::End), Some(Pull::End));
        for _ in 0..60 {
            rect = resized(
                rect,
                x,
                y,
                Point::from((-40.0, -40.0)),
                display(),
                pip::DEFAULT_ASPECT,
            );
        }
        assert!((rect.w - pip::SMALLEST).abs() < 0.001, "{rect:?}");
        // And one step the other way is larger at once, by at least the step: a
        // corner takes whichever of its two edges was pulled further, so a
        // window this shape grows by more across than the stick asked for.
        let grown = resized(
            rect,
            x,
            y,
            Point::from((40.0, 40.0)),
            display(),
            pip::DEFAULT_ASPECT,
        );
        assert!(grown.w >= rect.w + 40.0, "{grown:?}");
    }

    /// The mark on a selected window never leaves the accent and never stops
    /// moving: it breathes between two strengths of it rather than blinking.
    #[test]
    fn the_mark_on_a_selected_window_breathes_rather_than_blinking() {
        let mut lowest = f32::MAX;
        let mut highest = f32::MIN;
        // A whole period, finely enough to catch both ends of it.
        for step in 0..=360 {
            let at = std::time::Duration::from_secs_f64(MARK_PERIOD * step as f64 / 360.0);
            let alpha = mark_alpha(at);
            assert!(
                (MARK_LOW..=MARK_HIGH).contains(&alpha),
                "{alpha} at step {step}"
            );
            lowest = lowest.min(alpha);
            highest = highest.max(alpha);
        }
        assert!((lowest - MARK_LOW).abs() < 0.001, "{lowest}");
        assert!((highest - MARK_HIGH).abs() < 0.001, "{highest}");
        // And it is where it started a period later, so nothing about it drifts
        // over an evening.
        let period = std::time::Duration::from_secs_f64(MARK_PERIOD);
        assert!((mark_alpha(period) - mark_alpha(std::time::Duration::ZERO)).abs() < 0.001);
    }

    /// The mark is the frame in the accent and a glow outside it, and nothing
    /// at all over the picture.
    ///
    /// The last clause is the one worth a test: this image is drawn over the
    /// window, so anything it paints inside the opening is a coloured veil over
    /// somebody's video.
    #[test]
    fn the_mark_is_the_frame_and_the_air_around_it_and_never_the_picture() {
        let frame = frame();
        let scale = 1.0;
        let width = (frame.outer.w + frame.shadow * 2.0).ceil() as i32;
        let height = (frame.outer.h + frame.shadow * 2.0).ceil() as i32;
        // Where the opening falls inside the image, worked out exactly as the
        // render works it out.
        let opening = Rectangle::<f64, Logical>::new(
            (frame.inner.x, frame.inner.y).into(),
            (frame.inner.w, frame.inner.h).into(),
        );
        let at = origin(&frame, scale);
        let hole = Rectangle::<i32, Physical>::new(
            backing(opening, scale).loc - at,
            backing(opening, scale).size,
        );
        let accent = 0x3B_82_F6;
        let pixels = paint_mark(&frame, scale, width, height, hole, accent);
        let alpha_at = |x: i32, y: i32| pixels[((y * width + x) * 4 + 3) as usize];

        // The middle of the picture: nothing.
        let middle = (hole.loc.x + hole.size.w / 2, hole.loc.y + hole.size.h / 2);
        assert_eq!(alpha_at(middle.0, middle.1), 0, "the video is veiled");

        // The middle of the frame's left edge: the accent, opaque.
        let edge = (
            hole.loc.x - (pip::BORDER / 2.0).round() as i32,
            hole.loc.y + hole.size.h / 2,
        );
        assert_eq!(alpha_at(edge.0, edge.1), 255, "the frame is not marked");
        let channel = |offset: i32| pixels[((edge.1 * width + edge.0) * 4 + offset) as usize];
        assert_eq!(
            [channel(0), channel(1), channel(2)],
            [0x3B, 0x82, 0xF6],
            "the frame is not the accent"
        );

        // And a pixel just outside the shape, which is the glow. Close in: the
        // glow is bright where it leaves the frame and gone well before the end
        // of the shadow it is painted into.
        let outside = (frame.shadow.round() as i32 - 2, height / 2);
        let glow = alpha_at(outside.0, outside.1);
        assert!(glow > 0 && glow < 255, "the glow is {glow}");
        // And nothing at all at the very edge of the picture, which is where the
        // mat's own shadow has faded out too.
        assert_eq!(alpha_at(0, height / 2), 0, "the glow runs to the edge");
    }

    /// A window cannot be pulled down to nothing, or up past the screen it is
    /// on — and the edge the hand is not holding stays put through both.
    #[test]
    fn a_window_cannot_be_pulled_smaller_than_a_window_or_larger_than_the_screen() {
        let origin = loose(pip::DEFAULT_ASPECT);
        let squashed = resized(
            origin,
            Some(Pull::End),
            Some(Pull::End),
            Point::from((-5000.0, -5000.0)),
            display(),
            pip::DEFAULT_ASPECT,
        );
        assert!(
            squashed.w >= pip::SMALLEST - 0.001,
            "{squashed:?} is a stamp"
        );
        assert!((squashed.x - origin.x).abs() < 0.001);

        let stretched = resized(
            origin,
            Some(Pull::End),
            Some(Pull::End),
            Point::from((5000.0, 5000.0)),
            display(),
            pip::DEFAULT_ASPECT,
        );
        assert!(
            stretched.w <= 1920.001 && stretched.h <= 1080.001,
            "{stretched:?}"
        );
    }

    /// A window is resized from the corner with the most screen behind it: one
    /// parked in a corner is pulled from the far one, which is where the room
    /// is, and one near an edge is pulled away from that edge.
    #[test]
    fn a_window_is_resized_from_the_corner_with_the_most_room() {
        for (place, corner) in [
            (pip::Place::TopRight, (Pull::Start, Pull::End)),
            (pip::Place::TopLeft, (Pull::End, Pull::End)),
            (pip::Place::BottomRight, (Pull::Start, Pull::Start)),
            (pip::Place::BottomLeft, (Pull::End, Pull::Start)),
        ] {
            let frame = pip::frame(
                1920.0,
                1080.0,
                pip::Size::Medium,
                place,
                pip::DEFAULT_ASPECT,
                0.0,
            );
            assert_eq!(
                resize_corner(frame.outer, display()),
                corner,
                "{place:?} is pulled the wrong way"
            );
        }

        // Near the bottom of the screen and well left of the middle: the room
        // is up and to the right, whatever the corner setting once said.
        let low = Rect {
            x: 100.0,
            y: 800.0,
            w: 400.0,
            h: 225.0,
        };
        assert_eq!(resize_corner(low, display()), (Pull::End, Pull::Start));

        // And the pointer is put on that corner of the window, which is what
        // makes the corner follow the hand rather than jump to it.
        let at = corner_point(low, Pull::End, Pull::Start);
        assert_eq!((at.x, at.y), (low.right(), low.y));
        let at = middle(low);
        assert_eq!((at.x, at.y), (300.0, 912.5));
    }

    /// The middle of the window is the client's cursor's; the edges are ours,
    /// and they say which way they will move.
    #[test]
    fn only_the_edges_take_the_pointers_shape() {
        assert_eq!(icon(Handle::Move), None);
        assert_eq!(
            icon(Handle::Edge {
                x: Some(Pull::End),
                y: Some(Pull::Start)
            }),
            Some(CursorIcon::NeResize)
        );
        assert_eq!(
            icon(Handle::Edge {
                x: None,
                y: Some(Pull::End)
            }),
            Some(CursorIcon::SResize)
        );
    }

    /// Nothing in the image is anything but black where it is transparent, so
    /// the premultiplied buffer cannot bleed a grey halo when it is scaled.
    #[test]
    fn the_image_is_premultiplied() {
        let frame = frame();
        let width = (frame.outer.w + frame.shadow * 2.0).ceil() as i32;
        let height = (frame.outer.h + frame.shadow * 2.0).ceil() as i32;
        let pixels = paint(&frame, 1.0, width, height, hole(&frame, 1.0));
        for pixel in pixels.chunks_exact(4) {
            for channel in 0..3 {
                assert!(
                    pixel[channel] <= pixel[3],
                    "a colour brighter than its own alpha: {pixel:?}"
                );
            }
        }
    }
}
