//! LineXinBar Desktop — a cross-media-bar style shell for the LineXinBar compositor.
//!
//! It is an ordinary Wayland client that binds `zwlr_layer_shell_v1`, so it
//! runs on LineXinBar as well as any other compositor implementing layer-shell.
//! That also makes it debuggable on its own, without starting a compositor.

mod appinfo;
mod apps;
mod controller;
mod dialog;
mod gpu;
mod guide;
mod icons;
mod keyboard;
mod launch;
mod menu;
mod model;
mod pointer;
mod screenshot;
mod settings;
mod steam_hid;
mod system;
mod theme;
mod ui;
mod uninstall;

use std::collections::HashSet;
use std::ffi::OsString;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use clap::Parser;
use lxb_protocol::client::lxb_shell_v1::{self, LxbShellV1};
use smithay_client_toolkit::compositor::{
    CompositorHandler, CompositorState, FrameCallbackData, Region,
};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::keyboard::{
    KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers,
};
use smithay_client_toolkit::seat::pointer::{
    cursor_shape::CursorShapeManager, PointerEvent, PointerEventKind, PointerHandler, BTN_LEFT,
};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
    LayerSurfaceConfigure, SurfaceKind,
};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::{delegate_registry, registry_handlers};
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_surface};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};

use crate::controller::ControllerInput;
use crate::gpu::Gpu;
use crate::guide::{Guide, Item, Mode};
use crate::icons::IconLoader;
use crate::model::{Action, Cursor, Xmb};
use crate::pointer::{Prefs, Stick};
use crate::system::{Knob, Quick};
use crate::ui::SlotLookup;

/// Size icons are decoded at; matches the atlas cell.
const ICON_SIZE: u32 = 128;

/// First `lxb_shell_v1` version that reports the foreground application
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

/// First version that will move the seat's pointer for the shell, and that
/// names the application in front of each display well enough to remember a
/// setting against. Below it the stick-pointer tile is left out of the menu
/// entirely: there is nothing it could do.
///
/// The highest this shell asks for. Version 6 has no constant of its own
/// because nothing is gated on it: it added the on-screen keyboard binding,
/// and an event arriving is all the proof a shell needs that it was sent.
const POINTER_SHELL_VERSION: u32 = 7;

/// First version with the key the shell can press on the seat's keyboard,
/// which is what the D-pad becomes while the pointer is being aimed. Gated
/// separately from the pointer: on a compositor with version 7 the stick still
/// points and clicks, and only the arrows are missing.
const KEYBOARD_KEY_VERSION: u32 = 8;

/// First version that can put a display into high dynamic range, and that says
/// which displays are capable of it. The highest this shell asks for. Below it
/// the Settings column still draws the page — the setting is remembered, and
/// the file it is written to is read by whichever compositor comes next — but
/// nothing is sent and the page reports HDR as unsupported, which is the truth
/// on a compositor that cannot do it.
const HDR_SHELL_VERSION: u32 = 9;

/// First version that lists what each display can be driven at, and can be
/// asked to change it. Below it no display reports any modes, so Settings >
/// Display > Resolution says there is nothing to choose — which is the truth
/// on a compositor that cannot be asked.
///
/// Version 10 has no constant of its own, for the reason version 6 has none:
/// nothing is gated on it. It added the event saying which HDR settings a
/// display can honour, and the event arriving is all the proof a shell needs —
/// until one does, every control is offered on every capable display, which is
/// what a shell that cannot be told otherwise has to assume.
const MODES_SHELL_VERSION: u32 = 11;

/// First version that will put the cursor away when asked. Below it the
/// compositor still hides the cursor for its own keyboard, and a controller
/// simply leaves it on screen: the shell reads the pad from `/dev/input`, so
/// there is nothing for a compositor that cannot be told to notice.
const HIDE_POINTER_VERSION: u32 = 12;

/// First version that will fly a window back out of the tile it was asked for
/// on.
///
/// Below it the window is raised and simply appears. The shell cannot make up
/// the difference: an application already running is given back by showing
/// *its* window arriving, and only the compositor has that window's pixels.
const RESTORE_SHELL_VERSION: u32 = 14;

/// First version that will move a window to another display and photograph
/// one. The highest this shell asks for.
///
/// Below it the guide's menu still draws both rows and both refuse: neither is
/// something a shell can do for itself. The window's place in the layout
/// belongs to the compositor, and so do its pixels.
const WINDOW_MOVE_AND_CAPTURE_VERSION: u32 = 15;

/// First version that names the application behind every window rather than
/// only the one in front.
///
/// Nothing is gated on it, because the answer arrives as data: below it every
/// window's application is the empty string, which matches no tile, and a tile
/// press starts the application instead of coming back to it — the behaviour
/// this shell had before it could ask. Kept so the ladder of versions can be
/// read off in one place.
#[allow(dead_code)]
const WINDOW_APP_ID_VERSION: u32 = 13;

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
const HOME_FLIGHT: f32 = lxb_protocol::overview::FLIGHT.as_millis() as f32 / MILLIS_PER_SECOND;

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

/// How long after the overview starts the cards are really in their slots, and
/// so the first moment anything may be drawn on them.
///
/// One answer for every mark the shell makes on a card — the frames, the
/// titles, the selection, the start screen's own miniature and the corners
/// repaired around it — because they all share the one hazard: the windows
/// underneath belong to the compositor, and until the flight is over they are
/// not where the layout says they are.
pub const CARD_ARRIVAL: f32 = HOME_FLIGHT + CARD_SETTLE;

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
#[command(name = "lxb-desktop", version, about, long_about = None)]
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
    /// Actions are `guide`, `keyboard`, `back`, `launch`, `up`, `down`,
    /// `left`, `right`, `prev-screen`, `next-screen`.
    ///
    /// Development aid: most of this shell's design is in its transitions,
    /// and they cannot be inspected — or screenshotted at a chosen moment —
    /// without a way to drive them that does not need a controller in hand.
    #[arg(long, hide = true, value_delimiter = ',', value_parser = parse_timed_action)]
    debug_actions: Vec<(f32, Action)>,

    /// Pretend these displays reported HDR support, as a comma-separated list
    /// of `name:peak:gamut` (`--debug-hdr-displays SCREEN-A:600:1,SCREEN-B:400:0`).
    /// `name` is any connector name; `peak` is cd/m², 0 for a display that
    /// does not say; `gamut` is 1 when sRGB colour intensity should be
    /// offered.
    ///
    /// Development aid, and the counterpart of `--debug-actions`: the screen
    /// list under Settings > Display > HDR is built from what the compositor
    /// reports, so on a machine whose displays are SDR — or in a nested
    /// session, which owns no connector at all — that page cannot be looked
    /// at. Nothing is sent to the compositor for these; they only populate the
    /// menu.
    #[arg(long, hide = true, value_delimiter = ',', value_parser = parse_debug_display)]
    debug_hdr_displays: Vec<(String, settings::Support)>,

    /// Pretend these displays offered these modes, as a comma-separated list of
    /// `name:WxH@Hz/WxH@Hz/…` (`--debug-display-modes SCREEN-A:3840x2160@120/1920x1080@60`).
    /// The first mode of each display is the one it is running.
    ///
    /// The counterpart of `--debug-hdr-displays`, and needed for the same
    /// reason: the resolution page is built from the connector mode lists the
    /// compositor reports, and a nested session owns no connector, so on a
    /// development machine that page has nothing in it. Nothing is sent to the
    /// compositor for these.
    #[arg(long, hide = true, value_delimiter = ',', value_parser = parse_debug_modes)]
    debug_display_modes: Vec<(String, Vec<settings::Offered>)>,
}

/// `name:peak:gamut`, for `--debug-hdr-displays`.
fn parse_debug_display(raw: &str) -> Result<(String, settings::Support), String> {
    let mut fields = raw.split(':');
    let name = fields.next().unwrap_or_default().trim();
    if name.is_empty() {
        return Err(format!("{raw:?} names no display"));
    }
    let number = |field: Option<&str>, default: u32| -> Result<u32, String> {
        match field.map(str::trim).filter(|field| !field.is_empty()) {
            Some(field) => field
                .parse()
                .map_err(|_| format!("{field:?} is not a number")),
            None => Ok(default),
        }
    };
    let peak = number(fields.next(), 0)?;
    let gamut = number(fields.next(), 1)?;
    Ok((
        name.to_string(),
        settings::Support {
            available: true,
            active: false,
            peak: peak.min(u16::MAX as u32) as u16,
            gamut: gamut != 0,
        },
    ))
}

/// `name:WxH@Hz/WxH@Hz/…`, for `--debug-display-modes`.
fn parse_debug_modes(raw: &str) -> Result<(String, Vec<settings::Offered>), String> {
    let (name, modes) = raw
        .split_once(':')
        .ok_or_else(|| format!("{raw:?} is not NAME:MODE/MODE"))?;
    let name = name.trim();
    if name.is_empty() {
        return Err(format!("{raw:?} names no display"));
    }
    let modes = modes
        .split('/')
        .filter(|mode| !mode.trim().is_empty())
        .enumerate()
        .map(|(index, mode)| {
            let (size, refresh) = match mode.split_once('@') {
                Some((size, refresh)) => (size, Some(refresh)),
                None => (mode, None),
            };
            let (width, height) = size
                .split_once('x')
                .ok_or_else(|| format!("{mode:?} is not WIDTHxHEIGHT"))?;
            let number = |raw: &str| -> Result<f64, String> {
                raw.trim()
                    .trim_end_matches("Hz")
                    .trim()
                    .parse()
                    .map_err(|_| format!("{raw:?} is not a number"))
            };
            Ok(settings::Offered {
                mode: settings::Mode {
                    resolution: settings::Resolution {
                        width: number(width)? as u32,
                        height: number(height)? as u32,
                    },
                    // Hertz on the command line, mHz everywhere else, so
                    // `@59.94` means what it says.
                    refresh: (refresh.map(number).transpose()?.unwrap_or(0.0) * 1000.0).round()
                        as u32,
                },
                // The first is what the display is running, which is the one
                // shape of these pages that cannot be described any other way.
                current: index == 0,
                preferred: index == 0,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    if modes.is_empty() {
        return Err(format!("{raw:?} lists no modes"));
    }
    Ok((name.to_string(), modes))
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

    // Before the catalogue: the Settings column shows which value is in force,
    // so the shell has to know what it is set to before it builds the rows
    // that say so — and before the first frame is drawn in a colour.
    settings::load();

    let categories = apps::scan();
    let total: usize = categories.iter().map(apps::Category::apps).sum();
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

    // LineXinBar's own protocol, which carries the guide binding and lets the
    // overlay close an application. Absent on every other compositor, where the
    // shell simply falls back to what it can do as an ordinary client.
    let shell_control = match globals.bind::<LxbShellV1, _, _>(
        &qh,
        1..=WINDOW_MOVE_AND_CAPTURE_VERSION,
        (),
    ) {
        Ok(control) => Some(control),
        Err(err) => {
            tracing::info!(
                %err,
                "lxb_shell_v1 unavailable; the guide overlay will only manage what this shell started"
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
        debug_hdr_displays: (!cli.debug_hdr_displays.is_empty())
            .then(|| cli.debug_hdr_displays.clone()),
        debug_display_modes: (!cli.debug_display_modes.is_empty())
            .then(|| cli.debug_display_modes.clone()),
        shell_control,
        foreground: None,
        applied_launch_output: None,
        applied_overview: None,
        applied_overview_selection: None,
        guide: Guide::default(),
        context_menu: menu::Menu::default(),
        dialog: dialog::Dialog::default(),
        app_facts: None,
        pending_capture: None,
        removal_plan: None,
        uninstalling: None,
        quick: Quick::start(),
        osk: keyboard::Osk::default(),
        menu_frame_drawn: false,
        overview_started_at: None,
        launching: None,
        restoring: None,
        guide_card_rects: std::collections::HashMap::new(),
        keyboard: None,
        pointer: None,
        cursor_shape: CursorShapeManager::bind(&globals, &qh).ok(),
        focused_surface: None,
        keep_keyboard_grabbed: cli.grab_keyboard,
        controller: ControllerInput::new(!cli.no_gamepad),
        stick: Stick::pointer(),
        scroll: Stick::scroll(),
        prefs: Prefs::load(),
        stick_buttons: Vec::new(),
        stick_keys: Vec::new(),
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

    // The keyboard hangs off a seat, which only exists after that roundtrip.
    // The first seat: a console has one, and an input method is per seat, so
    // taking them all would mean several keyboards fighting over one screen.
    match shell.seat_state.seats().next() {
        Some(seat) => {
            shell.osk.attach(&globals, &qh, &seat);
        }
        None => tracing::info!("no seat; the on-screen keyboard is off"),
    }

    for output in shell.output_state.outputs().collect::<Vec<_>>() {
        shell.add_panel(&qh, output);
    }
    if shell.panels.is_empty() {
        anyhow::bail!("this compositor advertises no outputs to draw on");
    }
    // The screen lists under Settings > Display are built from what the
    // displays report. Seed them before the first frame so the pages are right
    // the first time they are opened rather than after the first change.
    shell.refresh_hdr_support();
    shell.refresh_display_modes();

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
        // Input-method focus can open or close the on-screen keyboard without
        // going through `on_action`. Reconcile after every source of input has
        // settled so a keyboard covering the values suspends their preview,
        // and closing it resumes the value still under the cursor.
        shell.sync_setting_preview();
        // A package manager answering is not a Wayland event and cannot wake
        // this loop, but the loop wakes anyway to poll the controller — which
        // is the only reason a worker can hand its answer to a frame at all.
        shell.sync_app_facts();
        // And likewise for a removal: neither the survey nor the removal itself
        // is a Wayland event, so the frame this loop was going to draw anyway is
        // what carries their answers on to the screen.
        shell.advance_uninstall();
        // Applies whatever the last events settled on: an application exiting
        // changes what the surface should be doing just as much as a keypress.
        shell.sync_surface_state();
        shell.sync_launch_output();
        shell.sync_overview();
        // Last of the three, and unconditional: a display plugged in
        // mid-session has to be told what the session is set to, and the only
        // thing that knows it has not been told is the diff inside this.
        shell.sync_hdr();
        shell.sync_mode();
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
             start lxb-desktop with a named WAYLAND_DISPLAY instead"
        );
    }

    let wayland =
        std::env::var_os("WAYLAND_DISPLAY").unwrap_or_else(|| OsString::from("wayland-0"));
    let xwayland = std::env::var_os("LXB_XWAYLAND_DISPLAY");
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

    // The shell's own glyphs first, so a session with no icon theme installed
    // at all still has a speaker and a sun on its quick-settings bars.
    for (name, drawing) in icons::BUILTIN {
        match icons::Icon::builtin(drawing, ICON_SIZE) {
            Some(icon) => out.push((name.to_string(), icon)),
            None => tracing::warn!(glyph = name, "could not rasterise a built-in glyph"),
        }
        seen.insert(name.to_string());
    }

    // Every row of every column, subcategories included: what a column holds
    // is a tree, and an icon missed here is one the atlas has no slot for.
    let mut rows: Vec<String> = Vec::new();
    for category in categories {
        apps::walk(&category.entries, &mut |entry| {
            if let Some(icon) = entry.icon() {
                rows.push(icon.to_string());
            }
        });
    }

    let names = std::iter::once(FALLBACK_APP_ICON.to_string())
        .chain(categories.iter().map(|c| c.icon.to_string()))
        .chain(rows);

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

/// An application the user has agreed to remove, and how far that has got.
///
/// One at a time, which needs no enforcing: the panel driving it takes every
/// button while it is up, so there is never a second answer to the question of
/// what is being removed.
struct Uninstall {
    app: apps::App,
    survey: uninstall::Survey,
    stage: Stage,
}

/// The steps a removal goes through, in the order it goes through them.
enum Stage {
    /// Waiting to be told what removing this takes, and whether it may be done
    /// at all.
    Surveying,
    /// The panel asking for a password is up, and this is what has been typed.
    ///
    /// The password lives here and in no other field of the shell. The panel is
    /// drawn from a count of characters and nothing else — see
    /// [`dialog::Line::Secret`] — and this goes away with the stage.
    Asking {
        removal: uninstall::Removal,
        password: uninstall::Secret,
    },
    /// The removal itself is running.
    Working(uninstall::Run),
    /// It is over, and the panel saying how is the last thing up.
    Done,
}

/// What the shell has to put on screen next, worked out with only the removal's
/// own state borrowed.
///
/// The two halves of [`Shell::advance_uninstall`] are split by this: deciding
/// needs the removal borrowed and acting needs the whole shell, and one
/// function doing both in one pass cannot borrow either.
enum Step {
    Nothing,
    /// Nothing on this machine claims the application.
    CannotPlace,
    /// It could be removed, but not by this user.
    Forbidden,
    /// A password is wanted. `retry` is whether the last one was refused.
    AskPassword {
        retry: bool,
    },
    /// It is being removed now.
    Removing,
    Removed,
    Failed(String),
}

/// Which way along the row of displays something is being sent.
///
/// The displays are a row in the order the compositor announced them — the same
/// order L1/R1 walk control along — so a direction is one step through that
/// list and nothing about where the screens physically stand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Toward {
    Next,
    Previous,
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
    /// What that application *is*, rather than what its window says: the
    /// `app_id` or X11 class the compositor reports. Per-application settings
    /// are filed under this, never under the title, which is a document name.
    app_id: Option<String>,
    /// What this display can do in high dynamic range, and whether it is
    /// doing it. Per display because capability is: one screen on a machine
    /// can be an HDR television and the other a laptop panel.
    hdr: settings::Support,
    /// The HDR settings this display was last told to use, so an unchanged
    /// choice is not resent every frame.
    applied_hdr: Option<settings::Hdr>,
    /// What this display can be driven at, as the compositor lists it. Empty
    /// where there is nothing to choose: a nested session, or a compositor too
    /// old to be asked.
    modes: Vec<settings::Offered>,
    /// Batch under construction; becomes `modes` on the done event.
    pending_modes: Vec<settings::Offered>,
    /// The resolution this display was last asked for, so a choice is not
    /// resent — and so a display is not put back into a mode the user changed
    /// from somewhere else.
    applied_mode: Option<settings::Mode>,
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
    applied_surface_state: Option<((Layer, KeyboardInteractivity), Clickable)>,
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
    /// How far this display's start screen has stepped back from the viewer to
    /// let one of its own panels stand over it — see [`ui::recede_into_depth`].
    ///
    /// A value of its own rather than the open panel's, because it is the
    /// *screen's* posture and not any one panel's: two panels hand over to each
    /// other in the middle of it — a menu folding away as the answer it raised
    /// comes out of the row that was pressed — and a push that read whichever
    /// of the two was further out would let the whole screen drift forward
    /// through the crossing and then go back again. Held here it simply never
    /// moves until the last of them has gone, and, being a position rather than
    /// a state, it reverses from wherever it is when a panel is dismissed
    /// before it finished arriving.
    depth_linear: f32,
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

/// A window flying back out of the tile that asked for it.
///
/// The compositor draws the flight; this is only what the shell needs to know
/// while it lasts — which display it is on, so that display keeps drawing, and
/// when it started, so the bar can step back down as the window lands.
struct Restore {
    panel: usize,
    /// The tile it is growing out of, so the start screen can lean towards it
    /// the way it does when an application is started.
    from: [f32; 4],
    started: Instant,
}

/// One window in the overview, as announced by the compositor.
#[derive(Debug, Clone, PartialEq, Eq)]
struct WindowCard {
    id: u32,
    title: String,
    /// What the application behind the window calls itself, empty when it says
    /// nothing or when the compositor is too old to tell us. This is what a
    /// tile is matched against before it starts anything.
    app_id: String,
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
    /// Displays `--debug-hdr-displays` invented, standing in for what the
    /// compositor would have reported. `None` in every real session.
    debug_hdr_displays: Option<Vec<(String, settings::Support)>>,
    /// The same for `--debug-display-modes` and the mode lists.
    debug_display_modes: Option<Vec<(String, Vec<settings::Offered>)>>,
    /// LineXinBar's session protocol, when running under LineXinBar.
    shell_control: Option<LxbShellV1>,
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
    /// The context menu, raised over whichever surface asked for it — the bar's
    /// selected tile, a window card in the guide, or whatever comes next. One
    /// for the session, because it takes every button while it is up and a
    /// second would be a second answer to who has the keys.
    context_menu: menu::Menu,
    /// The centred panel a menu row can raise: what the shell knows about an
    /// application, or a question about removing it. Separate from the menu
    /// rather than replacing it, because the menu it came out of is still
    /// folding back into its own anchor while this one grows.
    dialog: dialog::Dialog,
    /// The application the open panel is describing and the package manager
    /// being asked about it, if any. Started on the press and read once a frame
    /// until it answers — see [`appinfo`] for why it cannot be asked on this
    /// thread. The application is carried along rather than looked up again so
    /// that the answer lands in a panel about the thing that was asked about.
    app_facts: Option<(apps::App, appinfo::Lookup)>,
    /// A screenshot the compositor is taking: the menu row it was asked for on,
    /// and the name of the application it is of.
    ///
    /// Held because the answer arrives afterwards and the panel that reports it
    /// has to grow out of the control that was pressed — which by then is a row
    /// of a menu that is folding away, so there is nothing left to ask where it
    /// was. One at a time: the menu closes on the press, so there is no second
    /// row to press until this one has been answered.
    pending_capture: Option<([f32; 4], String)>,
    /// What removing the application the uninstall question is about would
    /// take, asked the moment that question goes up so the answer is there by
    /// the time it is answered. Taken by the Yes button; dropped by the No one.
    removal_plan: Option<(apps::App, uninstall::Survey)>,
    /// A removal the user has agreed to, and how far it has got.
    uninstalling: Option<Uninstall>,
    /// The volume and brightness bars in the guide's sidebar, and the worker
    /// that keeps them true.
    quick: Quick,
    /// The on-screen keyboard, and the two Wayland objects behind it: the
    /// input method that says when a text field wants one, and the virtual
    /// keyboard that types.
    osk: keyboard::Osk,
    /// Whether a frame of the open menu has been committed yet. The overview
    /// waits for it, so the cards never fly under a stale bar.
    menu_frame_drawn: bool,
    /// When the compositor was told to start flying windows into their cards.
    overview_started_at: Option<Instant>,
    /// The application the shell has started and is waiting for, if any: what
    /// answers the press while the process gets itself on screen.
    launching: Option<launch::Launch>,
    /// A window flying back out of its tile, drawn by the compositor.
    restoring: Option<Restore>,
    /// Eased card rectangles by window id (`u64::MAX` is the start card),
    /// advanced every frame on the compositor's own spring so the frames the
    /// shell draws travel with the windows the compositor is easing.
    guide_card_rects: std::collections::HashMap<u64, Glide>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    /// The seat's pointer, taken for one thing only: the on-screen keyboard.
    ///
    /// The rest of the shell is deliberately not clickable — it is a console
    /// bar driven from a controller, and every one of its surfaces covers a
    /// whole display, so a shell that accepted clicks anywhere would swallow
    /// them from the application it is sitting in front of. The board is the
    /// exception because it is a picture of a keyboard: keys are for pressing,
    /// and anyone with a mouse plugged in will try.
    pointer: Option<wl_pointer::WlPointer>,
    /// Says what the cursor should look like over our surfaces, when the
    /// compositor supports being told. Without it the pointer keeps whatever
    /// shape the application under the board last asked for, which over a
    /// keyboard is usually a text beam.
    cursor_shape: Option<CursorShapeManager>,
    /// Which of our surfaces holds keyboard focus, if any. Only one can, so
    /// this doubles as "is the shell being driven right now".
    focused_surface: Option<wl_surface::WlSurface>,
    keep_keyboard_grabbed: bool,
    controller: ControllerInput,
    /// The right stick's pointer and the left one's scrolling: the curves that
    /// turn deflection into movement, the per-application answers to whether
    /// they are turned on at all, and the mouse buttons currently held down.
    stick: Stick,
    scroll: Stick,
    prefs: Prefs,
    /// Buttons the compositor has been told are down. Held so that they can be
    /// let go of when the pointer stops being driven — a button reported
    /// pressed and never released leaves the application holding a drag that
    /// nothing on the controller can end.
    stick_buttons: Vec<u32>,
    /// Arrow keys the compositor has been told are down, for the same reason
    /// and with a sharper edge to it: a held key repeats in the application,
    /// so one never released repeats forever.
    stick_keys: Vec<u32>,

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
            Some("lxb-desktop-backdrop"),
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
            Some("lxb-desktop"),
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
            cursor: Cursor::for_model(&self.xmb),
            foreground: None,
            app_id: None,
            hdr: settings::Support::default(),
            applied_hdr: None,
            modes: Vec::new(),
            pending_modes: Vec::new(),
            applied_mode: None,
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
            depth_linear: 0.0,
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
        self.sync_setting_preview();
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

        // One palette clock for the whole shell: the wallpaper, glass and UI
        // on every display must read the same point of the same transition.
        // A visible backdrop already requests the next frame continuously;
        // hidden fullscreen-covered outputs remain idle as before.
        theme::animate(dt);

        // The outer loop syncs surface state before it asks us to draw. A
        // launch can finish only here, after the handover fade reaches zero;
        // resync immediately so the normal bar's final frame is below the
        // application rather than still on the splash's overlay layer.
        if self.advance_launch(now) {
            self.sync_surface_state();
        }
        // And the same for a window that has finished flying back: the bar
        // only leaves once the application covers it.
        if self.advance_restore(now) {
            self.sync_surface_state();
        }

        // Every display eases towards its own selection, including the ones
        // nobody is driving: they were left mid-glide when control moved away.
        let mut cursor_moving = vec![false; self.panels.len()];
        for (moving, panel) in cursor_moving.iter_mut().zip(&mut self.panels) {
            *moving = panel.cursor.animate(dt);
        }

        // Resolved before the GPU and panels are borrowed, since they read
        // `self` while it is mutably held below.
        let show_menu = self.guide.is_menu();
        // A switch going over outlives the keystroke that threw it, so the
        // frames have to keep coming until it has settled. The context menu's
        // growth is the same kind of thing: a menu dismissed with one press
        // still has to be watched falling back into the control it came from.
        let pressing =
            self.guide.pressing() || self.context_menu.is_animating() || self.dialog.is_animating();
        let keyboard_visible = self.keyboard_visible();
        let keyboard_overlay = self.keyboard_overlay();
        // Whether the board on screen is typing into a panel this surface is
        // drawing rather than into whatever is in front of it.
        let board_types_here = self.osk.types_here();
        let focused_panel = self.focused_panel;
        let app_label = self.app_label().map(str::to_string);
        let screen_label = self.screen_label();
        let time = self.start.elapsed().as_secs_f32();
        let clock = wall_clock();
        let clock_face = wall_clock_face();
        // The cards keep the compositor's time, not the menu's.
        let card_age = self.card_age();

        // Brightness belongs to a screen, so the worker is told which one is
        // in front of the user whether or not the menu is open — a bar that
        // only started asking when the sidebar appeared would arrive a few
        // hundred milliseconds of i2c later than the sidebar did. Only what is
        // on screen is kept refreshed.
        self.quick.watch(
            self.panels
                .get(focused_panel)
                .map(|panel| panel.name.as_str()),
            show_menu,
        );
        let volume = self.quick.level(Knob::Volume);
        let brightness = self.quick.level(Knob::Brightness);
        // What the machine is playing changes without anybody pressing
        // anything, so the open panel is brought up to date once a frame — an
        // application that has started making a noise appears in it, and one
        // that has stopped leaves.
        self.sync_mixer();
        // Which rows the column has. Set from the same answer the bars are
        // drawn from, so a control that goes away cannot leave a row behind.
        self.guide.set_bars(guide::Bars {
            volume: volume.is_some(),
            brightness: brightness.is_some(),
        });
        // And whether the pointer tile has anything behind it, on the same
        // terms: only LineXinBar can move a pointer for a client.
        self.guide.set_pointer_control(self.pointer_control());
        // And whether there is an application for the tiles to be about, which
        // is what decides whether the highlight will stop on one.
        self.guide.set_pointer_target(self.pointer_app().is_some());
        let stick_pointer = self.stick_pointer_on();
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
                let slots = lxb_protocol::overview::card_slots(
                    panel.width as f64,
                    panel.height as f64,
                    cards.len(),
                    selected,
                );
                self.guide_card_rects.retain(|key, _| keys.contains(key));
                for ((card, key), slot) in cards.iter_mut().zip(&keys).zip(&slots) {
                    let fitted =
                        lxb_protocol::overview::fit(slot, card.width as f64, card.height as f64);
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
            // What the Close entry would act on, named as an application
            // rather than as a window: the button ends Firefox, not the page
            // Firefox happens to be showing. Taken from the same answer the
            // press itself uses, so the label can never name one thing while
            // the button kills another — and `None` there is the start screen,
            // which has no process to end and so gets no entry at all.
            let close_target = self
                .close_target()
                .map(|window| self.window_app_name(window));
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
                    &items,
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
        // The board's rise and fall, on the same terms and for the same
        // reason: it is drawn on the display being driven, and the others must
        // not run its clock on.
        let board_arrived = ui::ease(self.osk.animate(dt));
        // And the context menu's, which is on the display it was raised over.
        // Its highlight is eased here too, for the same reason the guide's is:
        // the rectangle it glides towards is the one the drawing will use.
        let context_open = ui::ease(self.context_menu.animate(dt));
        let context_highlight = self
            .context_menu
            .is_on_screen()
            .then(|| {
                let (width, height) = self
                    .panels
                    .get(focused_panel)
                    .map(|panel| (panel.width as f32, panel.height as f32))?;
                // How much room there is can change under an open menu — a
                // display put into another mode, or a rotated one — so the
                // window is kept true every frame rather than only at the
                // moment the menu was raised.
                self.context_menu
                    .set_window(ui::menu_rows_that_fit(&self.context_menu, height));
                let row = ui::context_menu_row_rect(
                    width,
                    height,
                    &self.context_menu,
                    self.context_menu.selected(),
                )?;
                Some(self.context_menu.animate_highlight(row, dt))
            })
            .flatten();
        let context_on_screen = self.context_menu.is_on_screen();

        // And the centred panel's, on exactly the same terms — it is the same
        // machinery underneath, growing out of the menu row that opened it.
        //
        // How much of the display the keyboard has taken goes in first. The
        // board *rises*, so this changes every frame while it does, and the
        // panel lifting with it is what makes the two read as one movement
        // rather than as a panel that jumped out of the way.
        let board_room = self
            .panels
            .get(focused_panel)
            .map(|panel| {
                let (width, height) = (panel.width as f32, panel.height as f32);
                let [_, top, _, _] = ui::keyboard_panel_rect(width, height);
                (height - top) * board_arrived
            })
            .unwrap_or_default();
        self.dialog.set_footer(if self.osk.types_here() {
            board_room
        } else {
            0.0
        });
        let dialog_open = ui::ease(self.dialog.animate(dt));
        let dialog_highlight = self
            .dialog
            .is_on_screen()
            .then(|| {
                let (width, height) = self
                    .panels
                    .get(focused_panel)
                    .map(|panel| (panel.width as f32, panel.height as f32))?;
                let button = ui::dialog_button_rect(
                    width,
                    height,
                    &self.dialog,
                    self.dialog.buttons.selected(),
                )?;
                Some(self.dialog.buttons.animate_highlight(button, dt))
            })
            .flatten();
        let dialog_on_screen = self.dialog.is_on_screen();

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

            // And the start screen's step back from whatever the shell has
            // raised over it, on the panels' own 200ms so that the screen goes
            // back over exactly the time one takes to arrive.
            //
            // One target for however many are stacked up: while any of them is
            // still on screen the screen stays where it is, and it only comes
            // home behind the last one to leave. Only where the bar is the
            // thing being stood over — with the guide up it is a miniature in a
            // frame of the guide's own, and a card that shrank out of its frame
            // would read as a mistake rather than as depth.
            let panel_over_bar = focused && !menu_here && (context_on_screen || dialog_on_screen);
            let depth_target = if panel_over_bar { 1.0 } else { 0.0 };
            panel.depth_linear = approach(panel.depth_linear, depth_target, dt / menu::FLIGHT);

            // Being hidden is not a reason to stop mid-animation: what stays
            // committed is what a translucent application in front shows, and
            // what the display goes back to if it is uncovered. Settle first,
            // then go quiet.
            let settling = panel.home_linear != home_target
                || panel.blur_linear != home_target
                || panel.depth_linear != depth_target
                || cursor_moving[index]
                || (pressing && index == focused_panel);
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

            // Whether the only reason this display is drawing at all is the
            // keyboard, or the hint standing in for it.
            //
            // The bar is not drawn then, and this is the whole of why: the
            // surface it shares with the board has been raised above the
            // application to carry the board, and the bar belongs *behind*
            // that application. Drawing it anyway put the start screen — every
            // icon of it — over the running application alongside the
            // keyboard, which is not an overlay anybody asked for. The bar
            // gives the screen up for as long as the board is on it.
            let board_only =
                draws_only_the_keyboard(focused, keyboard_overlay, menu_here, board_types_here);

            if let Some(backdrop_target) = panel.backdrop_target.as_mut() {
                // Nothing of the backdrop shows past an application either, so
                // a board on top of one costs no wallpaper.
                if !board_only {
                    let params = gpu::Backdrop {
                        blur: backdrop_blur,
                        ..Default::default()
                    };
                    if let Err(err) = gpu.render(backdrop_target, &[], &[], time, Some(params)) {
                        tracing::warn!(?err, "backdrop render failed");
                    }
                }
            }

            // The bar, wherever the start screen currently is: the whole
            // display, its card, or somewhere between the two mid-flight.
            let mut scene = if board_only {
                ui::Scene::default()
            } else {
                ui::build(
                    &self.xmb,
                    &panel.cursor,
                    width as f32,
                    height as f32,
                    focused,
                    clock.as_deref(),
                    time,
                    &Slots(gpu),
                )
            };
            // The start screen steps back from whatever the shell has raised
            // over it — the context menu, and the centred panel one of its
            // commands opens — before it is dimmed and before it is shrunk
            // into the guide's card, both of which are about something else.
            ui::recede_into_depth(
                &mut scene,
                width as f32,
                height as f32,
                ui::ease(panel.depth_linear),
            );
            if home > 0.0 {
                scene.place_into(bar_rect, width as f32, height as f32);
                if menu_here && panel.home_fades_in {
                    scene.fade(ui::card_fade(card_age));
                }
            }
            if menu_here {
                // The guide is drawn over it, so anything of the bar still
                // crossing the sidebar has to give way to the panel.
                let sidebar = lxb_protocol::overview::sidebar_width(width as f64) as f32;
                scene.fade_text_before(sidebar, sidebar * 0.5);
                // The bar is a separate scene, so the dialog's panel — a quad,
                // and every quad is drawn under every text run — cannot cover
                // the start card's labels. They have to step back themselves.
                if power_open > 0.0 {
                    ui::recede_behind_power_dialog(
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
                        clock: clock_face
                            .as_ref()
                            .map(|(time, date)| ui::Clock { time, date }),
                        volume,
                        brightness,
                        stick_pointer,
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
                        slots: &Slots(gpu),
                    },
                    width as f32,
                    height as f32,
                );
                scene.quads.extend(guide.quads);
                scene.texts.extend(guide.texts);
            }

            // The context menu, over whatever raised it. Drawn on the eased
            // position rather than on whether it is open: a menu that has been
            // answered is still folding back into the control it came out of,
            // and the row it was answered with is still going down.
            if focused && context_on_screen {
                ui::recede_behind_context_menu(
                    &mut scene,
                    width as f32,
                    height as f32,
                    &self.context_menu,
                    context_open,
                );
                let over = ui::build_context_menu(
                    ui::ContextMenuView {
                        menu: &self.context_menu,
                        highlight: context_highlight,
                        open: context_open,
                        behind: backdrop_blur,
                        time,
                        slots: &Slots(gpu),
                    },
                    width as f32,
                    height as f32,
                );
                scene.quads.extend(over.quads);
                scene.texts.extend(over.texts);
            }

            // The centred panel, over the menu that raised it — which is still
            // on screen, folding back into its own anchor, and whose rows would
            // otherwise print straight through this one.
            if focused && dialog_on_screen {
                ui::recede_behind_dialog(
                    &mut scene,
                    width as f32,
                    height as f32,
                    &self.dialog,
                    dialog_open,
                );
                let over = ui::build_dialog(
                    ui::DialogView {
                        dialog: &self.dialog,
                        highlight: dialog_highlight,
                        open: dialog_open,
                        behind: backdrop_blur,
                        time,
                        slots: &Slots(gpu),
                    },
                    width as f32,
                    height as f32,
                );
                scene.quads.extend(over.quads);
                scene.texts.extend(over.texts);
            }

            // The on-screen keyboard, and the corner hint that stands in for
            // it. Only on the display being driven, and on the same answer
            // `sync_surface_state` raised this surface on, so the board can
            // never be drawn on a display still sitting behind its
            // application.
            if focused && keyboard_visible {
                // `is_on_screen`, not `is_open`: a dismissed board is still on
                // its way down, and the hint that replaces it has to wait for
                // the display's edge to take it rather than appearing through
                // it.
                if self.osk.is_on_screen() {
                    let board = ui::build_keyboard(
                        ui::KeyboardView {
                            board: &self.osk.board,
                            slots: &Slots(gpu),
                            arrived: board_arrived,
                            behind: backdrop_blur,
                            time,
                        },
                        width as f32,
                        height as f32,
                    );
                    // A scene is all its quads and then all its text, so the
                    // board's panel cannot cover a label the bar already put
                    // down. Those labels have to go.
                    scene.hide_text_behind(ui::keyboard_panel_rect(width as f32, height as f32));
                    scene.quads.extend(board.quads);
                    scene.texts.extend(board.texts);
                } else {
                    let hint_rect = ui::keyboard_hint_rect(width as f32, height as f32);
                    let hint = ui::build_keyboard_hint(
                        ui::HintView {
                            slots: &Slots(gpu),
                            fade: 1.0,
                            behind: backdrop_blur,
                        },
                        width as f32,
                        height as f32,
                    );
                    scene.hide_text_behind(hint_rect);
                    scene.quads.extend(hint.quads);
                    scene.texts.extend(hint.texts);
                }
            }

            // A window flying back out of its tile. Nothing is drawn for it —
            // the compositor is drawing the window itself, in front of this
            // surface — so all the start screen does is lean towards the tile
            // the application is growing out of, exactly as it does for one
            // being started. It does not fade: it is still the thing behind
            // the window for the whole flight, and a bar fading to nothing
            // would leave a hole around a window that has not arrived yet.
            if let Some(restore) = self.restoring.as_ref().filter(|r| r.panel == index) {
                let flown = now.duration_since(restore.started).as_secs_f32()
                    / lxb_protocol::overview::FLIGHT.as_secs_f32();
                let grown = ui::ease(flown.clamp(0.0, 1.0));
                let zoom = 1.0 + LAUNCH_PUSH * grown;
                let [cx, cy] = [
                    restore.from[0] + restore.from[2] * 0.5,
                    restore.from[1] + restore.from[3] * 0.5,
                ];
                scene.scale_by(zoom, [cx - cx * zoom, cy - cy * zoom]);
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
                        ui::card_fade(card_age)
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
        // Being driven is not the same as holding the keyboard, and the
        // on-screen keyboard is the case that separates them: it is up, the
        // user is picking out letters on it with the stick, and it has
        // deliberately left the Wayland keyboard with the application it is
        // typing into. Reading focus alone here threw away every direction and
        // every press the moment the board appeared.
        let active = controller_is_driving(
            self.keep_keyboard_grabbed,
            self.focused_surface.is_some(),
            self.osk.is_open(),
        );
        let poll = self.controller.poll(now.duration_since(self.start), active);
        // The pointer first. An action can close the menu or start an
        // application, either of which changes whether the stick should be
        // driving anything, and it should be answered on the state the poll
        // was actually taken in.
        self.drive_pointer(&poll, now);
        let pressed_something = controller_pressed_something(&poll);
        for action in poll.actions {
            self.on_action(action);
        }
        // And asked afterwards, on the state the presses left behind: the
        // guide button is a controller press *and* the thing that stops the
        // stick aiming, so asking first would answer for the screen the user
        // has just left.
        if pressed_something {
            self.put_the_pointer_down();
        }
    }

    /// Tell the compositor the user has picked the controller up, so the
    /// cursor can go away.
    ///
    /// Silent while the right stick is aiming, and that is the whole of the
    /// exception: with the stick pointer on, the pad *is* the mouse — `A` is
    /// its left button and the D-pad is its wheel — and a cursor that blinked
    /// out at every click would be one the user could not use.
    fn put_the_pointer_down(&mut self) {
        if self.stick_pointer_aiming() {
            return;
        }
        let Some(control) = self.shell_control.as_ref() else {
            return;
        };
        if control.version() < HIDE_POINTER_VERSION {
            return;
        }
        control.hide_pointer();
    }

    /// Move the pointer with the right stick, where that is turned on.
    ///
    /// Only ever *inside* an application: the shell's own screens are driven
    /// with the stick that navigates them and have no pointer to speak of, and
    /// the menu is drawn over the very application the pointer would be
    /// travelling across. So the stick goes back to being the stick the moment
    /// anything of the shell's is on screen, and the pointer is left exactly
    /// where the user parked it.
    fn drive_pointer(&mut self, poll: &controller::Poll, now: Instant) {
        let Some(control) = self.shell_control.clone() else {
            return;
        };
        if control.version() < POINTER_SHELL_VERSION {
            return;
        }

        if !self.stick_pointer_aiming() {
            // Whatever the sticks are doing now, they are not moving a
            // pointer: forget the interval so coming back does not integrate
            // the gap, and let go of anything still held down.
            self.stick.rest();
            self.scroll.rest();
            self.release_stick_buttons(&control);
            self.release_stick_keys(&control);
            return;
        }

        let at = now.duration_since(self.start).as_secs_f32();
        let mut sent = false;
        let (x, y) = poll.right_stick;
        if let Some((dx, dy)) = self.stick.motion(x, y, at) {
            control.move_pointer(dx, dy);
            sent = true;
        }

        // The buttons and the wheel are the on-screen keyboard's the moment it
        // is up: it is driven with the D-pad, the left stick and `A`, and two
        // things bound to one control is worse than one of them being absent.
        // Aiming carries on regardless — the board has no use for the right
        // stick — so the pointer is still where it was left when the board
        // goes away.
        if !self.stick_pointer_clicking() {
            self.scroll.rest();
            self.release_stick_buttons(&control);
            self.release_stick_keys(&control);
            if sent {
                let _ = self.conn.flush();
            }
            return;
        }

        // The left stick is the wheel. It is free to be: the shell has already
        // stopped navigating with it by the time an application is in front.
        let (sx, sy) = poll.scroll_stick;
        if let Some((dx, dy)) = self.scroll.motion(sx, sy, at) {
            control.scroll_pointer(dx, dy);
            sent = true;
        }

        // And the D-pad beside it is the arrows, which is what a D-pad is.
        // Scrolling and arrowing are not the same job: a wheel moves a view
        // and leaves the caret where it was, while the arrows move the caret,
        // step through a list and open a menu — and a console has one control
        // shaped like the arrow keys.
        if self.send_stick_keys(&control, &poll.arrows) {
            sent = true;
        }

        for (button, down) in &poll.clicks {
            if *down {
                if !self.stick_buttons.contains(button) {
                    self.stick_buttons.push(*button);
                }
            } else if let Some(index) = self.stick_buttons.iter().position(|held| held == button) {
                self.stick_buttons.remove(index);
            } else {
                // A release for a press made while the pointer was not being
                // driven — turning the switch on mid-click. The application
                // never saw the press, so it must not see the release.
                continue;
            }
            control.pointer_button(
                *button,
                if *down {
                    lxb_shell_v1::ButtonState::Pressed
                } else {
                    lxb_shell_v1::ButtonState::Released
                },
            );
            sent = true;
        }

        if sent {
            if let Err(err) = self.conn.flush() {
                tracing::warn!(?err, "could not send the stick pointer's movement");
            }
        }
    }

    /// Let go of every button the stick is holding down.
    fn release_stick_buttons(&mut self, control: &LxbShellV1) {
        if self.stick_buttons.is_empty() {
            return;
        }
        for button in std::mem::take(&mut self.stick_buttons) {
            control.pointer_button(button, lxb_shell_v1::ButtonState::Released);
        }
        let _ = self.conn.flush();
    }

    /// Forward the D-pad's edges as arrow keys, and remember what is down.
    ///
    /// Returns whether anything was sent. A release for a direction the
    /// application never saw pressed is dropped, exactly as a click's is: the
    /// switch can be turned on with a thumb already on the D-pad.
    fn send_stick_keys(&mut self, control: &LxbShellV1, arrows: &[(u32, bool)]) -> bool {
        // A compositor one version behind still points and clicks; it simply
        // has no way to be handed a key, so nothing is pressed and there is
        // then nothing for the release to let go of either.
        if control.version() < KEYBOARD_KEY_VERSION {
            return false;
        }
        let mut sent = false;
        for (key, down) in arrows {
            if *down {
                if self.stick_keys.contains(key) {
                    continue;
                }
                self.stick_keys.push(*key);
            } else if let Some(index) = self.stick_keys.iter().position(|held| held == key) {
                self.stick_keys.remove(index);
            } else {
                continue;
            }
            control.keyboard_key(
                *key,
                if *down {
                    lxb_shell_v1::KeyState::Pressed
                } else {
                    lxb_shell_v1::KeyState::Released
                },
            );
            sent = true;
        }
        sent
    }

    /// Let go of every arrow the D-pad is holding down.
    ///
    /// Needed for a stronger reason than the buttons are: a key the client is
    /// repeating and never hears the release of goes on repeating, so a menu
    /// closed with a direction held would leave the application scrolling by
    /// itself for as long as it was open.
    fn release_stick_keys(&mut self, control: &LxbShellV1) {
        if self.stick_keys.is_empty() {
            return;
        }
        for key in std::mem::take(&mut self.stick_keys) {
            control.keyboard_key(key, lxb_shell_v1::KeyState::Released);
        }
        let _ = self.conn.flush();
    }

    /// The application the pointer switch is about: the one in front on the
    /// display being driven, named the way it names itself.
    ///
    /// `None` when nothing is running there, or when the application told
    /// nobody what it is — a setting cannot be filed under nothing.
    fn pointer_app(&self) -> Option<&str> {
        self.panels
            .get(self.focused_panel)?
            .app_id
            .as_deref()
            .filter(|app_id| !app_id.is_empty())
    }

    /// Whether this session can move a pointer at all.
    fn pointer_control(&self) -> bool {
        self.shell_control
            .as_ref()
            .is_some_and(|control| control.version() >= POINTER_SHELL_VERSION)
    }

    /// Whether the switch is on for the application in front — as a setting,
    /// which is what the menu draws. Whether it is *being acted on* is
    /// [`Self::stick_pointer_aiming`], and differs while the menu is open,
    /// which is the only time the two are ever both asked.
    fn stick_pointer_on(&self) -> bool {
        self.pointer_app()
            .is_some_and(|app| self.prefs.stick_pointer(app))
    }

    /// Whether the right stick should be aiming the pointer right now.
    ///
    /// Both halves of the question: the user turned it on for this
    /// application, and the application is the thing being pointed *at* —
    /// not the menu over it, and not a launch still in flight, both of which
    /// are the shell's own screens with nothing on them to click.
    ///
    /// The on-screen keyboard is not in that list. It is drawn over the
    /// application, but it is driven with the D-pad, the left stick and `A`,
    /// and it has no use at all for the right stick — so aiming carries on
    /// underneath it, and the pointer is still where it was left when the
    /// board goes away. What the board does take is [everything
    /// else](Self::stick_pointer_clicking).
    fn stick_pointer_aiming(&self) -> bool {
        pointer_aims(
            self.guide.is_over_app(),
            self.launching.is_some(),
            self.stick_pointer_on(),
        )
    }

    /// Whether the buttons and the wheel are the pointer's as well.
    ///
    /// Everything aiming needs, and the board out of the way: `A` presses the
    /// key under its cursor and the D-pad moves that cursor, so a pointer that
    /// also claimed them would make one press do two things.
    fn stick_pointer_clicking(&self) -> bool {
        pointer_clicks(self.stick_pointer_aiming(), self.osk.is_open())
    }

    fn on_key(&mut self, keysym: Keysym) {
        // A password field takes the whole keyboard while it is up, and takes
        // it first. Everything below this line turns keys into *actions* — Q
        // would be Back, Escape would leave the panel — and a password with a Q
        // in it is a password the user cannot type.
        if self.password_wanted() {
            // Except the guide, which outranks everything the shell draws and
            // every grab an application can take. A modal that could swallow it
            // would be the one screen in the shell with no way out of it, which
            // is exactly the failure that rule exists to prevent.
            if action_for_keysym(keysym) == Some(Action::Guide) {
                self.on_action(Action::Guide);
                return;
            }
            if let Some(stroke) = keyboard::stroke_for(keysym) {
                if self.type_into_password(stroke) {
                    return;
                }
            }
            // A key the board's keymap has no character for — a bare modifier,
            // a volume key — is not this field's and is not an action either
            // while the field is up.
            return;
        }
        if let Some(action) = action_for_keysym(keysym) {
            self.on_action(action);
        }
    }

    /// Whether the panel on screen is waiting for a password to be typed into
    /// it.
    fn password_wanted(&self) -> bool {
        self.uninstalling
            .as_ref()
            .is_some_and(|state| matches!(state.stage, Stage::Asking { .. }))
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

    /// What to call the application a window belongs to, for a button that
    /// acts on the whole of it.
    ///
    /// Three answers, best first: what the desktop entry that installed it
    /// calls the application, what the window itself claims to be, and — only
    /// when it claims nothing at all — the window's title, which is then the
    /// only name left. A title is the last resort rather than the first
    /// because it describes the *contents* of a window: closing a browser is
    /// not closing the video that is playing in it.
    fn window_app_name(&self, window: &WindowCard) -> String {
        if let Some(app) = self.xmb.app_for_window(&window.app_id) {
            return app.name.clone();
        }
        app_id_name(&window.app_id).unwrap_or_else(|| window.title.clone())
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
        self.handle_action(action);
        self.sync_setting_preview();
    }

    /// Reconcile the temporary appearance with the row that is actually
    /// highlighted after an action. Leaving the values column by Back or Left
    /// therefore flows back to the applied accent; changing screens follows
    /// the cursor on the screen that now owns control.
    fn sync_setting_preview(&mut self) {
        let setting = if self.guide.is_menu() || self.osk.is_open() {
            None
        } else {
            self.panels
                .get(self.focused_panel)
                .and_then(|panel| panel.cursor.current_setting(&self.xmb))
        };
        settings::preview(setting);
    }

    /// Carry out an action before [`Self::sync_setting_preview`] observes its
    /// result. Keeping the reconciliation outside this function means even an
    /// early return after entering or applying a value cannot skip it.
    fn handle_action(&mut self, action: Action) {
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
                // The menu takes the keyboard outright, so a board left up
                // under it could not type: it would be sending its letters to
                // the shell's own overlay. Without the slide down, because the
                // menu is not this screen with the board taken off it — it is
                // another screen, drawn over the whole of the space the board
                // would have been travelling through.
                self.osk.dismiss_at_once();
                // A context menu is about something on the screen the guide is
                // replacing, so it cannot outlive the press either — nor can
                // the panel it raised.
                self.context_menu.close();
                self.close_dialog();
                if self.guide.toggle() {
                    self.begin_home_flight(bar_on_top);
                }
                self.needs_redraw = true;
            }
            Action::Keyboard => {
                self.context_menu.close();
                self.close_dialog();
                self.toggle_keyboard();
            }
            Action::Back => self.on_back(),
            // Ahead of the menu, not behind it: the guide is where the user is
            // told the shoulder buttons move between displays, so that is the
            // last place they may stop working. The menu travels with them.
            //
            // The context menu does not: it is pinned to a control on one
            // display, and there is nothing for it to be about on the next.
            Action::PrevScreen => {
                self.context_menu.close();
                self.close_dialog();
                self.focus_screen(-1);
            }
            Action::NextScreen => {
                self.context_menu.close();
                self.close_dialog();
                self.focus_screen(1);
            }
            // The board is the innermost thing on screen while it is up, and
            // it takes every direction: nothing behind it should move under a
            // cursor that is picking out letters. The context menu is the
            // innermost thing the *shell* draws, so it comes first of the rest.
            _ if self.osk.is_open() => self.on_keyboard_action(action),
            // Ahead of `Menu`, unlike the context menu itself: a modal panel
            // has taken the screen, and the button that raises a menu about
            // something on that screen has nothing left to be about.
            _ if self.dialog.is_open() => self.on_dialog_action(action),
            Action::Menu => self.toggle_context_menu(),
            _ if self.context_menu.is_open() => self.on_context_menu_action(action),
            _ if self.guide.is_menu() => self.on_menu_action(action),
            Action::Launch => {
                // A subcategory is opened rather than launched. Accept is the
                // way *in* on the original bar as much as Right is, and a row
                // that swallowed the button because it starts no process would
                // be the one row in the shell that nothing opens.
                if let Some(panel) = self.panels.get_mut(self.focused_panel) {
                    if panel.cursor.enter(&self.xmb) {
                        self.needs_redraw = true;
                        return;
                    }
                }
                // A value is chosen rather than launched. Nothing else has to
                // be told: the mark moves in the catalogue every display is
                // drawn from, and the palette is read afresh by every colour
                // in the next frame — so both screens of a two-screen session
                // change together, without either being handed anything.
                let chosen = match self.panels.get(self.focused_panel) {
                    // Disjoint fields: this display's cursor reads the shared
                    // catalogue while the catalogue itself is written.
                    Some(panel) => panel.cursor.choose(&mut self.xmb),
                    None => None,
                };
                if let Some(setting) = chosen {
                    settings::apply(setting);
                    // The accent needs nobody told; a display setting does,
                    // and straight away rather than on the next loop pass —
                    // the user has just pressed a button and is waiting to see
                    // whether the screen changed.
                    self.sync_hdr();
                    self.needs_redraw = true;
                    return;
                }
                self.start_selection();
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
        // A removal that is under way is the one thing on this surface Back
        // cannot take away. It is a few seconds of disk that cannot be undone
        // half done, and the panel over it is the only place the user will be
        // told how it went. The guide still opens over it, which is the
        // shell's standing promise about being able to get out of anything.
        if self.removal_running() {
            return;
        }
        // One layer at a time, from the top down. A panel raised by a menu row
        // is in front of the menu, so Back from it lands back on the screen the
        // menu was about — the menu itself has already folded away, which is
        // what makes this one step rather than two.
        if self.close_dialog() {
            self.needs_redraw = true;
            return;
        }
        // The context menu is drawn over whatever raised it, so Back from it is
        // back to that — not out of both, and not out of the guide underneath
        // it.
        if self.context_menu.close() {
            self.needs_redraw = true;
            return;
        }
        // The keyboard is in front of everything else the shell draws, so it
        // is the first thing Back takes away — and the field it was typing
        // into is still there afterwards, which is what the corner hint is
        // then for. A board that came up for the shell's own password field is
        // the exception: taking it away leaves the field with nothing to fill
        // it in, so Back means "leave this panel" and is handled above.
        if !self.osk.types_here() && self.osk.close() {
            self.sync_surface_state();
            self.needs_redraw = true;
            return;
        }
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
            // On the bar, Back is a step out of the path the user has walked
            // into a category before it is anything else. One level at a time
            // and the same way round as everywhere else in the shell: the menu
            // is reached from the top of a column, not from three levels down
            // inside one.
            Mode::Bar | Mode::BarOverApp => {
                let left = self
                    .panels
                    .get_mut(self.focused_panel)
                    .is_some_and(|panel| panel.cursor.leave());
                if left {
                    self.needs_redraw = true;
                } else {
                    self.open_guide();
                }
            }
        }
    }

    /// Show or hide the on-screen keyboard.
    ///
    /// One condition, and it is about whether the letters can leave the shell
    /// at all: there has to be a virtual keyboard to send them through.
    /// Nothing else is asked. The board is deliberately summonable over an
    /// application that never announced a text field — an X11 client, a
    /// browser built without Wayland IME — and that is a case the shell has no
    /// way to tell apart from an application with nothing to type into, so it
    /// does not try. Over the bar with nothing running there is nowhere for
    /// the letters to go; the board still opens, because a shortcut that
    /// silently does nothing on some screens is worse than one that visibly
    /// does nothing.
    fn toggle_keyboard(&mut self) {
        if self.osk.close() {
            self.sync_surface_state();
            self.needs_redraw = true;
            return;
        }
        if !self.osk.can_type() {
            tracing::debug!("no virtual keyboard on this compositor; nothing to type with");
            return;
        }
        // Step out of the way first, for the same reason the guide closes the
        // board: whichever of the two is in front has the keys.
        self.guide.close();
        self.osk.open();
        self.sync_surface_state();
        self.needs_redraw = true;
    }

    /// Start the application the bar's cursor is on.
    ///
    /// Factored out of the accept button because the context menu's Launch row
    /// is the same command reached another way, and a second copy of this is a
    /// second answer to the one question that matters here: what happens when
    /// the application is *already* running.
    fn start_selection(&mut self) {
        // It is come back to, never started a second time. A console has one of
        // each thing running, and a tile that silently produced a second copy
        // would also produce two cards to close, of which closing either could
        // take both — the application is what Close ends, not the window.
        if let Some((display, window)) = self.window_for_selection() {
            self.restore_window(display, window);
            return;
        }
        // Say where this is being launched from before starting it, so the
        // answer cannot be overtaken by the window itself.
        self.sync_launch_output();
        let Some(panel) = self.panels.get(self.focused_panel) else {
            return;
        };
        // Everything the splash needs, read before the launch: the tile it
        // opens out of, and what was already on the display, so the
        // application's own window can be told from them.
        let opening = panel
            .cursor
            .current_app(&self.xmb)
            .map(|app| (app.name.clone(), app.icon.clone()));
        let from = ui::launch_origin(panel.width as f32, panel.height as f32);
        let known: Vec<u32> = panel.windows.iter().map(|window| window.id).collect();
        let foreground = panel.foreground.clone().unwrap_or_default();
        // Disjoint fields, so the shared catalogue can be mutated while this
        // display's cursor is read.
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
        // A launched application takes the screen, so step back out of the way
        // and let it have the keyboard.
        if self.xmb.running_app().is_some() {
            self.guide.close();
        }
    }

    // --- the context menu --------------------------------------------------

    /// Raise the context menu on whatever is selected, or put away the one that
    /// is already up.
    ///
    /// Which entries it gets is decided by whichever surface is in front, and
    /// that is the only thing that differs between one caller and the next:
    /// each hands over the rectangle of the control being acted on, a name for
    /// it, and its rows. Adding a third place to raise one from is another arm
    /// here and nothing anywhere else.
    fn toggle_context_menu(&mut self) {
        if self.context_menu.close() {
            self.needs_redraw = true;
            return;
        }
        // Nothing is raised over a modal question about ending the session, or
        // over a launch that has taken the display: neither has anything on
        // screen for a menu to be about.
        if self.guide.power_open() || self.launching.is_some() {
            return;
        }
        let Some(height) = self
            .panels
            .get(self.focused_panel)
            .map(|panel| panel.height as f32)
        else {
            return;
        };
        let raised = if self.guide.is_menu() {
            self.window_card_menu()
        } else {
            self.bar_entry_menu()
        };
        let Some((anchor, title, entries)) = raised else {
            return;
        };

        // How many rows this display has room for goes in with the entries: the
        // menu keeps it because where a long list is scrolled to is state, and
        // everything else about its size is worked out afresh every frame.
        let rows = ui::context_menu_rows_that_fit(height);
        if self.context_menu.open_at(anchor, title, entries, rows) {
            self.needs_redraw = true;
        }
    }

    /// The menu for the bar's focused tile: the disc it stands on, the name of
    /// what is on it, and what can be done to it.
    ///
    /// Only over an application. A subcategory and a settings value are rows
    /// that lead somewhere or mean something rather than objects with a life of
    /// their own, and neither has anything a menu would offer yet.
    ///
    /// Two bands: what the menu can *tell* the user about the application and
    /// what it can do to the installation, then what it can do with the
    /// application right now. Close is in the second band because it is the way
    /// out of the menu rather than something done to the tile — on the bar
    /// nothing is running yet, so there is nothing here for a Close to end.
    fn bar_entry_menu(&self) -> Option<([f32; 4], Option<String>, Vec<menu::Entry>)> {
        let panel = self.panels.get(self.focused_panel)?;
        let app = panel.cursor.current_app(&self.xmb)?;
        let anchor = ui::launch_origin(panel.width as f32, panel.height as f32);
        Some((
            anchor,
            Some(app.name.clone()),
            vec![
                menu::Entry::new(menu::Command::Information, "Information")
                    .glyph(icons::SETTING_INFO),
                // Not drawn as grave, even though removing an application is
                // exactly that: this row does not remove anything, it asks. The
                // warmth belongs on the button that answers.
                menu::Entry::new(menu::Command::Uninstall, "Uninstall").glyph(icons::UNINSTALL),
                menu::Entry::new(menu::Command::Launch, "Launch")
                    .glyph(icons::LAUNCH)
                    .group(1),
                menu::Entry::new(menu::Command::Dismiss, "Close").group(1),
            ],
        ))
    }

    // --- the volume mixer --------------------------------------------------

    /// Raise the mixer out of the tile in the guide's column: every application
    /// making a noise, and the session's own output under them.
    ///
    /// The same panel every other context menu uses, because it is the same
    /// kind of object — a short list about one control, grown out of that
    /// control — and the rows it carries are tracks rather than commands. What
    /// makes it a mixer is the entries and nothing else.
    fn open_mixer(&mut self) {
        let Some((width, height)) = self.focused_size() else {
            return;
        };
        let items = self.guide.items(self.closable());
        let Some(index) = items.iter().position(|item| *item == Item::Mixer) else {
            return;
        };
        let anchor = ui::menu_item_rect(&items, index, width, height);
        let entries = self.mixer_entries();
        // No title. Every other menu is raised over something whose name the
        // user needs saying — an application, a window — and this one is a
        // column of applications each carrying its own name and picture. A
        // header would be the panel telling them what they are looking at.
        if self
            .context_menu
            .open_at(anchor, None, entries, ui::mixer_rows_that_fit(height))
        {
            self.needs_redraw = true;
        } else {
            // Nothing to mix: a machine with no volume control at all. The
            // tile still went down, which is the honest answer — there is no
            // mixer here rather than a mixer that failed to open.
            tracing::info!("nothing on this machine has a volume to set");
        }
    }

    /// The mixer's rows, newest sound first, with the session's own output at
    /// the foot of the list.
    ///
    /// The output is last and in a band of its own because it is not one of the
    /// applications: it is the thing they are all playing through, and turning
    /// *it* down turns all of them down. A user looking for the game they can
    /// hear should find it before they find the master control.
    fn mixer_entries(&self) -> Vec<menu::Entry> {
        let mut entries: Vec<menu::Entry> = self
            .quick
            .streams()
            .into_iter()
            .map(|stream| {
                // What the desktop entry that installed it calls it, and the
                // icon the bar already draws for it, wherever the catalogue
                // knows the program: `application.name` is whatever the
                // application says of itself — "Zen", "cmus", "Music Player
                // Daemon" — and the bar has the name the user chose it by.
                let installed = stream
                    .binary
                    .as_deref()
                    .and_then(|binary| self.xmb.app_for_window(binary))
                    .or_else(|| self.xmb.app_for_window(&stream.name));
                let name = installed
                    .map(|app| app.name.clone())
                    .unwrap_or_else(|| stream.name.clone());
                menu::Entry::new(menu::Command::MuteApplication(stream.key), name)
                    .level(stream.level)
                    // An application the catalogue has never heard of — a
                    // helper, or something started from a terminal — is drawn
                    // with the generic executable icon, which is what the atlas
                    // falls back to for a name it cannot resolve, the empty one
                    // included.
                    .icon(
                        installed
                            .and_then(|app| app.icon.clone())
                            .unwrap_or_default(),
                    )
            })
            .collect();
        if let Some(level) = self.quick.level(Knob::Volume) {
            entries.push(
                menu::Entry::new(menu::Command::MuteOutput, "System")
                    .icon(icons::CATEGORY_SYSTEM)
                    .level(level)
                    .group(1),
            );
        }
        entries
    }

    /// Whether the panel that is up is the mixer.
    ///
    /// Asked of the rows rather than remembered as a flag: a panel of tracks is
    /// a mixer, and a second place saying so is a second place to get it wrong
    /// when the menu is dismissed by one of the several things that dismiss it.
    fn mixer_is_up(&self) -> bool {
        self.context_menu.is_open()
            && self
                .context_menu
                .entries()
                .iter()
                .any(|entry| entry.level.is_some())
    }

    /// Keep the open mixer showing what the machine is actually playing.
    ///
    /// Both halves of that: an application that has started or stopped making a
    /// noise, and a level that has moved — whether the user moved it here, or
    /// the application moved it itself.
    fn sync_mixer(&mut self) {
        if !self.mixer_is_up() {
            return;
        }
        let entries = self.mixer_entries();
        if self.context_menu.refresh(entries) {
            self.needs_redraw = true;
        }
    }

    /// Move the highlighted track, if the highlighted row is one.
    ///
    /// This is the only menu where Left and Right mean anything. Everywhere
    /// else the panel is one column of commands and a sideways press is
    /// deliberately swallowed; a track is the one row that is *slid* rather
    /// than pressed, and it is slid with the same two directions the sidebar's
    /// own volume bar uses.
    fn nudge_mixer(&mut self, delta: i32) -> bool {
        let Some(entry) = self.context_menu.selected_entry() else {
            return false;
        };
        if entry.level.is_none() {
            return false;
        }
        match entry.command {
            menu::Command::MuteApplication(key) => self.quick.nudge_stream(key, delta),
            menu::Command::MuteOutput => self.quick.nudge(Knob::Volume, delta),
            _ => false,
        }
    }

    /// The menu for the guide's selected window card, on the same terms — and
    /// written separately for the point of the exercise: raising a menu
    /// somewhere new is one more of these and nothing anywhere else.
    fn window_card_menu(&self) -> Option<([f32; 4], Option<String>, Vec<menu::Entry>)> {
        let panel = self.panels.get(self.focused_panel)?;
        // The card the guide is pointing at, in the deck the guide builds: the
        // windows on this display and then the start screen, which is not a
        // window and has nothing that can be done to it.
        let selected = self.guide.selected_window(panel.windows.len() + 1);
        let window = panel.windows.get(selected)?;
        let anchor = self.guide_card_rect(panel, selected)?;

        // Where the two moves would land, if anywhere. The displays are a row
        // in the order the compositor announced them, so "next" and "previous"
        // are the ends of that row and neither wraps: a user on the first
        // screen has nothing before it, and one on the last has nothing after.
        //
        // Withheld rather than left out. A greyed row says the command exists
        // and does not apply here, which is the truth on a single-display
        // session and on both ends of a three-display one; a row that
        // disappeared would leave the menu a different shape on every screen.
        let (before, after) = self.neighbouring_displays();
        let move_next = menu::Entry::new(menu::Command::MoveToNextDisplay, "Move to next display")
            .glyph(icons::ARROW_RIGHT);
        let move_previous = menu::Entry::new(
            menu::Command::MoveToPreviousDisplay,
            "Move to previous display",
        )
        .glyph(icons::ARROW_LEFT);
        Some((
            anchor,
            Some(self.window_app_name(window)),
            vec![
                if after.is_some() {
                    move_next
                } else {
                    move_next.disabled()
                },
                if before.is_some() {
                    move_previous
                } else {
                    move_previous.disabled()
                },
                menu::Entry::new(menu::Command::Screenshot, "Screenshot the app")
                    .glyph(icons::SCREENSHOT),
                // Its own band: the three above act on the window, and this one
                // acts on the menu. `B` does the same and is not discoverable.
                menu::Entry::new(menu::Command::Dismiss, "Cancel").group(1),
            ],
        ))
    }

    /// The displays either side of the one being driven, as indices into
    /// [`Shell::panels`].
    ///
    /// One answer for both the menu and the commands it raises, so a row the
    /// user could press is a row that has somewhere to send the window. The
    /// order is the compositor's announcement order, which is the same order
    /// L1/R1 walk the displays in.
    fn neighbouring_displays(&self) -> (Option<usize>, Option<usize>) {
        neighbours(self.focused_panel, self.panels.len())
    }

    /// Where card `index` is on `panel` right now.
    ///
    /// The eased rectangle the frames are drawn at where there is one, so the
    /// menu grows out of the card the user can see rather than out of where it
    /// is going. Its settled slot otherwise — which is where a card that has
    /// not been drawn yet would start from anyway.
    fn guide_card_rect(&self, panel: &Panel, index: usize) -> Option<[f32; 4]> {
        let window = panel.windows.get(index)?;
        if let Some(glide) = self.guide_card_rects.get(&(window.id as u64)) {
            return Some(glide.at);
        }
        let count = panel.windows.len() + 1;
        let slots = lxb_protocol::overview::card_slots(
            panel.width as f64,
            panel.height as f64,
            count,
            self.guide.selected_window(count),
        );
        let fitted = lxb_protocol::overview::fit(
            slots.get(index)?,
            window.width as f64,
            window.height as f64,
        );
        Some([
            fitted.x as f32,
            fitted.y as f32,
            fitted.w as f32,
            fitted.h as f32,
        ])
    }

    /// Drive the menu: Up and Down move the highlight, Accept chooses.
    ///
    /// Left and Right do nothing on a column of commands, on purpose: the menu
    /// is one column, and Back is the way out of it — a sideways press that
    /// also dismissed it would make a mistimed direction close the thing the
    /// user was reading. On a row that carries a track they move that track,
    /// which is the same thing they do to the bars in the sidebar behind it.
    fn on_context_menu_action(&mut self, action: Action) {
        match action {
            Action::Up | Action::Down => {
                let delta = if action == Action::Up { -1 } else { 1 };
                if self.context_menu.move_selection(delta) {
                    self.needs_redraw = true;
                }
            }
            Action::Left | Action::Right => {
                let delta = if action == Action::Left { -1 } else { 1 };
                if self.nudge_mixer(delta) {
                    self.sync_mixer();
                    self.needs_redraw = true;
                }
            }
            Action::Launch => self.choose_context_menu(),
            _ => {}
        }
    }

    /// Carry out the menu's highlighted command.
    fn choose_context_menu(&mut self) {
        // Where the row is *before* it is chosen. A command that opens a panel
        // grows it out of the very control that was pressed, and by the time
        // `choose` has returned the menu is already on its way back into its own
        // anchor.
        let from = self
            .focused_size()
            .and_then(|(width, height)| {
                ui::context_menu_row_rect(
                    width,
                    height,
                    &self.context_menu,
                    self.context_menu.selected(),
                )
            })
            .unwrap_or_else(|| self.context_menu.anchor());
        let Some(command) = self.context_menu.choose() else {
            return;
        };
        self.carry_out(command, from);
    }

    /// Drive the centred panel, on the menu's own terms: Up and Down move the
    /// highlight and Accept presses the answer.
    ///
    /// Everything else is swallowed rather than passed on. It is modal — that
    /// is the whole difference between it and the menu — so a direction it has
    /// no use for must not reach the bar and move the selection the panel is
    /// about underneath it.
    fn on_dialog_action(&mut self, action: Action) {
        match action {
            Action::Up | Action::Down => {
                let delta = if action == Action::Up { -1 } else { 1 };
                if self.dialog.buttons.move_selection(delta) {
                    self.needs_redraw = true;
                }
            }
            Action::Launch => {
                let from = self
                    .focused_size()
                    .and_then(|(width, height)| {
                        ui::dialog_button_rect(
                            width,
                            height,
                            &self.dialog,
                            self.dialog.buttons.selected(),
                        )
                    })
                    .unwrap_or_else(|| self.dialog.buttons.anchor());
                if let Some(command) = self.dialog.buttons.choose() {
                    self.carry_out(command, from);
                }
            }
            _ => {}
        }
    }

    /// Do what a chosen row or button asks for.
    ///
    /// One place for every command, whichever surface it was pressed on: the
    /// menu raises panels whose buttons are more commands, and two match
    /// statements would be two places for a new one to be forgotten. `from` is
    /// the rectangle of the control that was pressed, which is what anything
    /// this opens grows out of.
    fn carry_out(&mut self, command: menu::Command, from: [f32; 4]) {
        match command {
            menu::Command::Information => self.show_app_information(from),
            menu::Command::Uninstall => self.ask_to_uninstall(from),
            menu::Command::Launch => self.start_selection(),
            menu::Command::ConfirmUninstall => self.begin_uninstall(),
            menu::Command::SubmitPassword => self.submit_password(),
            menu::Command::MoveToNextDisplay => self.move_selected_window(Toward::Next),
            menu::Command::MoveToPreviousDisplay => self.move_selected_window(Toward::Previous),
            menu::Command::Screenshot => self.screenshot_selected_window(from),
            // The panel stays up for both of these: a mixer row answers by
            // changing, and the answer is on the panel. `sync_mixer` is what
            // puts it there — the level the press asked for is already held by
            // the worker, whether or not the server has caught up.
            menu::Command::MuteApplication(key) => {
                self.quick.toggle_stream_mute(key);
                self.sync_mixer();
            }
            menu::Command::MuteOutput => {
                self.quick.toggle_mute();
                self.sync_mixer();
            }
            // The panel is already on its way out — choosing a row hands the
            // keys back and starts it folding — so all that is left is to stop
            // waiting on whatever it was for.
            menu::Command::Dismiss => {
                self.removal_plan = None;
                self.abandon_uninstall("cancelled");
            }
            menu::Command::Placeholder(name) => tracing::info!(
                command = name,
                "context menu: nothing is wired to this entry yet"
            ),
        }
        self.needs_redraw = true;
    }

    /// Put up what the shell knows about the selected application.
    ///
    /// The version and the size are not known yet when this returns — they come
    /// from a package manager, which takes long enough to drop frames — so the
    /// panel opens with those two rows saying so and [`Self::sync_app_facts`]
    /// fills them in when the answer lands.
    fn show_app_information(&mut self, from: [f32; 4]) {
        let Some(app) = self.selected_app().cloned() else {
            return;
        };
        let lines = information_lines(&app, None);
        let icon = app.icon.clone();
        let lookup = appinfo::Lookup::start(&app.path);
        self.app_facts = Some((app, lookup));
        self.dialog.ask(
            from,
            icon,
            lines,
            vec![menu::Entry::new(menu::Command::Dismiss, "Close")],
            0,
        );
    }

    /// Ask whether to remove the selected application.
    ///
    /// Working out what removing it would take starts here rather than when the
    /// question is answered. It costs two or three short-lived processes, and it
    /// buys the thing that matters: by the time the user has read a question and
    /// pressed a button, the shell already knows whether it needs a password,
    /// and the button leads straight to the panel that fits instead of to a
    /// spinner.
    fn ask_to_uninstall(&mut self, from: [f32; 4]) {
        let Some(app) = self.selected_app().cloned() else {
            return;
        };
        // Nothing is waiting on a package manager for this panel, and a lookup
        // still running for the last one must not rewrite it.
        self.app_facts = None;
        self.removal_plan = Some((app.clone(), uninstall::Survey::start(&app.path)));
        self.dialog.ask(
            from,
            app.icon.clone(),
            vec![
                // The sentence is set across two lines at the one place it can
                // always be broken. Every run the shell draws is a single line
                // with an ellipsis where the rest would have been, and the name
                // of the application is the half of this question that must not
                // be the half that gets cut.
                dialog::Line::Note("Do you want to uninstall the".to_string()),
                dialog::Line::Heading(format!("{}?", app.name)),
                dialog::Line::Rule,
            ],
            vec![
                menu::Entry::new(menu::Command::ConfirmUninstall, "Yes").destructive(),
                menu::Entry::new(menu::Command::Dismiss, "No"),
            ],
            // On No. A confirmation whose default answer destroys something is
            // not a confirmation.
            1,
        );
    }

    // --- removing an application --------------------------------------------

    /// The user has said yes. Take up the plan that was worked out while they
    /// were reading the question, and go wherever it leads.
    fn begin_uninstall(&mut self) {
        let Some((app, survey)) = self.removal_plan.take() else {
            // The question cannot be answered without having been asked, so
            // this is unreachable — and if it ever is reached, the safe thing
            // is to remove nothing and say so rather than to work out a plan
            // now, on the far side of the only confirmation there is.
            tracing::warn!("a removal was confirmed that was never surveyed");
            return;
        };
        tracing::info!(application = %app.name, entry = ?app.path, "removal confirmed");
        self.uninstalling = Some(Uninstall {
            app,
            survey,
            stage: Stage::Surveying,
        });
        // Something has to be on screen while the survey finishes, on the rare
        // frames where it has not. Put it up first and then look: the usual
        // case is that the answer is already in and this panel is replaced
        // before it has grown far enough to read.
        self.say_working("Checking…");
        self.advance_uninstall();
    }

    /// Take the password the field has collected and start the removal with it.
    fn submit_password(&mut self) {
        let started = {
            let Some(state) = self.uninstalling.as_mut() else {
                return;
            };
            // Nothing typed is not a password sudo should be asked about: the
            // user would be told their password was wrong, which is not what
            // happened. The field simply waits.
            let empty =
                matches!(&state.stage, Stage::Asking { password, .. } if password.is_empty());
            if empty || !matches!(state.stage, Stage::Asking { .. }) {
                return;
            }
            // Taken out whole, so the password moves to the worker rather than
            // being copied out of a field the panel is still drawing from.
            let Stage::Asking { removal, password } =
                std::mem::replace(&mut state.stage, Stage::Done)
            else {
                return;
            };
            state.stage = Stage::Working(uninstall::Run::start(removal, Some(password)));
            true
        };
        if started {
            self.say_working("Removing… this can take a moment.");
            self.dismiss_password_board();
        }
    }

    /// Bring a removal up to date with its workers, and put up whatever panel
    /// the answer calls for.
    ///
    /// Two halves, and they are split because they cannot borrow the same
    /// things: deciding needs the removal's own state held mutably, and acting
    /// needs the whole shell. [`Step`] is what passes between them.
    fn advance_uninstall(&mut self) {
        let Some(state) = self.uninstalling.as_mut() else {
            return;
        };
        let step = match &mut state.stage {
            Stage::Surveying => match state.survey.plan() {
                None => Step::Nothing,
                Some(plan) => match (plan.removal, plan.authority) {
                    (None, _) => Step::CannotPlace,
                    (Some(_), uninstall::Authority::Forbidden) => Step::Forbidden,
                    (Some(removal), uninstall::Authority::NeedsPassword) => {
                        state.stage = Stage::Asking {
                            removal,
                            password: uninstall::Secret::default(),
                        };
                        Step::AskPassword { retry: false }
                    }
                    (Some(removal), _) => {
                        state.stage = Stage::Working(uninstall::Run::start(removal, None));
                        Step::Removing
                    }
                },
            },
            Stage::Working(run) => match run.outcome() {
                None => Step::Nothing,
                Some(uninstall::Outcome::Removed) => {
                    state.stage = Stage::Done;
                    Step::Removed
                }
                Some(uninstall::Outcome::Failed(why)) => {
                    state.stage = Stage::Done;
                    Step::Failed(why)
                }
                // The one failure the user can do something about. Straight
                // back to the field, with the plan intact — the removal itself
                // has not been attempted.
                Some(uninstall::Outcome::WrongPassword) => match state.survey.plan() {
                    Some(uninstall::Plan {
                        removal: Some(removal),
                        ..
                    }) => {
                        state.stage = Stage::Asking {
                            removal,
                            password: uninstall::Secret::default(),
                        };
                        Step::AskPassword { retry: true }
                    }
                    _ => {
                        state.stage = Stage::Done;
                        Step::CannotPlace
                    }
                },
            },
            Stage::Asking { .. } | Stage::Done => Step::Nothing,
        };

        if matches!(step, Step::Nothing) {
            return;
        }
        let Some(app) = self.uninstalling.as_ref().map(|state| state.app.clone()) else {
            return;
        };
        match step {
            Step::Nothing => {}
            Step::CannotPlace => {
                self.finish_uninstall();
                self.say_and_acknowledge(
                    &app,
                    vec![
                        dialog::Line::Heading(app.name.clone()),
                        dialog::Line::Note(
                            "LineXinBar cannot tell what installed this, so it".to_string(),
                        ),
                        dialog::Line::Note("cannot remove it.".to_string()),
                        dialog::Line::Rule,
                    ],
                );
            }
            Step::Forbidden => {
                self.finish_uninstall();
                self.say_and_acknowledge(
                    &app,
                    vec![
                        dialog::Line::Heading(app.name.clone()),
                        dialog::Line::Note("You do not have permission to remove".to_string()),
                        dialog::Line::Note("applications on this system.".to_string()),
                        dialog::Line::Rule,
                    ],
                );
            }
            Step::AskPassword { retry } => self.ask_for_password(&app, retry),
            Step::Removing => self.say_working("Removing…"),
            Step::Removed => {
                self.finish_uninstall();
                // The tile has to go with it. Everything on the bar is built
                // from the desktop entries on disk, and one of those has just
                // been deleted, so the honest way to take the application off
                // the bar is to look again.
                self.rescan_applications();
                self.say_and_acknowledge(
                    &app,
                    vec![
                        dialog::Line::Heading(app.name.clone()),
                        dialog::Line::Note("has been removed.".to_string()),
                        dialog::Line::Rule,
                    ],
                );
            }
            Step::Failed(why) => {
                self.finish_uninstall();
                self.say_and_acknowledge(
                    &app,
                    vec![
                        dialog::Line::Heading(format!("Could not remove {}", app.name)),
                        dialog::Line::Note(why),
                        dialog::Line::Rule,
                    ],
                );
            }
        }
        self.needs_redraw = true;
    }

    /// Let go of the removal, and of the keyboard it may have borrowed.
    fn finish_uninstall(&mut self) {
        self.uninstalling = None;
        self.dismiss_password_board();
    }

    /// Give up on a removal that has not been started. Says nothing and puts
    /// nothing up: whatever the user pressed is already taking the panel away.
    fn abandon_uninstall(&mut self, why: &'static str) {
        let Some(state) = self.uninstalling.take() else {
            return;
        };
        // A removal that is already running is not abandoned — it is only
        // stopped being watched, which is worth a line, because the application
        // will disappear from the bar the next time the shell looks.
        if matches!(state.stage, Stage::Working(_)) {
            tracing::warn!(application = %state.app.name, why, "left a removal running");
        }
        self.dismiss_password_board();
    }

    /// Whether a removal is actually under way, which is the one thing on this
    /// surface that must not be interrupted by a stray press of Back.
    fn removal_running(&self) -> bool {
        self.uninstalling
            .as_ref()
            .is_some_and(|state| matches!(state.stage, Stage::Working(_)))
    }

    /// Put up a panel with nothing to press, because the shell is busy behind
    /// it and there is nothing useful to offer until it is not.
    fn say_working(&mut self, note: &str) {
        let (app, icon) = match self.uninstalling.as_ref() {
            Some(state) => (state.app.name.clone(), state.app.icon.clone()),
            None => return,
        };
        let from = self.dialog_origin();
        self.dialog.wait(
            from,
            icon,
            vec![
                dialog::Line::Heading(app),
                dialog::Line::Note(note.to_string()),
            ],
        );
    }

    /// Put up a panel that says something and has one button to say it has been
    /// read.
    fn say_and_acknowledge(&mut self, app: &apps::App, lines: Vec<dialog::Line>) {
        let from = self.dialog_origin();
        self.dialog.ask(
            from,
            app.icon.clone(),
            lines,
            vec![menu::Entry::new(menu::Command::Dismiss, "OK")],
            0,
        );
    }

    /// Put up the field that collects the password, with the keyboard.
    fn ask_for_password(&mut self, app: &apps::App, retry: bool) {
        let from = self.dialog_origin();
        let typed = match self.uninstalling.as_ref().map(|state| &state.stage) {
            Some(Stage::Asking { password, .. }) => password.typed(),
            _ => 0,
        };
        self.dialog.ask(
            from,
            app.icon.clone(),
            vec![
                dialog::Line::Heading(format!("Removing {}", app.name)),
                dialog::Line::Note(if retry {
                    "That password was not accepted. Try again.".to_string()
                } else {
                    "Enter your password to allow this.".to_string()
                }),
                dialog::Line::Secret { typed },
                dialog::Line::Rule,
            ],
            vec![
                menu::Entry::new(menu::Command::SubmitPassword, "Uninstall").destructive(),
                menu::Entry::new(menu::Command::Dismiss, "Cancel"),
            ],
            // On Cancel, exactly as the question was: the button that destroys
            // something is never the one already under the user's thumb.
            1,
        );
        // The board comes up with the field, because on a console there is
        // nothing else to type with. It types *here* rather than through the
        // virtual keyboard — see `keyboard::Osk::open_here`, and note that the
        // ordinary board refuses to open at all without one, which would have
        // left a password field with no way to fill it in.
        self.osk.open_here();
        self.sync_surface_state();
    }

    /// Redraw the password field after a keystroke.
    ///
    /// The panel is rebuilt from the count alone, so this is the whole of what
    /// typing changes on screen — and the one line of the panel that ever holds
    /// anything about the password is a number.
    fn refresh_password_field(&mut self) {
        let typed = match self.uninstalling.as_ref().map(|state| &state.stage) {
            Some(Stage::Asking { password, .. }) => password.typed(),
            _ => return,
        };
        let lines = self
            .dialog
            .lines()
            .iter()
            .map(|line| match line {
                dialog::Line::Secret { .. } => dialog::Line::Secret { typed },
                other => other.clone(),
            })
            .collect();
        self.dialog.say(lines);
        self.needs_redraw = true;
    }

    /// Apply one keystroke to the password being typed. Returns whether it was
    /// this field's to take.
    fn type_into_password(&mut self, stroke: keyboard::Stroke) -> bool {
        // Whether the field is even there, and what the keystroke did to it,
        // decided with only the removal borrowed — the two keys that finish
        // the field need the whole shell afterwards.
        let done = {
            let Some(state) = self.uninstalling.as_mut() else {
                return false;
            };
            let Stage::Asking { password, .. } = &mut state.stage else {
                return false;
            };
            match stroke {
                keyboard::Stroke::Char(character) => {
                    password.push(character);
                    None
                }
                keyboard::Stroke::BACKSPACE => {
                    password.pop();
                    None
                }
                keyboard::Stroke::ENTER => Some(true),
                keyboard::Stroke::ESCAPE => Some(false),
                // Tab, the arrows, the function keys: a password field has no
                // use for any of them, and passing them on to the bar
                // underneath would move the selection this panel is about.
                _ => return true,
            }
        };
        match done {
            Some(true) => self.submit_password(),
            Some(false) => {
                self.abandon_uninstall("escaped");
                self.close_dialog();
                self.needs_redraw = true;
            }
            None => self.refresh_password_field(),
        }
        true
    }

    /// Put the board away if it was up for a password, and tell the compositor.
    fn dismiss_password_board(&mut self) {
        if self.osk.types_here() && self.osk.close() {
            self.sync_surface_state();
            self.needs_redraw = true;
        }
    }

    /// Where the panel that is on screen now sits, so the next one grows out of
    /// it rather than out of a control that has long since gone.
    fn dialog_origin(&self) -> [f32; 4] {
        self.focused_size()
            .map(|(width, height)| ui::dialog_rect(width, height, &self.dialog))
            .unwrap_or_else(|| self.dialog.buttons.anchor())
    }

    /// Look at what is installed again, after something has been removed.
    ///
    /// Every display's cursor is put back to the top with it. The columns have
    /// changed shape underneath them — one row shorter, and possibly one column
    /// shorter — and a cursor left pointing at the fourth row of a column that
    /// now has three is worse than one that has plainly started again.
    fn rescan_applications(&mut self) {
        let categories = apps::scan();
        let total: usize = categories.iter().map(apps::Category::apps).sum();
        tracing::info!(
            categories = categories.len(),
            applications = total,
            "scanned applications again"
        );
        self.xmb.categories = categories;
        for panel in &mut self.panels {
            panel.cursor = Cursor::for_model(&self.xmb);
        }
        self.needs_redraw = true;
    }

    /// Put the centred panel away, and stop waiting on anything it had asked
    /// for. Returns whether it was open.
    fn close_dialog(&mut self) -> bool {
        self.app_facts = None;
        self.removal_plan = None;
        self.abandon_uninstall("dismissed");
        self.dialog.close()
    }

    /// Fill the open panel's version and size in once the package manager has
    /// answered.
    fn sync_app_facts(&mut self) {
        let Some((app, lookup)) = self.app_facts.as_ref() else {
            return;
        };
        let Some(facts) = lookup.answer() else {
            return;
        };
        // Only into the panel that asked. One dismissed while its answer was
        // still in flight gets nothing written into it on the way out.
        if self.dialog.is_open() {
            let lines = information_lines(app, Some(&facts.facts));
            self.dialog.say(lines);
            self.needs_redraw = true;
        }
        self.app_facts = None;
    }

    /// The application the bar's cursor is on, which is what a menu raised from
    /// the bar and everything it opens are about.
    fn selected_app(&self) -> Option<&apps::App> {
        self.panels
            .get(self.focused_panel)?
            .cursor
            .current_app(&self.xmb)
    }

    /// The size of the display being driven.
    fn focused_size(&self) -> Option<(f32, f32)> {
        self.panels
            .get(self.focused_panel)
            .map(|panel| (panel.width as f32, panel.height as f32))
    }

    /// Drive the board: the directions move the cursor, and accept presses a
    /// key.
    fn on_keyboard_action(&mut self, action: Action) {
        let direction = match action {
            Action::Left => Some(guide::Move::Left),
            Action::Right => Some(guide::Move::Right),
            Action::Up => Some(guide::Move::Up),
            Action::Down => Some(guide::Move::Down),
            _ => None,
        };
        if let Some(direction) = direction {
            if self.osk.board.move_selection(direction) {
                self.needs_redraw = true;
            }
            return;
        }
        if action == Action::Launch {
            self.press_key();
        }
    }

    /// Act on a key the user pressed on their own keyboard while the board was
    /// up — which is to say, put the board away.
    ///
    /// A key on a real keyboard is the user saying they do not need a picture
    /// of one, so the board leaves rather than being driven. The key it
    /// intercepted goes on to the application first, so that dismissing the
    /// board costs no letter: the board holds the grab that key arrived on,
    /// and nothing else is in a position to pass it along.
    ///
    /// Timestamped from the shell's own clock rather than the seat's, even
    /// though the seat's is the one the key really arrived on. Everything the
    /// virtual keyboard sends has to keep increasing, and the two clocks have
    /// different origins: interleaving them would hand the application a
    /// keystroke from the past every time the user reached for the board.
    fn on_typed(&mut self, typed: keyboard::Typed) {
        let at = self.start.elapsed().as_millis() as u32;
        // Except when the field being typed into is the shell's own. Then the
        // key belongs here, and it must not be forwarded anywhere: a password
        // pushed through the virtual keyboard would go to whichever client
        // holds the keys. The board still goes away — the user plainly has a
        // keyboard — and everything after this arrives through `on_key`.
        if self.osk.types_here() {
            if let keyboard::Typed::Send(stroke) = typed {
                self.type_into_password(stroke);
            }
            if !matches!(typed, keyboard::Typed::Ignored) && self.osk.close() {
                self.sync_surface_state();
                self.needs_redraw = true;
            }
            return;
        }
        if let keyboard::Typed::Send(stroke) = typed {
            if !self.osk.send(stroke, at) {
                tracing::debug!(?stroke, "nothing typed: no virtual keyboard");
            }
            self.flush_keystroke();
        }
        if matches!(typed, keyboard::Typed::Ignored) {
            return;
        }
        if self.osk.dismiss_for_typing() {
            tracing::debug!("a key on the user's own keyboard put the board away");
            self.sync_surface_state();
            self.needs_redraw = true;
        }
    }

    /// Put the board's cursor on the key the pointer is over.
    ///
    /// Hovering *is* selecting, rather than a second highlight of the
    /// pointer's own. The board already has one cursor, one button that
    /// presses whatever it is on, and one drawing that says where it is; a
    /// pointer that lit a different key would leave the user two places to
    /// look and `A` doing the wrong one of them.
    ///
    /// Nothing under the cursor leaves the selection alone. Crossing a gap
    /// between two keys is not a reason to forget which key the thumb was on.
    fn hover_key(&mut self, key: Option<(usize, usize)>) {
        let Some((row, column)) = key else {
            return;
        };
        if self.osk.is_open() && self.osk.board.select(row, column) {
            self.needs_redraw = true;
        }
    }

    /// Ask for the cursor a keyboard deserves, if the compositor will be told.
    ///
    /// Over a text field the application has usually asked for a beam, and a
    /// beam left hanging over a picture of a keyboard says the letters can be
    /// selected rather than pressed. A pointing hand is what a row of buttons
    /// takes.
    fn set_cursor_shape(
        &self,
        qh: &QueueHandle<Self>,
        pointer: &wl_pointer::WlPointer,
        serial: u32,
    ) {
        use smithay_client_toolkit::reexports::protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1 as shape;
        let Some(manager) = self.cursor_shape.as_ref() else {
            return;
        };
        let device = manager.get_shape_device(pointer, qh);
        device.set_shape(serial, shape::Shape::Pointer);
        device.destroy();
    }

    /// Send whatever the selected key types.
    fn press_key(&mut self) {
        // The protocol wants a millisecond timestamp that keeps going up, and
        // the shell already has one clock everything else is measured against.
        let at = self.start.elapsed().as_millis() as u32;
        // Read before the press: closing the board clears it.
        let types_here = self.osk.types_here();
        let press = self.osk.press(at);
        // A board typing into the shell's own field sends nothing anywhere —
        // it hands the keystroke back, and this is where it goes.
        if types_here {
            if let keyboard::Press::Type(stroke) = press {
                self.type_into_password(stroke);
            }
        }
        if press == keyboard::Press::Close {
            self.sync_surface_state();
        }
        self.flush_keystroke();
        self.needs_redraw = true;
    }

    /// A key is a request on the connection and arrives when the connection is
    /// flushed. The loop does that on its way into the poll, but a shell that
    /// is otherwise idle — which, with the board up and an application in
    /// front, it is — should not make a letter wait for the next frame.
    fn flush_keystroke(&self) {
        if let Err(err) = self.conn.flush() {
            tracing::warn!(?err, "could not send the keystroke");
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

    /// Bring the launch splash up to date, and let it go once it has handed
    /// the display over. Returns whether that changed the layer state by
    /// removing the splash.
    ///
    /// Its own step rather than part of the drawing, because what it watches
    /// for — a window that was not there before — is the very thing that
    /// stops this display drawing at all: without it the splash would freeze
    /// at the moment the application covered the bar.
    fn advance_launch(&mut self, now: Instant) -> bool {
        if self.launching.is_none() {
            return false;
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
                return true;
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
        finished
    }

    /// The window the selected tile's application already has, as a display
    /// and a window on it.
    ///
    /// The display the user is on is searched first, so an application running
    /// on two displays brings back the copy in front of them. Windows whose
    /// application never named itself match nothing: an unnamed window is not
    /// evidence that *this* application is running, and guessing would start
    /// nothing where the user asked for something.
    fn window_for_selection(&self) -> Option<(usize, u32)> {
        let app = self
            .panels
            .get(self.focused_panel)?
            .cursor
            .current_app(&self.xmb)?;
        let on = |index: usize| {
            let panel = self.panels.get(index)?;
            let window = panel
                .windows
                .iter()
                .find(|window| app.owns_window(&window.app_id))?;
            Some((index, window.id))
        };
        on(self.focused_panel).or_else(|| (0..self.panels.len()).find_map(on))
    }

    /// Bring back a window the selected application already has.
    ///
    /// The splash is the one a launch puts up, for the same reason: the press
    /// has to be answered on the tile it was made on. It is drawn on the
    /// display the window is on, because that is where the application is
    /// about to appear — applications are pinned to the display they opened
    /// on, so coming back to one means going to that display, and the shell's
    /// own focus follows so the guide and the next press land there too.
    fn restore_window(&mut self, display: usize, window: u32) {
        let Some(control) = self.shell_control.clone() else {
            return;
        };
        if control.version() < OVERVIEW_SHELL_VERSION {
            return;
        }

        let Some(panel) = self.panels.get(display) else {
            return;
        };
        let name = self
            .panels
            .get(self.focused_panel)
            .and_then(|panel| panel.cursor.current_app(&self.xmb))
            .map(|app| app.name.clone())
            .unwrap_or_default();
        let from = ui::launch_origin(panel.width as f32, panel.height as f32);

        tracing::info!(
            app = %name,
            display = %panel.name,
            window,
            "application is already running; coming back to it"
        );

        // No splash. The application is *there* — it never stopped — so what
        // the user is shown is that window arriving, grown out of the tile
        // they pressed by the only process that has its pixels. A panel with
        // an icon and a spinner would be announcing a load that is not
        // happening.
        if control.version() >= RESTORE_SHELL_VERSION {
            control.activate_window_from(
                window,
                from[0].round() as i32,
                from[1].round() as i32,
                from[2].max(0.0).round() as u32,
                from[3].max(0.0).round() as u32,
            );
            self.restoring = Some(Restore {
                panel: display,
                from,
                started: Instant::now(),
            });
        } else {
            // An older compositor cannot fly it, and the shell cannot fly it
            // for them. Raising it is still the right answer to the press.
            control.activate_window(window);
            self.guide.close();
        }

        self.focused_panel = display;
        self.needs_redraw = true;
    }

    /// Advance a window flying back out of its tile, and get out of its way
    /// once it has landed.
    ///
    /// The start screen stays up for the whole flight — the compositor draws
    /// the window in front of it — so that what recedes behind the arriving
    /// application is the screen the user actually left, rather than a gap
    /// where it used to be. Only when the window fills the display does the
    /// bar drop back down and give up the keyboard.
    fn advance_restore(&mut self, now: Instant) -> bool {
        let Some(restore) = self.restoring.as_ref() else {
            return false;
        };
        if now.duration_since(restore.started) < lxb_protocol::overview::FLIGHT {
            self.needs_redraw = true;
            return false;
        }
        self.restoring = None;
        self.guide.close();
        self.needs_redraw = true;
        true
    }

    /// Whether a window is flying back onto `index`, which keeps that display
    /// drawing while the flight is on.
    fn restoring_on(&self, index: usize) -> bool {
        self.restoring
            .as_ref()
            .is_some_and(|restore| restore.panel == index)
    }

    /// Whether a launch splash is on `index` — which is also what keeps that
    /// display drawing while the application it is waiting for covers the bar.
    fn launching_on(&self, index: usize) -> bool {
        self.launching
            .as_ref()
            .is_some_and(|splash| splash.panel == index)
    }

    /// Whether anything this display draws can still be seen.
    ///
    /// Behind a fullscreen application none of it can, and an animated
    /// background nobody is looking at costs exactly what the application in
    /// front — a game, usually — is asking the GPU for. Every frame the shell
    /// skips is also a frame the compositor does not have to composite. The
    /// guide is the exception: it is drawn *over* the application, which is
    /// the whole point of it.
    fn panel_is_visible(&self, index: usize) -> bool {
        let Some(panel) = self.panels.get(index) else {
            return false;
        };
        if self.launching_on(index) || self.restoring_on(index) {
            return true;
        }
        if index == self.focused_panel && self.guide.is_over_app() {
            return true;
        }
        // So is the keyboard, and its hint: both are drawn over the
        // application on purpose, so a display that has stopped drawing
        // because something covered its bar has to start again for them.
        if index == self.focused_panel && self.keyboard_visible() {
            return true;
        }
        !bar_is_covered(panel.width, panel.height, &panel.windows)
    }

    /// Whether the focused display is drawing the keyboard, or the hint that
    /// says how to summon it.
    ///
    /// A different thing from the guide, and configured differently: it is
    /// drawn above the application without taking either the keys or the
    /// pointer from it.
    /// `is_on_screen` rather than `is_open`, so that a board being put away
    /// keeps the display drawing and its surface raised until it has finished
    /// leaving. Nothing may vanish before its transition ends, and the whole
    /// of this answer is what would take it off screen mid-fall.
    fn keyboard_visible(&self) -> bool {
        keyboard_is_visible(
            self.guide.is_over_app(),
            self.osk.is_on_screen(),
            self.osk.wants_hint(),
            self.app_running(),
            self.osk.types_here(),
        )
    }

    /// Whether that drawing is happening *over* a running application.
    ///
    /// The narrower question, and the one the bar has to answer to: the
    /// surface it shares with the board is raised above the application to
    /// carry it, and the bar belongs behind that application. With nothing
    /// running there is nothing to be in front of, and the bar stays where it
    /// is rather than blanking itself for a board floating over an empty
    /// screen.
    fn keyboard_overlay(&self) -> bool {
        self.keyboard_visible() && self.app_running()
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
            // A bar is slid, not pressed. Left and Right move it, which is
            // also the one place they do not cross to the window cards: a
            // console's settings sliders work the same way, and the way off a
            // bar is the way you arrived at it, vertically.
            Action::Left | Action::Right
                if self.guide.pane() == guide::Pane::Menu && self.selected_bar().is_some() =>
            {
                let Some(bar) = self.selected_bar() else {
                    return;
                };
                let delta = if action == Action::Left { -1 } else { 1 };
                if self.quick.nudge(knob(bar), delta) {
                    self.needs_redraw = true;
                }
            }
            // The tiles share a line, so Left and Right walk along it first.
            // Off its end they carry on meaning what they mean everywhere else
            // in the column — Right crosses to the window cards.
            Action::Left | Action::Right
                if self.guide.pane() == guide::Pane::Menu
                    && self.guide.can_move_in_line(
                        if action == Action::Left { -1 } else { 1 },
                        closable,
                    ) =>
            {
                let delta = if action == Action::Left { -1 } else { 1 };
                if self.guide.move_in_line(delta, closable) {
                    self.needs_redraw = true;
                }
            }
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

    /// Which bar the highlight is sitting on, if it is on one.
    fn selected_bar(&self) -> Option<guide::Bar> {
        self.guide.selected_item(self.closable())?.bar()
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
            // Remembered against the application, not the session: a stick
            // that is a mouse in a browser is a camera in a game, and the
            // whole point of the switch is that the answer differs.
            Item::Pointer => match self.pointer_app().map(str::to_string) {
                Some(app) => {
                    self.guide.press(item);
                    let on = self.prefs.toggle_stick_pointer(&app);
                    // A stick left deflected while the switch was off must not
                    // deliver the interval it spent there the moment it comes
                    // on.
                    self.stick.rest();
                    if !on {
                        if let Some(control) = self.shell_control.clone() {
                            self.release_stick_buttons(&control);
                        }
                    }
                }
                // Nothing in front to attach it to, so the tile is drawn but
                // inert — the same answer as pressing A on the brightness bar.
                None => tracing::debug!("no application to turn the stick pointer on for"),
            },
            // The one tile that opens something instead of turning something
            // over. The press still plays: what the user pressed is a control
            // in the column, and the panel grows out of that very control, so
            // the tile going down is the first frame of the panel arriving.
            Item::Mixer => {
                self.guide.press(item);
                self.open_mixer();
            }
            // A on the volume bar silences the session, the way the key marked
            // with a crossed-out speaker does. There is no equivalent for a
            // screen, so A on the brightness bar does nothing rather than
            // something invented.
            Item::Volume => {
                self.quick.toggle_mute();
            }
            Item::Brightness => {}
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

    /// Send the selected card's window to the display beside this one.
    ///
    /// The direction is resolved against the same neighbours the menu drew its
    /// rows from, and a direction with no display in it does nothing: a
    /// disabled row cannot be chosen, so arriving here with nowhere to go means
    /// a display was unplugged while the menu was open.
    ///
    /// The compositor moves it, because a window's place in the layout is the
    /// compositor's. Nothing is done to the guide afterwards: the window list
    /// for both displays comes back over the protocol, the card leaves this
    /// display's deck, and the deck closes up around it.
    fn move_selected_window(&mut self, toward: Toward) {
        let (before, after) = self.neighbouring_displays();
        let Some(index) = (match toward {
            Toward::Next => after,
            Toward::Previous => before,
        }) else {
            tracing::debug!(?toward, "no display that way to move the window to");
            return;
        };
        let Some(id) = self.close_target().map(|window| window.id) else {
            return;
        };
        let Some(control) = self.shell_control.clone() else {
            return;
        };
        if control.version() < WINDOW_MOVE_AND_CAPTURE_VERSION {
            tracing::info!("this compositor cannot move a window between displays");
            return;
        }
        let Some(target) = self.panels.get(index) else {
            return;
        };

        tracing::info!(display = %target.name, id, "moving a window to another display");
        control.move_window_to_output(id, &target.output);
        if let Err(err) = self.conn.flush() {
            tracing::warn!(?err, "could not send the move request");
        }
    }

    /// Photograph the selected card's window into the user's pictures.
    ///
    /// The shell chooses the file and the compositor writes it: only the
    /// compositor has the window's pixels — a client cannot see another
    /// client's surface, which is the whole point of Wayland — and only the
    /// shell has any business deciding where a user's screenshots live.
    ///
    /// What the user is told happens when the answer comes back, not here. Both
    /// halves of this can fail, and a panel that said "saved" before anything
    /// had been written would eventually be lying to somebody.
    fn screenshot_selected_window(&mut self, from: [f32; 4]) {
        let Some(card) = self.close_target() else {
            return;
        };
        let (id, app) = (card.id, self.window_app_name(card));

        let Some(control) = self.shell_control.clone() else {
            return self.report_screenshot(from, app, None);
        };
        if control.version() < WINDOW_MOVE_AND_CAPTURE_VERSION {
            tracing::info!("this compositor cannot photograph a window");
            return self.report_screenshot(from, app, None);
        }
        // The clock decides the name, so a screenshot taken on a machine whose
        // clock cannot be read has no name to be filed under.
        let Some(taken_at) = local_time() else {
            tracing::warn!("cannot read the clock, so cannot name a screenshot");
            return self.report_screenshot(from, app, None);
        };
        let Some(path) = screenshot::destination(&taken_at) else {
            return self.report_screenshot(from, app, None);
        };
        let path = screenshot::unclaimed(path, &taken_at);

        control.capture_window(id, path.to_string_lossy().into_owned());
        if let Err(err) = self.conn.flush() {
            tracing::warn!(?err, "could not send the screenshot request");
            return self.report_screenshot(from, app, None);
        }
        // Where the panel grows from, held until the compositor answers: the
        // row that was pressed is folding away with the rest of the menu, and
        // the answer belongs to the control the user actually touched.
        self.pending_capture = Some((from, app));
    }

    /// Say what came of a screenshot: where it went, or that it did not happen.
    ///
    /// The centred panel rather than a line in the log, because a button that
    /// silently either worked or did not is a button nobody trusts twice. It
    /// grows out of `from` — the menu row that was pressed — exactly as
    /// Information does.
    fn report_screenshot(&mut self, from: [f32; 4], app: String, saved: Option<PathBuf>) {
        let lines = match &saved {
            // The folder, not the file. A panel gives one line to a value and
            // the name is a timestamp — the longest thing here and the least
            // worth reading, since the picture the user just took is the newest
            // one in the folder. Where that folder *is* is the question this
            // answers, and it is the one they cannot work out for themselves.
            Some(path) => vec![
                dialog::Line::Heading(app),
                dialog::Line::Note("The screenshot has been saved.".to_string()),
                dialog::Line::Rule,
                dialog::Line::field(
                    "Saved in",
                    screenshot::abbreviated(path.parent().unwrap_or(path)),
                ),
                dialog::Line::Rule,
            ],
            None => vec![
                dialog::Line::Heading(app),
                dialog::Line::Note("The screenshot could not be saved.".to_string()),
                dialog::Line::Rule,
            ],
        };
        self.dialog.ask(
            from,
            None,
            lines,
            vec![menu::Entry::new(menu::Command::Dismiss, "Close")],
            0,
        );
        self.needs_redraw = true;
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

    /// Tell every display what the Settings column asks of it.
    ///
    /// Sent to all of them rather than to the one the user is driving: the
    /// Settings tree is the session's, drawn identically on every screen, so a
    /// choice made on one is a choice about all of them. The compositor
    /// ignores it on a display that cannot do it, and says so in the event
    /// that comes back — which is what the page reports rather than pretending
    /// the setting took.
    ///
    /// Diffed per display so this can be called every loop iteration: a
    /// display plugged in halfway through the session is then configured by
    /// the same path that configured the others, without a second one for it.
    fn sync_hdr(&mut self) {
        let Some(control) = self.shell_control.clone() else {
            return;
        };
        if control.version() < HDR_SHELL_VERSION {
            return;
        }
        let mut sent = false;
        for panel in &mut self.panels {
            // Each display's own settings, filed under the connector name the
            // compositor knows it by — which is the same name the Settings
            // column lists it under, so what was chosen on a screen's page is
            // what that screen is told.
            let wanted = settings::hdr_for(&panel.name);
            if panel.applied_hdr == Some(wanted) {
                continue;
            }
            control.set_output_hdr(
                &panel.output,
                wanted.enabled as u32,
                wanted.sdr_brightness as u32,
                wanted.srgb_intensity as u32,
                wanted.peak_brightness as u32,
            );
            panel.applied_hdr = Some(wanted);
            sent = true;
            tracing::debug!(display = %panel.name, ?wanted, "asked for these display settings");
        }
        if sent {
            if let Err(err) = self.conn.flush() {
                tracing::warn!(?err, "could not send the display settings");
            }
        }
    }

    /// Ask each display for the mode it has been given, if it has been given
    /// one and is not already there.
    ///
    /// One request for both rows of the page, because a connector is set to a
    /// mode rather than to a size and a rate separately — which is why the two
    /// settings meet in one entry before they are sent.
    ///
    /// Diffed per display like [`Shell::sync_hdr`], and for the same reason: a
    /// display plugged in halfway through the session comes back to the mode
    /// it was left in by the same path that set it in the first place.
    ///
    /// Only displays with a mode of their own are told anything. A display
    /// nobody has chosen for is left at whatever the compositor brought it up
    /// at — which is the compositor's own config, and not something the shell
    /// should overrule by sending a mode it invented.
    fn sync_mode(&mut self) {
        let Some(control) = self.shell_control.clone() else {
            return;
        };
        if control.version() < MODES_SHELL_VERSION {
            return;
        }
        let mut sent = false;
        for panel in &mut self.panels {
            let Some(wanted) = settings::mode_for(&panel.name) else {
                continue;
            };
            if panel.applied_mode == Some(wanted) {
                continue;
            }
            control.set_output_mode(
                &panel.output,
                wanted.resolution.width,
                wanted.resolution.height,
                wanted.refresh,
            );
            panel.applied_mode = Some(wanted);
            sent = true;
            tracing::debug!(display = %panel.name, ?wanted, "asked for this mode");
        }
        if sent {
            if let Err(err) = self.conn.flush() {
                tracing::warn!(?err, "could not send the mode");
            }
        }
    }

    /// Hand the Settings column the mode lists, and rebuild it if they
    /// changed.
    ///
    /// Separate from [`Shell::refresh_hdr_support`] because the two arrive in
    /// separate events and neither implies the other: a display can change
    /// what it is being driven at without changing what it can do in HDR.
    fn refresh_display_modes(&mut self) {
        let reported: Vec<(String, Vec<settings::Offered>)> = match &self.debug_display_modes {
            // A development aid, for looking at a page that needs a connector
            // this session does not have. See the flag's own documentation.
            Some(injected) => injected.clone(),
            None => self
                .panels
                .iter()
                .map(|panel| (panel.name.clone(), panel.modes.clone()))
                .collect(),
        };
        if settings::note_modes(reported) {
            settings::refresh(&mut self.xmb.categories);
            self.needs_redraw = true;
        }
    }

    /// Hand the Settings column what every display reports, and rebuild it if
    /// that changed.
    ///
    /// Per display rather than folded into one answer: the page lists screens,
    /// and which screens it lists is exactly this. A display plugged in
    /// mid-session appears in it, and one unplugged drops out, without anything
    /// else being told.
    fn refresh_hdr_support(&mut self) {
        let reported: Vec<(String, settings::Support)> = match &self.debug_hdr_displays {
            // A development aid, for looking at a page that needs hardware
            // this machine may not have. See the flag's own documentation.
            Some(injected) => injected.clone(),
            None => self
                .panels
                .iter()
                .map(|panel| (panel.name.clone(), panel.hdr))
                .collect(),
        };
        if settings::note_support(reported) {
            settings::refresh(&mut self.xmb.categories);
            self.needs_redraw = true;
        }
    }

    /// What of display `index` accepts the pointer, given the surface state it
    /// has just been given.
    ///
    /// The board's rectangle is asked for on exactly the condition the drawing
    /// uses — this display is the one being driven, the board is up, and it is
    /// the board rather than the corner hint — so the hole in the input region
    /// is never anywhere but under a keyboard that is actually painted there.
    fn clickable(
        &self,
        index: usize,
        state: (Layer, KeyboardInteractivity),
        keyboard_visible: bool,
    ) -> Clickable {
        // `is_open`, not `is_on_screen`: a board on its way down has already
        // given the keys back, and the clicks go with them. Pressing a key
        // that is halfway off the display would be pressing a keyboard that,
        // as far as the user has been told, is gone.
        let board =
            (index == self.focused_panel && keyboard_visible && self.osk.is_open()).then(|| {
                let panel = &self.panels[index];
                let [x, y, w, h] =
                    ui::keyboard_panel_rect(panel.width.max(16) as f32, panel.height.max(9) as f32);
                [x as i32, y as i32, w.ceil() as i32, h.ceil() as i32]
            });
        clickable_region(state, board)
    }

    fn sync_surface_state(&mut self) {
        let app_running = self.app_running();
        // Whether the board is up at all, not only whether it is up over an
        // application: the shell must let go of the keys either way, or the
        // letters it types come straight back to it and are read as
        // navigation.
        let keyboard = self.keyboard_visible();
        let states: Vec<((Layer, KeyboardInteractivity), Clickable)> = (0..self.panels.len())
            .map(|index| {
                let state = self.guide.surface_state(
                    index == self.focused_panel,
                    app_running,
                    self.keep_keyboard_grabbed,
                    self.launching_on(index),
                    keyboard,
                    self.base_layer,
                );
                (state, self.clickable(index, state, keyboard))
            })
            .collect();
        // Whether each display is currently visible enough for a new frame to
        // carry the change. A just-covered display may still draw one cleanup
        // frame because it was visible previously; commit its layer state now
        // so that frame cannot put the ordinary bar above the application.
        let drawing: Vec<bool> = (0..self.panels.len())
            .map(|index| self.panel_is_visible(index))
            .collect();
        let mut changed = false;

        for ((panel, desired), drawing) in self.panels.iter_mut().zip(states).zip(drawing) {
            if panel.applied_surface_state == Some(desired) {
                continue;
            }
            let ((layer, interactivity), clickable) = desired;

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

            // A surface drawn over an application while declining its keyboard
            // has to decline its pointer too. That is the keyboard overlay and
            // the launch splash: both are things the shell has put in front of
            // something the user is still using, and a fullscreen surface that
            // swallowed every click while passing the keys through would make
            // the application behind it unusable with a mouse for as long as
            // it was up.
            //
            // Except for the board itself, which is a picture of a keyboard
            // and has to be pressable. So the hole in the region is the board's
            // panel and nothing else: keys are clicked, and the rest of the
            // display still belongs to the application underneath.
            match clickable {
                Clickable::Everything => panel.layer.wl_surface().set_input_region(None),
                Clickable::Nothing | Clickable::Board(_) => match Region::new(&self.compositor) {
                    Ok(region) => {
                        if let Clickable::Board([x, y, w, h]) = clickable {
                            region.add(x, y, w, h);
                        }
                        panel
                            .layer
                            .wl_surface()
                            .set_input_region(Some(region.wl_region()));
                    }
                    Err(err) => {
                        tracing::warn!(?err, "could not make the overlay click-through")
                    }
                },
            }
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

/// What the information panel says about `app`.
///
/// One function for both passes — the panel as it opens, before the package
/// manager has said anything, and the panel once it has — so the two cannot
/// disagree about the order of the rows or about what an unknown value looks
/// like. `facts` is `None` while the question is still out.
///
/// Nothing is invented. An application with no `Comment` in its desktop entry
/// gets no description line at all rather than a sentence written here on its
/// behalf, and a value no package manager could give is "Unknown" rather than
/// something derived from the size of the program on disk, which would be wrong
/// by an order of magnitude for anything that ships data alongside it.
fn information_lines(app: &apps::App, facts: Option<&appinfo::Facts>) -> Vec<dialog::Line> {
    let known = |value: Option<String>| match (facts, value) {
        (None, _) => "Reading…".to_string(),
        (Some(_), Some(value)) => value,
        (Some(_), None) => "Unknown".to_string(),
    };
    let mut lines = vec![dialog::Line::Heading(app.name.clone())];
    if let Some(comment) = app.comment.as_deref() {
        lines.push(dialog::Line::Note(comment.to_string()));
    }
    lines.push(dialog::Line::Rule);
    lines.push(dialog::Line::field(
        "Version",
        known(facts.and_then(|facts| facts.version.clone())),
    ));
    lines.push(dialog::Line::field(
        "Size",
        known(facts.and_then(|facts| facts.size).map(appinfo::human_size)),
    ));
    lines.push(dialog::Line::Rule);
    lines
}

/// Whether the on-screen keyboard, or the corner hint that stands in for it,
/// is on screen.
///
/// The board asks nothing of what is running: it can be summoned over an
/// application, or over the bar with nothing running at all. That is the point
/// of a manual shortcut — the applications that most need one are exactly the
/// ones the shell cannot tell have a text field, so it does not try to guess.
///
/// The hint is the other way round. It says how to reach a keyboard *for a
/// text field*, and a text field belongs to an application, so with nothing
/// running there is nothing for it to point at and nothing for it to be on top
/// of.
///
/// The menu overrides both. It is a different screen, and a keyboard on top of
/// it would be typing into the shell.
fn keyboard_is_visible(
    menu_over_app: bool,
    board_open: bool,
    hint_wanted: bool,
    app_running: bool,
    typing_here: bool,
) -> bool {
    // A board typing into the shell overrides all of it. The rule below exists
    // because a keyboard on top of the shell's own screens would be typing into
    // the shell — which was always wrong, until the shell grew the one field
    // that is its own. Where the letters are going is the whole question, and
    // here the answer is "here".
    if typing_here {
        return board_open;
    }
    !menu_over_app && (board_open || (hint_wanted && app_running))
}

/// Whether a display is on screen for the on-screen keyboard and nothing else.
///
/// The bar and the board share one surface, and they want opposite things of
/// it: the bar belongs behind the running application, the board on top of it.
/// While the board has raised that surface, the bar therefore cannot be drawn
/// on it — doing so laid the whole start screen, icons and all, over the
/// application beside the keyboard.
///
/// The menu is the exception, because the menu raises the surface for its own
/// reasons and is meant to be seen.
fn draws_only_the_keyboard(
    focused: bool,
    keyboard_overlay: bool,
    menu_here: bool,
    typing_here: bool,
) -> bool {
    // And the board's own field is the other exception, for the mirror of that
    // reason: what the board is typing into is a panel this surface is drawing,
    // so blanking everything but the keys would take the field away.
    focused && keyboard_overlay && !menu_here && !typing_here
}

/// The name an application gives itself, made presentable.
///
/// An `app_id` is written for a machine to match on, so the parts of it that
/// are not the name are dropped: `org.mozilla.firefox` is a reverse-DNS
/// spelling of Firefox, and what is left after the last dot is the only part
/// of it anyone recognises. The first letter is raised because a label sits
/// beside Resume and Dashboard and would otherwise read as a command typed
/// into a terminal.
///
/// Only reached when nothing installed claims the window — a game started
/// through Steam, an Android application under Waydroid, anything whose desktop
/// entry is not on this machine. `None` when the window named itself nothing at
/// all, which is the caller's cue that there is no name to be had here.
fn app_id_name(app_id: &str) -> Option<String> {
    let name = app_id.trim().rsplit('.').next()?.trim();
    let mut letters = name.chars();
    let first = letters.next()?;
    Some(format!("{}{}", first.to_uppercase(), letters.as_str()))
}

/// Whether the right stick is aiming the pointer, as a rule on its own.
///
/// `turned_on` is the switch: an application is in front, it said what it is,
/// and the user turned the pointer on for it. The other two are the shell's
/// own screens being in the way — the menu drawn over the application, and a
/// launch splash still waiting for one. Neither has anything to point at.
fn pointer_aims(menu_over_app: bool, launching: bool, turned_on: bool) -> bool {
    turned_on && !menu_over_app && !launching
}

/// Whether the buttons and the wheel are the pointer's too.
///
/// Everything aiming needs, and the on-screen keyboard out of the way. The
/// board is driven with the D-pad, the left stick and `A` — exactly what the
/// clicks and the scrolling would take — while the right stick means nothing
/// to it at all. So the two halves part company for as long as it is up:
/// aiming carries on underneath, and the pointer is still where it was left
/// when the board goes away.
fn pointer_clicks(aiming: bool, board_open: bool) -> bool {
    aiming && !board_open
}

/// Whether a poll carries a press — a hand on the pad rather than on a mouse.
///
/// Everything that went *down*, whether or not the shell is the one that acts
/// on it: with an application in front the buttons and the D-pad are dropped
/// here and read straight from `/dev/input` by the game instead, and a thumb on
/// them is still a thumb that is not on a mouse. Releases say nothing — a
/// button coming back up is the end of a press already counted, and the last
/// thing a pad reports before it is put down.
fn controller_pressed_something(poll: &controller::Poll) -> bool {
    !poll.actions.is_empty()
        || poll.clicks.iter().any(|(_, down)| *down)
        || poll.arrows.iter().any(|(_, down)| *down)
}

/// Whether the shell should act on more from a controller than the two
/// controls that always reach it.
///
/// Holding the keyboard is the usual answer, and the on-screen keyboard is the
/// case where it is the wrong one: the board is up and being driven precisely
/// *because* it has left the Wayland keyboard with the application it types
/// into. Asking about focus alone dropped every direction and every press for
/// as long as the board was on screen.
fn controller_is_driving(grabbed: bool, has_keyboard_focus: bool, board_open: bool) -> bool {
    grabbed || has_keyboard_focus || board_open
}

/// Whether a surface in this state lets the pointer through to whatever is
/// underneath it.
///
/// Exactly the states that put the shell over a running application without
/// taking its keyboard — the on-screen keyboard, its corner hint, and the
/// launch splash. Derived from the pair rather than tracked beside it so the
/// two can never disagree: a surface that has declined the keys has, by that
/// fact, declined to be what the user is interacting with.
fn passes_pointer_through(state: (Layer, KeyboardInteractivity)) -> bool {
    state == (Layer::Overlay, KeyboardInteractivity::None)
}

/// What of one of the shell's surfaces accepts the pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Clickable {
    /// The whole of it, which is what a menu or the bar wants: the shell is
    /// what the user is interacting with, and nothing is behind it that a
    /// click was meant for.
    Everything,
    /// None of it. The shell is drawn over an application it has deliberately
    /// left the keyboard with, so the clicks are the application's too.
    Nothing,
    /// One rectangle of it, in surface coordinates: the on-screen keyboard.
    Board([i32; 4]),
}

/// Which of those a surface in `state` is, given the board's rectangle when
/// this display is the one drawing it.
///
/// Split out from the surface state for the same reason that is split out from
/// the drawing: what the shell tells the compositor and what the shell paints
/// have to be two readings of one answer, not two answers.
fn clickable_region(state: (Layer, KeyboardInteractivity), board: Option<[i32; 4]>) -> Clickable {
    if !passes_pointer_through(state) {
        return Clickable::Everything;
    }
    match board {
        Some(rect) => Clickable::Board(rect),
        None => Clickable::Nothing,
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
    card_age >= CARD_ARRIVAL
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
        let (next, next_speed) = lxb_protocol::overview::spring(
            *at as f64,
            *speed as f64,
            *to as f64,
            lxb_protocol::overview::CARD_SPRING,
            dt as f64,
        );
        (*at, *speed) = (next as f32, next_speed as f32);
    }
    glide.at
}

/// The displays either side of display `index`, out of `count` of them.
///
/// Neither end wraps, and that is the whole of the rule the guide menu's two
/// move rows are drawn from: on the first display there is nothing before it,
/// on the last there is nothing after it, and on a session with one display
/// there is neither. A row with nothing behind it is drawn as an outline and
/// the highlight steps over it.
///
/// Wrapping was considered and is wrong here. Everything else in this shell
/// wraps — the bar's columns, the menu's rows — because those are rings the
/// user is walking round. A row of displays is not: the user can see where the
/// screens are, "next" past the last one would put the window on the screen at
/// the far end of the desk, and a move is not something to be surprised by.
fn neighbours(index: usize, count: usize) -> (Option<usize>, Option<usize>) {
    if index >= count {
        return (None, None);
    }
    (
        index.checked_sub(1),
        Some(index + 1).filter(|next| *next < count),
    )
}

/// The local time now, broken down, or `None` when it cannot be worked out —
/// in which case the shell shows no clock rather than a wrong one.
fn local_time() -> Option<libc::tm> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    let secs = now.as_secs().min(libc::time_t::MAX as u64) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: localtime_r only reads `secs` and writes `tm`, both valid here.
    if unsafe { libc::localtime_r(&secs, &mut tm) }.is_null() {
        return None;
    }
    Some(tm)
}

/// The local wall clock, in the bar's corner format (`6/12 0:40`).
fn wall_clock() -> Option<String> {
    let tm = local_time()?;
    Some(format!(
        "{}/{} {}:{:02}",
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min
    ))
}

/// The guide sidebar's header: the time, and the day beside it.
fn wall_clock_face() -> Option<(String, String)> {
    local_time().as_ref().map(clock_face)
}

const WEEKDAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// Written out here rather than handed to `strftime`, which would answer in
/// whatever locale the session happened to inherit while every other word in
/// this shell is in English. A header reading "wtorek, 5 sierpnia" over
/// "Nothing is running" looks like a bug, not like localisation — the day to
/// translate this is the day the rest of it is translated too.
///
/// Both names are cut to three letters, which is what lets the day share the
/// clock's line instead of taking one of its own. The sidebar's width is
/// clamped while its type scales with the display, so the room beside the time
/// is at its narrowest on a big screen — and "Wednesday, 28 September" printed
/// straight through the time there. Three letters is the one form that fits at
/// every size, and a header is a glance rather than a sentence.
fn clock_face(tm: &libc::tm) -> (String, String) {
    let short = |name: &str| name.chars().take(3).collect::<String>();
    let weekday = WEEKDAYS
        .get(tm.tm_wday.clamp(0, 6) as usize)
        .copied()
        .unwrap_or_default();
    let month = MONTHS
        .get(tm.tm_mon.clamp(0, 11) as usize)
        .copied()
        .unwrap_or_default();
    (
        format!("{}:{:02}", tm.tm_hour, tm.tm_min),
        format!("{} {} {}", short(weekday), tm.tm_mday, short(month)),
    )
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
        "keyboard" => Action::Keyboard,
        "menu" => Action::Menu,
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
        // The keyboard equivalents of a controller's guide button. LineXinBar
        // also forwards its own binding, which is what works while an
        // application holds the keyboard and this shell sees nothing.
        //
        // The Menu key is one of them and stays one of them. It reads as the
        // context menu's key on a desktop, and it was briefly given to that —
        // which was a mistake: with Steam running, the pad's own guide button
        // is Steam's before it is ever the shell's, and this key is then the
        // only thing that opens the guide without Big Picture coming up with
        // it. A shortcut nobody can take away is worth more here than a
        // shortcut in the right place.
        Keysym::Home | Keysym::XF86_HomePage | Keysym::Menu => Some(Action::Guide),
        // The keyboard equivalent of the top face button. `Shift+F10` arrives
        // as plain F10 — the other chord a desktop spells the context menu
        // with — and `y` is the letter printed on the button itself, which is
        // the same kind of shorthand as the WASD and HJKL above.
        Keysym::F10 | Keysym::y | Keysym::Y => Some(Action::Menu),
        // And of the controller chord that summons the keyboard. Of little use
        // to somebody who already has a keyboard, but LineXinBar forwards its
        // own binding as this, and it is what makes the board drivable at all
        // on a machine with no pad plugged in.
        Keysym::XF86_Keyboard => Some(Action::Keyboard),
        _ => None,
    }
}

/// Which control a menu bar drives. The guide names the row; the system module
/// names the thing that moves.
fn knob(bar: guide::Bar) -> Knob {
    match bar {
        guide::Bar::Volume => Knob::Volume,
        guide::Bar::Brightness => Knob::Brightness,
    }
}

/// Adapts the renderer's atlas lookup to what the layout code needs.
struct Slots<'a>(&'a Gpu);

impl SlotLookup for Slots<'_> {
    fn slot_for(&self, icon: Option<&str>) -> Option<u32> {
        icon.and_then(|name| self.0.slot(name))
            .or_else(|| self.0.slot(FALLBACK_APP_ICON))
    }

    fn glyph(&self, name: &str) -> Option<u32> {
        self.0.slot(name)
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
        if capability == Capability::Pointer && self.pointer.is_none() {
            match self.seat_state.get_pointer(qh, &seat) {
                Ok(pointer) => self.pointer = Some(pointer),
                // Not fatal, and not even unusual: a console with no mouse in
                // it has a seat with no pointer on it. Everything but clicking
                // the on-screen keyboard carries on.
                Err(err) => tracing::info!(?err, "no pointer on this seat"),
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
        if capability == Capability::Pointer {
            if let Some(pointer) = self.pointer.take() {
                pointer.release();
            }
        }
    }

    fn remove_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: wl_seat::WlSeat) {
    }
}

impl PointerHandler for Shell {
    /// The pointer, over the one part of the shell that accepts it.
    ///
    /// Only the on-screen keyboard has an input region at all — see
    /// `sync_surface_state` — so anything arriving here is over the board, and
    /// what is under the cursor is simply the key the cursor is on.
    fn pointer_frame(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        pointer: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            let Some(index) = self.panels.iter().position(|p| p.owns(&event.surface)) else {
                continue;
            };
            let (width, height) = {
                let panel = &self.panels[index];
                (panel.width.max(16) as f32, panel.height.max(9) as f32)
            };
            let (x, y) = (event.position.0 as f32, event.position.1 as f32);
            let key = ui::keyboard_key_at(x, y, width, height);

            match event.kind {
                PointerEventKind::Enter { serial } => {
                    self.set_cursor_shape(qh, pointer, serial);
                    self.hover_key(key);
                }
                PointerEventKind::Motion { .. } => self.hover_key(key),
                // A press rather than a release, because that is when a key on
                // a keyboard fires and when the board's own `A` fires. The key
                // pressed is whatever the hover just put the selection on, so
                // the button and the controller press exactly the same thing.
                PointerEventKind::Press { button, .. } if button == BTN_LEFT && key.is_some() => {
                    self.hover_key(key);
                    self.press_key();
                }
                _ => {}
            }
        }
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

impl Dispatch<LxbShellV1, ()> for Shell {
    fn event(
        state: &mut Self,
        _proxy: &LxbShellV1,
        event: lxb_shell_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            lxb_shell_v1::Event::Guide => {
                tracing::debug!("guide binding forwarded by the compositor");
                state.on_action(Action::Guide);
            }
            lxb_shell_v1::Event::Keyboard => {
                tracing::debug!("keyboard binding forwarded by the compositor");
                state.on_action(Action::Keyboard);
            }
            lxb_shell_v1::Event::Foreground { title } => {
                state.foreground = (!title.is_empty()).then_some(title);
                tracing::debug!(foreground = ?state.foreground, "foreground application changed");
                // The menu offers different entries with and without an
                // application, so it has to be redrawn.
                state.needs_redraw = true;
            }
            lxb_shell_v1::Event::OutputForeground { output, title } => {
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
            lxb_shell_v1::Event::OutputAppId { output, app_id } => {
                let app_id = (!app_id.is_empty()).then_some(app_id);
                if let Some(panel) = state.panels.iter_mut().find(|p| p.output == output) {
                    if panel.app_id != app_id {
                        tracing::debug!(
                            display = %panel.name,
                            app_id = ?app_id,
                            "foreground application identity changed on a display"
                        );
                        panel.app_id = app_id;
                        // The tile is drawn from this, and so is whether the
                        // stick is driving anything: an application closing is
                        // what turns the pointer back into a stick.
                        state.needs_redraw = true;
                    }
                }
            }
            lxb_shell_v1::Event::WindowCaptured { id, path } => {
                let saved = (!path.is_empty()).then(|| PathBuf::from(path));
                tracing::info!(id, ?saved, "the compositor answered a screenshot");
                // Nothing pending means nothing asked: another shell's
                // screenshot, or one this shell gave up on. Either way there is
                // no control left for a panel to grow out of.
                if let Some((from, app)) = state.pending_capture.take() {
                    state.report_screenshot(from, app, saved);
                }
            }
            lxb_shell_v1::Event::OutputHdr {
                output,
                supported,
                enabled,
                max_luminance,
            } => {
                if let Some(panel) = state.panels.iter_mut().find(|p| p.output == output) {
                    let reported = settings::Support {
                        available: supported != 0,
                        active: enabled != 0,
                        peak: max_luminance.min(u16::MAX as u32) as u16,
                        // Not carried by this event; it arrives in
                        // output_hdr_controls, and until it does a capable
                        // display is assumed to be able to do all of it —
                        // which is what a shell talking to a compositor too
                        // old to say otherwise has to assume anyway.
                        gamut: panel.hdr.gamut,
                    };
                    if panel.hdr != reported {
                        tracing::info!(
                            display = %panel.name,
                            supported = reported.available,
                            enabled = reported.active,
                            peak_nits = reported.peak,
                            "high dynamic range on a display"
                        );
                        panel.hdr = reported;
                        state.refresh_hdr_support();
                    }
                }
            }
            lxb_shell_v1::Event::OutputHdrControls { output, controls } => {
                let gamut = controls
                    .into_result()
                    .is_ok_and(|controls| controls.contains(lxb_shell_v1::HdrControl::Gamut));
                if let Some(panel) = state.panels.iter_mut().find(|p| p.output == output) {
                    if panel.hdr.gamut != gamut {
                        tracing::info!(
                            display = %panel.name,
                            gamut,
                            "which HDR settings this display can honour"
                        );
                        panel.hdr.gamut = gamut;
                        state.refresh_hdr_support();
                    }
                }
            }
            lxb_shell_v1::Event::OutputMode {
                output,
                width,
                height,
                refresh,
                flags,
            } => {
                if let Some(panel) = state.panels.iter_mut().find(|p| p.output == output) {
                    let flags = flags
                        .into_result()
                        .unwrap_or(lxb_shell_v1::ModeFlag::empty());
                    panel.pending_modes.push(settings::Offered {
                        mode: settings::Mode {
                            resolution: settings::Resolution { width, height },
                            refresh,
                        },
                        current: flags.contains(lxb_shell_v1::ModeFlag::Current),
                        preferred: flags.contains(lxb_shell_v1::ModeFlag::Preferred),
                    });
                }
            }
            lxb_shell_v1::Event::OutputModesDone { output } => {
                if let Some(panel) = state.panels.iter_mut().find(|p| p.output == output) {
                    let modes = std::mem::take(&mut panel.pending_modes);
                    if panel.modes != modes {
                        tracing::debug!(
                            display = %panel.name,
                            count = modes.len(),
                            running = ?modes.iter().find(|offered| offered.current).map(|offered| offered.mode),
                            "what a display can be driven at changed"
                        );
                        panel.modes = modes;
                        state.refresh_display_modes();
                    }
                }
            }
            lxb_shell_v1::Event::OutputWindow {
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
                        app_id: String::new(),
                        width,
                        height,
                    });
                }
            }
            // Attaches to the window it followed. Named by id rather than by
            // position so that a compositor which one day sends these in some
            // other order still lands them on the right window.
            lxb_shell_v1::Event::OutputWindowAppId { output, id, app_id } => {
                if let Some(panel) = state.panels.iter_mut().find(|p| p.output == output) {
                    if let Some(window) = panel
                        .pending_windows
                        .iter_mut()
                        .find(|window| window.id == id)
                    {
                        window.app_id = app_id;
                    }
                }
            }
            lxb_shell_v1::Event::OutputWindowsDone { output } => {
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
        assert!(cards_have_landed(CARD_ARRIVAL));

        // The corners are repaired on the same answer the decoration fades in
        // on, so they can never round in plain sight: nothing is on the card
        // to see them do it.
        assert_eq!(ui::card_fade(CARD_ARRIVAL), 0.0);
    }

    /// The bug in its third form, and the one that could be watched: the frame
    /// and title of a card drawn at the slot its window was still travelling
    /// to, ruled across the middle of that window for the rest of the flight.
    #[test]
    fn no_card_is_decorated_while_its_window_is_still_flying() {
        for age in [0.0, HOME_FLIGHT * 0.4, HOME_FLIGHT * 0.9, HOME_FLIGHT] {
            assert_eq!(
                ui::card_fade(age),
                0.0,
                "the cards must stay bare {age}s into a {HOME_FLIGHT}s flight"
            );
        }
        // And they are up promptly once they have: the decoration settles onto
        // a card that has stopped rather than being an animation of its own.
        assert!(ui::card_fade(CARD_ARRIVAL + 0.1) > 0.5);
        assert_eq!(ui::card_fade(CARD_ARRIVAL + 0.2), 1.0);
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
            app_id: String::new(),
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

    /// The keyboard and the bar share a surface and want opposite things of
    /// it. Raising it for the board and then drawing the bar on it too put the
    /// whole start screen over the running application — every icon of it,
    /// beside the keys.
    #[test]
    fn the_bar_gives_the_screen_up_while_the_keyboard_is_on_it() {
        const DRIVEN: bool = true;
        const BOARD: bool = true;
        const MENU: bool = true;
        const HERE: bool = true;

        assert!(draws_only_the_keyboard(DRIVEN, BOARD, !MENU, !HERE));
        // The menu raises the surface for its own reasons and is meant to be
        // seen, so it keeps drawing everything it draws.
        assert!(!draws_only_the_keyboard(DRIVEN, BOARD, MENU, !HERE));
        // Nothing is being kept off a display that has no board on it.
        assert!(!draws_only_the_keyboard(DRIVEN, !BOARD, !MENU, !HERE));
        // Nor off the displays the user is not driving: the board is on one
        // screen, and the others carry on showing their own bar.
        assert!(!draws_only_the_keyboard(!DRIVEN, BOARD, !MENU, !HERE));
        // And a board typing into the shell's own password field is the whole
        // exception: what it types into is a panel on this very surface, so
        // blanking everything but the keys would take the field away.
        assert!(!draws_only_the_keyboard(DRIVEN, BOARD, !MENU, HERE));
    }

    /// The one hole the shell ever opens for the pointer is the board, and it
    /// is exactly the board: a fullscreen surface that swallowed clicks would
    /// make the application under it unusable with a mouse, and one that
    /// swallowed none of them would leave a picture of a keyboard that cannot
    /// be pressed.
    #[test]
    fn only_the_keyboard_takes_clicks_from_the_application_underneath() {
        let board = [420, 700, 1080, 340];
        let over_app = (Layer::Overlay, KeyboardInteractivity::None);

        assert_eq!(
            clickable_region(over_app, Some(board)),
            Clickable::Board(board)
        );
        // The launch splash and the corner hint are the same surface state
        // with no board on it, and they take nothing.
        assert_eq!(clickable_region(over_app, None), Clickable::Nothing);

        // The menu is the shell being what the user is interacting with, so
        // all of it takes clicks — and the board's rectangle is beside the
        // point there, because the menu never has one on it.
        for state in [
            (Layer::Overlay, KeyboardInteractivity::Exclusive),
            (Layer::Background, KeyboardInteractivity::Exclusive),
            (Layer::Background, KeyboardInteractivity::OnDemand),
        ] {
            assert_eq!(clickable_region(state, None), Clickable::Everything);
            assert_eq!(clickable_region(state, Some(board)), Clickable::Everything);
        }
    }

    /// The board can be summoned anywhere; the hint appears only where there
    /// is a text field waiting for it.
    #[test]
    fn the_board_goes_anywhere_and_the_hint_only_over_a_field() {
        const MENU: bool = true;
        const BOARD: bool = true;
        const HINT: bool = true;
        const APP: bool = true;
        const HERE: bool = true;

        // Summoned over an application, and summoned over the bar with nothing
        // running: both are the user asking for it, and neither is refused.
        assert!(keyboard_is_visible(!MENU, BOARD, !HINT, APP, !HERE));
        assert!(
            keyboard_is_visible(!MENU, BOARD, !HINT, !APP, !HERE),
            "a shortcut that silently does nothing on the home screen is worse \
             than one that visibly does nothing"
        );

        // The hint needs both halves: a field waiting for typing, and an
        // application for it to be drawn on top of.
        assert!(keyboard_is_visible(!MENU, !BOARD, HINT, APP, !HERE));
        assert!(!keyboard_is_visible(!MENU, !BOARD, HINT, !APP, !HERE));
        assert!(!keyboard_is_visible(!MENU, !BOARD, !HINT, APP, !HERE));

        // And the menu takes precedence over either: it is a different screen,
        // and a board on top of it would be typing into the shell.
        assert!(!keyboard_is_visible(MENU, BOARD, HINT, APP, !HERE));

        // Unless the shell is the thing being typed into. A password field is
        // the shell asking for letters, so the board is drawn over the guide,
        // over an application, over anything — and the hint, which is about
        // reaching a keyboard for an *application's* field, has nothing to say.
        assert!(keyboard_is_visible(MENU, BOARD, HINT, APP, HERE));
        assert!(keyboard_is_visible(MENU, BOARD, !HINT, !APP, HERE));
        assert!(!keyboard_is_visible(!MENU, !BOARD, HINT, APP, HERE));
    }

    /// The regression that made the keyboard look like a picture of a
    /// keyboard: it was on screen, and every direction and press from the
    /// controller was being dropped before it got there.
    /// The stick is a mouse only where there is something to point at, and
    /// only the half of it that nothing else wants stays behind the board.
    #[test]
    fn the_pointer_keeps_aiming_under_the_board_but_gives_up_its_buttons() {
        const OFF: bool = false;
        const ON: bool = true;

        // Nothing turned on: nothing happens, whatever else is true.
        assert!(!pointer_aims(OFF, OFF, OFF));
        assert!(!pointer_clicks(pointer_aims(OFF, OFF, OFF), OFF));

        // Turned on, with the application in front: the whole mouse.
        let aiming = pointer_aims(OFF, OFF, ON);
        assert!(aiming);
        assert!(pointer_clicks(aiming, OFF));

        // The board is up: still aiming, no longer clicking. It is driven
        // with the D-pad, the left stick and `A`, and the right stick means
        // nothing to it.
        assert!(pointer_aims(OFF, OFF, ON));
        assert!(!pointer_clicks(pointer_aims(OFF, OFF, ON), ON));

        // The menu is over the application, or a launch is still in flight:
        // the shell's own screens, with nothing on them to point at.
        for (menu, launching) in [(ON, OFF), (OFF, ON), (ON, ON)] {
            assert!(!pointer_aims(menu, launching, ON), "{menu} {launching}");
            assert!(!pointer_clicks(pointer_aims(menu, launching, ON), OFF));
        }
    }

    /// The cursor goes away when the user picks the pad up, and the pad is
    /// picked up in more ways than the shell itself hears about.
    #[test]
    fn every_kind_of_controller_press_puts_the_cursor_away() {
        let poll = |actions: Vec<Action>, clicks: Vec<(u32, bool)>, arrows: Vec<(u32, bool)>| {
            controller::Poll {
                actions,
                right_stick: (0.0, 0.0),
                scroll_stick: (0.0, 0.0),
                clicks,
                arrows,
            }
        };

        // A pad nobody is touching. This is most polls, thirty times a second.
        assert!(!controller_pressed_something(&poll(
            Vec::new(),
            Vec::new(),
            Vec::new()
        )));

        // Navigating the bar.
        assert!(controller_pressed_something(&poll(
            vec![Action::Down],
            Vec::new(),
            Vec::new()
        )));
        // And the presses the shell drops because an application is in front:
        // the game is reading them, and the hand is on the pad either way.
        assert!(controller_pressed_something(&poll(
            Vec::new(),
            vec![(0x110, true)],
            Vec::new()
        )));
        assert!(controller_pressed_something(&poll(
            Vec::new(),
            Vec::new(),
            vec![(103, true)]
        )));

        // A release is the end of a press already counted. Letting it through
        // would hide the cursor once more on the way back up, which is the one
        // moment the user may be reaching for the mouse again.
        assert!(!controller_pressed_something(&poll(
            Vec::new(),
            vec![(0x110, false)],
            vec![(103, false)]
        )));
    }

    #[test]
    fn the_controller_drives_the_board_that_has_left_it_the_keyboard() {
        const GRABBED: bool = true;
        const FOCUSED: bool = true;
        const BOARD: bool = true;

        // The case that was broken. The board holds no Wayland keyboard on
        // purpose — that is what keeps the text field it types into focused —
        // so focus is exactly the wrong thing to ask about here.
        assert!(controller_is_driving(!GRABBED, !FOCUSED, BOARD));

        // Unchanged either way round: the bar is driven when it holds the
        // keyboard, or when it was told to keep hold of it.
        assert!(controller_is_driving(!GRABBED, FOCUSED, !BOARD));
        assert!(controller_is_driving(GRABBED, !FOCUSED, !BOARD));

        // And with no board and no focus the shell is behind an application,
        // where only the guide button and the keyboard chord reach it.
        assert!(!controller_is_driving(!GRABBED, !FOCUSED, !BOARD));
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

    /// A `tm` for a given local moment, so the header can be asserted on
    /// without waiting for the clock to say the right thing.
    fn at(hour: i32, minute: i32, wday: i32, mday: i32, mon: i32) -> libc::tm {
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        tm.tm_hour = hour;
        tm.tm_min = minute;
        tm.tm_wday = wday;
        tm.tm_mday = mday;
        tm.tm_mon = mon;
        tm
    }

    /// Which of the guide menu's two move rows can be pressed, on a session
    /// with one display, two, and three.
    #[test]
    fn a_window_is_never_moved_off_either_end_of_the_row_of_displays() {
        assert_eq!(neighbours(0, 1), (None, None), "nowhere to send it");

        assert_eq!(neighbours(0, 2), (None, Some(1)));
        assert_eq!(neighbours(1, 2), (Some(0), None));

        assert_eq!(neighbours(0, 3), (None, Some(1)), "the first has no before");
        assert_eq!(neighbours(1, 3), (Some(0), Some(2)), "the middle has both");
        assert_eq!(neighbours(2, 3), (Some(1), None), "the last has no after");

        // A display unplugged between the menu being drawn and a row being
        // chosen leaves the focused index past the end of the list.
        assert_eq!(neighbours(3, 2), (None, None));
        assert_eq!(neighbours(0, 0), (None, None));
    }

    #[test]
    fn the_sidebars_header_reads_as_a_time_and_a_day() {
        let (time, date) = clock_face(&at(15, 18, 2, 5, 7));
        assert_eq!(time, "15:18");
        assert_eq!(date, "Tue 5 Aug");

        // Midnight is `0:04`, not `00:04` — the corner clock has always
        // written it that way and the header should not disagree with it.
        let (time, date) = clock_face(&at(0, 4, 0, 1, 0));
        assert_eq!(time, "0:04");
        assert_eq!(date, "Sun 1 Jan");

        // The longest the day can get. It shares the clock's line, so this is
        // the string the sidebar has to have room for beside the time.
        assert_eq!(clock_face(&at(9, 0, 3, 28, 8)).1, "Wed 28 Sep");

        // Out-of-range fields come from a `tm` this did not fill in; a header
        // is not worth a panic over one.
        let (time, _) = clock_face(&at(23, 59, 9, 31, 40));
        assert_eq!(time, "23:59");
    }

    /// The fallback for a window nothing installed claims. It is still the
    /// application being named, so what is drawn is the recognisable part of
    /// the name the window gave itself — not the reverse-DNS wrapping around
    /// it, and not the window's title.
    #[test]
    fn an_unknown_window_is_named_by_what_it_calls_itself() {
        assert_eq!(app_id_name("steam"), Some("Steam".to_string()));
        assert_eq!(app_id_name("org.mozilla.firefox"), Some("Firefox".into()));
        assert_eq!(app_id_name("Waydroid"), Some("Waydroid".into()));

        // A window that named itself nothing has no name to give, and the
        // caller falls back to the only other thing it knows.
        assert_eq!(app_id_name(""), None);
        assert_eq!(app_id_name("  "), None);
    }

    #[test]
    fn debug_actions_parse_as_a_timed_script() {
        let cli = Cli::try_parse_from(["lxb-desktop", "--debug-actions", "2:guide,3.5:right"])
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
        let cli = Cli::try_parse_from(["lxb-desktop", "--grab-keyboard", "--no-gamepad"])
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

    /// The context menu never takes a key off the guide.
    ///
    /// The Menu key was given to it once and taken straight back: with Steam
    /// running, the pad's guide button is Steam's before it is the shell's, and
    /// a keyboard key that opens the guide without Big Picture coming up with
    /// it is the way out of that. Nothing about a context menu is worth a way
    /// out of a fullscreen application.
    #[test]
    fn the_context_menu_takes_no_key_the_guide_answers() {
        for keysym in [Keysym::F10, Keysym::y, Keysym::Y] {
            assert_eq!(action_for_keysym(keysym), Some(Action::Menu));
        }
        for keysym in [Keysym::Home, Keysym::XF86_HomePage, Keysym::Menu] {
            assert_ne!(action_for_keysym(keysym), Some(Action::Menu));
        }
    }
}
