//! LineXinBar Desktop — a cross-media-bar style shell for the LineXinBar compositor.
//!
//! It is an ordinary Wayland client that binds `zwlr_layer_shell_v1`, so it
//! runs on LineXinBar as well as any other compositor implementing layer-shell.
//! That also makes it debuggable on its own, without starting a compositor.

mod appinfo;
mod apps;
mod art;
mod controller;
mod dialog;
mod gpu;
mod guide;
mod icons;
mod keyboard;
mod launch;
mod media;
mod menu;
mod model;
mod notify;
mod pointer;
mod polkit;
mod screenshot;
mod secret;
mod settings;
mod sound;
mod steam;
mod steam_hid;
mod sun;
mod system;
mod theme;
mod thumbs;
mod trash;
mod ui;
mod uninstall;
mod wallpaper_clock;

use std::collections::{HashMap, HashSet};
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
use smithay_client_toolkit::reexports::protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1 as shape;
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::keyboard::{
    KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers, RepeatInfo,
};
use smithay_client_toolkit::seat::pointer::{
    cursor_shape::CursorShapeManager, AxisScroll, PointerEvent, PointerEventKind, PointerHandler,
    BTN_LEFT, BTN_RIGHT,
};
use smithay_client_toolkit::seat::touch::TouchHandler;
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
    LayerSurfaceConfigure, SurfaceKind,
};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::{delegate_registry, registry_handlers};
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_surface, wl_touch};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};

use crate::apps::Entry;
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
/// one.
///
/// Below it the guide's menu still draws both rows and both refuse: neither is
/// something a shell can do for itself. The window's place in the layout
/// belongs to the compositor, and so do its pixels.
const WINDOW_MOVE_AND_CAPTURE_VERSION: u32 = 15;

/// First version that says which way up each display's picture is drawn, and
/// can be asked to turn it.
///
/// Below it no display reports an orientation, so Settings > Display >
/// Orientation says there is nothing to turn — which is the truth on a
/// compositor that cannot be asked.
const TRANSFORM_SHELL_VERSION: u32 = 16;

/// First version that will photograph a whole display, and that forwards the
/// screenshot key.
///
/// Below it the key is never heard of and there is no request to answer it
/// with: photographing a screen means reading every client on it, which no
/// client may do.
const SCREENSHOT_SHELL_VERSION: u32 = 17;

/// First version that carries the desktop portal's question — may this
/// application see a display, and which one.
///
/// The shell is the only part of the session that can draw, so it is the only
/// part that can ask; the portal is a client like any other and the compositor
/// is what carries the question between them.
const SHARE_SHELL_VERSION: u32 = 18;

/// First version that will run an application without ever showing it.
///
/// Below it Valve's client cannot be driven out of sight, and the session is
/// honest about that rather than half-hiding it: the client is left to put its
/// own windows wherever it likes, which is what every version of this shell
/// did until now.
const OUT_OF_SIGHT_SHELL_VERSION: u32 = 19;

/// First version that will warm a display's picture, and that says which
/// displays have a colour ramp to warm. The highest this shell asks for.
///
/// Below it Settings > Display > Night light still draws its page and still
/// remembers what it was set to — the file it is written to is read by
/// whichever compositor comes next — but nothing is sent and no display reports
/// being able to do it, so the page says there is nothing here to warm. Which
/// is the truth on a compositor that cannot be asked.
const NIGHT_LIGHT_SHELL_VERSION: u32 = 20;

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

/// How long one game's picture takes to give the display over to the next, in
/// seconds.
///
/// Slower than anything the cursor does, and deliberately. The bar's own moves
/// are a fifth of a second because they answer a button and have to feel like
/// the button; this is the *room* changing around them, and a room that
/// changed as briskly as a selection would read as a flicker behind the thing
/// the user is actually moving. Slow enough to be a dissolve, short enough
/// that walking down a library is not a slideshow running a step behind.
const SCENERY_FADE: f32 = 0.45;

/// How long a cover takes to gain its colour, or to lose it, in seconds.
///
/// Quicker than the picture behind the bar, because this one is an *answer*:
/// the download the user has been watching has finished, and the cover going
/// from grey to colour under a highlight that has not moved is the shell
/// saying so. Slow enough to be seen happening, which is the whole of the
/// message — a cover that was already in colour on the next frame would leave
/// nothing to notice.
const COLOUR_FADE: f32 = 0.35;

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

    /// Leave Steam out of this session entirely.
    ///
    /// The Games column loses its Steam row, no stored session is read, and
    /// nothing in this process talks to Steam. For a machine where somebody
    /// else's account is signed in, and for a session that should make no
    /// network connections at all.
    #[arg(long, action = clap::ArgAction::SetTrue)]
    no_steam: std::primitive::bool,

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

    /// Pretend this account was signed in to Steam and owned these games, as a
    /// comma-separated list of `name[:installed]`
    /// (`--debug-steam-library 'A Game:1,Another Game'`).
    ///
    /// Development aid, and needed for the reason the two above are: the Steam
    /// column is built out of somebody's library, so it cannot be looked at —
    /// or screenshotted — without an account, a password and a network. These
    /// rows are invented and are marked as such in the log; nothing is asked
    /// of Steam for them and nothing they name can be started.
    #[arg(long, hide = true, value_delimiter = ',', value_parser = parse_debug_game)]
    debug_steam_library: Vec<(String, std::primitive::bool)>,
}

/// `name[:installed]`, for `--debug-steam-library`.
fn parse_debug_game(raw: &str) -> Result<(String, bool), String> {
    let (name, installed) = match raw.rsplit_once(':') {
        Some((name, flag)) if matches!(flag.trim(), "0" | "1") => (name, flag.trim() == "1"),
        // A colon is part of a great many game names, so one that is not
        // followed by a flag is part of the name.
        _ => (raw, false),
    };
    let name = name.trim();
    if name.is_empty() {
        return Err(format!("{raw:?} names no game"));
    }
    Ok((name.to_string(), installed))
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
            // Always, and not a field of its own: an invented display is here
            // so that a page which needs a connector this session has not got
            // can be looked at, and the Night light page needs one exactly as
            // the HDR page does. Nothing is sent for these either, so no
            // picture anywhere is warmed by saying so.
            night_light: true,
            warming: false,
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
    let wallpaper_handoff = wallpaper_clock::WallpaperClock::from_environment(theme::accent().name);

    let mut categories = apps::scan();
    if !cli.no_steam {
        // Before the icons are decoded, so the row's glyph is in the atlas
        // with the rest rather than being asked for on the first frame that
        // draws it. Signed out at this point whatever the disk says: the
        // worker has not answered yet, and the row says what is known now.
        apps::offer_steam(&mut categories, None);
    }
    let categories = categories;

    let total: usize = categories.iter().map(apps::Category::apps).sum();
    tracing::info!(
        categories = categories.len(),
        applications = total,
        "scanned applications"
    );

    // Started here rather than with the rest of the shell's state: the walk
    // over the user's home directory runs on a worker, the first frame is
    // several hundred milliseconds of GPU setup away, and Multimedia's two
    // rows can be full by the time anybody sees them.
    let media = media::Library::start();
    if !thumbs::films_can_be_thumbnailed() {
        tracing::info!(
            "neither ffmpegthumbnailer nor ffmpeg is installed; films will keep their glyph"
        );
    }

    // Started here rather than with the rest of the shell's state, beside the
    // walk and for the same reason: restoring a stored Steam session is a
    // round trip to Steam and back, and the first frame is several hundred
    // milliseconds of GPU setup away.
    let mut steam = if cli.no_steam {
        tracing::info!("--no-steam: this session will not talk to Steam");
        steam::Steam::settled()
    } else if !cli.debug_steam_library.is_empty() {
        // A fixture is a fixture: a session pretending to have a library does
        // not also have a worker signing in behind it.
        steam::Steam::settled()
    } else {
        steam::Steam::start()
    };
    if !cli.debug_steam_library.is_empty() {
        steam.invent(&cli.debug_steam_library);
    }
    // Whether the games on the bar are somebody's actual library, which is
    // what decides whether their artwork may be fetched. See `art::Art`.
    let steam_is_real = steam.driving();

    // The wallpaper is the first visible part of a handed-off session, and it
    // needs no icon at all. Build its provisional atlas from the two procedural
    // cells (solid white and the selection glow), then find and decode the full
    // catalogue on a worker while the GPU is already presenting that same
    // moving wallpaper. The bar's normal arrival waits for the worker, so the
    // empty atlas is never visible as an incomplete row.
    let startup = StartupIcons::start(categories.clone());

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
        1..=NIGHT_LIGHT_SHELL_VERSION,
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
        polkit: polkit::Agent::start(),
        notifier: notify::Service::start(),
        notifications: notify::Center::new(settings::do_not_disturb()),
        icon_theme: IconLoader::new(),
        authenticating: None,
        sharing: None,
        pending_share: None,
        pending_capture: None,
        removal_plan: None,
        uninstalling: None,
        open_with: Vec::new(),
        open_with_chosen: 0,
        sorting: None,
        searching: None,
        deleting: None,
        quick: Quick::start(),
        steam,
        steam_buttons: Vec::new(),
        sounds: sound::Sounds::new(),
        osk: keyboard::Osk::default(),
        menu_frame_drawn: false,
        overview_started_at: None,
        launching: None,
        steam_windows: HashSet::new(),
        ended_by_the_shell: HashSet::new(),
        awaiting_steam: None,
        restoring: None,
        guide_card_rects: std::collections::HashMap::new(),
        keyboard: None,
        held_key: None,
        // Until the seat says otherwise, which it does with every keyboard it
        // hands out. Repeat is what a keyboard does; off is the setting.
        key_repeat_on: true,
        pointer: None,
        touch: None,
        touches: std::collections::HashMap::new(),
        pointer_enter: None,
        pointer_shape: None,
        scrolled: (0.0, 0.0),
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
        startup,
        xmb: Xmb::with_session_displays(categories, child_wayland_display, child_xwayland_display),
        media,
        // Only a real library has real artwork. An invented one — see
        // `--debug-steam-library` — numbers its games from one, and app 10 is
        // Counter-Strike: a fixture drawn with Valve's pictures would be a
        // screenshot of a library nobody owns.
        art: art::Art::start(steam_is_real),
        thumbs: thumbs::Thumbs::start(),
        drained: Drained::default(),
        exit: false,
        needs_redraw: true,
        next_frame_deadline: Instant::now(),
        wallpaper_clock: wallpaper_handoff.unwrap_or_else(wallpaper_clock::WallpaperClock::local),
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
    // A fixture library goes on the bar through exactly the path a real one
    // does — the row that names the account, then the column — so what is on
    // screen is the same arrangement the worker would have produced.
    if !cli.debug_steam_library.is_empty() {
        let account = shell.steam.account().map(str::to_string);
        let shifted = apps::offer_steam(&mut shell.xmb.categories, account);
        shell.absorb(shifted);
        let rows = shell.steam.rows();
        shell.shelve_games(rows);
    }

    // The screen lists under Settings > Display are built from what the
    // displays report. Seed them before the first frame so the pages are right
    // the first time they are opened rather than after the first change.
    shell.refresh_hdr_support();
    shell.refresh_display_modes();

    // Before the client is woken, not when the first game is pressed: the
    // session signs it in on its own as it starts, and a window suppressed
    // only from the first press onwards is one the user has already seen.
    shell.keep_steam_out_of_sight(true);

    while !shell.exit {
        event_queue.dispatch_pending(&mut shell)?;
        shell.xmb.reap_children();

        let now = Instant::now();
        // The full atlas lands before any worker is polled below, since those
        // workers may write notifications, thumbnails, covers or logos into
        // it. Until it lands, only the separately rendered backdrop is shown.
        shell.finish_startup_icons();
        shell.present_deferred_share();
        shell.poll_controller(now);
        // And the keyboard on the same clock: a held arrow is as much an
        // input still happening as a held D-pad is, and neither of them is a
        // Wayland event that would have woken this loop by itself.
        shell.repeat_held_key(now);
        // Scheduled actions, for capturing the menu's transitions.
        while debug_action_is_due(
            shell.startup.ready,
            shell.start.elapsed().as_secs_f32(),
            action_schedule.last().map(|(at, _)| *at),
        ) {
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
        // And a search field the cursor has been walked out of, on the same
        // terms and for the same reason: the bar can move underneath a board
        // by routes that never touch the field itself.
        shell.sync_search();
        // A package manager answering is not a Wayland event and cannot wake
        // this loop, but the loop wakes anyway to poll the controller — which
        // is the only reason a worker can hand its answer to a frame at all.
        if shell.startup.ready {
            shell.sync_app_facts();
        }
        // The same again for the walk over the user's home directory: a song
        // found on a worker thread is not a Wayland event either, and this is
        // the frame it reaches the bar on.
        if shell.startup.ready {
            shell.sync_media();
        }
        // And for Steam, which is the same shape again: a library read over a
        // network on a worker thread, arriving on whichever frame it is ready.
        if shell.startup.ready {
            shell.sync_steam();
        }
        // The other way round as well: a column stepped into is a question
        // about the disk, asked here because every way of stepping into one —
        // a stick, a key, a click, a finger — has by now settled into the same
        // cursor.
        if shell.startup.ready {
            shell.notice_open_shelves();
        }
        // And the pictures of what it found, which are made on two more
        // workers and land in the atlas here — for the rows the cursor is
        // near, and nowhere else.
        if shell.startup.ready {
            shell.sync_thumbnails();
        }
        // And likewise for a removal: neither the survey nor the removal itself
        // is a Wayland event, so the frame this loop was going to draw anyway is
        // what carries their answers on to the screen.
        shell.advance_uninstall();
        // The same again for an authorisation this session has been asked to
        // prove: `polkitd` asks on a thread of its own, and polkit's PAM helper
        // answers on another.
        if shell.startup.ready {
            shell.sync_polkit();
        }
        // And for what the rest of the machine has announced: the bus answers
        // on its own thread, so this is the frame its news reaches the screen
        // on, and the frame the programs that sent it hear back on.
        if shell.startup.ready {
            shell.sync_notifications();
        }
        // Applies whatever the last events settled on: an application exiting
        // changes what the surface should be doing just as much as a keypress.
        shell.sync_surface_state();
        // The same settled answer drives Start's music. This is deliberately
        // outside individual foreground and guide handlers: changing display,
        // choosing the Start card, launching, restoring and an application
        // exiting are different routes to the same ownership transition.
        let music_wanted = xmb_music_wanted(
            !shell.panels.is_empty(),
            shell.any_app_open(),
            // Wherever it is happening: an application taking either display is
            // an application taking the session's sound with it.
            shell.launching.is_some() || shell.restoring.is_some(),
        );
        shell.sounds.sync_music(music_wanted, now);
        shell.sync_launch_output();
        shell.sync_overview();
        // Last of the three, and unconditional: a display plugged in
        // mid-session has to be told what the session is set to, and the only
        // thing that knows it has not been told is the diff inside these.
        shell.sync_hdr();
        // And the one of the four that changes with nothing pressed: the
        // night light's schedule is kept here, so the hour turning is noticed
        // on the pass of this loop it turns on.
        shell.sync_night_light();
        shell.sync_mode();
        shell.sync_turn();
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

/// Whether a scheduled development action may be taken from the queue.
/// Startup time still counts towards its deadline, but the action is retained
/// until the controls it addresses have actually arrived.
fn debug_action_is_due(ready: bool, elapsed: f32, deadline: Option<f32>) -> bool {
    ready && deadline.is_some_and(|deadline| elapsed >= deadline)
}

/// Decode the shell's own marks at the front of the completed atlas.
///
/// They are compiled into the binary and do not touch the icon theme. The
/// provisional wallpaper-only atlas needs neither these nor catalogue icons;
/// keeping them here preserves the settled atlas's slot ordering.
fn load_builtin_icons() -> Vec<(String, icons::Icon)> {
    let mut out = Vec::new();
    for (name, drawing) in icons::BUILTIN {
        match icons::Icon::builtin(drawing, ICON_SIZE) {
            Some(icon) => out.push((name.to_string(), icon)),
            None => tracing::warn!(glyph = name, "could not rasterise a built-in glyph"),
        }
    }
    out
}

/// Decode every icon the catalogue names, with the built-ins first so their
/// slots and fallbacks retain the exact ordering the settled atlas had before
/// startup was split over two threads.
fn load_icons(categories: &[apps::Category]) -> Vec<(String, icons::Icon)> {
    let mut out = load_builtin_icons();
    let mut loader = IconLoader::new();
    let mut seen: HashSet<String> = icons::BUILTIN
        .into_iter()
        .map(|(name, _)| name.to_string())
        .collect();

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

/// Full catalogue atlas being prepared off the rendering thread.
struct StartupIcons {
    answer: std::sync::mpsc::Receiver<Vec<(String, icons::Icon)>>,
    pending: Option<Vec<(String, icons::Icon)>>,
    ready: bool,
}

impl StartupIcons {
    fn start(categories: Vec<apps::Category>) -> Self {
        let (tx, answer) = std::sync::mpsc::channel();
        let worker_categories = categories.clone();
        let pending = match std::thread::Builder::new()
            .name("lxb-startup-icons".to_string())
            .spawn(move || {
                let _ = tx.send(load_icons(&worker_categories));
            }) {
            Ok(_) => None,
            Err(err) => {
                // Thread creation can fail under a tight process limit. Keep
                // the session complete in that exceptional case, even though
                // it means doing the old synchronous work before the first
                // wallpaper frame.
                tracing::warn!(?err, "could not start the icon decoder");
                Some(load_icons(&categories))
            }
        };
        Self {
            answer,
            pending,
            ready: false,
        }
    }

    fn take(&mut self) -> Option<Vec<(String, icons::Icon)>> {
        if self.ready {
            return None;
        }
        match self.answer.try_recv() {
            Ok(icons) => Some(icons),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                // The worker owns no fallible operation that can unwind under
                // normal input, but if it does, let the procedural atlas become
                // the settled one rather than holding the session in depth.
                tracing::warn!("startup icon worker stopped before answering");
                self.ready = true;
                None
            }
        }
    }
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
        password: secret::Secret,
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

/// An authorisation `polkitd` has asked this session to prove, and how far that
/// has got.
///
/// One at a time, which the shell enforces by not taking a second question off
/// the agent while this is set: the panel driving it holds every button, so a
/// second would be a question the user cannot see — and one that would inherit
/// the press meant for the first.
struct Authenticating {
    /// What was asked, and whose password will do.
    request: polkit::Request,
    /// The conversation with polkit's PAM helper.
    ///
    /// Replaced rather than reused when a password is refused: one helper is
    /// one attempt, and another try means another helper. Dropping it kills the
    /// one it replaces.
    session: polkit::Session,
    /// What has been typed.
    ///
    /// The password lives here and in no other field of the shell. The panel is
    /// drawn from a count of characters — see [`dialog::Line::Secret`] — and
    /// this goes away with the authentication.
    password: secret::Secret,
    /// The sentence above the field: what PAM asked, or what the shell says
    /// while PAM has not asked anything yet.
    note: String,
    /// Whether the helper is waiting for a line. False while it is thinking,
    /// which is when Enter must not send it an empty one.
    asked: bool,
    /// Whether anything has been handed to this helper at all. What separates a
    /// password that was refused, which is worth offering another try at, from
    /// a helper that failed before it could ask — which is not, and would loop
    /// for ever if it were.
    answered: bool,
}

/// A shelf of the user's own files being searched, and what has been typed
/// into its field so far.
///
/// One at a time, for the same reason a removal is: there is one on-screen
/// keyboard and it is up on one display, so there is never a second field
/// wanting the letter that was just pressed.
///
/// The text is here rather than read back off the row it is drawn on because
/// the row is rebuilt from the worker's own answer every time the shelf is
/// narrowed — a field that read itself off the bar would lose whatever had
/// been typed since the last delivery, which on a large collection is most of
/// the word.
struct Searching {
    kind: media::Kind,
    text: String,
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
    /// What this display was last told about its night light: whether the
    /// filter should be burning at that moment and how warm.
    ///
    /// The answer rather than the setting, because the schedule is resolved
    /// here — see `Shell::sync_night_light`. That is what makes nine in the
    /// evening arriving a change this diff notices, without anything having
    /// been pressed.
    applied_night_light: Option<(bool, u16)>,
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
    /// Which way up the compositor says it is drawing this display, where the
    /// turn is the compositor's to make. `None` where it is not — a nested
    /// session, or a compositor too old to be asked — which is what keeps the
    /// display off the Orientation page rather than on it and inert.
    turned: Option<settings::Orientation>,
    /// The orientation this display was last asked for, so a choice is not
    /// resent, for the reason `applied_mode` is not.
    applied_turn: Option<settings::Orientation>,
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
    /// How far this display's start screen has come forward out of the depth
    /// the shell starts it in: 0 far back and unlit, 1 arrived — see
    /// [`ui::arrive_from_depth`]. Linear, as the ramps above are; `ui::ease`
    /// shapes it.
    ///
    /// Per display because a bar is: every screen the shell is given draws its
    /// own, so every screen watches its own arrive. That covers the display
    /// plugged in halfway through a session as well as the ones the shell came
    /// up on, and it is the same event either way — a start screen appearing
    /// where there was none.
    ///
    /// Run again whenever the start screen is handed a display it did not
    /// have; see [`Panel::arrive_out_of_black`] and
    /// [`Shell::an_application_ended_by_itself`].
    arrival_linear: f32,
    /// Whether this display has drawn a frame the current arrival can start
    /// from.
    ///
    /// What holds the screen at the back of its depth until there is one. The
    /// shell does a great deal before its first frame — binding the
    /// compositor's globals, reading the catalogue, decoding every icon in it,
    /// building the atlas — and that first frame's `dt` is all of that time.
    /// Charged to the arrival it would spend most of the animation before a
    /// single pixel had been shown, and what the user would see is the tail of
    /// a move that started while the screen was still black.
    ///
    /// The same trap waits at the other end of an application. A display behind
    /// a fullscreen window draws nothing at all, and with every display covered
    /// the shell idles on `HIDDEN_POLL` — so the first frame after the
    /// application goes away carries half a second of `dt` and would spend the
    /// whole arrival at once. Hence a flag rather than a one-off: an arrival
    /// begins from a frame, whichever arrival it is.
    arrival_has_a_frame: bool,
    /// How much black is still lying over this display: 1 the frame an
    /// application walked out of it, 0 once the screen is clear — see
    /// [`ui::cover_with_black`]. Linear, like the ramps above it.
    ///
    /// Only ever set by a handover. The shell's own first frames need nothing
    /// of the sort: there is no last frame of anybody else's to cover, and a
    /// session that began by fading up from black would be inventing a join
    /// where there is none.
    from_black_linear: f32,
    /// The start card's rectangle as of the last frame with the menu open —
    /// the end the start screen flies to, and flies back from once the menu
    /// has closed and the cards are gone.
    home_rect: [f32; 4],
    /// Which shelf of the user's own files this display is standing in, if it
    /// is standing in one — see [`Shell::notice_open_shelves`].
    ///
    /// Per display for the reason the cursor is: two screens browse
    /// independently, and somebody opening Images on the second one is asking
    /// the same question as somebody opening it on the first.
    shelf_open: Option<media::Kind>,
    /// The game whose picture is standing behind this display, and the one it
    /// is fading away from.
    ///
    /// Per display for the same reason again: each screen has its own cursor,
    /// so each is looking at its own game — or at none, which is the state
    /// every display starts in and goes back to on the way out of the library.
    scenery: Scenery,
}

/// One display's crossfade between the pictures behind it.
///
/// Held as *which games*, not as which layers of the texture: a picture that
/// has not been fetched yet has no layer, and the whole point of this is to
/// keep the last one on screen until the next one is ready rather than dipping
/// through the wallpaper in between.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct Scenery {
    /// The game being faded away from.
    from: Option<u32>,
    /// The game being faded to, which is the one under this display's cursor.
    /// `None` while the cursor is anywhere but a game, which fades the picture
    /// out to the shell's own wallpaper.
    to: Option<u32>,
    /// How far across, 0 to 1, in linear time — [`ui::ease`] shapes it. At 1
    /// the fade is over and `from` no longer matters.
    across: f32,
}

impl Scenery {
    /// Point this display at a game, or at none.
    ///
    /// Interrupting a fade is the whole difficulty here: what is on screen
    /// mid-fade is two pictures at once, and there is nowhere to keep the sum
    /// of them. Three cases, and none of them changes how much picture is on
    /// screen at the moment it happens, which is what would be seen as a jump:
    ///
    /// * Turning back to the picture being left — the cursor moved and moved
    ///   back — simply runs the same fade the other way from where it is.
    /// * Past halfway, the arriving picture is the one mostly on screen, so it
    ///   becomes the one being left and the newcomer starts from nothing.
    /// * Before halfway it is still the *old* picture that is mostly on
    ///   screen, so that stays, and the newcomer takes over the fade at the
    ///   point the picture it replaces had reached. What goes is whatever was
    ///   faintest, which is the least there is to see going.
    fn look_at(&mut self, game: Option<u32>) {
        if self.to == game {
            return;
        }
        if self.from == game {
            self.from = self.to;
            self.to = game;
            self.across = 1.0 - self.across;
            return;
        }
        if self.across >= 0.5 {
            self.from = self.to;
            self.across = 0.0;
        }
        self.to = game;
    }

    /// Move the fade on by `step` of its length.
    fn advance(&mut self, step: f32) {
        if !self.moving() {
            return;
        }
        self.across = (self.across + step).min(1.0);
        if self.across >= 1.0 {
            // Arrived. What it came from is nothing any display is showing now,
            // and holding it would hold a layer of the picture texture with it.
            self.from = None;
        }
    }

    /// Whether the crossfade still has somewhere to get to.
    fn moving(&self) -> bool {
        self.across < 1.0 && (self.from.is_some() || self.to.is_some())
    }

    /// What the renderer draws for it this frame.
    ///
    /// A game whose picture has not arrived resolves to no layer at all, which
    /// is what leaves the one being faded from at full strength: the fade is
    /// not advancing either, so the two agree.
    fn showing(&self, gpu: &gpu::Gpu) -> gpu::Hero {
        let across = ui::ease(self.across);
        gpu::Hero {
            from: self.from.and_then(|app_id| gpu.scenery(app_id)),
            leaving: 1.0 - across,
            to: self.to.and_then(|app_id| gpu.scenery(app_id)),
            arriving: across,
        }
    }

    /// The games whose pictures this display needs kept.
    fn wanted(&self) -> impl Iterator<Item = u32> + '_ {
        self.from.into_iter().chain(self.to)
    }
}

/// How much colour every game's cover is drawn with.
///
/// One for the whole shell rather than one per display, unlike [`Scenery`]:
/// what this is about is the disk, and every display is looking at the same
/// disk. Two screens showing the same library show the same greyed covers, and
/// a download finishing gives the colour back on both at once — which is what
/// "every display animates" means for a fact that is not a display's own.
#[derive(Debug, Default)]
struct Drained(HashMap<u32, Colour>);

/// Where one cover is between colour and grey, and where it is heading.
#[derive(Debug, Clone, Copy)]
struct Colour {
    at: f32,
    to: f32,
}

impl Drained {
    /// Take in what the library now says. `games` is every title in it, and
    /// whether it can be played right now.
    ///
    /// Rebuilt rather than edited, so a game that has left the library stops
    /// being remembered — and so that this is one pass over the rows the shell
    /// already has in its hand rather than a search per title.
    fn told(&mut self, games: impl Iterator<Item = (u32, bool)>) {
        let mut now = HashMap::with_capacity(self.0.len());
        for (app_id, installed) in games {
            let to = if installed { 0.0 } else { 1.0 };
            // A game seen for the first time starts where it belongs. The
            // library arriving is not the library changing, and a shelf that
            // faded up out of grey on the frame it appeared would announce
            // something that has not happened.
            let at = self.0.get(&app_id).map_or(to, |colour| colour.at);
            now.insert(app_id, Colour { at, to });
        }
        self.0 = now;
    }

    /// Move every cover on by `step` of the fade's length.
    fn advance(&mut self, step: f32) {
        for colour in self.0.values_mut() {
            colour.at = approach(colour.at, colour.to, step);
        }
    }

    /// Whether any cover is still on its way.
    fn moving(&self) -> bool {
        self.0.values().any(|colour| colour.at != colour.to)
    }

    /// How grey one game's cover is drawn this frame. A game this has not been
    /// told about is drawn from what the row itself says, with no fade — there
    /// is nothing to fade from.
    fn of(&self, app_id: u32, installed: bool) -> f32 {
        let at = self
            .0
            .get(&app_id)
            .map_or(if installed { 0.0 } else { 1.0 }, |colour| colour.at);
        // Shaped here rather than in the step, for the reason the picture
        // behind the bar is: what is kept has to be a position, so a fade
        // turned round halfway carries on from where the cover is instead of
        // from where a restarted curve would put it.
        ui::ease(at)
    }
}

impl Panel {
    fn owns(&self, surface: &wl_surface::WlSurface) -> bool {
        self.layer.wl_surface() == surface
    }

    /// Hand this display to the start screen: black over whatever was there,
    /// and the bar back at the far end of its arrival to come forward out of it
    /// — see [`ui::cover_with_black`] and [`ui::arrive_from_depth`].
    ///
    /// For a display the start screen is being *given* rather than one it has
    /// held all along. Coming up on a fresh session is one of those; an
    /// application ending and leaving the screen to the bar is the other, and
    /// from the bar's side they are the same event — something that was not on
    /// this display is now the whole of it, and it says so the same way both
    /// times rather than blinking into place because this time nobody rebooted.
    ///
    /// Only the second gets the black. A session's first frame has no last
    /// frame of anybody else's to cover.
    ///
    /// The frame flag goes with the ramps, and it is the half that matters: they
    /// are fed the interval between frames, and there is no such interval yet on
    /// the frame a handover starts.
    fn arrive_out_of_black(&mut self) {
        self.arrival_linear = 0.0;
        self.arrival_has_a_frame = false;
        self.from_black_linear = 1.0;
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

/// A press on a Steam game, waiting for Valve's client to be ready for it.
///
/// Starting a game is a round trip — the client may have to be woken and
/// signed in first — so the press is answered immediately with the same splash
/// every other launch gets, and this is what the shell has to remember until
/// the client is ready: which game was pressed, and everything about the press
/// that was read off the bar at the time.
struct AwaitingSteam {
    app_id: u32,
    name: String,
    /// So a failure can be explained on the display it was pressed from, out
    /// of the tile it was pressed on, once the splash has been taken away.
    panel: usize,
    from: [f32; 4],
    /// When the press was, for the log: the interesting number is how long a
    /// cold client keeps somebody waiting.
    asked: Instant,
}

/// A desktop portal's question, while it is on screen.
#[derive(Debug, Clone)]
struct ShareQuestion {
    /// The portal's own number for it, quoted back in the answer.
    id: u32,
    /// Which display each offered row is, as a place in `panels`. Held rather
    /// than re-derived when a row is pressed: a display can be unplugged while
    /// the question is up, and the row that was drawn as "HDMI-A-1" must not
    /// become whichever screen happens to be second by then.
    displays: Vec<wl_output::WlOutput>,
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
    /// This session's polkit agent, which is what answers `polkitd` when
    /// something on the machine wants the user to prove they may do it.
    ///
    /// `None` on a session that cannot have one — no system bus, or a
    /// LineXinBar started inside a desktop whose own agent got there first. See
    /// [`polkit`], and note that the shell asks nothing of it: an agent that
    /// never registered simply never has a question waiting.
    polkit: Option<polkit::Agent>,
    /// This session's notification daemon: `org.freedesktop.Notifications` on
    /// the session bus, which is how everything else on the machine announces
    /// that something has happened.
    ///
    /// `None` on a session that cannot have one — no session bus, or a
    /// LineXinBar started inside a desktop whose own daemon holds the name.
    /// Exactly as with the polkit agent, the shell asks nothing of it in that
    /// case: a daemon that never took the name simply never has anything
    /// waiting. See [`notify`].
    notifier: Option<notify::Service>,
    /// What has been announced, and what is still being shown in the corner of
    /// the screen. Kept whether or not the daemon started, so every path that
    /// draws or navigates it has one answer rather than two.
    notifications: notify::Center,
    /// The icon theme, kept open for the pictures announcements bring with
    /// them.
    ///
    /// Every other icon in this shell is found and decoded before the first
    /// frame, by a loader that is dropped when that is done — the catalogue is
    /// known by then and nothing adds to it. An announcement is the one thing
    /// that names a picture while the session is running, so this is a second
    /// loader with a much longer life and almost nothing to do. It is kept
    /// rather than built per announcement because what it caches is the theme
    /// index itself: reading and parsing every `index.theme` on the machine to
    /// find one icon, and then throwing that away, would be the expensive half
    /// done over and over.
    icon_theme: IconLoader,
    /// The authentication on screen now, if any: what was asked, the
    /// conversation with polkit's PAM helper, and what has been typed into the
    /// field so far.
    authenticating: Option<Authenticating>,
    /// The share question currently on screen: which question it is, and which
    /// of this shell's displays each row of it offers.
    ///
    /// Held because the answer goes back over the protocol by number, and
    /// because a second application asking while the first question is up must
    /// not quietly replace it — see [`Shell::ask_to_share`].
    sharing: Option<ShareQuestion>,
    /// A portal question that arrived while only the startup wallpaper was on
    /// screen. Held rather than refused: icon decode is bounded local work,
    /// and the application has already chosen to wait for the user's answer.
    pending_share: Option<(u32, String)>,
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
    /// The applications the open Open with list is offering, in the order it is
    /// offering them: [`menu::Command::OpenWithHandler`] carries a position in
    /// this and nothing else. Emptied when the menu goes away, so a stale index
    /// can never name a program.
    open_with: Vec<media::Handler>,
    /// Which of them opens the type at the moment — the row wearing the tick.
    ///
    /// Held beside the list rather than worked out from it, because the list is
    /// deliberately *not* rebuilt while the panel is up. The user picks by
    /// pointing at a row, and rows that reordered themselves under the press —
    /// the newly chosen application jumping to the head, where the default
    /// belongs — would move the next row they were reaching for.
    open_with_chosen: usize,
    /// The shelf a display has just asked to have listed in a different order,
    /// and which display that was.
    ///
    /// Held only until those rows arrive. See [`Shell::sort_selected_shelf`]:
    /// the cursor goes to the head of the column at once *and* again when the
    /// new order lands, and this is what remembers that the second one is owed.
    sorting: Option<(media::Kind, usize)>,
    /// The shelf whose search field the on-screen keyboard is typing into, and
    /// what has been typed so far.
    ///
    /// The field's own record of itself, ahead of both the bar and the worker:
    /// the letter goes here on the frame the key is pressed, is written onto
    /// the row for the frame to draw, and is sent to the worker to narrow the
    /// shelf with. `None` whenever the board is not in a search, which is
    /// nearly always.
    searching: Option<Searching>,
    /// The file the Delete question is about.
    ///
    /// Held rather than looked up again when the question is answered, for the
    /// reason [`Shell::removal_plan`] is held: the panel is about one particular
    /// thing, and the one command in the shell that destroys somebody's file
    /// must act on the file the panel named and on nothing else.
    deleting: Option<media::File>,
    /// The volume and brightness bars in the guide's sidebar, and the worker
    /// that keeps them true.
    quick: Quick,
    /// Steam: who is signed in, the library that follows from it, and the
    /// panel that signs somebody in. The client itself is a worker thread on
    /// the other side of this — see [`steam`] and the `lxb-steam` crate — for
    /// the reason the walk over the user's files is one: nothing that waits on
    /// a network may happen on the thread that draws.
    steam: steam::Steam,
    /// Which buttons the sign-in panel last went up with.
    ///
    /// Held so that redrawing the panel can tell a new *question* from the
    /// same question with different words in it. Typing a letter and Steam
    /// rotating the code on screen both change what the panel says; neither
    /// should make it grow out of its anchor a second time.
    steam_buttons: Vec<menu::Command>,
    /// The shell's own effects and Start music, and the output they go to.
    /// Beside the bars rather than beside the controller, though most of them
    /// answer a button: this is the other end of the machine's audio, and what
    /// the bars set is what it comes out at.
    sounds: sound::Sounds,
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
    /// The windows a game started through Valve's client turned out to be.
    ///
    /// Written down at the one moment it can be known — the launch splash
    /// handing the screen over — because nothing about a window says who
    /// started it afterwards. A game out of the library announces whatever
    /// class its binary happens to have, which for a Proton title is routinely
    /// the architecture it was built for, so the shell cannot tell one of
    /// these from any other unnamed window by looking at it.
    ///
    /// What it is for is naming: see [`Self::window_app_name`]. Ids rather than
    /// classes, because the class is exactly the part that is not to be
    /// trusted — half the Proton library calls itself `x86_64`, and a session
    /// that tagged the *class* would be claiming every later window of that
    /// name was a game somebody launched here.
    steam_windows: HashSet<u32>,
    /// Windows the shell itself has just done away with: killed from the
    /// guide's Close, or sent to another display.
    ///
    /// What tells the difference between an application the user ended and one
    /// that ended on its own — see [`Self::an_application_ended_by_itself`],
    /// which is the whole reason the difference is worth keeping. Both leave
    /// the same hole in the window list, and only one of them is a surprise.
    ///
    /// An id is spent the moment the window it names goes, so a window that
    /// moves to another display costs nothing afterwards: it leaves this
    /// display's list, the id is used up there, and its arrival on the other
    /// display is an ordinary window appearing. What is left behind is the case
    /// of a kill that did not take — the id sits here until that window
    /// eventually does go, and the shell treats that as its own doing. Which it
    /// arguably was.
    ended_by_the_shell: HashSet<u32>,
    /// A game that has been pressed and cannot be started yet, because Steam
    /// has not said whether this account owns it. Its splash is already up.
    awaiting_steam: Option<AwaitingSteam>,
    /// A window flying back out of its tile, drawn by the compositor.
    restoring: Option<Restore>,
    /// Eased card rectangles by window id (`u64::MAX` is the start card),
    /// advanced every frame on the compositor's own spring so the frames the
    /// shell draws travel with the windows the compositor is easing.
    guide_card_rects: std::collections::HashMap<u64, Glide>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    /// The key the user is holding down, if any, and when it next acts.
    ///
    /// Repeat is the client's own work under Wayland — the compositor sends
    /// one press and one release, and the toolkit only fills the gap for a
    /// client that runs its repeat in a calloop, which this loop is not. So
    /// the shell keeps the key itself and steps it in [`Shell::repeat_held_key`],
    /// at the rate the D-pad already walks the bar at.
    held_key: Option<HeldKey>,
    /// Whether the seat repeats keys at all. The user can turn repeat off for
    /// the session, and the shell's own is still the session's.
    key_repeat_on: bool,
    /// The seat's pointer.
    ///
    /// A console is driven from a controller and the shell is drawn for one,
    /// but the machine it runs on is a PC with a mouse in the drawer, and every
    /// one of the shell's surfaces covers a whole display. So what the pointer
    /// can reach is exactly what the shell is currently showing — see
    /// [`Shell::spot_at`] — and nothing while the shell is standing out of an
    /// application's way, which is the same rule the input region is cut to.
    pointer: Option<wl_pointer::WlPointer>,
    /// The seat's touchscreen, on the machines that have one. Everything a
    /// finger does goes through the same hit test the pointer does; what
    /// differs is only that there is no hovering — see [`Shell::on_touch`].
    touch: Option<wl_touch::WlTouch>,
    /// Where each finger currently down went down, so a tap can be told from a
    /// drag across the display. Keyed by the touch point's own id.
    touches: std::collections::HashMap<i32, Touch>,
    /// The serial of the last pointer enter, which is what a request to change
    /// the cursor's shape has to carry however long ago the pointer arrived.
    pointer_enter: Option<u32>,
    /// The shape it was last set to, so an unchanged one is not resent on every
    /// motion event. Cleared on each enter rather than assumed: what the cursor
    /// looks like when it arrives is whatever the surface it came from asked
    /// for, which over a text field is a beam.
    pointer_shape: Option<shape::Shape>,
    /// Wheel movement, in notches, that has not yet added up to a step of the
    /// selection.
    ///
    /// A touchpad sends a continuous stream of fractions of a notch and the
    /// shell moves in whole rows; keeping the remainder is what makes a slow
    /// two-finger drag scroll one row at a time instead of nothing at all.
    scrolled: (f64, f64),
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
    /// Catalogue atlas being decoded while the first wallpaper frames move.
    /// No runtime picture is written until this has replaced the provisional
    /// procedural atlas, so replacement can never discard a notification,
    /// thumbnail, cover or logo that arrived during startup.
    startup: StartupIcons,

    xmb: Xmb,
    /// The user's own music, films and photographs, and the walk over their
    /// home directory that keeps finding them. Held beside the catalogue
    /// rather than in it:
    /// the catalogue is rebuilt whenever software is installed or removed, and
    /// what is on the disk does not change because a package did.
    media: media::Library,
    /// The workers that make a picture of a film or a photograph, and what
    /// they have been asked for. Kept beside the library for the same reason
    /// it is kept beside the catalogue: what is on the disk is not the shell's
    /// to rebuild when something is installed.
    thumbs: thumbs::Thumbs,
    /// And the workers that fetch Steam's own pictures of a game — the cover
    /// on its row and the picture behind the display while it is chosen.
    /// Beside the library rather than in it for the same reason again: the
    /// catalogue is rebuilt whenever the account's library changes, and a
    /// picture already fetched does not stop being that game's picture.
    art: art::Art,
    /// How much colour each of those covers is drawn with, which is whether
    /// its game is on this disk. Beside the pictures rather than in the
    /// catalogue because it is a fade and the catalogue has no clock: the rows
    /// are rebuilt from the library several times a second while a game is
    /// being fetched, and a fade held in one of them would restart on each.
    drained: Drained,
    exit: bool,
    needs_redraw: bool,
    next_frame_deadline: Instant,
    wallpaper_clock: wallpaper_clock::WallpaperClock,
    start: Instant,
    last_frame: Instant,
    frames: u32,
    fps_window: Instant,
    conn: Connection,
}

impl Shell {
    /// Install the complete atlas and release every display into its existing
    /// arrival. Until this succeeds the backdrop is fully live, while the main
    /// scene remains at arrival zero and therefore contributes no visible ink.
    fn finish_startup_icons(&mut self) {
        if self.startup.ready {
            return;
        }
        let icons = self.startup.pending.take().or_else(|| self.startup.take());
        let Some(icons) = icons else {
            // A disconnected worker marks itself ready and leaves the procedural
            // atlas in place. It remains a usable wallpaper; the bar begins
            // with missing-icon fallbacks instead of trapping the session.
            if self.startup.ready {
                self.needs_redraw = true;
            }
            return;
        };
        let Some(gpu) = self.gpu.as_mut() else {
            // The device has not been created yet. Keep the answer until its
            // first configure has created the provisional atlas.
            self.startup.pending = Some(icons);
            return;
        };
        match gpu.replace_icons(icons) {
            Ok(()) => {
                self.startup.ready = true;
                tracing::info!("startup icons ready; beginning the bar arrival");
                self.needs_redraw = true;
                self.last_frame = Instant::now();
            }
            Err(err) => {
                // Do not take down a session whose wallpaper is already on
                // screen. The invariant failure is logged loudly; the
                // procedural atlas itself remains valid.
                tracing::error!(?err, "could not install the completed icon atlas");
                self.startup.ready = true;
                self.needs_redraw = true;
            }
        }
    }

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
            applied_night_light: None,
            modes: Vec::new(),
            pending_modes: Vec::new(),
            applied_mode: None,
            turned: None,
            applied_turn: None,
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
            arrival_linear: 0.0,
            arrival_has_a_frame: false,
            from_black_linear: 0.0,
            home_rect: [0.0; 4],
            shelf_open: None,
            scenery: Scenery::default(),
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
        // The screen lists under Settings > Display are built from the panels,
        // and a display that has gone has to leave them here: the compositor's
        // per-display events only ever carry the displays that are still
        // there, so nothing else is going to mention this one again.
        self.refresh_hdr_support();
        self.refresh_display_modes();
        self.refresh_display_turns();
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
        self.focus_panel(next);
    }

    /// Hand control to display `index`, whichever way the user asked for it: a
    /// shoulder button, or a press made on it.
    ///
    /// Answers whether control actually moved, because the press that moved it
    /// is spent on the move — see [`Shell::press_on`].
    fn focus_panel(&mut self, next: usize) -> bool {
        if next >= self.panels.len() || next == self.focused_panel {
            return false;
        }
        self.focused_panel = next;
        tracing::debug!(display = %self.panels[next].name, "control moved to a display");
        // The menu does not travel onto a launch, and this is the whole of
        // that rule: control moves, the menu does not come with it.
        //
        // Being covered is not the same as being closed, which is what made
        // this worth a rule of its own. The splash is drawn last and takes the
        // whole display, so a menu that arrived under it was invisible — but
        // the pad is read straight from the device rather than through the
        // surface, so it went on driving: directions moving a selection nobody
        // could see, and `A` landing on whichever row it had stopped at, up to
        // and including the power dialog. A control the user cannot see is one
        // they cannot be pressing on purpose.
        //
        // Only the arrival is refused; the *move* is not. Control crosses to
        // the loading screen exactly as asked, and what the user finds there
        // is the thing that display is actually doing.
        if self.guide.is_menu() && self.launching_on(next) {
            tracing::debug!("control arrived at a launch, so the menu did not come with it");
            self.guide.dismiss(self.app_running());
        }
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
        true
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
        let time = self.wallpaper_clock.elapsed_secs();
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
        // And which application it is in front of, whether or not the mixer is
        // open: the row the mixer keeps for that application is how a silence
        // set on it — here, or in another desktop before this session — is
        // undone, and a change made to that row is waited on afterwards with
        // nothing on screen at all.
        self.quick.watch_front(self.in_front());
        let volume = self.quick.level(Knob::Volume);
        let brightness = self.quick.level(Knob::Brightness);
        // What the machine is playing changes without anybody pressing
        // anything, so the open panel is brought up to date once a frame — an
        // application that has started making a noise appears in it, and one
        // that has stopped leaves.
        self.sync_mixer();
        // And what it can play *through* changes the same way: headphones are
        // plugged in while the page listing them is on screen.
        self.sync_sound_devices();
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

        // The game each display is looking at, if it is looking at one.
        //
        // Worked out here rather than in the loop below, which cannot reach
        // the catalogue: by then every panel is borrowed for drawing. A game
        // Steam has no picture of is not one of these — the shell stops
        // waiting for a picture that is never coming and lets the wallpaper
        // back in, which is the honest answer for a title with no artwork.
        let chosen: Vec<Option<u32>> = self
            .panels
            .iter()
            .map(|panel| {
                let game = panel.cursor.current_entry(&self.xmb)?.game()?;
                if self.art.hopeless(game.app_id, art::Piece::Hero) {
                    return None;
                }
                Some(game.app_id)
            })
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
                let row = ui::context_menu_highlight_rect(width, height, &self.context_menu)?;
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

        // The colour coming back into a cover whose game has just landed on the
        // disk, or leaving one that has gone off it. Advanced once for the
        // frame rather than once per display: it is one fade, shown on all of
        // them, and running its clock on per panel would take it at twice the
        // speed on a machine with two screens.
        self.drained.advance(dt / COLOUR_FADE);
        let covers_settling = self.drained.moving();

        let Some(gpu) = self.gpu.as_mut() else {
            self.next_frame_deadline = now + FRAME_CALLBACK_WATCHDOG;
            return;
        };
        // Where each game's cover ended up, and how much colour is in it, for
        // the rows about to be drawn. Bound out here because the panels are
        // borrowed one at a time below and the shell is not reachable from
        // inside that.
        let art = &self.art;
        let drained = &self.drained;
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

            // And the one journey down that axis that is made only once: the
            // start screen coming forward out of the depth it was started in.
            // It runs from the frame after this display's first, so the time
            // the shell spent getting to that frame is not charged to it — see
            // `Panel::arrival_has_a_frame`.
            if startup_arrival_can_advance(self.startup.ready, panel.arrival_has_a_frame) {
                panel.arrival_linear = approach(panel.arrival_linear, 1.0, dt / ui::ARRIVAL);
                // And the black the handover laid over the display, which comes
                // off in a third of the time the bar takes to arrive through it.
                panel.from_black_linear =
                    approach(panel.from_black_linear, 0.0, dt / ui::BLACK_HANDOVER);
            }

            // And the picture behind this display, which is the game under its
            // own cursor. The crossfade waits for the picture it is going *to*
            // — a fade started before there is anything to fade into would put
            // the wallpaper on screen between two games, which is a flash of
            // the shell in the middle of a move between two of somebody's
            // titles. It never waits to leave: fading out has everything it
            // needs already.
            panel.scenery.look_at(chosen[index]);
            let arriving = panel
                .scenery
                .to
                .is_none_or(|app_id| gpu.scenery(app_id).is_some());
            if arriving {
                panel.scenery.advance(dt / SCENERY_FADE);
            }
            let hero = panel.scenery.showing(gpu);

            // Being hidden is not a reason to stop mid-animation: what stays
            // committed is what a translucent application in front shows, and
            // what the display goes back to if it is uncovered. Settle first,
            // then go quiet.
            let settling = panel.home_linear != home_target
                || panel.blur_linear != home_target
                || panel.depth_linear != depth_target
                || panel.arrival_linear < 1.0
                || panel.from_black_linear > 0.0
                || (arriving && panel.scenery.moving())
                || covers_settling
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
            // From here on this display has shown something, so its arrival has
            // a frame to start from.
            panel.arrival_has_a_frame = self.startup.ready;

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
                    if let Err(err) =
                        gpu.render(backdrop_target, &[], &[], time, Some(params), hero)
                    {
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
                    &Slots { gpu, art, drained },
                    // The caret belongs to the display being typed on, which
                    // is the one holding the keyboard.
                    focused && self.searching.is_some(),
                )
            };
            // Before anything else is done to it: the arrival is where the
            // screen *is* on the shell's first frames, and everything below is
            // about what is standing over it or what has taken the display from
            // it. On a session that has been up for a second this does nothing
            // at all.
            ui::arrive_from_depth(
                &mut scene,
                width as f32,
                height as f32,
                panel.arrival_linear,
            );
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
            // And after all of those, the black an application walking out left
            // over the display. It is a sheet over the *screen* rather than
            // part of the scene: one laid on earlier would have been shrunk by
            // the arrival and by the card flight, and a curtain the size of the
            // start screen is a curtain with the display showing round it.
            ui::cover_with_black(
                &mut scene,
                width as f32,
                height as f32,
                ui::ease(panel.from_black_linear),
            );
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
                        do_not_disturb: self.notifications.quiet(),
                        unread: self.notifications.badge(),
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
                        slots: &Slots { gpu, art, drained },
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
                        slots: &Slots { gpu, art, drained },
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
                        slots: &Slots { gpu, art, drained },
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
                            slots: &Slots { gpu, art, drained },
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
                            slots: &Slots { gpu, art, drained },
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
                let zoom = 1.0 + LAUNCH_PUSH * grown;
                let [cx, cy] = [
                    splash.from[0] + splash.from[2] * 0.5,
                    splash.from[1] + splash.from[3] * 0.5,
                ];
                scene.scale_by(zoom, [cx - cx * zoom, cy - cy * zoom]);
                // Only where there is a panel to be behind. A game is answered
                // on its own picture with nothing drawn over the display, so
                // there is nothing for a label to be hidden by — and the bar's
                // own fade below takes every one of them anyway.
                if splash.game().is_none() {
                    let (panel_rect, _) =
                        ui::launch_panel(splash.from, width as f32, height as f32, open);
                    scene.hide_text_behind(panel_rect);
                }
                scene.fade(1.0 - grown);

                let icon = splash
                    .icon
                    .as_deref()
                    .and_then(|name| Slots { gpu, art, drained }.slot_for(Some(name)));
                let over = ui::build_launch(
                    ui::LaunchView {
                        name: &splash.name,
                        icon,
                        game: splash.game().is_some(),
                        logo: splash.game().and_then(|app_id| gpu.logo(app_id)),
                        doing: splash.doing(),
                        blackout: splash.blackout(now),
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

            // The corner of the screen, last of all and over everything: the
            // bar, the guide, a panel raised from it, the on-screen keyboard,
            // a launch splash. An announcement arriving while any of those is
            // up is still an announcement, and there is nothing in this shell
            // it should be behind.
            //
            // On the display being driven and no other, which is the same rule
            // the guide follows — and the same answer `sync_surface_state`
            // gives when it decides which surface has to rise to the overlay
            // layer for this, so the two cannot disagree about where the
            // bubbles are.
            if index == self.focused_panel && !self.notifications.toasts().is_empty() {
                let cards: Vec<ui::ToastCard> = self
                    .notifications
                    .toasts()
                    .iter()
                    .map(|toast| ui::ToastCard {
                        title: toast.about.title(),
                        body: &toast.about.body,
                        // The program's own picture, and the bell behind it.
                        // A bubble with a hole where its icon should be is
                        // worse than one wearing the shell's own mark: the
                        // mark is at least true — this *is* an announcement —
                        // where the hole says only that something failed to
                        // load. It catches both ways of having no picture: a
                        // program that named none, and one whose name this
                        // machine's icon theme has never heard of.
                        // `glyph` rather than `slot_for`, which is the whole
                        // of what makes the sentence above true: `slot_for`
                        // stands the generic application icon in for anything
                        // it cannot find, so the fallback below could never
                        // have been reached and a name this machine has not
                        // got came out as a blank executable rather than as
                        // the bell.
                        icon: toast
                            .about
                            .icon_name()
                            .and_then(|name| Slots { gpu, art, drained }.glyph(name))
                            .or_else(|| Slots { gpu, art, drained }.glyph(icons::NOTIFICATIONS)),
                        stage: toast.stage(),
                        progress: toast.progress(),
                    })
                    .collect();
                // The same blur every other pane in the shell bends: a bubble
                // is glass laid over whatever is behind it, and glass showing
                // a sharp copy of that is a window, not a pane.
                let over = ui::build_toasts(&cards, width as f32, height as f32, backdrop_blur);
                // Every quad is drawn before every text run, so the glass
                // would otherwise sit behind the labels of whatever it has
                // landed on. The bubbles cut them instead, which is what
                // something in front of them does.
                for card in ui::toast_rects(&cards, width as f32, height as f32) {
                    scene.hide_text_behind(card);
                }
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

            // A game opening holds the display with this surface, so this
            // surface has to carry the picture too.
            //
            // The hero normally lives on the backdrop surface, which is on the
            // background layer — under every application window. That is right
            // for a wallpaper and wrong for a loading screen: the instant the
            // game's own window maps, the picture the splash was standing on
            // is behind it, and what is left is a title floating over a game
            // the shell has not revealed yet. So while a game is opening, the
            // surface that *is* over the window paints the picture as well.
            //
            // It stops at the black, not at the window. Once the screen has
            // gone fully black the picture under it is the game's own, and
            // painting a hero over that would spend the whole way up revealing
            // the library the user just left instead of the game they asked
            // for — see [`launch::Launch::uncovering`].
            let game_opening = self
                .launching
                .as_ref()
                .filter(|splash| splash.panel == index && splash.game().is_some())
                .is_some_and(|splash| splash.drawing(now) && !splash.uncovering(now));

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
            } else if game_opening || (focused && self.guide.is_over_app()) {
                Some(gpu::Backdrop::default())
            } else {
                None
            };

            match gpu.render(
                target,
                &scene.quads,
                &scene.texts,
                time,
                main_backdrop,
                hero,
            ) {
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
                    match unsafe {
                        gpu::Gpu::new_wallpaper(display_ptr, surface_ptr, width, height)
                    } {
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

    /// Step the key the user is holding down, if it is time and if it is a key
    /// that repeats at all.
    ///
    /// The keyboard's half of what the D-pad gets from [`controller`]: a held
    /// direction walks the bar until it is let go of, rather than moving one
    /// row and stopping. It is done here rather than by the toolkit because
    /// Wayland gives a client the press and the release and nothing between
    /// them, and the toolkit's own filler runs in a calloop this shell does not
    /// have. The loop wakes for the controller often enough that a step is
    /// never more than a poll late.
    fn repeat_held_key(&mut self, now: Instant) {
        if !self.key_repeat_on {
            return;
        }
        let Some(held) = self.held_key.as_mut() else {
            return;
        };
        if !held.due(now) {
            return;
        }
        let keysym = held.keysym;
        if !key_repeats(keysym, self.password_wanted()) {
            // Acted once when it was pressed, and that was the whole of it.
            // Dropped rather than left to be asked about every pass: what the
            // key is worth cannot change while it is down.
            self.held_key = None;
            return;
        }
        self.on_key(keysym);
    }

    fn poll_controller(&mut self, now: Instant) {
        // Being driven is not the same as holding the keyboard, and the
        // on-screen keyboard is the case that separates them: it is up, the
        // user is picking out letters on it with the stick, and it has
        // deliberately left the Wayland keyboard with the application it is
        // typing into. Reading focus alone here threw away every direction and
        // every press the moment the board appeared.
        let active = self.startup.ready
            && controller_is_driving(
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

    /// The same application, as the volume mixer has to list it: whether or not
    /// it is making a sound right now.
    ///
    /// The mixer is otherwise a list of what the server is playing, and an
    /// application that is silent at this moment is not on it — including one
    /// silent *because* it was muted, here or in whatever desktop the user was
    /// in yesterday. That mute is remembered by the sound server per
    /// application and comes back with the application, so without a row for it
    /// there is nothing left to undo it with. Hence this row, for the one
    /// application the user can be assumed to be asking about.
    ///
    /// The names are every spelling its sounds might be listed under, best
    /// first, because the two sides name one application differently and only
    /// this side has anything to look it up in: what the window calls itself,
    /// what the desktop entry that installed it declares and runs, what the
    /// Steam library calls a game whose window is a numbered `steam_app_…`,
    /// and — last, because a title describes the *contents* of a window — what
    /// the window is titled, which for a game under Proton is usually the only
    /// thing it and its sound have in common.
    fn in_front(&self) -> Option<system::InFront> {
        let id = self.pointer_app()?.to_string();
        let installed = self.xmb.app_for_window(&id);
        let game = id
            .strip_prefix("steam_app_")
            .and_then(|app_id| app_id.parse().ok())
            .and_then(|app_id| self.steam.game(app_id))
            .map(|game| game.name.clone());
        let titled = self.app_label().map(str::to_string);

        let mut names: Vec<String> = Vec::new();
        let mut add = |name: Option<String>| {
            let Some(name) = name.filter(|name| !name.trim().is_empty()) else {
                return;
            };
            if !names.iter().any(|held| held.eq_ignore_ascii_case(&name)) {
                names.push(name);
            }
        };
        add(Some(id.clone()));
        // `org.mozilla.firefox` is the window of a program called `firefox`,
        // and the sound is the program's.
        add(id.rsplit('.').next().map(str::to_string));
        if let Some(app) = installed {
            for name in app.window_names() {
                add(Some(name));
            }
            add(Some(app.name.clone()));
        }
        add(game.clone());
        add(titled.clone());

        Some(system::InFront {
            // Named the way every other thing in this shell that stands for an
            // application is named — see [`Self::window_app_name`] — except
            // that a game out of the library beats the window's own name,
            // which for a Steam game is a number nobody could read.
            title: installed
                .map(|app| app.name.clone())
                .or(game)
                .or_else(|| app_id_name(&id))
                .or(titled)
                .unwrap_or_else(|| id.clone()),
            id,
            names,
        })
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
        // A field of the shell's own takes the whole keyboard while it is up,
        // and takes it first. Everything below this line turns keys into
        // *actions* — Q would be Back, Escape would leave the panel — and a
        // password with a Q in it is a password the user cannot type, as a
        // search for a song with a Q in its name is a search nobody can make.
        if self.field_wanted() {
            // Except the guide, which outranks everything the shell draws and
            // every grab an application can take. A modal that could swallow it
            // would be the one screen in the shell with no way out of it, which
            // is exactly the failure that rule exists to prevent.
            if action_for_keysym(keysym) == Some(Action::Guide) {
                self.on_action(Action::Guide);
                return;
            }
            if let Some(stroke) = keyboard::stroke_for(keysym) {
                if self.type_into_shell(stroke) {
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

    /// Apply one keystroke to whichever field of the shell's own is waiting for
    /// it. Returns whether there was one.
    ///
    /// Three of them, and no two can be up together: both passwords are asked
    /// for by a modal panel that takes every button — and the shell will not
    /// raise a second while the first is on screen — and a search field is a
    /// row on a bar those panels are drawn over. The passwords are asked first
    /// all the same, because theirs are the letters that must never go anywhere
    /// else.
    fn type_into_shell(&mut self, stroke: keyboard::Stroke) -> bool {
        self.type_into_password(stroke)
            || self.type_into_authentication(stroke)
            || self.type_into_steam(stroke)
            || self.type_into_search(stroke)
    }

    /// Whether something the shell has drawn is waiting to be typed into, and
    /// so whether keys are letters rather than buttons.
    fn field_wanted(&self) -> bool {
        self.password_wanted() || self.steam.field_wanted() || self.search_wanted()
    }

    /// Whether the panel on screen is waiting for a password to be typed into
    /// it.
    ///
    /// Either panel that has a field on it, because what this decides is the
    /// same for both: keys are letters rather than buttons, they repeat, and
    /// the board must not be taken away by Back — a field with no keyboard on a
    /// console is a field that cannot be filled in.
    fn password_wanted(&self) -> bool {
        self.uninstalling
            .as_ref()
            .is_some_and(|state| matches!(state.stage, Stage::Asking { .. }))
            || self.authenticating.is_some()
            || self.steam.password_wanted()
    }

    /// Whether the cursor is standing in a search field that is being typed
    /// into.
    ///
    /// Both halves are asked, because the field is a row on the bar rather than
    /// a panel over it: a search that had been opened and then walked away from
    /// would otherwise go on swallowing the keyboard from wherever the cursor
    /// had got to.
    fn search_wanted(&self) -> bool {
        self.searching.is_some()
            && self
                .selected_search()
                .is_some_and(|search| search.role == apps::Role::Field)
    }

    /// Whether an application is on screen in front of this shell.
    fn app_running(&self) -> bool {
        self.app_label().is_some()
    }

    /// Whether *any* display in this session has an application open on it.
    ///
    /// The wider question, and a different one: [`Self::app_running`] is about
    /// the display holding control, because that is the one the guide is drawn
    /// on and the one its rows act on. This is about the session, for the
    /// things that belong to the whole of it rather than to a screen — the
    /// Start screen's background music, which must not start up behind a game
    /// on the other display merely because control has crossed to a bar.
    ///
    /// A window nobody is looking at counts as much as the one in front: an
    /// application left behind the bar on the second screen is still an
    /// application the user has open. Where the compositor describes only the
    /// session as a whole there is no per-display answer to gather, and that
    /// one report is already the session's.
    fn any_app_open(&self) -> bool {
        if self.foreground_is_per_display() {
            return self
                .panels
                .iter()
                .any(|panel| panel.foreground.is_some() || !panel.windows.is_empty());
        }
        self.foreground.is_some() || self.xmb.running_app().is_some()
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
    ///
    /// A game Valve's client started is the exception, and takes its title
    /// first. The reasoning above rests on a class being a name somebody
    /// chose, and for these it is not: a Proton title announces whatever its
    /// binary was called, which is routinely `x86_64`, so the shell offered to
    /// "Close X86_64" while the card directly beside that button said Celeste.
    /// A game has no document open in it either — the objection to titles does
    /// not arise — and the title is what its own card is captioned with, so
    /// this is the two of them agreeing rather than a second opinion.
    ///
    /// Only these, which is why they are written down when they arrive rather
    /// than recognised by their class: see [`Self::steam_windows`]. Anything
    /// the shell is not certain about keeps the old order, so a browser is
    /// still closed by name and not by the tab it is showing.
    fn window_app_name(&self, window: &WindowCard) -> String {
        window_name(
            window,
            self.xmb
                .app_for_window(&window.app_id)
                .map(|app| app.name.as_str()),
            self.steam_windows.contains(&window.id),
        )
    }

    /// Forget the games whose windows have gone.
    ///
    /// Not tidiness: a compositor hands out window ids and is free to use one
    /// again once the window it named is gone, so an id kept past its window
    /// is a claim waiting to be made about somebody else's.
    fn forget_closed_windows(&mut self) {
        if self.steam_windows.is_empty() {
            return;
        }
        let open: HashSet<u32> = self
            .panels
            .iter()
            .flat_map(|panel| panel.windows.iter().map(|window| window.id))
            .collect();
        self.steam_windows.retain(|id| open.contains(id));
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
        // Before the catalogue lands there is deliberately no visible control
        // to act on. A button pressed over the handoff wallpaper must not
        // launch whichever invisible row happened to start selected.
        if !self.startup.ready {
            return;
        }
        self.handle_action(action);
        self.sync_setting_preview();
    }

    /// A move that landed: the frame it changes, and the click the user hears
    /// for it.
    ///
    /// Every place a move lands something goes through here or through
    /// [`Self::stepped_back`] — the bar, a menu, a panel, the on-screen
    /// keyboard — because a move that clicked on the start screen and went
    /// silently over a menu would be two different buttons. Which control made
    /// it is not part of it either: a click on a row of the bar puts the
    /// selection there exactly as a direction does, and the user who made the
    /// same move by hand is owed the same answer. Moves that landed on
    /// *nothing* do neither: an edge the user has pushed into is answered by
    /// nothing happening, and a click there would say something happened.
    ///
    /// The guide overlay is the exception, and it is an exception of voice
    /// rather than of silence: it is a screen of its own rather than another
    /// column of the bar, so it answers a direction with its own click — see
    /// [`Self::guide_stepped`].
    fn stepped(&mut self) {
        self.needs_redraw = true;
        self.sounds.step();
    }

    /// The same for the guide overlay's own controls: its entry column, its
    /// tiles, its bars, its window cards and the power dialog in front of
    /// them.
    ///
    /// The panels raised *out* of the guide are not these. A context menu is
    /// the same component wherever it was raised from and keeps the sounds it
    /// has everywhere else, because a component that changed voice with its
    /// backdrop would be two controls that look alike.
    fn guide_stepped(&mut self) {
        self.needs_redraw = true;
        self.sounds.guide_step();
    }

    /// The same, for the one move that undoes one: a step back out of a
    /// subcategory.
    ///
    /// Whichever control made it — Back is the button for it and Left is the
    /// direction, and they are the same move. The bar is a path walked into,
    /// and answering the way in and the way out with the same noise would
    /// leave the ear no way of telling which way the user is going.
    fn stepped_back(&mut self) {
        self.needs_redraw = true;
        self.sounds.back();
    }

    /// Finish one cursor move with the feedback the place it landed calls for.
    ///
    /// The ordinary click, unless the move walked out of the open path —
    /// including a click that crosses several visible levels at once, which is
    /// still the one move out. This is one call per input action, so that jump
    /// is answered by one back sound rather than one for every column it
    /// crossed.
    /// Move the value the cursor is standing on, when what it is standing on
    /// is a bar. `true` when this press was the bar's, so the cursor never
    /// hears it.
    ///
    /// Up and Down only. Left is how every column in the tree is left and must
    /// go on meaning that here; Right has nowhere further to go, since a bar
    /// opens onto nothing.
    ///
    /// The step is applied like any other setting — the row carries what
    /// pressing it would do, rebuilt from the live value every time the column
    /// is — so nothing about sliding is special except which button did it.
    /// The column is then rebuilt, because the bar's own row *is* the value:
    /// its number, its fill and its colour all move together, and none of them
    /// is a mark that could be moved in place.
    fn step_bar(&mut self, action: Action) -> bool {
        if !matches!(action, Action::Up | Action::Down) {
            return false;
        }
        let Some(panel) = self.panels.get(self.focused_panel) else {
            return false;
        };
        let Some(bar) = panel
            .cursor
            .current_entry(&self.xmb)
            .and_then(apps::Entry::bar)
        else {
            return false;
        };
        let step = match action {
            Action::Up => bar.up,
            _ => bar.down,
        };
        // The end of the range. The press was still the bar's — a cursor that
        // took it would walk out of a column the user is trying to hold a
        // direction in — but nothing moved, so nothing is said about it.
        let Some(step) = step else {
            return true;
        };
        settings::apply(step);
        settings::refresh(&mut self.xmb.categories);
        self.sync_night_light();
        // The same click a row moving under the cursor makes, because that is
        // what happened: this column's one row now reads differently.
        self.stepped();
        true
    }

    fn finish_cursor_move(&mut self, before_depth: usize) {
        match cursor_feedback(before_depth, self.column_depth()) {
            CursorFeedback::Step => self.stepped(),
            CursorFeedback::Back => self.stepped_back(),
        }
    }

    /// Answer one press that was carried out on a screen of the shell's own.
    ///
    /// Split from carrying it out because the two disagree about what the
    /// press *was*: the shell has already begun doing it by the time it could
    /// be asked what happened, and the sound is about what the user chose. So
    /// the classification is read off the selection first — see
    /// [`chosen_feedback`] — and spent here.
    fn answer_choice(&mut self, chosen: ChosenFeedback, screen: Screen) {
        match chosen {
            ChosenFeedback::Silent => {}
            ChosenFeedback::Kept => match screen {
                Screen::Start => self.sounds.select(),
                Screen::Guide => self.sounds.guide_select(),
            },
        }
        self.needs_redraw = true;
    }

    /// How many subcategories deep the driven display's bar is standing. Zero
    /// on the category's own column, and zero when there is no display to ask.
    fn column_depth(&self) -> usize {
        self.panels
            .get(self.focused_panel)
            .map(|panel| panel.cursor.depth())
            .unwrap_or(0)
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
                // A launch is not something the menu opens over. See
                // [`guide_answers`]: the press is spent here and nothing is
                // said about it, because nothing happened.
                if !self.guide_answers_the_press() {
                    return;
                }
                // Read before the mode flips: `is_over_app` is true of the
                // menu too, and what matters is whether the bar was the thing
                // on screen before this press.
                let bar_on_top = self.guide.is_over_app();
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
                    // The one sound in the shell that belongs to a button
                    // rather than to a screen, and it is spent here rather than
                    // in `open_guide` on purpose: the guide also comes up for a
                    // portal's question and for an authorisation, and neither
                    // of those is somebody asking for it. Only the press that
                    // *opened* it — closing the overlay is the screen behind it
                    // coming back, which needs nothing said about it.
                    self.sounds.guide_open();
                    self.begin_home_flight(bar_on_top);
                }
                self.needs_redraw = true;
            }
            Action::Keyboard => {
                self.context_menu.close();
                self.close_dialog();
                self.toggle_keyboard();
            }
            // Ahead of everything that could be on screen, and it closes none
            // of it. A photograph is of what is in front of the user at that
            // instant, panels and boards and menus included: a chord that
            // tidied the screen before photographing it would be a chord that
            // cannot photograph the screen.
            Action::Screenshot => self.screenshot_driven_display(),
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
                        self.answer_choice(ChosenFeedback::Kept, Screen::Start);
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
                // A search is neither opened nor launched: the field takes the
                // keyboard, and the row under it puts the whole shelf back.
                if self.press_search_row() {
                    self.answer_choice(ChosenFeedback::Kept, Screen::Start);
                    return;
                }
                if let Some(setting) = chosen {
                    settings::apply(setting);
                    // A sound device is the sound server's to carry out, as a
                    // display setting is the compositor's, and it is handed
                    // over here for the same reason: `settings` records what
                    // the shell remembers, and neither of these is that.
                    if let settings::Setting::SoundDevice { direction, id } = setting {
                        self.quick.use_device(direction, id);
                    }
                    // The night light is the one page in this tree whose
                    // *shape* depends on what was just chosen: asking it to
                    // keep hours puts two rows on the page that were not there
                    // a moment ago, and taking the hours away takes them off
                    // again. Nothing else would rebuild that — the compositor
                    // has no event for a schedule it never sees — so the
                    // column is rebuilt here, by the same call every other
                    // change to its contents goes through.
                    //
                    // Every one of the five, not only the ones that add a row:
                    // all five write the comment on the row above them, and a
                    // page left standing says the wrong hours until something
                    // else happens to rebuild it.
                    if matches!(
                        setting,
                        settings::Setting::Display {
                            value: settings::DisplayValue::NightLight(_)
                                | settings::DisplayValue::NightLightTemperature(_)
                                | settings::DisplayValue::NightLightSchedule(_)
                                | settings::DisplayValue::NightLightFrom(_)
                                | settings::DisplayValue::NightLightUntil(_),
                            ..
                        }
                    ) {
                        settings::refresh(&mut self.xmb.categories);
                    }
                    // The accent needs nobody told; a display setting does,
                    // and straight away rather than on the next loop pass —
                    // the user has just pressed a button and is waiting to see
                    // whether the screen changed.
                    self.sync_hdr();
                    self.sync_night_light();
                    self.answer_choice(ChosenFeedback::Kept, Screen::Start);
                    return;
                }
                self.start_selection();
            }
            _ => {
                // A value on a bar answers Up and Down itself. It is the one
                // row of its column, so there is nowhere for the cursor to
                // move — and moving the *value* is what those two mean once
                // the column under them is a scale rather than a list. Left is
                // untouched and still leaves, which is the whole reason a bar
                // can live in a column at all.
                if self.step_bar(action) {
                    return;
                }
                // How deep in the path the cursor was, so a Left that came
                // back out of a subcategory can be told from one that walked
                // along the category row. They are one call and one answer —
                // see [`Cursor::navigate`], where coming out is what Left
                // means first — so what separates them is the depth either
                // side of it rather than anything the move itself reports.
                let before = self.column_depth();
                let moved = match self.panels.get_mut(self.focused_panel) {
                    Some(panel) => panel.cursor.navigate(action, &self.xmb),
                    None => false,
                };
                if moved {
                    self.finish_cursor_move(before);
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
        // it. One layer at a time inside the panel as well: a menu that has
        // stepped into a further list steps back out of it first, because that
        // list was reached by a press and Back undoes one press.
        if self.context_menu.is_open() {
            if !self.context_menu.back() {
                self.context_menu.close();
            }
            self.needs_redraw = true;
            return;
        }
        // The keyboard is in front of everything else the shell draws, so it
        // is the first thing Back takes away — and the field it was typing
        // into is still there afterwards, which is what the corner hint is
        // then for. A board that came up for the shell's own password field is
        // the exception: taking it away leaves the field with nothing to fill
        // it in, so Back means "leave this panel" and is handled above.
        //
        // A search field is not that exception, though it is also the shell's
        // own. It is a row on the bar rather than a modal, so what is behind
        // the board is a column the user can carry on using — and what they
        // have typed stays on the row, narrowing it, exactly as it was. Back
        // out of the field is not back out of the search; the row that says
        // "Clear search" is the only thing that undoes one.
        if !self.password_wanted() && self.osk.close() {
            self.searching = None;
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
                    self.stepped_back();
                } else if self.guide_answers_the_press() {
                    // The other door into the same menu, and it is held shut
                    // by the same rule. A launch left this one open would make
                    // "the menu does not come up during a launch" false, and
                    // in the worst way: the bar the step is out of is under
                    // the splash, so the menu would be raised unseen and be
                    // there when the application arrived.
                    self.open_guide();
                }
            }
        }
    }

    /// Whether a press of the user's may raise the menu at all.
    ///
    /// The rule is [`guide_answers`]; this reads the two facts it needs off
    /// the shell. Both of the places the user can open the menu from ask —
    /// the Home button and a step back from the top of the bar — and nothing
    /// else does, which is the point: a portal's question and an
    /// authorisation raise the menu through [`Self::open_guide`] without
    /// coming past here, because those are not the user asking and they need
    /// an answer whatever else is going on.
    fn guide_answers_the_press(&self) -> bool {
        guide_answers(self.launching_on(self.focused_panel), self.guide.is_menu())
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
        // The two rows Steam put on the bar are answered before anything else:
        // the service row raises a panel or moves the bar, and an installed
        // title with no safe native target explains the missing capability.
        match self.panels.get(self.focused_panel).and_then(|panel| {
            panel
                .cursor
                .current_entry(&self.xmb)
                .map(|entry| (entry.service().is_some(), entry.game().cloned()))
        }) {
            Some((true, _)) => return self.press_steam_row(),
            // A game the account owns and this machine has not got is a press
            // that means "get it": the one row on the bar whose press is
            // answered by offering rather than by starting.
            Some((_, Some(game))) if !game.installed => return self.offer_to_install(&game),
            _ => {}
        }
        // It is come back to, never started a second time. A console has one of
        // each thing running, and a tile that silently produced a second copy
        // would also produce two cards to close, of which closing either could
        // take both — the application is what Close ends, not the window.
        if let Some((display, window)) = self.window_for_selection() {
            self.restore_window(display, window);
            // The same answer a fresh start gets. No process is forked for
            // this one, but the screen changes hands exactly as it would, and
            // that is the half of it the user is watching.
            self.sounds.launch();
            return;
        }
        // Say where this is being launched from before starting it, so the
        // answer cannot be overtaken by the window itself.
        self.sync_launch_output();
        // A game that links Steamworks cannot be forked here: something has to
        // answer it, and what this shell answers with has to be asked of Steam
        // first. That is a round trip, so it takes the press and returns.
        if self.ask_steam_before_starting() {
            return;
        }
        let Some(panel) = self.panels.get(self.focused_panel) else {
            return;
        };
        // Everything the splash needs, read before the launch: the tile it
        // opens out of, and what was already on the display, so the
        // application's own window can be told from them.
        // A file is answered for by its own name and its own mark, not by the
        // player's, and a game under its own name and the mark of the column
        // it came from rather than under Steam's: the user pressed a song or a
        // game, and a splash that said "VLC" or "Steam" would be about a
        // program they never chose to think about.
        let opening = match panel.cursor.current_entry(&self.xmb) {
            Some(apps::Entry::App(app)) => Some((app.name.clone(), app.icon.clone())),
            Some(apps::Entry::Media(file)) => {
                Some((file.title.clone(), Some(file.kind.glyph().to_string())))
            }
            Some(apps::Entry::Game(game)) => {
                Some((game.name.clone(), Some(icons::STEAM.to_string())))
            }
            _ => None,
        };
        let from = ui::launch_origin(panel.width as f32, panel.height as f32);
        let known: Vec<u32> = panel.windows.iter().map(|window| window.id).collect();
        let foreground = panel.foreground.clone().unwrap_or_default();
        // Disjoint fields, so the shared catalogue can be mutated while this
        // display's cursor is read.
        match self.xmb.launch_selected(&panel.cursor) {
            Some(pid) => {
                self.needs_redraw = true;
                // Something is starting. Here rather than beside the splash,
                // which is only built for a row the shell can name: the sound
                // is about the process, and a launch nobody could put a title
                // on is still a launch. And not before the process exists — a
                // press that started nothing must not be answered as though it
                // had.
                self.sounds.launch();
                if let Some((name, icon)) = opening {
                    let splash = launch::Launch::new(
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
                    );
                    self.launching = Some(splash);
                }
            }
            None => {
                // Nothing was forked. An ordinary application that will not
                // start has already been reported by the launcher; there is
                // nothing left to say here.
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
        if self
            .context_menu
            .open_at(anchor, title.map(menu::Title::new), entries, rows)
        {
            self.needs_redraw = true;
        }
    }

    /// The menu for the bar's focused tile.
    ///
    /// Two of them, because the bar holds two kinds of thing a menu can be
    /// about: an application that was installed, and a file the user made or
    /// downloaded. A subcategory and a settings value are neither — they are
    /// rows that lead somewhere or mean something rather than objects with a
    /// life of their own — and neither has anything a menu would offer.
    ///
    /// The two lists have not one row in common, which is why they are two
    /// functions rather than one with the differences picked out inside it.
    /// Everything the application menu offers is about an *installation*: what
    /// put it there, how much disk it takes, how to take it off. None of those
    /// is a question about a song, and the one that looks closest —
    /// Uninstall — is the one that would be most wrong, because a piece of
    /// music has no package to remove and the only thing the row could mean is
    /// deleting it.
    fn bar_entry_menu(&self) -> Option<([f32; 4], Option<String>, Vec<menu::Entry>)> {
        if self.selected_media().is_some() {
            return self.media_entry_menu();
        }
        // Steam's two rows are two more kinds of thing, and neither shares a
        // row with the other two: nothing on this machine installed a game in
        // somebody's Steam library, so there is no package to survey and no
        // file to delete — what can be done to one is Steam's list, not the
        // package manager's. See `game_entry_menu` and `service_entry_menu`.
        if self.selected_game().is_some() {
            return self.game_entry_menu();
        }
        if self.selected_service().is_some() {
            return self.service_entry_menu();
        }
        self.application_entry_menu()
    }

    /// The menu for an installed application: the disc it stands on, the name
    /// of what is on it, and what can be done to it.
    ///
    /// Two bands: what the menu can *tell* the user about the application and
    /// what it can do to the installation, then what it can do with the
    /// application right now. Close is in the second band because it is the way
    /// out of the menu rather than something done to the tile — on the bar
    /// nothing is running yet, so there is nothing here for a Close to end.
    fn application_entry_menu(&self) -> Option<([f32; 4], Option<String>, Vec<menu::Entry>)> {
        let panel = self.panels.get(self.focused_panel)?;
        let app = panel.cursor.current_app(&self.xmb)?;
        let anchor = ui::launch_origin(panel.width as f32, panel.height as f32);
        Some((
            anchor,
            Some(app.name.clone()),
            vec![
                menu::Entry::new(menu::Command::Information, "Information")
                    .glyph(icons::SETTING_INFO),
                // Grave, even though this row removes nothing by itself and
                // only asks. Where it leads is what the highlight has to say:
                // a row that looked like Information and Launch until the
                // question was already on the screen would have said nothing at
                // the one moment the user was still choosing whether to go
                // there. The button that answers is stronger again — it is
                // `destructive`, in the shell's fixed red.
                menu::Entry::new(menu::Command::Uninstall, "Uninstall")
                    .glyph(icons::UNINSTALL)
                    .grave(),
                menu::Entry::new(menu::Command::Launch, "Launch")
                    .glyph(icons::LAUNCH)
                    .group(1),
                menu::Entry::new(menu::Command::Dismiss, "Close").group(1),
            ],
        ))
    }

    // --- the menu over one of the user's own files -------------------------

    /// The menu for a song, a film or a photograph on the bar.
    ///
    /// Five rows in two bands. The first three act on the file itself, in the
    /// order somebody reaches for them — open it, open it in something else,
    /// get rid of it. The last two are not about the file at all: Sort is about
    /// the *column*, and Cancel is about the menu. That is what the rule between
    /// them is saying, and it is why Sort is below it rather than up with the
    /// commands it is nothing like.
    ///
    /// Two of the five can be unavailable, and both are drawn greyed rather
    /// than left out, so the panel keeps its shape wherever it is raised:
    ///
    /// * **Open with**, when nothing installed says it handles this type. There
    ///   is then nothing to choose *between* — Open would fall through to
    ///   `xdg-open`, which is not an answer to "which program".
    /// * **Delete**, for a file that is not under the user's home directory.
    ///   See [`crate::trash::is_the_users_own`].
    fn media_entry_menu(&self) -> Option<([f32; 4], Option<String>, Vec<menu::Entry>)> {
        let panel = self.panels.get(self.focused_panel)?;
        let file = self.selected_media()?;
        let anchor = ui::launch_origin(panel.width as f32, panel.height as f32);
        Some((
            anchor,
            Some(file.title.clone()),
            media_rows(
                !media::handlers(file, &self.xmb.categories).is_empty(),
                trash::is_the_users_own(&file.path),
            ),
        ))
    }

    /// The Open with list: every application that says it opens this kind of
    /// file, best first, each under its own name and its own picture.
    ///
    /// The first row is always the one a plain Open would use, because both
    /// come out of [`media::handlers`]. A tick marks it, the same tick the
    /// Settings column puts on the value a setting is currently set to — it is
    /// the same statement, that this is the answer already in force.
    ///
    /// Held as well as listed. The rows carry a position in this list and
    /// nothing else, so the list has to outlive the press that chose one.
    fn open_with_entries(&mut self) -> Vec<menu::Entry> {
        let Some(file) = self.selected_media().cloned() else {
            return Vec::new();
        };
        self.open_with = media::handlers(&file, &self.xmb.categories)
            .into_iter()
            .filter_map(|app| media::handler_row(&file, app))
            .collect();
        // The head of the list, because that is where [`media::handlers`] puts
        // the answer already in force.
        self.open_with_chosen = 0;
        open_with_rows(&self.open_with, self.open_with_chosen)
    }

    /// The Sort list: the nine orders a shelf can be listed in, with the one it
    /// is in now ticked.
    ///
    /// An order that this collection cannot be put in is drawn greyed. Several
    /// filesystems keep no creation time at all, and on one of those the two
    /// Created rows would otherwise be offered, chosen, and do nothing — which
    /// the user would read as the shell being broken rather than as the disk
    /// not knowing.
    fn sort_entries(&self) -> Vec<menu::Entry> {
        let Some(kind) = self.selected_media().map(|file| file.kind) else {
            return Vec::new();
        };
        sort_rows(self.media.sort(kind), self.media.orders(kind))
    }

    /// The file the bar's cursor is on, if it is on one.
    fn selected_media(&self) -> Option<&media::File> {
        self.panels
            .get(self.focused_panel)?
            .cursor
            .current_entry(&self.xmb)
            .and_then(apps::Entry::media)
    }

    /// The search row the bar's cursor is on, if it is on one of the two.
    fn selected_search(&self) -> Option<&apps::Search> {
        self.panels
            .get(self.focused_panel)?
            .cursor
            .current_entry(&self.xmb)
            .and_then(apps::Entry::search)
    }

    /// Act on a press of one of the two rows at the head of a shelf. Returns
    /// whether the press was one of theirs.
    ///
    /// The field takes the keyboard rather than doing anything itself, which is
    /// the whole of what makes it a field: what a press on it means is "I am
    /// about to type", and the answer to that is a board.
    fn press_search_row(&mut self) -> bool {
        let Some((kind, role, query)) = self
            .selected_search()
            .map(|search| (search.kind, search.role, search.query.clone()))
        else {
            return false;
        };
        match role {
            apps::Role::Field => self.open_search_field(kind, query),
            // Straight to the whole shelf, with no board raised over it: the
            // row says what it does and there is nothing to type. The cursor
            // is left where it is, which is the row that has just gone — so it
            // lands on the field above it, one row up, and the user is looking
            // at the head of their own collection again.
            apps::Role::Clear => {
                tracing::info!(shelf = apps::shelf_title(kind), "cleared the search");
                self.set_search(kind, String::new());
            }
        }
        true
    }

    /// Raise the keyboard over the field at the head of `kind`'s shelf.
    ///
    /// It types *here* rather than through the virtual keyboard — see
    /// [`keyboard::Osk::open_here`] — for the same reason the password field
    /// does: what is being typed belongs to the shell, and a letter sent
    /// through the virtual keyboard would go to whichever client is holding
    /// the keys. That it also opens on a machine with no virtual keyboard at
    /// all is the other half of the bargain, and here it is the difference
    /// between a searchable collection and a field that could never be filled
    /// in.
    fn open_search_field(&mut self, kind: media::Kind, query: String) {
        self.searching = Some(Searching { kind, text: query });
        self.osk.open_here();
        self.sync_surface_state();
        self.needs_redraw = true;
    }

    /// Apply one keystroke to the search being typed. Returns whether there was
    /// a field for it to go into.
    fn type_into_search(&mut self, stroke: keyboard::Stroke) -> bool {
        // Not merely that a search was opened, but that its field is still the
        // row under the cursor. The bar can be walked away from underneath a
        // board — a pointer resting on the category row is enough — and a
        // letter that went on being filed into a field nobody is looking at
        // would be the shell typing somewhere the user cannot see.
        if !self.search_wanted() {
            return false;
        }
        // Whether there is a field at all, and what the key did to it, decided
        // with only the search borrowed: the two keys that finish typing need
        // the whole shell afterwards.
        let done = {
            let Some(searching) = self.searching.as_mut() else {
                return false;
            };
            match stroke {
                keyboard::Stroke::Char(character) => {
                    searching.text.push(character);
                    None
                }
                keyboard::Stroke::BACKSPACE => {
                    searching.text.pop();
                    None
                }
                // Done, and take me to it.
                keyboard::Stroke::ENTER => Some(true),
                // Away, and the search stands. Escape does not put back what
                // was there before, and that is deliberate: the column has
                // been narrowing under the user's eyes with every letter, so
                // there is no earlier list still on screen for "cancel" to
                // mean. What undoes a search is the row that says it does.
                keyboard::Stroke::ESCAPE => Some(false),
                // Tab, the arrows, the function keys. A field of one line has
                // no use for any of them, and letting them past to the bar
                // underneath would move the cursor off the row being typed
                // into.
                _ => return true,
            }
        };
        match done {
            Some(reached) => {
                self.close_search_field();
                // Down to the first thing the search found, because somebody
                // who has just finished typing a name is asking to be taken to
                // what it names — and leaving them on the field with the
                // answer below it would make them ask twice.
                if reached {
                    self.rest_on_first_match();
                }
            }
            None => self.refresh_search(),
        }
        true
    }

    /// Put what has been typed on the field, and narrow the shelf to it.
    ///
    /// Two different speeds on purpose. The letter is written onto the row
    /// here, so it is on the next frame; the shelf is narrowed on the worker
    /// and its rows arrive when they are ready. See
    /// [`media::Library::set_search`].
    fn refresh_search(&mut self) {
        let Some(searching) = self.searching.as_ref() else {
            return;
        };
        let (kind, text) = (searching.kind, searching.text.clone());
        apps::set_search_text(&mut self.xmb.categories, kind, &text);
        self.media.set_search(kind, &text);
        self.needs_redraw = true;
    }

    /// Narrow a shelf without a board being up: the Clear row, and anything
    /// else that changes a search outright rather than a letter at a time.
    fn set_search(&mut self, kind: media::Kind, query: String) {
        if let Some(searching) = self.searching.as_mut().filter(|open| open.kind == kind) {
            searching.text = query.clone();
        }
        apps::set_search_text(&mut self.xmb.categories, kind, &query);
        self.media.set_search(kind, &query);
        self.needs_redraw = true;
    }

    /// Put this display's cursor on the first file the search found.
    ///
    /// The rows it lands among are the ones the *last* delivery brought, which
    /// while a large shelf is still being narrowed may not be the final answer.
    /// That is the right list to move into all the same: it is the one on the
    /// screen the user is looking at, and a cursor that waited for the shelf to
    /// settle would be a press that did nothing.
    fn rest_on_first_match(&mut self) {
        let Some(panel) = self.panels.get(self.focused_panel) else {
            return;
        };
        let first = panel
            .cursor
            .current_entries(&self.xmb)
            .iter()
            .position(|entry| entry.media().is_some());
        // Nothing matched, so there is nowhere to go and the field keeps the
        // cursor — which is where the user will want it, since the next thing
        // to do with a search that found nothing is change it.
        let Some(first) = first else {
            return;
        };
        // Disjoint fields: this display's cursor is moved while the catalogue
        // it is being moved through is read.
        let xmb = &self.xmb;
        if let Some(panel) = self.panels.get_mut(self.focused_panel) {
            if panel.cursor.point_at_row(first, xmb) {
                self.needs_redraw = true;
            }
        }
    }

    /// End a search the cursor is no longer standing in.
    ///
    /// The field is a row on the bar rather than a panel over it, so the bar
    /// can be walked out from under the board without the field ever being
    /// touched: a pointer resting on the category row moves the cursor, and so
    /// does control passing to another display, or the shelf emptying beneath
    /// it. None of those goes anywhere near the field, so the question is asked
    /// once a frame after every source of input has settled — exactly as the
    /// settings preview is, and for exactly the same reason.
    ///
    /// The board goes with it. A keyboard left standing over a column that
    /// nobody is typing into is the one thing on the screen with nothing to do,
    /// and the keys it swallows are the ones that would have moved the bar.
    fn sync_search(&mut self) {
        if self.searching.is_none() || self.search_wanted() {
            return;
        }
        tracing::debug!("the cursor left the search field; the board goes with it");
        self.close_search_field();
    }

    /// Put the board away and leave the field with what was typed in it.
    fn close_search_field(&mut self) {
        self.searching = None;
        if self.osk.close() {
            self.sync_surface_state();
        }
        self.needs_redraw = true;
    }

    /// What the row the cursor is standing inside is called — "Music", "Video",
    /// "Images" — which is what the Sort list is titled after.
    ///
    /// The column and not the file: what is being ordered is the whole shelf,
    /// and a panel headed with the name of one song would be saying it was
    /// about that song.
    fn selected_column_title(&self) -> Option<String> {
        let kind = self.selected_media()?.kind;
        Some(apps::shelf_title(kind).to_string())
    }

    /// Make the application the Open with list offered at `index` the one that
    /// opens files of this type.
    ///
    /// It does not open anything. The list answers "which program opens these",
    /// and that is a question about every file of the type rather than about
    /// the one the menu was raised over — so what a press does is write the
    /// answer down, and Open is what acts on it. Opening the file as well would
    /// make the two rows of the menu impossible to tell apart: a user who
    /// wanted to *change* which program handles their photographs would have to
    /// watch one of them open every time they did it.
    ///
    /// Written where every other desktop keeps it, so it holds outside this
    /// shell and after a restart. See [`media::make_default`].
    ///
    /// The panel stays up, and the tick moves to the row that was pressed. That
    /// is the whole answer — there is nothing else to see — and it is the same
    /// bargain the mixer's tracks strike: a list where exactly one row is in
    /// force is a control being *set*, so it is the user's to leave when they
    /// are satisfied rather than the shell's to close after one press.
    fn choose_default_handler(&mut self, index: usize) {
        let Some(handler) = self.open_with.get(index).cloned() else {
            return;
        };
        if !media::make_default(&handler) {
            // The tick stays where it was: nothing was written, so nothing
            // about what opens these files has changed.
            return;
        }
        tracing::info!(
            mime = handler.mime,
            with = %handler.name,
            entry = %handler.id,
            "this is what opens files of this type now"
        );
        self.open_with_chosen = index;
        if self
            .context_menu
            .refresh(open_with_rows(&self.open_with, index))
        {
            self.needs_redraw = true;
        }
    }

    /// List the shelf the cursor is standing in in a different order.
    ///
    /// Written down as well as applied. An order is a preference rather than an
    /// action — nobody chooses "largest first" meaning "until I next start the
    /// shell" — so it goes in the settings file beside the accent and the
    /// display modes, per shelf, because how somebody wants their music listed
    /// says nothing about how they want their photographs listed.
    fn sort_selected_shelf(&mut self, sort: media::Sort) {
        let Some(kind) = self.selected_media().map(|file| file.kind) else {
            return;
        };
        if !self.media.set_sort(kind, sort) {
            return;
        }
        tracing::info!(
            shelf = apps::shelf_title(kind),
            order = sort.key(),
            "listing a shelf in a different order"
        );
        settings::remember_media_sort(kind, sort);
        // At the top of it, on the display that asked. Somebody who has just
        // said "newest first" is asking to be shown the newest, and a cursor
        // held on the file it happened to be standing on would answer with that
        // file's new position instead — which on a large shelf is somewhere in
        // the middle of a list they never see the head of. Every other display
        // keeps its file, because the order changed underneath it rather than
        // at its request.
        //
        // Twice over, and deliberately. The cursor goes to the top now, so the
        // press is answered on the frame it lands rather than whenever the
        // worker has finished reordering half a million rows; and the note here
        // is what puts it there *again* when they arrive, because otherwise the
        // ordinary rule would take over and put the cursor back on the file it
        // is standing on. The first row is the first row either way, so the
        // second one moves nothing the user can see.
        self.sorting = Some((kind, self.focused_panel));
        // Disjoint fields: this display's cursor is moved while the catalogue
        // it is being moved through is read.
        let xmb = &self.xmb;
        if let Some(panel) = self.panels.get_mut(self.focused_panel) {
            panel.cursor.rest_on_first_row(xmb);
        }
        self.needs_redraw = true;
    }

    /// Ask whether to delete the selected file.
    ///
    /// The one question in the shell about something the user made rather than
    /// something they installed, and the panel says where the file is going.
    /// "Delete" is what the row is called because that is what the user means
    /// by it; the trash is where it lands, and a person who has just deleted a
    /// photograph by accident needs to be told, on the way past, that there is
    /// somewhere to get it back from.
    fn ask_to_delete(&mut self, from: [f32; 4]) {
        let Some(file) = self.selected_media().cloned() else {
            return;
        };
        // Nothing else may be waiting on this panel.
        self.app_facts = None;
        self.removal_plan = None;
        let name = file.title.clone();
        let icon = file.kind.glyph().to_string();
        self.deleting = Some(file);
        self.dialog.ask(
            from,
            Some(icon),
            vec![
                // Broken across two lines at the one place it can always be
                // broken, for the reason the uninstall question is: every run
                // the shell draws is a single line with an ellipsis where the
                // rest would have been, and the name is the half that must not
                // be the half that gets cut.
                dialog::Line::Note("Do you want to delete".to_string()),
                dialog::Line::Heading(format!("{name}?")),
                dialog::Line::Note("It will be moved to the trash.".to_string()),
                dialog::Line::Rule,
            ],
            vec![
                menu::Entry::new(menu::Command::Dismiss, "No"),
                menu::Entry::new(menu::Command::ConfirmDelete, "Yes").grave(),
            ],
            // On No, which is the row it is standing on as well as the row it
            // is drawn first — see the uninstall question, which this follows.
            0,
        );
    }

    /// The user has said yes.
    ///
    /// The row goes as soon as the file does, rather than at the walk's next
    /// pass five minutes later: the user has just watched themselves delete
    /// something, and a bar still offering to play it is a bar that has not
    /// understood. A failure is reported in a panel rather than a line in the
    /// log, for the reason a screenshot's failure is — a button that silently
    /// either worked or did not is a button nobody trusts twice.
    fn delete_the_file(&mut self, from: [f32; 4]) {
        let Some(file) = self.deleting.take() else {
            return;
        };
        match trash::discard(&file.path) {
            Ok(_) => {
                // The row goes from the bar here and the file goes from the
                // shelf on the worker, rather than the shelf being rebuilt and
                // sent back: one row has gone, and rebuilding the column to say
                // so would be the whole collection's worth of work for it.
                self.media.forget(&file.path);
                let xmb = &mut self.xmb;
                if apps::forget_media(&mut xmb.categories, &file.path) {
                    self.needs_redraw = true;
                }
            }
            Err(err) => {
                tracing::warn!(
                    %err,
                    file = %file.path.display(),
                    "could not move a file to the trash"
                );
                self.dialog.ask(
                    from,
                    Some(file.kind.glyph().to_string()),
                    vec![
                        dialog::Line::Heading(file.title.clone()),
                        dialog::Line::Note("This could not be deleted.".to_string()),
                        dialog::Line::Rule,
                    ],
                    vec![menu::Entry::new(menu::Command::Dismiss, "Close")],
                    0,
                );
            }
        }
    }

    // --- the volume mixer --------------------------------------------------

    /// Raise the mixer out of the tile in the guide's column: the application
    /// in front, every application making a noise, and the session's own output
    /// under them.
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
        // It always opens: the shell's own row is on the list whatever the
        // machine is playing, and whatever the machine can be asked about its
        // playing. That is what makes the tile beside the stick pointer one
        // that is never dimmed.
        if self
            .context_menu
            .open_at(anchor, None, entries, ui::mixer_rows_that_fit(height))
        {
            self.needs_redraw = true;
        }
    }

    /// The mixer's rows: the application in front, then the sounds the server
    /// is playing, with the shell's own audio at the foot of the list.
    ///
    /// The application in front is first and is there whether or not it is
    /// making a sound — see [`system::InFront`], which is where its row comes
    /// from and why it has to exist at all. It leads because it is the one the
    /// user is looking at and so the one they opened this about, which also
    /// puts the highlight on it the moment the panel appears.
    ///
    /// The shell is last and in a band of its own because it is not one of the
    /// applications: it is the thing the panel is being drawn *by*, and it is
    /// the only row here that no sound server knows about. A user looking for
    /// the game they can hear should find it before they find the interface.
    ///
    /// The session's own output is deliberately not a row. It is the thing all
    /// of these play through, and it already has a control — the volume bar in
    /// the sidebar, which is why that bar is there whether or not this panel
    /// opens. Putting it here as well would be the same control twice, and the
    /// shell's own audio would have nowhere to be set from.
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
        // Always, unlike every row above it: the applications come and go with
        // what the machine is playing, and the shell is the one thing on the
        // list that is certainly there — it is the thing drawing the list.
        entries.push(
            menu::Entry::new(menu::Command::MuteShell, "System")
                .icon(icons::CATEGORY_SYSTEM)
                .level(settings::sound())
                .group(1),
        );
        entries
    }

    // --- what the machine has announced --------------------------------------

    /// Take what the bus has said, move the bubbles on, and answer the programs
    /// that are owed an answer.
    ///
    /// Once a frame, like every other worker the shell listens to, and for the
    /// same reason: nothing here is a Wayland event. The three steps are in
    /// this order on purpose — what has just arrived is filed before the corner
    /// is advanced, so a bubble is never a frame late, and the programs are
    /// answered last so that one pass tells them about everything this frame
    /// did, including the announcements it has only just taken.
    fn sync_notifications(&mut self) {
        let Some(service) = self.notifier.as_ref() else {
            return;
        };
        let standing = !self.notifications.toasts().is_empty();
        let arrived = self.notifications.collect(service);
        // Once for the frame, however many came in on it, and only for one
        // that put something in the corner — see `sound::Sounds::notified`.
        if arrived.raised {
            self.sounds.notified();
        }
        let moving = self.notifications.animate();
        self.notifications.answer(service);
        // The first bubble lifts this display's surface to the overlay layer
        // and the last one lets it back down — see `Guide::surface_state`. Only
        // when that changes: the state is applied by comparison, but working
        // it out costs a pass over every display, and the corner is empty for
        // almost the whole of a session.
        if standing != !self.notifications.toasts().is_empty() {
            self.sync_surface_state();
        }
        if arrived.changed {
            // Whatever picture the new ones asked for, before anything tries
            // to draw them.
            self.load_notification_icons();
            // The open panel is a list of the very things that have just
            // changed, so it is brought up to date rather than left showing
            // what was true when it opened — the same bargain the mixer
            // strikes with the sound server.
            self.sync_notification_panel();
        }
        if arrived.changed || moving {
            self.needs_redraw = true;
        }
    }

    /// Find and decode the pictures the announcements on hand asked for, and
    /// put them in the atlas.
    ///
    /// The one place in the shell that adds an icon after the first frame, and
    /// it has to be: everything else draws icons the launcher's catalogue
    /// named, which is settled before the session starts, while an
    /// announcement carries whatever its sender chose. Usually that is
    /// something already in the atlas — the sender's own application icon —
    /// and this does nothing at all. It is `--icon=software-update-available`,
    /// or a path into a package's own share directory, that has nowhere to be
    /// drawn from until this runs.
    ///
    /// Done here, on the frame the announcement arrives, rather than where the
    /// row is drawn: drawing happens once per display per frame and must not
    /// be reading files, and the atlas needs `&mut` in any case.
    ///
    /// Synchronously, which is worth being deliberate about. Finding an icon
    /// means stat-ing a handful of theme directories and decoding one picture,
    /// and the shell already does exactly that a few hundred times before it
    /// draws anything. Doing one more on the frame an announcement lands is
    /// well inside a frame's budget, and the alternative — handing it to a
    /// worker — would mean a bubble that appears without its picture and
    /// gains it a moment later, which is worse than the cost it saves.
    fn load_notification_icons(&mut self) {
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        // Only what could be drawn: the list behind the bell is every
        // announcement there is, so it covers the corner as well.
        let wanted: Vec<usize> = self
            .notifications
            .list()
            .iter()
            .enumerate()
            // Already in the atlas, from the catalogue or from an earlier
            // announcement. Asked before any file is touched, because the
            // common case is a program whose icon the launcher already knows
            // and the right amount of work for that is none at all.
            .filter(|(_, held)| {
                held.icon_name()
                    .is_some_and(|name| gpu.slot(name).is_none())
            })
            .map(|(index, _)| index)
            .collect();

        for index in wanted {
            // The name, and the picture decoded from the pixels if that is
            // what was sent. Taken together in one look so that the borrow of
            // the list ends before the loader and the atlas are reached, both
            // of which are other fields of this shell.
            let Some((name, decoded, carried_pixels)) =
                self.notifications.list().get(index).map(|held| {
                    (
                        held.icon_name().unwrap_or_default().to_string(),
                        // A picture that came as bytes is decoded from them.
                        // There is nothing to look up: the name it is filed
                        // under was made *out of* those pixels, precisely
                        // because a picture sent this way has none of its own.
                        held.image().and_then(|image| {
                            icons::Icon::from_pixels(
                                image.width,
                                image.height,
                                image.stride,
                                image.channels,
                                &image.data,
                                ICON_SIZE,
                            )
                        }),
                        held.image().is_some(),
                    )
                })
            else {
                continue;
            };
            if name.is_empty() {
                continue;
            }

            let icon = match decoded {
                Some(icon) => Some(icon),
                // Nothing to search the theme for. A picture that arrived as
                // bytes and would not decode is not going to be found filed
                // under its own hash.
                None if carried_pixels => {
                    tracing::debug!(
                        icon = name,
                        "an announcement sent a picture that could not be read"
                    );
                    None
                }
                None => self.icon_theme.load(&name, ICON_SIZE),
            };
            let Some(icon) = icon else {
                // Not a failure worth telling the user about: a program is
                // free to name an icon this machine has not got, and what is
                // drawn instead is the shell's own bell, which is true — this
                // *is* an announcement.
                tracing::debug!(
                    icon = name,
                    "an announcement named an icon this machine has not got"
                );
                continue;
            };
            if let Some(gpu) = self.gpu.as_mut() {
                gpu.put_icon(&name, &icon);
            }
        }
    }

    /// Raise the notification list out of its tile in the guide's column.
    ///
    /// The same panel the mixer is drawn in, because it is the same kind of
    /// object: a short list about one thing, grown out of the control that is
    /// about it. What makes it a notification list rather than a mixer is the
    /// entries and nothing else.
    fn open_notifications(&mut self) {
        let Some((width, height)) = self.focused_size() else {
            return;
        };
        let items = self.guide.items(self.closable());
        let Some(index) = items.iter().position(|item| *item == Item::Notifications) else {
            return;
        };
        let anchor = ui::menu_item_rect(&items, index, width, height);
        let entries = self.notification_entries();
        // A title, unlike the mixer's. The mixer is a column of applications
        // each carrying its own name and picture, so a header would be telling
        // the user what they can already see; this list can be *empty*, and a
        // panel with one row in it saying "Nothing to read" needs to say what
        // it is a panel of before that row means anything.
        // Opened on the announcement under Clear All, never on Clear All
        // itself. The first row of this panel is the one row that throws
        // everything away, and a panel that opens with it already highlighted
        // would hand that to anyone who opens the list and presses accept the
        // way they press accept everywhere else. It is a press of Up from
        // where the highlight starts, which is near enough to reach and far
        // enough not to be walked into — the same bargain a confirmation
        // makes by opening on No.
        if self.context_menu.open_selecting(
            anchor,
            Some(menu::Title::new("Notifications")),
            entries,
            ui::mixer_rows_that_fit(height),
            1,
        ) {
            // Wider than every other panel in the shell — see
            // [`ui::NOTIFICATION_EXTRA_WIDTH`]. This is the one list whose rows
            // carry somebody else's sentences and a button of their own, and it
            // stays wide for the announcement stepped into from it.
            self.context_menu.widen(ui::NOTIFICATION_EXTRA_WIDTH);
            // The list being on screen is what *read* means, so the mark on the
            // bell starts going out as the panel it points at grows. Only on
            // the panel actually opening: a press that raised nothing has shown
            // the user nothing.
            self.notifications.mark_seen();
            self.needs_redraw = true;
        }
    }

    /// The list's rows — see [`notification_rows`], which is where they are
    /// built — with each told how many lines its writing needs.
    fn notification_entries(&mut self) -> Vec<menu::Entry> {
        let mut entries = notification_rows(self.notifications.list());
        let height = self.focused_size().map_or(1080.0, |(_, height)| height);
        self.measure_rows(&mut entries, height);
        entries
    }

    /// Ask the renderer how many lines each row's writing takes, so the rows
    /// that carry more than one can open out to hold it — see
    /// [`ui::context_row_growth`].
    ///
    /// The counting cannot happen where the rows are built, and that is the
    /// whole reason this exists: how wide a word is is known only to the thing
    /// that shapes text, so the rows are built without a care for it and then
    /// measured here, where the renderer is in reach. Once per panel and not
    /// once per frame — a label does not change width between frames, and
    /// shaping every row of every frame to rediscover a number that never moves
    /// would be paying a rendering cost for a fact about the layout.
    ///
    /// Only the announcement panels are measured, of all the shell's menus.
    /// Every other row in the shell is named by the shell, in words chosen to
    /// fit; these carry a sentence written by whatever program sent it.
    fn measure_rows(&mut self, entries: &mut [menu::Entry], height: f32) {
        // A row that is there to be read is capped by the display rather than
        // by the three lines a row in a list gets: it is the reason its panel
        // was raised, so the only thing entitled to cut it short is the edge of
        // the screen.
        let read = ui::context_read_lines(height) as usize;
        let listed = ui::CONTEXT_MAX_LINES as usize;
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        for entry in entries {
            let (width, size) = ui::context_label_box(entry, ui::NOTIFICATION_EXTRA_WIDTH);
            let cap = if entry.reading { read } else { listed };
            // Measured bold, which is how a row's label is drawn once the
            // highlight is on it — and a row that opens out under the highlight
            // only ever does so there. Measuring the lighter weight would be
            // measuring a width the label never has at the moment it matters.
            // A row that cannot be chosen is drawn in the lighter weight and
            // always will be, so it is measured that way.
            entry.lines = gpu.lines_needed(&entry.label, size, entry.enabled, width, cap) as u8;
            // And the line under it, which is the longer of the two on an
            // announcement: the summary is a headline and the body is the
            // sentence. Never bold — the second line stays quiet under the
            // highlight, which is how the row says which of the two is the
            // heading.
            let Some(detail) = entry.detail.as_deref() else {
                continue;
            };
            let (width, size) = ui::context_detail_box(entry, ui::NOTIFICATION_EXTRA_WIDTH);
            entry.detail_lines = gpu.lines_needed(detail, size, false, width, cap) as u8;
        }
    }

    /// One announcement opened: what it said in full, the buttons the program
    /// offered with it, and a way to put it away.
    ///
    /// The body is a row that cannot be chosen rather than a title, because a
    /// title is one line and a body is a paragraph — and because what the
    /// panel is about is already on the header. A row the highlight steps over
    /// is how this shell draws something that is there to be read.
    ///
    /// Which is why these rows are measured too. A row that cannot be chosen
    /// never comes under the highlight, so it would never open out under one;
    /// it is drawn at its full height from the moment the panel arrives, and a
    /// panel whose whole purpose is the paragraph on it would otherwise show
    /// one line of it and an ellipsis.
    fn notification_actions(&mut self, id: u32) -> Vec<menu::Entry> {
        let Some(held) = self.notifications.list().iter().find(|held| held.id == id) else {
            return Vec::new();
        };
        let mut entries = Vec::with_capacity(held.actions.len() + 2);
        if !held.body.trim().is_empty() {
            entries.push(
                menu::Entry::new(menu::Command::DismissNotification(id), held.body.clone())
                    .reading(),
            );
        }
        entries.extend(
            held.actions
                .iter()
                .enumerate()
                .map(|(index, (key, label))| {
                    // The program's own word for the button, and never the shell's:
                    // whatever it wrote there is what its user was told to expect.
                    // Falling back to the key is not a nicety either — a program that
                    // sent an empty label has still offered a button, and one drawn
                    // with nothing on it is one nobody can press on purpose.
                    let label = if label.trim().is_empty() {
                        key.as_str()
                    } else {
                        label.as_str()
                    };
                    menu::Entry::new(menu::Command::InvokeNotification(id, index), label)
                        .glyph(icons::LAUNCH)
                }),
        );
        entries.push(
            menu::Entry::new(menu::Command::DismissNotification(id), "Dismiss")
                .glyph(icons::UNINSTALL)
                .group(1),
        );
        let height = self.focused_size().map_or(1080.0, |(_, height)| height);
        self.measure_rows(&mut entries, height);
        entries
    }

    /// The header that panel is raised under: what the announcement is called,
    /// and how many lines saying it takes.
    ///
    /// Measured for the same reason its rows are — see [`Self::measure_rows`]
    /// — and it is the one header in the shell that needs measuring. Every
    /// other panel is titled with a name the shell chose, and chose to fit;
    /// this one is titled with a sentence whatever program sent the
    /// announcement wrote, and a heading cut with an ellipsis on the panel
    /// raised to read the thing in full is the shell hiding the very line the
    /// user pressed the row to see.
    fn announcement_title(&mut self, id: u32) -> Option<menu::Title> {
        let text = self
            .notifications
            .list()
            .iter()
            .find(|held| held.id == id)?
            .title()
            .to_string();
        let height = self.focused_size().map_or(1080.0, |(_, height)| height);
        let (width, size) = ui::context_title_box(ui::NOTIFICATION_EXTRA_WIDTH);
        let cap = ui::context_title_lines(height) as usize;
        // Bold, because that is the weight a header is set in — and a heading
        // measured light is a heading measured at a width it never has.
        let lines = match self.gpu.as_mut() {
            Some(gpu) => gpu.lines_needed(&text, size, true, width, cap) as u8,
            None => 1,
        };
        Some(menu::Title::new(text).lines(lines))
    }

    /// Whether the panel that is up is the notification list.
    ///
    /// See also [`age_of`], which is how each of its rows says when.
    ///
    /// Asked of the rows rather than remembered as a flag, exactly as the mixer
    /// is: a second place saying which panel is up is a second place to get it
    /// wrong when the menu is dismissed by one of the several things that
    /// dismiss it.
    fn notification_panel_is_up(&self) -> bool {
        self.context_menu.is_open()
            && self.context_menu.entries().iter().any(|entry| {
                matches!(
                    entry.command,
                    menu::Command::DismissNotification(_) | menu::Command::DismissNotifications
                )
            })
    }

    /// Keep the open list showing what has actually been announced.
    fn sync_notification_panel(&mut self) {
        if !self.notification_panel_is_up() {
            return;
        }
        // Something that arrives into an open list has been read on the way in:
        // the row appears in front of somebody who is looking at the list. A
        // mark left lit for it would be the bell telling the user to go and
        // read what they had just watched arrive, and it would still be there
        // when they closed the panel.
        self.notifications.mark_seen();
        let entries = self.notification_entries();
        if self.context_menu.refresh(entries) {
            self.needs_redraw = true;
        }
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
            menu::Command::MuteShell => nudge_shell_sound(delta),
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
    /// Left and Right do nothing on a column of plain commands, on purpose: the
    /// menu is one column, and Back is the way out of it — a sideways press
    /// that also dismissed it would make a mistimed direction close the thing
    /// the user was reading. On a row that carries a track they move that
    /// track, which is the same thing they do to the bars in the sidebar
    /// behind it, and on a row with a button on its end they step onto that
    /// button and back off it.
    ///
    /// The three cannot collide: a track fills its row and a row with a track
    /// has no button, so at most one of them has anything to say.
    fn on_context_menu_action(&mut self, action: Action) {
        match action {
            Action::Up | Action::Down => {
                let delta = if action == Action::Up { -1 } else { 1 };
                if self.context_menu.move_selection(delta) {
                    self.stepped();
                }
            }
            Action::Left | Action::Right => {
                let delta = if action == Action::Left { -1 } else { 1 };
                if self.context_menu.move_aside(delta) {
                    self.stepped();
                } else if self.nudge_mixer(delta) {
                    self.sync_mixer();
                    self.stepped();
                }
            }
            Action::Launch => self.choose_context_menu(),
            _ => {}
        }
    }

    /// Carry out the menu's highlighted command.
    fn choose_context_menu(&mut self) {
        // A row that has asked for a further list has already been answered.
        // The panel is still showing the list it was pressed on for as long as
        // that press is being watched, and a second Accept in that moment would
        // press the same row again — so it is swallowed rather than carried out
        // against rows that are on their way off the panel.
        if self.context_menu.is_descending() {
            return;
        }
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
                    self.stepped();
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
            // The same thing Accept on the row does, and deliberately the same
            // code: the cursor has not moved — the menu has had every button
            // since it went up — so what "the selection" means here is the file
            // the panel is titled after.
            menu::Command::Open => self.start_selection(),
            // These two do not act, they ask. The panel stays where it is and
            // shows what was asked for; see `Menu::descend`.
            menu::Command::OpenWith => {
                let entries = self.open_with_entries();
                let title = self.selected_media().map(|file| file.title.clone());
                self.context_menu
                    .descend(title.map(menu::Title::new), entries);
            }
            menu::Command::Sort => {
                let entries = self.sort_entries();
                let title = self.selected_column_title();
                self.context_menu
                    .descend(title.map(menu::Title::new), entries);
            }
            menu::Command::OpenWithHandler(index) => self.choose_default_handler(index),
            menu::Command::SortBy(sort) => self.sort_selected_shelf(sort),
            menu::Command::Delete => self.ask_to_delete(from),
            menu::Command::ConfirmDelete => self.delete_the_file(from),
            menu::Command::ConfirmUninstall => self.begin_uninstall(),
            menu::Command::SubmitPassword => self.submit_password(),
            menu::Command::Authenticate => self.submit_authentication(),
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
            // And the same bargain again for the notification list: the row
            // going *is* the answer, so the panel stays and is brought up to
            // date under the highlight.
            menu::Command::DismissNotification(id) => {
                self.notifications.dismiss(id);
                // Only the list is worth keeping up to date. A press inside an
                // opened announcement dismissed the very thing that list was
                // about, so the panel is on its way out anyway — and refreshing
                // it would put the outer list's rows under a highlight that is
                // still standing in the inner one.
                self.sync_notification_panel();
            }
            menu::Command::ShowNotification(id) => {
                let entries = self.notification_actions(id);
                let title = self.announcement_title(id);
                self.context_menu.descend(title, entries);
            }
            menu::Command::InvokeNotification(id, action) => {
                // Looked up now rather than carried in the command, because a
                // command is `Copy` and a key is a `String` — and because the
                // announcement may have been replaced under the open panel by
                // the program that sent it, in which case the button pressed
                // is the one on the list as it stands.
                let key = self
                    .notifications
                    .list()
                    .iter()
                    .find(|held| held.id == id)
                    .and_then(|held| held.actions.get(action))
                    .map(|(key, _)| key.clone());
                match key {
                    Some(key) => {
                        self.notifications.invoke(id, &key);
                    }
                    // The announcement went while the panel was open. Nothing
                    // to press, and nothing to say about it: the row is gone
                    // from the list behind this one already.
                    None => tracing::debug!(id, action, "no such notification action"),
                }
            }
            menu::Command::DismissNotifications => {
                self.notifications.dismiss_all();
                self.sync_notification_panel();
            }
            menu::Command::MuteShell => {
                let sound = settings::sound();
                settings::set_sound(system::Level {
                    muted: !sound.muted,
                    ..sound
                });
                self.sync_mixer();
            }
            // The panel is already on its way out — choosing a row hands the
            // keys back and starts it folding — so all that is left is to stop
            // waiting on whatever it was for.
            menu::Command::Dismiss => {
                self.removal_plan = None;
                self.deleting = None;
                self.abandon_uninstall("cancelled");
                // A question closed by anything other than one of its own rows
                // is a question that was not answered, and an unanswered
                // question is a no.
                self.answer_share(None);
                self.abandon_authentication("cancelled");
            }
            menu::Command::ShareDisplay(row) => self.answer_share(Some(row)),
            menu::Command::RefuseShare => self.answer_share(None),
            menu::Command::SteamSignIn => {
                self.steam.begin();
                self.show_steam_panel();
            }
            menu::Command::SteamWithQr => {
                self.steam.with_qr();
                self.show_steam_panel();
            }
            menu::Command::SteamWithPassword => {
                self.steam.with_password();
                self.show_steam_panel();
            }
            menu::Command::SteamSubmit => {
                self.steam.submit();
                self.show_steam_panel();
            }
            menu::Command::SteamCancel => {
                self.steam.cancel();
                self.show_steam_panel();
            }
            menu::Command::SteamSignOut => self.steam.sign_out(),
            menu::Command::SteamRefresh => self.steam.refresh(),
            // Asks rather than acts, exactly as `Sort` does one column along:
            // the panel stays where it is and shows the orders. See
            // `Menu::descend`.
            menu::Command::SteamSort => {
                let entries = steam_sort_rows(self.steam.sort(), self.steam.orders());
                let title = Some(menu::Title::new(apps::steam_title()));
                self.context_menu.descend(title, entries);
            }
            menu::Command::SteamSortBy(sort) => self.sort_steam_library(sort),
            menu::Command::SteamInstall(app_id) => self.install_steam_game(app_id),
            menu::Command::SteamStopInstalling(app_id) => self.steam.stop_installing(app_id),
            menu::Command::SteamInstallWithSteam(app_id) => {
                self.steam_hand_over(app_id, lxb_steam::Doing::Install)
            }
            menu::Command::SteamUninstall(app_id) => self.offer_to_uninstall(app_id),
            menu::Command::SteamUninstallNow(app_id) => self.uninstall_steam_game(app_id),
            menu::Command::SteamDo(doing) => self.steam_do(doing),
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
                menu::Entry::new(menu::Command::Dismiss, "No"),
                menu::Entry::new(menu::Command::ConfirmUninstall, "Yes").grave(),
            ],
            // On No, and No is drawn first. A confirmation whose default answer
            // destroys something is not a confirmation — and one that puts that
            // answer where the harmless one stands in every other question is
            // barely better, because the hand goes where it went last time.
            //
            // The Steam question this matches is `Shell::offer_to_uninstall`;
            // the two are the same panel asked about different things and used
            // to disagree about both the order and the colour.
            0,
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
                            password: secret::Secret::default(),
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
                            password: secret::Secret::default(),
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
                menu::Entry::new(menu::Command::Dismiss, "Cancel"),
                menu::Entry::new(menu::Command::SubmitPassword, "Uninstall").grave(),
            ],
            // Cancel first and standing on it, exactly as the question was:
            // the button that destroys something is never the one already
            // under the user's thumb. Typing the password and pressing Return
            // submits it without going near either button — see
            // `Shell::type_into_password`.
            0,
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

    // --- proving the user may do something ----------------------------------

    /// Bring the session's polkit agent up to date with the screen: take away a
    /// question that has been withdrawn, carry on the one that is up, and put
    /// up the next one when there is room for it.
    ///
    /// Once a frame, like every other worker the shell listens to. Nothing here
    /// is a Wayland event — `polkitd` asks on a thread of its own and the PAM
    /// helper answers on another — so this is the frame they reach the screen
    /// on.
    fn sync_polkit(&mut self) {
        let (withdrawn, asked) = match self.polkit.as_ref() {
            Some(agent) => {
                // A question is only *taken* when there is somewhere to put it.
                // The shell asks one thing at a time — the panel holds every
                // button while it is up — so a second authorisation waits in
                // the agent's own queue rather than being refused. Nobody is
                // kept waiting who was not already waiting on the panel in
                // front of them.
                let room = self.authenticating.is_none()
                    && !self.dialog.is_on_screen()
                    && !self.context_menu.is_on_screen();
                (agent.withdrawn(), room.then(|| agent.asked()).flatten())
            }
            None => return,
        };

        // Whoever asked has given up, or somebody else answered it. The panel
        // comes down without being answered, because there is no longer anybody
        // to answer.
        if withdrawn
            .iter()
            .any(|cookie| self.authenticating_about(cookie))
        {
            tracing::info!("the authentication on screen was withdrawn");
            // Dropped rather than abandoned: `polkitd` has already closed the
            // question, and answering a cookie it has forgotten is one D-Bus
            // call that can only fail.
            self.authenticating = None;
            // The board came up with the field and goes away with it — on this
            // route too, which is the one that reaches neither of the two
            // answers. A keyboard left standing over the guide with nothing to
            // type into is the shell asking for something nobody is waiting
            // for.
            self.dismiss_password_board();
            self.close_dialog();
            self.needs_redraw = true;
        }

        self.advance_authentication();

        if let Some(request) = asked {
            self.ask_to_authenticate(request);
        }
    }

    /// Whether the panel on screen is about the question this cookie names.
    fn authenticating_about(&self, cookie: &str) -> bool {
        self.authenticating
            .as_ref()
            .is_some_and(|state| state.request.cookie == cookie)
    }

    /// Put a question from `polkitd` on screen and start the conversation with
    /// polkit's helper.
    ///
    /// The guide is opened under it, exactly as a share question does and for
    /// the same reason: an authorisation can be asked for while a game is
    /// filling the screen, and a panel raised on the bar alone would be drawn
    /// behind it.
    fn ask_to_authenticate(&mut self, request: polkit::Request) {
        tracing::info!(
            action = %request.action_id,
            user = %request.user,
            "asking the user to authenticate"
        );
        let session = polkit::Session::start(&request.user, &request.cookie);
        let note = waiting_note(&request);
        self.authenticating = Some(Authenticating {
            request,
            session,
            password: secret::Secret::default(),
            note,
            asked: false,
            answered: false,
        });

        self.open_guide();
        let from = self.dialog_origin();
        let lines = match self.authenticating.as_ref() {
            Some(state) => authentication_lines(&state.request.message, &state.note, 0),
            None => return,
        };
        let raised = self.dialog.ask(
            from,
            Some(icons::AUTHENTICATE.to_string()),
            lines,
            vec![
                menu::Entry::new(menu::Command::Authenticate, "Authenticate"),
                menu::Entry::new(menu::Command::Dismiss, "Cancel"),
            ],
            // On Cancel. A panel that appears without being asked for must not
            // have the answer that hands over authority sitting under the
            // thumb of somebody pressing A at something else.
            1,
        );
        if !raised {
            self.abandon_authentication("the panel could not be raised");
            return;
        }
        // Said once, as the question arrives, and only now that it is really on
        // screen. Nothing else in the shell announces itself — every other
        // sound answers a control the user pressed — and this one has to,
        // because the panel appears over whatever they were doing without
        // anybody having asked for it. See [`sound::AUTHENTICATE`].
        self.sounds.authenticate();
        // The board comes up with the field, for the reason the removal's does:
        // on a console there is nothing else to type a password with.
        self.osk.open_here();
        self.sync_surface_state();
        self.needs_redraw = true;
    }

    /// Carry the conversation with polkit's helper on to the screen.
    fn advance_authentication(&mut self) {
        let Some(state) = self.authenticating.as_mut() else {
            return;
        };
        let said = state.session.said();
        if said.is_empty() {
            return;
        }

        // PAM's own words for what went wrong, when it has any. Worth more than
        // the shell's standing line about a password not being accepted — "your
        // account is locked" is not something to answer by trying again.
        let mut complaint = None;
        let mut ended = None;
        for what in said {
            match what {
                // Whatever PAM wants typed goes in the field. An echoing prompt
                // is drawn with marks like any other, which is the one place
                // this shell does not do as it is told: the panel has a single
                // field and it is built for a secret. Showing a one-time code
                // in the clear on a screen a game was filling a moment ago is
                // not an improvement, and the prompt above it says what to
                // type.
                polkit::Said::Asked { prompt, echo } => {
                    tracing::debug!(prompt, echo, "the helper is asking");
                    state.note = prompt_note(&prompt, &state.request);
                    state.asked = true;
                }
                polkit::Said::Told(text) => state.note = text,
                polkit::Said::Complained(text) => {
                    tracing::info!(said = %text, "PAM complained");
                    complaint = Some(text);
                }
                polkit::Said::Finished(proved) => {
                    ended = Some(proved);
                    break;
                }
            }
        }

        match ended {
            None => {
                if let Some(said) = complaint {
                    state.note = said;
                }
                self.refresh_authentication_panel();
            }
            // The helper has already told `polkitd` the password was right, so
            // there is nothing left to do but let the call return and take the
            // panel away.
            Some(true) => {
                tracing::info!(action = %state.request.action_id, "authenticated");
                self.finish_authentication(polkit::Answer::Proved);
                self.close_dialog();
                self.needs_redraw = true;
            }
            // A helper that failed *before* anything was typed did not refuse a
            // password — it could not ask for one. Offering another try would
            // loop for ever on a machine whose helper is missing or broken, so
            // that one is said out loud and this one is offered again, exactly
            // as a removal offers a refused `sudo` password again.
            Some(false) if state.answered => {
                let request = state.request.clone();
                state.session = polkit::Session::start(&request.user, &request.cookie);
                state.password = secret::Secret::default();
                state.note = complaint
                    .unwrap_or_else(|| "That password was not accepted. Try again.".to_string());
                state.asked = false;
                state.answered = false;
                self.refresh_authentication_panel();
                self.needs_redraw = true;
            }
            Some(false) => {
                let said = complaint.clone();
                tracing::warn!(?said, "the helper gave up before asking for anything");
                self.finish_authentication(polkit::Answer::Declined);
                self.say_authentication_failed(said);
            }
        }
    }

    /// Hand what has been typed to the helper.
    fn submit_authentication(&mut self) {
        let handed = {
            let Some(state) = self.authenticating.as_mut() else {
                return;
            };
            // Nothing typed is not an answer to send: PAM would be told the
            // password was empty and would say it was wrong, which is not what
            // happened. The field simply waits — and so does a field the helper
            // has not asked anything of yet.
            if !state.asked || state.password.is_empty() {
                return;
            }
            // Taken out whole, so the password moves to the worker rather than
            // being copied out of a field the panel is still drawing from.
            state.session.answer(std::mem::take(&mut state.password));
            state.asked = false;
            state.answered = true;
            state.note = "Checking…".to_string();
            true
        };
        if handed {
            self.refresh_authentication_panel();
            self.needs_redraw = true;
        }
    }

    /// Apply one keystroke to the password being typed into an authentication.
    /// Returns whether it was this field's to take.
    fn type_into_authentication(&mut self, stroke: keyboard::Stroke) -> bool {
        let done = {
            let Some(state) = self.authenticating.as_mut() else {
                return false;
            };
            match stroke {
                keyboard::Stroke::Char(character) => {
                    state.password.push(character);
                    None
                }
                keyboard::Stroke::BACKSPACE => {
                    state.password.pop();
                    None
                }
                keyboard::Stroke::ENTER => Some(true),
                keyboard::Stroke::ESCAPE => Some(false),
                // Tab, the arrows, the function keys: a password field has no
                // use for any of them, and passing them on to the bar
                // underneath would move the selection this panel is drawn over.
                _ => return true,
            }
        };
        match done {
            Some(true) => self.submit_authentication(),
            Some(false) => {
                self.abandon_authentication("escaped");
                self.close_dialog();
                self.needs_redraw = true;
            }
            None => {
                self.refresh_authentication_panel();
                self.needs_redraw = true;
            }
        }
        true
    }

    /// Rewrite the panel from what the authentication knows now — the prompt,
    /// and how many characters have been typed into it.
    fn refresh_authentication_panel(&mut self) {
        let Some(state) = self.authenticating.as_ref() else {
            return;
        };
        // Only into the panel that asked. One dismissed while the helper was
        // still talking gets nothing written into it on the way out.
        if !self.dialog.is_open() {
            return;
        }
        let lines =
            authentication_lines(&state.request.message, &state.note, state.password.typed());
        self.dialog.say(lines);
    }

    /// Tell `polkitd` how it went and let go of the conversation.
    fn finish_authentication(&mut self, answer: polkit::Answer) {
        let Some(state) = self.authenticating.take() else {
            return;
        };
        if let Some(agent) = self.polkit.as_ref() {
            agent.answer(&state.request.cookie, answer);
        }
        // The board came up with the field and goes away with it.
        self.dismiss_password_board();
        // And here the helper is killed, if it is still going: dropping the
        // session is what ends it.
    }

    /// Give up on the authentication on screen, because the user did.
    fn abandon_authentication(&mut self, why: &'static str) {
        if let Some(state) = self.authenticating.as_ref() {
            tracing::info!(action = %state.request.action_id, why, "authentication declined");
        }
        self.finish_authentication(polkit::Answer::Declined);
    }

    /// Say that the machine could not be asked at all.
    ///
    /// The one failure worth a panel of its own. Everything else the user can
    /// do something about — type it again, or press Cancel — but a helper that
    /// is missing or refuses to run leaves an application saying it is not
    /// authorised with nothing anywhere saying why.
    fn say_authentication_failed(&mut self, said: Option<String>) {
        let from = self.dialog_origin();
        let mut lines = vec![
            dialog::Line::Heading("Authentication failed".to_string()),
            dialog::Line::Note("This machine could not be asked to check".to_string()),
            dialog::Line::Note("your password.".to_string()),
        ];
        if let Some(said) = said {
            lines.push(dialog::Line::Note(said));
        }
        lines.push(dialog::Line::Rule);
        self.dialog.ask(
            from,
            Some(icons::AUTHENTICATE.to_string()),
            lines,
            vec![menu::Entry::new(menu::Command::Dismiss, "OK")],
            0,
        );
        self.needs_redraw = true;
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
        // The files the walk has found are the shell's own record of what is on
        // the disk, and a package coming off the machine has not changed that.
        // So they are carried across to the columns that were just rebuilt
        // rather than asked for again — see [`apps::carried_media`].
        let carried = apps::carried_media(&mut self.xmb.categories);
        self.xmb.categories = categories;
        for mut made in carried {
            made.orders = self.media.orders(made.kind);
            let hung = apps::shelve_media(&mut self.xmb.categories, made);
            self.media.discard(hung.worn);
        }
        for panel in &mut self.panels {
            panel.cursor = Cursor::for_model(&self.xmb);
        }
        self.needs_redraw = true;
    }

    /// Put the centred panel away, and stop waiting on anything it had asked
    /// for. Returns whether it was open.
    /// Take the centred panel away, and let go of whatever it was standing in
    /// for.
    ///
    /// The share question is the one that cannot simply be dropped: something
    /// on the other side of the portal is *waiting* on it. A panel taken away
    /// by the guide button, by Back, by a shoulder button — by anything that
    /// is not one of its own rows — used to leave the question standing with
    /// nobody left to answer it, and one unanswered question is enough to make
    /// the whole session deaf: the shell refuses a second question while one
    /// is up, so every later application asking to share was refused before it
    /// could be shown to anybody. That is a portal that has stopped working
    /// with nothing on screen to say so.
    ///
    /// An unanswered question is a no, which is the rule everywhere else here
    /// too, and answering is idempotent — the row that dismisses the panel has
    /// usually answered already.
    fn close_dialog(&mut self) -> bool {
        self.app_facts = None;
        self.removal_plan = None;
        self.abandon_uninstall("dismissed");
        self.answer_share(None);
        self.abandon_authentication("dismissed");
        // A sign-in dismissed by Back is a sign-in given up on: the panel was
        // the whole of it, and one left running behind a bar nobody can see it
        // from would go on polling Steam for a code that is no longer on
        // screen.
        self.steam.cancel();
        self.steam_buttons.clear();
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

    /// Keep the atlas holding pictures of the rows that are being looked at,
    /// and nothing else.
    ///
    /// Three steps, once a frame, and the order matters: work out what is worth
    /// having, give up everything else so there is room, then take whatever the
    /// workers have finished. A picture that arrives for a row the cursor has
    /// since left is dropped on the floor here rather than uploaded — it cost
    /// nothing to make the second time, because it is on the disk now.
    fn sync_thumbnails(&mut self) {
        let (files, games) = self.rows_worth_having();
        for path in &files {
            if self
                .gpu
                .as_ref()
                .is_some_and(|gpu| gpu.thumbnail(path).is_none())
            {
                self.thumbs.want(path);
            }
        }
        // A cover the atlas already holds is not asked for again — the whole
        // journey is a read, a decode and an upload, and repeating it once a
        // frame for every row on screen is the one way this could cost
        // anything. Where no cover has been found yet there is no path to ask
        // the atlas about, and that is exactly when it has to be asked for.
        for app_id in &games {
            let held = self.art.cover(*app_id).and_then(|path| {
                self.gpu
                    .as_ref()
                    .and_then(|gpu| gpu.thumbnail(path).map(|_| ()))
            });
            if held.is_none() {
                self.art.want(*app_id, art::Piece::Cover);
            }
        }
        // And the picture behind each display, which is the one game each
        // cursor is standing on rather than a row near it: a hero is most of a
        // megabyte, and fetching nine of them for every row somebody scrolls
        // past would be the whole library over the wire by teatime.
        let scenery: HashSet<u32> = self
            .panels
            .iter()
            .flat_map(|panel| panel.scenery.wanted())
            .collect();
        for app_id in &scenery {
            if self
                .gpu
                .as_ref()
                .is_some_and(|gpu| gpu.scenery(*app_id).is_none())
            {
                self.art.want(*app_id, art::Piece::Hero);
            }
        }
        // And that game's logo, which is what a launch splash puts in the
        // middle of the picture. Asked for while the cursor is merely standing
        // on the row, well before anything has been pressed, and that is the
        // point: a logo asked for at the moment of the press would arrive
        // somewhere into an animation that has already begun, so the title
        // would appear as a name and then turn into artwork. It is a tenth of
        // what a hero costs and it is wanted in exactly the same places.
        let logos: HashSet<u32> = scenery
            .iter()
            .copied()
            // The game being started as well, in case its row is somehow no
            // longer the one the cursor is on: the splash is on screen for
            // minutes, and a logo dropped out of the atlas underneath it would
            // take the title off it.
            .chain(self.launching.as_ref().and_then(launch::Launch::game))
            .collect();
        for app_id in &logos {
            if self
                .gpu
                .as_ref()
                .is_some_and(|gpu| gpu.logo(*app_id).is_none())
            {
                self.art.want(*app_id, art::Piece::Logo);
            }
        }

        let made = self.thumbs.take();
        // Taken before the keys are worked out, not after: a cover arriving is
        // how the shell learns where that game's picture *is*, and a wanted
        // set built a moment earlier would not have the path in it — so the
        // picture would be dropped, and nothing would ever ask for it again.
        let fetched = self.art.take();
        let mut wanted = files;
        for app_id in &games {
            if let Some(path) = self.art.cover(*app_id) {
                wanted.insert(path.to_path_buf());
            }
        }

        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        gpu.retain_thumbnails(&wanted);
        gpu.retain_scenery(&scenery);
        gpu.retain_logos(&logos);
        for (path, picture) in made {
            if !wanted.contains(&path) || gpu.thumbnail(&path).is_some() {
                continue;
            }
            if gpu.put_thumbnail(&path, &picture) {
                self.needs_redraw = true;
            }
        }
        for made in fetched {
            let put = match &made {
                art::Made::Cover { path, picture }
                    if wanted.contains(path) && gpu.thumbnail(path).is_none() =>
                {
                    gpu.put_thumbnail(path, picture)
                }
                art::Made::Hero {
                    app_id,
                    scenery: picture,
                } if scenery.contains(app_id) => gpu.put_scenery(*app_id, picture),
                art::Made::Logo { app_id, picture } if logos.contains(app_id) => {
                    gpu.put_logo(*app_id, picture)
                }
                // A picture for a row or a display that has moved on. Dropped
                // here rather than uploaded; it is in the cache on the disk by
                // now, so having it back costs a read.
                _ => false,
            };
            self.needs_redraw |= put;
        }
    }

    /// What is on screen and made of pictures, or about to be.
    ///
    /// The rows around each display's cursor, in whichever column it is
    /// standing in — a handful either side, so scrolling meets pictures that
    /// are already there instead of a column that fills in behind the user.
    /// Every other file in the library, which may be twenty thousand of them,
    /// is not asked about at all. That is the whole of "only when they are
    /// needed": the work is bounded by the size of the screen rather than by
    /// the size of the collection.
    ///
    /// Two kinds of row, answered together because they are the same walk: a
    /// file of the user's own, which is a path, and a game, which is an app id
    /// whose picture may not be on this machine at all yet.
    fn rows_worth_having(&self) -> (HashSet<PathBuf>, HashSet<u32>) {
        /// How many rows either side of the cursor are worth having ready.
        const REACH: usize = 4;

        let mut files = HashSet::new();
        let mut games = HashSet::new();
        for panel in &self.panels {
            let entries = panel.cursor.current_entries(&self.xmb);
            let selected = panel.cursor.selected_item();
            let from = selected.saturating_sub(REACH);
            let to = (selected + REACH + 1).min(entries.len());
            for entry in &entries[from..to] {
                match entry {
                    Entry::Game(game) => {
                        games.insert(game.app_id);
                    }
                    _ => {
                        if let Some(file) = entry.media().filter(|file| file.kind.has_picture()) {
                            files.insert(file.path.clone());
                        }
                    }
                }
            }
        }
        (files, games)
    }

    // --- Steam -------------------------------------------------------------

    /// Say that there is no Steam client here to do this with.
    ///
    /// Out loud, in a panel, rather than a press that quietly does nothing:
    /// the row was on the bar and the user pressed it, so the reason it did
    /// not start has to arrive where the press did. A library can be signed in
    /// to and compatible native titles launched on a machine with no Steam
    /// client; this panel is specifically for a requested client-backed
    /// install, repair, removal or fallback.
    fn say_no_steam_client(&mut self, name: &str) {
        let from = self.dialog_origin();
        self.dialog.ask(
            from,
            Some(icons::STEAM.to_string()),
            vec![
                dialog::Line::Heading(name.to_string()),
                dialog::Line::Note(
                    "There is no Steam client installed on this machine to run it with."
                        .to_string(),
                ),
                dialog::Line::Rule,
            ],
            vec![menu::Entry::new(menu::Command::Dismiss, "OK")],
            0,
        );
        self.needs_redraw = true;
    }

    /// Begin fetching one game.
    ///
    /// Which build comes down is Valve's client's decision and not this
    /// shell's — the same decision Steam makes anywhere else, about this
    /// system and this account's licences.
    fn install_steam_game(&mut self, app_id: u32) {
        tracing::info!(app_id, "asking Valve's client to fetch a game");
        self.steam.install(app_id);
    }

    /// Take one game off the disk, having asked first.
    ///
    /// Only ever reached from [`Shell::offer_to_uninstall`]. Valve's client is
    /// told not to put its own confirmation up — that window over the bar is
    /// the whole of what this integration is for — so the shell's panel is the
    /// only thing standing between a press and a game being deleted, and there
    /// must be no way to this that does not go through it.
    fn uninstall_steam_game(&mut self, app_id: u32) {
        tracing::info!(app_id, "asking Valve's client to remove a game");
        self.steam.uninstall(app_id);
    }

    /// Ask before taking a game off the disk.
    ///
    /// Steam asks this itself when its own uninstaller is used, and that is
    /// exactly the window this shell will not have. So the question moves here
    /// rather than being lost: it is somebody's game and somebody's evening's
    /// download, and the press that removes it is the last chance anybody gets
    /// to have meant something else.
    fn offer_to_uninstall(&mut self, app_id: u32) {
        let Some(game) = self.steam.game(app_id) else {
            return;
        };
        let (name, note) = (game.name.clone(), game.note());
        let from = self.dialog_origin();
        self.dialog.ask(
            from,
            Some(icons::STEAM.to_string()),
            // Short lines, because the panel clips rather than wraps. The
            // row's own note is the second of them because it is where the
            // size lives, which is most of what this decision is about.
            vec![
                dialog::Line::Heading(name),
                dialog::Line::Note(note),
                dialog::Line::Note("It stays in the library.".to_string()),
                dialog::Line::Rule,
            ],
            vec![
                menu::Entry::new(menu::Command::Dismiss, "Keep It"),
                menu::Entry::new(menu::Command::SteamUninstallNow(app_id), "Uninstall")
                    .glyph(icons::UNINSTALL)
                    .grave(),
            ],
            // Standing on the row that changes nothing, because this is the
            // press that cannot be taken back: the one that lands by accident
            // has to be the harmless one.
            0,
        );
        self.needs_redraw = true;
    }

    /// Offer to fetch a game the account owns and this machine has not got.
    ///
    /// Asked rather than simply started, because it is somebody's line and
    /// somebody's disk: a press meaning "I want this" and a press meaning "and
    /// spend forty gigabytes on it now" are the same press, and only one of
    /// them can be taken back.
    fn offer_to_install(&mut self, game: &apps::Game) {
        // On its way off the disk instead. There is nothing to decide — it is
        // seconds of work and it was asked for — so this says what is
        // happening rather than offering to start it coming back down, which
        // is the one press that would be answered by fetching forty gigabytes
        // of what is being deleted.
        if self.steam.is_removing(game.app_id) {
            let from = self.dialog_origin();
            self.dialog.ask(
                from,
                Some(icons::STEAM.to_string()),
                vec![
                    dialog::Line::Heading(game.name.clone()),
                    dialog::Line::Note("This game is being removed.".to_string()),
                    dialog::Line::Rule,
                ],
                vec![menu::Entry::new(menu::Command::Dismiss, "OK")],
                0,
            );
            self.needs_redraw = true;
            return;
        }

        // Already coming down: the press asks whether to stop, which is the
        // only thing left to decide about it.
        if self.steam.is_fetching(game.app_id) {
            let note = self
                .steam
                .fetching(game.app_id)
                .map(|so_far| so_far.said())
                .unwrap_or_else(|| "Installing…".to_string());
            let from = self.dialog_origin();
            self.dialog.ask(
                from,
                Some(icons::STEAM.to_string()),
                // Short lines, because the panel clips rather than wraps: a
                // sentence that runs off the end is a sentence that says
                // something else.
                vec![
                    dialog::Line::Heading(game.name.clone()),
                    dialog::Line::Note(note),
                    // Said plainly, because it is the whole of what the press
                    // costs: there is no resuming a download here, so what has
                    // arrived is of no use to a later one and goes with it.
                    dialog::Line::Note("Stopping removes what arrived.".to_string()),
                    dialog::Line::Note("Installing again starts over.".to_string()),
                    dialog::Line::Rule,
                ],
                vec![
                    menu::Entry::new(menu::Command::Dismiss, "Keep Installing"),
                    menu::Entry::new(
                        menu::Command::SteamStopInstalling(game.app_id),
                        "Stop Installing",
                    ),
                ],
                0,
            );
            self.needs_redraw = true;
            return;
        }

        if !self.steam.signed_in() {
            return self.say_steam_needs_an_account(&game.name);
        }
        let from = self.dialog_origin();
        self.dialog.ask(
            from,
            Some(icons::STEAM.to_string()),
            vec![
                dialog::Line::Heading(game.name.clone()),
                dialog::Line::Note("This game is not on this machine.".to_string()),
                dialog::Line::Note("It can be fetched from Steam now.".to_string()),
                dialog::Line::Rule,
            ],
            vec![
                menu::Entry::new(menu::Command::SteamInstall(game.app_id), "Install"),
                menu::Entry::new(menu::Command::Dismiss, "Not Now"),
            ],
            0,
        );
        self.needs_redraw = true;
    }

    /// Keep Valve's client off the screen, or give it back.
    ///
    /// The client is a program this shell drives rather than presents: it is
    /// signed in, asked for games and read for a library, none of which anybody
    /// should have to watch. It does not agree — it puts up a "starting game"
    /// dialog over the shell's own loading screen, and raises its storefront
    /// behind the game that is starting — and there is no flag that stops it.
    /// So the compositor is asked not to show it. See
    /// `lxb_shell_v1.keep_out_of_sight`.
    ///
    /// Sent for every name in [`lxb_steam::client::WINDOW_NAMES`], and only in
    /// a session that drives the client at all: hiding a Steam somebody started
    /// for themselves would be this shell taking away a window it never gave.
    fn keep_steam_out_of_sight(&self, hidden: bool) {
        if !self.steam.driving() {
            return;
        }
        let Some(control) = self.shell_control.as_ref() else {
            return;
        };
        if control.version() < OUT_OF_SIGHT_SHELL_VERSION {
            // An older compositor cannot be asked, and the session is honest
            // about it rather than half-hiding the client: it goes on putting
            // its windows wherever it likes, exactly as it always did.
            tracing::info!("this compositor cannot run an application out of sight");
            return;
        }
        tracing::debug!(hidden, "asking the compositor about Valve's client windows");
        for name in lxb_steam::client::WINDOW_NAMES {
            control.keep_out_of_sight(name.to_string(), u32::from(hidden));
        }
    }

    /// Take a press on a Steam game and hand it to Valve's client.
    ///
    /// Returns whether the press was taken here. Every installed Steam game
    /// comes this way now: the client is the only thing that gets a modern
    /// game right — the runtime it wants, the Proton prefix it already has,
    /// the anti-cheat it ships, the overlay it expects — and a shell that
    /// started the executable itself got all of that wrong for anything more
    /// complicated than a single native binary.
    ///
    /// What the user sees is a loading screen and then their game. The client
    /// may have to be started and signed in first, which takes the better part
    /// of a minute from cold, and that happens underneath the splash: the
    /// press is answered on the screen immediately, exactly as any other
    /// launch is, and [`Self::steam_client_said`] carries on when the client
    /// is ready.
    ///
    /// The two ways this cannot go anywhere are answered here rather than
    /// silently: no Valve client on the machine, and nobody signed in. Both
    /// would otherwise be a press that appeared to do nothing.
    fn ask_steam_before_starting(&mut self) -> bool {
        // One at a time. A second press while the first is still waiting on
        // the client must not start two of anything.
        if self.awaiting_steam.is_some() {
            return true;
        }
        let Some(game) = self.selected_game().cloned() else {
            return false;
        };
        if !game.installed {
            return false;
        }

        if !game.steam_client {
            self.say_no_steam_client(&game.name);
            return true;
        }
        if !self.steam.signed_in() {
            self.say_steam_needs_an_account(&game.name);
            return true;
        }

        let Some(panel) = self.panels.get(self.focused_panel) else {
            return false;
        };
        let from = ui::launch_origin(panel.width as f32, panel.height as f32);
        let known: Vec<u32> = panel.windows.iter().map(|window| window.id).collect();
        let foreground = panel.foreground.clone().unwrap_or_default();

        // The press is answered on the screen straight away, exactly as any
        // other launch is. There is no process to watch and there will not be
        // one — the game is the client's child, not ours — so the splash waits
        // on the window and nothing else.
        let splash = launch::Launch::new(
            game.name.clone(),
            Some(icons::STEAM.to_string()),
            self.focused_panel,
            from,
            None,
            Instant::now(),
            launch::Before {
                windows: &known,
                foreground: &foreground,
            },
        )
        .through_steam(game.app_id);
        self.launching = Some(splash);
        // The screen is changing hands, which is what this sound is about, and
        // it has changed hands whether or not the client comes up.
        self.sounds.launch();
        self.awaiting_steam = Some(AwaitingSteam {
            app_id: game.app_id,
            name: game.name.clone(),
            panel: self.focused_panel,
            from,
            asked: Instant::now(),
        });
        tracing::info!(app_id = game.app_id, name = %game.name, "waking Valve's client to start this game");
        // Hidden again. It normally already is; what this covers is the user
        // having asked to see the client earlier in the session, which gave
        // sight back and would otherwise leave it showing over this game.
        self.keep_steam_out_of_sight(true);
        self.steam.wake_client();
        self.needs_redraw = true;
        true
    }

    /// Valve's client has said something about itself, and a press may be
    /// waiting on it.
    fn steam_client_said(&mut self, report: lxb_steam::ClientReport) {
        match report {
            // Nothing to do but keep the splash up, which is already up — and
            // is already saying so: the splash goes up on [`launch::Doing::Steam`]
            // and moves to the game in `start_the_waiting_game` below.
            //
            // It used to say nothing here, on the reasoning that "starting
            // Steam" is the shell's own plumbing and not something the user
            // asked about. That is right for a wait somebody does not notice
            // and wrong for this one: a cold client is most of a minute before
            // the game is so much as asked for, and a picture with a spinner on
            // it for that long does not read as working, it reads as stuck.
            lxb_steam::ClientReport::Waking => {}
            lxb_steam::ClientReport::Ready => self.start_the_waiting_game(),
            lxb_steam::ClientReport::Unavailable(why) => {
                let Some(waiting) = self.awaiting_steam.take() else {
                    return;
                };
                // The splash was the shell's promise that something was
                // starting, and now nothing is. It goes before the
                // explanation, so the explanation is not drawn behind it.
                self.launching = None;
                self.say_steam_could_not_start_it(&waiting, &why);
            }
        }
    }

    /// The client is up; ask it for the game the press was about.
    fn start_the_waiting_game(&mut self) {
        let Some(waiting) = self.awaiting_steam.take() else {
            return;
        };
        tracing::info!(
            app_id = waiting.app_id,
            name = %waiting.name,
            waited = ?waiting.asked.elapsed(),
            "Valve's client is ready; asking it to start the game"
        );
        if let Err(why) = self.steam.play(waiting.app_id) {
            self.launching = None;
            return self.say_steam_could_not_start_it(&waiting, &why);
        }
        // From here the splash waits for a window, and only for a window. The
        // client may update the game, build its prefix or run an installer
        // first; none of that is visible from here and none of it is failure.
        if let Some(splash) = self.launching.as_mut() {
            splash.now_starting_through_steam(Instant::now());
        }
        // A game has the screen now, so the guide steps out of its way — the
        // same thing an ordinary launch does.
        if self.xmb.running_app().is_some() {
            self.guide.close();
        }
        self.needs_redraw = true;
    }

    /// Valve's client could not be brought up, so the game does not start.
    fn say_steam_could_not_start_it(&mut self, waiting: &AwaitingSteam, why: &str) {
        self.osk.dismiss_at_once();
        tracing::warn!(app_id = waiting.app_id, %why, "the game was not started");
        let lines = vec![
            dialog::Line::Heading(waiting.name.clone()),
            dialog::Line::Note("Steam could not be started, so this game cannot run.".to_string()),
            dialog::Line::Note(why.to_string()),
            dialog::Line::Rule,
        ];
        let from = if self.focused_panel == waiting.panel {
            self.dialog_origin()
        } else {
            waiting.from
        };
        self.dialog.ask(
            from,
            Some(icons::STEAM.to_string()),
            lines,
            vec![menu::Entry::new(menu::Command::Dismiss, "OK")],
            0,
        );
        self.needs_redraw = true;
    }

    /// A game was asked for and no window ever came.
    ///
    /// Said rather than passed over, because the splash has just spent four
    /// minutes promising the user that something was happening. What went
    /// wrong is not knowable from here — the game is the client's child and
    /// the client says nothing about it — so this says only what is true.
    fn say_steam_never_started_it(&mut self, from: [f32; 4], name: String) {
        tracing::warn!(%name, "Steam never opened a window for this game");
        self.dialog.ask(
            from,
            Some(icons::STEAM.to_string()),
            vec![
                dialog::Line::Heading(name),
                dialog::Line::Note("Steam did not open this game.".to_string()),
                dialog::Line::Note(
                    "It may still be updating it, or it may have stopped.".to_string(),
                ),
                dialog::Line::Rule,
            ],
            vec![menu::Entry::new(menu::Command::Dismiss, "OK")],
            0,
        );
        self.needs_redraw = true;
    }

    /// Nobody is signed in, so Valve's client cannot be signed in either.
    ///
    /// The client is signed in with *this* session's credential, so a shell
    /// that is signed out has nothing to give it — and a client that had to
    /// ask for itself would put its own login window on screen, which is the
    /// one thing this must never do.
    fn say_steam_needs_an_account(&mut self, name: &str) {
        let from = self.dialog_origin();
        self.dialog.ask(
            from,
            Some(icons::STEAM.to_string()),
            vec![
                dialog::Line::Heading(name.to_string()),
                dialog::Line::Note(
                    "Steam starts this game, and it needs the account this shell is signed in to. Sign in to Steam first."
                        .to_string(),
                ),
                dialog::Line::Rule,
            ],
            vec![menu::Entry::new(menu::Command::Dismiss, "OK")],
            0,
        );
        self.needs_redraw = true;
    }

    /// Take in whatever the Steam worker has said, and put it on the bar.
    ///
    /// Three things can have changed and they are answered separately, because
    /// they disturb different parts of the screen: who is signed in rebuilds
    /// one row of the Games column, the library rebuilds a column of its own,
    /// and the sign-in panel is a modal that may have to be raised, redrawn or
    /// taken away.
    fn sync_steam(&mut self) {
        let changed = self.steam.sync();
        if changed.account {
            let account = self.steam.account().map(str::to_string);
            tracing::info!(?account, "the Steam account changed");
            let shifted = apps::offer_steam(&mut self.xmb.categories, account);
            self.absorb(shifted);
        }
        if changed.library {
            let rows = self.steam.rows();
            tracing::info!(games = rows.len(), "the Steam library changed");
            self.shelve_games(rows);
        }
        if changed.panel {
            self.show_steam_panel();
        }
        // After the column and the panel, because a failure puts a modal up
        // and the bar it is drawn over should already be the new one.
        if let Some(report) = changed.client {
            self.steam_client_said(report);
        }
        for answer in changed.installed {
            self.steam_ended(answer);
        }
        if changed.account || changed.library {
            self.needs_redraw = true;
        }
    }

    /// Hang the library on the bar, leaving every display on the game it was
    /// on and every cover with the amount of colour its game has earned.
    ///
    /// The rows arrive re-sorted, which is the whole reason both of those need
    /// doing: a game that has just finished downloading has moved from the
    /// bottom half of the library to the top, and both what is under each
    /// display's highlight and what is in colour are answers that moved with
    /// it. Where each cursor is standing has to be read *before* the rows are
    /// replaced, because afterwards there is nothing left to compare against.
    fn shelve_games(&mut self, rows: Vec<apps::Entry>) {
        self.drained.told(
            rows.iter()
                .filter_map(apps::Entry::game)
                .map(|game| (game.app_id, game.installed)),
        );
        let standing: Vec<Option<u32>> = match self.steam_column() {
            Some(at) => self
                .panels
                .iter()
                .map(|panel| panel.cursor.game_in_column(&self.xmb, at))
                .collect(),
            None => Vec::new(),
        };

        let shifted = apps::shelve_steam(&mut self.xmb.categories, rows);
        self.absorb(shifted);

        // Found again rather than kept: putting the column up or taking it
        // down moves every column after it, and a cursor is about to be told
        // which one to look in.
        let Some(at) = self.steam_column() else {
            return;
        };
        let xmb = &self.xmb;
        for (panel, was) in self.panels.iter_mut().zip(standing) {
            if let Some(app_id) = was {
                panel.cursor.keep_on_game(xmb, at, app_id);
            }
        }
    }

    /// List the Steam column in a different order.
    ///
    /// Written down as well as applied, for the reason a shelf's order is —
    /// nobody chooses "last played first" meaning "until I next start the
    /// shell" — and under one key, because a person has one library.
    ///
    /// The column is rebuilt from the library rather than re-sorted where it
    /// hangs: [`steam::Steam::rows`] is the one place that turns games into
    /// rows, and a second ordering of the finished column would be a second
    /// opinion about what the order is. Every download in flight keeps its
    /// percentage across the move, because that is held per game rather than
    /// per row.
    fn sort_steam_library(&mut self, sort: lxb_steam::library::Sort) {
        if !self.steam.set_sort(sort) {
            return;
        }
        tracing::info!(order = sort.key(), "listing the Steam library differently");
        settings::remember_steam_sort(sort);

        // Where the cursor goes is the one place this differs from a shelf. A
        // shelf is re-sorted on the worker and the cursor is put at the top
        // twice — once now and once when the rows land — because half a million
        // files take a while to come back. The library is already here, so the
        // rows below are the new ones and the cursor can simply be placed in
        // them.
        self.shelve_games(self.steam.rows());

        // At the top of it, on the display that asked. Somebody who has just
        // said "largest first" is asking to be shown the largest, and a cursor
        // held on the game it happened to be standing on would answer with that
        // game's new position instead — which in a library of hundreds is
        // somewhere in the middle of a list they never see the head of. Every
        // other display keeps its game, because the order changed underneath it
        // rather than at its request: `shelve_games` has already put them back
        // on it.
        //
        // Only when the cursor is actually in the library. The menu can be
        // raised over a game on a display standing in the Steam column, which
        // is the only place it can be raised from at all — but a display whose
        // cursor is elsewhere is not one that asked for anything.
        let Some(at) = self.steam_column() else {
            return;
        };
        let xmb = &self.xmb;
        if let Some(panel) = self
            .panels
            .get_mut(self.focused_panel)
            .filter(|panel| panel.cursor.selected_category == at)
        {
            panel.cursor.rest_on_first_row(xmb);
        }
        self.needs_redraw = true;
    }

    /// Where the Steam library hangs on the bar, when there is one.
    fn steam_column(&self) -> Option<usize> {
        self.xmb
            .categories
            .iter()
            .position(|column| column.id == apps::steam_column())
    }

    /// A game has finished moving on or off the disk, one way or the other.
    ///
    /// Only the ways that did not work are announced. One that worked has
    /// already said so on the bar — the row it was counting up on now reads as
    /// an installed game, or as one that is not there any more — and a modal
    /// over the top of that would be the shell interrupting somebody to tell
    /// them what they are looking at.
    fn steam_ended(&mut self, answer: steam::Ended) {
        let (app_id, lines, rows) = match answer {
            steam::Ended::Done { .. } | steam::Ended::Removed { .. } => {
                tracing::info!(?answer, "a game finished moving");
                return;
            }
            // Nothing went wrong: the game wants something answered that only
            // Steam's own window can ask, so that is what is offered. The
            // wording is Steam's question rather than a failure, and the rows
            // say what happens next rather than inviting the same press again.
            steam::Ended::Failed {
                app_id,
                why: lxb_steam::Stopped::Asks(what),
            } => {
                tracing::info!(app_id, %what, "this game cannot be fetched silently");
                (
                    app_id,
                    vec![
                        dialog::Line::Note(format!("This game has {what} first.")),
                        dialog::Line::Note("Steam has to ask that itself.".to_string()),
                    ],
                    vec![
                        menu::Entry::new(
                            menu::Command::SteamInstallWithSteam(app_id),
                            "Install with Steam",
                        )
                        .glyph(icons::LAUNCH),
                        menu::Entry::new(menu::Command::Dismiss, "Not Now"),
                    ],
                )
            }
            steam::Ended::Failed {
                app_id,
                why: lxb_steam::Stopped::Failed(why),
            } => {
                tracing::warn!(app_id, %why, "a download did not finish");
                (
                    app_id,
                    vec![
                        dialog::Line::Note(why),
                        dialog::Line::Note("Nothing was left on the disk.".to_string()),
                    ],
                    vec![
                        menu::Entry::new(menu::Command::SteamInstall(app_id), "Try Again"),
                        menu::Entry::new(menu::Command::Dismiss, "OK"),
                    ],
                )
            }
            steam::Ended::RemoveFailed { app_id, why } => {
                tracing::warn!(app_id, %why, "a game was not removed");
                (
                    app_id,
                    vec![
                        dialog::Line::Note(why),
                        dialog::Line::Note("It is still on the disk.".to_string()),
                    ],
                    vec![
                        menu::Entry::new(menu::Command::SteamUninstallNow(app_id), "Try Again"),
                        menu::Entry::new(menu::Command::Dismiss, "OK"),
                    ],
                )
            }
        };

        let name = self
            .steam
            .game(app_id)
            .map(|game| game.name.clone())
            .unwrap_or_else(|| format!("App {app_id}"));
        let from = self.dialog_origin();
        let mut said = vec![dialog::Line::Heading(name)];
        said.extend(lines);
        said.push(dialog::Line::Rule);
        self.dialog
            .ask(from, Some(icons::STEAM.to_string()), said, rows, 0);
        self.needs_redraw = true;
    }

    /// A column has appeared on the bar or gone from it. Keep every display's
    /// cursor on the column it was looking at.
    ///
    /// The same care [`Shell::sync_media`] takes when the walk earns Multimedia
    /// its column back, and needed more often here: signing in adds a column
    /// and signing out takes one away, and either can happen while somebody is
    /// standing three columns further along.
    fn absorb(&mut self, shifted: apps::Shifted) {
        for panel in &mut self.panels {
            if let Some(at) = shifted.added {
                panel.cursor.category_added(at);
            }
            if let Some(at) = shifted.removed {
                panel.cursor.category_removed(at, &self.xmb);
            }
        }
    }

    /// Put the sign-in panel on screen, bring it up to date, or take it away.
    ///
    /// Rebuilt from the stage rather than edited, and *how* it is put up
    /// depends on whether the question has changed: a panel whose buttons are
    /// the same one is the same panel with different words in it — somebody
    /// typing, or Steam rotating the code on screen — and re-raising it would
    /// make it grow out of its anchor again on every keystroke.
    fn show_steam_panel(&mut self) {
        let Some(panel) = self.steam.panel() else {
            if self.dialog.is_open() {
                self.close_dialog();
            }
            self.steam_buttons.clear();
            return;
        };

        let buttons: Vec<menu::Command> = panel.buttons.iter().map(|entry| entry.command).collect();
        if self.dialog.is_open() && buttons == self.steam_buttons {
            self.dialog.say(panel.lines);
        } else {
            let from = self.dialog_origin();
            self.dialog.ask(
                from,
                Some(icons::STEAM.to_string()),
                panel.lines,
                panel.buttons,
                panel.start,
            );
            self.steam_buttons = buttons;
        }

        // The board comes up with a field and goes away with it, because on a
        // console there is nothing else to type with — the same reason the
        // uninstall panel raises one. It types *here* rather than through the
        // virtual keyboard: see `keyboard::Osk::open_here`.
        if panel.typing {
            self.osk.open_here();
        } else if self.osk.types_here() {
            self.osk.close();
        }
        self.sync_surface_state();
        self.needs_redraw = true;
    }

    /// Apply one keystroke to whichever field of the sign-in panel is up.
    /// Returns whether there was one.
    fn type_into_steam(&mut self, stroke: keyboard::Stroke) -> bool {
        match self.steam.type_into(stroke) {
            steam::Typed::Elsewhere => false,
            steam::Typed::Into => {
                self.show_steam_panel();
                true
            }
            steam::Typed::Done { submitted: true } => {
                self.steam.submit();
                self.show_steam_panel();
                true
            }
            steam::Typed::Done { submitted: false } => {
                self.steam.cancel();
                self.show_steam_panel();
                true
            }
        }
    }

    /// What pressing the Steam row at the head of the Games column does.
    ///
    /// Signed out, it asks how they would like to sign in. Signed in, it takes
    /// them to the column that signing in built — which is the next column
    /// along, so the press is the same journey Right would have made, and the
    /// row is the sign that there is somewhere to go.
    fn press_steam_row(&mut self) {
        if self.steam.signed_in() {
            if self.step_to_steam_column() {
                self.sounds.step();
            } else {
                // Signed in with no column to go to: the library has not
                // arrived, which on a console switched on before the router is
                // the ordinary case. The press asks for it again rather than
                // doing nothing visible.
                self.steam.refresh();
            }
            return;
        }
        self.steam.begin();
        self.show_steam_panel();
    }

    /// Take the focused display to the Steam column. `false` when there is not
    /// one, which is every session where the library has not arrived yet.
    fn step_to_steam_column(&mut self) -> bool {
        let Some(at) = self.steam_column() else {
            return false;
        };
        let xmb = &self.xmb;
        let Some(panel) = self.panels.get_mut(self.focused_panel) else {
            return false;
        };
        panel.cursor.select_category(at, xmb);
        self.needs_redraw = true;
        true
    }

    /// Start whatever the Steam menu asked for on the selected title.
    ///
    /// Every one of these is an explicitly labelled Valve-client fallback and
    /// hands a `steam:` URL through the same launch the bar uses for anything
    /// else. A normal Play press never comes through here.
    fn steam_do(&mut self, doing: lxb_steam::Doing) {
        // `Open` is about the client rather than about a title, so it does not
        // need one selected. Everything else does.
        let app_id = match doing {
            lxb_steam::Doing::Open => 0,
            _ => match self.selected_game() {
                Some(game) => game.app_id,
                None => return,
            },
        };
        self.steam_hand_over(app_id, doing);
    }

    /// Hand one `steam:` URL to the client, and give the screen back so that
    /// what it raises can be seen.
    ///
    /// Every one of these ends in a window of the client's own — its
    /// storefront, its file check, its install wizard — so sight is given back
    /// before it is asked for. Otherwise the press would suppress the very
    /// thing it was for, and read as doing nothing at all. It is taken away
    /// again at the next game press, which is the next time the client is
    /// something being driven rather than something being looked at.
    fn steam_hand_over(&mut self, app_id: u32, doing: lxb_steam::Doing) {
        let name = match doing {
            lxb_steam::Doing::Open => "Steam".to_string(),
            _ => self
                .steam
                .game(app_id)
                .map(|game| game.name.clone())
                .unwrap_or_else(|| "Steam".to_string()),
        };
        self.keep_steam_out_of_sight(false);
        // Handed to the client rather than started as a program: the client is
        // already running, and a second one would exit the moment it had
        // passed the request to the first.
        if let Err(why) = self.steam.tell(app_id, doing) {
            tracing::warn!(%name, %why, "Steam would not take that");
            self.say_no_steam_client(&name);
        }
    }

    /// The Steam title under the cursor, if the cursor is on one.
    fn selected_game(&self) -> Option<&apps::Game> {
        self.panels
            .get(self.focused_panel)?
            .cursor
            .current_entry(&self.xmb)?
            .game()
    }

    /// Whether the cursor is on the Steam row at the head of the Games column.
    fn selected_service(&self) -> Option<&apps::Service> {
        self.panels
            .get(self.focused_panel)?
            .cursor
            .current_entry(&self.xmb)?
            .service()
    }

    /// The menu for one Steam title: what can be done to it, in the order a
    /// person would want them.
    ///
    /// The rows differ between the two halves of the column because the two
    /// halves are different situations, not different states of one: a game
    /// that is here can be played, checked or removed, and a game that is not
    /// can only be fetched. Offering the other three greyed out would be four
    /// rows of which three are refusals.
    fn game_entry_menu(&self) -> Option<([f32; 4], Option<String>, Vec<menu::Entry>)> {
        let panel = self.panels.get(self.focused_panel)?;
        let game = panel.cursor.current_entry(&self.xmb)?.game()?;
        let anchor = ui::launch_origin(panel.width as f32, panel.height as f32);

        let rows = steam_game_menu_rows(game);
        Some((anchor, Some(game.name.clone()), rows))
    }

    /// The menu for the Steam row itself: what can be done to the *account*,
    /// as against to anything in its library.
    ///
    /// Two bands, and the rule between them is what is being asked of whom.
    /// Above it are the things Steam is asked to do — fetch the library again,
    /// forget this machine. Below it are the ones it is not: the order the
    /// column is listed in, which is the shell's own answer and is offered here
    /// because this row is the handle the whole library hangs off; the client,
    /// which is another program; and the way out of the menu.
    fn service_entry_menu(&self) -> Option<([f32; 4], Option<String>, Vec<menu::Entry>)> {
        let panel = self.panels.get(self.focused_panel)?;
        let service = panel.cursor.current_entry(&self.xmb)?.service()?;
        let anchor = ui::launch_origin(panel.width as f32, panel.height as f32);

        let mut rows = Vec::new();
        match service.account.as_deref() {
            Some(_) => {
                rows.push(
                    menu::Entry::new(menu::Command::SteamRefresh, "Refresh the library")
                        .glyph(icons::REFRESH),
                );
                rows.push(
                    menu::Entry::new(menu::Command::SteamSignOut, "Sign out")
                        .glyph(icons::SIGN_OUT)
                        .grave(),
                );
            }
            None => rows.push(
                menu::Entry::new(menu::Command::SteamSignIn, "Sign in to Steam")
                    .glyph(icons::LAUNCH),
            ),
        }
        // Only where there is a library on the bar to be ordered. Nobody signed
        // in has no column, and an order for a column that is not there is a row
        // whose answer the user could not be shown.
        if self.steam_column().is_some() {
            rows.push(
                menu::Entry::new(menu::Command::SteamSort, "Sort")
                    .glyph(icons::SORT)
                    .group(1),
            );
        }
        // The client itself is an optional fallback, so do not offer a row
        // which can only end in a "not installed" refusal.
        if self.steam.has_client() {
            rows.push(
                menu::Entry::new(
                    menu::Command::SteamDo(lxb_steam::Doing::Open),
                    lxb_steam::Doing::Open.label(),
                )
                .group(1),
            );
        }
        rows.push(menu::Entry::new(menu::Command::Dismiss, "Cancel").group(1));
        Some((
            anchor,
            Some(
                service
                    .account
                    .clone()
                    .unwrap_or_else(|| "Steam".to_string()),
            ),
            rows,
        ))
    }

    /// Hang whatever the worker has finished on the rows that hold it.
    ///
    /// The rows are replaced wholesale rather than added to, because what
    /// arrives is a whole shelf in its own order and lands anywhere in the
    /// list. So every display's cursor is asked which *file* it is standing on
    /// before the swap and put back on that same file after it: a row number
    /// means nothing across a list that has grown in the middle, and a cursor
    /// left on one would have the user reading one song and pressing another.
    ///
    /// All of the work is already done by the time this runs. What is left is
    /// swapping a vector in, putting the cursors right, and handing the rows
    /// that were there back to the worker to let go of — see
    /// [`media::Library::discard`].
    fn sync_media(&mut self) {
        let ready = self.media.take();
        if ready.is_empty() {
            return;
        }

        for made in ready {
            let kind = made.kind;
            // Which file each display is standing on, if it is standing on one.
            // By the row it holds rather than by its path: the worker builds
            // every list out of the same shared files, so this is a pointer
            // against a pointer where a path is a string against a string —
            // and it is asked once per row of a shelf that may hold half a
            // million of them.
            let xmb = &self.xmb;
            let standing: Vec<Option<media::Shelved>> = self
                .panels
                .iter()
                .map(|panel| {
                    panel
                        .cursor
                        .current_entry(xmb)
                        .and_then(apps::Entry::shelved)
                        .cloned()
                })
                .collect();

            // Disjoint fields: the catalogue is written while this display's
            // own state is read.
            let hung = apps::shelve_media(&mut self.xmb.categories, made);

            // The rows that have just arrived carry the query the worker
            // narrowed them by, which is the one it was told about — and by
            // now the user may have typed two more letters. Putting the field
            // back to what has actually been typed is what stops it flickering
            // backwards a word at a time while somebody types quickly.
            if let Some(searching) = self.searching.as_ref().filter(|open| open.kind == kind) {
                let text = searching.text.clone();
                apps::set_search_text(&mut self.xmb.categories, kind, &text);
            }
            if let Some(at) = hung.column {
                tracing::info!(
                    at,
                    column = self.xmb.categories[at].title,
                    "the walk found files on a machine with nothing installed to \
                     open them; that column is back"
                );
            }

            let to_top = self
                .sorting
                .take_if(|(sorted, _)| *sorted == kind)
                .map(|(_, panel)| panel);
            let xmb = &self.xmb;
            for (at_panel, (panel, was)) in self.panels.iter_mut().zip(standing).enumerate() {
                if let Some(at) = hung.column {
                    panel.cursor.category_added(at);
                }
                if to_top == Some(at_panel) {
                    panel.cursor.rest_on_first_row(xmb);
                    continue;
                }
                if let Some(file) = was {
                    panel.cursor.keep_on_media(xmb, &file);
                }
            }
            self.media.discard(hung.worn);
        }
        self.needs_redraw = true;
    }

    /// Ask the walk to look at the disk again when somebody opens a shelf.
    ///
    /// Stepping into Music, Video or Images is a person asking what they have,
    /// and the honest answer is one that includes the film they recorded a
    /// minute ago. The walk comes round on its own every few minutes, which is
    /// the right interval for a collection nobody is looking at and far too
    /// long for the one they have just opened.
    ///
    /// So the column is the question. Not the row above it — a cursor walking
    /// down Multimedia passes Music and Video on its way to the players, and
    /// asking on the way past would make idle scrolling walk the home
    /// directory. Stepping *in* is deliberate, and it is also the only moment
    /// where anything new could be seen.
    ///
    /// Cheap in the two ways that matter. It is a look at the first row of the
    /// open column, which the bar is drawing anyway, and it fires on the frame
    /// the column changes rather than on every frame the user is in it; and
    /// what it asks for is bounded on the other side, where one walk answers
    /// however many times it was asked — see [`media::Library::look_again`].
    fn notice_open_shelves(&mut self) {
        let xmb = &self.xmb;
        let mut opened = false;
        for panel in &mut self.panels {
            let shelf = apps::shelf_shown(panel.cursor.current_entries(xmb));
            // Only on the way in, and only into a different shelf: leaving one
            // is not a question, and standing in one is not a new one.
            opened |= shelf.is_some() && shelf != panel.shelf_open;
            panel.shelf_open = shelf;
        }
        if opened {
            self.media.look_again();
        }
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
                self.stepped();
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
                self.type_into_shell(stroke);
            }
            // The board goes; the field it was raised over does not. Somebody
            // who has just typed the first letter of a search on a real
            // keyboard is going to type the second one on it too, and that
            // letter arrives through `on_key` — which sends it here for as
            // long as the cursor is standing in the field. A search that ended
            // with the board would turn the rest of the word into button
            // presses.
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

    /// Ask for the cursor whatever is under it deserves, if the compositor will
    /// be told.
    ///
    /// Two shapes and one rule: a pointing hand over anything the shell will
    /// answer for, and the ordinary arrow everywhere else. That is the only
    /// thing the cursor can say about a screen it is being used on by someone
    /// who was given a controller — where a press does something — and over a
    /// text field the application has usually asked for a beam, which left
    /// hanging over a picture of a keyboard says the letters can be selected
    /// rather than pressed.
    ///
    /// Only sent when it changes. A shape device per motion event would be an
    /// object created and destroyed for every pixel the mouse travels.
    fn set_cursor_shape(&mut self, qh: &QueueHandle<Self>, wanted: shape::Shape) {
        let (Some(manager), Some(pointer)) = (self.cursor_shape.as_ref(), self.pointer.as_ref())
        else {
            return;
        };
        let Some(serial) = self.pointer_enter else {
            return;
        };
        if self.pointer_shape == Some(wanted) {
            return;
        }
        let device = manager.get_shape_device(pointer, qh);
        device.set_shape(serial, wanted);
        device.destroy();
        self.pointer_shape = Some(wanted);
    }

    // -- the pointer and the finger -----------------------------------------
    //
    // A console is driven from a controller and the shell is drawn for one, but
    // the machine under it is a PC with a mouse in the drawer and, often
    // enough, a touchscreen. Nothing about the design has to change to be
    // pointed at: what a click does is what moving the selection there and
    // pressing `A` does, reached through the same actions, so there is one
    // answer to what every control means rather than two that can drift apart.
    //
    // One rule shapes all of it. Things that *stand still* light up under the
    // pointer and answer the first click; things that *move when they are
    // selected* take one click to select and another to act. The sidebar's
    // rows, the menus, the dialog's buttons and the keyboard's keys are the
    // first kind, and hovering one is selecting it — the board has worked that
    // way since it was drawn. The bar's rows and the overview's cards are the
    // second: selecting one slides it to the middle of the screen, so a hover
    // that selected would pull the thing being pointed at out from under the
    // cursor and leave its neighbour there to be selected in turn. The cursor
    // would walk the bar across the display with nobody touching the mouse.

    /// What display `index` has under `(x, y)`, in that display's own
    /// coordinates.
    ///
    /// Read back to front, in the order [`Shell::draw`] lays the screen down:
    /// whatever is nearest the user is what a press at that point is about. The
    /// panels that are modal — the board, the centred dialog, the power
    /// question — swallow everything outside themselves rather than letting it
    /// through, which is exactly what they do to the directions on a pad.
    fn spot_at(&self, index: usize, x: f32, y: f32) -> Spot {
        let Some(panel) = self.panels.get(index) else {
            return Spot::Nothing;
        };
        let (width, height) = (panel.width.max(16) as f32, panel.height.max(9) as f32);
        // Only the display being driven has any of this on it; the others are
        // showing their own bar and nothing else. See `draw`.
        let focused = index == self.focused_panel;

        // The board is in front of everything else the shell draws, and takes
        // whatever lands on its keys. What the rest of the display is depends
        // on what the board is typing into: the shell's own field leaves the
        // panel holding that field still standing and still answerable, and
        // everything else belongs to the application underneath — which is why
        // nothing outside the keys reaches us there at all.
        if focused && self.keyboard_visible() && self.osk.is_open() {
            if let Some((row, column)) = ui::keyboard_key_at(x, y, width, height) {
                return Spot::Key(row, column);
            }
            if !self.osk.types_here() {
                return Spot::Nothing;
            }
        }

        if focused && self.dialog.is_open() {
            for button in 0..self.dialog.buttons.entries().len() {
                if let Some(rect) = ui::dialog_button_rect(width, height, &self.dialog, button) {
                    if within(rect, x, y) {
                        return Spot::DialogButton(button);
                    }
                }
            }
            return Spot::Nothing;
        }

        if focused && self.context_menu.is_open() {
            return self.menu_spot_at(x, y, width, height);
        }

        if focused && self.guide.is_menu() {
            if self.guide.power_open() {
                let rows = self.guide.power_items().len();
                return (0..rows)
                    .find(|row| within(ui::power_dialog_row_rect(width, height, rows, *row), x, y))
                    .map_or(Spot::Nothing, Spot::PowerRow);
            }
            return self.guide_spot_at(index, x, y, width, height);
        }

        // Measured against the bar where it settles, so a click that lands
        // while the screen is still arriving is carried back through the move
        // the drawing was carried forward through first.
        let (x, y) = ui::arrival_point(x, y, width, height, panel.arrival_linear);
        match ui::bar_hit(&self.xmb, &panel.cursor, x, y, width, height) {
            Some(spot) => Spot::Bar(spot),
            None => Spot::Nothing,
        }
    }

    /// The context menu's part of [`Self::spot_at`].
    fn menu_spot_at(&self, x: f32, y: f32, width: f32, height: f32) -> Spot {
        let slots = self.gpu.as_ref().map(|gpu| Slots {
            gpu,
            art: &self.art,
            drained: &self.drained,
        });
        for row in 0..self.context_menu.entries().len() {
            let Some(rect) = ui::context_menu_row_rect(width, height, &self.context_menu, row)
            else {
                continue;
            };
            if !within(rect, x, y) {
                continue;
            }
            // A row of the mixer is a name with a groove under it, so where it
            // was pressed is part of the answer — the groove sets the level and
            // the speaker at its head silences it, which is the same pair of
            // gestures the sidebar's own bars take.
            let level = self
                .context_menu
                .entries()
                .get(row)
                .and_then(|entry| Some((entry, entry.level?)))
                .zip(slots.as_ref())
                .and_then(|((entry, level), slots)| {
                    ui::mixer_level_at(rect, entry, level, height, slots, x)
                });
            // The button on the end of the row is inside the row's own
            // rectangle, so it is asked about first: a click there is the
            // row's other command, not the row.
            let aside = ui::context_menu_aside_rect(width, height, &self.context_menu, row)
                .is_some_and(|rect| within(rect, x, y));
            return Spot::MenuRow { row, level, aside };
        }
        // Outside the panel altogether. A menu is a note attached to something
        // on the screen behind it, and pressing that screen is how every menu
        // ever drawn is dismissed.
        if within(
            ui::context_menu_rect(width, height, &self.context_menu),
            x,
            y,
        ) {
            Spot::Nothing
        } else {
            Spot::OutsideMenu
        }
    }

    /// The guide's part of [`Self::spot_at`]: the sidebar, then the deck.
    fn guide_spot_at(&self, index: usize, x: f32, y: f32, width: f32, height: f32) -> Spot {
        // The column is measured from where the sidebar settles, so a click
        // arriving while it is still sliding in has to be measured from there
        // too — see [`ui::sidebar_slide_x`].
        let slide = ui::sidebar_slide_x(self.guide.age(), width);
        let closable = self.closable();
        let items = self.guide.items(closable);
        for (row, item) in items.iter().enumerate() {
            let mut rect = ui::menu_item_rect(&items, row, width, height);
            rect[0] += slide;
            if !within(rect, x, y) {
                continue;
            }
            let level = item
                .bar()
                .and_then(|_| ui::bar_level_at(rect, height, x))
                .filter(|_| self.guide.is_enabled(*item));
            return Spot::Entry { item: *item, level };
        }

        // The sidebar's own glass. Nothing under the point, and nothing behind
        // it either: the cards begin where the panel ends.
        let mut sidebar = ui::sidebar_panel_rect(width, height);
        sidebar[0] += slide;
        if within(sidebar, x, y) {
            return Spot::Nothing;
        }

        // The deck, at the rectangles it is actually drawn at rather than the
        // slots it is heading for: a card halfway through a glide is where the
        // user can see it, and that is what they are pointing at.
        let Some(panel) = self.panels.get(index) else {
            return Spot::Nothing;
        };
        let keys = panel
            .windows
            .iter()
            .map(|window| window.id as u64)
            .chain(std::iter::once(u64::MAX));
        for (card, key) in keys.enumerate() {
            if let Some(glide) = self.guide_card_rects.get(&key) {
                if within(glide.at, x, y) {
                    return Spot::Card(card);
                }
            }
        }
        Spot::Nothing
    }

    /// Put the selection on `spot`, and say whether it now *is* the selection.
    ///
    /// `false` where there was nothing to select — a gap between two rows, a
    /// control the highlight is not allowed to stop on — and for the two kinds
    /// a click reaches in two steps, which are not selected by being pointed at
    /// at all. A press acts on nothing when this says no, which is what keeps a
    /// click on a disabled tile from pressing the row that was selected before.
    fn point_at(&mut self, spot: Spot) -> bool {
        if !self.startup.ready {
            return false;
        }
        let closable = self.closable();
        let (moved, landed) = match spot {
            Spot::Key(row, column) => {
                self.hover_key(Some((row, column)));
                (false, true)
            }
            Spot::DialogButton(button) => (
                self.dialog.buttons.select(button),
                self.dialog.buttons.selected() == button,
            ),
            Spot::MenuRow {
                row, aside: true, ..
            } => (
                self.context_menu.select_aside(row),
                self.context_menu.selected() == row && self.context_menu.on_aside(),
            ),
            Spot::MenuRow { row, .. } => (
                self.context_menu.select(row),
                self.context_menu.selected() == row && !self.context_menu.on_aside(),
            ),
            Spot::PowerRow(row) => (
                self.guide.select_power(row),
                self.guide.power_index() == row,
            ),
            Spot::Entry { item, .. } => (
                self.guide.select(item, closable),
                self.guide.selected_item(closable) == Some(item),
            ),
            Spot::Card(_) | Spot::Bar(_) | Spot::OutsideMenu | Spot::Nothing => (false, false),
        };
        if moved {
            self.needs_redraw = true;
        }
        landed
    }

    /// Put the selection on `spot` outright, whatever kind of thing it is —
    /// the bar's rows and the deck's cards included, which a left click reaches
    /// in two steps.
    ///
    /// What the right button does before it raises a menu. A menu is *about*
    /// something, and the something has to be the thing that was pointed at:
    /// one raised over the row that happened to be selected already would be a
    /// list of things to do to an application the user is not pointing at.
    fn aim_at(&mut self, spot: Spot) -> bool {
        if !self.startup.ready {
            return false;
        }
        let moved = match spot {
            Spot::Card(card) => {
                let count = self
                    .panels
                    .get(self.focused_panel)
                    .map(|panel| panel.windows.len() + 1)
                    .unwrap_or(1);
                card < count && {
                    self.guide.select_window(card, count);
                    true
                }
            }
            Spot::Bar(ui::BarSpot::Item(row)) => match self.panels.get_mut(self.focused_panel) {
                Some(panel) => {
                    panel.cursor.point_at_row(row, &self.xmb);
                    true
                }
                None => false,
            },
            Spot::Bar(ui::BarSpot::Category(category)) => {
                match self.panels.get_mut(self.focused_panel) {
                    Some(panel) => {
                        panel.cursor.point_at_category(category, &self.xmb);
                        true
                    }
                    None => false,
                }
            }
            _ => return self.point_at(spot),
        };
        if moved {
            self.needs_redraw = true;
            self.sync_setting_preview();
        }
        moved
    }

    /// The pointer resting at `(x, y)` on display `index`: select what is under
    /// it where selecting is what pointing at a thing means, and say so with
    /// the cursor either way.
    ///
    /// Only on the display being driven. A pointer swept across the other
    /// screen changes nothing there, because moving the selection on a display
    /// that has not got control would be a highlight nobody's buttons can act
    /// on — and moving *control* with it is what this shell used to do and no
    /// longer does. See [`Shell::press_on`]. The shape is still answered on
    /// every display, since it is what says the click would be worth making.
    fn hover(&mut self, qh: &QueueHandle<Self>, index: usize, x: f32, y: f32) {
        let spot = self.spot_at(index, x, y);
        if !self.startup.ready {
            self.set_cursor_shape(qh, shape::Shape::Default);
            return;
        }
        if index == self.focused_panel {
            self.point_at(spot);
        }
        self.set_cursor_shape(
            qh,
            if spot.is_actionable() {
                shape::Shape::Pointer
            } else {
                shape::Shape::Default
            },
        );
    }

    /// A press made on display `index`, at `(x, y)` on it.
    ///
    /// The first press on a display that has not got control is spent taking
    /// it: control moves there, the overlay travels with it, and nothing is
    /// pressed. A shell that acted on that press as well would be acting on a
    /// screen it was not drawing the selection on a frame earlier — and on the
    /// guide, which was still over the other display when the button went
    /// down. The next press lands on what the user can now see is selected.
    fn press_on(&mut self, index: usize, x: f32, y: f32) -> Option<Spot> {
        if !self.startup.ready {
            return None;
        }
        if self.focus_panel(index) {
            return None;
        }
        Some(self.spot_at(index, x, y))
    }

    /// Press whatever is at `spot`: a click of the left button, or a tap.
    ///
    /// Everything that can be selected goes through [`Action::Launch`] rather
    /// than being carried out here, so a press of the mouse and a press of `A`
    /// are the same press. What is left are the things a controller reaches
    /// another way — a value set by *where* it was clicked, a screen dismissed
    /// by pressing past it, and the two kinds that take a click to select.
    fn press_at(&mut self, spot: Spot) {
        if !self.startup.ready {
            return;
        }
        let landed = self.point_at(spot);
        match spot {
            Spot::Key(..) if landed => self.press_key(),
            Spot::DialogButton(_) | Spot::PowerRow(_) if landed => self.on_action(Action::Launch),
            // A groove carries its answer in where it was pressed — the one
            // control in the shell that a press says something *with* rather
            // than merely to. Off the groove it is the button at its head, and
            // that is the ordinary press.
            Spot::MenuRow {
                level: Some(level), ..
            } if landed => {
                if self.set_mixer_level(level) {
                    self.sync_mixer();
                    self.needs_redraw = true;
                }
            }
            Spot::Entry {
                item,
                level: Some(level),
            } if landed => {
                if let Some(bar) = item.bar() {
                    if self.quick.set(knob(bar), level) {
                        self.needs_redraw = true;
                    }
                }
            }
            Spot::MenuRow { .. } | Spot::Entry { .. } if landed => self.on_action(Action::Launch),
            // The deck and the bar move when they are selected, so the first
            // press is the selection travelling to the pointer and the second
            // is the press of what has arrived there.
            Spot::Card(card) => self.press_card(card),
            Spot::Bar(spot) => self.press_bar(spot),
            Spot::OutsideMenu if self.context_menu.close() => self.needs_redraw = true,
            _ => {}
        }
    }

    /// Set the level of the mixer row the highlight is on, wherever the click
    /// along its groove put it.
    fn set_mixer_level(&mut self, level: f32) -> bool {
        match self
            .context_menu
            .selected_entry()
            .map(|entry| entry.command)
        {
            Some(menu::Command::MuteApplication(key)) => self.quick.set_stream(key, level),
            Some(menu::Command::MuteShell) => set_shell_sound(level),
            _ => false,
        }
    }

    /// A press on window card `card` of the deck.
    fn press_card(&mut self, card: usize) {
        let count = self
            .panels
            .get(self.focused_panel)
            .map(|panel| panel.windows.len() + 1)
            .unwrap_or(1);
        if card >= count {
            return;
        }
        if self.guide.select_window(card, count) {
            // The deck's own move, in the overlay's voice — the same press on
            // the bar is a step along it, and this is the same gesture over a
            // screen that answers in a click of its own.
            self.guide_stepped();
            return;
        }
        self.on_action(Action::Launch);
    }

    /// A press somewhere on the start screen.
    fn press_bar(&mut self, spot: ui::BarSpot) {
        let before = self.column_depth();
        let Some(panel) = self.panels.get_mut(self.focused_panel) else {
            return;
        };
        let moved = match spot {
            ui::BarSpot::Item(row) => {
                if !panel.cursor.point_at_row(row, &self.xmb) {
                    // Already the selection, so this is the press of it.
                    self.on_action(Action::Launch);
                    return;
                }
                true
            }
            // A category is a place rather than a thing to open: the column
            // under it is already showing whatever it holds, so arriving is the
            // whole of what a press on one does.
            ui::BarSpot::Category(category) => panel.cursor.point_at_category(category, &self.xmb),
            // A row of the trail is a column the path was opened *through*, and
            // pressing it is walking back out to it — however many steps that
            // takes, because that is how many columns the user can see between
            // where they are and where they pointed.
            ui::BarSpot::Trail(steps) => {
                (0..steps).fold(false, |left, _| panel.cursor.leave() || left)
            }
        };
        if moved {
            self.finish_cursor_move(before);
            self.sync_setting_preview();
        }
    }

    /// Move the wheel, in the units `wl_pointer.axis` reports.
    ///
    /// The wheel is the directions, which is what makes it work everywhere at
    /// once: on the bar it walks the column, in a menu it walks the rows, and
    /// across the cards it walks the deck — because that is what Up and Down
    /// already mean in each of those places.
    fn scroll_by(&mut self, horizontal: &AxisScroll, vertical: &AxisScroll) {
        // The upright axis first: a wheel that can also be tilted must not move
        // the selection diagonally, which would be two moves for the one
        // gesture the user made.
        let (down, carried) = scroll_steps(self.scrolled.1 + scroll_notches(vertical));
        self.scrolled.1 = carried;
        for _ in 0..down.abs() {
            self.on_action(if down > 0 { Action::Down } else { Action::Up });
        }

        let (right, carried) = scroll_steps(self.scrolled.0 + scroll_notches(horizontal));
        self.scrolled.0 = carried;
        for _ in 0..right.abs() {
            self.on_action(if right > 0 {
                Action::Right
            } else {
                Action::Left
            });
        }
    }

    /// Send whatever the selected key types.
    fn press_key(&mut self) {
        // Before the press is worked out, and without asking what the key was:
        // a key going down is the thing being answered, and a board where
        // Shift and the key that puts it away were the two silent ones would
        // read as a board with two dead keys on it. This is the one place
        // every press of it arrives — the stick and the D-pad through
        // `on_keyboard_action`, a finger and a mouse through `press_at`.
        self.sounds.key();
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
                self.type_into_shell(stroke);
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
        let mut drawing = false;
        // The windows this launch turned out to be, if it was a game's. Taken
        // here and applied below rather than written straight down, because
        // this is the one place in the shell that knows it — see
        // [`Self::steam_windows`].
        let mut game_windows: Vec<u32> = Vec::new();
        if let Some(splash) = self.launching.as_mut() {
            if let Some(arrival) = splash.advance(now, &known, &foreground, alive) {
                if arrival == launch::Arrival::Window && splash.game().is_some() {
                    game_windows.extend(splash.newcomers(&known));
                }
                // At `info`, because this is the one line that says why a
                // loading screen went away, and a launch that hands over to
                // the wrong window is invisible without it — the default
                // filter is `info` and a shell session has no terminal to
                // raise it from.
                tracing::info!(
                    app = %splash.name,
                    ?arrival,
                    %foreground,
                    windows = known.len(),
                    waited = now.duration_since(splash.started()).as_secs_f32(),
                    "launch splash handing the display over"
                );
            }
            finished = splash.finished(now);
            drawing = splash.drawing(now);
        }
        if !game_windows.is_empty() {
            tracing::debug!(
                windows = ?game_windows,
                "these windows are a game Valve's client started"
            );
            self.steam_windows.extend(game_windows);
        }
        if finished {
            let never_appeared = self
                .launching
                .take()
                .filter(launch::Launch::steam_never_appeared)
                .map(|splash| (splash.from, splash.name));
            if let Some((from, name)) = never_appeared {
                self.say_steam_never_started_it(from, name);
            }
        }
        // Only while there is something of it on the screen. Once it has faded
        // it goes on watching for a few seconds with nothing to draw, and a
        // display that kept redrawing through that would be taking frames from
        // the game it has just handed them to. A window appearing or going
        // away is announced by the compositor and asks for its own redraw, so
        // nothing is missed by being quiet here.
        if drawing || finished {
            self.needs_redraw = true;
        }
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

    /// An application on `index` has ended without the shell being the one to
    /// end it: hand the display to the start screen, and let it arrive.
    ///
    /// Two things happen here, and they are one answer to the same question —
    /// what should a display show when the thing that was filling it walks out?
    ///
    /// The start screen, first of all, and *whatever else is running*. A
    /// display uncovering itself down to the next application is the machine
    /// answering a question nobody asked: the user quit a game, and what they
    /// get is somebody else's window — the browser left open an hour ago, the
    /// installer that never got closed — with no more explanation than that it
    /// happened to be underneath. The shell is what the machine goes back to,
    /// so it takes the screen the same way the guide's own Dashboard row takes
    /// it, and everything still running stays running, one press of the guide
    /// button away.
    ///
    /// And it arrives rather than appearing, on the arrival the shell comes up
    /// on. From the bar's side the two are the same event — a display that was
    /// not showing the start screen is now showing nothing else — and an
    /// animation the user has already been taught to read as *here is the start
    /// screen* is the one worth reusing. It is also the honest way to fill the
    /// moment: the application's last frame and the bar's first are otherwise
    /// consecutive, which reads as a stutter rather than as a handover.
    ///
    /// Out of black, and that part is a join rather than an effect. The
    /// application's last frame goes back to the compositor on the compositor's
    /// own schedule, and what stands behind it — the next window down, the
    /// wallpaper coming back — arrives on its. A tenth of a second of black
    /// covers the seam between them; see [`ui::BLACK_HANDOVER`].
    ///
    /// What this is *not* is the guide's Close. That press is the user putting
    /// an application away deliberately, from a menu they opened, and they are
    /// still standing in that menu when it goes — with the deck closing up
    /// around the card that left, which is the animation that belongs to it.
    /// Nothing here fires for it; see [`Self::ended_by_the_shell`].
    fn an_application_ended_by_itself(&mut self, index: usize) {
        // Not over the menu. The user is standing in it, the card that left has
        // already gone from the deck, and dismissing the menu will put the bar
        // wherever the display then needs it — see `Guide::dismiss`.
        if self.guide.is_menu() {
            return;
        }
        // Nor over a launch or a window still flying home. Both own the display
        // for as long as they last and both end by handing it over themselves;
        // a start screen arriving through either would be two answers at once.
        if self.launching_on(index) || self.restoring_on(index) {
            return;
        }
        let covered = self
            .panels
            .get(index)
            .is_some_and(|panel| bar_is_covered(panel.width, panel.height, &panel.windows));
        if covered {
            // Something else is still in front. Only the display being driven
            // can be raised over it — an overlay belongs to the screen holding
            // control, and putting one on a display nobody is looking at would
            // be the shell taking a screen it was not given. The others keep
            // what the compositor leaves them.
            if index != self.focused_panel {
                return;
            }
            self.guide.show_bar_over_app();
            // The loop syncs after every round of events anyway — an
            // application exiting is one of the things that call says it is
            // for. This is the same call, made where the decision is: a
            // function that moves the surface and leaves the commit to a
            // caller three files away is one edit from not moving it at all.
            // It is idempotent, and an unchanged state is not resent.
            self.sync_surface_state();
        }
        tracing::info!(
            display = self
                .panels
                .get(index)
                .map(|panel| panel.name.as_str())
                .unwrap_or("?"),
            over_an_application = covered,
            "an application ended by itself; the start screen is coming back"
        );
        if let Some(panel) = self.panels.get_mut(index) {
            panel.arrive_out_of_black();
        }
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
        // And so is the corner of the screen. This is the one case where the
        // shell has something to draw over an application that nobody asked
        // for, so it is the one this optimisation would silently swallow: a
        // display that had stopped drawing because a game covered its bar
        // would go on not drawing while a notification came and went.
        if index == self.focused_panel && !self.notifications.toasts().is_empty() {
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

    /// Drive the guide overlay: its entry column, its bars, its window cards
    /// and the power dialog in front of them.
    ///
    /// Nothing in here sounds, alone among the places a direction moves
    /// something — see [`Self::stepped`], which every one of those others goes
    /// through. The overlay is a screen of its own rather than another column
    /// of the bar, and it is to be given a voice of its own rather than lent
    /// the bar's; until it has one it is quiet, which is a truer answer than
    /// the wrong sound would be. The panels raised *out* of it are not covered
    /// by this: a context menu is the same component wherever it was raised
    /// from, and it cannot take the overlay's sound and the bar's at once.
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
                        self.guide_stepped();
                    }
                }
                Action::Launch => {
                    if let Some(item) = self.guide.power_item() {
                        // Answered before it is carried out, unlike everywhere
                        // else: the answers here end the session or the
                        // machine, and a sound queued behind one of them would
                        // be a sound the user never hears.
                        self.answer_choice(ChosenFeedback::Kept, Screen::Guide);
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
                    self.guide_stepped();
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
                    self.guide_stepped();
                }
            }
            // Up/Down in the entry column scroll it; everything directional in
            // the cards pane, and the crossings between the two, live in the
            // guide's own focus model.
            Action::Up | Action::Down if self.guide.pane() == guide::Pane::Menu => {
                let delta = if action == Action::Up { -1 } else { 1 };
                if self.guide.move_selection(delta, closable) {
                    self.guide_stepped();
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
                    self.guide_stepped();
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
                        // A card is the window it is a picture of, so choosing
                        // one is that application taking the screen back. In
                        // the overlay's own voice, like every other press made
                        // on it: nothing is being started here, and the sound
                        // that says something is belongs to Start.
                        Some(card) => {
                            self.activate_window(card.id);
                            self.answer_choice(ChosenFeedback::Kept, Screen::Guide);
                        }
                        // Past the windows: the start-screen card. Home is
                        // the bar — over the running application when there
                        // is one, plainly otherwise. Either way the shell
                        // keeps the display.
                        None => {
                            self.guide.dismiss(app_running);
                            self.answer_choice(ChosenFeedback::Kept, Screen::Guide);
                        }
                    }
                }
                guide::Pane::Menu => {
                    if let Some(item) = self.guide.selected_item(closable) {
                        // Read before the press, because the press is what
                        // makes it untrue — see [`chosen_feedback`].
                        let chosen = chosen_feedback(item, self.pointer_app().is_some());
                        self.activate(item);
                        self.answer_choice(chosen, Screen::Guide);
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
            // The other switch, and the one that is about the session rather
            // than about whatever is in front of it — so unlike the pointer
            // tile it always has something to act on, and it is written down
            // where the shell's own settings live rather than against an
            // application's name.
            //
            // The centre is told rather than asked: it is what decides whether
            // an announcement reaches the corner and whether it makes a noise,
            // and those are one answer given in one place.
            Item::DoNotDisturb => {
                self.guide.press(item);
                let quiet = settings::set_do_not_disturb(!settings::do_not_disturb());
                self.notifications.set_quiet(quiet);
            }
            // The same again: a tile that raises a panel rather than turning
            // something over, and the press plays for the same reason — the
            // tile going down is the first frame of the panel arriving.
            Item::Notifications => {
                self.guide.press(item);
                self.open_notifications();
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
                // Before the machine goes, because what comes back is a login
                // screen and this is the last moment anything can tell it how
                // loud the machine was and which speakers it was coming out of.
                // Suspend is deliberately not one of these: it comes back to
                // this same session, with the same volume, and nothing in
                // between has looked at it.
                settings::tell_the_login_screen_before_leaving();
                run_detached("systemctl poweroff", ["systemctl", "poweroff"]);
            }
            guide::PowerItem::Exit => {
                // The login screen is a second away, and the sound server that
                // knows where this session was playing goes down with the
                // session. See `settings::tell_the_login_screen_before_leaving`.
                settings::tell_the_login_screen_before_leaving();
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
        // Written down before it is asked for, so that the hole this leaves in
        // the window list is read as the shell's own doing rather than as an
        // application walking out — see [`Self::an_application_ended_by_itself`].
        self.ended_by_the_shell.insert(id);
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
        // A window leaving this display's list because it was sent to the next
        // one is not an application ending, and the display it left has not
        // been handed anything: it has been left with whatever was already
        // behind. Same note as the kill above.
        self.ended_by_the_shell.insert(id);
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

    /// Put a desktop portal's question on screen: may this application see a
    /// display, and which one?
    ///
    /// The guide is opened under it. A question about the whole screen has to
    /// be visible over whatever is filling that screen, and the guide is
    /// already the shell's answer to being in front of an application — layer,
    /// keyboard and all. A panel raised on the bar alone would be drawn behind
    /// the game the user is being asked about.
    fn ask_to_share(&mut self, id: u32, app_id: &str) {
        let Some(control) = self.shell_control.clone() else {
            return;
        };
        if control.version() < SHARE_SHELL_VERSION {
            return;
        }
        // One question at a time. The panel is modal and holds every button
        // while it is up, so a second application asking now would be a
        // question the user cannot see — and one that would inherit the press
        // meant for the first. It is refused, and the application may ask
        // again.
        if self.sharing.is_some() {
            tracing::info!(id, "refusing a second share question while one is up");
            control.answer_share(id, None);
            let _ = self.conn.flush();
            return;
        }

        let application = match app_id.trim() {
            "" => "An application".to_string(),
            named => named.to_string(),
        };
        let mut rows = Vec::new();
        let mut displays = Vec::new();
        for panel in &self.panels {
            rows.push(menu::Entry::new(
                menu::Command::ShareDisplay(displays.len()),
                &panel.name,
            ));
            displays.push(panel.output.clone());
        }
        if displays.is_empty() {
            control.answer_share(id, None);
            let _ = self.conn.flush();
            return;
        }
        // In a band of its own, below the displays: it is the other kind of
        // answer, not one more screen.
        rows.push(menu::Entry::new(menu::Command::RefuseShare, "Don't allow").group(1));

        let lines = vec![
            dialog::Line::Heading(format!("{application} wants to share your screen")),
            // Short on purpose: the panel gives a note one line, and cuts what
            // runs past it. A warning the user cannot read to the end of is
            // not a warning.
            dialog::Line::Note("It will see everything on that screen.".to_string()),
            dialog::Line::Rule,
        ];

        // Over whatever is in front, which is the whole point of asking.
        self.open_guide();
        let from = self.dialog_origin();
        // The refusal is what the panel opens on: the safe answer is the one a
        // press of A on a question nobody read lands on.
        let refuse = rows.len() - 1;
        if !self.dialog.ask(from, None, lines, rows, refuse) {
            control.answer_share(id, None);
            let _ = self.conn.flush();
            return;
        }
        self.sharing = Some(ShareQuestion { id, displays });
        self.needs_redraw = true;
    }

    /// Raise the portal question that arrived before the complete shell did.
    /// One is all the protocol can sensibly queue here: further questions are
    /// refused at their event boundary just as they are while a modal share
    /// question is already visible.
    fn present_deferred_share(&mut self) {
        if !self.startup.ready || self.sharing.is_some() {
            return;
        }
        if let Some((id, app_id)) = self.pending_share.take() {
            self.ask_to_share(id, &app_id);
        }
    }

    /// Answer the question on screen, with a display or with nothing.
    fn answer_share(&mut self, row: Option<usize>) {
        let Some(question) = self.sharing.take() else {
            return;
        };
        let Some(control) = self.shell_control.clone() else {
            return;
        };
        let chosen = row.and_then(|row| question.displays.get(row));
        tracing::info!(
            id = question.id,
            allowed = chosen.is_some(),
            "answering a share request"
        );
        control.answer_share(question.id, chosen);
        if let Err(err) = self.conn.flush() {
            tracing::warn!(?err, "could not send the answer");
        }
    }

    /// Photograph the display the controller is driving, because the
    /// screenshot chord was pressed on it.
    ///
    /// The keyboard's route into this comes the other way round: the
    /// compositor holds that binding, works out which display the keyboard is
    /// on, and forwards it as a `screenshot` event naming the display. A pad
    /// is not the compositor's to read at all — it is opened here, straight
    /// from `/dev/input` — so the display is this shell's own answer, and the
    /// one it already uses for every other thing a controller does: the
    /// display with control, which is the one the shoulder buttons move
    /// between and the one the guide would open over.
    fn screenshot_driven_display(&mut self) {
        let Some(output) = self
            .panels
            .get(self.focused_panel)
            .map(|panel| panel.output.clone())
        else {
            tracing::debug!("no display to photograph");
            return;
        };
        self.screenshot_output(&output);
    }

    /// Photograph a whole display, because the screenshot key was pressed on
    /// it.
    ///
    /// The same division of labour as the menu's row, and the same two halves:
    /// the compositor has the pixels and this has the folder. What is
    /// different is that nothing on screen asked for it, so nothing on screen
    /// answers for it either — the display flashes, which is the compositor's
    /// to draw, and the picture turns up in Images like any other photograph
    /// in the user's pictures.
    ///
    /// A panel would be wrong here rather than merely unnecessary. The key is
    /// pressed over whatever is in front, which is usually a game holding the
    /// whole screen and the keyboard with it: a modal answer would either be
    /// drawn behind it, where nobody would see it, or in front of it, where it
    /// would take the keys off what the user was doing.
    fn screenshot_output(&mut self, output: &wl_output::WlOutput) {
        let name = self
            .panels
            .iter()
            .find(|panel| panel.output == *output)
            .map(|panel| panel.name.clone())
            .unwrap_or_default();

        let Some(control) = self.shell_control.clone() else {
            return;
        };
        if control.version() < SCREENSHOT_SHELL_VERSION {
            // Unreachable in practice — the event only arrives from a
            // compositor new enough to send it — but the version is what says
            // the request exists, and a request sent to an object that does
            // not have it is a protocol error that kills the session.
            tracing::info!("this compositor cannot photograph a display");
            return;
        }
        // The clock decides the name, so a screenshot taken on a machine whose
        // clock cannot be read has no name to be filed under.
        let Some(taken_at) = local_time() else {
            tracing::warn!("cannot read the clock, so cannot name a screenshot");
            return;
        };
        let Some(path) = screenshot::destination(&taken_at) else {
            return;
        };
        let path = screenshot::unclaimed(path, &taken_at);

        tracing::info!(display = %name, ?path, "photographing a display");
        control.capture_output(output, path.to_string_lossy().into_owned());
        if let Err(err) = self.conn.flush() {
            tracing::warn!(?err, "could not send the screenshot request");
        }
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

    /// Tell every display whether its night light should be burning at this
    /// moment, and how warm.
    ///
    /// The one setting in this shell that changes without anybody touching it.
    /// A schedule is a clock and a time zone, and neither is anything the
    /// compositor should own — so what is sent is the *answer*, worked out here
    /// against the machine's own local time, and the compositor is left with a
    /// switch and a temperature. Nine in the evening arriving is then an
    /// ordinary change to the value being diffed, carried out by the same path
    /// that carries out a press.
    ///
    /// Which is why this is safe to call every loop iteration and has to be:
    /// the loop wakes thirty times a second whether or not anything is being
    /// drawn — it has a controller to poll — and this is the only thing
    /// watching for the hour to turn. Reading the clock is cheap and cached;
    /// see [`settings::local_time`].
    fn sync_night_light(&mut self) {
        let Some(control) = self.shell_control.clone() else {
            return;
        };
        if control.version() < NIGHT_LIGHT_SHELL_VERSION {
            return;
        }
        let mut sent = false;
        for panel in &mut self.panels {
            let wanted = settings::night_light_now(&panel.name);
            if panel.applied_night_light == Some(wanted) {
                continue;
            }
            let (burning, kelvin) = wanted;
            control.set_output_night_light(&panel.output, burning as u32, kelvin as u32);
            panel.applied_night_light = Some(wanted);
            sent = true;
            tracing::debug!(
                display = %panel.name,
                burning,
                kelvin,
                "asked for this night light"
            );
        }
        if sent {
            if let Err(err) = self.conn.flush() {
                tracing::warn!(?err, "could not send the night light");
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

    /// Ask each display to be turned the way it was left, if it has been
    /// turned and is not already there.
    ///
    /// Diffed per display like [`Shell::sync_mode`], and for the same reason:
    /// a display plugged in halfway through the session comes back standing on
    /// its side by the same path that stood it there in the first place.
    ///
    /// Only displays somebody has turned are told anything. One nobody has is
    /// left the way the compositor brought it up — which is the compositor's
    /// own config, and not something the shell should overrule by sending an
    /// orientation it invented.
    fn sync_turn(&mut self) {
        let Some(control) = self.shell_control.clone() else {
            return;
        };
        if control.version() < TRANSFORM_SHELL_VERSION {
            return;
        }
        let mut sent = false;
        for panel in &mut self.panels {
            let Some(wanted) = settings::turn_for(&panel.name) else {
                continue;
            };
            if panel.applied_turn == Some(wanted) {
                continue;
            }
            let Ok(turn) = lxb_shell_v1::Transform::try_from(wanted.code()) else {
                continue;
            };
            control.set_output_transform(&panel.output, turn);
            panel.applied_turn = Some(wanted);
            sent = true;
            tracing::debug!(display = %panel.name, ?wanted, "asked for this orientation");
        }
        if sent {
            if let Err(err) = self.conn.flush() {
                tracing::warn!(?err, "could not send the orientation");
            }
        }
    }

    /// Hand the Settings column what the machine can play through and record
    /// from, and rebuild it if that changed.
    ///
    /// The listing is only kept fresh while somebody is standing in the
    /// Settings column, because reading it is three subprocesses and nothing
    /// else in the shell is about it — see [`Quick::watch_devices`]. Any
    /// display's column: each screen has an XMB of its own and a cursor of its
    /// own, and the page can be open on the second one while the first shows
    /// something else entirely.
    ///
    /// Only screens that are actually being drawn count. A display with an
    /// application over the whole of it is a display whose bar is not on
    /// screen, and a cursor left standing in Settings behind a fullscreen game
    /// must not have the shell polling the sound server for the length of it.
    fn sync_sound_devices(&mut self) {
        let (id, ..) = apps::SHELL_SETTINGS;
        let watching = (0..self.panels.len()).any(|index| {
            self.panel_is_visible(index)
                && self.panels[index]
                    .cursor
                    .current_category(&self.xmb)
                    .is_some_and(|category| category.id == id)
        });
        self.quick.watch_devices(watching);
        if !watching {
            return;
        }
        if settings::note_devices(self.quick.devices()) {
            settings::refresh(&mut self.xmb.categories);
            self.needs_redraw = true;
        }
    }

    /// Hand the Settings column which way up each display is being drawn, and
    /// rebuild it if that changed.
    ///
    /// Only the displays the compositor reports one for, which are the ones it
    /// turns itself: a display it says nothing about is one the Orientation
    /// page must not list, because choosing there would do nothing.
    fn refresh_display_turns(&mut self) {
        let reported: Vec<(String, settings::Orientation)> = self
            .panels
            .iter()
            .filter_map(|panel| Some((panel.name.clone(), panel.turned?)))
            .collect();
        if settings::note_turned(reported) {
            settings::refresh(&mut self.xmb.categories);
            self.needs_redraw = true;
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
        // Whether the keys belong to the shell rather than to whatever is in
        // front. Asked of the *field* as well as of the board, because the two
        // do not end together: dismissing the board sets `types_here` false at
        // once and then takes a quarter of a second to fall off the bottom of
        // the display, and the field it was raised over is still there and
        // still being typed into throughout. A surface that handed the keys
        // back for that quarter second would lose whatever was typed in it —
        // which, since what dismissed the board was the user starting to type,
        // is the very next letter of the word.
        let typing_here = self.osk.types_here() || self.field_wanted();
        let states: Vec<((Layer, KeyboardInteractivity), Clickable)> = (0..self.panels.len())
            .map(|index| {
                let state = self.guide.surface_state(
                    index == self.focused_panel,
                    app_running,
                    self.keep_keyboard_grabbed,
                    self.launching_on(index),
                    keyboard,
                    typing_here,
                    // Only the display being driven. A bubble is the shell
                    // asking for a moment of the user's attention, and it asks
                    // on the screen they are looking at — the same rule the
                    // guide follows, and for the same reason: two corners
                    // announcing the same thing is one of them talking to
                    // nobody.
                    index == self.focused_panel && !self.notifications.toasts().is_empty(),
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

/// The rows over one Steam title.
///
/// Kept separate from the live menu so what a game offers is testable without
/// a Wayland session.
///
/// Two bands, for the reason the menu over one of the user's files has two —
/// see [`media_rows`]. Everything above the rule is done to the *game*, and
/// every one of those needs Valve's client, so a machine without one offers
/// none of them rather than rows that can only refuse. Nothing below the rule
/// is about the game at all: Sort is about the column, Cancel is about the
/// menu, and neither has anything to ask Steam — which is why the shorter menu
/// on a machine with no client is still worth raising.
fn steam_game_menu_rows(game: &apps::Game) -> Vec<menu::Entry> {
    let mut rows = Vec::new();
    if game.steam_client {
        if game.installed {
            rows.push(menu::Entry::new(menu::Command::Launch, "Play").glyph(icons::LAUNCH));
            // Verifying is the client's own window, and is named as such:
            // unlike Play and Uninstall, it is not something this shell can
            // put a screen of its own in front of. It runs for minutes and
            // the only account of how it is going is Steam's.
            rows.push(menu::Entry::new(
                menu::Command::SteamDo(lxb_steam::Doing::Verify),
                lxb_steam::Doing::Verify.label(),
            ));
            rows.push(
                menu::Entry::new(menu::Command::SteamUninstall(game.app_id), "Uninstall")
                    .glyph(icons::UNINSTALL)
                    .grave(),
            );
        } else {
            rows.push(
                menu::Entry::new(menu::Command::SteamInstall(game.app_id), "Install")
                    .glyph(icons::LAUNCH),
            );
        }
    }
    rows.push(
        menu::Entry::new(menu::Command::SteamSort, "Sort")
            .glyph(icons::SORT)
            .group(1),
    );
    rows.push(menu::Entry::new(menu::Command::Dismiss, "Cancel").group(1));
    rows
}

/// The rows of the Steam column's Sort list: the eight orders, the one in force
/// ticked, and any this library cannot be put in greyed out.
///
/// The greying is not decoration. A machine with nothing installed knows no
/// sizes — Steam does not say how big a game is until it is being fetched — and
/// an account whose last-played times did not arrive has no dates to sort by.
/// Offered, chosen, and doing nothing is what the user would read as the shell
/// being broken, where an outline says out loud that the answer is not here.
fn steam_sort_rows(
    now: lxb_steam::library::Sort,
    knows: lxb_steam::library::Orders,
) -> Vec<menu::Entry> {
    let mut rows: Vec<menu::Entry> = lxb_steam::library::SORTS
        .iter()
        .map(|sort| {
            let row = menu::Entry::new(menu::Command::SteamSortBy(*sort), sort.label());
            let row = if *sort == now {
                row.glyph(icons::CHOSEN)
            } else {
                row
            };
            if sort.orders(knows) {
                row
            } else {
                row.disabled()
            }
        })
        .collect();
    rows.push(menu::Entry::new(menu::Command::Dismiss, "Cancel").group(1));
    rows
}

#[cfg(test)]
mod steam_game_menu_tests {
    use super::*;

    fn game(installed: bool, steam_client: bool) -> apps::Game {
        apps::Game {
            app_id: 7,
            name: "Fixture".to_string(),
            note: String::new(),
            installed,
            updating: false,
            steam_client,
        }
    }

    fn labels(game: &apps::Game) -> Vec<String> {
        steam_game_menu_rows(game)
            .into_iter()
            .map(|row| row.label)
            .collect()
    }

    /// Every row that acts on a game needs Valve's client, so a machine
    /// without one offers none of them rather than rows that can only refuse.
    /// What is left is the band that was never about the game: the column's
    /// order, which is the shell's own to change, and the way out.
    #[test]
    fn without_a_client_there_is_nothing_to_do_to_the_game() {
        assert_eq!(labels(&game(true, false)), vec!["Sort", "Cancel"]);
        assert_eq!(labels(&game(false, false)), vec!["Sort", "Cancel"]);
    }

    /// The two halves of the column are two situations and not two states of
    /// one: a game that is here can be played, checked or removed, and a game
    /// that is not can only be fetched.
    #[test]
    fn what_a_game_offers_depends_on_whether_it_is_here() {
        assert_eq!(
            labels(&game(true, true)),
            vec!["Play", "Verify with Steam", "Uninstall", "Sort", "Cancel"]
        );
        assert_eq!(
            labels(&game(false, true)),
            vec!["Install", "Sort", "Cancel"]
        );
    }

    /// Sort is below the rule with Cancel, and not up with the commands. What
    /// is above the rule is done to this game; Sort is about the column it is
    /// standing in, exactly as the Sort row over one of the user's files is
    /// about the shelf rather than the file.
    #[test]
    fn the_order_of_the_column_is_not_a_thing_done_to_the_game() {
        let rows = steam_game_menu_rows(&game(true, true));
        let sort = rows
            .iter()
            .find(|row| row.command == menu::Command::SteamSort)
            .expect("the Sort row");
        assert_eq!(sort.group, 1);
        assert!(rows
            .iter()
            .filter(|row| row.group == 0)
            .all(|row| row.command != menu::Command::SteamSort),);
    }

    /// The Sort list offers every order, ticks the one in force, and greys the
    /// ones this library has nothing to be sorted by — with the tick still on a
    /// greyed row if that is where it belongs, because what the column is
    /// listed in is a fact about it whether or not it can be changed from here.
    #[test]
    fn the_sort_list_ticks_one_order_and_greys_what_steam_cannot_answer() {
        use lxb_steam::library::{Orders, Sort, SORTS};

        let nothing_known = Orders::default();
        let rows = steam_sort_rows(Sort::InstalledFirst, nothing_known);
        assert_eq!(
            rows.len(),
            SORTS.len() + 1,
            "every order, and the way out of the list"
        );
        assert_eq!(
            rows.last().map(|row| row.command),
            Some(menu::Command::Dismiss)
        );

        let ticked: Vec<&str> = rows
            .iter()
            .filter(|row| row.glyph == Some(icons::CHOSEN))
            .map(|row| row.label.as_str())
            .collect();
        assert_eq!(ticked, [Sort::InstalledFirst.label()]);

        let greyed: Vec<&str> = rows
            .iter()
            .filter(|row| !row.enabled)
            .map(|row| row.label.as_str())
            .collect();
        assert_eq!(
            greyed,
            [
                Sort::RecentlyPlayedFirst.label(),
                Sort::MostPlayedFirst.label(),
                Sort::LeastPlayedFirst.label(),
                Sort::LargestFirst.label(),
                Sort::SmallestFirst.label(),
            ],
            "a library with nothing installed and no playtimes can only be \
             sorted by what it is called"
        );

        // And a library Steam has answered for in full offers all eight.
        let everything = Orders {
            sizes: true,
            playtimes: true,
            played: true,
        };
        let rows = steam_sort_rows(Sort::LargestFirst, everything);
        assert!(rows.iter().all(|row| row.enabled));
        assert_eq!(
            rows.iter()
                .find(|row| row.glyph == Some(icons::CHOSEN))
                .map(|row| row.label.as_str()),
            Some(Sort::LargestFirst.label())
        );
    }

    /// None of the Sort rows holds the panel. Choosing an order re-lists the
    /// column behind the menu and takes the cursor to the head of it, so the
    /// answer is the bar rather than the panel — unlike Open with, where the
    /// tick moving *is* the whole answer and the panel has to stay to be read.
    #[test]
    fn choosing_an_order_is_a_way_off_the_menu() {
        let rows = steam_sort_rows(
            lxb_steam::library::Sort::default(),
            lxb_steam::library::Orders {
                sizes: true,
                playtimes: true,
                played: true,
            },
        );
        assert!(rows.iter().all(|row| !row.holds));
    }

    /// Only the row that really does raise a window of Steam's is named for
    /// it. Play, Install and Uninstall are the shell's own presses now — they
    /// are answered with a loading screen or a panel of the shell's, and a row
    /// that said "with Steam" would be promising a window that never comes.
    #[test]
    fn only_the_row_that_raises_a_steam_window_is_named_for_it() {
        let rows = steam_game_menu_rows(&game(true, true));
        assert_eq!(rows[0].command, menu::Command::Launch);
        assert!(rows[1].label.ends_with("with Steam"));
        assert!(!rows[2].label.contains("Steam"));
    }

    /// Removing a game is asked about before it happens, and the row that asks
    /// is not the row that does it. Valve's client is told not to put its own
    /// confirmation up, so this menu row must reach the shell's panel — a menu
    /// wired straight to the removal would delete somebody's game on one
    /// press with nothing in between.
    #[test]
    fn the_uninstall_row_asks_rather_than_removes() {
        let rows = steam_game_menu_rows(&game(true, true));
        assert_eq!(rows[2].command, menu::Command::SteamUninstall(7));
        assert_ne!(rows[2].command, menu::Command::SteamUninstallNow(7));
    }
}

/// The rows of the menu over one of the user's own files.
///
/// A free function rather than a method, so the one thing about this menu that
/// is a decision — which rows it has, in what order, and where the rule between
/// the bands falls — can be exercised without a running session behind it.
/// `handled` is whether anything installed says it opens files of this kind;
/// `deletable` is whether the file is the user's own to delete.
fn media_rows(handled: bool, deletable: bool) -> Vec<menu::Entry> {
    let open_with = menu::Entry::new(menu::Command::OpenWith, "Open with").glyph(icons::OPEN_WITH);
    let delete = menu::Entry::new(menu::Command::Delete, "Delete")
        .glyph(icons::UNINSTALL)
        .grave();
    vec![
        menu::Entry::new(menu::Command::Open, "Open").glyph(icons::LAUNCH),
        if handled {
            open_with
        } else {
            open_with.disabled()
        },
        // Grave, for the reason Uninstall is: the row asks rather than
        // deletes, but it is the step towards losing the file, and the warmth
        // has to be under the highlight while that is still the user's choice
        // to make. The Yes it leads to carries the fixed red as well.
        if deletable { delete } else { delete.disabled() },
        // The band break: everything above acts on the file, and neither of
        // these two does — one is about the column and the other is about the
        // menu.
        menu::Entry::new(menu::Command::Sort, "Sort")
            .glyph(icons::SORT)
            .group(1),
        menu::Entry::new(menu::Command::Dismiss, "Cancel").group(1),
    ]
}

/// The rows of the Open with list, built from the applications it is offering,
/// with the tick on the one at `chosen` — the application that opens the type
/// as things stand.
///
/// The same tick the Settings column puts on the value a setting is currently
/// set to, making the same statement. Every row holds the panel: this is a list
/// of alternatives being set rather than a list of commands being run, so a
/// press moves the tick and the user leaves in their own time.
fn open_with_rows(offering: &[media::Handler], chosen: usize) -> Vec<menu::Entry> {
    let mut rows: Vec<menu::Entry> = offering
        .iter()
        .enumerate()
        .map(|(index, handler)| {
            let row = menu::Entry::new(menu::Command::OpenWithHandler(index), handler.name.clone())
                // An application the icon theme cannot answer for falls back to the
                // generic application picture, which the atlas already does for a
                // name it does not know — the empty one included.
                .icon(handler.icon.clone().unwrap_or_default())
                .holds();
            if index == chosen {
                row.glyph(icons::CHOSEN)
            } else {
                row
            }
        })
        .collect();
    // And the way out does not hold, because leaving is what it is for.
    rows.push(menu::Entry::new(menu::Command::Dismiss, "Cancel").group(1));
    rows
}

/// The rows of the Sort list: the nine orders, the one in force ticked, and any
/// this collection cannot be put in greyed out.
fn sort_rows(now: media::Sort, knows: media::Orders) -> Vec<menu::Entry> {
    let mut rows: Vec<menu::Entry> = media::SORTS
        .iter()
        .map(|sort| {
            let row = menu::Entry::new(menu::Command::SortBy(*sort), sort.label());
            let row = if *sort == now {
                row.glyph(icons::CHOSEN)
            } else {
                row
            };
            if sort.orders(knows) {
                row
            } else {
                row.disabled()
            }
        })
        .collect();
    rows.push(menu::Entry::new(menu::Command::Dismiss, "Cancel").group(1));
    rows
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

/// How wide a sentence on the panel is allowed to be, in characters, and how
/// many lines of it are shown.
///
/// The panel gives a note one line and cuts what runs past it, and polkit's
/// messages are written by whoever wrote the policy file — "Authentication is
/// required to install untrusted software" is longer than any sentence the
/// shell writes for itself. So it is broken between words here rather than
/// being cut mid-word by the drawing. Three lines is the whole of every message
/// polkit ships; a fourth would be a policy file being unreasonable, and it ends
/// in an ellipsis so that the panel says it has been cut.
const MESSAGE_WIDTH: usize = 46;
const MESSAGE_LINES: usize = 3;

/// What the panel says while an authentication is on screen.
///
/// One function for every pass — as it opens, as PAM asks for something, and
/// after every keystroke — so the panel cannot change shape underneath the user
/// as it is rewritten. It takes the three things that change rather than the
/// whole authentication, and `typed` is a count: what the user actually typed
/// is not something this needs, and a panel built from it would be a copy of a
/// password living in the layout for as long as the panel was up.
fn authentication_lines(message: &str, note: &str, typed: usize) -> Vec<dialog::Line> {
    let mut lines = vec![dialog::Line::Heading("Authentication needed".to_string())];
    for line in wrapped(message, MESSAGE_WIDTH, MESSAGE_LINES) {
        lines.push(dialog::Line::Note(line));
    }
    lines.push(dialog::Line::Note(note.to_string()));
    lines.push(dialog::Line::Secret { typed });
    lines.push(dialog::Line::Rule);
    lines
}

/// What the panel says before PAM has asked for anything.
///
/// It is up for the fraction of a second before the helper answers, and it says
/// the same thing that helper is about to: something has to be typed, and whose
/// it has to be.
fn waiting_note(request: &polkit::Request) -> String {
    if request.yourself {
        "Enter your password to allow this.".to_string()
    } else {
        format!("Enter the password for {}.", request.user)
    }
}

/// What the panel says when PAM has asked for something.
///
/// PAM's own prompt is used only when it is not the one every machine asks:
/// "Password: " is a label for a field the panel has already drawn, and
/// replacing a sentence that says whose password is wanted with one word would
/// be losing the only part the user needs on a shell where several people's
/// passwords could be the answer. Anything else — a one-time code, a token, a
/// question a module invented — is shown as it came, because the shell has no
/// idea what it is asking for and must not pretend to.
fn prompt_note(prompt: &str, request: &polkit::Request) -> String {
    let plain = prompt.trim().trim_end_matches(':').trim().to_lowercase();
    let usual = plain.is_empty()
        || plain == "password"
        || plain.starts_with("password for ")
        || plain.ends_with("'s password");
    if usual {
        waiting_note(request)
    } else {
        prompt.trim().to_string()
    }
}

/// Break `text` between words into at most `lines` lines of about `width`
/// characters, ending in an ellipsis if there was more of it.
///
/// A word longer than the width is left whole and allowed to be the whole line:
/// breaking it would produce two halves of a word nobody can read, and the
/// drawing cuts a line that overruns anyway.
fn wrapped(text: &str, width: usize, lines: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        match out.last_mut() {
            Some(line) if line.chars().count() + 1 + word.chars().count() <= width => {
                line.push(' ');
                line.push_str(word);
            }
            _ => {
                if out.len() == lines {
                    // There is more, and no line left to put it on.
                    if let Some(line) = out.last_mut() {
                        line.push('…');
                    }
                    break;
                }
                out.push(word.to_string());
            }
        }
    }
    out
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

/// The rule behind [`Shell::window_app_name`], with everything it needs handed
/// to it: what the catalogue calls this window's class, if anything, and
/// whether the window is a game Valve's client started.
///
/// Free-standing because the shell it is a method on cannot be built without a
/// compositor, and this is the one piece of it worth checking on its own — it
/// decides what a button that *kills something* is called, and every way of
/// getting it wrong names one thing while ending another.
fn window_name(window: &WindowCard, installed: Option<&str>, from_steam: bool) -> String {
    if from_steam && !window.title.trim().is_empty() {
        return window.title.clone();
    }
    if let Some(name) = installed {
        return name.to_string();
    }
    app_id_name(&window.app_id).unwrap_or_else(|| window.title.clone())
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

/// The audible part of one cursor move, which is decided by where it landed
/// rather than by the control that made it.
///
/// Leaving a path is the one move that sounds different, because a category
/// button, the Left direction and a row in the breadcrumb trail are three
/// spellings of Back. The result is singular even when that click closes
/// several levels.
///
/// Only reached for a move that landed: the call is under the test for it, so
/// there is no silent case here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CursorFeedback {
    Step,
    Back,
}

fn cursor_feedback(before_depth: usize, after_depth: usize) -> CursorFeedback {
    if after_depth < before_depth {
        CursorFeedback::Back
    } else {
        CursorFeedback::Step
    }
}

/// Which of the shell's own screens a press was made on.
///
/// Only the two that have a voice. A press inside a context menu, a dialog or
/// the on-screen keyboard is that component's own and never reaches here —
/// see [`Shell::guide_stepped`] for why a component keeps its sound wherever
/// it is raised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Screen {
    Start,
    Guide,
}

/// The audible part of one press on a screen of the shell's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChosenFeedback {
    /// The press acted on nothing: an inert tile, a row that starts no
    /// process. Nothing happened, so nothing says it did.
    Silent,
    /// The press was answered, in the voice of the screen it was made on.
    Kept,
}

/// What pressing the guide's selected tile or row is about to do.
///
/// Read before the press is carried out, because carrying it out is what makes
/// the answer untrue: Resume closes the overlay, and by the time it has, the
/// application it handed the screen to is no longer behind anything.
///
/// Every press the guide answers at all it answers in its own voice, the
/// applications included. `app-launch.ogg` belongs to Start, where a press
/// *starts* something; the guide only ever goes back to what is already
/// running, and a screen with a voice of its own does not borrow another's for
/// half its rows. See [`Shell::answer_choice`].
fn chosen_feedback(item: Item, pointer_target: bool) -> ChosenFeedback {
    match item {
        // Two controls that are drawn but inert when there is nothing for them
        // to act on: the stick switch with no application to attach it to, and
        // the brightness bar, which has no equivalent of silencing a session.
        Item::Pointer if !pointer_target => ChosenFeedback::Silent,
        Item::Brightness => ChosenFeedback::Silent,
        _ => ChosenFeedback::Kept,
    }
}

/// Whether the session belongs to Start closely enough to carry its background
/// music.
///
/// One answer for the whole session rather than one per display, which is the
/// point of it: the music is not a property of a screen but of what the user is
/// doing, and somebody playing a game on one display and leaving Start up on
/// the other is playing a game. So a single application open anywhere ends it,
/// whichever display holds control and whatever is drawn on the rest. What is
/// left is the session where every screen is the Start screen, which is the
/// only one it belongs to.
///
/// A launch or a restore is the same answer arriving early: the display is
/// already being handed over, and waiting for the compositor's foreground event
/// would leave the music playing into the first second of the application.
///
/// The guide is not a screen that stops it: raised over an otherwise empty
/// Start it keeps the same background, exactly as the bar it was raised from
/// does. With no display there is no Start screen to hear.
fn xmb_music_wanted(has_panel: bool, apps_open: bool, handing_over: bool) -> bool {
    has_panel && !apps_open && !handing_over
}

/// Whether a press of the user's opens the menu, as a rule on its own.
///
/// It does, except over a launch. The splash is the shell's answer to the last
/// press — it says the button was heard and the application is on its way — and
/// a menu raised on top of that would be a second screen about the same moment,
/// with nothing on it that could act. Every row it offers is about something
/// that is already running: Resume goes back to the screen the splash is
/// covering, Close is about a window that does not exist yet. The press is
/// spent and nothing is said about it, which is the shell's ordinary answer to
/// a move that landed on nothing.
///
/// It is a bounded silence, and that is what makes it bearable. A splash gives
/// up on its own — twenty seconds for an application the shell forked, a minute
/// for a game handed to Valve's client, and at once if a process the shell is
/// watching dies — so the button is never dead for longer than the wait it
/// belongs to, and a launch that fails ends in a panel saying so rather than in
/// a screen nobody can leave.
///
/// `launching_here` is this display's launch and not the session's: a game
/// coming up on one screen is not a reason the other screen's menu should stop
/// working, and the splash is only ever on the display it was started from.
///
/// `showing` is the way out, and the reason this is not simply `!launching`. A
/// portal's question and an authorisation both raise the menu without asking
/// this, because they are the system asking rather than the user, and either
/// can arrive mid-launch. Refusing the press with the menu already up would
/// turn the button that gets the user out of everything into the one that
/// trapped them there.
fn guide_answers(launching_here: bool, showing: bool) -> bool {
    showing || !launching_here
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

/// What one of the shell's surfaces has under a point.
///
/// Named after the thing on screen rather than after the command it carries:
/// what a press does is the selection's business — see [`Shell::press_at`] —
/// and this is only where the finger is.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Spot {
    /// A key of the on-screen keyboard.
    Key(usize, usize),
    /// A button of the modal panel.
    DialogButton(usize),
    /// A row of the context menu. `level` is where along a mixer row's groove
    /// the point fell, and `None` for every row that has no groove — the
    /// speaker at its head included, which is a button rather than a value.
    /// `aside` is whether the point fell on the button at the row's right-hand
    /// end rather than on the row, which is the row's other command.
    MenuRow {
        row: usize,
        level: Option<f32>,
        aside: bool,
    },
    /// The screen around the context menu, which is a way out of it.
    OutsideMenu,
    /// A choice in the power dialog.
    PowerRow(usize),
    /// An entry in the guide's sidebar, with the same reading of `level` as a
    /// menu row's for the two quick-settings bars.
    Entry { item: Item, level: Option<f32> },
    /// A window miniature in the overview; the start screen's own card last.
    Card(usize),
    /// Something on the start screen.
    Bar(ui::BarSpot),
    /// Somewhere the shell is drawing but nothing of it is: the sidebar's own
    /// glass, the air between two keys, the wallpaper past the end of the bar.
    Nothing,
}

impl Spot {
    /// Whether the cursor over this should say there is something here.
    ///
    /// A groove counts, and so does the screen around a menu: pressing past a
    /// menu dismisses it, which is one of the things a press can do.
    fn is_actionable(self) -> bool {
        !matches!(self, Spot::Nothing)
    }
}

/// Whether `(x, y)` is inside a rectangle.
fn within([x, y, w, h]: [f32; 4], at_x: f32, at_y: f32) -> bool {
    at_x >= x && at_x < x + w && at_y >= y && at_y < y + h
}

/// How far a device with no notches has to travel to be worth one, in the units
/// `wl_pointer.axis` reports.
///
/// Fifteen has been a wheel click in every toolkit since X11, and it is near
/// enough what a touchpad reports for a comfortable finger's travel — which is
/// what it is here for: the shell moves in whole rows, so a two-finger drag
/// should be about one row per centimetre. A wheel says how many notches it
/// turned and is never measured against this at all.
const SCROLL_STEP: f64 = 15.0;

/// How many notches of the wheel one axis of one scroll event is worth.
///
/// Three ways of being told the same thing, in the order they can be trusted. A
/// high-resolution wheel counts in hundred-and-twentieths of a notch, an older
/// one counts whole notches, and a device with no notches at all — a touchpad,
/// a trackpoint, a stick — reports only a distance and is measured against
/// [`SCROLL_STEP`]. Taking a wheel at its word is what makes one click of it
/// one row on every machine, whatever that compositor decided a notch is worth
/// in pixels.
fn scroll_notches(axis: &AxisScroll) -> f64 {
    if axis.value120 != 0 {
        return f64::from(axis.value120) / 120.0;
    }
    if axis.discrete != 0 {
        return f64::from(axis.discrete);
    }
    axis.absolute / SCROLL_STEP
}

/// How many whole steps of the selection `notches` are worth, and what is left
/// over to be carried into the next event.
///
/// The remainder is the point of it. A touchpad reports a stream of fractions
/// of a notch, and a shell that rounded each of them to nothing would never
/// move at all under a slow two-finger drag; one that rounded each of them up
/// would cross the whole column under the same drag.
fn scroll_steps(notches: f64) -> (i32, f64) {
    let whole = notches.trunc();
    (whole as i32, notches - whole)
}

/// How far a finger may wander and still have been a tap, in logical pixels.
///
/// A finger is never still, and a screen that dropped a press over two pixels
/// of tremble would be one that ignores half of what it is told. Wider than a
/// mouse would need, because a fingertip is wider than a cursor.
const TAP_SLOP: f32 = 24.0;

/// A finger that is currently down.
#[derive(Debug, Clone, Copy)]
struct Touch {
    /// Which display it landed on, and where on it.
    panel: usize,
    at: (f32, f32),
    /// Whether it has since travelled far enough to be a drag rather than a
    /// tap, in which case letting go of it presses nothing.
    dragged: bool,
    /// Whether putting it down was what moved control to this display, in
    /// which case letting go of it presses nothing either: the tap is spent on
    /// the move.
    claimed: bool,
}

/// A key currently held down on the keyboard, and when it next acts.
///
/// One at a time, which is what `wl_keyboard` describes: a second key pressed
/// before the first is let go takes the repeat over, and the release of a key
/// that already lost it changes nothing.
#[derive(Debug, Clone, Copy)]
struct HeldKey {
    keysym: Keysym,
    next: Instant,
}

impl HeldKey {
    fn pressed(keysym: Keysym, now: Instant) -> Self {
        Self {
            keysym,
            next: now + controller::INITIAL_REPEAT_DELAY,
        }
    }

    /// Whether the key is due to act again, booking the step after it if so.
    fn due(&mut self, now: Instant) -> bool {
        if now < self.next {
            return false;
        }
        // Booked from `now` rather than from the deadline just passed, so a
        // frame that ran long is never followed by a burst of stale steps —
        // the same rule the D-pad's repeat is scheduled by.
        self.next = now + controller::REPEAT_INTERVAL;
        true
    }
}

/// Whether holding this key down should go on acting, or acted once and is
/// now simply a key that is down.
///
/// Only the directions are a *rate*: a held Return would launch the same
/// application over and over, a held Home would flicker the guide open and
/// shut, and a held Escape would back out of every column the user has opened
/// rather than the one they are in.
///
/// A field being typed into is the exception, and takes the ordinary meaning
/// of a held key: a letter fills it and Backspace empties it. The two keys
/// that *finish* the field are not among them, for the reason above — one
/// password is submitted once.
fn key_repeats(keysym: Keysym, typing: bool) -> bool {
    if typing {
        return matches!(
            keyboard::stroke_for(keysym),
            Some(keyboard::Stroke::Char(_) | keyboard::Stroke::BACKSPACE)
        );
    }
    matches!(
        action_for_keysym(keysym),
        Some(Action::Left | Action::Right | Action::Up | Action::Down)
    )
}

/// The notification panel's rows: a way to clear the lot, and under it one per
/// announcement, newest first.
///
/// Newest first because that is the order they are kept in — see
/// [`notify::Center`] — and not because anything sorts them here. The one place
/// that decides the order is the place that holds them.
///
/// Clearing sits at the *top*, above the separator. It used to sit under the
/// list, where it is the last thing read and therefore, on the face of it,
/// where a summary belongs. But a list of announcements is a list that scrolls,
/// and the bottom of one is somewhere the user has to travel to: the row was
/// off the panel the moment there were more announcements than rows, and
/// reaching it meant either the whole journey down or an Up that wraps the
/// highlight round and drags the window to the end of the list with it. At the
/// top it is on the panel from the moment the panel opens, and one press of Up
/// from where the highlight starts.
///
/// A free function, and not a method, for the same reason
/// [`steam_game_menu_rows`] is one: the order of these rows is the whole of
/// what this decides, and a list that can be built without a running shell is
/// one a test can read back.
fn notification_rows(list: &[notify::Notification]) -> Vec<menu::Entry> {
    if list.is_empty() {
        // A panel refuses to open with nothing choosable on it, and this one
        // has to open: the tile's whole question is *did I miss anything*, and
        // "no" is an answer the user is entitled to get rather than a button
        // that appears not to work. So the empty list is a row — one that says
        // so, and that puts the panel away when it is pressed, which is what
        // every other way out of this panel does too.
        //
        // Alone, with no Clear All over it. A row that throws away nothing is
        // furniture, and the one thing this panel has to say when it is empty
        // is that it is empty.
        return vec![
            menu::Entry::new(menu::Command::Dismiss, "Nothing to read").glyph(icons::NOTIFICATIONS)
        ];
    }

    let mut entries = vec![
        menu::Entry::new(menu::Command::DismissNotifications, "Clear All")
            .glyph(icons::UNINSTALL)
            .holds(),
    ];
    entries.extend(list.iter().map(|held| {
        // A press opens it where there is something to open, and puts it away
        // where there is not. An announcement with no buttons has nothing
        // behind it but a list of one, and making somebody step into that to
        // get back out again would be the panel wasting their time; one with
        // buttons must not be dismissed by the press that was reaching for
        // them.
        let command = if held.actions.is_empty() {
            menu::Command::DismissNotification(held.id)
        } else {
            menu::Command::ShowNotification(held.id)
        };
        let entry = menu::Entry::new(command, held.title())
            // When it arrived, on a line of its own above the summary, and
            // whatever the program went on to say below it.
            //
            // The two shared a line until the user asked for this, separated by
            // a dot: "now · Would you like to install updates now?". It was one
            // line shorter, and it read as one sentence that began with a time
            // — the reader had to find the separator before either half meant
            // anything. Split, each run is one thing: a time, a summary, and
            // what was said. The cost is a third line, and therefore fewer
            // announcements on a panel; the user has seen both and chosen.
            .stamp(age_of(held.arrived))
            // An empty body leaves the row two lines rather than putting an
            // empty one under the summary: `detail` takes nothing as nothing.
            .detail(held.body.trim())
            // What the launcher's catalogue draws for the program that sent it,
            // wherever it knows the program — so the row in this panel wears
            // the same picture as the tile on the start screen. Anything it
            // cannot answer for falls back to the generic application icon,
            // which is the honest picture of a program the machine knows
            // nothing else about. The bell for a program that named no icon at
            // all, for the same reason the bubble in the corner wears one: the
            // shell's own mark is true — this is an announcement — where an
            // empty column is a row that looks like it is still loading.
            .icon(held.icon_name().unwrap_or(icons::NOTIFICATIONS))
            // And a button of its own on the end of the row, for the one thing
            // an announcement is nearly always wanted for. Reading it and
            // throwing it away is the whole of what this list is for, and
            // without this the throwing away is a step into the row and a
            // Dismiss at the bottom of what is behind it.
            .aside(
                menu::Command::DismissNotification(held.id),
                icons::UNINSTALL,
            )
            // Below the separator that Clear All sits above. A group of its own
            // is what draws the line between the one row that acts on the whole
            // list and the list itself.
            .group(1);
        // The panel stays up for a row that dismisses. The answer to pressing
        // one is the row going, and a panel that folded away would take the
        // answer with it before it could be seen — the same reason a mixer's
        // tracks hold it. A row that *opens* does not need telling: stepping
        // into a list keeps the panel by definition.
        if held.actions.is_empty() {
            entry.holds()
        } else {
            entry
        }
    }));
    entries
}

/// How long ago something arrived, as a row says it.
///
/// Rounded down and coarse on purpose, and never a clock time. What the row is
/// answering is *did I miss this, or has it only just happened* — and to that
/// question "3m" and "3:07" are the same answer, except that one of them makes
/// the reader do the arithmetic. Anything inside a minute is "now": a list
/// somebody has just opened should not be counting seconds at them.
///
/// Days rather than dates at the far end, because a notification a week old is
/// a list nobody has cleared rather than a record anyone is consulting.
fn age_of(arrived: Instant) -> String {
    let seconds = arrived.elapsed().as_secs();
    match seconds {
        0..60 => "now".to_string(),
        60..3600 => format!("{}m", seconds / 60),
        3600..86_400 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86_400),
    }
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
/// Whether the windows a display has just lost amount to an application
/// walking out of it — which is the shell's cue to take the screen back and
/// come forward on to it; see [`Shell::an_application_ended_by_itself`].
///
/// Two conditions, and the second is as important as the first. It has to be
/// something the shell did not end: a card closed from the guide leaves the
/// same hole in the window list, and the user who closed it is standing in the
/// menu watching the deck close up, which is that press's own answer. And it
/// has to have been *filling the display*, because that is what makes this the
/// bar being handed a screen. A dialog closing over a start screen that was on
/// show throughout hands it nothing, and a dialog closing over a game that is
/// still running hands it nothing either.
///
/// `ended_by_the_shell` is spent as it is read: an id names one window, and a
/// claim left lying about would be spent on some later window that happened to
/// be given the same number. Every departing window is asked about for that
/// reason, rather than the list being searched for the first that answers.
fn walked_out(
    gone: &[WindowCard],
    width: u32,
    height: u32,
    ended_by_the_shell: &mut HashSet<u32>,
) -> bool {
    let mut by_itself = false;
    for window in gone {
        if !ended_by_the_shell.remove(&window.id) {
            by_itself = true;
        }
    }
    by_itself && bar_is_covered(width, height, gone)
}

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

/// The catalogue has to be complete *and* the held wallpaper frame has to
/// have been presented before the bar's ordinary arrival clock may move.
fn startup_arrival_can_advance(icons_ready: bool, has_frame: bool) -> bool {
    icons_ready && has_frame
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
        "screenshot" => Action::Screenshot,
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

/// How far one direction moves the mixer's System row.
///
/// The same step the machine's own controls take, deliberately: the row sits
/// in a column with the applications' rows and under the sidebar's own bars,
/// and a row that travelled at a different pace from the ones around it would
/// be a different control that happened to look the same.
const SHELL_SOUND_STEP: f32 = 0.05;

/// Move the shell's own sound by one step. Reports whether it moved.
///
/// Turning it up is also how a silenced shell is brought back, which is what
/// every volume control on the machine does — [`Quick::nudge`] included, and
/// this row is the one beside those.
fn nudge_shell_sound(delta: i32) -> bool {
    let sound = settings::sound();
    let value = (sound.value + delta as f32 * SHELL_SOUND_STEP).clamp(0.0, 1.0);
    settings::set_sound(system::Level {
        value,
        muted: sound.muted && delta <= 0,
    })
}

/// Put it exactly where the groove was clicked, on the same terms: dragging a
/// silenced shell up brings it back, because it is the same gesture made with
/// a different instrument.
fn set_shell_sound(value: f32) -> bool {
    let sound = settings::sound();
    settings::set_sound(system::Level {
        value,
        muted: sound.muted && value <= sound.value,
    })
}

/// Adapts the renderer's atlas lookup to what the layout code needs.
struct Slots<'a> {
    gpu: &'a Gpu,
    art: &'a art::Art,
    drained: &'a Drained,
}

impl SlotLookup for Slots<'_> {
    fn slot_for(&self, icon: Option<&str>) -> Option<u32> {
        icon.and_then(|name| self.gpu.slot(name))
            .or_else(|| self.gpu.slot(FALLBACK_APP_ICON))
    }

    fn glyph(&self, name: &str) -> Option<u32> {
        self.gpu.slot(name)
    }

    fn thumbnail(&self, path: &std::path::Path) -> Option<gpu::Thumb> {
        self.gpu.thumbnail(path)
    }

    /// Two questions, because a game's cover is not filed under a path the
    /// layout could know: where the picture ended up, which is the art
    /// worker's business, and whether the atlas is still holding it.
    fn cover(&self, app_id: u32) -> Option<gpu::Thumb> {
        self.gpu.thumbnail(self.art.cover(app_id)?)
    }

    /// The fading answer rather than the plain one: a cover that has just
    /// become playable is somewhere between grey and colour for a moment.
    fn drain(&self, app_id: u32, installed: bool) -> f32 {
        self.drained.of(app_id, installed)
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
                // it has a seat with no pointer on it. Everything the
                // controller reaches — which is everything — carries on.
                Err(err) => tracing::info!(?err, "no pointer on this seat"),
            }
        }
        if capability == Capability::Touch && self.touch.is_none() {
            match self.seat_state.get_touch(qh, &seat) {
                Ok(touch) => self.touch = Some(touch),
                Err(err) => tracing::info!(?err, "no touchscreen on this seat"),
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
            self.pointer_enter = None;
            if let Some(pointer) = self.pointer.take() {
                pointer.release();
            }
        }
        if capability == Capability::Touch {
            self.touches.clear();
            if let Some(touch) = self.touch.take() {
                touch.release();
            }
        }
    }

    fn remove_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: wl_seat::WlSeat) {
    }
}

impl PointerHandler for Shell {
    /// The pointer, over whatever of the shell it has reached.
    ///
    /// The input region is cut to exactly what the shell is showing — see
    /// `sync_surface_state` — so an event arriving here is one the shell is
    /// meant to answer, and what it is about is whatever [`Shell::spot_at`]
    /// finds under it.
    fn pointer_frame(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _pointer: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            let Some(index) = self.panels.iter().position(|p| p.owns(&event.surface)) else {
                continue;
            };
            let (x, y) = (event.position.0 as f32, event.position.1 as f32);

            match event.kind {
                PointerEventKind::Enter { serial } => {
                    // Kept because every later request for a cursor shape has
                    // to carry the serial of the enter that put the pointer
                    // here, however long ago that was.
                    self.pointer_enter = Some(serial);
                    self.pointer_shape = None;
                    self.hover(qh, index, x, y);
                }
                PointerEventKind::Leave { .. } => {
                    self.pointer_enter = None;
                    self.pointer_shape = None;
                    // Half a notch of wheel left over belongs to the surface
                    // the pointer has left, not to the next one it arrives on.
                    self.scrolled = (0.0, 0.0);
                }
                PointerEventKind::Motion { .. } => {
                    self.hover(qh, index, x, y);
                }
                // On the press rather than the release, because that is when a
                // key on a keyboard fires, when a controller's `A` fires, and
                // when every other button in this shell fires.
                PointerEventKind::Press { button, .. } => {
                    let Some(spot) = self.press_on(index, x, y) else {
                        continue;
                    };
                    match button {
                        BTN_LEFT => self.press_at(spot),
                        // The context menu is the pad's `Y`: the short list of
                        // things that can be done to whatever is selected, as
                        // against the one thing a press does. That is what a
                        // right button has meant on every desktop since there
                        // were two of them, so it is bound to the same action
                        // and inherits every rule about where it may be raised.
                        BTN_RIGHT if self.aim_at(spot) => self.on_action(Action::Menu),
                        _ => {}
                    }
                }
                // Where the wheel was turned decides which display it turns
                // rather than what on it: it moves the selection, and where
                // the selection is is not the pointer's business. Turning it
                // is an act on that display in the way that resting the
                // pointer over it is not, so it takes control exactly as a
                // click does — and then turns the display it has taken.
                //
                // Not while only the startup wallpaper is up: there is no bar
                // to scroll yet, and a wheel turned during those first frames
                // would move a selection the user cannot see.
                PointerEventKind::Axis {
                    horizontal,
                    vertical,
                    ..
                } if self.startup.ready => {
                    self.focus_panel(index);
                    self.scroll_by(&horizontal, &vertical);
                }
                _ => {}
            }
        }
    }
}

impl TouchHandler for Shell {
    /// A finger arriving.
    ///
    /// It selects but does not press: what a tap does is decided when it is
    /// lifted, because until then it may yet turn out to be a drag. The
    /// selection moving under the finger is the whole of the feedback a
    /// touchscreen can give — there is no hovering, so this is the only chance
    /// to say what is about to be pressed.
    fn down(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _touch: &wl_touch::WlTouch,
        _serial: u32,
        _time: u32,
        surface: wl_surface::WlSurface,
        id: i32,
        position: (f64, f64),
    ) {
        if !self.startup.ready {
            return;
        }
        let Some(index) = self.panels.iter().position(|p| p.owns(&surface)) else {
            return;
        };
        let at = (position.0 as f32, position.1 as f32);
        // A finger put down on a display that has not got control takes it, and
        // that is the whole of what this touch does — the same rule a click
        // follows, and for the same reason: what it would otherwise press was
        // chosen on a screen that was not being driven when it was touched.
        let claimed = self.focus_panel(index);
        self.touches.insert(
            id,
            Touch {
                panel: index,
                at,
                dragged: false,
                claimed,
            },
        );
        if claimed {
            return;
        }
        let spot = self.spot_at(index, at.0, at.1);
        self.point_at(spot);
    }

    /// A finger lifting, which is the press — if it stayed where it was put.
    fn up(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _touch: &wl_touch::WlTouch,
        _serial: u32,
        _time: u32,
        id: i32,
    ) {
        let Some(touch) = self.touches.remove(&id) else {
            return;
        };
        if touch.dragged || touch.claimed {
            return;
        }
        let spot = self.spot_at(touch.panel, touch.at.0, touch.at.1);
        self.press_at(spot);
    }

    /// A finger moving. Past the slop it stops being a tap, and from then on it
    /// presses nothing however it ends.
    fn motion(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _touch: &wl_touch::WlTouch,
        _time: u32,
        id: i32,
        position: (f64, f64),
    ) {
        let Some(touch) = self.touches.get_mut(&id) else {
            return;
        };
        let travelled = (position.0 as f32 - touch.at.0).hypot(position.1 as f32 - touch.at.1);
        if travelled > TAP_SLOP {
            touch.dragged = true;
        }
    }

    /// The compositor has taken the sequence away — a gesture it decided was
    /// its own. Nothing was pressed, and nothing is now.
    fn cancel(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _touch: &wl_touch::WlTouch) {
        self.touches.clear();
    }

    fn shape(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _touch: &wl_touch::WlTouch,
        _id: i32,
        _major: f64,
        _minor: f64,
    ) {
    }

    fn orientation(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _touch: &wl_touch::WlTouch,
        _id: i32,
        _orientation: f64,
    ) {
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
            // Every key is released by a leave, and a repeat the shell never
            // hears the end of is a bar that walks by itself.
            self.held_key = None;
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
        if !self.startup.ready {
            // Do not remember it for repeat either: a key pressed over the
            // wallpaper must be pressed again once controls exist.
            self.held_key = None;
            return;
        }
        // Taken before the key is acted on, so that whatever the press opens
        // is already the screen the repeat will be walking through.
        self.held_key = Some(HeldKey::pressed(event.keysym, Instant::now()));
        self.on_key(event.keysym);
    }

    fn repeat_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        _event: KeyEvent,
    ) {
        // Never called here: the toolkit only repeats for a client that runs
        // its keyboard in a calloop, and this shell runs its own loop so that
        // a gamepad — which is no Wayland object — can wake it. The repeat is
        // therefore the shell's own, in [`Shell::repeat_held_key`], and acting
        // on this as well would step the bar twice for every one press.
    }

    fn release_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        event: KeyEvent,
    ) {
        // Only if it is still the key that is repeating: a second key pressed
        // meanwhile has taken the repeat over, and letting go of the first must
        // not stop it.
        if self
            .held_key
            .is_some_and(|held| held.keysym == event.keysym)
        {
            self.held_key = None;
        }
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

    /// The seat's repeat setting, of which only the switch is the shell's
    /// business.
    ///
    /// A user who turned repeat off gets no repeat here either. The *rate*
    /// they set is a different matter and is not taken: it is the rate their
    /// applications type at, and the bar is not being typed into — it steps
    /// through categories, at the one pace the D-pad already steps them at.
    fn update_repeat_info(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        info: RepeatInfo,
    ) {
        self.key_repeat_on = !matches!(info, RepeatInfo::Disable);
        if !self.key_repeat_on {
            self.held_key = None;
        }
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
            lxb_shell_v1::Event::ShareRequest { id, app_id } => {
                tracing::info!(id, %app_id, "the portal is asking about the screen");
                if state.startup.ready {
                    state.ask_to_share(id, &app_id);
                } else if state.pending_share.is_none() {
                    state.pending_share = Some((id, app_id));
                } else if let Some(control) = state.shell_control.as_ref() {
                    // The first question is already waiting for controls it
                    // can be shown on. A second cannot inherit its eventual
                    // answer, so it is explicitly refused on the same terms as
                    // a second question arriving while the panel is visible.
                    tracing::info!(id, "refusing a second share question during startup");
                    control.answer_share(id, None);
                    let _ = state.conn.flush();
                }
            }
            lxb_shell_v1::Event::Screenshot { output } => {
                tracing::debug!("screenshot binding forwarded by the compositor");
                state.screenshot_output(&output);
            }
            lxb_shell_v1::Event::OutputCaptured { output, path } => {
                let name = state
                    .panels
                    .iter()
                    .find(|panel| panel.output == output)
                    .map(|panel| panel.name.clone())
                    .unwrap_or_default();
                // No panel either way. The picture itself is the answer the
                // user gets — that, the flash the display gave when it was
                // taken, and the shutter beside it — so these lines are for
                // the session that has to be told why there is no picture.
                match (!path.is_empty()).then(|| PathBuf::from(path)) {
                    Some(saved) => {
                        // Only on the answer, and only on a good one: the
                        // compositor sends this once the file is written and
                        // starts the flash in the same breath, so the sound
                        // and the flash arrive together and both mean a
                        // picture exists rather than that a chord was spelled.
                        state.sounds.shutter();
                        // And it is a picture in the user's pictures, so it
                        // belongs on the shelf now rather than whenever the
                        // walk next comes round. This is the file it just
                        // named, so nothing has to be searched for.
                        state.media.found(&saved);
                        tracing::info!(display = %name, ?saved, "photographed a display")
                    }
                    None => {
                        tracing::warn!(display = %name, "the display could not be photographed")
                    }
                }
            }
            lxb_shell_v1::Event::WindowCaptured { id, path } => {
                let saved = (!path.is_empty()).then(|| PathBuf::from(path));
                tracing::info!(id, ?saved, "the compositor answered a screenshot");
                // A photograph of one window is a photograph, and lands in the
                // same folder as any other. On the shelf as it is taken, for
                // the reason a whole display's is.
                if let Some(saved) = saved.as_deref() {
                    state.media.found(saved);
                }
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
                        // Not carried by this event either, and not implied by
                        // it in either direction: an SDR laptop panel can be
                        // warmed and a nested session cannot be, whatever
                        // either of them says about HDR. It arrives in
                        // output_night_light and is left exactly as it was
                        // until it does.
                        night_light: panel.hdr.night_light,
                        warming: panel.hdr.warming,
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
            lxb_shell_v1::Event::OutputNightLight {
                output,
                supported,
                active,
            } => {
                if let Some(panel) = state.panels.iter_mut().find(|p| p.output == output) {
                    let can = supported != 0;
                    let warm = active != 0;
                    if panel.hdr.night_light != can || panel.hdr.warming != warm {
                        tracing::info!(
                            display = %panel.name,
                            supported = can,
                            warming = warm,
                            "the night light on a display"
                        );
                        panel.hdr.night_light = can;
                        panel.hdr.warming = warm;
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
            lxb_shell_v1::Event::OutputTransform { output, transform } => {
                // Anything that is not one of the eight is a compositor
                // speaking a later version of this protocol than the shell.
                // Left as it was rather than guessed at: a page that marked
                // the wrong row would be worse than one that marks none.
                let reported = transform
                    .into_result()
                    .ok()
                    .and_then(|transform| settings::Orientation::from_code(transform as u32));
                if let Some(panel) = state.panels.iter_mut().find(|p| p.output == output) {
                    if reported.is_some() && panel.turned != reported {
                        tracing::info!(
                            display = %panel.name,
                            turned = ?reported,
                            "which way up a display is drawn"
                        );
                        panel.turned = reported;
                        state.refresh_display_turns();
                    }
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
                let Some(index) = state.panels.iter().position(|p| p.output == output) else {
                    return;
                };
                let panel = &mut state.panels[index];
                let windows = std::mem::take(&mut panel.pending_windows);
                if panel.windows == windows {
                    return;
                }
                tracing::debug!(
                    display = %panel.name,
                    count = windows.len(),
                    "window list changed on a display"
                );
                // Which windows this display has lost, read before the new list
                // replaces the old one. This event is the only thing that says a
                // window has gone: there is no "closed" of its own, and the
                // compositor is free to hand the id out again afterwards.
                let gone: Vec<WindowCard> = panel
                    .windows
                    .iter()
                    .filter(|window| !windows.iter().any(|now| now.id == window.id))
                    .cloned()
                    .collect();
                let (width, height) = (panel.width, panel.height);
                panel.windows = windows;
                // What is being let go of here is a claim about an id.
                state.forget_closed_windows();
                if walked_out(&gone, width, height, &mut state.ended_by_the_shell) {
                    state.an_application_ended_by_itself(index);
                }
                state.needs_redraw = true;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod scenery_tests {
    use super::*;

    /// A cursor arriving on a game fades its picture up out of the wallpaper,
    /// and leaving the library fades it back down. Nothing is on screen at
    /// either end but the shell's own background, which is what a display that
    /// has never seen a Steam library shows.
    #[test]
    fn a_display_fades_into_a_game_and_back_out_of_one() {
        let mut scenery = Scenery::default();
        assert!(!scenery.moving(), "nothing to fade to or from");

        scenery.look_at(Some(7));
        assert_eq!((scenery.from, scenery.to), (None, Some(7)));
        assert!(scenery.moving());

        scenery.advance(0.5);
        assert!((scenery.across - 0.5).abs() < 0.001);
        scenery.advance(0.9);
        assert_eq!(scenery.across, 1.0, "a fade settles exactly on its end");
        assert!(!scenery.moving());

        scenery.look_at(None);
        assert_eq!(
            (scenery.from, scenery.to),
            (Some(7), None),
            "the game becomes the picture being left"
        );
        scenery.advance(2.0);
        assert_eq!(scenery.from, None, "and is let go of once it has gone");
        assert!(scenery.wanted().next().is_none(), "so is its layer");
    }

    /// Moving on to a second game crossfades between the two rather than
    /// through the wallpaper: what the user is doing is comparing two games,
    /// and a flash of purple between them is the shell interrupting that.
    #[test]
    fn one_game_hands_the_display_to_the_next() {
        let mut scenery = Scenery::default();
        scenery.look_at(Some(7));
        scenery.advance(1.0);

        scenery.look_at(Some(9));
        assert_eq!((scenery.from, scenery.to), (Some(7), Some(9)));
        assert_eq!(scenery.across, 0.0);
        assert_eq!(scenery.wanted().collect::<Vec<_>>(), vec![7, 9]);
    }

    /// A cursor moved and moved straight back runs the same fade the other
    /// way, from wherever it had got to. Starting again would take the
    /// picture that is nearly gone back to full strength and then fade it out
    /// a second time.
    #[test]
    fn turning_back_reverses_the_fade_it_is_in() {
        let mut scenery = Scenery::default();
        scenery.look_at(Some(7));
        scenery.advance(1.0);
        scenery.look_at(Some(9));
        scenery.advance(0.25);

        scenery.look_at(Some(7));
        assert_eq!((scenery.from, scenery.to), (Some(9), Some(7)));
        assert!(
            (scenery.across - 0.75).abs() < 0.001,
            "three quarters of the way back to it, not none: {}",
            scenery.across
        );
    }

    /// Interrupted anywhere else, what is dropped is whichever picture was
    /// faintest — so however fast somebody scrolls, the amount of picture on
    /// screen never jumps.
    #[test]
    fn an_interrupted_fade_drops_the_faintest_of_the_two() {
        // Past halfway the newcomer is mostly on screen, so it is what the
        // next fade leaves behind.
        let mut late = Scenery::default();
        late.look_at(Some(7));
        late.advance(1.0);
        late.look_at(Some(9));
        late.advance(0.8);
        late.look_at(Some(11));
        assert_eq!((late.from, late.to), (Some(9), Some(11)));
        assert_eq!(late.across, 0.0);

        // Before halfway it is still the old one that is mostly on screen, so
        // that stays and the newcomer takes over the fade where it stood.
        let mut early = Scenery::default();
        early.look_at(Some(7));
        early.advance(1.0);
        early.look_at(Some(9));
        early.advance(0.2);
        early.look_at(Some(11));
        assert_eq!((early.from, early.to), (Some(7), Some(11)));
        assert!((early.across - 0.2).abs() < 0.001);
    }

    /// A library arriving is drawn as it stands: the games on the disk in
    /// colour, the rest grey, with nothing fading anywhere. A shelf that faded
    /// up out of grey on the frame it appeared would be announcing something
    /// that has not happened.
    #[test]
    fn a_library_arrives_already_answered() {
        let mut drained = Drained::default();
        drained.told([(7, true), (9, false)].into_iter());

        assert_eq!(drained.of(7, true), 0.0, "on the disk, so in colour");
        assert_eq!(drained.of(9, false), 1.0, "not on it, so grey");
        assert!(!drained.moving(), "and neither of them is going anywhere");
    }

    /// The colour comes back over a moment rather than between two frames,
    /// which is the whole of what the user sees when a download finishes.
    #[test]
    fn a_finished_download_takes_its_colour_back_gradually() {
        let mut drained = Drained::default();
        drained.told([(9, false)].into_iter());

        drained.told([(9, true)].into_iter());
        assert_eq!(drained.of(9, true), 1.0, "still grey on the frame it lands");
        assert!(drained.moving());

        drained.advance(0.5);
        let half = drained.of(9, true);
        assert!(half > 0.0 && half < 1.0, "somewhere in between: {half}");
        assert!(drained.moving());

        drained.advance(0.5);
        assert_eq!(drained.of(9, true), 0.0, "and lands exactly in colour");
        assert!(!drained.moving(), "so the displays can stop drawing");
    }

    /// Removing one goes the same way round, and a fade turned back halfway
    /// carries on from where the cover is: a game whose removal failed must
    /// not flash to full grey before coming back.
    #[test]
    fn a_cover_turned_back_halfway_carries_on_from_where_it_is() {
        let mut drained = Drained::default();
        drained.told([(7, true)].into_iter());

        drained.told([(7, false)].into_iter());
        drained.advance(0.5);
        let going = drained.of(7, false);

        drained.told([(7, true)].into_iter());
        assert_eq!(
            drained.of(7, true),
            going,
            "the frame it turns round on looks exactly like the one before it"
        );
        drained.advance(0.5);
        assert_eq!(drained.of(7, true), 0.0);
    }

    /// A game the shell has not been told about is drawn from what its own row
    /// says. There is nothing to fade from, and a cover that started grey and
    /// brightened would say the game had just been installed.
    #[test]
    fn a_game_nobody_has_spoken_of_is_drawn_as_its_row_stands() {
        let drained = Drained::default();
        assert_eq!(drained.of(7, true), 0.0);
        assert_eq!(drained.of(9, false), 1.0);
    }

    /// A game that has left the library is forgotten, so a shell left running
    /// for a week does not hold a fade for every title it has ever seen.
    #[test]
    fn a_game_that_leaves_the_library_is_let_go_of() {
        let mut drained = Drained::default();
        drained.told([(7, true), (9, false)].into_iter());
        drained.told([(7, true)].into_iter());
        assert_eq!(drained.0.len(), 1);
        assert!(!drained.0.contains_key(&9));
    }
}

#[cfg(test)]
mod flight_tests {
    use super::*;

    /// A debug script's deadline can pass while the wallpaper is all that is
    /// visible, but its action stays in the queue and runs as soon as the bar
    /// has arrived far enough to own controls.
    #[test]
    fn a_scheduled_action_is_retained_until_startup_is_ready() {
        assert!(!debug_action_is_due(false, 4.0, Some(1.0)));
        assert!(debug_action_is_due(true, 4.0, Some(1.0)));
        assert!(!debug_action_is_due(true, 0.5, Some(1.0)));
        assert!(!debug_action_is_due(true, 4.0, None));
    }

    /// Icon discovery may finish on either side of the first presented
    /// wallpaper frame. Neither ordering is allowed to charge hidden startup
    /// time to the bar's arrival: it starts only after both have happened.
    #[test]
    fn the_startup_arrival_waits_for_icons_and_a_presented_frame() {
        assert!(!startup_arrival_can_advance(false, false));
        assert!(!startup_arrival_can_advance(true, false));
        assert!(!startup_arrival_can_advance(false, true));
        assert!(startup_arrival_can_advance(true, true));
    }

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

    /// The wheel moves the selection in whole rows, and what is left over is
    /// carried rather than dropped: a touchpad reports a stream of fractions of
    /// a notch, and a shell that rounded each of them to nothing would never
    /// move at all under a slow two-finger drag.
    #[test]
    fn the_wheel_moves_in_whole_rows_and_keeps_the_remainder() {
        assert_eq!(scroll_steps(1.0), (1, 0.0));
        assert_eq!(scroll_steps(-1.0), (-1, 0.0));
        assert_eq!(scroll_steps(3.0), (3, 0.0));

        // Less than a notch moves nothing and is all remainder.
        let (steps, left) = scroll_steps(0.4);
        assert_eq!(steps, 0);
        assert!((left - 0.4).abs() < 1e-9);

        // And the remainder is what carries the next one over the line: four
        // tenths and then eight is one row, with two tenths still to come.
        let (steps, left) = scroll_steps(left + 0.8);
        assert_eq!(steps, 1);
        assert!((left - 0.2).abs() < 1e-9, "{left}");
    }

    /// A wheel says how many notches it turned and is taken at its word, so one
    /// click of it is one row whatever a compositor thinks a notch is worth in
    /// pixels. Only a device with nothing to count — a touchpad — is measured.
    #[test]
    fn a_wheel_is_counted_in_notches_and_a_touchpad_is_measured() {
        let axis = |absolute: f64, discrete: i32, value120: i32| AxisScroll {
            absolute,
            discrete,
            value120,
            relative_direction: None,
            stop: false,
        };

        // A high-resolution wheel, which is what a modern compositor sends.
        assert_eq!(scroll_notches(&axis(10.0, 1, 120)), 1.0);
        assert_eq!(scroll_notches(&axis(-10.0, -1, -120)), -1.0);
        // Part of a notch from a free-spinning wheel is part of a row.
        assert_eq!(scroll_notches(&axis(2.5, 0, 30)), 0.25);
        // An older compositor, counting whole notches only.
        assert_eq!(scroll_notches(&axis(10.0, 2, 0)), 2.0);
        // And a touchpad, which reports a distance and nothing else.
        assert_eq!(scroll_notches(&axis(SCROLL_STEP, 0, 0)), 1.0);
        assert_eq!(scroll_notches(&axis(0.0, 0, 0)), 0.0);
    }

    /// A rectangle holds its own top-left corner and not the next one's, so two
    /// rows laid edge to edge never both answer for the pixel between them.
    #[test]
    fn a_point_belongs_to_one_rectangle() {
        let rect = [10.0, 20.0, 30.0, 40.0];
        assert!(within(rect, 10.0, 20.0));
        assert!(within(rect, 39.9, 59.9));
        assert!(!within(rect, 40.0, 40.0));
        assert!(!within(rect, 20.0, 60.0));
        assert!(!within(rect, 9.9, 40.0));
    }

    /// The cursor says whether there is anything under it, and the screen
    /// around a menu counts: pressing past a menu dismisses it, which is one of
    /// the things a press can do.
    #[test]
    fn the_cursor_says_where_a_press_would_do_something() {
        assert!(Spot::Key(0, 0).is_actionable());
        assert!(Spot::OutsideMenu.is_actionable());
        assert!(Spot::Bar(ui::BarSpot::Category(0)).is_actionable());
        assert!(Spot::Entry {
            item: Item::Resume,
            level: None
        }
        .is_actionable());
        assert!(!Spot::Nothing.is_actionable());
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

    /// A launch is the shell already answering the last press, and the menu
    /// does not open over its own answer.
    #[test]
    fn the_menu_does_not_come_up_over_a_launch() {
        const NO: bool = false;
        const YES: bool = true;

        // The ordinary case, both ways round: nothing on its way, the press
        // opens the menu and the next one closes it again.
        assert!(guide_answers(NO, NO));
        assert!(guide_answers(NO, YES));

        // Something on its way on this display: the press is spent.
        assert!(!guide_answers(YES, NO));
    }

    /// The button that gets the user out of everything must not be the one
    /// that leaves them somewhere.
    ///
    /// Nothing the user does can raise the menu over a launch, but a portal's
    /// question and an authorisation both can — they are the system asking,
    /// they can arrive at any moment, and they go up through `open_guide`
    /// without passing the rule. If the same rule then refused the press that
    /// closes the menu, the user would be shut in with it.
    #[test]
    fn a_menu_raised_over_a_launch_can_still_be_closed() {
        assert!(guide_answers(true, true));
    }

    /// One display's launch is not the other display's business.
    ///
    /// The splash is drawn on the display the application was started from and
    /// nowhere else, so that is the only screen with the shell's answer
    /// already on it. Asking the session-wide question here would leave a
    /// second screen's menu dead for a minute over a game that is not coming
    /// up on it.
    #[test]
    fn a_launch_next_door_does_not_hold_this_screens_menu_shut() {
        assert!(guide_answers(false, false));
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

    /// An application that ends on its own hands the display back to the start
    /// screen, and the shell says so by coming forward on to it.
    #[test]
    fn an_application_that_quits_by_itself_hands_the_display_back() {
        let mut shells = HashSet::new();
        let game = WindowCard {
            id: 7,
            ..window(1920, 1080)
        };
        assert!(walked_out(&[game], 1920, 1080, &mut shells));
    }

    /// But the guide's own Close does not. The user put that application away
    /// deliberately, from a menu they are still standing in, and the deck
    /// closing up around the card that left is that press's answer.
    #[test]
    fn a_card_closed_from_the_guide_is_not_an_application_walking_out() {
        let mut shells = HashSet::from([7]);
        let killed = WindowCard {
            id: 7,
            ..window(1920, 1080)
        };
        assert!(!walked_out(
            std::slice::from_ref(&killed),
            1920,
            1080,
            &mut shells
        ));
        assert!(
            shells.is_empty(),
            "the claim is spent on the window it named"
        );

        // And spent is spent: the same application quitting on its own later —
        // a compositor is free to hand the number out again — is a surprise
        // like any other.
        assert!(walked_out(&[killed], 1920, 1080, &mut shells));
    }

    /// A window that was not filling the display hands the bar nothing: it was
    /// already on screen behind it, or a game still running is.
    #[test]
    fn a_small_window_closing_is_not_a_display_being_handed_over() {
        let mut shells = HashSet::new();
        let dialog = WindowCard {
            id: 3,
            ..window(600, 400)
        };
        assert!(!walked_out(&[dialog], 1920, 1080, &mut shells));
    }

    /// Several windows can go at once — an application that ends takes its
    /// dialogs with it, and killing one window of a process can end the rest.
    /// One of them being the shell's doing does not make the others its doing.
    #[test]
    fn one_shell_kill_does_not_account_for_every_window_that_went_with_it() {
        let mut shells = HashSet::from([7]);
        let killed = WindowCard {
            id: 7,
            ..window(600, 400)
        };
        let went_with_it = WindowCard {
            id: 8,
            ..window(1920, 1080)
        };
        assert!(walked_out(&[killed, went_with_it], 1920, 1080, &mut shells));
        assert!(shells.is_empty());
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

    /// The music belongs to a session with nothing running in it, not to
    /// whichever screen happens to hold control.
    ///
    /// The bug it was reported as: an application on the first display and
    /// Start on the second, and moving to the second started the music up
    /// behind the application. Both screens are the user's, and one of them has
    /// a program on it.
    #[test]
    fn music_belongs_to_a_session_with_nothing_open_in_it() {
        // Every screen showing Start, and nothing being handed over: the one
        // session the music is for. The guide raised over it is still that
        // session — see [`xmb_music_wanted`].
        assert!(xmb_music_wanted(true, false, false));

        // One application open anywhere ends it, whichever display it is on and
        // whatever the display holding control is showing.
        assert!(!xmb_music_wanted(true, true, false));

        // And a handoff wins even while the screen still contains Start's
        // pixels: the launch splash and the window flying back are both the
        // moment before an application has the display.
        assert!(!xmb_music_wanted(true, false, true));

        // No output means there is no Start screen to hear it. Hotplug makes
        // this a false-to-true edge, so the track begins at sample zero.
        assert!(!xmb_music_wanted(false, false, false));
    }
}

#[cfg(test)]
mod input_tests {
    use super::*;

    /// Left and Back reach the same path exit through different input routes,
    /// while a pointer can cross several breadcrumb rows in one press. All of
    /// them are one user action and therefore one back sound.
    #[test]
    fn leaving_a_path_has_one_distinct_piece_of_feedback() {
        assert_eq!(cursor_feedback(1, 0), CursorFeedback::Back);
        assert_eq!(cursor_feedback(3, 0), CursorFeedback::Back);

        // Everything else that landed is the ordinary step, whether it was a
        // direction along a column, a direction into one, or a click that put
        // the selection on a row outright.
        assert_eq!(cursor_feedback(1, 1), CursorFeedback::Step);
        assert_eq!(cursor_feedback(1, 2), CursorFeedback::Step);
        assert_eq!(cursor_feedback(0, 0), CursorFeedback::Step);
    }

    /// Every row of the guide that does anything answers in the guide's own
    /// voice — the rows that hand an application the display included.
    ///
    /// Resume with something running behind the overlay is the row this is
    /// about. It used to take `app-launch.ogg`, on the grounds that being
    /// handed the screen is one event however it was asked for; the user's rule
    /// is the other way round, and it is about the screens rather than about
    /// the applications. That clip belongs to Start, where a press *starts*
    /// something. The guide only ever goes back to what is already up.
    #[test]
    fn every_guide_row_that_acts_answers_in_the_guides_own_voice() {
        for item in [
            Item::Resume,
            Item::Close,
            Item::Mixer,
            Item::DoNotDisturb,
            Item::Notifications,
            Item::Dashboard,
            Item::Power,
            Item::Volume,
        ] {
            assert_eq!(
                chosen_feedback(item, true),
                ChosenFeedback::Kept,
                "{item:?} is answered by the screen it was pressed on"
            );
        }

        // The stick switch with an application to attach itself to is a
        // switch; with none it is drawn and inert, and a control that did
        // nothing must not say it did. The brightness bar has no press at all.
        assert_eq!(chosen_feedback(Item::Pointer, true), ChosenFeedback::Kept);
        assert_eq!(
            chosen_feedback(Item::Pointer, false),
            ChosenFeedback::Silent
        );
        assert_eq!(
            chosen_feedback(Item::Brightness, true),
            ChosenFeedback::Silent
        );
    }

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

    fn window(id: u32, app_id: &str, title: &str) -> WindowCard {
        WindowCard {
            id,
            title: title.to_string(),
            app_id: app_id.to_string(),
            width: 1920,
            height: 1080,
        }
    }

    /// A game Valve's client started is named by its own window, and nothing
    /// else is.
    ///
    /// The class is the part that cannot be trusted here: a Proton title
    /// announces whatever its binary was called, so the shell was offering to
    /// "Close X86_64" beside a card captioned Celeste. That reasoning applies
    /// to these windows and to no others — a browser's title is the page it is
    /// showing, and a button that ends Firefox must not be named after a video.
    #[test]
    fn a_game_started_through_steam_is_named_by_its_window() {
        let game = window(7, "x86_64", "Celeste");
        assert_eq!(window_name(&game, None, true), "Celeste");
        // The same window, if the shell had not seen it start: back to the
        // class, which is the answer that made this worth changing.
        assert_eq!(window_name(&game, None, false), "X86_64");

        // Nothing else takes its title, however uninformative the class is.
        let browser = window(9, "firefox", "(7) Something on the internet — YouTube");
        assert_eq!(window_name(&browser, Some("Firefox"), false), "Firefox");
        assert_eq!(
            window_name(&browser, None, false),
            "Firefox",
            "an unknown class is still a name somebody chose"
        );

        // An installed name is the better answer for anything that is not one
        // of these, and it still wins.
        let native = window(11, "org.example.Game", "Level 3 — 60fps");
        assert_eq!(
            window_name(&native, Some("Example Game"), false),
            "Example Game"
        );
        assert_eq!(
            window_name(&native, None, false),
            "Game",
            "the tail of a reverse-DNS class, not the level it is on"
        );

        // And a game whose window says nothing keeps the old chain rather than
        // being called the empty string.
        let unnamed = window(13, "x86_64", "   ");
        assert_eq!(window_name(&unnamed, None, true), "X86_64");
        let nameless = window(15, "", "");
        assert_eq!(window_name(&nameless, None, true), "");
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

    /// The smallest step worth asserting either side of a deadline.
    const MOMENT: Duration = Duration::from_millis(1);

    /// A held arrow walks the bar, exactly as a held D-pad does.
    #[test]
    fn holding_a_direction_steps_at_the_pads_rate() {
        let start = Instant::now();
        let mut held = HeldKey::pressed(Keysym::Down, start);

        // Nothing at all until the initial delay is up: a tap is one row.
        assert!(!held.due(start));
        assert!(!held.due(start + controller::INITIAL_REPEAT_DELAY - MOMENT));
        assert!(held.due(start + controller::INITIAL_REPEAT_DELAY));

        // And a step every interval from then on.
        let second = start + controller::INITIAL_REPEAT_DELAY + controller::REPEAT_INTERVAL;
        assert!(!held.due(second - MOMENT));
        assert!(held.due(second));

        // A pass that ran long steps once and books the next from where it
        // finished, rather than paying out every step it slept through.
        let late = second + controller::REPEAT_INTERVAL * 10;
        assert!(held.due(late));
        assert!(!held.due(late + MOMENT));
        assert!(held.due(late + controller::REPEAT_INTERVAL));
    }

    /// Only a direction is a rate. Everything else acts once however long it
    /// is held for.
    #[test]
    fn only_the_directions_repeat() {
        for keysym in [Keysym::Left, Keysym::Right, Keysym::Up, Keysym::Down] {
            assert!(key_repeats(keysym, false), "{keysym:?} should walk the bar");
        }
        // Launch, Back, Guide, Menu, the screen keys: one press, one answer.
        for keysym in [
            Keysym::Return,
            Keysym::space,
            Keysym::Escape,
            Keysym::BackSpace,
            Keysym::Home,
            Keysym::Menu,
            Keysym::F10,
            Keysym::Tab,
            Keysym::ISO_Left_Tab,
        ] {
            assert!(
                !key_repeats(keysym, false),
                "{keysym:?} should act once however long it is held"
            );
        }
    }

    /// A password field is a text field, and a held key fills or empties it.
    #[test]
    fn a_field_being_typed_into_repeats_its_letters() {
        for keysym in [Keysym::a, Keysym::Z, Keysym::_9, Keysym::BackSpace] {
            assert!(key_repeats(keysym, true), "{keysym:?} should type on");
        }
        // The keys that finish the field still finish it once, and the
        // directions the field ignores have nothing to repeat.
        for keysym in [Keysym::Return, Keysym::Escape, Keysym::Down, Keysym::Tab] {
            assert!(
                !key_repeats(keysym, true),
                "{keysym:?} should not repeat into a password"
            );
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

/// The menu over one of the user's own files: which rows it has, in what order,
/// and which of them can be pressed from where.
///
/// How a row says when something arrived.
///
/// The question a reader is asking of that line is *did I miss this, or has it
/// only just happened*, and the answer is coarse on purpose: every step here is
/// one where the extra precision would tell them nothing they would act on.
#[cfg(test)]
mod notification_age_tests {
    use super::*;

    fn ago(seconds: u64) -> String {
        age_of(
            Instant::now()
                .checked_sub(Duration::from_secs(seconds))
                .expect("the clock has run for a minute"),
        )
    }

    #[test]
    fn an_age_is_as_coarse_as_the_question_it_answers() {
        // Anything inside a minute is "now". A list somebody has just opened
        // must not count seconds at them.
        assert_eq!(ago(0), "now");
        assert_eq!(ago(59), "now");

        assert_eq!(ago(60), "1m");
        assert_eq!(ago(59 * 60 + 59), "59m");
        assert_eq!(ago(60 * 60), "1h");
        assert_eq!(ago(23 * 3600 + 3599), "23h");
        assert_eq!(ago(24 * 3600), "1d");

        // A week-old announcement is a list nobody has cleared rather than a
        // record anyone is consulting, so it never becomes a date.
        assert_eq!(ago(9 * 24 * 3600), "9d");
    }

    /// When it arrived, what it is called and what it said are three runs, and
    /// never one sentence with a time on the front of it.
    ///
    /// They shared a line until the user asked for this — `now  ·  Would you
    /// like to install updates now?` — where the reader had to find the
    /// separator before either half meant anything.
    #[test]
    fn a_row_says_when_above_what_rather_than_beside_it() {
        let mut held = notify::Notification::heard(1, "System Update Available");
        held.body = "Version 3.1 is waiting to be installed".to_string();
        let rows = notification_rows(std::slice::from_ref(&held));
        let row = &rows[1];

        assert_eq!(row.stamp.as_deref(), Some("now"));
        assert_eq!(row.label, "System Update Available");
        assert_eq!(
            row.detail.as_deref(),
            Some("Version 3.1 is waiting to be installed")
        );
        // Nothing is run together with anything else. The separator that used
        // to join the first two is what this is watching for.
        for run in [
            row.stamp.as_deref(),
            Some(row.label.as_str()),
            row.detail.as_deref(),
        ] {
            let run = run.unwrap();
            assert!(!run.contains('·'), "{run:?} carries a separator");
        }

        // An announcement with nothing more to say is two runs and not a third
        // empty one — the time and the summary, which is the whole of what
        // arrived.
        let bare = notification_rows(&[notify::Notification::heard(2, "Disk ejected")]);
        assert_eq!(bare[1].stamp.as_deref(), Some("now"));
        assert_eq!(bare[1].detail, None);
    }

    /// Clearing the list is the panel's first row, and the announcements come
    /// under it. It is where the highlight can reach in one press of Up from
    /// where it starts, and — unlike the bottom of a list that scrolls — it is
    /// on the panel the moment the panel opens.
    #[test]
    fn clearing_the_list_is_the_row_above_it() {
        let held = [
            notify::Notification::heard(3, "Newest"),
            notify::Notification::heard(2, "Older"),
            notify::Notification::heard(1, "Oldest"),
        ];
        let rows = notification_rows(&held);
        let labels: Vec<&str> = rows.iter().map(|row| row.label.as_str()).collect();
        assert_eq!(labels, ["Clear All", "Newest", "Older", "Oldest"]);
        assert_eq!(rows[0].command, menu::Command::DismissNotifications);

        // On the other side of a separator from the list, because it is the one
        // row that is not about a single announcement.
        assert_eq!(rows[0].group, 0);
        assert!(rows[1..].iter().all(|row| row.group == 1));

        // And it holds the panel up. The answer to pressing it is an empty
        // list, which a panel that folded away would take with it.
        assert!(rows[0].holds);
    }

    /// With nothing to read there is nothing to clear, and the panel says the
    /// one thing it has to say.
    #[test]
    fn an_empty_list_offers_no_way_to_empty_it() {
        let rows = notification_rows(&[]);
        let labels: Vec<&str> = rows.iter().map(|row| row.label.as_str()).collect();
        assert_eq!(labels, ["Nothing to read"]);
    }
}

/// The rows are the whole of what this menu *is*, and they are the part a later
/// change is most likely to disturb without meaning to — so they are asserted
/// against by name rather than left to be noticed on screen.
#[cfg(test)]
mod file_menu_tests {
    use super::*;

    fn commands(rows: &[menu::Entry]) -> Vec<menu::Command> {
        rows.iter().map(|row| row.command).collect()
    }

    fn labels(rows: &[menu::Entry]) -> Vec<&str> {
        rows.iter().map(|row| row.label.as_str()).collect()
    }

    /// Five rows in two bands, and the rule falls above Sort: everything over
    /// it acts on the file, and neither of the two under it does.
    #[test]
    fn the_file_menu_is_five_rows_with_sort_and_cancel_below_the_rule() {
        let rows = media_rows(true, true);
        assert_eq!(
            labels(&rows),
            ["Open", "Open with", "Delete", "Sort", "Cancel"]
        );
        assert_eq!(
            commands(&rows),
            [
                menu::Command::Open,
                menu::Command::OpenWith,
                menu::Command::Delete,
                menu::Command::Sort,
                menu::Command::Dismiss,
            ]
        );

        let bands: Vec<u8> = rows.iter().map(|row| row.group).collect();
        assert_eq!(bands, [0, 0, 0, 1, 1], "one rule, and it falls above Sort");
        assert!(rows.iter().all(|row| row.enabled));
        // Delete is the one row here the highlight arrives on warm: it asks
        // rather than deletes, but it is the way to losing the file, and the
        // other four are not.
        let grave: Vec<bool> = rows.iter().map(|row| row.grave).collect();
        assert_eq!(grave, [false, false, true, false, false]);
    }

    /// The two rows that can be unavailable are drawn greyed rather than left
    /// out, so the panel is the same shape wherever it is raised.
    #[test]
    fn a_row_that_cannot_be_taken_here_is_still_drawn() {
        let rows = media_rows(false, false);
        assert_eq!(labels(&rows).len(), 5);
        let enabled: Vec<bool> = rows.iter().map(|row| row.enabled).collect();
        assert_eq!(enabled, [true, false, false, true, true]);
    }

    /// The Open with list names every application it was given, ticks the one
    /// that opens the type now, and ends in its own way out. Its rows hold the
    /// panel, because setting which program opens a kind of file is not a thing
    /// anybody does once and leaves.
    #[test]
    fn the_open_with_list_ticks_the_one_open_would_have_used() {
        let offering = |names: &[&str]| -> Vec<media::Handler> {
            names
                .iter()
                .map(|name| media::Handler {
                    name: name.to_string(),
                    icon: Some(format!("{name}-icon")),
                    id: format!("{name}.desktop"),
                    mime: "audio/flac",
                })
                .collect()
        };
        let offered = offering(&["mpv", "VLC", "Audacity"]);
        let rows = open_with_rows(&offered, 0);

        assert_eq!(labels(&rows), ["mpv", "VLC", "Audacity", "Cancel"]);
        assert_eq!(
            commands(&rows)[..3],
            [
                menu::Command::OpenWithHandler(0),
                menu::Command::OpenWithHandler(1),
                menu::Command::OpenWithHandler(2),
            ]
        );
        assert_eq!(rows[0].glyph, Some(icons::CHOSEN));
        assert!(rows[1].glyph.is_none() && rows[2].glyph.is_none());
        assert_eq!(rows[1].icon.as_deref(), Some("VLC-icon"));
        // Its own band, like every other way off a panel.
        assert_eq!(rows[3].group, 1);
        assert_eq!(rows[3].command, menu::Command::Dismiss);

        // Choosing one moves the tick and nothing else: the rows stay in the
        // order the panel opened with, so the row under the next press is the
        // row the user was already looking at.
        let after = open_with_rows(&offered, 2);
        assert_eq!(labels(&after), labels(&rows));
        assert_eq!(after[2].glyph, Some(icons::CHOSEN));
        assert!(after[0].glyph.is_none());

        // And every application holds the panel; only the way out lets go.
        assert!(rows[..3].iter().all(|row| row.holds), "{rows:?}");
        assert!(!rows[3].holds);
    }

    /// Nine orders and a way out, the one in force ticked, and the two the disk
    /// cannot answer for greyed.
    #[test]
    fn the_sort_list_offers_every_order_and_marks_the_one_in_force() {
        // A shelf the filesystem could tell nothing about the dates of.
        let undated = media::Orders::default();

        let rows = sort_rows(media::Sort::LargestFirst, undated);
        assert_eq!(rows.len(), media::SORTS.len() + 1);
        assert_eq!(
            commands(&rows).last(),
            Some(&menu::Command::Dismiss),
            "the way out is last"
        );
        let ticked: Vec<&str> = rows
            .iter()
            .filter(|row| row.glyph == Some(icons::CHOSEN))
            .map(|row| row.label.as_str())
            .collect();
        assert_eq!(ticked, [media::Sort::LargestFirst.label()]);

        // Knowing no dates, the four date orders are offered greyed — and the
        // rest are not.
        for row in &rows {
            let by_date = matches!(
                row.command,
                menu::Command::SortBy(
                    media::Sort::NewestFirst
                        | media::Sort::OldestFirst
                        | media::Sort::LastChangedFirst
                        | media::Sort::LongestUntouchedFirst
                )
            );
            assert_eq!(row.enabled, !by_date, "{}", row.label);
        }

        // And a shelf that knows when its files were last written can be put in
        // both of the Modified orders — which every filesystem can answer for.
        {
            let rows = sort_rows(
                media::Sort::NameAscending,
                media::Orders {
                    created: false,
                    modified: true,
                },
            );
            let modified: Vec<bool> = rows
                .iter()
                .filter(|row| {
                    matches!(
                        row.command,
                        menu::Command::SortBy(
                            media::Sort::LastChangedFirst | media::Sort::LongestUntouchedFirst
                        )
                    )
                })
                .map(|row| row.enabled)
                .collect();
            assert_eq!(modified, [true, true]);
        }
    }
}

#[cfg(test)]
mod authentication_tests {
    use super::*;

    fn request(message: &str, yourself: bool) -> polkit::Request {
        polkit::Request {
            cookie: "1-cookie".to_string(),
            message: message.to_string(),
            action_id: "org.example.action".to_string(),
            user: "root".to_string(),
            yourself,
        }
    }

    /// The one rule about this panel: what is typed into it never reaches the
    /// layout. The field is a count of characters, and the password is not in
    /// any line of it at any length.
    #[test]
    fn the_field_is_a_count_and_the_panel_never_holds_the_password() {
        for typed in [0, 1, 8, 400] {
            let lines = authentication_lines(
                "Authentication is required to install software",
                "Enter your password to allow this.",
                typed,
            );
            let secrets: Vec<&dialog::Line> = lines
                .iter()
                .filter(|line| matches!(line, dialog::Line::Secret { .. }))
                .collect();
            assert_eq!(secrets.len(), 1, "one field, always");
            assert_eq!(secrets[0], &dialog::Line::Secret { typed });

            // Nothing else on the panel says anything about it — not even how
            // long it is.
            for line in &lines {
                if let dialog::Line::Heading(text) | dialog::Line::Note(text) = line {
                    assert!(
                        !text.contains(&typed.to_string()) || typed == 0,
                        "{text} is counting the password out loud"
                    );
                }
            }
        }
    }

    /// polkit's message is written by whoever wrote the policy file, and the
    /// panel gives a note one line. It is broken between words rather than cut
    /// mid-word by the drawing.
    #[test]
    fn a_long_message_is_broken_between_words_rather_than_cut() {
        let message = "Authentication is required to install untrusted software \
                       from a repository nobody has heard of";
        let lines = authentication_lines(message, "Enter your password.", 0);
        let notes: Vec<&str> = lines
            .iter()
            .filter_map(|line| match line {
                dialog::Line::Note(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        // The message's lines, and the instruction under them.
        assert!(notes.len() > 2, "{notes:?} did not wrap");
        assert!(notes
            .iter()
            .all(|note| note.chars().count() <= MESSAGE_WIDTH + 1));
        // Every word survives, in order, with nothing invented.
        let rejoined = notes[..notes.len() - 1].join(" ");
        let cut = rejoined.trim_end_matches('…');
        assert!(
            message
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .starts_with(cut),
            "{rejoined} is not the message"
        );
        assert_eq!(notes[notes.len() - 1], "Enter your password.");
    }

    /// A message longer than the panel says so rather than stopping mid
    /// sentence as though that were all of it.
    #[test]
    fn a_message_with_no_end_in_sight_ends_in_an_ellipsis() {
        let long = "word ".repeat(200);
        let cut = wrapped(&long, MESSAGE_WIDTH, MESSAGE_LINES);
        assert_eq!(cut.len(), MESSAGE_LINES);
        assert!(cut.last().is_some_and(|line| line.ends_with('…')));

        // And one that fits does not: an ellipsis on a complete sentence would
        // say something had been left out.
        let short = wrapped("Authentication is required", MESSAGE_WIDTH, MESSAGE_LINES);
        assert_eq!(short, vec!["Authentication is required"]);

        // A single word too long to break is left whole rather than split into
        // two halves of a word nobody can read.
        let one = "supercalifragilisticexpialidocious-and-then-some-more-of-it";
        assert_eq!(wrapped(one, MESSAGE_WIDTH, MESSAGE_LINES), vec![one]);
    }

    /// Whose password it is is the part the user needs, so PAM's "Password: "
    /// — a label for a field the panel has already drawn — does not replace
    /// the sentence that says it.
    #[test]
    fn the_usual_prompt_is_replaced_and_an_unusual_one_is_shown_as_it_came() {
        let mine = request("...", true);
        let theirs = request("...", false);
        assert_eq!(waiting_note(&mine), "Enter your password to allow this.");
        assert_eq!(waiting_note(&theirs), "Enter the password for root.");

        for usual in [
            "Password:",
            "Password: ",
            "password",
            "",
            "Password for root:",
        ] {
            assert_eq!(prompt_note(usual, &mine), waiting_note(&mine), "{usual:?}");
            assert_eq!(
                prompt_note(usual, &theirs),
                waiting_note(&theirs),
                "{usual:?}"
            );
        }

        // Anything the shell does not recognise is shown as PAM wrote it: it
        // has no idea what is being asked for and must not pretend to.
        assert_eq!(
            prompt_note("One-time code: ", &mine),
            "One-time code:",
            "a code the shell invented a sentence for is a code nobody can type"
        );
        assert_eq!(
            prompt_note("Verification code", &theirs),
            "Verification code"
        );
    }
}
