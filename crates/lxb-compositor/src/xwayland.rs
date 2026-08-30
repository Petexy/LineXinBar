//! XWayland server/window-manager integration.

use std::cell::Cell;
use std::os::fd::OwnedFd;

use smithay::desktop::space::{RenderZindex, SpaceElement};
use smithay::desktop::Window;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Rectangle};
use smithay::wayland::selection::data_device::{
    clear_data_device_selection, current_data_device_selection_userdata,
    request_data_device_client_selection, set_data_device_selection,
};
use smithay::wayland::selection::primary_selection::{
    clear_primary_selection, current_primary_selection_userdata, request_primary_client_selection,
    set_primary_selection,
};
use smithay::wayland::selection::SelectionTarget;
use smithay::wayland::xwayland_keyboard_grab::XWaylandKeyboardGrabHandler;
use smithay::wayland::xwayland_shell::{XWaylandShellHandler, XWaylandShellState};
use smithay::xwayland::xwm::{Reorder, ResizeEdge, WmWindowProperty, XwmId};
use smithay::xwayland::{X11Surface, X11Wm, XwmHandler};
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{Atom, AtomEnum, ConnectionExt, InputFocus, PropMode};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use crate::focus::{x11_surface_matches, KeyboardFocusTarget};
use crate::input::{window_accepts_keyboard_focus, window_is_x11_chrome};
use crate::outputs::remap_window_preserving_stack;
use crate::state::LxbState;

/// Last client-requested X11 geometry, kept separately from the fullscreen
/// geometry LineXinBar configures for application windows. A client can change
/// `_NET_WM_WINDOW_TYPE` after mapping; if it becomes a notification/menu we
/// can then restore its intended transient size instead of leaving a
/// fullscreen non-focusable blocker above the application.
#[derive(Debug)]
struct X11ClientGeometry(Cell<Rectangle<i32, Logical>>);

#[derive(Debug)]
struct X11FocusEligibility(Cell<bool>);

/// Small read-only X11 connection used to expose the distinction Smithay 0.7
/// keeps private: `WM_HINTS input=false` means either "never focus" or the
/// valid globally-active model when paired with `WM_TAKE_FOCUS`.
pub(crate) struct X11FocusProbe {
    connection: RustConnection,
    /// The X screen's root window, which pointer positions are relative to.
    root: u32,
    wm_protocols: Atom,
    wm_take_focus: Atom,
    net_active_window: Atom,
}

impl X11FocusProbe {
    pub(crate) fn connect(display: &str) -> anyhow::Result<Self> {
        let (connection, screen) = x11rb::connect(Some(display))?;
        let root = connection
            .setup()
            .roots
            .get(screen)
            .map(|screen| screen.root)
            .ok_or_else(|| anyhow::anyhow!("XWayland reported no screen {screen}"))?;
        let wm_protocols = connection
            .intern_atom(false, b"WM_PROTOCOLS")?
            .reply()?
            .atom;
        let wm_take_focus = connection
            .intern_atom(false, b"WM_TAKE_FOCUS")?
            .reply()?
            .atom;
        let net_active_window = connection
            .intern_atom(false, b"_NET_ACTIVE_WINDOW")?
            .reply()?
            .atom;
        Ok(Self {
            connection,
            root,
            wm_protocols,
            wm_take_focus,
            net_active_window,
        })
    }

    /// Say which window the session considers active, in the property the
    /// window manager is expected to keep for it.
    ///
    /// This is the answer a great many X clients actually read, and the one
    /// this session was getting wrong. Focus is not enough for them: a window
    /// declaring ICCCM's globally active model — which is every window Wine
    /// makes, and so every game under Proton — is not activated by being given
    /// the X input focus, and answers `WM_TAKE_FOCUS` only when it feels like
    /// it. `_NET_ACTIVE_WINDOW` is what it goes back to, and a window that does
    /// not find itself named there concludes it is in the background whatever
    /// else it has been given.
    ///
    /// What that looked like, measured on a real session: a game brought back
    /// to the front stayed parked in Wine's message wait at 1.5% of a core with
    /// the X input focus on it, `_NET_WM_STATE_FOCUSED` set and every pointer
    /// motion arriving at the X server. It threw away every one of them,
    /// because motion is what an inactive application drops — and it woke on
    /// the first *click*, which is the one event that activates a window
    /// through `WM_MOUSEACTIVATE` and is then swallowed by having done so. From
    /// the user's chair: nothing works until you click twice. Naming the window
    /// here took it from 5 CPU ticks in three seconds to 224, with the cursor
    /// following the pointer again.
    ///
    /// Nothing is written unless it changed. This is asked wherever activation
    /// is worked out, which is several times a second on a session that is
    /// merely animating.
    pub(crate) fn set_active_window(&self, window: u32) -> Result<(), String> {
        self.connection
            .change_property32(
                PropMode::REPLACE,
                self.root,
                self.net_active_window,
                AtomEnum::WINDOW,
                &[window],
            )
            .map_err(|err| err.to_string())?
            .check()
            .map_err(|err| err.to_string())?;
        self.connection.flush().map_err(|err| err.to_string())
    }

    /// Where XWayland believes its pointer is, in root coordinates.
    ///
    /// Normally this is only ever an echo of the Wayland pointer, because
    /// XWayland moves its own in response to the motion the compositor sends
    /// it. It stops being an echo when an X client synthesises input — XTEST,
    /// which is how Steam drives the Steam Controller's trackpad once it has
    /// claimed the pad. Those events never reach the compositor at all
    /// (measured: a flick moves no evdev device on the machine), so this is
    /// the only place that motion can be observed from.
    pub(crate) fn pointer_position(&self) -> Option<(i16, i16)> {
        let reply = self
            .connection
            .query_pointer(self.root)
            .ok()?
            .reply()
            .ok()?;
        // `same_screen` is false when the pointer is off this screen entirely,
        // and the coordinates are then meaningless rather than merely stale.
        reply.same_screen.then_some((reply.root_x, reply.root_y))
    }

    /// Which window the X server itself believes has the keyboard.
    ///
    /// Not the same question as which window *this compositor* focused, and the
    /// gap between the two answers is the point. Smithay sets the X input focus
    /// as a side effect of the keyboard entering an `X11Surface`, but an X
    /// client may call `XSetInputFocus` itself at any time, and nothing tells
    /// the compositor when one does. A game that maps two windows and focuses
    /// the one the compositor did not is invisible from the Wayland side alone:
    /// the compositor goes on believing the window it focused still has the
    /// keyboard, and its own equality check then skips re-focusing it.
    ///
    /// `None` where the connection failed rather than where nothing is focused;
    /// X reports "no window" as a window of its own, which is passed through as
    /// it stands so the caller can tell the two apart.
    pub(crate) fn focused_window(&self) -> Option<u32> {
        Some(self.connection.get_input_focus().ok()?.reply().ok()?.focus)
    }

    /// Give the keyboard to `window` at the X level.
    ///
    /// A plain client request rather than anything a window manager is
    /// privileged to do, which is why this connection may make it at all. See
    /// [`LxbState::check_xwayland_focus`] for when it is right to.
    /// Checked rather than fired and forgotten. X reports a focus request it
    /// refused — a window that is not viewable is the one that matters — as an
    /// asynchronous error, so an unchecked request comes back `Ok` from a
    /// connection that is about to throw the error away, and the focus quietly
    /// stays where it was.
    pub(crate) fn take_input_focus(&self, window: u32) -> Result<(), String> {
        self.connection
            .set_input_focus(InputFocus::PARENT, window, x11rb::CURRENT_TIME)
            .map_err(|err| err.to_string())?
            .check()
            .map_err(|err| err.to_string())
    }

    /// Whether `window` is `ancestor` or sits somewhere beneath it.
    ///
    /// The X input focus names a window, not a top-level, and a toolkit is
    /// entitled to put it on a child of the one this compositor manages —
    /// Valve's client does exactly that, focusing the Chromium window inside
    /// each of its frames. Comparing ids alone reads every one of those as the
    /// keyboard having gone astray.
    ///
    /// The walk is bounded rather than trusted to terminate. It climbs a tree
    /// another process is free to reparent underneath it, and a cycle or a
    /// pathological depth would otherwise hang the compositor's event loop.
    fn contains(&self, ancestor: u32, window: u32) -> bool {
        let mut current = window;
        for _ in 0..32 {
            if current == ancestor {
                return true;
            }
            if current == x11rb::NONE {
                return false;
            }
            let parent = self
                .connection
                .query_tree(current)
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .map(|reply| reply.parent);
            match parent {
                // A window that has gone between the focus query and this walk
                // is not evidence of anything.
                None => return true,
                Some(parent) => current = parent,
            }
        }
        false
    }

    fn supports_take_focus(&self, window: u32) -> anyhow::Result<bool> {
        let reply = self
            .connection
            .get_property(false, window, self.wm_protocols, AtomEnum::ATOM, 0, 64)?
            .reply()?;
        Ok(reply
            .value32()
            .is_some_and(|mut atoms| atoms.any(|atom| atom == self.wm_take_focus)))
    }
}

pub(crate) fn x11_window_accepts_input(window: &Window) -> bool {
    let Some(surface) = window.x11_surface() else {
        return true;
    };
    if !surface
        .hints()
        .is_some_and(|hints| hints.input == Some(false))
    {
        return true;
    }
    window
        .user_data()
        .get::<X11FocusEligibility>()
        .is_some_and(|eligibility| eligibility.0.get())
}

pub(crate) fn remember_x11_client_geometry(window: &Window, geometry: Rectangle<i32, Logical>) {
    window
        .user_data()
        .get_or_insert(|| X11ClientGeometry(Cell::new(geometry)))
        .0
        .set(geometry);
}

fn remembered_x11_client_geometry(window: &Window) -> Option<Rectangle<i32, Logical>> {
    window
        .user_data()
        .get::<X11ClientGeometry>()
        .map(|geometry| geometry.0.get())
}

impl LxbState {
    fn refresh_x11_focus_eligibility(&self, window: &Window) {
        let Some(surface) = window.x11_surface() else {
            return;
        };
        let eligible = if surface
            .hints()
            .is_some_and(|hints| hints.input == Some(false))
        {
            self.lxb
                .x11_focus_probe
                .as_ref()
                .and_then(
                    |probe| match probe.supports_take_focus(surface.window_id()) {
                        Ok(supported) => Some(supported),
                        Err(err) => {
                            tracing::warn!(
                                ?err,
                                window = surface.window_id(),
                                "failed to inspect X11 WM_TAKE_FOCUS"
                            );
                            None
                        }
                    },
                )
                .unwrap_or(false)
        } else {
            true
        };
        window
            .user_data()
            .get_or_insert(|| X11FocusEligibility(Cell::new(eligible)))
            .0
            .set(eligible);
    }

    fn window_for_x11(&self, surface: &X11Surface) -> Option<Window> {
        self.lxb
            .space
            .elements()
            .find(|window| {
                window
                    .x11_surface()
                    .is_some_and(|candidate| x11_surface_matches(candidate, surface))
            })
            .cloned()
    }

    fn retile_x11(&mut self, surface: &X11Surface) {
        if let Some(window) = self.window_for_x11(surface) {
            self.lxb.outputs.tile_window(&mut self.lxb.space, &window);
        }
        self.queue_redraw();
    }

    fn remove_x11_window(&mut self, surface: &X11Surface) {
        if let Some(window) = self.window_for_x11(surface) {
            // While it still has its id — see `toplevel_destroyed`.
            self.lxb
                .restores
                .forget(crate::overview::window_id(&window));
            self.lxb.space.unmap_elem(&window);
            self.lxb.outputs.relayout_windows(&mut self.lxb.space);
        }
        self.focus_topmost_window();
        self.queue_redraw();
    }

    /// Raise a compositor window and keep XWayland's frame stack/EWMH state in
    /// the same order. Smithay deliberately leaves this synchronization to the
    /// compositor.
    /// Say so when the X server's idea of keyboard focus has drifted from ours.
    ///
    /// Polled beside the pointer, and for the same reason: there is nothing to
    /// subscribe to. An X client taking focus for itself is handled inside the
    /// X server, and the compositor is told nothing — so the only way to notice
    /// is to ask.
    ///
    /// Where nobody holds it at all, the focus is also taken back, and that is
    /// the case this exists to repair. A window declaring `WM_HINTS input =
    /// false` while listing `WM_TAKE_FOCUS` is asking for ICCCM's *globally
    /// active* model: the window manager is expressly not to set the focus
    /// itself, but to send `WM_TAKE_FOCUS` and let the client set it. Smithay
    /// honours that to the letter and sets no focus. A client that then never
    /// answers the message leaves the X server focused on nothing, and every
    /// key the user presses is delivered nowhere — the window is on screen,
    /// drawing, believed focused by this compositor and deaf.
    ///
    /// That is not hypothetical: it is every Unreal Engine game under Proton.
    /// The message Smithay sends carries `CurrentTime`, and ICCCM requires
    /// `WM_TAKE_FOCUS` to carry a real timestamp, which is reason enough for a
    /// client to drop it.
    ///
    /// Only when the focus is *nothing*, though. A different window holding it
    /// is reported and left alone: that is a client moving focus between its
    /// own windows — a game and its launcher, an application and its dialog —
    /// and a compositor that forced its own belief back every tick would fight
    /// it. The poll interval is also the client's chance to answer first, so a
    /// well-behaved one is never overridden.
    ///
    /// Logged only when the pair changes, because it is polled several times a
    /// second and a session sitting in a steady disagreement would otherwise
    /// fill the journal with one line per tick.
    pub(crate) fn check_xwayland_focus(&mut self) {
        let Some(actual) = self
            .lxb
            .x11_focus_probe
            .as_ref()
            .and_then(X11FocusProbe::focused_window)
        else {
            return;
        };
        let believed = self
            .lxb
            .seat
            .get_keyboard()
            .and_then(|keyboard| keyboard.current_focus())
            .and_then(|focus| match focus {
                KeyboardFocusTarget::X11(surface) => Some(surface),
                KeyboardFocusTarget::Wayland(_) => None,
            });
        // Nothing to disagree about: the keyboard is on a Wayland window, and
        // where X points its own focus while no X client holds it is X's
        // business.
        let Some(believed) = believed else {
            self.lxb.last_x11_focus_drift = None;
            return;
        };
        if believed.window_id() == actual
            || self
                .lxb
                .x11_focus_probe
                .as_ref()
                .is_some_and(|probe| probe.contains(believed.window_id(), actual))
        {
            self.lxb.last_x11_focus_drift = None;
            return;
        }
        let drift = (believed.window_id(), actual);
        if self.lxb.last_x11_focus_drift == Some(drift) {
            return;
        }
        self.lxb.last_x11_focus_drift = Some(drift);

        // `None`, rather than some other window: nobody is being interrupted by
        // taking it, and the alternative is a keyboard that reaches no window
        // at all.
        if actual == x11rb::NONE {
            let taken = self
                .lxb
                .x11_focus_probe
                .as_ref()
                .map(|probe| probe.take_input_focus(believed.window_id()));
            match taken {
                Some(Ok(())) => tracing::info!(
                    window = believed.window_id(),
                    title = believed.title(),
                    class = believed.class(),
                    "no X11 window held the keyboard; gave it to the focused one"
                ),
                Some(Err(err)) => tracing::warn!(
                    window = believed.window_id(),
                    title = believed.title(),
                    class = believed.class(),
                    %err,
                    "no X11 window holds the keyboard and it could not be given away"
                ),
                None => {}
            }
            return;
        }

        tracing::warn!(
            focused = believed.window_id(),
            title = believed.title(),
            class = believed.class(),
            holds_the_keyboard = actual,
            "an X11 window other than the focused one holds the X input focus; \
             keys are reaching it rather than the window on screen"
        );
    }

    pub(crate) fn raise_window(&mut self, window: &Window, activate: bool) {
        // X owns the stacking order of override-redirect windows. Moving one
        // in Space without a corresponding X request makes hit testing and
        // rendering disagree with the client-visible stack.
        if window
            .x11_surface()
            .is_some_and(|surface| surface.is_override_redirect())
        {
            if activate {
                // Preserve X's OR stack while still keeping the compositor's
                // activation state and _NET_WM_STATE_FOCUSED coherent.
                let windows: Vec<Window> = self.lxb.space.elements().cloned().collect();
                for candidate in windows {
                    candidate.set_activate(&candidate == window);
                }
            }
            return;
        }
        self.lxb.space.raise_element(window, activate);
        self.sync_x11_raise(window);
    }

    /// Synchronize an already-performed compositor raise with XWayland.
    pub(crate) fn sync_x11_raise(&mut self, window: &Window) {
        let Some(surface) = window.x11_surface().cloned() else {
            return;
        };
        // Override-redirect windows own their X stacking and are not members
        // of _NET_CLIENT_LIST_STACKING.
        if surface.is_override_redirect() {
            return;
        }
        let Some(xwm) = self.lxb.xwm.as_mut() else {
            return;
        };
        if surface.xwm_id() != Some(xwm.id()) {
            return;
        }
        if let Err(err) = xwm.raise_window(&surface) {
            tracing::warn!(
                ?err,
                window = surface.window_id(),
                "failed to synchronize X11 raise"
            );
        }
    }

    fn restack_override_redirect(&mut self, window: &Window, above: Option<u32>) {
        let mut order: Vec<Window> = self.lxb.space.elements().cloned().collect();
        let Some(old_index) = order.iter().position(|element| element == window) else {
            return;
        };
        let target = order.remove(old_index);

        let insert_at = match above {
            // Space iterates back-to-front. X11's `above_sibling` names the
            // window immediately below this one; NONE means the bottom.
            None => x11_restack_insert_index(None),
            Some(sibling) => order
                .iter()
                .position(|element| {
                    element.x11_surface().is_some_and(|surface| {
                        surface.window_id() == sibling
                            || surface.mapped_window_id() == Some(sibling)
                    })
                })
                .map(|index| x11_restack_insert_index(Some(index)))
                .unwrap_or(old_index.min(order.len())),
        };
        order.insert(insert_at, target);

        for element in order {
            self.lxb.space.raise_element(&element, false);
        }
    }
}

fn x11_restack_insert_index(above_sibling_index: Option<usize>) -> usize {
    above_sibling_index.map_or(0, |index| index + 1)
}

impl XWaylandShellHandler for LxbState {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        &mut self.lxb.xwayland_shell_state
    }

    fn surface_associated(&mut self, _xwm: XwmId, _wl_surface: WlSurface, surface: X11Surface) {
        // Map requests and Wayland-surface association may arrive in either
        // order. Once paired, refresh the desktop wrapper so rendering, frame
        // callbacks and hit testing immediately see the new surface tree.
        if let Some(window) = self.window_for_x11(&surface) {
            window.on_commit();

            // A focus set before wl_surface association did not have a
            // Wayland client for data-device focus. Refresh only that existing
            // target; a late association must never steal focus from a newer
            // window (or from the shell).
            let already_focused = self
                .lxb
                .seat
                .get_keyboard()
                .and_then(|keyboard| keyboard.current_focus())
                .is_some_and(|focus| focus == KeyboardFocusTarget::X11(surface.clone()));
            if already_focused {
                self.set_keyboard_focus(None);
                self.set_window_keyboard_focus(&window);
            }
        }
        self.queue_redraw();
    }
}

impl XWaylandKeyboardGrabHandler for LxbState {
    fn keyboard_focus_for_xsurface(&self, surface: &WlSurface) -> Option<KeyboardFocusTarget> {
        self.lxb
            .window_for_surface(surface)
            .and_then(|window| window.x11_surface().cloned())
            .map(KeyboardFocusTarget::X11)
    }
}

impl XwmHandler for LxbState {
    fn xwm_state(&mut self, _xwm: XwmId) -> &mut X11Wm {
        self.lxb
            .xwm
            .as_mut()
            .expect("XWM event delivered before XWM initialization")
    }

    fn new_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn new_override_redirect_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn map_window_request(&mut self, _xwm: XwmId, surface: X11Surface) {
        if let Err(err) = surface.set_mapped(true) {
            tracing::warn!(
                ?err,
                window = surface.window_id(),
                "failed to map X11 window"
            );
            return;
        }

        let window = self
            .window_for_x11(&surface)
            .unwrap_or_else(|| Window::new_x11_window(surface.clone()));
        self.refresh_x11_focus_eligibility(&window);
        self.map_new_window(window);
        tracing::info!(
            window = surface.window_id(),
            title = surface.title(),
            class = surface.class(),
            "mapped X11 toplevel"
        );
    }

    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, surface: X11Surface) {
        let geometry = surface.geometry();
        // A client may change override_redirect between MapRequest and
        // MapNotify. Reuse the managed wrapper if one already exists so the
        // same X window is never rendered and focused twice.
        let window = self
            .window_for_x11(&surface)
            .unwrap_or_else(|| Window::new_x11_window(surface.clone()));
        self.refresh_x11_focus_eligibility(&window);
        // Menus/tooltips stay above application windows, while focusable
        // override-redirect games participate in the normal application
        // stack. Keeping every OR window in Overlay would make such a game
        // permanently cover a managed app even after focus was cycled.
        let z = if window_is_x11_chrome(&window) {
            RenderZindex::Overlay
        } else {
            RenderZindex::Shell
        };
        window.override_z_index(z as u8);
        remap_window_preserving_stack(&mut self.lxb.space, &window, geometry.loc);
        if self.lxb.takes_the_keyboard(&window) {
            self.raise_window(&window, true);
            self.set_window_keyboard_focus(&window);
        }
        tracing::debug!(window = surface.window_id(), ?geometry, "mapped X11 popup");
        self.queue_redraw();
    }

    fn unmapped_window(&mut self, _xwm: XwmId, surface: X11Surface) {
        self.remove_x11_window(&surface);
        if !surface.is_override_redirect() {
            if let Err(err) = surface.set_mapped(false) {
                tracing::warn!(
                    ?err,
                    window = surface.window_id(),
                    "failed to unmap X11 window"
                );
            }
        }
    }

    fn destroyed_window(&mut self, _xwm: XwmId, surface: X11Surface) {
        self.remove_x11_window(&surface);
    }

    fn configure_request(
        &mut self,
        _xwm: XwmId,
        surface: X11Surface,
        x: Option<i32>,
        y: Option<i32>,
        width: Option<u32>,
        height: Option<u32>,
        _reorder: Option<Reorder>,
    ) {
        let mut geometry = surface.geometry();
        geometry.loc.x = x.unwrap_or(geometry.loc.x);
        geometry.loc.y = y.unwrap_or(geometry.loc.y);
        geometry.size.w = width.map(|value| value as i32).unwrap_or(geometry.size.w);
        geometry.size.h = height.map(|value| value as i32).unwrap_or(geometry.size.h);

        if !surface.is_override_redirect() {
            if let Some(window) = self.window_for_x11(&surface) {
                remember_x11_client_geometry(&window, geometry);
                if !window_is_x11_chrome(&window) {
                    // LineXinBar's regular application windows are always
                    // output-sized. Managed X11 chrome keeps client geometry.
                    self.retile_x11(&surface);
                    return;
                }
            }
        }

        if let Err(err) = surface.configure(geometry) {
            tracing::warn!(
                ?err,
                window = surface.window_id(),
                "failed to configure X11 window"
            );
        }
    }

    fn configure_notify(
        &mut self,
        _xwm: XwmId,
        surface: X11Surface,
        geometry: Rectangle<i32, Logical>,
        above: Option<u32>,
    ) {
        let Some(window) = self.window_for_x11(&surface) else {
            return;
        };

        // An application window is where LineXinBar put it, not where the client
        // has just moved itself. X11 toolkits routinely restore a remembered
        // position once they finish starting — which is how an application
        // launched on the second display walks over to the first one — and
        // following that here would also leave the window mapped somewhere the
        // configure it was sent says it is not. Put it back instead.
        if !surface.is_override_redirect() && !window_is_x11_chrome(&window) {
            let placed = self.lxb.space.element_location(&window);
            if placed != Some(geometry.loc) {
                tracing::debug!(
                    window = surface.window_id(),
                    moved_to = ?geometry.loc,
                    belongs_at = ?placed,
                    "X11 application moved itself; re-tiling it"
                );
                self.retile_x11(&surface);
            }
            return;
        }

        remap_window_preserving_stack(&mut self.lxb.space, &window, geometry.loc);
        if surface.is_override_redirect() {
            self.restack_override_redirect(&window, above);
        }
        self.queue_redraw();
    }

    fn property_notify(&mut self, _xwm: XwmId, surface: X11Surface, property: WmWindowProperty) {
        // Everything that can change what kind of window this is. The window
        // type has always been here; the other three are the ones a window
        // that arrived saying nothing about itself can say something with —
        // see [`crate::input::x11_window_says_nothing`]. A client is entitled
        // to create a window, map it, and only then name itself, and one that
        // does must become an application here rather than be left as the
        // furniture it looked like for its first frame.
        let says_what_it_is = matches!(
            property,
            WmWindowProperty::Title
                | WmWindowProperty::Class
                | WmWindowProperty::Pid
                | WmWindowProperty::Hints
                | WmWindowProperty::WindowType
        );
        if !says_what_it_is && property != WmWindowProperty::Protocols {
            return;
        }

        let window = self.window_for_x11(&surface);
        if let Some(window) = &window {
            self.refresh_x11_focus_eligibility(window);
            if says_what_it_is {
                if !window_is_x11_chrome(window) {
                    window.override_z_index(RenderZindex::Shell as u8);
                    self.lxb.outputs.tile_window(&mut self.lxb.space, window);
                } else {
                    window.override_z_index(RenderZindex::Overlay as u8);
                    let geometry = remembered_x11_client_geometry(window)
                        .unwrap_or_else(|| surface.geometry());
                    if let Err(err) = surface.configure(geometry) {
                        tracing::warn!(
                            ?err,
                            window = surface.window_id(),
                            "failed to restore X11 chrome geometry"
                        );
                    }
                    remap_window_preserving_stack(&mut self.lxb.space, window, geometry.loc);
                    self.raise_window(window, false);
                }
            }
        }

        let currently_focused = self
            .lxb
            .seat
            .get_keyboard()
            .and_then(|keyboard| keyboard.current_focus())
            .is_some_and(|focus| focus == KeyboardFocusTarget::X11(surface.clone()));
        if currently_focused && window.is_some_and(|window| !window_accepts_keyboard_focus(&window))
        {
            self.focus_topmost_window();
        }
        self.queue_redraw();
    }

    fn maximize_request(&mut self, _xwm: XwmId, surface: X11Surface) {
        // Already true of every window here; re-tiling states it again.
        self.retile_x11(&surface);
    }

    fn unmaximize_request(&mut self, _xwm: XwmId, surface: X11Surface) {
        // Refused, as on the Wayland side: application windows are always
        // maximized to their output. `retile_x11` re-asserts both the state
        // and the geometry.
        self.retile_x11(&surface);
    }

    fn fullscreen_request(&mut self, _xwm: XwmId, surface: X11Surface) {
        let _ = surface.set_fullscreen(true);
        self.retile_x11(&surface);
    }

    fn unfullscreen_request(&mut self, _xwm: XwmId, surface: X11Surface) {
        let _ = surface.set_fullscreen(false);
        self.retile_x11(&surface);
    }

    fn resize_request(
        &mut self,
        _xwm: XwmId,
        surface: X11Surface,
        _button: u32,
        _edge: ResizeEdge,
    ) {
        self.retile_x11(&surface);
    }

    fn move_request(&mut self, _xwm: XwmId, surface: X11Surface, _button: u32) {
        self.retile_x11(&surface);
    }

    fn allow_selection_access(&mut self, xwm: XwmId, _selection: SelectionTarget) -> bool {
        self.lxb
            .seat
            .get_keyboard()
            .and_then(|keyboard| keyboard.current_focus())
            .is_some_and(|focus| {
                matches!(focus, KeyboardFocusTarget::X11(surface) if surface.xwm_id() == Some(xwm))
            })
    }

    fn send_selection(
        &mut self,
        _xwm: XwmId,
        selection: SelectionTarget,
        mime_type: String,
        fd: OwnedFd,
    ) {
        match selection {
            SelectionTarget::Clipboard => {
                if let Err(err) =
                    request_data_device_client_selection(&self.lxb.seat, mime_type, fd)
                {
                    tracing::warn!(?err, "failed to send Wayland clipboard to X11");
                }
            }
            SelectionTarget::Primary => {
                if let Err(err) = request_primary_client_selection(&self.lxb.seat, mime_type, fd) {
                    tracing::warn!(?err, "failed to send Wayland primary selection to X11");
                }
            }
        }
    }

    fn new_selection(&mut self, _xwm: XwmId, selection: SelectionTarget, mime_types: Vec<String>) {
        match selection {
            SelectionTarget::Clipboard => {
                set_data_device_selection(&self.lxb.display_handle, &self.lxb.seat, mime_types, ())
            }
            SelectionTarget::Primary => {
                set_primary_selection(&self.lxb.display_handle, &self.lxb.seat, mime_types, ())
            }
        }
    }

    fn cleared_selection(&mut self, _xwm: XwmId, selection: SelectionTarget) {
        match selection {
            SelectionTarget::Clipboard
                if current_data_device_selection_userdata(&self.lxb.seat).is_some() =>
            {
                clear_data_device_selection(&self.lxb.display_handle, &self.lxb.seat)
            }
            SelectionTarget::Primary
                if current_primary_selection_userdata(&self.lxb.seat).is_some() =>
            {
                clear_primary_selection(&self.lxb.display_handle, &self.lxb.seat)
            }
            _ => {}
        }
    }

    fn disconnected(&mut self, xwm: XwmId) {
        if self.lxb.xwm.as_ref().map(X11Wm::id) != Some(xwm) {
            return;
        }

        // Drop the dead connection and every surface owned by it. Otherwise
        // future launches would inherit a dead DISPLAY and focus could remain
        // pinned to a frozen X11 window.
        self.lxb.xwm = None;
        self.lxb.x11_focus_probe = None;
        self.lxb.xwayland_display = None;
        self.lxb.xwayland_ready = false;

        let windows: Vec<Window> = self
            .lxb
            .space
            .elements()
            .filter(|window| {
                window
                    .x11_surface()
                    .is_some_and(|surface| surface.xwm_id() == Some(xwm))
            })
            .cloned()
            .collect();
        for window in windows {
            self.lxb.space.unmap_elem(&window);
        }
        self.lxb.outputs.relayout_windows(&mut self.lxb.space);
        self.focus_topmost_window();
        self.queue_redraw();

        // The shell was started with this X server's private DISPLAY and
        // cannot have its process environment rewritten. Continuing would
        // make every later X11 launch target a dead socket. End the session so
        // the outer supervisor (or nested development script) can restart the
        // compositor, XWayland and shell as one coherent display boundary.
        tracing::error!(
            xwm = ?xwm,
            "XWayland connection closed; ending session to avoid stale DISPLAY launches"
        );
        self.lxb.fatal_error = Some(
            "private XWayland connection closed; restart the complete LineXinBar session".into(),
        );
        self.lxb.running = false;
        self.lxb.loop_signal.stop();
    }
}

smithay::delegate_xwayland_shell!(LxbState);
smithay::delegate_xwayland_keyboard_grab!(LxbState);

#[cfg(test)]
mod tests {
    use super::x11_restack_insert_index;

    #[test]
    fn x11_restack_order_is_back_to_front() {
        assert_eq!(x11_restack_insert_index(None), 0);
        assert_eq!(x11_restack_insert_index(Some(0)), 1);
        assert_eq!(x11_restack_insert_index(Some(3)), 4);
    }
}
