//! Multi-display layout.
//!
//! Gamescope is single-output by construction: it owns one CRTC and scales one
//! application onto it. LineXinBar keeps the same "one app fills the screen"
//! model but tracks an arbitrary number of outputs, each with its own logical
//! position, scale, transform and window stack.
//!
//! Outputs are laid out in a stable order (the order they were first seen), so
//! unplugging and replugging a monitor does not shuffle the others around.

use std::cell::RefCell;

use smithay::desktop::{layer_map_for_output, Space, Window};
use smithay::output::{Output, Scale};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::utils::{Logical, Point, Rectangle, Size, Transform};
use smithay::wayland::shell::xdg::ToplevelStateSet;

use lxb_protocol::pip;

use crate::config::{Config, OutputLayout};
use crate::input::window_is_x11_chrome;

/// One mode a display can be driven at, as the shell is told about it.
///
/// Not [`smithay::output::Mode`], which is what an output is *currently* set
/// to and grows a new entry every time that changes: a nested window resized
/// three times has three modes by that measure, none of which anybody could
/// ask to be put back into. This is the connector's own list, which is fixed
/// for as long as the cable is in, plus the two things that make one row of it
/// worth pointing at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayMode {
    pub width: u32,
    pub height: u32,
    /// Refresh in mHz, as DRM reports it. 0 when there is no meaningful rate.
    pub refresh: u32,
    /// The display is being driven at this mode right now.
    pub current: bool,
    /// The display names this one as its own — its native timing.
    pub preferred: bool,
}

/// The display a window belongs to, remembered on the window itself.
#[derive(Debug)]
struct HomeOutput(RefCell<Output>);

/// Record the display a window was placed on.
pub(crate) fn assign_output(window: &Window, output: &Output) {
    *window
        .user_data()
        .get_or_insert(|| HomeOutput(RefCell::new(output.clone())))
        .0
        .borrow_mut() = output.clone();
}

/// The display a window was placed on, whether or not it has drawn anything
/// there yet.
pub(crate) fn assigned_output(window: &Window) -> Option<Output> {
    window
        .user_data()
        .get::<HomeOutput>()
        .map(|home| home.0.borrow().clone())
}

/// Which display a window belongs to, in order of how well each answer knows.
///
/// The assignment comes first because it is the only one that is right at the
/// moment it matters most. `Space` derives the outputs an element is on from
/// its bounding box, and a window that has mapped but not yet committed a
/// buffer has no bounding box — so it overlaps nothing, belongs to no display,
/// and every path that re-tiles it in that window (a sibling closing, the
/// client asking to be maximized, an X11 configure) would fall through to the
/// first output. That is exactly how an application started on the second
/// screen arrives on the first one the moment it finishes loading.
///
/// `overlapping` is what the space does know, for a window mapped before this
/// was recorded. `None` when neither has an answer — which is a real one, and
/// different from the guess [`pick_output`] makes.
fn known_output(
    assigned: Option<&Output>,
    overlapping: Option<&Output>,
    connected: &[Output],
) -> Option<Output> {
    // A display can be unplugged while a window assigned to it is still open,
    // so both answers are validated rather than trusted. The space's is no
    // safer than the assignment: an element keeps the outputs it overlapped
    // until the next refresh, so right after a hotplug removal it still names
    // the display that has just gone — and a window tiled onto a display with
    // no geometry left is a window that is never tiled at all.
    let connected = |output: &&Output| connected.contains(output);
    assigned
        .filter(connected)
        .or_else(|| overlapping.filter(connected))
        .cloned()
}

/// The same, plus the last resort: a window has to be put somewhere, and the
/// first connected display is where.
fn pick_output(
    assigned: Option<&Output>,
    overlapping: Option<&Output>,
    connected: &[Output],
) -> Option<Output> {
    known_output(assigned, overlapping, connected).or_else(|| connected.first().cloned())
}

/// Move a mapped window without changing its position in the compositor's
/// stack. `Space::map_element` always raises existing elements, which is not
/// desirable for routine configure notifications and output relayouts.
pub(crate) fn remap_window_preserving_stack(
    space: &mut Space<Window>,
    window: &Window,
    location: Point<i32, Logical>,
) {
    let order: Vec<Window> = space.elements().cloned().collect();
    let was_mapped = order.iter().any(|element| element == window);

    space.map_element(window.clone(), location, false);
    if was_mapped {
        for element in order {
            space.raise_element(&element, false);
        }
    }
}

/// The states every application window is configured with.
///
/// `Maximized` is the load-bearing one: it is what turns the size in the
/// configure from a suggestion into an instruction. The tiled edges say the
/// same thing to clients that reason about their surroundings rather than
/// their state, and they are also what tells a client drawing its own
/// decorations to square off its corners and drop its shadow — there is no
/// desktop behind this window for a shadow to fall on.
pub(crate) fn set_maximized_states(states: &mut ToplevelStateSet) {
    states.set(xdg_toplevel::State::Maximized);
    states.set(xdg_toplevel::State::TiledLeft);
    states.set(xdg_toplevel::State::TiledRight);
    states.set(xdg_toplevel::State::TiledTop);
    states.set(xdg_toplevel::State::TiledBottom);
}

/// Tracks the logical arrangement of every enabled output.
#[derive(Debug, Default)]
pub struct OutputManager {
    /// Outputs in the order they were added; drives auto-placement.
    order: Vec<Output>,
    /// How much larger than life applications draw themselves.
    ///
    /// Here because it is part of the layout: what it changes is how much room
    /// a window is given on the display it is tiled onto. See [`crate::scale`],
    /// which is where the rest of that bargain — what the client is told, and
    /// how its pixels are drawn back out — is written down.
    scale: crate::scale::AppScale,
    /// What a browser's picture-in-picture window is given, and the mat drawn
    /// round it.
    ///
    /// Here for the reason the scale above it is here: it is part of the
    /// layout. The floating window is the one window on this session that is
    /// not tiled to its display, and what this holds is the rectangle it is
    /// tiled to instead — asked at exactly the same moment, in exactly the same
    /// call. See [`crate::pip`], which is where the shape of it is argued.
    pip: crate::pip::Pip,
}

impl OutputManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Draw applications this much larger than life from now on. `true` when
    /// that is a change, which is what the caller re-tiles and redraws on.
    pub fn set_app_scale(&mut self, scale: crate::scale::AppScale) -> bool {
        let changed = self.scale != scale;
        self.scale = scale;
        changed
    }

    /// What the shell has asked a picture-in-picture window to look like.
    pub fn pip(&self) -> &crate::pip::Pip {
        &self.pip
    }

    /// The same, to be written to — which only [`crate::pip`] itself does, to
    /// keep the note of which windows were floating last time it looked.
    pub fn pip_mut(&mut self) -> &mut crate::pip::Pip {
        &mut self.pip
    }

    /// Take a new answer. `true` when it is a change, which is what the caller
    /// lays the windows out and redraws on.
    pub fn set_pip(&mut self, settings: crate::pip::Settings) -> bool {
        self.pip.set(settings)
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Register a new output and place it according to `config`.
    pub fn add_output(&mut self, space: &mut Space<Window>, output: &Output, config: &Config) {
        if self.order.iter().any(|o| o == output) {
            return;
        }
        self.order.push(output.clone());
        self.relayout(space, config);
    }

    /// Drop an output (hotplug removal) and re-pack the survivors.
    pub fn remove_output(&mut self, space: &mut Space<Window>, output: &Output, config: &Config) {
        self.order.retain(|o| o != output);
        space.unmap_output(output);
        self.relayout(space, config);
    }

    /// Recompute logical positions for every output, then re-tile windows.
    ///
    /// Outputs with an explicit `position` in the config are pinned there and
    /// take no part in auto-placement; the rest are packed in order.
    pub fn relayout(&mut self, space: &mut Space<Window>, config: &Config) {
        let layout = config.general.output_layout;
        let gap = config.general.output_gap;
        let mut cursor = 0i32;

        for output in &self.order {
            let name = output.name();
            let entry = config.output_for(&name);

            let size = logical_size(output);

            let position = match entry.and_then(|e| e.position) {
                Some([x, y]) => Point::from((x, y)),
                None => match layout {
                    OutputLayout::Horizontal => {
                        let p = Point::from((cursor, 0));
                        cursor += size.w + gap;
                        p
                    }
                    OutputLayout::Vertical => {
                        let p = Point::from((0, cursor));
                        cursor += size.h + gap;
                        p
                    }
                    OutputLayout::Mirror => Point::from((0, 0)),
                },
            };

            space.map_output(output, position);
            // `map_output` only records the position inside the space; the
            // output's own location is what wl_output and xdg_output advertise,
            // so it has to be set too or every client sees all displays stacked
            // at the origin.
            output.change_current_state(None, None, None, Some(position));

            tracing::debug!(
                output = %name,
                x = position.x,
                y = position.y,
                w = size.w,
                h = size.h,
                "placed output"
            );
        }

        self.relayout_windows(space);
    }

    /// Apply the config's scale/transform/mode preferences to a freshly created
    /// output. The caller is responsible for the mode itself, since only the
    /// backend knows which modes the hardware actually offers.
    pub fn apply_output_config(output: &Output, config: &Config) {
        let name = output.name();
        let Some(entry) = config.output_for(&name) else {
            return;
        };

        let scale = entry.scale.map(Scale::Fractional);
        let transform = entry.transform.as_deref().and_then(parse_transform);

        if scale.is_some() || transform.is_some() {
            output.change_current_state(None, transform, scale, None);
        }
    }

    /// Whether the config disables this connector outright.
    pub fn is_enabled(name: &str, config: &Config) -> bool {
        config
            .output_for(name)
            .and_then(|e| e.enabled)
            .unwrap_or(true)
    }

    /// The output under a logical point, falling back to the first output.
    pub fn output_at(&self, space: &Space<Window>, point: Point<f64, Logical>) -> Option<Output> {
        space
            .output_under(point)
            .next()
            .cloned()
            .or_else(|| self.order.first().cloned())
    }

    /// The usable area of an output: its geometry minus layer-shell exclusive
    /// zones (panels, docks, and the desktop shell when it reserves space).
    pub fn usable_area(space: &Space<Window>, output: &Output) -> Option<Rectangle<i32, Logical>> {
        let geometry = space.output_geometry(output)?;
        let mut zone = layer_map_for_output(output).non_exclusive_zone();
        // `non_exclusive_zone` is output-relative; lift it into space coordinates.
        zone.loc += geometry.loc;
        Some(zone)
    }

    /// The display a window belongs to, where anything knows: the one it was
    /// placed on for as long as that display is still connected, else whatever
    /// the space can say about where the window currently is.
    pub fn window_display(&self, space: &Space<Window>, window: &Window) -> Option<Output> {
        let connected: Vec<Output> = space.outputs().cloned().collect();
        known_output(
            assigned_output(window).as_ref(),
            space.outputs_for_element(window).first(),
            &connected,
        )
    }

    /// The display to put a window on: the one it belongs to, or the first
    /// connected one when nothing knows.
    pub fn window_output(&self, space: &Space<Window>, window: &Window) -> Option<Output> {
        let connected: Vec<Output> = space.outputs().cloned().collect();
        pick_output(
            assigned_output(window).as_ref(),
            space.outputs_for_element(window).first(),
            &connected,
        )
    }

    /// Size a single window to fill its output's usable area.
    pub fn tile_window(&self, space: &mut Space<Window>, window: &Window) {
        if let Some(output) = self.window_output(space, window) {
            self.tile_window_on_output(space, window, &output);
        }
    }

    /// Size a window on an explicit output. Newly mapped/moved elements do
    /// not have refreshed Space output associations yet, so callers that
    /// already chose a target must not infer it through
    /// `outputs_for_element`.
    pub fn tile_window_on_output(
        &self,
        space: &mut Space<Window>,
        window: &Window,
        output: &Output,
    ) {
        // Before the early returns below: chrome belongs to the display it
        // opened on just as much as an application window does, even though
        // its geometry is the client's business rather than ours.
        assign_output(window, output);

        // X11 chrome (override-redirect popups as well as managed menus,
        // notifications and splash windows) chooses its own geometry and must
        // never become a full-output application window during relayout.
        if window_is_x11_chrome(window) {
            return;
        }
        if window
            .x11_surface()
            .is_some_and(|surface| surface.is_override_redirect())
        {
            return;
        }

        // The one window that is not tiled to the display: a browser's
        // picture-in-picture, which is given a corner of the *whole* screen
        // rather than of what the layer surfaces leave over. It floats over
        // those too — see [`crate::render`] — so measuring it against their
        // exclusive zones would hold it off an edge nothing is on, which is
        // also why it is answered before the usable area is asked for at all.
        if self.floats(window) {
            self.float_window(space, window, output);
            return;
        }

        let Some(area) = Self::usable_area(space, output) else {
            return;
        };

        if let Some(toplevel) = window.toplevel() {
            // The one place a window is given less room than the display has,
            // and the first of the three parts of drawing an application larger
            // than life: fewer logical pixels to lay its interface out in, each
            // of them worth more of the screen. The other two are the scale the
            // client is told and the way its buffer is drawn back out; see
            // [`crate::scale`]. A Wayland window only — the X11 branch below
            // takes the whole area, because there is no scale to tell an X11
            // client about and a magnified window is not a larger one.
            let room = self.room_for(window, area.size);
            toplevel.with_pending_state(|state| {
                state.size = Some(room);
                // A size alone is only advisory. xdg-shell lets a client pick
                // its own dimensions unless the configure also carries a state
                // that makes the size binding, so without this a client maps at
                // whatever size it likes — typically its remembered desktop
                // geometry, which then hangs off the edge of the output and
                // only snaps into place once the user maximizes it by hand.
                set_maximized_states(&mut state.states);
                // For the window's own idea of a sensible size, before and
                // outside of any state we impose.
                state.bounds = Some(room);
            });
            // And the second part, sent with the size it belongs to: the scale
            // that turns those logical pixels back into the display's own.
            crate::scale::tell(
                window,
                output.current_scale().fractional_scale(),
                self.scale.factor(),
            );
            toplevel.send_pending_configure();
        } else if let Some(surface) = window.x11_surface() {
            // The X11 equivalent: _NET_WM_STATE_MAXIMIZED_{HORZ,VERT}, so a
            // toolkit that sizes itself from the state property agrees with the
            // geometry we are about to hand it.
            if let Err(err) = surface.set_maximized(true) {
                tracing::warn!(?err, "failed to mark X11 window maximized");
            }
            if let Err(err) = surface.configure(area) {
                tracing::warn!(?err, "failed to tile X11 window");
            }
        }
        remap_window_preserving_stack(space, window, area.loc);
    }

    /// The size to configure `window` at so that what the user ends up seeing
    /// is `area`.
    ///
    /// The one division that makes an application draw larger than life, asked
    /// in one place because two different callers have to make it about two
    /// different rectangles and they are the same question: tiling a window
    /// onto the usable area of its display, and answering a client that asks
    /// to go fullscreen on the whole of it. A caller that took the rectangle
    /// it wanted covered and handed that to the client would be handing over a
    /// buffer a factor too large in each direction and having it drawn a
    /// factor past the screen — which is exactly what cropped a fullscreen
    /// video, on a browser window scaled above natural size, until the
    /// fullscreen path came through here too.
    ///
    /// [`Self::window_scale`] rather than this manager's own factor, so the
    /// window the setting does not apply to — the floating one — is answered
    /// here the same way the render and the input mapping answer it.
    pub fn room_for(&self, window: &Window, area: Size<i32, Logical>) -> Size<i32, Logical> {
        crate::scale::configured_size(area, self.window_scale(window))
    }

    /// How much larger than life `window` draws itself: the session's factor
    /// for an application, and one to one for the floating window.
    ///
    /// The exception is the whole reason this is asked here rather than of
    /// [`crate::scale::window_scale`] directly. The application scale answers
    /// how far the user is sitting from a screen full of interface; a video
    /// already shrunk into a corner has no interface to enlarge, and enlarging
    /// it would only crop the picture. Everything that has to agree about a
    /// window's size on screen — what the client is told over
    /// `wp_fractional_scale_v1`, where its pixels are drawn, and where a press
    /// on them lands — asks this one question.
    pub fn window_scale(&self, window: &Window) -> f64 {
        match self.floats(window) {
            true => 1.0,
            false => crate::scale::window_scale(self.scale, window),
        }
    }

    /// Whether this window is the floating one, which is the shell's setting
    /// and the window's own title. See [`crate::state::Lxb::floating`], which
    /// is the same question asked where the whole compositor is in hand.
    pub fn floats(&self, window: &Window) -> bool {
        self.pip.settings().floating && crate::pip::can_float(window)
    }

    /// Put the floating window in its corner of `display`, at the size the shell
    /// asked for and the shape its own client asked for.
    ///
    /// The opening in the mat rather than the whole shape: the mat is drawn
    /// over the window's own edges, so a client configured at the outer
    /// rectangle would have the outermost tenth of its picture painted over.
    /// What it is handed is the rectangle that is actually left visible.
    ///
    /// Not maximized and not tiled, unlike every other window here. Those
    /// states are how a client is told its size is binding and that there is no
    /// desktop behind it for a shadow to fall on; this window has both the
    /// opposite facts about it, and a browser told it was maximized squares off
    /// the very corners this exists to round.
    ///
    /// **The first configure carries no size at all.** That is the question in
    /// [`crate::pip::Floating`]: this window arrives having been tiled to the
    /// whole display, so its shape is our shape and not its own, and xdg-shell's
    /// way of asking a client what size it wants is to send none. The answer
    /// arrives as the next size it draws that is not one of ours, and from then
    /// on the rectangle is worked out from it and sent like any other — and it
    /// is still listened for, because a browser puts a video of another shape
    /// into the same window. What is sent is written down
    /// ([`crate::pip::Floating::told`]) so that the client drawing it is read as
    /// agreement rather than as something new to say.
    ///
    /// The application scale is deliberately not applied. It answers how far
    /// the user is sitting from a *full screen* of interface; a video already
    /// scaled down to a quarter of the display has no interface to enlarge, and
    /// enlarging it would only crop the picture.
    ///
    /// **A window the user has moved by hand is placed where they put it**, and
    /// takes no part in the column — see [`crate::pip::Floating::placed`]. Its
    /// rectangle is still brought back onto the display every time through,
    /// because that is a fact that can change under a window long after the
    /// hand let go of it: a mode change, a rotation, a screen unplugged.
    fn float_window(&self, space: &mut Space<Window>, window: &Window, output: &Output) {
        let Some(display) = space.output_geometry(output) else {
            return;
        };
        let drawn = space
            .element_geometry(window)
            .map(|geometry| geometry.size)
            .unwrap_or_default();
        let state = crate::pip::floating_state(window);
        let asking = state.ask(drawn);
        let since = state.since(|| self.pip.next_in_order());
        let (width, height) = (display.size.w as f64, display.size.h as f64);
        let frame = match state.placed() {
            // Where the user dragged it to, brought back onto the display it is
            // on — which is a fact that can change under a window long after
            // the hand let go of it, from a mode change or a rotation. The
            // clamped rectangle is written back, so that what the next drag
            // starts from is what is on the screen.
            Some(rect) => {
                let rect = pip::hand_placed(rect, width, height, state.aspect());
                state.place_at(rect);
                pip::frame_at(rect, width, height, state.aspect())
            }
            // The whole column, and this window's place in it. Both come from
            // the same list so they cannot disagree about which shape belongs to
            // which window — see [`OutputManager::floating_column`].
            None => {
                let (shapes, place) = self.floating_column(space, output, since, state.aspect());
                match self
                    .pip
                    .frames(display.size, &shapes)
                    .into_iter()
                    .nth(place)
                {
                    Some(frame) => frame,
                    None => return,
                }
            }
        };
        state.placed_in(frame, std::time::Instant::now());

        let inner = frame.inner;
        let size = Size::<i32, Logical>::from((inner.w.round() as i32, inner.h.round() as i32));
        let at = display.loc
            + Point::<i32, Logical>::from((inner.x.round() as i32, inner.y.round() as i32));

        if let Some(toplevel) = window.toplevel() {
            toplevel.with_pending_state(|state| {
                // No size while the question is out: that *is* the question.
                state.size = (!asking).then_some(size);
                state.bounds = Some(size);
                // Whatever it was configured with while it was an ordinary
                // window, which it may well have been a moment ago: a browser
                // titles this window after it maps.
                state.states.unset(xdg_toplevel::State::Maximized);
                state.states.unset(xdg_toplevel::State::Fullscreen);
                state.states.unset(xdg_toplevel::State::TiledLeft);
                state.states.unset(xdg_toplevel::State::TiledRight);
                state.states.unset(xdg_toplevel::State::TiledTop);
                state.states.unset(xdg_toplevel::State::TiledBottom);
            });
            // Told the display's own scale and no application scale, which is
            // the buffer this window's size really asks for.
            crate::scale::tell(window, output.current_scale().fractional_scale(), 1.0);
            toplevel.send_pending_configure();
            // What it has been told to be, so that it drawing exactly this is
            // read as agreement rather than as a fresh answer.
            if !asking {
                state.told(size);
            }
        } else if let Some(surface) = window.x11_surface() {
            // X11 has no way to ask a window what size it would like and no
            // separate geometry to ask it about, so there is no question to put
            // here: the window is sized, as it always was.
            if let Err(err) = surface.set_maximized(false) {
                tracing::warn!(?err, "failed to unmark an X11 window maximized");
            }
            if let Err(err) = surface.configure(Rectangle::new(at, size)) {
                tracing::warn!(?err, "failed to place an X11 window as picture-in-picture");
            }
            state.told(size);
        }
        remap_window_preserving_stack(space, window, at);
    }

    /// The shapes of every floating window on `output`, in the order they
    /// started floating, and which of them the window at `since` is.
    ///
    /// Per display, because a corner is a corner of one screen: two videos on
    /// two screens each keep the corner they were put in. In the order they
    /// started floating and not the order they are stacked in, because a window
    /// is raised by being clicked on — and a second video that jumped into the
    /// corner because somebody pressed pause on the first one would be two
    /// windows swapping places under the hand doing it.
    ///
    /// The display is read from what each window was **assigned**, never from
    /// which output the space says it overlaps. A window that has just been
    /// mapped or moved has no refreshed output association yet — the same trap
    /// [`OutputManager::tile_window_on_output`] is written against — and a
    /// window missing from its own column is a column with nothing in it, which
    /// is a floating window that is never given a rectangle at all. What that
    /// looked like on screen was a video drawn over the whole display, which is
    /// why it is written down here rather than remembered.
    ///
    /// `aspect` is the asking window's own shape, used if it is somehow not in
    /// the column at all: a window laid out on its own is better than one laid
    /// out nowhere.
    fn floating_column(
        &self,
        space: &Space<Window>,
        output: &Output,
        since: u64,
        aspect: f64,
    ) -> (Vec<f64>, usize) {
        let mut column: Vec<(u64, f64)> = space
            .elements()
            .filter(|window| self.floats(window))
            .filter(|window| assigned_output(window).as_ref() == Some(output))
            // And a window the user has dragged somewhere is not in the column
            // at all: it left, and the place it stood in is free. That is what
            // makes the next video that starts floating take the corner rather
            // than stand below a window that is no longer under it.
            .filter(|window| crate::pip::floating_state(window).placed().is_none())
            .filter_map(|window| {
                let state = crate::pip::floating_state(window);
                Some((state.floating_since()?, state.aspect()))
            })
            .collect();
        column.sort_by_key(|(since, _)| *since);
        match column.iter().position(|(other, _)| *other == since) {
            Some(place) => (
                column.into_iter().map(|(_, aspect)| aspect).collect(),
                place,
            ),
            None => (vec![aspect], 0),
        }
    }

    /// Turn one display's picture, and rebuild everything the turn moved.
    ///
    /// `false` when it is already at that orientation, which is what keeps this
    /// idempotent: a shell that sends what a display is already doing — as one
    /// re-sending its settings for a display plugged back in does — must not
    /// re-tile every window on it for nothing.
    ///
    /// A quarter turn swaps the display's logical width and height, so the
    /// order below is the order a mode change uses and for the same reasons:
    /// the layer surfaces anchored to this display are arranged against its new
    /// geometry first, and the windows are then tiled into what those leave
    /// over. `relayout` re-packs the displays laid out after this one as well,
    /// because a screen that has just become tall and narrow has moved them.
    ///
    /// Nothing is sent to the connector. This is the compositor's own drawing:
    /// the picture is composited turned and scanned out at the mode's own
    /// pixels, which is why it needs no hardware support and can never be
    /// refused by the hardware.
    pub fn set_transform(
        &mut self,
        space: &mut Space<Window>,
        output: &Output,
        transform: Transform,
        config: &Config,
    ) -> bool {
        if output.current_transform() == transform {
            return false;
        }
        output.change_current_state(None, Some(transform), None, None);
        layer_map_for_output(output).arrange();
        self.relayout(space, config);
        tracing::info!(
            output = %output.name(),
            ?transform,
            size = ?logical_size(output),
            "turned a display's picture"
        );
        true
    }

    /// The displays this compositor arranges, in the order it arranges them.
    ///
    /// Not every connected display: only the ones whose place in the row is
    /// this compositor's to decide, which is what the shell's page has to be
    /// built from. Two kinds are left out, and both would otherwise be a row
    /// that does nothing when pressed.
    ///
    /// A display pinned with `position` in the config takes no part in
    /// auto-placement — [`Self::relayout`] puts it where the file says and does
    /// not advance the cursor for it — so moving it along a list it is not in
    /// would change nothing. And a session laid out as [`OutputLayout::Mirror`]
    /// has every display at the origin showing the same region, where there is
    /// no first screen for one to be.
    pub fn placed(&self, config: &Config) -> Vec<Output> {
        if config.general.output_layout == OutputLayout::Mirror {
            return Vec::new();
        }
        self.order
            .iter()
            .filter(|output| {
                config
                    .output_for(&output.name())
                    .and_then(|entry| entry.position)
                    .is_none()
            })
            .cloned()
            .collect()
    }

    /// Put a display at `place`, trading with whichever display is there.
    ///
    /// A trade rather than an insertion, and that is the whole of the
    /// behaviour: it is self-inverse, so a user who has just put their fourth
    /// screen first can undo it by putting it back, and it composes — a shell
    /// restoring a whole arrangement sends one of these per display in
    /// ascending place, and each one fixes a display that no later request can
    /// disturb, because no later request names the place it was fixed at.
    ///
    /// `false` when nothing moved: the display is already there, it is not one
    /// this compositor places, or there is no such place. What follows a `true`
    /// is a full relayout — every display after the two that traded may have
    /// moved, since they need not be the same width — and every window with
    /// them.
    pub fn set_place(
        &mut self,
        space: &mut Space<Window>,
        output: &Output,
        place: usize,
        config: &Config,
    ) -> bool {
        let placed = self.placed(config);
        let Some(from) = placed.iter().position(|candidate| candidate == output) else {
            tracing::debug!(
                output = %output.name(),
                "this display's place is not this compositor's to set"
            );
            return false;
        };
        let Some(other) = placed.get(place) else {
            tracing::debug!(
                output = %output.name(),
                place,
                placed = placed.len(),
                "there is no such place to move a display to"
            );
            return false;
        };
        if from == place {
            return false;
        }
        // The two indices are into `placed`, which skips the pinned displays;
        // the swap happens in `order`, which does not. Found by identity rather
        // than by arithmetic for exactly that reason.
        let (Some(a), Some(b)) = (
            self.order.iter().position(|held| held == output),
            self.order.iter().position(|held| held == other),
        ) else {
            return false;
        };
        self.order.swap(a, b);
        self.relayout(space, config);
        tracing::info!(
            output = %output.name(),
            traded_with = %other.name(),
            from,
            to = place,
            "moved a display along the layout"
        );
        true
    }

    /// Re-tile every mapped window. Cheap enough to call on any layout change.
    pub fn relayout_windows(&self, space: &mut Space<Window>) {
        let windows: Vec<Window> = space.elements().cloned().collect();
        for window in windows {
            self.tile_window(space, &window);
        }
    }
}

impl crate::state::LxbState {
    /// Move one display along the arrangement. `true` when anything moved.
    ///
    /// Unlike a mode or a transform there is no backend behind this: the
    /// layout is the compositor's own bookkeeping either way, so a nested
    /// session arranges its displays exactly as a session on real connectors
    /// does.
    pub fn set_output_place(&mut self, output: &smithay::output::Output, place: usize) -> bool {
        let config = self.lxb.config.clone();
        let moved = self
            .lxb
            .outputs
            .set_place(&mut self.lxb.space, output, place, &config);
        if moved {
            // Nothing is scanned out by the move itself: the displays are in
            // new places and every window has been re-tiled into them, none of
            // which reaches a screen until that screen draws again.
            self.queue_redraw();
        }
        moved
    }
}

/// The output's size in logical coordinates, honouring scale and transform.
///
/// This must round exactly as `Space::output_geometry` does — it ceils — or at
/// a fractional scale the layout would advance the cursor by a width that
/// disagrees with the geometry every hit-test and render path reads back,
/// leaving neighbouring outputs a pixel apart or a pixel overlapped.
fn logical_size(output: &Output) -> Size<i32, Logical> {
    let Some(mode) = output.current_mode() else {
        return Size::from((0, 0));
    };
    output
        .current_transform()
        .transform_size(mode.size)
        .to_f64()
        .to_logical(output.current_scale().fractional_scale())
        .to_i32_ceil()
}

/// The inverse of [`parse_transform`], so an orientation chosen at runtime can
/// be written back into the same file it would have been read from.
pub fn transform_name(transform: Transform) -> &'static str {
    match transform {
        Transform::Normal => "normal",
        Transform::_90 => "90",
        Transform::_180 => "180",
        Transform::_270 => "270",
        Transform::Flipped => "flipped",
        Transform::Flipped90 => "flipped-90",
        Transform::Flipped180 => "flipped-180",
        Transform::Flipped270 => "flipped-270",
    }
}

fn parse_transform(raw: &str) -> Option<Transform> {
    Some(match raw.trim().to_ascii_lowercase().as_str() {
        "normal" | "0" => Transform::Normal,
        "90" => Transform::_90,
        "180" => Transform::_180,
        "270" => Transform::_270,
        "flipped" => Transform::Flipped,
        "flipped-90" | "flipped90" => Transform::Flipped90,
        "flipped-180" | "flipped180" => Transform::Flipped180,
        "flipped-270" | "flipped270" => Transform::Flipped270,
        _ => return None,
    })
}

#[cfg(test)]
mod transform_names {
    use super::*;

    /// An orientation chosen at runtime is written back into the file the
    /// config is read from, so every one of the eight has to come back as
    /// itself. A name that does not parse is a display that comes up unrotated
    /// and then turns, which is the modeset this was all meant to remove.
    #[test]
    fn every_orientation_survives_being_written_down() {
        for turn in [
            Transform::Normal,
            Transform::_90,
            Transform::_180,
            Transform::_270,
            Transform::Flipped,
            Transform::Flipped90,
            Transform::Flipped180,
            Transform::Flipped270,
        ] {
            let name = transform_name(turn);
            assert_eq!(parse_transform(name), Some(turn), "{name}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::output::{Mode, PhysicalProperties, Subpixel};

    fn output(name: &str) -> Output {
        Output::new(
            name.to_string(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "test".into(),
                model: "test".into(),
            },
        )
    }

    fn connected() -> Vec<Output> {
        let outputs = vec![output("first"), output("second")];
        for o in &outputs {
            o.change_current_state(
                Some(Mode {
                    size: (1920, 1080).into(),
                    refresh: 60_000,
                }),
                None,
                None,
                None,
            );
        }
        outputs
    }

    /// The bug this exists for: an application started on the second display
    /// re-tiles before it has drawn anything — because its updater window
    /// closed, or because it asked to be maximized — and `Space` cannot say
    /// which display a window with no bounding box is on. Without the
    /// assignment the fallback is the first output, and the application moves
    /// itself to the wrong screen as it finishes loading.
    #[test]
    fn a_window_stays_on_the_display_it_was_placed_on() {
        let outputs = connected();
        assert_eq!(
            pick_output(Some(&outputs[1]), None, &outputs),
            Some(outputs[1].clone())
        );
    }

    /// And it outranks what the space believes, so a client that has managed
    /// to drag its own window across a display boundary is put back rather
    /// than followed.
    #[test]
    fn the_assignment_outranks_where_the_window_currently_overlaps() {
        let outputs = connected();
        assert_eq!(
            pick_output(Some(&outputs[1]), Some(&outputs[0]), &outputs),
            Some(outputs[1].clone())
        );
    }

    /// A display can be unplugged while a window assigned to it is open. The
    /// window has to land somewhere, and the space's own answer is the next
    /// best one.
    #[test]
    fn a_window_assigned_to_a_display_that_is_gone_falls_back() {
        let outputs = connected();
        let unplugged = output("elsewhere");
        assert_eq!(
            pick_output(Some(&unplugged), Some(&outputs[1]), &outputs),
            Some(outputs[1].clone())
        );
        assert_eq!(
            pick_output(Some(&unplugged), None, &outputs),
            Some(outputs[0].clone())
        );
    }

    /// The space's answer goes stale in exactly the same moment: an element
    /// keeps the outputs it overlapped until the next refresh, so right after
    /// a hotplug removal it still names the display that has gone — and a
    /// window tiled onto a display with no geometry left is never tiled at all.
    #[test]
    fn a_display_that_is_gone_is_not_taken_from_the_space_either() {
        let outputs = connected();
        let unplugged = output("elsewhere");
        assert_eq!(known_output(None, Some(&unplugged), &outputs), None);
        assert_eq!(
            pick_output(None, Some(&unplugged), &outputs),
            Some(outputs[0].clone())
        );
    }

    #[test]
    fn with_no_displays_there_is_nowhere_to_put_a_window() {
        assert_eq!(pick_output(None, None, &[]), None);
    }

    /// "Nowhere known" and "the first display" are different answers, and the
    /// callers that ask which display the user is on need to be able to tell
    /// them apart — a guess there would shut out the pointer, which knows
    /// better.
    #[test]
    fn an_unplaced_window_belongs_to_no_display_rather_than_the_first_one() {
        let outputs = connected();
        assert_eq!(known_output(None, None, &outputs), None);
        assert_eq!(pick_output(None, None, &outputs), Some(outputs[0].clone()));
    }

    /// The whole point of the state set: a configure that carries a size but
    /// no `Maximized` leaves the size advisory, and a client is free to map at
    /// its own remembered geometry — half of it hanging off the output until
    /// the user maximizes it by hand.
    #[test]
    fn a_tiled_window_is_told_its_size_is_binding() {
        let mut states = ToplevelStateSet::default();
        set_maximized_states(&mut states);
        assert!(states.contains(xdg_toplevel::State::Maximized));
    }

    #[test]
    fn a_tiled_window_is_tiled_on_every_edge() {
        let mut states = ToplevelStateSet::default();
        set_maximized_states(&mut states);
        for edge in [
            xdg_toplevel::State::TiledLeft,
            xdg_toplevel::State::TiledRight,
            xdg_toplevel::State::TiledTop,
            xdg_toplevel::State::TiledBottom,
        ] {
            assert!(states.contains(edge), "{edge:?} missing");
        }
    }

    /// Re-tiling a window that is already maximized must not look like a
    /// change, or every relayout would send a redundant configure.
    #[test]
    fn setting_the_states_twice_changes_nothing() {
        let mut once = ToplevelStateSet::default();
        set_maximized_states(&mut once);
        let mut twice = once.clone();
        set_maximized_states(&mut twice);
        assert_eq!(once, twice);
    }

    #[test]
    fn parses_transforms() {
        assert_eq!(parse_transform("normal"), Some(Transform::Normal));
        assert_eq!(parse_transform("90"), Some(Transform::_90));
        assert_eq!(parse_transform("flipped-180"), Some(Transform::Flipped180));
        assert_eq!(parse_transform("sideways"), None);
    }

    /// Three displays in the order they were plugged in, and a space to lay
    /// them out in.
    ///
    /// Named after nothing on anybody's desk: the arrangement is a list of
    /// whatever the backend announced, and a test written around one
    /// developer's connectors is how a dependency on their hardware gets in.
    fn arranged() -> (OutputManager, Space<Window>, Vec<Output>) {
        let outputs = vec![output("one"), output("two"), output("three")];
        for out in &outputs {
            out.change_current_state(
                Some(Mode {
                    size: (1920, 1080).into(),
                    refresh: 60_000,
                }),
                None,
                None,
                None,
            );
        }
        let mut space = Space::default();
        let mut manager = OutputManager::new();
        let config = Config::default();
        for out in &outputs {
            manager.add_output(&mut space, out, &config);
        }
        (manager, space, outputs)
    }

    /// The names of the displays as they are laid out, for reading an
    /// arrangement off in one line.
    fn order_of(manager: &OutputManager, config: &Config) -> Vec<String> {
        manager
            .placed(config)
            .iter()
            .map(|output| output.name())
            .collect()
    }

    /// Displays are laid out in the order they arrived until somebody says
    /// otherwise, and moving one to a place trades it with the display
    /// standing there.
    #[test]
    fn moving_a_display_trades_it_with_the_one_it_displaces() {
        let (mut manager, mut space, outputs) = arranged();
        let config = Config::default();
        assert_eq!(order_of(&manager, &config), ["one", "two", "three"]);

        // The third screen becomes the first, and the first takes its place.
        assert!(manager.set_place(&mut space, &outputs[2], 0, &config));
        assert_eq!(order_of(&manager, &config), ["three", "two", "one"]);

        // Which makes it its own undo, and puts every window back where it was
        // without the user having to work out what they did.
        assert!(manager.set_place(&mut space, &outputs[2], 2, &config));
        assert_eq!(order_of(&manager, &config), ["one", "two", "three"]);
    }

    /// A display already where it was asked to be, and a place that is not one,
    /// both change nothing — and say so, because the caller republishes the
    /// arrangement on a `true` and would otherwise do it for every press.
    #[test]
    fn a_move_that_moves_nothing_is_not_a_move() {
        let (mut manager, mut space, outputs) = arranged();
        let config = Config::default();

        assert!(!manager.set_place(&mut space, &outputs[1], 1, &config));
        assert!(!manager.set_place(&mut space, &outputs[0], 3, &config));
        assert!(!manager.set_place(&mut space, &output("elsewhere"), 0, &config));
        assert_eq!(order_of(&manager, &config), ["one", "two", "three"]);
    }

    /// What the shell relies on when it restores a whole arrangement: sending
    /// one move per display, in ascending place, arrives at exactly that order
    /// however scrambled the displays started out.
    ///
    /// Each move fixes one display, and can only disturb a display standing in
    /// the place it was asked for — which no later move names. So walking the
    /// wanted order from the front is a selection sort with the compositor
    /// doing the swapping.
    #[test]
    fn an_arrangement_can_be_restored_one_display_at_a_time() {
        let config = Config::default();
        for wanted in [
            ["three", "one", "two"],
            ["two", "three", "one"],
            ["three", "two", "one"],
            ["one", "two", "three"],
        ] {
            let (mut manager, mut space, outputs) = arranged();
            for (place, name) in wanted.iter().enumerate() {
                let output = outputs
                    .iter()
                    .find(|output| output.name() == *name)
                    .expect("a display of that name");
                manager.set_place(&mut space, output, place, &config);
            }
            assert_eq!(order_of(&manager, &config), wanted, "restoring {wanted:?}");
        }
    }

    /// A display pinned to a position by the config takes no part in the
    /// arrangement, because the arrangement does not decide where it is. It is
    /// left out of the list rather than given a place it does not have.
    #[test]
    fn a_pinned_display_is_not_in_the_arrangement() {
        let (mut manager, mut space, outputs) = arranged();
        let config = Config {
            outputs: vec![crate::config::OutputConfig {
                name: "two".into(),
                position: Some([0, 2160]),
                ..Default::default()
            }],
            ..Default::default()
        };

        assert_eq!(order_of(&manager, &config), ["one", "three"]);
        // And the places are places in that list: putting the third screen
        // first trades it with the first, not with the pinned one between them.
        assert!(manager.set_place(&mut space, &outputs[2], 0, &config));
        assert_eq!(order_of(&manager, &config), ["three", "one"]);
        assert!(!manager.set_place(&mut space, &outputs[1], 0, &config));
    }

    /// Mirrored displays all show the same region from the same origin, so
    /// there is no first screen for one of them to be moved to.
    #[test]
    fn mirrored_displays_have_no_order() {
        let (mut manager, mut space, outputs) = arranged();
        let config = Config {
            general: crate::config::General {
                output_layout: OutputLayout::Mirror,
                ..Default::default()
            },
            ..Default::default()
        };

        assert!(manager.placed(&config).is_empty());
        assert!(!manager.set_place(&mut space, &outputs[2], 0, &config));
    }
}
