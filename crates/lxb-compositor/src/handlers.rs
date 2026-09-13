//! Wayland protocol handler implementations for [`LxbState`].

use std::os::unix::io::OwnedFd;

use smithay::backend::renderer::sync::Fence;
use smithay::backend::renderer::utils::on_commit_buffer_handler;
use smithay::desktop::{
    find_popup_root_surface, get_popup_toplevel_coords, layer_map_for_output,
    LayerSurface as DesktopLayerSurface, PopupKind, Window,
};
use smithay::input::pointer::{CursorImageStatus, PointerHandle};
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::output::Output;
use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::protocol::wl_seat::WlSeat;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, Resource};
use smithay::utils::{Logical, Point, Serial};
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{
    add_blocker, add_pre_commit_hook, get_parent, is_sync_subsurface, with_states,
    CompositorClientState, CompositorHandler, CompositorState,
};
use smithay::wayland::dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier};
use smithay::wayland::drm_syncobj::{DrmSyncobjCachedState, DrmSyncobjHandler, DrmSyncobjState};
use smithay::wayland::fractional_scale::FractionalScaleHandler;
use smithay::wayland::output::OutputHandler;
use smithay::wayland::pointer_constraints::{with_pointer_constraint, PointerConstraintsHandler};
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::selection::data_device::{
    set_data_device_focus, ClientDndGrabHandler, DataDeviceHandler, DataDeviceState,
    ServerDndGrabHandler,
};
use smithay::wayland::selection::primary_selection::{
    set_primary_focus, PrimarySelectionHandler, PrimarySelectionState,
};
use smithay::wayland::selection::{SelectionHandler, SelectionSource, SelectionTarget};
use smithay::wayland::shell::wlr_layer::{
    KeyboardInteractivity, Layer, LayerSurface, LayerSurfaceData, WlrLayerShellHandler,
    WlrLayerShellState,
};
use smithay::wayland::shell::xdg::decoration::XdgDecorationHandler;
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgPopupSurfaceData, XdgShellHandler,
    XdgShellState, XdgToplevelSurfaceData,
};
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::wayland::xdg_activation::{
    XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
};
use smithay::{
    delegate_compositor, delegate_cursor_shape, delegate_data_device, delegate_dmabuf,
    delegate_fractional_scale, delegate_layer_shell, delegate_output, delegate_pointer_constraints,
    delegate_pointer_gestures, delegate_presentation, delegate_primary_selection,
    delegate_relative_pointer, delegate_seat, delegate_shm, delegate_single_pixel_buffer,
    delegate_viewporter, delegate_xdg_activation, delegate_xdg_decoration, delegate_xdg_shell,
};

use crate::focus::KeyboardFocusTarget;
use crate::input::window_is_x11_chrome;
use crate::outputs::{assign_output, remap_window_preserving_stack, set_maximized_states};
use crate::state::{client_compositor_state, LxbState};
use crate::xwayland::remember_x11_client_geometry;

// ---------------------------------------------------------------------------
// wl_compositor
// ---------------------------------------------------------------------------

impl CompositorHandler for LxbState {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.lxb.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        client_compositor_state(client)
    }

    /// Hold a surface's commit until the GPU work behind it has actually
    /// finished.
    ///
    /// A client using explicit synchronisation hands over a buffer together
    /// with an *acquire point*: a place on a timeline that signals once the
    /// rendering into that buffer is done. Nothing about the buffer says so by
    /// itself, so a compositor that reads it as soon as the commit arrives is
    /// reading whatever the GPU happened to have written by then — a frame torn
    /// across its own draw calls.
    ///
    /// The commit is therefore made to wait. Smithay's transaction machinery
    /// takes a blocker per surface and applies the state once every one of them
    /// has cleared, and the kernel wakes us through an eventfd on the timeline
    /// rather than us polling it, so a slow frame costs this compositor no work
    /// at all — it costs the client the frame it was already going to miss.
    ///
    /// Registered per surface as it is created, because a client may start
    /// using sync points at any commit and the hook has to already be there
    /// when it does.
    fn new_surface(&mut self, surface: &WlSurface) {
        add_pre_commit_hook::<Self, _>(surface, |state, _dh, surface| {
            let acquire = with_states(surface, |states| {
                states
                    .cached_state
                    .get::<DrmSyncobjCachedState>()
                    .pending()
                    .acquire_point
                    .clone()
            });
            let Some(acquire) = acquire else {
                return;
            };
            // Already done: taking the slow path here would cost a round trip
            // through the event loop for a frame that is ready to be drawn.
            if acquire.is_signaled() {
                return;
            }
            let Ok((blocker, source)) = acquire.generate_blocker() else {
                // The point cannot be waited on, so the alternative to reading
                // the buffer now is never reading it. A frame that may be torn
                // beats a client that never draws again.
                return;
            };
            let Some(client) = surface.client() else {
                return;
            };
            // Counted, because a commit held here is a frame the client has
            // handed over and cannot have back, and from its own side that is
            // indistinguishable from a compositor that has stopped listening.
            // A game that stops drawing is asked about this: see
            // [`crate::render`]'s quiet-application watch.
            let held_since = std::time::Instant::now();
            let inserted = state
                .lxb
                .loop_handle
                .insert_source(source, move |_, _, state| {
                    state.lxb.blocked_commits = state.lxb.blocked_commits.saturating_sub(1);
                    if state.lxb.blocked_commits == 0 {
                        state.lxb.blocked_since = None;
                    }
                    // A frame's GPU work outlasting a whole second is not a
                    // slow frame, it is a client waiting on something it is not
                    // going to get.
                    if held_since.elapsed() > std::time::Duration::from_secs(1) {
                        tracing::warn!(
                            waited = ?held_since.elapsed(),
                            "a client's frame was held for its own GPU work far longer than a frame"
                        );
                    }
                    let dh = state.lxb.display_handle.clone();
                    state
                        .client_compositor_state(&client)
                        .blocker_cleared(state, &dh);
                    Ok(())
                });
            if let Err(err) = inserted {
                tracing::warn!(?err, "could not wait on a client's acquire point");
                return;
            }
            state.lxb.blocked_commits += 1;
            state.lxb.blocked_since.get_or_insert(held_since);
            add_blocker(surface, blocker);
        });
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);

        // A sync subsurface's state is applied together with its parent, so
        // there is nothing to do until the root commits.
        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            if let Some(window) = self.lxb.window_for_surface(&root) {
                window.on_commit();
                self.announce_window(&window, &root);
                crate::render::drew(&window);
                // And, for a floating window that has just said what shape it
                // wants to be, the layout that acts on the answer — see
                // [`crate::pip::Floating`]. Free for every other window.
                self.settle_a_floating_window(&window);
                // And for one that has just left a corner to fill the display,
                // the frame it has actually become large on, which is the frame
                // the flight out of that corner can start from.
                self.fly_out_of_the_corner(&window);
            }
        }

        self.lxb.popups.commit(surface);

        self.refresh_layer_surfaces(surface);
        // Layer geometry must be arranged from the state applied by this
        // commit before the first configure is sent. `new_layer_surface` runs
        // before the client has committed its anchors and requested size, so
        // configuring first would send Smithay's default half-output geometry.
        self.handle_initial_configure(surface);

        // Something changed on screen. Backends that render on demand need
        // telling; the timer-driven ones ignore it.
        self.queue_redraw();
    }
}

impl LxbState {
    /// Say which window this is, once — the first time it has anything to show.
    ///
    /// Not where the window is mapped, although that is where the shape of it
    /// is logged. A toplevel is created before its client has said a word about
    /// itself, so the line there can only ever name a window that arrived
    /// already titled, which is none of them; by the first buffer both the
    /// `app_id` and the title are set. And the first buffer is the moment worth
    /// recording anyway, because that is when the window starts covering
    /// whatever was on the display before it.
    ///
    /// Which is the question this answers, and it took a game to ask it: an
    /// application under Proton opens a window per Win32 window, this shell
    /// tiles each of them over the whole display, and reading a run afterwards
    /// meant telling those apart by nothing but a geometry they all share.
    fn announce_window(&self, window: &Window, surface: &WlSurface) {
        /// Marker: this window has been named already.
        struct Announced;

        let has_buffer =
            smithay::backend::renderer::utils::with_renderer_surface_state(surface, |state| {
                state.buffer().is_some()
            });
        if has_buffer != Some(true) {
            return;
        }
        if !window.user_data().insert_if_missing(|| Announced) {
            return;
        }
        tracing::info!(
            app_id = crate::shell_control::window_app_id(window),
            title = crate::shell_control::window_title(window),
            geometry = ?self.lxb.space.element_geometry(window),
            "a window is showing"
        );
    }

    /// Send the mandatory first `configure` once a surface has a role and has
    /// committed without a buffer, as required by xdg-shell and layer-shell.
    fn handle_initial_configure(&mut self, surface: &WlSurface) {
        // Popups. A menu stays invisible until it is configured, because a
        // client may not attach a buffer before its first xdg_surface.configure.
        if let Some(PopupKind::Xdg(popup)) = self.lxb.popups.find_popup(surface) {
            let initial_configure_sent = with_states(surface, |states| {
                states
                    .data_map
                    .get::<XdgPopupSurfaceData>()
                    .map(|d| d.lock().unwrap().initial_configure_sent)
                    .unwrap_or(true)
            });
            if !initial_configure_sent {
                // Only fails on a protocol violation by the client, which has
                // already been reported to it.
                if let Err(err) = popup.send_configure() {
                    tracing::warn!(?err, "failed to configure popup");
                }
            }
            // A popup is never a toplevel or a layer surface, and looking it up
            // as one would find its parent window instead.
            return;
        }

        // xdg toplevels.
        if let Some(window) = self.lxb.window_for_surface(surface) {
            if let Some(toplevel) = window.toplevel() {
                let initial_configure_sent = with_states(surface, |states| {
                    states
                        .data_map
                        .get::<XdgToplevelSurfaceData>()
                        .map(|d| d.lock().unwrap().initial_configure_sent)
                        .unwrap_or(true)
                });
                if !initial_configure_sent {
                    toplevel.send_configure();
                }
            }
        }

        // Layer surfaces.
        if let Some(output) = self.output_for_layer_surface(surface) {
            let initial_configure_sent = with_states(surface, |states| {
                states
                    .data_map
                    .get::<LayerSurfaceData>()
                    .map(|d| d.lock().unwrap().initial_configure_sent)
                    .unwrap_or(true)
            });

            if !initial_configure_sent {
                // `refresh_layer_surfaces` arranges the map right after this,
                // so only the lookup is needed here.
                let layer = layer_map_for_output(&output)
                    .layer_for_surface(surface, smithay::desktop::WindowSurfaceType::ALL)
                    .cloned();
                if let Some(layer) = layer {
                    layer.layer_surface().send_configure();
                }
            }
        }
    }

    /// Re-run layer arrangement when a mapped layer surface commits, since its
    /// exclusive zone or keyboard interactivity may have changed.
    fn refresh_layer_surfaces(&mut self, surface: &WlSurface) {
        let Some(output) = self.output_for_layer_surface(surface) else {
            return;
        };

        let rearranged = {
            let mut map = layer_map_for_output(&output);
            map.arrange()
        };
        if rearranged {
            self.lxb.outputs.relayout_windows(&mut self.lxb.space);
        }

        let previous = self.lxb.exclusive_keyboard_focus.clone();
        self.refresh_exclusive_focus();
        // The layer is mapped before its first pending state is committed.
        // In the normal shell path that means `OnDemand` was not visible to
        // `new_layer_surface`, leaving the seat without focus. Re-evaluate when
        // there is no valid target as well as when an exclusive grab changes.
        // Do not do it for every animated commit: a valid OnDemand focus is
        // deliberately preserved.
        if previous != self.lxb.exclusive_keyboard_focus || self.keyboard_focus_needs_refresh() {
            self.focus_topmost_window();
        }
        // The pointer, every time, and not only when the keyboard moved with
        // it. This is the moment the shell's own screen arrives over an
        // application or steps back off it, and what is under a pointer nobody
        // has touched changes with it — see [`LxbState::refresh_pointer_focus`].
        // The two do not always move together: the shell can leave the overlay
        // layer, or shrink its input region to a keyboard's keys, without the
        // seat's exclusive claim changing at all, and the pointer left pointing
        // at a surface that is no longer under it belongs to the shell — so a
        // click meant for the game underneath goes on landing on the start
        // screen until the user moves the mouse. Cheap when nothing changed:
        // this is a walk of the layers and windows under one point, and it
        // returns without sending anything once the focus is already right.
        self.refresh_pointer_focus();
        // And whether each application is still the one on screen. This is the
        // commit that puts the shell's own screen over a display or takes it
        // off again, which is the whole of what decides that — see
        // [`LxbState::refresh_window_activation`].
        self.refresh_window_activation();
    }

    /// Record which layer surface, if any, currently demands exclusive
    /// keyboard focus.
    ///
    /// The session shell wins any argument about this. Its overlay is the
    /// guide — the one surface whose whole job is to be reachable from inside
    /// a fullscreen application — and an application that puts up an exclusive
    /// layer surface of its own must not be able to hold the keyboard against
    /// it. Anything else keeps the first claim found, as before.
    pub fn refresh_exclusive_focus(&mut self) {
        let mut exclusive = None;
        let mut shell_exclusive = None;

        'outputs: for output in self.lxb.space.outputs() {
            let map = layer_map_for_output(output);
            for layer in map.layers() {
                if layer.cached_state().keyboard_interactivity != KeyboardInteractivity::Exclusive {
                    continue;
                }
                let surface = layer.layer_surface().wl_surface().clone();
                let from_shell = surface
                    .client()
                    .is_some_and(|client| self.lxb.shell_control.is_shell_client(&client));
                if from_shell {
                    shell_exclusive = Some(surface);
                    break 'outputs;
                }
                exclusive.get_or_insert(surface);
            }
        }

        self.lxb.exclusive_keyboard_focus = shell_exclusive.or(exclusive);
    }

    fn output_for_layer_surface(&self, surface: &WlSurface) -> Option<Output> {
        self.lxb
            .space
            .outputs()
            .find(|o| {
                layer_map_for_output(o)
                    .layer_for_surface(surface, smithay::desktop::WindowSurfaceType::ALL)
                    .is_some()
            })
            .cloned()
    }
}

impl BufferHandler for LxbState {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

impl ShmHandler for LxbState {
    fn shm_state(&self) -> &ShmState {
        &self.lxb.shm_state
    }
}

// ---------------------------------------------------------------------------
// xdg-shell
// ---------------------------------------------------------------------------

impl XdgShellHandler for LxbState {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.lxb.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let window = Window::new_wayland_window(surface);
        self.map_new_window(window);
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        self.unconstrain_popup(&surface);
        if let Err(err) = self.lxb.popups.track_popup(PopupKind::from(surface)) {
            tracing::warn!(?err, "failed to track popup");
        }
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        self.unconstrain_popup(&surface);
        surface.send_repositioned(token);
    }

    fn grab(&mut self, surface: PopupSurface, seat: WlSeat, serial: Serial) {
        let seat: Seat<Self> = Seat::from_resource(&seat).unwrap();
        let popup = PopupKind::Xdg(surface);
        if let Ok(root) = find_popup_root_surface(&popup) {
            if let Ok(mut grab) = self
                .lxb
                .popups
                .grab_popup(root.into(), popup, &seat, serial)
            {
                if let Some(keyboard) = seat.get_keyboard() {
                    if keyboard.is_grabbed()
                        && !(keyboard.has_grab(serial)
                            || keyboard.has_grab(grab.previous_serial().unwrap_or(serial)))
                    {
                        grab.ungrab(smithay::desktop::PopupUngrabStrategy::All);
                        return;
                    }
                    keyboard.set_focus(self, grab.current_grab(), serial);
                    // The grab is not optional. Besides routing keys along the
                    // popup chain, it is what hands focus back to the window
                    // when the menu closes; without it the application is left
                    // deaf to the keyboard from the first menu it ever opens.
                    keyboard.set_grab(
                        self,
                        smithay::desktop::PopupKeyboardGrab::new(&grab),
                        serial,
                    );
                }
                if let Some(pointer) = seat.get_pointer() {
                    if pointer.is_grabbed()
                        && !(pointer.has_grab(serial)
                            || pointer.has_grab(grab.previous_serial().unwrap_or(grab.serial())))
                    {
                        grab.ungrab(smithay::desktop::PopupUngrabStrategy::All);
                        return;
                    }
                    pointer.set_grab(
                        self,
                        smithay::desktop::PopupPointerGrab::new(&grab),
                        serial,
                        smithay::input::pointer::Focus::Keep,
                    );
                }
            }
        }
    }

    fn fullscreen_request(&mut self, surface: ToplevelSurface, output: Option<WlOutput>) {
        let wl_surface = surface.wl_surface();
        let window = self.lxb.window_for_surface(wl_surface);
        // The display the window is already on wins over the one the client
        // names. A window here fills its display either way, so "fullscreen on
        // that other screen" is not a layout this shell has — and a client
        // that names an output at all usually names the one it believes it
        // started on, which after a splash screen or an updater window is
        // routinely the wrong one.
        let target = window
            .as_ref()
            .and_then(|window| self.lxb.outputs.window_output(&self.lxb.space, window))
            .or_else(|| output.as_ref().and_then(Output::from_resource))
            .or_else(|| self.lxb.space.outputs().next().cloned());

        if let Some(output) = target {
            let geometry = self.lxb.space.output_geometry(&output).unwrap_or_default();
            // Fullscreen is the whole display, and on a session drawing
            // applications larger than life the whole display is not the number
            // of logical pixels the display has. The window is configured at a
            // fraction of it, told over `wp_fractional_scale_v1` to fill that
            // fraction with the display's own pixels, and drawn back out over
            // all of it — the same three parts as an ordinary tiled window, and
            // the same division, from the same place. See [`crate::scale`].
            //
            // Handing the client the display's own size instead asks it for a
            // buffer a factor too large in each direction, and the growth that
            // puts an ordinary window over the whole display then puts that one
            // a factor past it: a video put fullscreen from a browser on a
            // session at 150% came back with a third of its width and a third
            // of its height off the screen. That is the bug this division is.
            //
            // A surface with no window yet has been told no scale either —
            // `new_fractional_scale` answers the display's own until there is a
            // window to ask about — so the undivided size is the honest answer
            // for it, and the tiling it gets when it maps carries both halves.
            let room = match window.as_ref() {
                Some(window) => self.lxb.outputs.room_for(window, geometry.size),
                None => geometry.size,
            };
            surface.with_pending_state(|state| {
                state.states.set(xdg_toplevel::State::Fullscreen);
                state.size = Some(room);
                // What the window may make of itself, moved with the size it
                // belongs to: the tiling left this at the usable area, which is
                // the smaller rectangle of the two.
                state.bounds = Some(room);
            });
            if let Some(window) = window {
                assign_output(&window, &output);
                remap_window_preserving_stack(&mut self.lxb.space, &window, geometry.loc);
                self.raise_window(&window, true);
            }
        }
        surface.send_configure();
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        surface.with_pending_state(|state| {
            state.states.unset(xdg_toplevel::State::Fullscreen);
        });
        // Deliberately no configure of our own here: leaving fullscreen means
        // going back to maximized, and that is what the re-tile below sends.
        // Answering first with `size = None` would invite the client to pick a
        // size for itself in the meantime.
        self.enforce_maximized(&surface);
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        // Already true of every window here, but the client still deserves the
        // configure that says so.
        self.enforce_maximized(&surface);
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        // Refused, in the only way the protocol offers: configure it straight
        // back to maximized. A floating window would have nowhere to float —
        // there is no desktop under these windows, and an un-maximized one is
        // exactly the half-off-the-output state this shell exists to avoid.
        self.enforce_maximized(&surface);
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        if let Some(window) = self.lxb.window_for_surface(surface.wl_surface()) {
            // The other half of the line `map_new_window` writes: which of an
            // application's windows went away, and when, against which of them
            // arrived.
            tracing::info!(
                app_id = crate::shell_control::window_app_id(&window),
                title = crate::shell_control::window_title(&window),
                "unmapped toplevel"
            );
            // Before it is unmapped, while it still has its id: a flight left
            // behind would be replayed on whatever window inherits that id.
            self.lxb
                .restores
                .forget(crate::overview::window_id(&window));
            self.lxb.space.unmap_elem(&window);
        }
        self.lxb.outputs.relayout_windows(&mut self.lxb.space);
        self.focus_topmost_window();
    }

    fn popup_destroyed(&mut self, _surface: PopupSurface) {
        self.focus_topmost_window();
    }
}

impl LxbState {
    /// Configure a toplevel back to the one layout it is allowed to have:
    /// maximized, filling its output's usable area.
    ///
    /// Re-tiling sends a configure only when something actually differs, so a
    /// client that asks to un-maximize a window that is already where we want
    /// it simply gets no answer, and keeps the state it has. That is the
    /// refusal.
    fn enforce_maximized(&mut self, surface: &ToplevelSurface) {
        if let Some(window) = self.lxb.window_for_surface(surface.wl_surface()) {
            self.lxb.outputs.tile_window(&mut self.lxb.space, &window);
        } else {
            // Not mapped yet, so there is no output to size against — set the
            // state at least, rather than leaving the request unanswered.
            surface.with_pending_state(|state| set_maximized_states(&mut state.states));
            surface.send_configure();
        }
    }

    /// Place a freshly created window on the focused output and give it focus.
    pub(crate) fn map_new_window(&mut self, window: Window) {
        // Where this window belongs, in order of how well each answer knows:
        // the record the shell filed for the launch this window turns out to
        // be, then what the shell said about launches in general, then the
        // display holding keyboard focus, then the one under the pointer, then
        // whatever exists.
        //
        // The launch's own record comes first because it is the only answer
        // that is right when two displays are loading at once: the
        // session-wide one names a single launch, so the *other* launch's
        // window — arriving second, or first, depending on which toolkit is
        // quicker — was placed on a display nobody started it from. See
        // `lxb_shell_v1.place_launch`.
        //
        // The shell's session-wide answer comes next because it is the only
        // source that is still right while an application is starting. Focus
        // has usually moved back to the *previous* application by the time the
        // new window maps, and a controller-driven session never moves the
        // pointer at all.
        //
        // Whatever this settles on is recorded on the window by the tiling
        // below and is where it stays: an application belongs to the display
        // it was started on, and nothing it does afterwards moves it.
        let output = self
            .launch_output_for(&window)
            .or_else(|| self.shell_launch_output())
            .or_else(|| self.keyboard_focus_output())
            .or_else(|| {
                self.lxb
                    .outputs
                    .output_at(&self.lxb.space, self.lxb.pointer_location)
            })
            .or_else(|| self.lxb.space.outputs().next().cloned());

        let mut location = output
            .as_ref()
            .and_then(|o| self.lxb.space.output_geometry(o))
            .map(|g| g.loc)
            .unwrap_or_default();

        // A window nobody can see must not be given the keyboard, or the user
        // is typing into something that is not on their screen. It is still
        // mapped and still configured — it is simply never looked at.
        let accepts_focus = self.lxb.takes_the_keyboard(&window);
        if let Some(surface) = window.x11_surface() {
            remember_x11_client_geometry(&window, surface.geometry());
        }
        if window_is_x11_chrome(&window) {
            // Managed X11 notifications, menus and splash windows retain the
            // geometry chosen by their client, just like override-redirect
            // chrome. Keep them visually above the full-output application.
            if let Some(surface) = window.x11_surface() {
                location = surface.geometry().loc;
                window.override_z_index(smithay::desktop::space::RenderZindex::Overlay as u8);
            }
        }
        self.lxb
            .space
            .map_element(window.clone(), location, accepts_focus);
        if let Some(output) = &output {
            self.lxb
                .outputs
                .tile_window_on_output(&mut self.lxb.space, &window, output);
        } else {
            self.lxb.outputs.tile_window(&mut self.lxb.space, &window);
        }

        tracing::info!(
            output = output.as_ref().map(|o| o.name()).unwrap_or_default(),
            geometry = ?self.lxb.space.element_geometry(&window),
            "mapped toplevel"
        );

        if accepts_focus {
            self.set_window_keyboard_focus(&window);
        } else {
            self.focus_topmost_window();
        }
    }

    /// Keep popups inside the output they belong to.
    fn unconstrain_popup(&self, popup: &PopupSurface) {
        let Ok(root) = find_popup_root_surface(&PopupKind::Xdg(popup.clone())) else {
            return;
        };
        let Some(window) = self.lxb.window_for_surface(&root) else {
            return;
        };
        let Some(output) = self.lxb.space.outputs_for_element(&window).first().cloned() else {
            return;
        };
        let Some(output_geo) = self.lxb.space.output_geometry(&output) else {
            return;
        };
        let Some(window_loc) = self.lxb.space.element_location(&window) else {
            return;
        };

        // The positioner works in the window's coordinate space.
        let mut target = output_geo;
        target.loc -= get_popup_toplevel_coords(&PopupKind::Xdg(popup.clone()));
        target.loc -= window_loc;

        popup.with_pending_state(|state| {
            state.geometry = state.positioner.get_unconstrained_geometry(target);
        });
    }
}

// ---------------------------------------------------------------------------
// xdg-decoration: we are a server-side-decoration-free compositor
// ---------------------------------------------------------------------------

impl XdgDecorationHandler for LxbState {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(zxdg_toplevel_decoration_v1::Mode::ServerSide);
        });
        toplevel.send_configure();
    }

    fn request_mode(
        &mut self,
        toplevel: ToplevelSurface,
        _mode: zxdg_toplevel_decoration_v1::Mode,
    ) {
        // Windows are tiled full-output, so client decorations would only waste
        // pixels. Always answer with server-side (i.e. none).
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(zxdg_toplevel_decoration_v1::Mode::ServerSide);
        });
        toplevel.send_configure();
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(zxdg_toplevel_decoration_v1::Mode::ServerSide);
        });
        toplevel.send_configure();
    }
}

// ---------------------------------------------------------------------------
// wlr-layer-shell — this is what the desktop shell binds to
// ---------------------------------------------------------------------------

impl WlrLayerShellHandler for LxbState {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.lxb.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: LayerSurface,
        wl_output: Option<WlOutput>,
        _layer: Layer,
        namespace: String,
    ) {
        let output = wl_output
            .as_ref()
            .and_then(Output::from_resource)
            .or_else(|| {
                self.lxb
                    .outputs
                    .output_at(&self.lxb.space, self.lxb.pointer_location)
            })
            .or_else(|| self.lxb.space.outputs().next().cloned());

        let Some(output) = output else {
            tracing::warn!(
                namespace,
                "layer surface requested with no output available"
            );
            surface.send_configure();
            return;
        };

        tracing::info!(namespace, output = output.name(), "new layer surface");

        // `LayerMap` works with the desktop wrapper, not the raw protocol object.
        let desktop_layer = DesktopLayerSurface::new(surface.clone(), namespace);

        {
            let mut map = layer_map_for_output(&output);
            if let Err(err) = map.map_layer(&desktop_layer) {
                tracing::warn!(?err, "failed to map layer surface");
                return;
            }
            map.arrange();
        }

        self.lxb.outputs.relayout_windows(&mut self.lxb.space);

        self.refresh_exclusive_focus();
        self.focus_topmost_window();
    }

    fn layer_destroyed(&mut self, surface: LayerSurface) {
        let mut changed = false;

        for output in self.lxb.space.outputs().cloned().collect::<Vec<_>>() {
            let mut map = layer_map_for_output(&output);
            let Some(layer) = map
                .layers()
                .find(|l| l.layer_surface() == &surface)
                .cloned()
            else {
                continue;
            };
            map.unmap_layer(&layer);
            map.arrange();
            changed = true;
        }

        if changed {
            self.lxb.outputs.relayout_windows(&mut self.lxb.space);
        }
        self.refresh_exclusive_focus();
        self.focus_topmost_window();
    }

    fn new_popup(&mut self, _parent: LayerSurface, popup: PopupSurface) {
        if let Err(err) = self.lxb.popups.track_popup(PopupKind::from(popup)) {
            tracing::warn!(?err, "failed to track layer popup");
        }
    }
}

// ---------------------------------------------------------------------------
// seat / selection
// ---------------------------------------------------------------------------

impl SeatHandler for LxbState {
    type KeyboardFocus = KeyboardFocusTarget;
    type PointerFocus = KeyboardFocusTarget;
    type TouchFocus = KeyboardFocusTarget;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.lxb.seat_state
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&KeyboardFocusTarget>) {
        let dh = &self.lxb.display_handle;
        let client = focused
            .and_then(WaylandFocus::wl_surface)
            .and_then(|surface| dh.get_client(surface.id()).ok());
        set_data_device_focus(dh, seat, client.clone());
        set_primary_focus(dh, seat, client);
        self.refresh_window_activation();
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        // Only when it changes kind, and never the surface itself: this fires
        // on every crossing between clients, and the one question it exists to
        // answer is why the pointer went invisible while it was plainly still
        // moving. A client asking for `hidden` is the usual reason, and nothing
        // else in the compositor can tell you that it happened.
        if std::mem::discriminant(&self.lxb.cursor_status) != std::mem::discriminant(&image) {
            tracing::debug!(
                from = %describe_cursor(&self.lxb.cursor_status),
                to = %describe_cursor(&image),
                "cursor image changed"
            );
        }
        self.lxb.cursor_status = image;
    }
}

/// Which of the three kinds of cursor a status is, for the log.
fn describe_cursor(status: &CursorImageStatus) -> String {
    match status {
        CursorImageStatus::Hidden => "hidden".to_string(),
        CursorImageStatus::Surface(_) => "a client's own surface".to_string(),
        CursorImageStatus::Named(icon) => format!("themed {icon:?}"),
    }
}

impl SelectionHandler for LxbState {
    type SelectionUserData = ();

    fn new_selection(
        &mut self,
        ty: SelectionTarget,
        source: Option<SelectionSource>,
        _seat: Seat<Self>,
    ) {
        if let Some(xwm) = self.lxb.xwm.as_mut() {
            if let Err(err) = xwm.new_selection(ty, source.map(|source| source.mime_types())) {
                tracing::warn!(?err, ?ty, "failed to publish Wayland selection to X11");
            }
        }
    }

    fn send_selection(
        &mut self,
        ty: SelectionTarget,
        mime_type: String,
        fd: OwnedFd,
        _seat: Seat<Self>,
        _user_data: &(),
    ) {
        if let Some(xwm) = self.lxb.xwm.as_mut() {
            if let Err(err) = xwm.send_selection(ty, mime_type, fd, self.lxb.loop_handle.clone()) {
                tracing::warn!(?err, ?ty, "failed to send X11 selection to Wayland");
            }
        }
    }
}

impl DataDeviceHandler for LxbState {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.lxb.data_device_state
    }
}

impl ClientDndGrabHandler for LxbState {}
impl ServerDndGrabHandler for LxbState {}

impl PrimarySelectionHandler for LxbState {
    fn primary_selection_state(&self) -> &PrimarySelectionState {
        &self.lxb.primary_selection_state
    }
}

impl PointerConstraintsHandler for LxbState {
    fn new_constraint(&mut self, surface: &WlSurface, pointer: &PointerHandle<Self>) {
        // Activate immediately only when the pointer already sits inside the
        // constraint's effective surface region.
        let has_focus = pointer
            .current_focus()
            .and_then(|focus| {
                focus
                    .wl_surface()
                    .map(|current| current.as_ref() == surface)
            })
            .unwrap_or(false);
        // In the surface's own coordinates, which on an application drawing
        // larger than life is not the screen's: a constraint's region is the
        // client's own rectangle. See `crate::input::Hit`.
        let local_location = self
            .surface_under(self.lxb.pointer_location)
            .filter(|hit| &hit.surface == surface)
            .map(|hit| hit.point - hit.origin);
        with_pointer_constraint(surface, pointer, |constraint| {
            if let Some(constraint) = constraint {
                let inside = local_location.is_some_and(|location| {
                    constraint
                        .region()
                        .map(|region| region.contains(location.to_i32_round()))
                        .unwrap_or(true)
                });
                tracing::info!(
                    kind = match &*constraint {
                        smithay::wayland::pointer_constraints::PointerConstraint::Locked(_) =>
                            "locked",
                        _ => "confined",
                    },
                    whole_surface = constraint.region().is_none(),
                    // The rectangles themselves, because the size of the box a
                    // pointer is being held in is what says whose box it is.
                    region = ?constraint.region().map(|region| region.rects.clone()),
                    ?local_location,
                    has_focus,
                    inside,
                    "a client asked to hold the pointer"
                );
                if has_focus && inside {
                    constraint.activate();
                }
            }
        });
    }

    fn cursor_position_hint(
        &mut self,
        surface: &WlSurface,
        pointer: &PointerHandle<Self>,
        location: Point<f64, Logical>,
    ) {
        let mut active_lock = false;
        with_pointer_constraint(surface, pointer, |constraint| {
            active_lock = constraint.is_some_and(|constraint| {
                constraint.is_active()
                    && matches!(
                        &*constraint,
                        smithay::wayland::pointer_constraints::PointerConstraint::Locked(_)
                    )
            });
        });
        if active_lock {
            self.lxb.pointer_position_hint = Some((surface.clone(), location));
        }
    }
}

// ---------------------------------------------------------------------------
// misc
// ---------------------------------------------------------------------------

impl OutputHandler for LxbState {}

impl smithay::wayland::tablet_manager::TabletSeatHandler for LxbState {
    fn tablet_tool_image(
        &mut self,
        _tool: &smithay::backend::input::TabletToolDescriptor,
        image: CursorImageStatus,
    ) {
        self.lxb.cursor_status = image;
    }
}

impl FractionalScaleHandler for LxbState {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        let window = self.lxb.window_for_surface(&surface);
        // Advertise the scale of whichever output the surface's window is on.
        let output = window
            .as_ref()
            .and_then(|w| self.lxb.space.outputs_for_element(w).first().cloned())
            .or_else(|| self.lxb.space.outputs().next().cloned())
            .map(|o| o.current_scale().fractional_scale())
            .unwrap_or(1.0);
        // Times how much larger than life its application is drawing, which is
        // the second of the three parts of that: the window was configured
        // smaller than the display, and this is what tells the client to fill
        // that smaller window with the display's own pixels. See
        // [`crate::scale`].
        let scale = crate::scale::preferred_scale(
            output,
            window
                .as_ref()
                .map(|window| self.lxb.outputs.window_scale(window))
                .unwrap_or(1.0),
        );

        with_states(&surface, |states| {
            smithay::wayland::fractional_scale::with_fractional_scale(states, |fs| {
                fs.set_preferred_scale(scale);
            });
        });
    }
}

impl XdgActivationHandler for LxbState {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.lxb.activation_state
    }

    fn request_activation(
        &mut self,
        _token: XdgActivationToken,
        _token_data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        if let Some(window) = self.lxb.window_for_surface(&surface) {
            // An application the shell keeps out of sight does not get to ask
            // for the screen back. Valve's client asks — every time it starts
            // a game — and granting it would raise the storefront over the
            // game that was starting.
            if self.lxb.takes_the_keyboard(&window) {
                self.raise_window(&window, true);
                self.set_window_keyboard_focus(&window);
            }
        }
    }
}

impl DmabufHandler for LxbState {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.lxb.dmabuf_state
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        dmabuf: smithay::backend::allocator::dmabuf::Dmabuf,
        notifier: ImportNotifier,
    ) {
        match self.backend.import_dmabuf(&dmabuf) {
            Ok(()) => {
                let _ = notifier.successful::<LxbState>();
            }
            Err(err) => {
                tracing::warn!(?err, "dmabuf import failed");
                notifier.failed();
            }
        }
    }
}

delegate_compositor!(LxbState);
delegate_shm!(LxbState);
delegate_xdg_shell!(LxbState);
delegate_xdg_decoration!(LxbState);
delegate_layer_shell!(LxbState);
delegate_seat!(LxbState);
delegate_data_device!(LxbState);
delegate_primary_selection!(LxbState);
delegate_output!(LxbState);
delegate_viewporter!(LxbState);
delegate_presentation!(LxbState);
delegate_fractional_scale!(LxbState);
delegate_relative_pointer!(LxbState);
delegate_pointer_constraints!(LxbState);
delegate_pointer_gestures!(LxbState);
delegate_single_pixel_buffer!(LxbState);
delegate_cursor_shape!(LxbState);
delegate_xdg_activation!(LxbState);
delegate_dmabuf!(LxbState);
crate::delegate_tearing_control!(LxbState);

/// Carrying a client's colour into the display pipeline.
///
/// The policy lives in [`LxbState::follow_surface_colour`]; this is only the
/// wiring. See [`crate::colour`] for why the user's setting outranks the
/// client's request rather than the other way round.
impl crate::colour::ColourHandler for LxbState {
    fn colour_changed(&mut self, surface: &WlSurface) {
        self.follow_surface_colour(surface);
    }

    fn describe_surface(
        &mut self,
        surface: &WlSurface,
        resource: &lxb_protocol::server::frog::frog_color_managed_surface::FrogColorManagedSurface,
    ) {
        // A display this session is not driving in HDR is described as sRGB
        // whatever the panel could do, because that is what the client's
        // pixels will actually meet.
        // A display this session is actually driving in HDR is described as
        // PQ/BT.2020, and one that is not is described as sRGB whatever the
        // panel could do — because what a client is owed here is what its
        // pixels will actually meet, not what the hardware is capable of.
        //
        // Answering PQ is only safe because the pipeline gets out of the way
        // when it has to: see `crate::render::output_shows_encoded_content`
        // and `hdr::Manager::request_passthrough`. A client that takes this
        // answer and fills the display gets its own encoding passed through
        // untouched.
        let status = self
            .output_of(surface)
            .map(|output| self.lxb.hdr.status(&output));
        let hdr = status.as_ref().is_some_and(|status| status.enabled);
        let peak = status
            .as_ref()
            .map(|status| status.max_luminance)
            .unwrap_or(0);
        crate::colour::send_preferred_metadata(resource, hdr, peak, 0.0);
    }

    fn output_of(&self, surface: &WlSurface) -> Option<Output> {
        let window = self.lxb.window_for_surface(surface)?;
        self.lxb
            .space
            .outputs_for_element(&window)
            .into_iter()
            .next()
    }
}
crate::delegate_colour_management!(LxbState);

/// The same, through the protocol everything that is not a Proton game speaks.
///
/// No second policy: this hands back the same two descriptions the frog
/// implementation above does, from the same [`crate::hdr::Status`].
impl crate::colour_management::ColourManagerHandler for LxbState {
    fn colour_manager_state(&mut self) -> &mut crate::colour_management::ColourManagerState {
        &mut self.lxb.colour_manager
    }

    fn output_is_hdr(&self, output: &Output) -> bool {
        self.lxb.hdr.status(output).enabled
    }

    fn output_for_resource(&self, resource: &WlOutput) -> Option<Output> {
        Output::from_resource(resource)
    }
}
crate::delegate_colour_manager!(LxbState);

impl DrmSyncobjHandler for LxbState {
    fn drm_syncobj_state(&mut self) -> Option<&mut DrmSyncobjState> {
        self.lxb.syncobj_state.as_mut()
    }
}
smithay::delegate_drm_syncobj!(LxbState);

impl LxbState {
    /// React to a surface saying what its colour is.
    ///
    /// Deliberately only re-describes the surface: see the note in
    /// [`crate::colour`] about the user's setting outranking the client's.
    fn follow_surface_colour(&mut self, _surface: &WlSurface) {}
}
