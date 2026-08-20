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
use smithay::wayland::drm_syncobj::DrmSyncobjState;
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

/// Public, bounded visual state handed over by the display manager.
///
/// Keep the spelling in sync with `lxb-desktop::wallpaper_clock`. The crates
/// deliberately do not depend on one another: the compositor only transports
/// this opaque value, while the shell owns all validation and semantics.
const BACKGROUND_HANDOFF_ENV: &str = "LXB_BACKGROUND_HANDOFF";

/// How often to ask XWayland where its pointer is.
///
/// Fast enough that a cursor following synthetic X11 input does not visibly
/// trail it, and slow enough that the round trip costs nothing worth counting:
/// a local socket exchange at this rate is well under a thousandth of a core.
const XWAYLAND_POINTER_INTERVAL: Duration = Duration::from_millis(8);

/// How often the session shell is checked for having exited.
///
/// A child cannot wake calloop by itself, so this is a poll. It only runs in
/// `--shell` mode.
///
/// One frame, because this interval is *on screen*. The shell's surfaces go
/// away with the process that owned them, and every frame between that and
/// this timer noticing is a frame the compositor draws with no session in it —
/// [`crate::backdrop`] is what stands in it now, and a fifth of a second of a
/// still wallpaper is long enough to read as the session having stopped rather
/// than ended. It also delays the whole logout by that much: what greetd is
/// waiting for, on the way back to the login screen, is this compositor's own
/// exit. A `waitpid` that returns immediately, sixty times a second, is not a
/// cost worth weighing against either.
const SESSION_SHELL_POLL: Duration = Duration::from_millis(16);

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
    /// One-shot wallpaper state captured before the compositor creates any
    /// child. It is injected into the session shell alone, never XWayland,
    /// autostarts, the portal, or applications launched later.
    pub pending_shell_handoff: Option<std::ffi::OsString>,
    /// Whether the autostart list, the session shell and the portal have been
    /// started. They are started once, as early as a Wayland frame can be
    /// drawn, and every later XWayland outcome finds this already true.
    pub session_launched: bool,
    /// The wallpaper drawn whenever the session has nothing on screen, so the
    /// displays are never shown a black screen between the login screen and
    /// the shell's first frame — or between the shell's last frame and the
    /// login screen coming back. Kept for the session's whole life; see
    /// [`crate::backdrop`].
    pub backdrop: Option<crate::backdrop::Backdrop>,
    /// The same wallpaper before it has been drawn, for the first display this
    /// compositor lights to draw it from. Taken once: one picture serves every
    /// display. See [`crate::backdrop::Opening`].
    pub opening: Option<crate::backdrop::Opening>,
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
    /// The volume key being held down, which is the one binding that goes on
    /// acting while it is held: see [`crate::input::VolumeKey`].
    pub volume_key: crate::input::VolumeKey,

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
    /// Explicit synchronisation, once a DRM device has been opened to import
    /// timelines against.
    ///
    /// `None` until then, and `None` for good on a device whose kernel driver
    /// cannot wake us when a timeline point signals: the protocol is only
    /// advertised where the acquire point can actually be waited on, because a
    /// compositor that took the fences and ignored them would show clients
    /// half-drawn frames rather than blocking on them.
    ///
    /// The nested backends never set it. There is no DRM device of our own
    /// there — the host compositor owns the display — so a client inside one
    /// synchronises the way it did before.
    pub syncobj_state: Option<DrmSyncobjState>,
    /// How many client commits are being held right now waiting for the GPU
    /// work behind them to finish — an explicit acquire point that has not
    /// signalled yet.
    ///
    /// Normally none, or one for an instant. A number that stays up is a client
    /// whose frame this compositor is sitting on, which from the client's side
    /// is indistinguishable from a compositor that has stopped answering: see
    /// [`crate::render`], which prints this beside an application that has gone
    /// quiet.
    pub blocked_commits: usize,
    /// When the run of held commits started, cleared when the last one clears.
    pub blocked_since: Option<std::time::Instant>,
    pub xwayland_shell_state: XWaylandShellState,
    #[allow(dead_code)]
    pub xwayland_keyboard_grab_state: XWaylandKeyboardGrabState,
    pub shell_control: ShellControlState,
    pub xwm: Option<X11Wm>,
    /// `wp_tearing_control_v1`, kept only so the global lives as long as the
    /// session does. What a client sets through it is read off the surface.
    #[allow(dead_code)]
    pub tearing_control: crate::tearing::TearingControlState,
    /// `frog_color_management_v1`. Kept for the global's lifetime; what a
    /// client says through it is read off the surface. See [`crate::colour`].
    #[allow(dead_code)]
    pub colour: crate::colour::ColourState,
    /// `wp_color_manager_v1`, the same question asked the standard way. This
    /// one is read from as well as held: it keeps the live output and feedback
    /// objects that have to be told when a display changes what it is being
    /// driven as. See [`crate::colour_management`].
    pub colour_manager: crate::colour_management::ColourManagerState,
    pub x11_focus_probe: Option<X11FocusProbe>,
    /// The window last named in `_NET_ACTIVE_WINDOW`, so the property is only
    /// written when the answer changes — see
    /// `LxbState::name_the_active_x11_window`. `None` until the first one goes
    /// out, so that one is always written.
    pub x11_active_window: Option<u32>,
    /// The last disagreement reported by [`LxbState::check_xwayland_focus`] —
    /// the window we focused, and the one X says holds the keyboard. Kept only
    /// so a steady disagreement is logged once rather than at every poll.
    pub last_x11_focus_drift: Option<(u32, u32)>,
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
    /// Applications stopped because nothing of them is on screen.
    pub sleepers: crate::sleep::Sleepers,
    /// Displays answering for a screenshot that has just been taken of them.
    pub flashes: crate::flash::Flashes,
    /// The black over every display, while the session is on its way out.
    pub curtain: crate::curtain::Curtain,
    /// The black over one display, while it rests behind a game being played
    /// on another. Separate from the curtain above it because it is a
    /// different statement — that one is about the session leaving, this one
    /// is about a panel nobody is looking at. See [`crate::blackout`].
    pub blackouts: crate::blackout::Blackouts,
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
    /// How often a confinement has refused to let the pointer move, and when
    /// that was last said out loud — see
    /// [`crate::input::watch_a_refused_pointer`], which is the only thing that
    /// reads or writes it.
    pub pointer_refused: Option<crate::input::Repeatedly>,
    /// The same for the pointer being held on screen, having been driven off
    /// every display — see `crate::input::watch_a_clamped_pointer`.
    pub pointer_clamped: Option<crate::input::Repeatedly>,
    /// When a client last placed the pointer itself, and how many times it has
    /// done so since — see `crate::input::watch_a_client_placing_the_pointer`.
    pub pointer_hints: Option<(std::time::Instant, u32)>,
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

    /// The applications the shell says are playing something, by the name each
    /// calls itself, folded the same way [`Lxb::unseen`] is.
    ///
    /// The one exception to the sleeper. An application with nothing on screen
    /// is stopped — see [`crate::sleep`] — and that is right for everything a
    /// program does out of sight except the one thing somebody is deliberately
    /// listening to. Which of the two a sound is cannot be seen from here: it
    /// is a fact about a media player on the session bus, and the shell is the
    /// half of this session that is on one.
    ///
    /// Empty in a session where nothing is playing, and in every session with
    /// no shell, which is what keeps it free. See [`Lxb::media_is_playing`] and
    /// `lxb_shell_v1.keep_awake`.
    pub playing: std::collections::HashSet<String>,
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
        let tearing_control = crate::tearing::TearingControlState::new::<Self>(dh);
        let colour = crate::colour::ColourState::new::<Self>(dh);
        let colour_manager = crate::colour_management::ColourManagerState::new::<Self>(dh);
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
                pending_shell_handoff: None,
                session_launched: false,
                backdrop: None,
                opening: None,
                session_shell_pid: None,
                fatal_error: None,
                running: true,
                config,
                keybindings,
                home_tap: crate::input::HomeTap::default(),
                volume_key: crate::input::VolumeKey::default(),
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
                syncobj_state: None,
                blocked_commits: 0,
                blocked_since: None,
                xwayland_shell_state,
                xwayland_keyboard_grab_state,
                shell_control,
                xwm: None,

                tearing_control,
                colour,
                colour_manager,
                x11_focus_probe: None,
                x11_active_window: None,
                last_x11_focus_drift: None,
                last_xwayland_pointer: None,
                last_synced_pointer: None,
                space: Space::default(),
                popups: PopupManager::default(),
                outputs: OutputManager::new(),
                overview: crate::overview::Overviews::default(),
                restores: crate::restore::Restores::default(),
                sleepers: crate::sleep::Sleepers::default(),
                flashes: crate::flash::Flashes::default(),
                curtain: crate::curtain::Curtain::default(),
                blackouts: crate::blackout::Blackouts::default(),
                screencopy,
                hdr: crate::hdr::Manager::default(),
                seat,
                pointer_location: (0.0, 0.0).into(),
                nested_host_pointer_location: None,
                pointer_position_hint: None,
                pointer_refused: None,
                pointer_clamped: None,
                pointer_hints: None,
                cursor_status: smithay::input::pointer::CursorImageStatus::default_named(),
                // Nothing has moved a pointer yet, and on a machine with no
                // mouse plugged in nothing ever will.
                pointer_visible: false,
                keyboard_focus_enabled: true,
                exclusive_keyboard_focus: None,
                unseen: std::collections::HashSet::new(),
                playing: std::collections::HashSet::new(),
            },
        })
    }

    /// Start LineXinBar's private XWayland server and the session beside it.
    ///
    /// The two are deliberately not in sequence. XWayland's handshake — fork,
    /// exec, the server's own initialisation, then the window manager's
    /// connection — is several hundred milliseconds during which the session
    /// shell does not exist and nothing is on screen. Waiting for it was the
    /// single longest black interval in a login, and nothing in it is needed
    /// to draw a Wayland frame.
    ///
    /// What a client actually needs is the display *name*, and that is settled
    /// by [`XWayland::spawn`]: it creates and binds the X11 sockets before
    /// returning, so `:N` is connectable from that moment on. An X11 program
    /// started in the next few milliseconds blocks on the socket until the
    /// server answers, which is the ordinary behaviour of every X client on
    /// every machine, rather than failing to find a display.
    ///
    /// `shell`, when set, is the session shell: it is started with the
    /// autostart list but supervised, so that quitting it ends the session.
    pub fn start_xwayland(
        &mut self,
        commands: Vec<String>,
        shell: Option<String>,
        background_handoff: Option<std::ffi::OsString>,
    ) {
        self.lxb.pending_autostart = commands;
        self.lxb.pending_shell = shell;

        // Before the session exists, so the very first frame the compositor
        // presents is already the wallpaper. Reading the record does not
        // consume it: `pending_shell_handoff` still carries it, unaltered, to
        // the one child entitled to it.
        //
        // Only where a display has not already drawn it. On the DRM backend
        // the first frame on a connector is committed while that connector is
        // being lit, which is long before this and is where the wallpaper is
        // actually needed; the nested backends draw nothing until the event
        // loop runs, so this is their first frame. See
        // [`crate::backdrop::Opening`].
        if self.lxb.pending_shell.is_some() && self.lxb.backdrop.is_none() {
            if let Some(opening) =
                crate::backdrop::Opening::of_a_session(true, background_handoff.as_deref())
            {
                self.lxb.backdrop = Some(opening.start(self.primary_aspect()));
            }
        }
        self.lxb.pending_shell_handoff = background_handoff;

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
        // Published now rather than on `Ready`, so the session started below
        // hands its own children a working `DISPLAY` from its first moment.
        self.lxb.xwayland_display = Some(x11_display_name);

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
                    state.forget_xwayland_display();
                    state.discard_xwayland_source_later();
                    state.launch_pending_autostart();
                }
            },
            XWaylandEvent::Error => {
                tracing::warn!("XWayland exited during startup; continuing Wayland-only");
                state.cancel_xwayland_timeout_later();
                state.lxb.xwm = None;
                state.lxb.x11_focus_probe = None;
                state.forget_xwayland_display();
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
                        state.forget_xwayland_display();
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
            }
        }

        // Beside XWayland, not behind it. Every path above that gave up on an
        // X server has already cleared the display name, so a Wayland-only
        // session starts here with the same single call.
        self.launch_pending_autostart();
    }

    /// The shape of the display the session is coming up on.
    ///
    /// One number for the whole seat. A second display of another shape gets
    /// the same bridge frame stretched to fit, which for the fraction of a
    /// second before the shell draws its own wallpaper on both is a better
    /// answer than rendering two images or showing one of them black.
    fn primary_aspect(&self) -> f32 {
        self.lxb
            .space
            .outputs()
            .find_map(|output| {
                let size = self.lxb.space.output_geometry(output)?.size;
                (size.w > 0 && size.h > 0).then(|| size.w as f32 / size.h as f32)
            })
            .unwrap_or(16.0 / 9.0)
    }

    /// Stop advertising an X display that will never answer.
    ///
    /// Only meaningful once the session exists: before that, clearing the
    /// field is enough, because nothing has read it yet.
    fn forget_xwayland_display(&mut self) {
        self.lxb.xwayland_display = None;
        self.lxb.xwayland_ready = false;
        if self.lxb.session_launched {
            let owns_seat = self.owns_the_seat();
            self.lxb.update_dbus_activation_environment(owns_seat);
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
                // Same tick, same reason: neither can be subscribed to.
                state.check_xwayland_focus();
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
        // Exactly once. XWayland reaching the end of its handshake — or giving
        // up on it — no longer decides when the session starts, but those paths
        // still call this, and starting the portal twice or stopping the one
        // this session just started would be worse than either.
        if self.lxb.session_launched {
            return;
        }
        self.lxb.session_launched = true;
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
        let background_handoff = self.lxb.pending_shell_handoff.take();
        let mut child = match self.lxb.command_for(command) {
            Some(mut cmd) => {
                inject_background_handoff(&mut cmd, background_handoff);
                match cmd.spawn() {
                    Ok(child) => child,
                    Err(err) => {
                        self.fail_session(format!(
                            "could not start the session shell {command:?}: {err}"
                        ));
                        return;
                    }
                }
            }
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

    /// Whether this window belongs to an application the shell says is playing
    /// something, and so one that must go on running with nothing on screen.
    ///
    /// Read from the window's *current* name for the same reason
    /// [`Lxb::out_of_sight`] is: a window that arrived nameless and then said
    /// what it was has to be recognised from the moment it says so, and an
    /// application that was renamed under a running window is the one case
    /// where being wrong here stops the music.
    pub fn media_is_playing(&self, window: &Window) -> bool {
        // The common case, and why nothing here has to be fast: a session with
        // nothing playing pays for none of it.
        if self.playing.is_empty() {
            return false;
        }
        let app_id = crate::shell_control::window_app_id(window);
        folded_app_id(&app_id).is_some_and(|app_id| self.playing.contains(&app_id))
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

/// Let everything this session starts render in high dynamic range, without
/// anybody having to ask for it.
///
/// A game under Proton does not get HDR by noticing the display can do it. It
/// has to be told to build an HDR swapchain: `ENABLE_HDR_WSI` loads the Vulkan
/// layer that carries a swapchain's colour to the compositor over
/// `frog_color_management_v1`, and `DXVK_HDR` is what lets a Direct3D title ask
/// for one at all. Neither has a default that means yes, so without them a game
/// renders SDR on an HDR screen however capable both ends are — which is why
/// every guide to HDR on Linux ends in a paragraph telling people to paste two
/// variables into a launch option.
///
/// Set here, on the session, rather than on Valve's client or on any one game.
/// This is the environment every child of the compositor gets, so it reaches
/// the shell, and through the shell it reaches Steam, and through Steam every
/// game Steam starts — as well as everything that never goes near Steam: a
/// Proton prefix run by hand, another launcher, an emulator. Setting it any
/// further in would be setting it once per way of starting a game, and would
/// miss the next one.
///
/// This is what a console does. Not because the variables are the same ones —
/// SteamOS runs the game nested inside gamescope and uses gamescope's own WSI
/// layer — but because on a console nobody types anything to make HDR work. The
/// session is the thing that has been configured, so the session is what
/// carries it.
///
/// Harmless where HDR is off or the screen cannot do it. The layer asks the
/// compositor what the display is and believes the answer: one this session is
/// not driving in HDR is described as sRGB, and the layer then leaves the
/// swapchain exactly as it found it. See [`crate::colour`].
///
/// Neither is forced. A value already in the environment this session was
/// started from was put there deliberately — including a `0` — and outranks a
/// default meant for everything.
fn offer_hdr_to_children(command: &mut std::process::Command) {
    for (key, value) in [("ENABLE_HDR_WSI", "1"), ("DXVK_HDR", "1")] {
        if std::env::var_os(key).is_none() {
            command.env(key, value);
        }
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
        // The display-manager record is for the desktop shell only. Removing
        // it here also overrides an accidental configured environment entry.
        .env_remove(BACKGROUND_HANDOFF_ENV)
        // Activation tokens belong to the compositor that issued them; an
        // inherited host token is invalid in this session.
        .env_remove("XDG_ACTIVATION_TOKEN")
        .env_remove("DESKTOP_STARTUP_ID");

    offer_hdr_to_children(command);

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

/// Add the captured record at the final shell-only launch boundary.
fn inject_background_handoff(
    command: &mut std::process::Command,
    handoff: Option<std::ffi::OsString>,
) {
    if let Some(handoff) = handoff {
        command.env(BACKGROUND_HANDOFF_ENV, handoff);
    }
}

/// Remove the one-shot record from the compositor before any child or worker
/// can inherit it. The shell receives the owned value later through
/// [`inject_background_handoff`].
pub(crate) fn take_background_handoff() -> Option<std::ffi::OsString> {
    let handoff = std::env::var_os(BACKGROUND_HANDOFF_ENV);
    // SAFETY: called at the start of `main`, before the compositor creates any
    // worker thread. No concurrent environment access exists at this point.
    unsafe { std::env::remove_var(BACKGROUND_HANDOFF_ENV) };
    handoff
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

// ---------------------------------------------------------------------------
// ending on a signal
// ---------------------------------------------------------------------------

/// The write end of the pipe the signal handler pokes, or `-1` before there is
/// one.
///
/// A raw fd in an atomic because that is what a signal handler may touch: it
/// may not allocate, take a lock, or call back into the runtime, and `write` is
/// one of the few calls that is safe there.
static SIGNAL_PIPE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

/// Note that a signal arrived, from inside the handler.
///
/// # Safety
///
/// Runs in a signal handler, so it does exactly one async-signal-safe thing:
/// writes a byte to a pipe the event loop is watching. A full pipe means a
/// signal is already waiting to be read, which is the same news.
extern "C" fn note_the_signal(signal: libc::c_int) {
    let fd = SIGNAL_PIPE.load(std::sync::atomic::Ordering::Relaxed);
    if fd < 0 {
        return;
    }
    let byte = signal as u8;
    // SAFETY: a byte written to a pipe fd that is open for as long as the
    // process runs. The result is deliberately ignored: there is nothing a
    // handler could do about it.
    unsafe {
        libc::write(fd, std::ptr::addr_of!(byte).cast(), 1);
    }
}

impl LxbState {
    /// End the session tidily when something asks it to stop.
    ///
    /// Without this a `SIGTERM` ends the compositor where it stands, and
    /// everything the exit path does is simply skipped: the displays keep the
    /// session's HDR encoding, the services it took on the user's bus are never
    /// released, and — the one that cannot be put right afterwards — every
    /// application it stopped stays stopped. `SIGCONT` is the only thing that
    /// undoes `SIGSTOP`, and a process that has been killed sends none.
    ///
    /// So the signal is turned into an ordinary event: the handler writes one
    /// byte down a pipe, the loop reads it, and the session ends the way
    /// quitting the shell ends it — through [`crate::sleep`]'s wake, the colour
    /// restore, and the rest of `main`.
    ///
    /// Three signals, and all of them mean the same thing here: `SIGTERM` is
    /// what logind and greetd send when they take a session down, `SIGINT` is a
    /// developer's Ctrl-C, and `SIGHUP` is the terminal going away.
    pub fn end_the_session_on_a_signal(&mut self) {
        let (read, write) = match pipe_pair() {
            Some(pair) => pair,
            None => {
                tracing::warn!("no pipe for signals; the session cannot end tidily on one");
                return;
            }
        };
        SIGNAL_PIPE.store(write, std::sync::atomic::Ordering::Relaxed);

        for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
            // SAFETY: installing a handler that does nothing but write a byte.
            unsafe {
                libc::signal(signal, note_the_signal as *const () as libc::sighandler_t);
            }
        }

        // SAFETY: `read` is a fresh pipe fd this owns from here on.
        let read = unsafe { <std::os::fd::OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(read) };
        let source = Generic::new(read, Interest::READ, Mode::Level);
        let inserted = self.lxb.loop_handle.insert_source(source, |_, fd, state| {
            let mut buffer = [0u8; 8];
            // SAFETY: reading into a buffer this call owns, from a pipe the
            // loop has just said is readable.
            let read = unsafe {
                libc::read(
                    std::os::fd::AsRawFd::as_raw_fd(&**fd),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                )
            };
            let signal = if read > 0 { buffer[0] as i32 } else { 0 };
            tracing::info!(signal, "asked to stop; ending the session");
            state.lxb.running = false;
            state.lxb.loop_signal.stop();
            Ok(PostAction::Continue)
        });
        if let Err(err) = inserted {
            tracing::warn!(?err, "could not watch for signals");
        }
    }
}

/// A close-on-exec, non-blocking pipe, as two raw fds.
fn pipe_pair() -> Option<(i32, i32)> {
    let mut fds = [0i32; 2];
    // SAFETY: `fds` is a two-element array, which is what pipe2 writes.
    let made = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) };
    (made == 0).then_some((fds[0], fds[1]))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::process::Command;

    use super::{
        confine_to_session, folded_app_id, inject_background_handoff, shell_split,
        BACKGROUND_HANDOFF_ENV,
    };

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
        assert_eq!(get(BACKGROUND_HANDOFF_ENV), Some(None));
    }

    #[test]
    fn background_handoff_is_injected_only_at_the_shell_boundary() {
        let mut command = Command::new("true");
        command.env(BACKGROUND_HANDOFF_ENV, "stale");

        confine_to_session(&mut command, "lxb-test", None);
        inject_background_handoff(&mut command, Some("canonical-record".into()));

        let value = command
            .get_envs()
            .find(|(name, _)| *name == OsStr::new(BACKGROUND_HANDOFF_ENV))
            .and_then(|(_, value)| value);
        assert_eq!(value, Some(OsStr::new("canonical-record")));
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
