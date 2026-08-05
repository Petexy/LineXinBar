//! Global compositor state and the Wayland protocol handler implementations.
//!
//! The state is deliberately split in two halves:
//!
//! * [`Linboard`] owns everything backend independent — protocol globals, the
//!   window [`Space`], the seat, configuration.
//! * [`Backend`] owns whatever the active backend needs (a winit window, or a
//!   set of DRM devices).
//!
//! [`LinboardState`] simply holds both. Keeping them in separate fields means a
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

/// How often the session shell is checked for having exited.
///
/// A child cannot wake calloop by itself, so this is a poll. It only runs in
/// `--shell` mode, and a fifth of a second is far below the threshold at which
/// a logout feels unresponsive.
const SESSION_SHELL_POLL: Duration = Duration::from_millis(200);

/// Top level state handed to every calloop callback and protocol dispatch.
pub struct LinboardState {
    pub backend: Backend,
    pub linboard: Linboard,
}

/// Backend independent compositor state.
pub struct Linboard {
    pub display_handle: DisplayHandle,
    pub loop_handle: LoopHandle<'static, LinboardState>,
    pub loop_signal: LoopSignal,
    pub start_time: Instant,
    pub socket_name: String,
    /// Display name of Linboard's private XWayland server once it is ready.
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
    /// Fatal session errors are reported after calloop has unwound, allowing
    /// sockets and clients to be dropped normally while still giving a
    /// service manager a non-zero exit status to restart.
    pub fatal_error: Option<String>,
    pub running: bool,

    pub config: Config,
    pub keybindings: KeyBindings,

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
    pub seat_state: SeatState<LinboardState>,
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

    // Desktop.
    pub space: Space<Window>,
    pub popups: PopupManager,
    pub outputs: OutputManager,
    /// Which display is showing the window overview, and how far along its
    /// enter/leave animation is.
    pub overview: crate::overview::Overviews,

    // Input.
    pub seat: Seat<LinboardState>,
    pub pointer_location: Point<f64, Logical>,
    /// Last host coordinate from an absolute-only nested backend, used to
    /// synthesize wp_relative_pointer deltas for nested testing.
    pub nested_host_pointer_location: Option<Point<f64, Logical>>,
    /// Client-provided position to restore after an active pointer lock ends.
    pub pointer_position_hint: Option<(WlSurface, Point<f64, Logical>)>,
    pub cursor_status: smithay::input::pointer::CursorImageStatus,
    /// Whether the parent has granted this nested compositor keyboard focus.
    /// Always true on the native backend.
    pub keyboard_focus_enabled: bool,
    /// Surface the keyboard focus is pinned to by a layer surface requesting
    /// exclusive interactivity, if any.
    pub exclusive_keyboard_focus: Option<WlSurface>,
}

impl LinboardState {
    /// Build the state, create the Wayland socket and register it with the loop.
    pub fn new(
        display: Display<LinboardState>,
        loop_handle: LoopHandle<'static, LinboardState>,
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
        let shm_state = ShmState::new::<Self>(dh, Vec::new());
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

        smithay::wayland::fractional_scale::FractionalScaleManagerState::new::<Self>(dh);
        smithay::wayland::relative_pointer::RelativePointerManagerState::new::<Self>(dh);
        smithay::wayland::pointer_constraints::PointerConstraintsState::new::<Self>(dh);
        smithay::wayland::pointer_gestures::PointerGesturesState::new::<Self>(dh);
        smithay::wayland::single_pixel_buffer::SinglePixelBufferState::new::<Self>(dh);
        smithay::wayland::cursor_shape::CursorShapeManagerState::new::<Self>(dh);

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
                    .linboard
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
            linboard: Linboard {
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
                fatal_error: None,
                running: true,
                config,
                keybindings,
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
                space: Space::default(),
                popups: PopupManager::default(),
                outputs: OutputManager::new(),
                overview: crate::overview::Overviews::default(),
                seat,
                pointer_location: (0.0, 0.0).into(),
                nested_host_pointer_location: None,
                pointer_position_hint: None,
                cursor_status: smithay::input::pointer::CursorImageStatus::default_named(),
                keyboard_focus_enabled: true,
                exclusive_keyboard_focus: None,
            },
        })
    }

    /// Start Linboard's private XWayland server, then launch the desktop once
    /// both the X server and its window manager are ready. Wayland-only
    /// startup remains available when XWayland is missing or fails.
    ///
    /// `shell`, when set, is the session shell: it is started with the
    /// autostart list but supervised, so that quitting it ends the session.
    pub fn start_xwayland(&mut self, commands: Vec<String>, shell: Option<String>) {
        self.linboard.pending_autostart = commands;
        self.linboard.pending_shell = shell;

        let (xwayland, client) = match XWayland::spawn(
            &self.linboard.display_handle,
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

        let handle = self.linboard.loop_handle.clone();
        let insert = handle.insert_source(xwayland, move |event, _, state| match event {
            XWaylandEvent::Ready {
                x11_socket,
                display_number,
            } => match X11Wm::start_wm(
                state.linboard.loop_handle.clone(),
                x11_socket,
                client.clone(),
            ) {
                Ok(wm) => {
                    state.cancel_xwayland_timeout_later();
                    state.linboard.xwm = Some(wm);
                    let display = format!(":{display_number}");
                    state.linboard.x11_focus_probe = match X11FocusProbe::connect(&display) {
                        Ok(probe) => Some(probe),
                        Err(err) => {
                            tracing::warn!(
                                ?err,
                                "cannot inspect X11 WM_PROTOCOLS; input=false windows will not take focus"
                            );
                            None
                        }
                    };
                    state.linboard.xwayland_display = Some(display);
                    state.linboard.xwayland_ready = true;
                    tracing::info!(
                        display = format_args!(":{display_number}"),
                        "XWayland ready"
                    );
                    state.launch_pending_autostart();
                }
                Err(err) => {
                    tracing::error!(?err, "failed to start XWayland window manager");
                    state.cancel_xwayland_timeout_later();
                    state.linboard.xwm = None;
                    state.linboard.x11_focus_probe = None;
                    state.linboard.xwayland_display = None;
                    state.linboard.xwayland_ready = false;
                    state.discard_xwayland_source_later();
                    state.launch_pending_autostart();
                }
            },
            XWaylandEvent::Error => {
                tracing::warn!("XWayland exited during startup; continuing Wayland-only");
                state.cancel_xwayland_timeout_later();
                state.linboard.xwm = None;
                state.linboard.x11_focus_probe = None;
                state.linboard.xwayland_display = None;
                state.linboard.xwayland_ready = false;
                state.discard_xwayland_source_later();
                state.launch_pending_autostart();
            }
        });

        let source_token = match insert {
            Ok(token) => token,
            Err(err) => {
                tracing::error!(?err, "failed to add XWayland to the event loop");
                self.linboard.xwm = None;
                self.linboard.xwayland_display = None;
                self.linboard.xwayland_ready = false;
                self.launch_pending_autostart();
                return;
            }
        };
        self.linboard.xwayland_source = Some(source_token);

        // Smithay 0.7 treats EOF on displayfd as "not ready yet". Without a
        // deadline, an XWayland process that exits before writing its display
        // number can therefore leave the shell autostart pending forever and
        // repeatedly wake the level-triggered source.
        let timeout_insert = handle.insert_source(
            Timer::from_duration(XWAYLAND_STARTUP_TIMEOUT),
            move |_, _, state| {
                state.linboard.xwayland_startup_timeout = None;
                if !state.linboard.xwayland_ready {
                    if let Some(token) = state.linboard.xwayland_source.take() {
                        state.linboard.loop_handle.remove(token);
                        tracing::warn!(
                            timeout_ms = XWAYLAND_STARTUP_TIMEOUT.as_millis(),
                            "XWayland startup timed out; starting Wayland-only session"
                        );
                        state.linboard.xwm = None;
                        state.linboard.x11_focus_probe = None;
                        state.linboard.xwayland_display = None;
                        state.launch_pending_autostart();
                    }
                }
                TimeoutAction::Drop
            },
        );
        match timeout_insert {
            Ok(token) => self.linboard.xwayland_startup_timeout = Some(token),
            Err(err) => {
                tracing::error!(?err, "failed to install XWayland startup timeout");
                handle.remove(source_token);
                self.linboard.xwayland_source = None;
                self.linboard.xwm = None;
                self.linboard.x11_focus_probe = None;
                self.linboard.xwayland_display = None;
                self.linboard.xwayland_ready = false;
                self.launch_pending_autostart();
            }
        }
    }

    /// A source cannot remove itself from inside its calloop callback. Queue
    /// the removal as an idle so a failed startup still drops the XWayland
    /// client and child cleanly before Wayland-only autostart proceeds.
    fn discard_xwayland_source_later(&mut self) {
        let Some(token) = self.linboard.xwayland_source.take() else {
            return;
        };
        let handle = self.linboard.loop_handle.clone();
        handle.insert_idle(move |state| state.linboard.loop_handle.remove(token));
    }

    fn cancel_xwayland_timeout_later(&mut self) {
        let Some(token) = self.linboard.xwayland_startup_timeout.take() else {
            return;
        };
        let handle = self.linboard.loop_handle.clone();
        handle.insert_idle(move |state| state.linboard.loop_handle.remove(token));
    }

    fn launch_pending_autostart(&mut self) {
        self.linboard.update_private_dbus_activation_environment();
        for command in std::mem::take(&mut self.linboard.pending_autostart) {
            self.linboard.spawn(&command);
        }
        if let Some(command) = self.linboard.pending_shell.take() {
            self.start_session_shell(&command);
        }
    }

    /// Start the session shell and end the session when it exits.
    ///
    /// Unlike [`Linboard::spawn`] this keeps the real child rather than
    /// double-forking it away, because the whole point is to know when it
    /// stops. Failing to start it is fatal: `--shell` promises a usable
    /// session, and a bare compositor with no shell is a black screen the user
    /// cannot get out of.
    fn start_session_shell(&mut self, command: &str) {
        let mut child = match self.linboard.command_for(command) {
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

        let command = command.to_string();
        let poll = self
            .linboard
            .loop_handle
            .insert_source(Timer::from_duration(SESSION_SHELL_POLL), move |_, _, state| {
                match child.try_wait() {
                    Ok(None) => TimeoutAction::ToDuration(SESSION_SHELL_POLL),
                    Ok(Some(status)) => {
                        if status.success() {
                            tracing::info!(command, %status, "session shell exited; ending session");
                        } else {
                            state.linboard.fatal_error = Some(format!(
                                "session shell {command:?} exited unsuccessfully: {status}"
                            ));
                        }
                        state.linboard.running = false;
                        state.linboard.loop_signal.stop();
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
        self.linboard.fatal_error = Some(error);
        self.linboard.running = false;
        self.linboard.loop_signal.stop();
    }
}

impl Linboard {
    /// Replace the private bus daemon's startup snapshot with Linboard's own
    /// display names before any D-Bus-activated GUI can be requested.
    ///
    /// The marker is deliberately required: mutating an inherited desktop bus
    /// would redirect unrelated host services into this compositor.
    fn update_private_dbus_activation_environment(&self) {
        if std::env::var_os("LINBOARD_PRIVATE_DBUS").as_deref() != Some(std::ffi::OsStr::new("1")) {
            return;
        }

        let x11 = self.xwayland_display.as_deref().unwrap_or("");
        let assignments = [
            format!("WAYLAND_DISPLAY={}", self.socket_name),
            format!("DISPLAY={x11}"),
            format!("LINBOARD_XWAYLAND_DISPLAY={x11}"),
            "XDG_SESSION_TYPE=wayland".to_string(),
            "XDG_CURRENT_DESKTOP=Linboard".to_string(),
            "XDG_SESSION_DESKTOP=Linboard".to_string(),
            "DESKTOP_SESSION=linboard".to_string(),
        ];
        match std::process::Command::new("dbus-update-activation-environment")
            .args(assignments)
            .env_remove("WAYLAND_SOCKET")
            .status()
        {
            Ok(status) if status.success() => {
                tracing::debug!("updated private D-Bus activation environment")
            }
            Ok(status) => tracing::warn!(
                ?status,
                "failed to update private D-Bus activation environment"
            ),
            Err(err) => tracing::warn!(
                ?err,
                "dbus-update-activation-environment is unavailable; D-Bus activation may not inherit Linboard's displays"
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
        // clients back to the host session when Linboard is nested.
        confine_to_session(
            &mut cmd,
            &self.socket_name,
            self.xwayland_display.as_deref(),
        );

        Some(cmd)
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

/// Prefer a companion binary sitting next to the running compositor.
///
/// This is what makes `target/release/linboard --shell` pick up the matching
/// `target/release/linboard-xmb` instead of an older installed copy, without
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

/// Pin a child process to Linboard's Wayland socket.
///
/// `WAYLAND_SOCKET` takes precedence over `WAYLAND_DISPLAY` in Wayland client
/// libraries. It is normally a connected file descriptor and cannot be shared
/// by another client, so inheriting the outer compositor's value is both wrong
/// and unsafe. Likewise, retaining the outer `DISPLAY` would let an X11-only
/// application escape the nested session. Only the display name produced by
/// Linboard's own XWayland server is forwarded.
fn confine_to_session(
    command: &mut std::process::Command,
    wayland_display: &str,
    xwayland_display: Option<&str>,
) {
    command
        .env("WAYLAND_DISPLAY", wayland_display)
        .env_remove("WAYLAND_SOCKET")
        .env_remove("LINBOARD_HOST_WAYLAND_DISPLAY")
        .env_remove("LINBOARD_HOST_WAYLAND_SOCKET")
        .env_remove("LINBOARD_HOST_DISPLAY")
        // Do not let nested applications mistake the outer KDE/GNOME session
        // for the desktop they are actually running in. Toolkit backend
        // selection should consistently prefer Linboard's Wayland socket.
        .env("XDG_SESSION_TYPE", "wayland")
        .env("XDG_CURRENT_DESKTOP", "Linboard")
        .env("XDG_SESSION_DESKTOP", "Linboard")
        .env("DESKTOP_SESSION", "linboard")
        // Activation tokens belong to the compositor that issued them; an
        // inherited host token is invalid in this session.
        .env_remove("XDG_ACTIVATION_TOKEN")
        .env_remove("DESKTOP_STARTUP_ID");

    if let Some(display) = xwayland_display {
        command
            .env("DISPLAY", display)
            .env("LINBOARD_XWAYLAND_DISPLAY", display);
    } else {
        command
            .env_remove("DISPLAY")
            .env_remove("LINBOARD_XWAYLAND_DISPLAY");
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

    use super::{confine_to_session, shell_split};

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
            .env("LINBOARD_HOST_WAYLAND_DISPLAY", "host-wayland")
            .env("LINBOARD_HOST_WAYLAND_SOCKET", "17")
            .env("LINBOARD_HOST_DISPLAY", ":0");

        confine_to_session(&mut command, "linboard-test", None);

        let get = |key: &str| {
            command
                .get_envs()
                .find(|(name, _)| *name == OsStr::new(key))
                .map(|(_, value)| value)
        };
        assert_eq!(
            get("WAYLAND_DISPLAY"),
            Some(Some(OsStr::new("linboard-test")))
        );
        assert_eq!(get("WAYLAND_SOCKET"), Some(None));
        assert_eq!(get("DISPLAY"), Some(None));
        assert_eq!(get("LINBOARD_XWAYLAND_DISPLAY"), Some(None));
        assert_eq!(get("LINBOARD_HOST_WAYLAND_DISPLAY"), Some(None));
        assert_eq!(get("LINBOARD_HOST_WAYLAND_SOCKET"), Some(None));
        assert_eq!(get("LINBOARD_HOST_DISPLAY"), Some(None));
        assert_eq!(get("XDG_SESSION_TYPE"), Some(Some(OsStr::new("wayland"))));
        assert_eq!(
            get("XDG_CURRENT_DESKTOP"),
            Some(Some(OsStr::new("Linboard")))
        );
        assert_eq!(get("XDG_ACTIVATION_TOKEN"), Some(None));
    }

    #[test]
    fn spawned_clients_receive_only_the_private_xwayland_display() {
        let mut command = Command::new("true");
        command
            .env("DISPLAY", ":0")
            .env("LINBOARD_XWAYLAND_DISPLAY", ":0");

        confine_to_session(&mut command, "linboard-test", Some(":47"));

        let get = |key: &str| {
            command
                .get_envs()
                .find(|(name, _)| *name == OsStr::new(key))
                .map(|(_, value)| value)
        };
        assert_eq!(get("DISPLAY"), Some(Some(OsStr::new(":47"))));
        assert_eq!(
            get("LINBOARD_XWAYLAND_DISPLAY"),
            Some(Some(OsStr::new(":47")))
        );
    }
}
