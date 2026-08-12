//! Global compositor state and the Wayland protocol handler implementations.
//!
//! The state is deliberately split in two halves:
//!
//! * [`Lxb`] owns everything backend independent — protocol globals, the
//!   window [`Space`], the seat, configuration.
//! * [`Backend`] owns whatever the active backend needs (a winit window, or a
//!   set of DRM devices).
//!
//! [`LxbState`] simply holds both. Keeping them in separate fields means a
//! render pass can borrow the backend mutably while still reading the space,
//! which a single flat struct would not allow.

use std::sync::Arc;
use std::time::{Duration, Instant};

use smithay::desktop::{PopupManager, Space, Window};
use smithay::input::{Seat, SeatState};
use smithay::reexports::calloop::{
    generic::Generic,
    timer::{TimeoutAction, Timer},
    Interest, LoopHandle, LoopSignal, Mode, PostAction, RegistrationToken,
};
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, Display, DisplayHandle};
use smithay::utils::{Logical, Point};
use smithay::wayland::compositor::{CompositorClientState, CompositorState};
use smithay::wayland::dmabuf::DmabufState;
use smithay::wayland::output::OutputManagerState;
use smithay::wayland::presentation::PresentationState;
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::selection::data_device::DataDeviceState;
use smithay::wayland::selection::primary_selection::PrimarySelectionState;
use smithay::wayland::shell::wlr_layer::WlrLayerShellState;
use smithay::wayland::shell::xdg::decoration::XdgDecorationState;
use smithay::wayland::shell::xdg::XdgShellState;
use smithay::wayland::shm::ShmState;
use smithay::wayland::socket::ListeningSocketSource;
use smithay::wayland::viewporter::ViewporterState;
use smithay::wayland::xdg_activation::XdgActivationState;
use smithay::wayland::xwayland_keyboard_grab::XWaylandKeyboardGrabState;
use smithay::wayland::xwayland_shell::XWaylandShellState;
use smithay::xwayland::{X11Wm, XWayland, XWaylandClientData, XWaylandEvent};

use crate::backend::Backend;
use crate::config::Config;
use crate::input::KeyBindings;
use crate::outputs::OutputManager;
use crate::shell_control::ShellControlState;
use crate::xwayland::X11FocusProbe;

const XWAYLAND_STARTUP_TIMEOUT: Duration = Duration::from_secs(5);

/// How often to ask XWayland where its pointer is.
///
/// Fast enough that a cursor following synthetic X11 input does not visibly
/// trail it, and slow enough that the round trip costs nothing worth counting:
/// a local socket exchange at this rate is well under a thousandth of a core.
const XWAYLAND_POINTER_INTERVAL: Duration = Duration::from_millis(8);

/// How often the session shell is checked for having exited.
///
/// A child cannot wake calloop by itself, so this is a poll. It only runs in
/// `--shell` mode, and a fifth of a second is far below the threshold at which
/// a logout feels unresponsive.
const SESSION_SHELL_POLL: Duration = Duration::from_millis(200);

/// Top level state handed to every calloop callback and protocol dispatch.
pub struct LxbState {
    pub backend: Backend,
    pub lxb: Lxb,
}

/// Backend independent compositor state.
pub struct Lxb {
    pub display_handle: DisplayHandle,
    pub loop_handle: LoopHandle<'static, LxbState>,
    pub loop_signal: LoopSignal,
    pub start_time: Instant,
    pub socket_name: String,
    /// Display name of LineXinBar's private XWayland server once it is ready.
    /// Never sourced from the outer session's `DISPLAY`.
    pub xwayland_display: Option<String>,
    pub xwayland_ready: bool,
    /// Registration owning the XWayland instance. It stays registered (but
    /// disabled after readiness) so dropping it cannot tear down XWayland's
    /// Wayland client while the private X server is in use.
    pub xwayland_source: Option<RegistrationToken>,
    pub xwayland_startup_timeout: Option<RegistrationToken>,
    pub pending_autostart: Vec<String>,
    /// The session shell requested with `--shell`, started alongside the
    /// autostart list. The compositor's lifetime is tied to it: when it exits,
    /// the session ends.
    pub pending_shell: Option<String>,
    /// That shell's process, once it is running. Applications are its
    /// children, which is what makes it the boundary Close walks up to: the
    /// last process before the shell is the launch, and the launch is the
    /// application. See [`crate::teardown`].
    pub session_shell_pid: Option<i32>,
    /// Fatal session errors are reported after calloop has unwound, allowing
    /// sockets and clients to be dropped normally while still giving a
    /// service manager a non-zero exit status to restart.
    pub fatal_error: Option<String>,
    pub running: bool,

    pub config: Config,
    pub keybindings: KeyBindings,
    /// The Windows key, watched for being pressed and let go of on its own.
    /// That is the home button on a keyboard, and it cannot live in the table
    /// above: see [`crate::input::HomeTap`].
    pub home_tap: crate::input::HomeTap,

    // Protocol globals. Several of these are never read after construction,
    // but dropping them would unadvertise the global, so they are owned here
    // for the lifetime of the compositor.
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    #[allow(dead_code)]
    pub xdg_decoration_state: XdgDecorationState,
    pub layer_shell_state: WlrLayerShellState,
    pub shm_state: ShmState,
    #[allow(dead_code)]
    pub output_manager_state: OutputManagerState,
    pub seat_state: SeatState<LxbState>,
    pub data_device_state: DataDeviceState,
    pub primary_selection_state: PrimarySelectionState,
    #[allow(dead_code)]
    pub viewporter_state: ViewporterState,
    #[allow(dead_code)]
    pub presentation_state: PresentationState,
    pub activation_state: XdgActivationState,
    pub dmabuf_state: DmabufState,
    pub xwayland_shell_state: XWaylandShellState,
    #[allow(dead_code)]
    pub xwayland_keyboard_grab_state: XWaylandKeyboardGrabState,
    pub shell_control: ShellControlState,
    pub xwm: Option<X11Wm>,
    pub x11_focus_probe: Option<X11FocusProbe>,
    /// Where XWayland's pointer was at the last check, and where ours was. A
    /// difference appearing in the first without the second having moved is
    /// motion an X client synthesised — see `follow_xwayland_pointer`.
    pub last_xwayland_pointer: Option<Point<f64, Logical>>,
    pub last_synced_pointer: Option<Point<f64, Logical>>,

    // Desktop.
    pub space: Space<Window>,
    pub popups: PopupManager,
    pub outputs: OutputManager,
    /// Which display is showing the window overview, and how far along its
    /// enter/leave animation is.
    pub overview: crate::overview::Overviews,
    /// Windows on their way back out of the tile that asked for them.
    pub restores: crate::restore::Restores,
    /// Displays answering for a screenshot that has just been taken of them.
    pub flashes: crate::flash::Flashes,
    /// Frames other clients have asked for over wlr-screencopy, and the damage
    /// each of them has been told about.
    pub screencopy: crate::screencopy::ScreencopyState,
    /// High dynamic range, per display: what the shell asked for, and what the
    /// connector turned out to be able to do about it.
    pub hdr: crate::hdr::Manager,

    // Input.
    pub seat: Seat<LxbState>,
    pub pointer_location: Point<f64, Logical>,
    /// Last host coordinate from an absolute-only nested backend, used to
    /// synthesize wp_relative_pointer deltas for nested testing.
    pub nested_host_pointer_location: Option<Point<f64, Logical>>,
    /// Client-provided position to restore after an active pointer lock ends.
    pub pointer_position_hint: Option<(WlSurface, Point<f64, Logical>)>,
    pub cursor_status: smithay::input::pointer::CursorImageStatus,
    /// Whether the cursor is drawn at all.
    ///
    /// A console is not a desktop: it is driven with a controller, and an
    /// arrow parked in the middle of the screen is a thing the user cannot
    /// move and did not ask for. So the pointer starts off screen and stays
    /// there until something actually moves it, and goes away again the moment
    /// the user reaches for a key or a controller instead. The two sides of
    /// that are `LxbState::pointer_moved` and
    /// [`LxbState::pointer_put_down`], in `input`.
    ///
    /// Separate from `cursor_status`, which is the *shape* whatever the
    /// pointer is over asked for. That is the client's answer to a different
    /// question and has to survive being hidden: a cursor that came back as a
    /// plain arrow over a text field would be the wrong cursor.
    pub pointer_visible: bool,
    /// Whether the parent has granted this nested compositor keyboard focus.
    /// Always true on the native backend.
    pub keyboard_focus_enabled: bool,
    /// Surface the keyboard focus is pinned to by a layer surface requesting
    /// exclusive interactivity, if any.
    pub exclusive_keyboard_focus: Option<WlSurface>,
    /// Applications the shell has asked to be run without ever being seen,
    /// by the name each calls itself, folded to lower case.
    ///
    /// A program the shell *drives* rather than presents. Valve's Steam client
    /// is the one that needs it: the shell signs it in, starts games through
    /// it and reads its library, none of which anybody should have to watch —
    /// but the client punctuates all of it with windows of its own, and each
    /// one lands on top of whatever the shell was drawing. Rather than hope
    /// every version of it can be talked out of them, the compositor simply
    /// does not put them on the screen.
    ///
    /// Empty in a session that never asks, which is what keeps this free: the
    /// question is only put to a window when there is something to compare it
    /// against. See [`Lxb::out_of_sight`] and `lxb_shell_v1.keep_out_of_sight`.
    pub unseen: std::collections::HashSet<String>,
}

impl LxbState {
    /// Build the state, create the Wayland socket and register it with the loop.
    pub fn new(
        display: Display<LxbState>,
        loop_handle: LoopHandle<'static, LxbState>,
        loop_signal: LoopSignal,
        backend: Backend,
        config: Config,
        socket_name: Option<String>,
    ) -> anyhow::Result<Self> {
        let display_handle = display.handle();
        let dh = &display_handle;

        let compositor_state = CompositorState::new::<Self>(dh);
        let xdg_shell_state = XdgShellState::new::<Self>(dh);
        let xdg_decoration_state = XdgDecorationState::new::<Self>(dh);
        let layer_shell_state = WlrLayerShellState::new::<Self>(dh);
        // The one extra format wl_shm offers beyond the two every compositor
        // must: it is what a screen copy is handed over in, and a client cannot
        // make a buffer in a format the compositor never advertised. See
        // [`crate::screencopy::FORMAT`].
        let shm_state = ShmState::new::<Self>(dh, vec![crate::screencopy::FORMAT]);
        // `new_with_xdg_output` also exports xdg-output, which clients need to
        // reason about logical positions in a multi-display layout.
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(dh);
        let mut seat_state = SeatState::new();
        let data_device_state = DataDeviceState::new::<Self>(dh);
        let primary_selection_state = PrimarySelectionState::new::<Self>(dh);
        let viewporter_state = ViewporterState::new::<Self>(dh);
        let presentation_state = PresentationState::new::<Self>(dh, libc::CLOCK_MONOTONIC as u32);
        let activation_state = XdgActivationState::new::<Self>(dh);
        let dmabuf_state = DmabufState::new();
        let xwayland_shell_state = XWaylandShellState::new::<Self>(dh);
        let xwayland_keyboard_grab_state = XWaylandKeyboardGrabState::new::<Self>(dh);
        let shell_control = ShellControlState::new::<Self>(dh);
        let screencopy = crate::screencopy::ScreencopyState::new::<Self>(dh);

        smithay::wayland::fractional_scale::FractionalScaleManagerState::new::<Self>(dh);
        smithay::wayland::relative_pointer::RelativePointerManagerState::new::<Self>(dh);
        smithay::wayland::pointer_constraints::PointerConstraintsState::new::<Self>(dh);
        smithay::wayland::pointer_gestures::PointerGesturesState::new::<Self>(dh);
        smithay::wayland::single_pixel_buffer::SinglePixelBufferState::new::<Self>(dh);
        smithay::wayland::cursor_shape::CursorShapeManagerState::new::<Self>(dh);
        // Text input, input methods and virtual keyboards, which together are
        // what lets the shell put a keyboard on screen and type into whatever
        // asked for one.
        crate::text_input::advertise(dh);

        let seat_name = backend.seat_name();
        let mut seat = seat_state.new_wl_seat(dh, seat_name.clone());

        let keyboard_config = &config.input;
        let xkb = smithay::input::keyboard::XkbConfig {
            rules: &keyboard_config.keyboard_rules,
            model: &keyboard_config.keyboard_model,
            layout: &keyboard_config.keyboard_layout,
            variant: &keyboard_config.keyboard_variant,
            options: keyboard_config.keyboard_options.clone(),
        };
        seat.add_keyboard(
            xkb,
            keyboard_config.repeat_delay,
            keyboard_config.repeat_rate,
        )?;
        seat.add_pointer();
        seat.add_touch();

        let keybindings = KeyBindings::from_config(&config);

        // Bring up the socket last, so clients only ever see a ready compositor.
        let socket = match &socket_name {
            Some(name) => ListeningSocketSource::with_name(name)?,
            None => ListeningSocketSource::new_auto()?,
        };
        let socket_name = socket.socket_name().to_string_lossy().into_owned();

        loop_handle
            .insert_source(socket, move |stream, _, state| {
                if let Err(err) = state
                    .lxb
                    .display_handle
                    .insert_client(stream, Arc::new(ClientState::default()))
                {
                    tracing::warn!(?err, "failed to accept client");
                }
            })
            .map_err(|e| anyhow::anyhow!("failed to insert socket source: {e}"))?;

        // Drive protocol dispatch from the same event loop as everything else.
        loop_handle
            .insert_source(
                Generic::new(display, Interest::READ, Mode::Level),
                |_, display, state| {
                    // SAFETY: the display is only ever dispatched from here, and
                    // calloop guarantees we are not re-entered.
                    unsafe { display.get_mut().dispatch_clients(state)? };
                    Ok(PostAction::Continue)
                },
            )
            .map_err(|e| anyhow::anyhow!("failed to insert display source: {e}"))?;

        Ok(Self {
            backend,
            lxb: Lxb {
                display_handle,
                loop_handle,
                loop_signal,
                start_time: Instant::now(),
                socket_name,
                xwayland_display: None,
                xwayland_ready: false,
                xwayland_source: None,
                xwayland_startup_timeout: None,
                pending_autostart: Vec::new(),
                pending_shell: None,
                session_shell_pid: None,
                fatal_error: None,
                running: true,
                config,
                keybindings,
                home_tap: crate::input::HomeTap::default(),
                compositor_state,
                xdg_shell_state,
                xdg_decoration_state,
                layer_shell_state,
                shm_state,
                output_manager_state,
                seat_state,
                data_device_state,
                primary_selection_state,
                viewporter_state,
                presentation_state,
                activation_state,
                dmabuf_state,
                xwayland_shell_state,
                xwayland_keyboard_grab_state,
                shell_control,
                xwm: None,
                x11_focus_probe: None,
                last_xwayland_pointer: None,
                last_synced_pointer: None,
                space: Space::default(),
                popups: PopupManager::default(),
                outputs: OutputManager::new(),
                overview: crate::overview::Overviews::default(),
                restores: crate::restore::Restores::default(),
                flashes: crate::flash::Flashes::default(),
                screencopy,
                hdr: crate::hdr::Manager::default(),
                seat,
                pointer_location: (0.0, 0.0).into(),
                nested_host_pointer_location: None,
                pointer_position_hint: None,
                cursor_status: smithay::input::pointer::CursorImageStatus::default_named(),
                // Nothing has moved a pointer yet, and on a machine with no
                // mouse plugged in nothing ever will.
                pointer_visible: false,
                keyboard_focus_enabled: true,
                exclusive_keyboard_focus: None,
                unseen: std::collections::HashSet::new(),
            },
        })
    }

    /// Start LineXinBar's private XWayland server, then launch the desktop once
    /// both the X server and its window manager are ready. Wayland-only
    /// startup remains available when XWayland is missing or fails.
    ///
    /// `shell`, when set, is the session shell: it is started with the
    /// autostart list but supervised, so that quitting it ends the session.
    pub fn start_xwayland(&mut self, commands: Vec<String>, shell: Option<String>) {
        self.lxb.pending_autostart = commands;
        self.lxb.pending_shell = shell;

        let (xwayland, client) = match XWayland::spawn(
            &self.lxb.display_handle,
            None,
            std::iter::empty::<(String, String)>(),
            true,
            std::process::Stdio::null(),
            std::process::Stdio::null(),
            |_| (),
        ) {
            Ok(result) => result,
            Err(err) => {
                tracing::warn!(?err, "XWayland unavailable; starting Wayland-only session");
                self.launch_pending_autostart();
                return;
            }
        };

        let x11_display_name = format!(":{}", xwayland.display_number());
        tracing::info!(x11_display = %x11_display_name, "starting private XWayland server");

        let handle = self.lxb.loop_handle.clone();
        let insert = handle.insert_source(xwayland, move |event, _, state| match event {
            XWaylandEvent::Ready {
                x11_socket,
                display_number,
            } => match X11Wm::start_wm(
                state.lxb.loop_handle.clone(),
                x11_socket,
                client.clone(),
            ) {
                Ok(wm) => {
                    state.cancel_xwayland_timeout_later();
                    state.lxb.xwm = Some(wm);
                    let display = format!(":{display_number}");
                    state.lxb.x11_focus_probe = match X11FocusProbe::connect(&display) {
                        Ok(probe) => Some(probe),
                        Err(err) => {
                            tracing::warn!(
                                ?err,
                                "cannot inspect X11 WM_PROTOCOLS; input=false windows will not take focus"
                            );
                            None
                        }
                    };
                    state.lxb.xwayland_display = Some(display);
                    state.lxb.xwayland_ready = true;
                    state.watch_xwayland_pointer();
                    tracing::info!(
                        display = format_args!(":{display_number}"),
                        "XWayland ready"
                    );
                    state.launch_pending_autostart();
                }
                Err(err) => {
                    tracing::error!(?err, "failed to start XWayland window manager");
                    state.cancel_xwayland_timeout_later();
                    state.lxb.xwm = None;
                    state.lxb.x11_focus_probe = None;
                    state.lxb.xwayland_display = None;
                    state.lxb.xwayland_ready = false;
                    state.discard_xwayland_source_later();
                    state.launch_pending_autostart();
                }
            },
            XWaylandEvent::Error => {
                tracing::warn!("XWayland exited during startup; continuing Wayland-only");
                state.cancel_xwayland_timeout_later();
                state.lxb.xwm = None;
                state.lxb.x11_focus_probe = None;
                state.lxb.xwayland_display = None;
                state.lxb.xwayland_ready = false;
                state.discard_xwayland_source_later();
                state.launch_pending_autostart();
            }
        });

        let source_token = match insert {
            Ok(token) => token,
            Err(err) => {
                tracing::error!(?err, "failed to add XWayland to the event loop");
                self.lxb.xwm = None;
                self.lxb.xwayland_display = None;
                self.lxb.xwayland_ready = false;
                self.launch_pending_autostart();
                return;
            }
        };
        self.lxb.xwayland_source = Some(source_token);

        // Smithay 0.7 treats EOF on displayfd as "not ready yet". Without a
        // deadline, an XWayland process that exits before writing its display
        // number can therefore leave the shell autostart pending forever and
        // repeatedly wake the level-triggered source.
        let timeout_insert = handle.insert_source(
            Timer::from_duration(XWAYLAND_STARTUP_TIMEOUT),
            move |_, _, state| {
                state.lxb.xwayland_startup_timeout = None;
                if !state.lxb.xwayland_ready {
                    if let Some(token) = state.lxb.xwayland_source.take() {
                        state.lxb.loop_handle.remove(token);
                        tracing::warn!(
                            timeout_ms = XWAYLAND_STARTUP_TIMEOUT.as_millis(),
                            "XWayland startup timed out; starting Wayland-only session"
                        );
                        state.lxb.xwm = None;
                        state.lxb.x11_focus_probe = None;
                        state.lxb.xwayland_display = None;
                        state.launch_pending_autostart();
                    }
                }
                TimeoutAction::Drop
            },
        );
        match timeout_insert {
            Ok(token) => self.lxb.xwayland_startup_timeout = Some(token),
            Err(err) => {
                tracing::error!(?err, "failed to install XWayland startup timeout");
                handle.remove(source_token);
                self.lxb.xwayland_source = None;
                self.lxb.xwm = None;
                self.lxb.x11_focus_probe = None;
                self.lxb.xwayland_display = None;
                self.lxb.xwayland_ready = false;
                self.launch_pending_autostart();
            }
        }
    }

    /// A source cannot remove itself from inside its calloop callback. Queue
    /// the removal as an idle so a failed startup still drops the XWayland
    /// client and child cleanly before Wayland-only autostart proceeds.
    fn discard_xwayland_source_later(&mut self) {
        let Some(token) = self.lxb.xwayland_source.take() else {
            return;
        };
        let handle = self.lxb.loop_handle.clone();
        handle.insert_idle(move |state| state.lxb.loop_handle.remove(token));
    }

    /// Keep the cursor with XWayland's pointer for as long as XWayland lives.
    ///
    /// A poll rather than an event, because there is nothing to subscribe to:
    /// XTEST motion is handled inside XWayland and announced to nobody. One
    /// round trip over a unix socket every tick is cheap, and it stops the
    /// moment XWayland goes away, because the probe goes with it.
    fn watch_xwayland_pointer(&mut self) {
        let insert = self.lxb.loop_handle.insert_source(
            Timer::from_duration(XWAYLAND_POINTER_INTERVAL),
            |_, _, state| {
                if state.lxb.x11_focus_probe.is_none() {
                    return TimeoutAction::Drop;
                }
                state.follow_xwayland_pointer();
                TimeoutAction::ToDuration(XWAYLAND_POINTER_INTERVAL)
            },
        );
        if let Err(err) = insert {
            tracing::warn!(
                ?err,
                "cannot watch XWayland's pointer; a cursor moved by X11 synthetic input \
                 will not be drawn where that input believes it is"
            );
        }
    }

    fn cancel_xwayland_timeout_later(&mut self) {
        let Some(token) = self.lxb.xwayland_startup_timeout.take() else {
            return;
        };
        let handle = self.lxb.loop_handle.clone();
        handle.insert_idle(move |state| state.lxb.loop_handle.remove(token));
    }

    fn launch_pending_autostart(&mut self) {
        let owns_seat = self.owns_the_seat();
        self.lxb.update_dbus_activation_environment(owns_seat);
        // Before anything in this session can ask for a portal, and for a
        // reason the pair of calls makes plain: a desktop portal reads *both*
        // which desktop it serves and what that desktop's backend can do
        // exactly once, when it starts. One left over from before this session
        // — from the login screen, from another desktop, from this session's
        // own predecessor — answers with whatever it learnt then, and there is
        // no request that makes it look again.
        if owns_seat {
            stop_desktop_portal("so the one this session gets is built inside it");
        }
        for command in std::mem::take(&mut self.lxb.pending_autostart) {
            self.lxb.spawn(&command);
        }
        if let Some(command) = self.lxb.pending_shell.take() {
            self.start_session_shell(&command);
            self.lxb.start_portal();
        }
    }

    /// Whether this compositor is the machine's session rather than a window
    /// inside somebody else's.
    ///
    /// The DRM backend is the whole of the answer: it drives the connectors and
    /// holds the seat, which is what "this is the session" means. The nested
    /// backends are a window on a desktop that already has one.
    pub fn owns_the_seat(&self) -> bool {
        matches!(self.backend, Backend::Udev(_))
    }

    /// Let go of the session-wide services this session caused to start.
    ///
    /// The desktop portal is the one that matters. It is one service per *user*
    /// rather than one per session, it reads which desktop it belongs to once
    /// at startup, and nothing restarts it — so a portal left running with
    /// LineXinBar's answers is inherited by whatever the user logs into next,
    /// and screen sharing is broken there instead. Stopping it here is what
    /// keeps a session from outliving itself: the next thing to ask for a
    /// portal gets a fresh one, started from that session's own environment.
    ///
    /// Only where the seat was ours to begin with. Nested inside another
    /// desktop the portal belongs to that desktop, and `systemctl --user`
    /// reaches the same user manager either way — so a development session
    /// ending must not take the host's portal down with it.
    pub fn release_session_services(&self) {
        if !self.owns_the_seat() {
            return;
        }
        stop_desktop_portal("so nothing after this session inherits its answers");
    }

    /// Start the session shell and end the session when it exits.
    ///
    /// Unlike [`Lxb::spawn`] this keeps the real child rather than
    /// double-forking it away, because the whole point is to know when it
    /// stops. Failing to start it is fatal: `--shell` promises a usable
    /// session, and a bare compositor with no shell is a black screen the user
    /// cannot get out of.
    fn start_session_shell(&mut self, command: &str) {
        let mut child = match self.lxb.command_for(command) {
            Some(mut cmd) => match cmd.spawn() {
                Ok(child) => child,
                Err(err) => {
                    self.fail_session(format!(
                        "could not start the session shell {command:?}: {err}"
                    ));
                    return;
                }
            },
            None => {
                self.fail_session(format!(
                    "could not parse the session shell command {command:?}"
                ));
                return;
            }
        };

        tracing::info!(command, pid = child.id(), "session shell started");
        self.lxb.session_shell_pid = Some(child.id() as i32);

        let command = command.to_string();
        let poll = self
            .lxb
            .loop_handle
            .insert_source(Timer::from_duration(SESSION_SHELL_POLL), move |_, _, state| {
                match child.try_wait() {
                    Ok(None) => TimeoutAction::ToDuration(SESSION_SHELL_POLL),
                    Ok(Some(status)) => {
                        if status.success() {
                            tracing::info!(command, %status, "session shell exited; ending session");
                        } else {
                            state.lxb.fatal_error = Some(format!(
                                "session shell {command:?} exited unsuccessfully: {status}"
                            ));
                        }
                        state.lxb.running = false;
                        state.lxb.loop_signal.stop();
                        TimeoutAction::Drop
                    }
                    Err(err) => {
                        // Without a usable child status the session can no
                        // longer be tied to the shell. Say so and stop
                        // polling rather than spinning on the same error.
                        tracing::error!(command, ?err, "cannot supervise the session shell");
                        TimeoutAction::Drop
                    }
                }
            });

        if let Err(err) = poll {
            self.fail_session(format!("could not supervise the session shell: {err}"));
        }
    }

    fn fail_session(&mut self, error: String) {
        tracing::error!(%error);
        self.lxb.fatal_error = Some(error);
        self.lxb.running = false;
        self.lxb.loop_signal.stop();
    }
}

/// How a name is folded before the two sides of [`Lxb::out_of_sight`] are
/// compared, or `None` for one that names nothing.
///
/// Both the name the shell asks about and the name a window calls itself go
/// through this, and that is the whole point of its existing: an X11 class is
/// conventionally capitalised where the same program's Wayland app id would
/// not be, and a shell naming an application should not have to know which of
/// the two it will turn out to be talking about.
///
/// Nothing is what an empty name matches. Folding it to a `None` here rather
/// than to an empty string is what keeps it out of the set in the first place,
/// so a request naming nothing cannot go on to hide every window whose client
/// never set an app id.
pub(crate) fn folded_app_id(name: &str) -> Option<String> {
    let name = name.trim().to_lowercase();
    (!name.is_empty()).then_some(name)
}

impl Lxb {
    /// Whether this window belongs to an application the shell is keeping off
    /// the screen.
    ///
    /// Asked wherever a window would otherwise be drawn, listed, focused or
    /// clicked, so that one answer covers every way a window can be noticed.
    /// It is deliberately read from the window's *current* `app_id` rather
    /// than decided once when it maps: an X11 client sets its class before it
    /// is mapped but a Wayland one may not, and a window that arrived nameless
    /// has to start being hidden the moment it says what it is.
    pub fn out_of_sight(&self, window: &Window) -> bool {
        // The overwhelmingly common case, and the reason nothing else here
        // has to be fast: no session with nothing to hide pays for any of it.
        if self.unseen.is_empty() {
            return false;
        }
        let app_id = crate::shell_control::window_app_id(window);
        folded_app_id(&app_id).is_some_and(|app_id| self.unseen.contains(&app_id))
    }

    /// Tell the bus what this session is, so that everything it starts on
    /// demand is started *into* the session rather than beside it.
    ///
    /// A D-Bus activated service inherits nothing from the process that asked
    /// for it: it is started by the bus, from the bus's own snapshot of the
    /// environment. So a session that never updates that snapshot has every
    /// activated service come up blind — with no display to draw on and, worse,
    /// no idea which desktop it is part of.
    ///
    /// The desktop portal is the one that matters, and it is the reason this is
    /// no longer confined to the nested case. `xdg-desktop-portal` chooses its
    /// backend from `XDG_CURRENT_DESKTOP` *once*, when it starts: activated
    /// without it, it finds no backend able to share a screen and answers every
    /// application that asks — OBS, Discord, a browser — with a portal that has
    /// no ScreenCast interface on it at all. That is not an error anybody sees;
    /// it looks exactly like an application that cannot capture screens.
    ///
    /// `owns_seat` is what keeps this from happening the other way round. On a
    /// nested backend the bus belongs to the desktop LineXinBar is a window on,
    /// and rewriting its snapshot would send *its* activated services into this
    /// compositor. There the marker is still required: `LXB_PRIVATE_DBUS=1`
    /// says the session was given a bus of its own — see `scripts/run-nested.sh`
    /// — and only then is the snapshot ours to write.
    fn update_dbus_activation_environment(&self, owns_seat: bool) {
        let private =
            std::env::var_os("LXB_PRIVATE_DBUS").as_deref() == Some(std::ffi::OsStr::new("1"));
        if !owns_seat && !private {
            tracing::debug!(
                "nested on somebody else's bus; leaving its activation environment alone"
            );
            return;
        }

        let x11 = self.xwayland_display.as_deref().unwrap_or("");
        let assignments = [
            format!("WAYLAND_DISPLAY={}", self.socket_name),
            format!("DISPLAY={x11}"),
            format!("LXB_XWAYLAND_DISPLAY={x11}"),
            "XDG_SESSION_TYPE=wayland".to_string(),
            "XDG_CURRENT_DESKTOP=LineXinBar".to_string(),
            "XDG_SESSION_DESKTOP=LineXinBar".to_string(),
            "DESKTOP_SESSION=lxb".to_string(),
        ];
        // `--systemd` as well as the bus, because on a systemd machine the user
        // manager is what starts the portal and it keeps a snapshot of its own.
        // The flag is ignored where there is no user manager to tell.
        match std::process::Command::new("dbus-update-activation-environment")
            .arg("--systemd")
            .args(assignments)
            .env_remove("WAYLAND_SOCKET")
            .status()
        {
            Ok(status) if status.success() => {
                tracing::info!(
                    display = %self.socket_name,
                    "the bus knows what this session is"
                )
            }
            Ok(status) => tracing::warn!(
                ?status,
                "failed to update the D-Bus activation environment; screen sharing will not work"
            ),
            Err(err) => tracing::warn!(
                ?err,
                "dbus-update-activation-environment is unavailable; D-Bus activation \
                 will not inherit LineXinBar's displays, and screen sharing will not work"
            ),
        }
    }

    /// Find the window owning `surface`, at any depth (toplevel, subsurface or popup).
    pub fn window_for_surface(&self, surface: &WlSurface) -> Option<Window> {
        self.space
            .elements()
            .find(|w| {
                // Almost every caller passes a root toplevel surface, so try
                // that before walking anything.
                if w.wl_surface().is_some_and(|s| &*s == surface) {
                    return true;
                }
                // `with_surfaces` covers the toplevel's subsurface tree and
                // every popup attached to it.
                let mut found = false;
                w.with_surfaces(|s, _| found |= s == surface);
                found
            })
            .cloned()
    }

    /// Build a child process confined to this session, ready to spawn.
    ///
    /// Returns `None` only when the command line cannot be parsed.
    pub fn command_for(&self, command: &str) -> Option<std::process::Command> {
        let argv = shell_split(command)?;
        let (program, args) = argv.split_first()?;

        let mut cmd = std::process::Command::new(sibling_binary(program));
        cmd.args(args).stdin(std::process::Stdio::null());

        for (k, v) in &self.config.general.env {
            cmd.env(k, v);
        }

        // Apply the display boundary after the configured environment so even
        // an accidental DISPLAY/WAYLAND_SOCKET entry cannot send autostarted
        // clients back to the host session when LineXinBar is nested.
        confine_to_session(
            &mut cmd,
            &self.socket_name,
            self.xwayland_display.as_deref(),
        );

        Some(cmd)
    }

    /// Start the session's desktop portal.
    ///
    /// This is what an application outside the session asks when it wants a
    /// piece of it — a screen to share, and in time the rest of what a portal
    /// answers. It is started here, beside the shell, rather than being left to
    /// D-Bus activation, for one reason: it is a client of this compositor, so
    /// it needs this session's `WAYLAND_DISPLAY`, and a portal activated by a
    /// bus that has not been told about the session yet would come up unable to
    /// see the very thing it exists to hand over.
    ///
    /// Only with `--shell`. A bare compositor is somebody debugging, and a
    /// second portal claiming the bus name in a session that already has one is
    /// worse than no portal at all.
    fn start_portal(&self) {
        // By name rather than by path, so a portal built beside the compositor
        // and one installed by a package are both found the way everything else
        // in the session is.
        self.spawn("lxb-portal");
    }

    /// Spawn a command, detached from the compositor's own process group.
    pub fn spawn(&self, command: &str) {
        let Some(mut cmd) = self.command_for(command) else {
            tracing::warn!(command, "could not parse command");
            return;
        };

        // Double-fork so children are reaped by init instead of zombifying here.
        unsafe {
            use std::os::unix::process::CommandExt;
            cmd.pre_exec(|| {
                match libc::fork() {
                    -1 => return Err(std::io::Error::last_os_error()),
                    0 => {}
                    _ => libc::_exit(0),
                }
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        match cmd.spawn() {
            Ok(mut child) => {
                let _ = child.wait();
                tracing::info!(command, "spawned");
            }
            Err(err) => tracing::warn!(command, ?err, "failed to spawn"),
        }
    }
}

/// Take the desktop portal down, so that the next one is built from scratch.
///
/// Called at both ends of a session that owns the seat, and it is the same act
/// both times: `xdg-desktop-portal` decides which backend answers screen
/// sharing — and asks that backend, once, what it can do — while it is
/// starting, and there is nothing that makes it ask again. A portal started a
/// moment before this session's own backend claimed its bus name therefore
/// believes for ever that the backend cannot embed a cursor, cannot hide one,
/// cannot do anything at all: the frontend's cached copy of those properties is
/// empty and every request that names one is refused as unavailable. OBS's
/// screen capture failed at exactly that, with the portal and the backend both
/// running and both perfectly well.
///
/// So the session ends whatever portal it inherited before its own backend
/// starts, and ends its own on the way out. What starts one again is the first
/// application to want one, which is long after the backend is listening.
///
/// Never fatal, and barely worth a line at the failure: a machine with no
/// systemd user manager never started a portal this way, and a portal that is
/// not running cannot be stale.
fn stop_desktop_portal(why: &str) {
    let stopped = std::process::Command::new("systemctl")
        .args(["--user", "stop", "xdg-desktop-portal.service"])
        .status();
    match stopped {
        Ok(status) if status.success() => tracing::info!(why, "stopped the desktop portal"),
        Ok(status) => tracing::debug!(?status, why, "could not stop the desktop portal"),
        Err(err) => tracing::debug!(?err, "systemctl is unavailable"),
    }
}

/// Prefer a companion binary sitting next to the running compositor.
///
/// This is what makes `target/release/lxb --shell` pick up the matching
/// `target/release/lxb-desktop` instead of an older installed copy, without
/// anyone having to spell out a path. Bare names only: anything already
/// containing a separator is the user being explicit.
fn sibling_binary(program: &str) -> std::ffi::OsString {
    if program.contains('/') {
        return program.into();
    }
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(program)))
        .filter(|path| path.is_file());

    match sibling {
        Some(path) => {
            tracing::debug!(path = %path.display(), "using companion binary");
            path.into_os_string()
        }
        None => program.into(),
    }
}

/// Pin a child process to LineXinBar's Wayland socket.
///
/// `WAYLAND_SOCKET` takes precedence over `WAYLAND_DISPLAY` in Wayland client
/// libraries. It is normally a connected file descriptor and cannot be shared
/// by another client, so inheriting the outer compositor's value is both wrong
/// and unsafe. Likewise, retaining the outer `DISPLAY` would let an X11-only
/// application escape the nested session. Only the display name produced by
/// LineXinBar's own XWayland server is forwarded.
fn confine_to_session(
    command: &mut std::process::Command,
    wayland_display: &str,
    xwayland_display: Option<&str>,
) {
    command
        .env("WAYLAND_DISPLAY", wayland_display)
        .env_remove("WAYLAND_SOCKET")
        .env_remove("LXB_HOST_WAYLAND_DISPLAY")
        .env_remove("LXB_HOST_WAYLAND_SOCKET")
        .env_remove("LXB_HOST_DISPLAY")
        // Do not let nested applications mistake the outer KDE/GNOME session
        // for the desktop they are actually running in. Toolkit backend
        // selection should consistently prefer LineXinBar's Wayland socket.
        .env("XDG_SESSION_TYPE", "wayland")
        .env("XDG_CURRENT_DESKTOP", "LineXinBar")
        .env("XDG_SESSION_DESKTOP", "LineXinBar")
        .env("DESKTOP_SESSION", "lxb")
        // Activation tokens belong to the compositor that issued them; an
        // inherited host token is invalid in this session.
        .env_remove("XDG_ACTIVATION_TOKEN")
        .env_remove("DESKTOP_STARTUP_ID");

    if let Some(display) = xwayland_display {
        command
            .env("DISPLAY", display)
            .env("LXB_XWAYLAND_DISPLAY", display);
    } else {
        command
            .env_remove("DISPLAY")
            .env_remove("LXB_XWAYLAND_DISPLAY");
    }
}

/// Minimal POSIX-ish word splitter: whitespace separated, with `'` and `"` quoting.
fn shell_split(input: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = input.chars().peekable();
    let mut has_word = false;

    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if has_word {
                    out.push(std::mem::take(&mut cur));
                    has_word = false;
                }
            }
            '\'' | '"' => {
                let quote = c;
                has_word = true;
                loop {
                    match chars.next() {
                        Some(c) if c == quote => break,
                        // Unterminated quote: reject rather than guess.
                        None => return None,
                        Some('\\') if quote == '"' => cur.push(chars.next()?),
                        Some(c) => cur.push(c),
                    }
                }
            }
            '\\' => {
                has_word = true;
                cur.push(chars.next()?);
            }
            c => {
                has_word = true;
                cur.push(c);
            }
        }
    }
    if has_word {
        out.push(cur);
    }
    Some(out)
}

#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, client_id: ClientId) {
        tracing::debug!(?client_id, "client connected");
    }

    fn disconnected(&self, client_id: ClientId, reason: DisconnectReason) {
        tracing::debug!(?client_id, ?reason, "client disconnected");
    }
}

/// Helper used by handlers to reach a client's compositor state.
pub fn client_compositor_state(client: &Client) -> &CompositorClientState {
    if let Some(data) = client.get_data::<ClientState>() {
        return &data.compositor_state;
    }
    if let Some(data) = client.get_data::<XWaylandClientData>() {
        return &data.compositor_state;
    }
    panic!("client created without compositor state")
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::process::Command;

    use super::{confine_to_session, folded_app_id, shell_split};

    /// The name the shell asks about and the name the window carries have to
    /// meet, and this is the only place they are made to — so a client that
    /// capitalises its X11 class, as the convention is, is still the
    /// application the shell named in lower case.
    #[test]
    fn a_name_matches_however_the_client_capitalises_it() {
        assert_eq!(folded_app_id("steam"), folded_app_id("Steam"));
        assert_eq!(folded_app_id("Steam"), Some("steam".to_string()));
        assert_eq!(folded_app_id(" steam\n"), Some("steam".to_string()));
    }

    /// A name that is nothing must not become the empty string in the set,
    /// where it would match every window whose client never set an app id —
    /// which on a session full of X11 clients is most of them.
    #[test]
    fn a_name_that_is_nothing_names_nothing() {
        assert_eq!(folded_app_id(""), None);
        assert_eq!(folded_app_id("   "), None);
        assert_eq!(folded_app_id("\t\n"), None);
    }

    /// And it is a whole name, not a part of one: a shell hiding `steam` is
    /// not asking for somebody's `steam-rom-manager` to go too.
    #[test]
    fn a_name_matches_whole_or_not_at_all() {
        assert_ne!(folded_app_id("steam"), folded_app_id("steam-rom-manager"));
        assert_ne!(folded_app_id("steam"), folded_app_id("steamwebhelper"));
    }

    #[test]
    fn splits_plain_words() {
        assert_eq!(
            shell_split("foot -e htop").unwrap(),
            vec!["foot", "-e", "htop"]
        );
    }

    #[test]
    fn respects_quotes() {
        assert_eq!(
            shell_split(r#"sh -c "echo hello world""#).unwrap(),
            vec!["sh", "-c", "echo hello world"]
        );
        assert_eq!(
            shell_split("prog 'a b' c").unwrap(),
            vec!["prog", "a b", "c"]
        );
    }

    #[test]
    fn keeps_empty_quoted_arguments() {
        assert_eq!(shell_split(r#"prog "" x"#).unwrap(), vec!["prog", "", "x"]);
    }

    #[test]
    fn rejects_unterminated_quote() {
        assert!(shell_split(r#"prog "oops"#).is_none());
    }

    #[test]
    fn spawned_clients_are_pinned_to_the_inner_wayland_socket() {
        let mut command = Command::new("true");
        // Simulate values inherited from a nested host and a conflicting user
        // configuration. The session boundary must win over both.
        command
            .env("WAYLAND_DISPLAY", "host-wayland")
            .env("WAYLAND_SOCKET", "17")
            .env("DISPLAY", ":0")
            .env("LXB_HOST_WAYLAND_DISPLAY", "host-wayland")
            .env("LXB_HOST_WAYLAND_SOCKET", "17")
            .env("LXB_HOST_DISPLAY", ":0");

        confine_to_session(&mut command, "lxb-test", None);

        let get = |key: &str| {
            command
                .get_envs()
                .find(|(name, _)| *name == OsStr::new(key))
                .map(|(_, value)| value)
        };
        assert_eq!(get("WAYLAND_DISPLAY"), Some(Some(OsStr::new("lxb-test"))));
        assert_eq!(get("WAYLAND_SOCKET"), Some(None));
        assert_eq!(get("DISPLAY"), Some(None));
        assert_eq!(get("LXB_XWAYLAND_DISPLAY"), Some(None));
        assert_eq!(get("LXB_HOST_WAYLAND_DISPLAY"), Some(None));
        assert_eq!(get("LXB_HOST_WAYLAND_SOCKET"), Some(None));
        assert_eq!(get("LXB_HOST_DISPLAY"), Some(None));
        assert_eq!(get("XDG_SESSION_TYPE"), Some(Some(OsStr::new("wayland"))));
        assert_eq!(
            get("XDG_CURRENT_DESKTOP"),
            Some(Some(OsStr::new("LineXinBar")))
        );
        assert_eq!(get("XDG_ACTIVATION_TOKEN"), Some(None));
    }

    #[test]
    fn spawned_clients_receive_only_the_private_xwayland_display() {
        let mut command = Command::new("true");
        command
            .env("DISPLAY", ":0")
            .env("LXB_XWAYLAND_DISPLAY", ":0");

        confine_to_session(&mut command, "lxb-test", Some(":47"));

        let get = |key: &str| {
            command
                .get_envs()
                .find(|(name, _)| *name == OsStr::new(key))
                .map(|(_, value)| value)
        };
        assert_eq!(get("DISPLAY"), Some(Some(OsStr::new(":47"))));
        assert_eq!(get("LXB_XWAYLAND_DISPLAY"), Some(Some(OsStr::new(":47"))));
    }
}
