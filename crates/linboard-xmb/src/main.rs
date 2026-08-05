//! Linboard XMB — a cross-media-bar style shell for the Linboard compositor.
//!
//! It is an ordinary Wayland client that binds `zwlr_layer_shell_v1`, so it
//! runs on Linboard as well as any other compositor implementing layer-shell.
//! That also makes it debuggable on its own, without starting a compositor.

mod apps;
mod controller;
mod gpu;
mod guide;
mod icons;
mod launch;
mod model;
mod theme;
mod ui;

use std::collections::HashSet;
use std::ffi::OsString;
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

use clap::Parser;
use linboard_protocol::client::linboard_shell_v1::{self, LinboardShellV1};
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState, FrameCallbackData};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::keyboard::{
    KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers,
};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
    LayerSurfaceConfigure, SurfaceKind,
};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::{delegate_registry, registry_handlers};
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::{wl_keyboard, wl_output, wl_seat, wl_surface};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};

use crate::controller::ControllerInput;
use crate::gpu::Gpu;
use crate::guide::{Guide, Item, Mode};
use crate::icons::IconLoader;
use crate::model::{Action, Cursor, Xmb};
use crate::ui::SlotLookup;

/// Size icons are decoded at; matches the atlas cell.
const ICON_SIZE: u32 = 128;

/// First `linboard_shell_v1` version that reports the foreground application
/// per display and can close one there. Below it a shell only learns about the
/// session as a whole, which is the wrong question once there is a bar on each
/// display.
const PER_OUTPUT_SHELL_VERSION: u32 = 3;

/// First version with the window overview: the compositor animates windows
/// into cards while the guide menu is open, and lets the shell activate one.
const OVERVIEW_SHELL_VERSION: u32 = 4;

/// First version that can end one named window outright, rather than asking
/// whatever is in front to close itself.
const KILL_SHELL_VERSION: u32 = 5;

/// Guaranteed atlas fallback for desktop entries that omit `Icon=` or name an
/// icon unavailable in the current theme. This is preferable to presenting a
/// blank coloured square, which looks like a rendering failure.
const FALLBACK_APP_ICON: &str = "application-x-executable";

/// A frame callback is the normal source of redraw pacing.  This deadline is
/// a watchdog for compositors/backends that fail to deliver one; without it a
/// single lost callback would freeze both the easing and animated backdrop.
const FRAME_CALLBACK_WATCHDOG: Duration = Duration::from_millis(24);

/// How often to look up while every display is hidden behind an application.
/// Nothing is being drawn, so this only has to be often enough to notice a
/// change no event announced — rare enough to cost nothing next to the game
/// in front.
const HIDDEN_POLL: Duration = Duration::from_millis(500);

/// How long the start screen takes to fly between filling its display and
/// sitting in its overview card, in seconds — the shared flight the
/// compositor gives the application windows beside it, so they travel
/// together.
const HOME_FLIGHT: f32 = linboard_protocol::overview::FLIGHT.as_millis() as f32 / MILLIS_PER_SECOND;

/// How far the bar leans towards the tile as an application opens off it.
/// Small: it is the background giving way, not a second animation competing
/// with the one the eye is following.
const LAUNCH_PUSH: f32 = 0.06;

/// How long past the flight the cards are given to actually be on screen.
///
/// The compositor samples the flight when it renders, and that frame is
/// scanned out after it, so the windows arrive a frame or two behind the
/// arithmetic.
const CARD_SETTLE: f32 = 2.0 / 60.0;

const MILLIS_PER_SECOND: f32 = 1000.0;

/// XDG data directories with `suffix` appended, most specific first.
///
/// Shared by the desktop-entry scan and the icon theme search so the two
/// cannot disagree about where a user's own files live.
pub fn xdg_data_dirs(suffix: &str) -> Vec<std::path::PathBuf> {
    use std::path::PathBuf;

    let mut dirs = Vec::new();

    if let Some(home) = std::env::var_os("XDG_DATA_HOME") {
        dirs.push(PathBuf::from(home).join(suffix));
    } else if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".local/share").join(suffix));
    }

    let data_dirs = std::env::var("XDG_DATA_DIRS")
        .unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string());
    dirs.extend(
        data_dirs
            .split(':')
            .filter(|d| !d.is_empty())
            .map(|d| PathBuf::from(d).join(suffix)),
    );

    dirs
}

#[derive(Debug, Parser)]
#[command(name = "linboard-xmb", version, about, long_about = None)]
struct Cli {
    /// Which layer to place the shell on. The animated backdrop always sits
    /// on the background layer, one below this, so the compositor can slot
    /// the overview's window cards between the two.
    #[arg(long, default_value = "bottom")]
    layer: LayerArg,

    /// Hold keyboard focus even while an application is running.
    ///
    /// The shell takes focus while it is the active launcher, then normally
    /// switches to `OnDemand` when an application is launched so that the
    /// application can take over.  This flag keeps the exclusive grab instead.
    #[arg(long, action = clap::ArgAction::SetTrue)]
    grab_keyboard: std::primitive::bool,

    /// Disable direct game-controller input (keyboard navigation still works).
    #[arg(long, action = clap::ArgAction::SetTrue)]
    no_gamepad: std::primitive::bool,

    /// Perform actions at fixed times after start-up, as a comma-separated
    /// list of `seconds:action` (`--debug-actions 2:guide,3:right,4:launch`).
    /// Actions are `guide`, `back`, `launch`, `up`, `down`, `left`, `right`.
    ///
    /// Development aid: most of this shell's design is in its transitions,
    /// and they cannot be inspected — or screenshotted at a chosen moment —
    /// without a way to drive them that does not need a controller in hand.
    #[arg(long, hide = true, value_delimiter = ',', value_parser = parse_timed_action)]
    debug_actions: Vec<(f32, Action)>,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum LayerArg {
    Background,
    Bottom,
    Top,
    Overlay,
}

impl From<LayerArg> for Layer {
    fn from(value: LayerArg) -> Self {
        match value {
            LayerArg::Background => Layer::Background,
            LayerArg::Bottom => Layer::Bottom,
            LayerArg::Top => Layer::Top,
            LayerArg::Overlay => Layer::Overlay,
        }
    }
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // A WAYLAND_SOCKET file descriptor represents this client's already-open
    // connection and cannot be propagated to independently launched clients.
    // Refuse that unusual invocation instead of guessing another socket and
    // potentially sending applications to the host compositor.
    let (child_wayland_display, child_xwayland_display) = child_session_displays()?;

    let categories = apps::scan();
    let total: usize = categories.iter().map(|c| c.apps.len()).sum();
    tracing::info!(
        categories = categories.len(),
        applications = total,
        "scanned applications"
    );

    // Decode every icon once, up front, so the atlas can be built in one go.
    let icons = load_icons(&categories);

    let conn = Connection::connect_to_env()
        .map_err(|e| anyhow::anyhow!("could not connect to a Wayland compositor: {e}"))?;
    let (globals, mut event_queue) = registry_queue_init(&conn)?;
    let qh = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh)
        .map_err(|e| anyhow::anyhow!("wl_compositor unavailable: {e}"))?;
    let layer_shell = LayerShell::bind(&globals, &qh).map_err(|e| {
        anyhow::anyhow!("this compositor does not support zwlr_layer_shell_v1: {e}")
    })?;

    // Linboard's own protocol, which carries the guide binding and lets the
    // overlay close an application. Absent on every other compositor, where the
    // shell simply falls back to what it can do as an ordinary client.
    let shell_control = match globals.bind::<LinboardShellV1, _, _>(&qh, 1..=KILL_SHELL_VERSION, ())
    {
        Ok(control) => Some(control),
        Err(err) => {
            tracing::info!(
                %err,
                "linboard_shell_v1 unavailable; the guide overlay will only manage what this shell started"
            );
            None
        }
    };

    let mut shell = Shell {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        compositor,
        layer_shell,
        panels: Vec::new(),
        focused_panel: 0,
        base_layer: cli.layer.into(),
        shell_control,
        foreground: None,
        applied_launch_output: None,
        applied_overview: None,
        applied_overview_selection: None,
        guide: Guide::default(),
        menu_frame_drawn: false,
        overview_started_at: None,
        launching: None,
        guide_card_rects: std::collections::HashMap::new(),
        keyboard: None,
        focused_surface: None,
        keep_keyboard_grabbed: cli.grab_keyboard,
        controller: ControllerInput::new(!cli.no_gamepad),
        gpu: None,
        pending_icons: Some(icons),
        xmb: Xmb::with_session_displays(categories, child_wayland_display, child_xwayland_display),
        exit: false,
        needs_redraw: true,
        next_frame_deadline: Instant::now(),
        start: Instant::now(),
        last_frame: Instant::now(),
        frames: 0,
        fps_window: Instant::now(),
        conn: conn.clone(),
    };

    let mut action_schedule = cli.debug_actions.clone();
    action_schedule.sort_by(|(a, _), (b, _)| a.total_cmp(b));
    action_schedule.reverse(); // popped from the back, earliest first

    // Outputs are bound during registry init, but their names and modes only
    // arrive on the next dispatch. Waiting for that means the bar comes up on
    // every display at once rather than one configure behind.
    event_queue.roundtrip(&mut shell)?;

    for output in shell.output_state.outputs().collect::<Vec<_>>() {
        shell.add_panel(&qh, output);
    }
    if shell.panels.is_empty() {
        anyhow::bail!("this compositor advertises no outputs to draw on");
    }

    while !shell.exit {
        event_queue.dispatch_pending(&mut shell)?;
        shell.xmb.reap_children();

        let now = Instant::now();
        shell.poll_controller(now);
        // Scheduled actions, for capturing the menu's transitions.
        while action_schedule
            .last()
            .is_some_and(|(at, _)| shell.start.elapsed().as_secs_f32() >= *at)
        {
            if let Some((_, action)) = action_schedule.pop() {
                tracing::debug!(?action, "scheduled debug action");
                shell.on_action(action);
            }
        }
        // Applies whatever the last events settled on: an application exiting
        // changes what the surface should be doing just as much as a keypress.
        shell.sync_surface_state();
        shell.sync_launch_output();
        shell.sync_overview();
        if now >= shell.next_frame_deadline {
            shell.needs_redraw = true;
        }
        shell.draw(&qh);

        if shell.exit {
            break;
        }

        let until_watchdog = shell
            .next_frame_deadline
            .saturating_duration_since(Instant::now());
        wait_for_wayland(
            &mut event_queue,
            until_watchdog.min(controller::POLL_INTERVAL),
        )?;
    }

    tracing::info!("exiting");
    Ok(())
}

/// Resolve the named Wayland socket that child applications can reconnect to.
fn child_session_displays() -> anyhow::Result<(OsString, Option<OsString>)> {
    if std::env::var_os("WAYLAND_SOCKET").is_some() {
        anyhow::bail!(
            "WAYLAND_SOCKET cannot be forwarded safely to launched applications; \
             start linboard-xmb with a named WAYLAND_DISPLAY instead"
        );
    }

    let wayland =
        std::env::var_os("WAYLAND_DISPLAY").unwrap_or_else(|| OsString::from("wayland-0"));
    let xwayland = std::env::var_os("LINBOARD_XWAYLAND_DISPLAY");
    Ok((wayland, xwayland))
}

/// Wait for Wayland traffic, but only for long enough to service controller
/// input and the redraw watchdog.  `blocking_dispatch` cannot be used here:
/// gamepads are not Wayland objects and therefore cannot wake it.
fn wait_for_wayland(event_queue: &mut EventQueue<Shell>, timeout: Duration) -> anyhow::Result<()> {
    event_queue.flush()?;

    let Some(read_guard) = event_queue.prepare_read() else {
        // Events entered the queue between dispatch_pending and prepare_read.
        return Ok(());
    };

    let nanos = timeout.as_nanos();
    let timeout_ms = nanos
        .saturating_add(999_999)
        .checked_div(1_000_000)
        .unwrap_or(0)
        .min(i32::MAX as u128) as i32;
    let mut fd = libc::pollfd {
        fd: read_guard.connection_fd().as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };

    // SAFETY: `fd` points to one valid pollfd for the duration of this call.
    let ready = unsafe { libc::poll(&mut fd, 1, timeout_ms) };
    if ready < 0 {
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::Interrupted {
            return Ok(());
        }
        return Err(err.into());
    }

    if ready > 0 {
        // Reading on HUP/ERR as well as POLLIN turns a dead compositor into a
        // useful connection error instead of a busy loop.
        read_guard.read()?;
    }

    Ok(())
}

/// Decode the icons for every category and application.
fn load_icons(categories: &[apps::Category]) -> Vec<(String, icons::Icon)> {
    let mut loader = IconLoader::new();
    let mut out = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    let names = std::iter::once(FALLBACK_APP_ICON.to_string())
        .chain(categories.iter().map(|c| c.icon.to_string()))
        .chain(
            categories
                .iter()
                .flat_map(|c| c.apps.iter().filter_map(|a| a.icon.clone())),
        );

    for name in names {
        if !seen.insert(name.clone()) {
            continue;
        }
        if let Some(icon) = loader.load(&name, ICON_SIZE) {
            out.push((name, icon));
        }
    }

    tracing::info!(loaded = out.len(), "decoded icons");
    out
}

/// The bar as it appears on one display.
///
/// The model behind it is shared, so every display shows the same selection —
/// what differs is the resolution it is laid out for and the swapchain it is
/// drawn into.
struct Panel {
    output: wl_output::WlOutput,
    name: String,
    layer: LayerSurface,
    /// The animated backdrop, on its own always-background surface below
    /// `layer`. Splitting it out is what lets the overview draw live window
    /// cards *over* the (blurred) start-screen background: the compositor
    /// stacks them between the two surfaces.
    backdrop: LayerSurface,
    /// Created on the first configure, once the size is known.
    target: Option<gpu::Target>,
    backdrop_target: Option<gpu::Target>,
    /// What this display is pointing at. Its own, so displays browse
    /// independently instead of mirroring one another.
    cursor: Cursor,
    /// Title of the application in front on *this* display, as reported by the
    /// compositor. Likewise its own: the guide opens on one display and must
    /// offer to resume or close what is on that one.
    foreground: Option<String>,
    /// The windows on this display, topmost first, as the compositor lists
    /// them for the overview. Empty on compositors without the protocol.
    windows: Vec<WindowCard>,
    /// Batch under construction; becomes `windows` on the done event.
    pending_windows: Vec<WindowCard>,
    width: u32,
    height: u32,
    frame_callback_pending: bool,
    backdrop_frame_pending: bool,
    /// Last layer and interactivity actually sent, so an unchanged mode does
    /// not commit the surface every frame.
    applied_surface_state: Option<(Layer, KeyboardInteractivity)>,
    /// How far this display's start screen has flown into its overview card:
    /// 0 fills the display, 1 sits in the card. Linear, so a flight
    /// interrupted halfway reverses from where it is; [`smoothstep`] shapes
    /// it on the way out.
    home_linear: f32,
    /// Whether this display's start screen fades in with the guide instead of
    /// flying — it was not on screen to fly from.
    home_fades_in: bool,
    /// Whether the last frame drawn for this display was one anybody could
    /// see. Going quiet costs one more frame after that, so whatever is left
    /// committed is what the display should be showing now rather than
    /// whatever it happened to be showing when it was covered up.
    was_visible: bool,
    /// Likewise for the backdrop's blur, which ramps with the menu rather
    /// than with the start screen — over an application the start screen
    /// never flies, but the background still has to soften.
    blur_linear: f32,
    /// The start card's rectangle as of the last frame with the menu open —
    /// the end the start screen flies to, and flies back from once the menu
    /// has closed and the cards are gone.
    home_rect: [f32; 4],
}

impl Panel {
    fn owns(&self, surface: &wl_surface::WlSurface) -> bool {
        self.layer.wl_surface() == surface
    }
}

/// One window in the overview, as announced by the compositor.
#[derive(Debug, Clone, PartialEq, Eq)]
struct WindowCard {
    id: u32,
    title: String,
    width: u32,
    height: u32,
}

struct Shell {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,

    compositor: CompositorState,
    layer_shell: LayerShell,
    /// One per display, in the order the compositor announced them.
    panels: Vec<Panel>,
    /// Which display the controller and keyboard are driving. Wayland has no
    /// notion of a primary output, so the compositor's first is the main one.
    focused_panel: usize,
    /// Layer the bar sits on when it is not covering an application.
    base_layer: Layer,
    /// Linboard's session protocol, when running under Linboard.
    shell_control: Option<LinboardShellV1>,
    /// Title of the foreground application, as reported by the compositor.
    foreground: Option<String>,
    /// Display last named to the compositor as the one to launch on, so an
    /// unchanged choice is not resent every frame.
    applied_launch_output: Option<wl_output::WlOutput>,
    /// Display the compositor was last told to show the window overview on,
    /// if any, so the toggle is only sent on changes.
    applied_overview: Option<wl_output::WlOutput>,
    /// Card selection last reported for it, likewise.
    applied_overview_selection: Option<u32>,
    guide: Guide,
    /// Whether a frame of the open menu has been committed yet. The overview
    /// waits for it, so the cards never fly under a stale bar.
    menu_frame_drawn: bool,
    /// When the compositor was told to start flying windows into their cards.
    overview_started_at: Option<Instant>,
    /// The application the shell has started and is waiting for, if any: what
    /// answers the press while the process gets itself on screen.
    launching: Option<launch::Launch>,
    /// Eased card rectangles by window id (`u64::MAX` is the start card),
    /// advanced every frame on the compositor's own spring so the frames the
    /// shell draws travel with the windows the compositor is easing.
    guide_card_rects: std::collections::HashMap<u64, Glide>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    /// Which of our surfaces holds keyboard focus, if any. Only one can, so
    /// this doubles as "is the shell being driven right now".
    focused_surface: Option<wl_surface::WlSurface>,
    keep_keyboard_grabbed: bool,
    controller: ControllerInput,

    gpu: Option<gpu::Gpu>,
    /// Icons waiting for the device to exist; taken on the first configure.
    pending_icons: Option<Vec<(String, icons::Icon)>>,

    xmb: Xmb,
    exit: bool,
    needs_redraw: bool,
    next_frame_deadline: Instant,
    start: Instant,
    last_frame: Instant,
    frames: u32,
    fps_window: Instant,
    conn: Connection,
}

impl Shell {
    /// Put the bar on a display.
    fn add_panel(&mut self, qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        if self.panels.iter().any(|panel| panel.output == output) {
            return;
        }

        // The backdrop first, so within a shared layer it also stacks below.
        let backdrop_surface = self.compositor.create_surface(qh);
        let backdrop = self.layer_shell.create_layer_surface(
            qh,
            backdrop_surface,
            Layer::Background,
            Some("linboard-xmb-backdrop"),
            Some(&output),
        );
        backdrop.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        backdrop.set_size(0, 0);
        backdrop.commit();

        let surface = self.compositor.create_surface(qh);
        // Naming the output matters here: left to choose, a compositor puts
        // every one of these on the same display and the others stay empty.
        let layer = self.layer_shell.create_layer_surface(
            qh,
            surface,
            self.base_layer,
            Some("linboard-xmb"),
            Some(&output),
        );
        // Fill the whole output: anchoring to all four edges with a zero size
        // asks the compositor for the full usable area.
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_size(0, 0);
        layer.commit();

        let name = self
            .output_state
            .info(&output)
            .and_then(|info| info.name)
            .unwrap_or_else(|| "?".to_string());
        tracing::info!(output = %name, "showing the bar on a display");

        self.panels.push(Panel {
            output,
            name,
            layer,
            backdrop,
            target: None,
            backdrop_target: None,
            cursor: Cursor::new(self.xmb.categories.len()),
            foreground: None,
            windows: Vec::new(),
            pending_windows: Vec::new(),
            width: 0,
            height: 0,
            frame_callback_pending: false,
            backdrop_frame_pending: false,
            applied_surface_state: None,
            home_linear: 0.0,
            home_fades_in: false,
            was_visible: true,
            blur_linear: 0.0,
            home_rect: [0.0; 4],
        });
        self.needs_redraw = true;
    }

    fn remove_panel(&mut self, output: &wl_output::WlOutput) {
        let Some(index) = self.panels.iter().position(|p| &p.output == output) else {
            return;
        };
        // Dropping the panel drops its layer surface and swapchain with it.
        self.panels.remove(index);

        // Keep pointing at the same display where possible, and never past the
        // end: unplugging the focused screen has to leave control somewhere.
        if self.focused_panel > index || self.focused_panel >= self.panels.len() {
            self.focused_panel = self.focused_panel.saturating_sub(1);
        }
        self.needs_redraw = true;
    }

    /// Hand control to another display. `delta` is a step through the display
    /// order, wrapping at both ends.
    fn focus_screen(&mut self, delta: i32) {
        if self.panels.len() < 2 {
            return;
        }
        let count = self.panels.len() as i32;
        let next = (self.focused_panel as i32 + delta).rem_euclid(count) as usize;
        if next == self.focused_panel {
            return;
        }
        self.focused_panel = next;
        tracing::debug!(display = %self.panels[next].name, "control moved to a display");
        // The layer surfaces have to change too: only the focused one takes
        // keyboard input, which is also how the compositor knows where to put
        // an application launched from here.
        //
        // An open menu travels with the user, and it should *arrive* — slide
        // in, windows flying to their cards — not blink into place while only
        // the display left behind animates.
        self.guide.replay_entrance();
        // The cards belong to the new display now: snap, don't glide across.
        self.guide_card_rects.clear();
        if self.guide.is_menu() {
            // The start screen on the display being arrived at flies in from
            // wherever it was showing. `false`, not `is_over_app`: the bar on
            // *that* display is behind whatever runs there, whatever the menu
            // is doing on the one being left.
            let panel = &mut self.panels[next];
            panel.home_linear = home_flight_start(false, !panel.windows.is_empty());
            panel.home_fades_in = panel.home_linear > 0.0;
            // Its overview waits for a menu frame of its own, for the same
            // reason opening the menu does.
            self.menu_frame_drawn = false;
        }
        self.needs_redraw = true;
    }

    fn draw(&mut self, qh: &QueueHandle<Self>) {
        if !self.needs_redraw {
            return;
        }

        let now = Instant::now();
        let dt = now.duration_since(self.last_frame).as_secs_f32();
        self.last_frame = now;

        self.advance_launch(now);

        // Every display eases towards its own selection, including the ones
        // nobody is driving: they were left mid-glide when control moved away.
        let mut cursor_moving = vec![false; self.panels.len()];
        for (moving, panel) in cursor_moving.iter_mut().zip(&mut self.panels) {
            *moving = panel.cursor.animate(dt);
        }

        // Resolved before the GPU and panels are borrowed, since they read
        // `self` while it is mutably held below.
        let show_menu = self.guide.is_menu();
        let focused_panel = self.focused_panel;
        let hint = self.control_hint();
        let app_label = self.app_label().map(str::to_string);
        let screen_label = self.screen_label();
        let time = self.start.elapsed().as_secs_f32();
        let clock = wall_clock();
        // The cards keep the compositor's time, not the menu's.
        let card_age = self.card_age();
        // A display whose bar is behind a fullscreen application draws
        // nothing at all this frame: see `panel_is_visible`.
        let visible: Vec<bool> = (0..self.panels.len())
            .map(|index| self.panel_is_visible(index))
            .collect();

        // The guide view belongs to the focused display. Assembled before the
        // panels are borrowed for drawing, because easing the card highlight
        // towards its target mutates the guide.
        let (guide_cards, guide_highlight, start_card_rect, close_target) = if show_menu {
            let panel = self.panels.get(focused_panel);
            let mut cards: Vec<ui::Card> = panel
                .map(|panel| {
                    panel
                        .windows
                        .iter()
                        .map(|window| ui::Card {
                            title: window.title.clone(),
                            width: window.width as f32,
                            height: window.height as f32,
                            start: false,
                            rect: [0.0; 4],
                        })
                        .collect()
                })
                .unwrap_or_default();
            // The start screen is always in the deck, last — so a display
            // with nothing running still has one card, and pressing A on it
            // always leads home.
            if let Some(panel) = panel {
                cards.push(ui::Card {
                    title: "Start screen".to_string(),
                    width: panel.width.max(16) as f32,
                    height: panel.height.max(9) as f32,
                    start: true,
                    rect: [0.0; 4],
                });
            }
            let keys: Vec<u64> = panel
                .map(|panel| {
                    panel
                        .windows
                        .iter()
                        .map(|window| window.id as u64)
                        .chain(std::iter::once(u64::MAX))
                        .collect()
                })
                .unwrap_or_default();

            let selected = self.guide.selected_window(cards.len());
            if let Some(panel) = panel {
                // Every selection step slides the whole column, so each card
                // eases towards its slot at the compositor's own glide rate —
                // the frames travel with the windows underneath. A card seen
                // for the first time snaps: no flight in from nowhere.
                let slots = linboard_protocol::overview::card_slots(
                    panel.width as f64,
                    panel.height as f64,
                    cards.len(),
                    selected,
                );
                self.guide_card_rects.retain(|key, _| keys.contains(key));
                for ((card, key), slot) in cards.iter_mut().zip(&keys).zip(&slots) {
                    let fitted = linboard_protocol::overview::fit(
                        slot,
                        card.width as f64,
                        card.height as f64,
                    );
                    let target = [
                        fitted.x as f32,
                        fitted.y as f32,
                        fitted.w as f32,
                        fitted.h as f32,
                    ];
                    // A card seen for the first time starts in its slot, at
                    // rest: the open flight is the compositor's, and a spring
                    // winding up from nowhere would fight it.
                    let glide = self.guide_card_rects.entry(*key).or_insert(Glide {
                        at: target,
                        velocity: [0.0; 4],
                    });
                    card.rect = spring_rect(glide, target, dt);
                }
            }

            // The frame takes the selected card's rectangle as it stands, not
            // an eased approach to it. The card is already gliding to the
            // middle of the column; a frame easing towards a moving target
            // would trail it across the screen and only catch up once it
            // stopped, which reads as the frame springing between cards
            // rather than marking the one that is being chosen.
            let highlight = cards.get(selected).map(|card| card.rect);
            // Where the start screen flies to: the same eased rectangle its
            // frame is drawn at, so the miniature and the frame around it can
            // never disagree.
            let start = cards
                .iter()
                .find(|card| card.start)
                .map(|card| card.rect)
                .unwrap_or([0.0; 4]);
            // What the Close entry would act on: the selected card, unless
            // that is the start screen, which is not an application and has
            // no process to end.
            let close_target = cards
                .get(selected)
                .filter(|card| !card.start)
                .map(|card| card.title.clone());
            (cards, highlight, start, close_target)
        } else {
            self.guide_card_rects.clear();
            (Vec::new(), None, [0.0; 4], None)
        };

        // The selected entry's chip, eased so it slides down the column.
        let closable = close_target.is_some();
        let guide_menu_highlight = show_menu
            .then(|| {
                let panel = self.panels.get(focused_panel)?;
                let items = self.guide.items(closable);
                let row = ui::menu_item_rect(
                    items,
                    self.guide.selected_index(closable),
                    panel.width as f32,
                    panel.height as f32,
                );
                Some(self.guide.animate_menu_highlight(row, dt))
            })
            .flatten();
        // Advanced once for the frame, not once per display: the dialog is on
        // the display being driven, and the others must not run its clock on.
        // Shaped here rather than in the model, like the start screen's own
        // flight — what is kept is a position, so that reversing it mid-way
        // continues from where the panel is instead of from where a restarted
        // curve would put it.
        let power_open = ui::ease(self.guide.animate_power(dt));

        let Some(gpu) = self.gpu.as_mut() else {
            self.next_frame_deadline = now + FRAME_CALLBACK_WATCHDOG;
            return;
        };
        self.needs_redraw = false;
        let mut drew = false;

        for (index, panel) in self.panels.iter_mut().enumerate() {
            let visible = visible[index];
            let Some(target) = panel.target.as_mut() else {
                panel.was_visible = visible;
                continue;
            };

            let (width, height) = target.size();
            let focused = index == focused_panel;
            // The guide is one overlay on the display the user is driving, not
            // a copy on each. The others carry on showing their own bar, which
            // is the whole point of giving them their own cursor.
            let menu_here = show_menu && focused;

            // The start screen flies between filling the display and sitting
            // in its overview card, on the same 300ms as the application
            // windows the compositor flies alongside it.
            if menu_here {
                panel.home_rect = start_card_rect;
            }
            let step = dt / HOME_FLIGHT;
            let home_target = if menu_here { 1.0 } else { 0.0 };
            panel.home_linear = approach(panel.home_linear, home_target, step);
            panel.blur_linear = approach(panel.blur_linear, home_target, step);

            // Being hidden is not a reason to stop mid-animation: what stays
            // committed is what a translucent application in front shows, and
            // what the display goes back to if it is uncovered. Settle first,
            // then go quiet.
            let settling = panel.home_linear != home_target
                || panel.blur_linear != home_target
                || cursor_moving[index];
            let draw_now = should_draw(visible, settling, panel.was_visible);
            if visible != panel.was_visible {
                // Worth a line: whether a display believes it is covered is
                // the difference between an idle shell and one burning a
                // game's frames, and it is decided from sizes that are easy
                // to be wrong about.
                tracing::debug!(
                    display = %panel.name,
                    visible,
                    output = ?(panel.width, panel.height),
                    windows = ?panel
                        .windows
                        .iter()
                        .map(|window| (window.width, window.height))
                        .collect::<Vec<_>>(),
                    "display visibility changed"
                );
            }

            // Ask for the next frame, then flush immediately.  The pending flag
            // is important: watchdog redraws may happen while a compositor is
            // late, and creating another callback on each fallback frame would
            // leak an ever-growing callback queue.
            //
            // The GPU driver owns its own event queue on this same connection
            // and issues the `wl_surface.commit` itself during `present`. A
            // frame request still sitting in our client-side buffer would
            // therefore land *after* that commit and be deferred to a frame
            // that never comes, so the flush here is what keeps the backdrop
            // animating.
            //
            // Nothing is asked for once there is nothing left to draw: a frame
            // callback is a request to be woken to draw again, and a hidden
            // display has nothing worth drawing until the application in front
            // of it goes away — which arrives as an event, not as a frame.
            if draw_now && !panel.frame_callback_pending {
                let surface = panel.layer.wl_surface();
                surface.frame(qh, FrameCallbackData(surface.clone()));
                panel.frame_callback_pending = true;
                if let Err(err) = self.conn.flush() {
                    tracing::warn!(?err, "could not flush the frame request");
                }
            }
            if draw_now && !panel.backdrop_frame_pending {
                let surface = panel.backdrop.wl_surface();
                surface.frame(qh, FrameCallbackData(surface.clone()));
                panel.backdrop_frame_pending = true;
                let _ = self.conn.flush();
            }
            panel.was_visible = visible;
            let home = smoothstep(panel.home_linear);
            let bar_rect = lerp_rect(
                [0.0, 0.0, width as f32, height as f32],
                panel.home_rect,
                home,
            );

            // The backdrop pass: always the animated background, softening
            // into the menu's blur as it opens and clearing again as it
            // closes — the same ramp in both directions, so an interrupted
            // one carries on from where it is.
            let backdrop_blur = smoothstep(panel.blur_linear);

            // Everything above is arithmetic and happens whether or not this
            // display is on screen, so a bar that was hidden mid-flight is
            // where it should be when it comes back rather than resuming an
            // animation nobody saw the start of. Only the drawing is skipped.
            if !draw_now {
                continue;
            }
            drew = true;

            if let Some(backdrop_target) = panel.backdrop_target.as_mut() {
                let params = gpu::Backdrop {
                    blur: backdrop_blur,
                    ..Default::default()
                };
                if let Err(err) = gpu.render(backdrop_target, &[], &[], time, Some(params)) {
                    tracing::warn!(?err, "backdrop render failed");
                }
            }

            // The bar, wherever the start screen currently is: the whole
            // display, its card, or somewhere between the two mid-flight.
            let mut scene = ui::build(
                &self.xmb,
                &panel.cursor,
                width as f32,
                height as f32,
                focused,
                &hint,
                clock.as_deref(),
                time,
                &Slots(gpu),
            );
            if home > 0.0 {
                scene.place_into(bar_rect, width as f32, height as f32);
                if menu_here && panel.home_fades_in {
                    scene.fade(ui::start_card_fade(card_age));
                }
            }
            if menu_here {
                // The guide is drawn over it, so anything of the bar still
                // crossing the sidebar has to give way to the panel.
                let sidebar = linboard_protocol::overview::sidebar_width(width as f64) as f32;
                scene.fade_text_before(sidebar, sidebar * 0.5);
                // The bar is a separate scene, so the dialog's panel — a quad,
                // and every quad is drawn under every text run — cannot cover
                // the start card's labels. They have to step back themselves.
                if power_open > 0.0 {
                    ui::recede_behind_dialog(
                        &mut scene,
                        width as f32,
                        height as f32,
                        self.guide.power_items().len(),
                        power_open,
                    );
                }
                let guide = ui::build_guide(
                    ui::GuideView {
                        guide: &self.guide,
                        app: app_label.as_deref(),
                        close_target: close_target.as_deref(),
                        screen: screen_label.as_deref(),
                        cards: &guide_cards,
                        highlight: guide_highlight,
                        menu_highlight: guide_menu_highlight,
                        // What the sidebar's glass is laid over: the backdrop
                        // surface below, drawn as softly as this.
                        behind: backdrop_blur,
                        card_age,
                        power: power_open,
                        time,
                    },
                    width as f32,
                    height as f32,
                );
                scene.quads.extend(guide.quads);
                scene.texts.extend(guide.texts);
            }

            // The launch splash, over everything this display was showing.
            // The bar it came out of leans towards the tile and fades, the way
            // a home screen gives way to the application opening off it, and
            // gives up the labels the panel covers — a scene is all its quads
            // and then all its text, so a panel drawn over one cannot hide
            // what it already said.
            if let Some(splash) = self.launching.as_ref().filter(|s| s.panel == index) {
                let open = splash.open(now);
                let grown = ui::ease(open);
                let (panel_rect, _) =
                    ui::launch_panel(splash.from, width as f32, height as f32, open);
                let zoom = 1.0 + LAUNCH_PUSH * grown;
                let [cx, cy] = [
                    splash.from[0] + splash.from[2] * 0.5,
                    splash.from[1] + splash.from[3] * 0.5,
                ];
                scene.scale_by(zoom, [cx - cx * zoom, cy - cy * zoom]);
                scene.hide_text_behind(panel_rect);
                scene.fade(1.0 - grown);

                let icon = splash
                    .icon
                    .as_deref()
                    .and_then(|name| Slots(gpu).slot_for(Some(name)));
                let over = ui::build_launch(
                    ui::LaunchView {
                        name: &splash.name,
                        icon,
                        from: splash.from,
                        open,
                        fade: splash.fade(now),
                        waiting: splash.waiting(),
                        time,
                    },
                    width as f32,
                    height as f32,
                );
                scene.quads.extend(over.quads);
                scene.texts.extend(over.texts);
            }

            // The cards are live windows the compositor draws below this
            // surface, so their corners can only be rounded from above: the
            // pass repaints everything just outside each rounded card with the
            // backdrop behind it. Only once they have landed — a card's rect
            // is where its window is *going*, and painting backdrop there
            // while it is still on its way lays a band of wallpaper across the
            // middle of a live window.
            let mut covers = [[0.0f32; 4]; gpu::MAX_COVERS];
            let mut cover_count = 0;
            if menu_here && cards_have_landed(card_age) {
                for card in guide_cards.iter().filter(|card| !card.start) {
                    if cover_count == covers.len() {
                        break;
                    }
                    covers[cover_count] = card.rect;
                    cover_count += 1;
                }
            }

            // The bar surface is transparent — the waves live on the backdrop
            // surface below, so the compositor can put overview cards between
            // the two. It paints its own background only where the start
            // screen itself is: the whole display when the bar is summoned
            // over an application, its card while the menu is open, and the
            // growing rectangle in between.
            let corner_radius = ui::CARD_RADIUS * (height as f32 / 1080.0).clamp(0.6, 2.5);
            let main_backdrop = if home > 0.0 {
                Some(gpu::Backdrop {
                    blur: 0.0,
                    window_rect: bar_rect,
                    // Squared off again as it fills the display: a fullscreen
                    // start screen has no corners to round.
                    corner_radius: corner_radius * home,
                    covers,
                    cover_count: cover_count as u32,
                    cover_blur: backdrop_blur,
                    // A card that is arriving rather than flying in fades up
                    // with the rest of them, wallpaper and all.
                    fade: if menu_here && panel.home_fades_in {
                        ui::start_card_fade(card_age)
                    } else {
                        1.0
                    },
                })
            } else if focused && self.guide.is_over_app() {
                Some(gpu::Backdrop::default())
            } else {
                None
            };

            match gpu.render(target, &scene.quads, &scene.texts, time, main_backdrop) {
                // Committed: the overview may start its windows flying now,
                // with a frame that belongs above them already in place.
                Ok(()) => self.menu_frame_drawn |= menu_here,
                Err(err) => tracing::warn!(?err, "render failed"),
            }
        }

        // The watchdog exists to recover from a frame callback that never
        // arrives. With every display hidden none were asked for, so waiting
        // on it would only be waking up to decide not to draw again: the loop
        // idles on events until an application closes or the guide is
        // summoned, both of which arrive as one.
        self.next_frame_deadline = Instant::now()
            + if drew {
                FRAME_CALLBACK_WATCHDOG
            } else {
                HIDDEN_POLL
            };

        // Frame pacing is easy to get wrong when the GPU driver owns the
        // surface commit, so make the rate observable — and a bar that has
        // gone quiet behind an application reads as an honest zero.
        self.frames += u32::from(drew);
        if self.fps_window.elapsed() >= std::time::Duration::from_secs(1) {
            tracing::debug!(fps = self.frames, panels = self.panels.len(), "frame rate");
            self.frames = 0;
            self.fps_window = Instant::now();
        }
    }

    /// Bring a panel's two swapchains into line with the size it was just
    /// given, creating the GPU device along with the first one.
    fn ensure_target(&mut self, index: usize) {
        let Some(panel) = self.panels.get(index) else {
            return;
        };
        if panel.width == 0 || panel.height == 0 {
            return;
        }
        let (width, height) = (panel.width, panel.height);
        let display_ptr = self.conn.backend().display_ptr() as *mut std::ffi::c_void;

        for main in [true, false] {
            let panel = &mut self.panels[index];
            let surface = if main {
                panel.layer.wl_surface()
            } else {
                panel.backdrop.wl_surface()
            };
            let surface_ptr = match surface.id().as_ptr() {
                ptr if !ptr.is_null() => ptr as *mut std::ffi::c_void,
                _ => {
                    tracing::error!("layer surface has no native pointer");
                    self.exit = true;
                    return;
                }
            };

            let slot = if main {
                &mut panel.target
            } else {
                &mut panel.backdrop_target
            };
            if let Some(target) = slot.as_mut() {
                if let Some(gpu) = self.gpu.as_ref() {
                    target.resize(gpu, width, height);
                }
                continue;
            }

            // SAFETY: both pointers come from live protocol objects owned by
            // this struct, which outlives the renderer.
            let created = match self.gpu.as_mut() {
                Some(gpu) => unsafe { gpu.add_target(display_ptr, surface_ptr, width, height) },
                None => {
                    let icons = self.pending_icons.take().unwrap_or_default();
                    match unsafe { gpu::Gpu::new(display_ptr, surface_ptr, width, height, icons) } {
                        Ok((gpu, target)) => {
                            self.gpu = Some(gpu);
                            Ok(target)
                        }
                        Err(err) => Err(err),
                    }
                }
            };

            match created {
                Ok(target) => {
                    let panel = &mut self.panels[index];
                    if main {
                        tracing::info!(width, height, "display ready");
                        panel.target = Some(target);
                    } else {
                        panel.backdrop_target = Some(target);
                    }
                    self.needs_redraw = true;
                }
                Err(err) => {
                    // One display failing is not worth taking the session down
                    // when another still works; losing them all is.
                    tracing::error!(?err, "could not set this display up for drawing");
                    if self.gpu.is_none() {
                        self.exit = true;
                    }
                    return;
                }
            }
        }
    }

    fn poll_controller(&mut self, now: Instant) {
        let active = self.keep_keyboard_grabbed || self.focused_surface.is_some();
        let actions = self.controller.poll(now.duration_since(self.start), active);
        for action in actions {
            self.on_action(action);
        }
    }

    fn on_key(&mut self, keysym: Keysym) {
        if let Some(action) = action_for_keysym(keysym) {
            self.on_action(action);
        }
    }

    /// Whether an application is on screen in front of this shell.
    fn app_running(&self) -> bool {
        self.app_label().is_some()
    }

    /// Whether the compositor reports the foreground application per display.
    fn foreground_is_per_display(&self) -> bool {
        self.shell_control
            .as_ref()
            .is_some_and(|control| control.version() >= PER_OUTPUT_SHELL_VERSION)
    }

    /// What to call the application the overlay is about.
    ///
    /// The overlay belongs to the display holding control, so this is that
    /// display's application — an application on another screen is not what
    /// the user is looking at, and offering to close it would be wrong.
    fn app_label(&self) -> Option<&str> {
        if self.foreground_is_per_display() {
            // This display's answer is the whole answer, including when it is
            // "nothing at all".
            return self.panels.get(self.focused_panel)?.foreground.as_deref();
        }
        // An older compositor only describes the session as a whole; without
        // the protocol at all, what this shell started is the best it can do
        // and the only thing it could act on anyway.
        self.foreground
            .as_deref()
            .or_else(|| self.xmb.running_app())
    }

    /// The footer's control summary, which only mentions moving between
    /// displays when there is somewhere to move to.
    fn control_hint(&self) -> String {
        let mut hint = String::from(
            "D-pad / stick move    A launch    Guide / B menu    Keyboard: arrows / Enter / Esc",
        );
        if self.panels.len() > 1 {
            hint.push_str("    L1 / R1 or Tab: screen");
        }
        hint
    }

    /// Name of the display being driven, when naming it tells the user
    /// anything — on a single display it only adds noise.
    fn screen_label(&self) -> Option<String> {
        if self.panels.len() < 2 {
            return None;
        }
        self.panels
            .get(self.focused_panel)
            .map(|panel| panel.name.clone())
    }

    fn on_action(&mut self, action: Action) {
        match action {
            Action::Guide => {
                // Read before the mode flips: `is_over_app` is true of the
                // menu too, and what matters is whether the bar was the thing
                // on screen before this press.
                let bar_on_top = self.guide.is_over_app();
                // The way out of a launch that is taking too long, or that the
                // user has changed their mind about. The application still
                // starts — the splash was only ever the shell's answer to the
                // press, and the user has stopped waiting for it.
                self.launching = None;
                if self.guide.toggle() {
                    self.begin_home_flight(bar_on_top);
                }
                self.needs_redraw = true;
            }
            Action::Back => self.on_back(),
            // Ahead of the menu, not behind it: the guide is where the user is
            // told the shoulder buttons move between displays, so that is the
            // last place they may stop working. The menu travels with them.
            Action::PrevScreen => self.focus_screen(-1),
            Action::NextScreen => self.focus_screen(1),
            _ if self.guide.is_menu() => self.on_menu_action(action),
            Action::Launch => {
                // Say where this is being launched from before starting it, so
                // the answer cannot be overtaken by the window itself.
                self.sync_launch_output();
                let Some(panel) = self.panels.get(self.focused_panel) else {
                    return;
                };
                // Everything the splash needs, read before the launch: the
                // tile it opens out of, and what was already on the display,
                // so the application's own window can be told from them.
                let opening = panel
                    .cursor
                    .current_app(&self.xmb)
                    .map(|app| (app.name.clone(), app.icon.clone()));
                let from = ui::launch_origin(panel.width as f32, panel.height as f32);
                let known: Vec<u32> = panel.windows.iter().map(|window| window.id).collect();
                let foreground = panel.foreground.clone().unwrap_or_default();
                // Disjoint fields, so the shared catalogue can be mutated while
                // this display's cursor is read.
                if let Some(pid) = self.xmb.launch_selected(&panel.cursor) {
                    self.needs_redraw = true;
                    if let Some((name, icon)) = opening {
                        self.launching = Some(launch::Launch::new(
                            name,
                            icon,
                            self.focused_panel,
                            from,
                            Some(pid),
                            Instant::now(),
                            launch::Before {
                                windows: &known,
                                foreground: &foreground,
                            },
                        ));
                    }
                }
                // A launched application takes the screen, so step back out of
                // the way and let it have the keyboard.
                if self.xmb.running_app().is_some() {
                    self.guide.close();
                }
            }
            _ => {
                let moved = match self.panels.get_mut(self.focused_panel) {
                    Some(panel) => panel.cursor.navigate(action, &self.xmb),
                    None => false,
                };
                if moved {
                    self.needs_redraw = true;
                }
            }
        }
    }

    /// Going back never quits outright — leaving is a deliberate choice in the
    /// menu, which is also what makes the menu discoverable.
    fn on_back(&mut self) {
        // One layer at a time: from the power dialog, back means back to the
        // menu it was opened from, not out of both.
        if self.guide.power_open() {
            self.guide.close_power();
            self.needs_redraw = true;
            return;
        }
        match self.guide.mode() {
            Mode::Menu => {
                self.guide.close();
                self.needs_redraw = true;
            }
            Mode::Bar | Mode::BarOverApp => self.open_guide(),
        }
    }

    /// Open the menu, settling where each display's start screen flies from.
    fn open_guide(&mut self) {
        // Read before opening: `is_over_app` is true of the menu too, and
        // what matters is whether the bar was drawn over the application
        // *before* this press.
        let bar_on_top = self.guide.is_over_app();
        self.guide.open();
        self.begin_home_flight(bar_on_top);
        self.needs_redraw = true;
    }

    /// Whether anything this display draws can still be seen.
    ///
    /// Behind a fullscreen application none of it can, and an animated
    /// background nobody is looking at costs exactly what the application in
    /// front — a game, usually — is asking the GPU for. Every frame the shell
    /// skips is also a frame the compositor does not have to composite. The
    /// guide is the exception: it is drawn *over* the application, which is
    /// the whole point of it.
    /// Bring the launch splash up to date, and let it go once it has handed
    /// the display over.
    ///
    /// Its own step rather than part of the drawing, because what it watches
    /// for — a window that was not there before — is the very thing that
    /// stops this display drawing at all: without it the splash would freeze
    /// at the moment the application covered the bar.
    fn advance_launch(&mut self, now: Instant) {
        if self.launching.is_none() {
            return;
        }
        let (known, foreground) = match self
            .launching
            .as_ref()
            .and_then(|splash| self.panels.get(splash.panel))
        {
            Some(panel) => (
                panel
                    .windows
                    .iter()
                    .map(|window| window.id)
                    .collect::<Vec<u32>>(),
                panel.foreground.clone().unwrap_or_default(),
            ),
            // Its display was unplugged mid-launch; nothing is going to show
            // up on it now.
            None => {
                self.launching = None;
                return;
            }
        };
        let alive = self
            .launching
            .as_ref()
            .and_then(|splash| splash.pid)
            .is_none_or(|pid| self.xmb.launch_alive(pid));

        let mut finished = false;
        if let Some(splash) = self.launching.as_mut() {
            if let Some(arrival) = splash.advance(now, &known, &foreground, alive) {
                tracing::debug!(
                    app = %splash.name,
                    ?arrival,
                    waited = now.duration_since(splash.started()).as_secs_f32(),
                    "launch splash handing the display over"
                );
            }
            finished = splash.finished(now);
        }
        if finished {
            self.launching = None;
        }
        self.needs_redraw = true;
    }

    /// Whether a launch splash is on `index` — which is also what keeps that
    /// display drawing while the application it is waiting for covers the bar.
    fn launching_on(&self, index: usize) -> bool {
        self.launching
            .as_ref()
            .is_some_and(|splash| splash.panel == index)
    }

    fn panel_is_visible(&self, index: usize) -> bool {
        let Some(panel) = self.panels.get(index) else {
            return false;
        };
        if self.launching_on(index) {
            return true;
        }
        if index == self.focused_panel && self.guide.is_over_app() {
            return true;
        }
        !bar_is_covered(panel.width, panel.height, &panel.windows)
    }

    /// Settle where each display's start screen flies from, for a menu that
    /// has just opened.
    ///
    /// A display showing its bar has the start screen filling it, so it
    /// shrinks into its card; one covered by an application never had it on
    /// screen, and flying it in from fullscreen would flash the whole bar
    /// over the application the menu is meant to be leaving. That card simply
    /// fades in with the others.
    fn begin_home_flight(&mut self, bar_on_top: bool) {
        for panel in &mut self.panels {
            panel.home_linear = home_flight_start(bar_on_top, !panel.windows.is_empty());
            // A start screen that does not fly in has to arrive some other
            // way: it fades up with the frames and titles, like the cards
            // whose windows are flying to meet them.
            panel.home_fades_in = panel.home_linear > 0.0;
        }
        self.menu_frame_drawn = false;
    }

    fn on_menu_action(&mut self, action: Action) {
        // The deck is every window plus the trailing start-screen card.
        let window_count = self
            .panels
            .get(self.focused_panel)
            .map(|panel| panel.windows.len() + 1)
            .unwrap_or(1);

        // The power dialog is modal: while it is up it takes every key, so
        // that a question about ending the session cannot be answered by
        // something the user meant for the menu behind it.
        if self.guide.power_open() {
            match action {
                Action::Up | Action::Down => {
                    let delta = if action == Action::Up { -1 } else { 1 };
                    if self.guide.move_power(delta) {
                        self.needs_redraw = true;
                    }
                }
                Action::Launch => {
                    if let Some(item) = self.guide.power_item() {
                        self.activate_power(item);
                    }
                }
                _ => {}
            }
            return;
        }

        let closable = self.closable();
        let app_running = self.app_running();

        match action {
            // Up/Down in the entry column scroll it; everything directional in
            // the cards pane, and the crossings between the two, live in the
            // guide's own focus model.
            Action::Up | Action::Down if self.guide.pane() == guide::Pane::Menu => {
                let delta = if action == Action::Up { -1 } else { 1 };
                if self.guide.move_selection(delta, closable) {
                    self.needs_redraw = true;
                }
            }
            Action::Up | Action::Down | Action::Left | Action::Right => {
                let direction = match action {
                    Action::Up => guide::Move::Up,
                    Action::Down => guide::Move::Down,
                    Action::Left => guide::Move::Left,
                    _ => guide::Move::Right,
                };
                if self.guide.move_focus(direction, window_count) {
                    self.needs_redraw = true;
                }
            }
            Action::Launch => match self.guide.pane() {
                guide::Pane::Windows => {
                    let index = self.guide.selected_window(window_count);
                    let card = self
                        .panels
                        .get(self.focused_panel)
                        .and_then(|panel| panel.windows.get(index));
                    match card {
                        Some(card) => self.activate_window(card.id),
                        // Past the windows: the start-screen card. Home is
                        // the bar — over the running application when there
                        // is one, plainly otherwise.
                        None => {
                            if app_running {
                                self.guide.show_bar_over_app();
                            } else {
                                self.guide.close();
                            }
                            self.needs_redraw = true;
                        }
                    }
                }
                guide::Pane::Menu => {
                    if let Some(item) = self.guide.selected_item(closable) {
                        self.activate(item);
                    }
                }
            },
            _ => {}
        }
    }

    /// Bring a window chosen in the overview to the front, and get out of the
    /// way: closing the menu also flies every window back to its real place.
    fn activate_window(&mut self, id: u32) {
        if let Some(control) = &self.shell_control {
            if control.version() >= OVERVIEW_SHELL_VERSION {
                control.activate_window(id);
                let _ = self.conn.flush();
            }
        }
        self.guide.close();
        self.needs_redraw = true;
    }

    /// Keep the compositor's window overview in step with the menu: on while
    /// the menu is open, on the display the user is driving, off otherwise.
    fn sync_overview(&mut self) {
        let Some(control) = self.shell_control.as_ref() else {
            return;
        };
        if control.version() < OVERVIEW_SHELL_VERSION {
            return;
        }

        // Not until the menu has been drawn once. The compositor stacks the
        // overview's cards *under* this surface, so windows that start flying
        // first are composited beneath whatever frame the shell last
        // committed — the bar, whole and fullscreen, printed over the cards
        // until the menu is drawn. One frame of it is one frame too many.
        let desired = (self.guide.is_menu() && self.menu_frame_drawn)
            .then(|| {
                self.panels
                    .get(self.focused_panel)
                    .map(|p| p.output.clone())
            })
            .flatten();
        // The compositor scrolls its card row from this, so it has to follow
        // every selection change, not just the overview toggling.
        let desired_selection = desired.as_ref().map(|_| {
            let count = self
                .panels
                .get(self.focused_panel)
                .map(|panel| panel.windows.len() + 1)
                .unwrap_or(1);
            self.guide.selected_window(count) as u32
        });

        let mut sent = false;
        if desired != self.applied_overview {
            // Entering on one display implicitly leaves on the others, so
            // only an outright close needs saying explicitly.
            match &desired {
                Some(output) => {
                    control.set_output_overview(output, 1);
                    // The gap between these two is what the cards are timed on.
                    tracing::debug!(after = self.guide.age(), "overview started");
                    // The moment the windows start flying, which is what the
                    // cards are decorated on.
                    self.overview_started_at = Some(Instant::now());
                }
                None => {
                    if let Some(previous) = &self.applied_overview {
                        control.set_output_overview(previous, 0);
                    }
                    self.overview_started_at = None;
                }
            }
            self.applied_overview = desired.clone();
            sent = true;
        }
        if desired_selection != self.applied_overview_selection {
            if let (Some(output), Some(index)) = (&desired, desired_selection) {
                control.set_overview_selection(output, index);
                sent = true;
            }
            self.applied_overview_selection = desired_selection;
        }

        if sent {
            if let Err(err) = self.conn.flush() {
                tracing::warn!(?err, "could not send the overview state");
            }
        }
    }

    /// How long the cards have been arriving, which is not how long the menu
    /// has been open.
    ///
    /// The frames, the titles and the start screen's own card are drawn over
    /// windows the *compositor* flies into place, and it cannot begin until it
    /// has been told to — a frame of the shell's own plus a round trip after
    /// the button was pressed. Timed from the press, the decoration arrives on
    /// windows that are still fullscreen, and the start screen appears on top
    /// of the very application the user is leaving.
    fn card_age(&self) -> f32 {
        card_age(
            self.overview_started_at
                .map(|at| at.elapsed().as_secs_f32()),
            self.guide.age(),
            self.overview_available(),
        )
    }

    /// Whether the compositor is going to fly anything at all. Without the
    /// overview there is nothing to wait for.
    fn overview_available(&self) -> bool {
        self.shell_control
            .as_ref()
            .is_some_and(|control| control.version() >= OVERVIEW_SHELL_VERSION)
    }

    /// The window whose card is selected, when it is a real window rather than
    /// the trailing start screen. What "Close" names, and acts on.
    fn close_target(&self) -> Option<&WindowCard> {
        let panel = self.panels.get(self.focused_panel)?;
        let index = self.guide.selected_window(panel.windows.len() + 1);
        panel.windows.get(index)
    }

    /// Whether the selected card is a window, which is what the entry column
    /// varies on.
    fn closable(&self) -> bool {
        self.close_target().is_some()
    }

    fn activate(&mut self, item: Item) {
        tracing::debug!(?item, "guide menu selection");
        match item {
            Item::Resume => self.guide.close(),
            Item::Dashboard => self.guide.show_bar_over_app(),
            Item::Close => {
                self.close_selected_window();
                // Stay in the menu. The card that was killed disappears from
                // the column and the next one takes its place, which is what
                // makes closing several applications one press each.
            }
            Item::Power => self.guide.open_power(),
        }
        self.needs_redraw = true;
    }

    fn activate_power(&mut self, item: guide::PowerItem) {
        tracing::info!(?item, "power dialog selection");
        match item {
            // The menu closes first either way: what the user should see
            // while the machine goes down is whatever they were doing, not
            // an overlay frozen mid-animation.
            guide::PowerItem::Suspend => {
                self.guide.close();
                run_detached("systemctl suspend", ["systemctl", "suspend"]);
            }
            guide::PowerItem::Shutdown => {
                self.guide.close();
                run_detached("systemctl poweroff", ["systemctl", "poweroff"]);
            }
            guide::PowerItem::Exit => {
                if let Some(control) = &self.shell_control {
                    control.quit();
                    let _ = self.conn.flush();
                }
                self.exit = true;
            }
            guide::PowerItem::Cancel => self.guide.close_power(),
        }
        self.needs_redraw = true;
    }

    /// End the application whose card is selected.
    ///
    /// Killed outright, not asked: this is the console's "close the game"
    /// button, and a game that puts up a "save first?" dialog behind an
    /// overlay the user has just dismissed has hung as far as they can tell.
    /// The compositor does the killing because it is the one that knows which
    /// process owns which window — including windows this shell never
    /// started.
    fn close_selected_window(&mut self) {
        let Some(id) = self.close_target().map(|window| window.id) else {
            return;
        };
        let Some(control) = self.shell_control.clone() else {
            // Standalone: our own children are all we can reach.
            self.xmb.terminate_launched_apps();
            return;
        };

        if control.version() >= KILL_SHELL_VERSION {
            control.kill_window(id);
        } else {
            // An older compositor can only be asked politely, and only about
            // whatever is in front on this display.
            match self.panels.get(self.focused_panel) {
                Some(panel) if self.foreground_is_per_display() => {
                    control.close_output_foreground(&panel.output)
                }
                _ => control.close_foreground(),
            }
        }

        if let Err(err) = self.conn.flush() {
            tracing::warn!(?err, "could not send the close request");
        }
    }

    /// Tell the compositor which display an application started here belongs
    /// on.
    ///
    /// Saying it outright rather than leaving it to be inferred from keyboard
    /// focus: focus has usually moved on to something else by the time the new
    /// window maps, and a focus change costs a round trip that a launch can
    /// otherwise overtake.
    fn sync_launch_output(&mut self) {
        let Some(control) = self.shell_control.as_ref() else {
            return;
        };
        // Added in version 2; an older compositor keeps inferring it.
        if control.version() < 2 {
            return;
        }
        let Some(panel) = self.panels.get(self.focused_panel) else {
            return;
        };
        if self.applied_launch_output.as_ref() == Some(&panel.output) {
            return;
        }

        control.set_launch_output(&panel.output);
        self.applied_launch_output = Some(panel.output.clone());
        tracing::debug!(display = %panel.name, "applications will open on this display");
        if let Err(err) = self.conn.flush() {
            tracing::warn!(?err, "could not send the launch display");
        }
    }

    fn sync_surface_state(&mut self) {
        let app_running = self.app_running();
        let states: Vec<(Layer, KeyboardInteractivity)> = (0..self.panels.len())
            .map(|index| {
                self.guide.surface_state(
                    index == self.focused_panel,
                    app_running,
                    self.keep_keyboard_grabbed,
                    self.launching_on(index),
                    self.base_layer,
                )
            })
            .collect();
        // Whether each display will be drawing frames that could carry the
        // change; see below.
        let drawing: Vec<bool> = (0..self.panels.len())
            .map(|index| self.panel_is_visible(index))
            .collect();
        let mut changed = false;

        for ((panel, desired), drawing) in self.panels.iter_mut().zip(states).zip(drawing) {
            if panel.applied_surface_state == Some(desired) {
                continue;
            }
            let (layer, interactivity) = desired;

            // `set_layer` arrived in version 2 of the protocol. Requesting it
            // on an older compositor is a fatal protocol error, so the overlay
            // simply stays on its original layer there.
            let can_set_layer = match panel.layer.kind() {
                SurfaceKind::Wlr(wlr) => wlr.version() >= 2,
                // `SurfaceKind` is non-exhaustive; a shell added later will not
                // be missing a request from 2018.
                _ => true,
            };
            if can_set_layer {
                panel.layer.set_layer(layer);
            } else if panel.applied_surface_state.is_none() {
                tracing::warn!(
                    "zwlr_layer_shell_v1 version 1: the guide overlay cannot rise above applications"
                );
            }

            panel.layer.set_keyboard_interactivity(interactivity);
            // Layer and interactivity are double-buffered, so they take effect
            // on the next commit of this surface — and who commits it depends
            // on whether the display is drawing.
            //
            // Drawing: leave it pending. Committing here would republish the
            // *last* frame at the new layer, so raising the menu would flash
            // the bar, whole and fullscreen, over the application for a frame
            // before the menu was drawn. The next frame carries the change
            // atomically with the content that belongs to it.
            //
            // Not drawing: commit now, because no frame is coming. This is
            // what a menu dismissed over a fullscreen application depends on:
            // the display goes quiet in the same breath, and a change left
            // pending would strand the menu on the overlay layer above that
            // application, still holding the keyboard, with no frame due that
            // could ever put it back.
            if !drawing {
                panel.layer.commit();
            }
            panel.applied_surface_state = Some(desired);
            changed = true;
        }

        if changed {
            if let Err(err) = self.conn.flush() {
                tracing::warn!(?err, "could not update the layer surfaces");
            }
            self.needs_redraw = true;
        }
    }
}

/// Whether a display draws this frame.
///
/// Hidden displays skip their drawing, but never *straight* away: a display
/// that was visible last frame draws one more, and one still animating draws
/// until it has settled. What stays committed is then the frame that belongs
/// to the state the display is in — which is what a translucent application
/// in front shows through, what the display returns to when it is uncovered,
/// and, since it is the commit that applies them, what carries any pending
/// layer or keyboard change with it.
fn should_draw(visible: bool, settling: bool, was_visible: bool) -> bool {
    visible || settling || was_visible
}

/// Whether an application on this display covers the bar completely.
///
/// The window list carries each window's logical size, so this is the shell
/// asking the question it actually cares about — is any of my drawing still
/// on screen — rather than assuming that anything running must be fullscreen.
/// A windowed application leaves the bar visible around it, and a bar that
/// froze there would look broken rather than thrifty.
fn bar_is_covered(width: u32, height: u32, windows: &[WindowCard]) -> bool {
    width > 0
        && height > 0
        && windows
            .iter()
            .any(|window| window.width >= width && window.height >= height)
}

/// Where a display's start screen flies from when the menu opens on it: 0.0
/// for the whole display, 1.0 for already sitting in its card.
///
/// It only flies from fullscreen if that is where the user was actually
/// looking. With an application covering the display the bar was not on
/// screen, and flying it in would flash it over that application — the very
/// thing the menu is there to step away from.
fn home_flight_start(bar_was_on_top: bool, application_covering: bool) -> f32 {
    if bar_was_on_top || !application_covering {
        0.0
    } else {
        1.0
    }
}

/// The clock the overview's cards are decorated on.
///
/// `started` is how long ago the compositor was told to begin, when it has
/// been; `guide_age` how long the menu has been open. While a compositor that
/// flies windows has not started yet the answer is zero — nothing has been
/// decorated because nothing has moved. Without one there is nothing to wait
/// for, and the cards may as well arrive with the menu.
fn card_age(started: Option<f32>, guide_age: f32, overview_available: bool) -> f32 {
    if overview_available {
        started.unwrap_or(0.0)
    } else {
        guide_age
    }
}

/// Whether the compositor's windows have finished flying into their cards, and
/// so whether their rects can be trusted as the shape of what is on screen.
///
/// Deliberately asked of the flight's own clock and nothing else. The shell's
/// animation is not a proxy for it: a menu opened over an application starts
/// with the start screen already in its card and the backdrop already blurred
/// — there was no bar on screen to fly or to blur out of — so every clock this
/// surface owns reads *arrived* on the first frame, while the windows still
/// have their whole flight ahead of them.
///
/// Late is free and early is not, hence [`CARD_SETTLE`]: a few more frames of
/// a square corner against a card that has stopped moving is nothing, whereas
/// one frame early paints backdrop over a window that is still crossing the
/// display.
fn cards_have_landed(card_age: f32) -> bool {
    card_age >= HOME_FLIGHT + CARD_SETTLE
}

/// Run a system command and forget about it.
///
/// Used for suspend and shutdown, which logind performs for the session that
/// asks; the shell has no more to do afterwards and nothing to report, since
/// the machine either goes down or the request was refused by a policy this
/// process cannot argue with. The failure is logged rather than shown: the
/// dialog it was chosen from has already closed, and there is nowhere left to
/// put a message.
fn run_detached<const N: usize>(what: &str, argv: [&str; N]) {
    let mut command = std::process::Command::new(argv[0]);
    command
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    match command.spawn() {
        Ok(mut child) => {
            // Reaped here rather than left a zombie: the shell outlives a
            // refused suspend.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(err) => tracing::warn!(?err, command = what, "could not run the power command"),
    }
}

/// One step of `step` towards `target`, stopping there rather than
/// overshooting. Reversing mid-flight simply changes the target, so the
/// animation continues from where it is instead of snapping.
fn approach(current: f32, target: f32, step: f32) -> f32 {
    if current < target {
        (current + step).min(target)
    } else {
        (current - step).max(target)
    }
}

/// Smooth acceleration in and out over 0..1 — the shape the *compositor*
/// flies its windows on, so the start screen travels with them.
///
/// The shell's own animations use [`ui::ease`], which is gentler off the mark.
/// This one is here to be matched, not chosen: changing it would leave the
/// start screen arriving on a different curve from the windows beside it.
fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// A rectangle on its way somewhere, and how fast each of its edges is going.
/// The velocity is what a spring carries between frames, and so what gives a
/// scroll a start rather than a launch.
#[derive(Debug, Clone, Copy)]
struct Glide {
    at: [f32; 4],
    velocity: [f32; 4],
}

fn lerp_rect(from: [f32; 4], to: [f32; 4], t: f32) -> [f32; 4] {
    let mut out = [0.0; 4];
    for (slot, (from, to)) in out.iter_mut().zip(from.iter().zip(&to)) {
        *slot = from + (to - from) * t;
    }
    out
}

/// One frame of a card's frame and title travelling to their slot, on the
/// spring the compositor glides the window itself with — the two halves of a
/// card stay together because they share both the stiffness and the solution.
fn spring_rect(glide: &mut Glide, target: [f32; 4], dt: f32) -> [f32; 4] {
    for ((at, speed), to) in glide.at.iter_mut().zip(&mut glide.velocity).zip(&target) {
        let (next, next_speed) = linboard_protocol::overview::spring(
            *at as f64,
            *speed as f64,
            *to as f64,
            linboard_protocol::overview::CARD_SPRING,
            dt as f64,
        );
        (*at, *speed) = (next as f32, next_speed as f32);
    }
    glide.at
}

/// The local wall clock, in the bar's corner format (`6/12 0:40`).
///
/// `None` when local time cannot be determined; the bar simply shows no clock
/// rather than a wrong one.
fn wall_clock() -> Option<String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    let secs = now.as_secs().min(libc::time_t::MAX as u64) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: localtime_r only reads `secs` and writes `tm`, both valid here.
    if unsafe { libc::localtime_r(&secs, &mut tm) }.is_null() {
        return None;
    }
    Some(format!(
        "{}/{} {}:{:02}",
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min
    ))
}

/// Parse one `seconds:action` pair of `--debug-actions`.
fn parse_timed_action(raw: &str) -> Result<(f32, Action), String> {
    let (at, name) = raw
        .split_once(':')
        .ok_or_else(|| format!("expected `seconds:action`, got {raw:?}"))?;
    let at: f32 = at
        .parse()
        .map_err(|_| format!("{at:?} is not a number of seconds"))?;
    let action = match name {
        "guide" => Action::Guide,
        "back" => Action::Back,
        "launch" => Action::Launch,
        "up" => Action::Up,
        "down" => Action::Down,
        "left" => Action::Left,
        "right" => Action::Right,
        "prev-screen" => Action::PrevScreen,
        "next-screen" => Action::NextScreen,
        other => return Err(format!("unknown action {other:?}")),
    };
    Ok((at, action))
}

fn action_for_keysym(keysym: Keysym) -> Option<Action> {
    match keysym {
        Keysym::Left | Keysym::KP_Left | Keysym::a | Keysym::A | Keysym::h | Keysym::H => {
            Some(Action::Left)
        }
        Keysym::Right | Keysym::KP_Right | Keysym::d | Keysym::D | Keysym::l | Keysym::L => {
            Some(Action::Right)
        }
        Keysym::Up | Keysym::KP_Up | Keysym::w | Keysym::W | Keysym::k | Keysym::K => {
            Some(Action::Up)
        }
        Keysym::Down | Keysym::KP_Down | Keysym::s | Keysym::S | Keysym::j | Keysym::J => {
            Some(Action::Down)
        }
        Keysym::Return | Keysym::KP_Enter | Keysym::space => Some(Action::Launch),
        Keysym::Escape | Keysym::BackSpace | Keysym::XF86_Back => Some(Action::Back),
        // Move between displays. xkb turns Shift+Tab into ISO_Left_Tab, so the
        // two directions arrive as distinct keysyms and no modifier state has
        // to be tracked to tell them apart.
        Keysym::Tab => Some(Action::NextScreen),
        Keysym::ISO_Left_Tab => Some(Action::PrevScreen),
        // The keyboard equivalents of a controller's guide button. Linboard
        // also forwards its own binding, which is what works while an
        // application holds the keyboard and this shell sees nothing.
        Keysym::Home | Keysym::XF86_HomePage | Keysym::Menu => Some(Action::Guide),
        _ => None,
    }
}

/// Adapts the renderer's atlas lookup to what the layout code needs.
struct Slots<'a>(&'a Gpu);

impl SlotLookup for Slots<'_> {
    fn slot_for(&self, icon: Option<&str>) -> Option<u32> {
        icon.and_then(|name| self.0.slot(name))
            .or_else(|| self.0.slot(FALLBACK_APP_ICON))
    }
}

impl LayerShellHandler for Shell {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, layer: &LayerSurface) {
        // Losing either of a panel's surfaces takes the whole panel: half a
        // display (bar without waves, or waves without a bar) helps nobody.
        self.panels
            .retain(|panel| &panel.layer != layer && &panel.backdrop != layer);
        // The session is over only when there is nowhere left to draw; a single
        // display going away is not a reason to quit.
        if self.panels.is_empty() {
            tracing::info!("every display closed the bar; exiting");
            self.exit = true;
        }
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        let Some(index) = self
            .panels
            .iter()
            .position(|panel| &panel.layer == layer || &panel.backdrop == layer)
        else {
            return;
        };
        let (width, height) = configure.new_size;
        // A zero size means "pick your own"; fall back to something sensible.
        self.panels[index].width = if width == 0 { 1280 } else { width };
        self.panels[index].height = if height == 0 { 720 } else { height };
        self.ensure_target(index);
        self.needs_redraw = true;
    }
}

impl CompositorHandler for Shell {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_factor: i32,
    ) {
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wayland_client::protocol::wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        if let Some(panel) = self.panels.iter_mut().find(|panel| panel.owns(surface)) {
            panel.frame_callback_pending = false;
        } else if let Some(panel) = self
            .panels
            .iter_mut()
            .find(|panel| panel.backdrop.wl_surface() == surface)
        {
            panel.backdrop_frame_pending = false;
        }
        // Any display asking for a frame paces the whole bar: the model behind
        // it is shared, so they animate together.
        self.needs_redraw = true;
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}

impl SeatHandler for Shell {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            match self.seat_state.get_keyboard(qh, &seat, None) {
                Ok(keyboard) => self.keyboard = Some(keyboard),
                Err(err) => tracing::warn!(?err, "could not obtain the keyboard"),
            }
        }
    }

    fn remove_capability(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard {
            self.focused_surface = None;
            if let Some(keyboard) = self.keyboard.take() {
                keyboard.release();
            }
        }
    }

    fn remove_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: wl_seat::WlSeat) {
    }
}

impl KeyboardHandler for Shell {
    fn enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _serial: u32,
        _raw: &[u32],
        _keysyms: &[Keysym],
    ) {
        self.focused_surface = Some(surface.clone());
        tracing::debug!("keyboard focus entered the shell");
    }

    fn leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _serial: u32,
    ) {
        // Focus can move straight from one of our displays to another, and the
        // leave for the old one may arrive after the enter for the new.
        if self.focused_surface.as_ref() == Some(surface) {
            self.focused_surface = None;
            tracing::debug!("keyboard focus left the shell; controller navigation paused");
        }
    }

    fn press_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        event: KeyEvent,
    ) {
        self.on_key(event.keysym);
    }

    fn repeat_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        event: KeyEvent,
    ) {
        // Held arrows should scroll, so repeats are treated as presses.
        self.on_key(event.keysym);
    }

    fn release_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        _event: KeyEvent,
    ) {
    }

    fn update_modifiers(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        _modifiers: Modifiers,
        _raw_modifiers: RawModifiers,
        _layout: u32,
    ) {
    }
}

impl OutputHandler for Shell {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        // A display plugged in mid-session gets the bar too.
        self.add_panel(qh, output);
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
        // A mode or scale change arrives as a layer-surface configure, which is
        // where the swapchain is resized.
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        self.remove_panel(&output);
    }
}

delegate_registry!(Shell);

impl ProvidesRegistryState for Shell {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

smithay_client_toolkit::delegate_dispatch2!(Shell);

impl Dispatch<LinboardShellV1, ()> for Shell {
    fn event(
        state: &mut Self,
        _proxy: &LinboardShellV1,
        event: linboard_shell_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            linboard_shell_v1::Event::Guide => {
                tracing::debug!("guide binding forwarded by the compositor");
                state.on_action(Action::Guide);
            }
            linboard_shell_v1::Event::Foreground { title } => {
                state.foreground = (!title.is_empty()).then_some(title);
                tracing::debug!(foreground = ?state.foreground, "foreground application changed");
                // The menu offers different entries with and without an
                // application, so it has to be redrawn.
                state.needs_redraw = true;
            }
            linboard_shell_v1::Event::OutputForeground { output, title } => {
                let title = (!title.is_empty()).then_some(title);
                // A display this shell has no panel for yet: the compositor
                // announces an output before the panel for it exists. Its
                // starting value is "nothing", which is what this would be.
                if let Some(panel) = state.panels.iter_mut().find(|p| p.output == output) {
                    if panel.foreground != title {
                        tracing::debug!(
                            display = %panel.name,
                            foreground = ?title,
                            "foreground application changed on a display"
                        );
                        panel.foreground = title;
                        state.needs_redraw = true;
                    }
                }
            }
            linboard_shell_v1::Event::OutputWindow {
                output,
                id,
                title,
                width,
                height,
            } => {
                if let Some(panel) = state.panels.iter_mut().find(|p| p.output == output) {
                    panel.pending_windows.push(WindowCard {
                        id,
                        title,
                        width,
                        height,
                    });
                }
            }
            linboard_shell_v1::Event::OutputWindowsDone { output } => {
                if let Some(panel) = state.panels.iter_mut().find(|p| p.output == output) {
                    let windows = std::mem::take(&mut panel.pending_windows);
                    if panel.windows != windows {
                        tracing::debug!(
                            display = %panel.name,
                            count = windows.len(),
                            "window list changed on a display"
                        );
                        panel.windows = windows;
                        state.needs_redraw = true;
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod flight_tests {
    use super::*;

    /// The bug: the start screen's card appearing on top of the application
    /// the user is leaving, because the shell decorated the overview on its
    /// own clock while the compositor had not begun flying anything yet.
    #[test]
    fn cards_are_decorated_on_the_compositors_clock_not_the_menus() {
        // Menu open for a while, compositor not yet told to start: nothing
        // has moved, so nothing is drawn on the cards.
        assert_eq!(card_age(None, 0.4, true), 0.0);

        // Once it has, the cards age with the flight rather than the menu.
        assert_eq!(card_age(Some(0.1), 0.4, true), 0.1);
        assert!(ui::card_fade(card_age(None, 0.4, true)) == 0.0);
        assert!(ui::card_fade(card_age(Some(0.5), 0.9, true)) == 1.0);

        // No overview to wait for — an older compositor, or none at all —
        // and the cards arrive with the menu, as they always did.
        assert_eq!(card_age(None, 0.4, false), 0.4);
    }

    /// The same bug in its second form: a card's corners repaired with
    /// backdrop while its window was still crossing the display, which drew a
    /// band of wallpaper through the middle of it.
    #[test]
    fn a_cards_corners_are_not_repaired_until_its_window_has_landed() {
        // A menu opened over an application: nothing of the shell's own
        // animates, so its clocks say "arrived" from the first frame. Only
        // the flight's clock knows better.
        assert!(!cards_have_landed(card_age(Some(0.0), 0.0, true)));
        assert!(!cards_have_landed(HOME_FLIGHT / 2.0));
        // Not on the stroke of the flight either: the compositor still has to
        // render that last step and have it scanned out.
        assert!(!cards_have_landed(HOME_FLIGHT));
        assert!(cards_have_landed(HOME_FLIGHT + CARD_SETTLE));

        // And the wait stays short enough to be over before the cards have
        // finished fading in, so the corners never round in plain sight.
        assert!(ui::card_fade(HOME_FLIGHT + CARD_SETTLE) < 1.0);
    }

    /// A press that lands mid-flight must carry on from where the start
    /// screen is, not restart it — that is the whole reason the position is
    /// kept linear and shaped only when it is used.
    #[test]
    fn a_reversed_flight_continues_from_where_it_is() {
        let step = (1.0 / 60.0) / HOME_FLIGHT;

        let mut position = 0.0;
        for _ in 0..9 {
            position = approach(position, 1.0, step);
        }
        assert!(position > 0.0 && position < 1.0, "should be mid-flight");

        // Turning round: the next frame is a step back from here, not a jump.
        let reversed = approach(position, 0.0, step);
        assert!((position - reversed - step).abs() < 1e-6);
    }

    #[test]
    fn a_flight_settles_exactly_on_its_target() {
        let mut position = 0.4;
        for _ in 0..1000 {
            position = approach(position, 1.0, 0.05);
        }
        assert_eq!(position, 1.0, "must land, not creep past or short");

        for _ in 0..1000 {
            position = approach(position, 0.0, 0.05);
        }
        assert_eq!(position, 0.0);
    }

    #[test]
    fn the_flight_eases_in_and_out_of_both_ends() {
        assert_eq!(smoothstep(0.0), 0.0);
        assert_eq!(smoothstep(1.0), 1.0);
        assert!((smoothstep(0.5) - 0.5).abs() < 1e-6);
        // Slower at the ends than through the middle.
        assert!(smoothstep(0.1) < 0.1);
        assert!(smoothstep(0.9) > 0.9);
        // Out-of-range positions cannot fling the start screen off screen.
        assert_eq!(smoothstep(-1.0), 0.0);
        assert_eq!(smoothstep(2.0), 1.0);
    }

    #[test]
    fn the_flight_runs_between_the_display_and_the_card() {
        let display = [0.0, 0.0, 1920.0, 1080.0];
        let card = [700.0, 260.0, 1000.0, 560.0];
        assert_eq!(lerp_rect(display, card, 0.0), display);
        assert_eq!(lerp_rect(display, card, 1.0), card);

        let half = lerp_rect(display, card, 0.5);
        assert_eq!(half, [350.0, 130.0, 1460.0, 820.0]);
    }

    fn window(width: u32, height: u32) -> WindowCard {
        WindowCard {
            id: 1,
            title: "App".into(),
            width,
            height,
        }
    }

    /// The regression this rule exists for: dismissing the menu over a
    /// fullscreen application hid the display in the same breath, so the
    /// frame that would have taken the menu off the overlay layer was never
    /// drawn — and the menu sat there on top of the application, holding the
    /// keyboard, with nothing due that could clear it.
    #[test]
    fn a_display_draws_one_last_frame_after_it_is_covered() {
        // Visible, then covered with nothing left to animate: one more frame.
        let mut was_visible = true;
        assert!(should_draw(false, false, was_visible));

        // Having drawn it, and only then, it goes quiet.
        was_visible = false;
        assert!(!should_draw(false, false, was_visible));

        // An animation still running keeps it drawing until it settles, so
        // the frame left behind is never a half-finished one.
        assert!(should_draw(false, true, false));
        // And a visible display always draws, settled or not.
        assert!(should_draw(true, false, false));
    }

    /// Animating a background nobody can see costs a game the frames it is
    /// asking for, so a covered bar stops drawing.
    #[test]
    fn a_fullscreen_application_covers_the_bar() {
        assert!(bar_is_covered(1920, 1080, &[window(1920, 1080)]));
        // Larger than the display (an oversized or scaled surface) still
        // covers it.
        assert!(bar_is_covered(1920, 1080, &[window(2560, 1440)]));
        // The covering window need not be the topmost one.
        assert!(bar_is_covered(
            1920,
            1080,
            &[window(400, 300), window(1920, 1080)]
        ));
    }

    /// But a bar that froze while it was still on screen would read as a
    /// hang, so anything short of full coverage keeps it running.
    #[test]
    fn a_windowed_application_leaves_the_bar_drawing() {
        assert!(!bar_is_covered(1920, 1080, &[]));
        assert!(!bar_is_covered(1920, 1080, &[window(1900, 1080)]));
        assert!(!bar_is_covered(1920, 1080, &[window(1920, 1000)]));
        // A display that has not been configured yet knows nothing about
        // what covers it; it must not conclude that it is hidden.
        assert!(!bar_is_covered(0, 0, &[window(1920, 1080)]));
    }

    /// The bug this rule exists for: opening the menu over an application
    /// must not fly the whole bar across it on the way to its card.
    #[test]
    fn the_start_screen_only_flies_from_the_display_it_was_filling() {
        // Nothing running: the bar was the screen, so it shrinks into place.
        assert_eq!(home_flight_start(false, false), 0.0);
        // An application covering it: the card is where it already was.
        assert_eq!(home_flight_start(false, true), 1.0);
        // Unless the bar was summoned over that application, in which case it
        // was on screen after all.
        assert_eq!(home_flight_start(true, true), 0.0);
    }
}

#[cfg(test)]
mod input_tests {
    use super::*;

    #[test]
    fn debug_actions_parse_as_a_timed_script() {
        let cli = Cli::try_parse_from(["linboard-xmb", "--debug-actions", "2:guide,3.5:right"])
            .expect("a timed action list should parse");
        assert_eq!(
            cli.debug_actions,
            vec![(2.0, Action::Guide), (3.5, Action::Right)]
        );
        assert!(parse_timed_action("2:sideways").is_err());
        assert!(parse_timed_action("guide").is_err());
    }

    #[test]
    fn boolean_options_are_value_less_switches() {
        let cli = Cli::try_parse_from(["linboard-xmb", "--grab-keyboard", "--no-gamepad"])
            .expect("boolean switches should parse without explicit true values");
        assert!(cli.grab_keyboard);
        assert!(cli.no_gamepad);
    }

    #[test]
    fn keyboard_navigation_supports_arrows_keypad_wasd_and_vim() {
        for keysym in [
            Keysym::Left,
            Keysym::KP_Left,
            Keysym::a,
            Keysym::A,
            Keysym::h,
        ] {
            assert_eq!(action_for_keysym(keysym), Some(Action::Left));
        }
        for keysym in [
            Keysym::Right,
            Keysym::KP_Right,
            Keysym::d,
            Keysym::D,
            Keysym::l,
        ] {
            assert_eq!(action_for_keysym(keysym), Some(Action::Right));
        }
        for keysym in [Keysym::Up, Keysym::KP_Up, Keysym::w, Keysym::W, Keysym::k] {
            assert_eq!(action_for_keysym(keysym), Some(Action::Up));
        }
        for keysym in [
            Keysym::Down,
            Keysym::KP_Down,
            Keysym::s,
            Keysym::S,
            Keysym::j,
        ] {
            assert_eq!(action_for_keysym(keysym), Some(Action::Down));
        }
    }

    #[test]
    fn keyboard_accept_and_back_have_controller_style_aliases() {
        for keysym in [Keysym::Return, Keysym::KP_Enter, Keysym::space] {
            assert_eq!(action_for_keysym(keysym), Some(Action::Launch));
        }
        for keysym in [Keysym::Escape, Keysym::BackSpace, Keysym::XF86_Back] {
            assert_eq!(action_for_keysym(keysym), Some(Action::Back));
        }
    }

    #[test]
    fn no_key_quits_outright() {
        // Leaving the session is a deliberate choice in the guide menu, so no
        // single keypress may end it — least of all Escape, which is what a
        // user presses to get out of a launched application.
        for keysym in [
            Keysym::Escape,
            Keysym::BackSpace,
            Keysym::XF86_Back,
            Keysym::q,
            Keysym::Q,
        ] {
            assert!(
                matches!(action_for_keysym(keysym), None | Some(Action::Back)),
                "{keysym:?} should not end the session on its own"
            );
        }
    }

    #[test]
    fn home_keys_summon_the_guide() {
        for keysym in [Keysym::Home, Keysym::XF86_HomePage, Keysym::Menu] {
            assert_eq!(action_for_keysym(keysym), Some(Action::Guide));
        }
    }
}
