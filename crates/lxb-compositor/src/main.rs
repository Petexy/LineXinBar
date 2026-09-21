//! LineXinBar — a micro Wayland compositor with multi-display support.

mod backdrop;
mod backend;
mod blackout;
mod capture;
mod colour;
mod colour_management;
mod config;
mod cursor;
mod curtain;
mod flash;
mod focus;
mod handlers;
mod handover;
mod hdr;
mod input;
mod outputs;
mod overview;
mod pip;
mod remembered;
mod render;
mod restore;
mod scale;
mod screencopy;
mod shell_control;
mod sleep;
mod state;
mod teardown;
mod tearing;
mod text_input;
mod xwayland;

use clap::{Parser, ValueEnum};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;

use crate::config::Config;
use crate::state::LxbState;

/// What a nested window is sized at when nobody says. Matches the size
/// smithay's own winit helper picks, which is what this flag's default has
/// always claimed to be.
const DEFAULT_WINDOW_SIZE: &str = "1280x800";

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum BackendChoice {
    /// Pick automatically: nested when a session is detected, DRM otherwise.
    Auto,
    /// Nested, single window, inside a running Wayland or X11 session.
    Winit,
    /// Nested, one window per virtual output. Needs an X server (Xwayland is fine).
    X11,
    /// Native DRM/KMS on a TTY. Drives every connected display.
    Udev,
}

#[derive(Debug, Parser)]
#[command(name = "lxb", version, about, long_about = None)]
struct Cli {
    /// Which backend to run.
    #[arg(short, long, value_enum, default_value_t = BackendChoice::Auto)]
    backend: BackendChoice,

    /// Boot straight into the desktop shell, as a session.
    ///
    /// Starts `lxb-desktop` (or `general.shell` from the config) and ties the
    /// compositor's lifetime to it, so quitting the shell logs you out. This
    /// is the one flag you need on a TTY: `lxb --shell`.
    #[arg(long)]
    shell: bool,

    /// Config file. Defaults to $XDG_CONFIG_HOME/lxb/config.toml.
    #[arg(short, long)]
    config: Option<std::path::PathBuf>,

    /// Wayland socket name to bind, e.g. `wayland-9`. Auto-picked when absent.
    #[arg(short, long)]
    socket: Option<String>,

    /// Number of virtual outputs. Only meaningful with `--backend x11`.
    ///
    /// That is the one backend that can open more than one window: `winit` is
    /// a single window by construction, and `udev` drives whatever connectors
    /// the machine actually has.
    #[arg(long, default_value_t = 1)]
    outputs: usize,

    /// Size of each nested window, as `WIDTHxHEIGHT`. Defaults to `1280x800`.
    ///
    /// Only meaningful with `--backend winit` or `--backend x11`, which are
    /// windows inside somebody else's session. `udev` takes its size from the
    /// connector and has nothing to resize.
    #[arg(long, value_name = "WIDTHxHEIGHT")]
    window_size: Option<String>,

    /// Also let a program of this name drive the session, by name alone.
    ///
    /// For working on LineXinBar, and named for what it is. `lxb_shell_v1` —
    /// the protocol that hides windows, closes them, injects key and pointer
    /// events, captures the screen and ends the session — is normally offered
    /// only to the shell and the portal this compositor started itself, which
    /// it knows by the pid the kernel puts on their connections. A name is not
    /// a credential: every process of this user can copy an executable to a
    /// file called `lxb-desktop`, and in this session that includes Valve's
    /// client, every game, every Flatpak and every browser. Naming one here
    /// hands the protocol to all of them.
    ///
    /// What it is for is a shell or a portal run by hand beside the compositor,
    /// which has no pid the compositor could have learned:
    /// `lxb-portal --debug-pick` is how the file chooser is looked at.
    #[arg(long, value_name = "NAME")]
    insecure_trust_program: Vec<String>,

    /// Command to run once the compositor is up. Overrides `general.autostart`.
    #[arg(trailing_var_arg = true)]
    command: Vec<String>,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let background_handoff = state::take_background_handoff();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // Before a socket exists for anything to connect to.
    shell_control::trust_these_programs(&cli.insecure_trust_program);

    // Parsed here, before any backend exists, and so on every backend. It used
    // to be parsed inside the X11 arm alone, which made one flag behave three
    // ways: refused on `x11`, and on `winit` and `udev` accepted, unread and
    // dropped — `--window-size nonsense` started a session without a word.
    let window_size = parse_size(cli.window_size.as_deref().unwrap_or(DEFAULT_WINDOW_SIZE))?;

    // And for the same reason, before a backend is built rather than after. A
    // socket name is a single file in `XDG_RUNTIME_DIR`, so one with a `/` in
    // it cannot be created — but the failure arrived from inside smithay, as
    // "Could not write to XDG_RUNTIME_DIR", after a window had already been
    // opened and a GPU context made. The name is the thing that was wrong.
    if let Some(socket) = &cli.socket {
        if socket.is_empty() || socket.contains('/') {
            anyhow::bail!("socket name must be a plain file name like `wayland-9`, got {socket:?}");
        }
    }

    // A `--config` the user typed and a config path nobody asked for are not
    // the same thing. The default path is allowed to be absent — most sessions
    // have no config file at all, and `Config::load` answers that with the
    // defaults — but a path given on the command line is one somebody expects
    // to be read, and `general.shell` lives in it: a mistyped path under
    // `--shell` would otherwise start the stock shell and read as the
    // configured one having failed.
    let config_path = match cli.config.clone() {
        Some(path) => {
            if !path.exists() {
                anyhow::bail!("no config file at {}", path.display());
            }
            path
        }
        None => Config::default_path()
            .ok_or_else(|| anyhow::anyhow!("could not determine a config path"))?,
    };
    let mut config = Config::load(&config_path)?;
    // Before anything opens a device: what the displays were last set to has to
    // be in hand by the time the first connector is lit, or the session pays a
    // modeset — a black screen — for each setting it learns afterwards.
    config.remember_displays(remembered::load());

    if !cli.command.is_empty() {
        // Quoted back into one string rather than joined with spaces. Every
        // command this compositor starts is carried as a single string, because
        // `general.autostart` and `general.shell` are written that way by hand;
        // but the shell that invoked `lxb` has already stripped the quoting off
        // `--`'s arguments, so joining on spaces loses every boundary that held
        // a space. `lxb -- touch 'a b.txt'` made a file called `a`. See
        // [`state::shell_quote`].
        config.general.autostart = vec![state::shell_quote(&cli.command)];
    }

    // Before any backend or child exists, so the compositor, its clients and
    // XWayland all end up drawing the same pointer.
    cursor::export_cursor_environment(
        config.general.cursor_theme.as_deref(),
        config.general.cursor_size,
    );
    // And, in the same breath and for the same reason, which keyboard the
    // session is being typed on: a program that draws its own picture of one —
    // the login screen's board, a toolkit application's search keyboard — reads
    // these when it starts and has no other way to find out. See
    // [`shell_control::export_keyboard_environment`], which is where the shell
    // changing the layout mid-session goes through.
    shell_control::export_keyboard_environment(
        &config.input.keyboard_layout,
        &config.input.keyboard_variant,
    );

    let backend = resolve_backend(cli.backend);
    tracing::info!(?backend, "starting lxb");

    // A flag that cannot be honoured here says so. Both of these are real
    // settings on `x11` and meaningless elsewhere, and saying nothing is what
    // made them look broken rather than inapplicable — the session came up,
    // one window, no complaint, and nothing to read afterwards.
    if cli.outputs > 1 && backend != BackendChoice::X11 {
        tracing::warn!(
            outputs = cli.outputs,
            ?backend,
            "only the x11 backend opens more than one window; this session has one output"
        );
    }
    if cli.window_size.is_some() && backend == BackendChoice::Udev {
        tracing::warn!(
            "--window-size is for the nested backends; this session is sized by its connectors"
        );
    }

    // Settled before the backend, because the DRM backend needs it before it
    // lights a connector: the commit that establishes a display's mode is a
    // frame on that display, and it used to be a black one. See
    // [`crate::backdrop::Opening`].
    let opening = backdrop::Opening::of_a_session(cli.shell, background_handoff.as_deref());

    let mut event_loop: EventLoop<'static, LxbState> = EventLoop::try_new()?;
    let display: Display<LxbState> = Display::new()?;

    let mut state = match backend {
        BackendChoice::Udev => backend::udev::init(
            &mut event_loop,
            display,
            config,
            cli.socket.clone(),
            opening,
        )?,
        BackendChoice::X11 => backend::x11::init(
            &mut event_loop,
            display,
            config,
            cli.socket.clone(),
            cli.outputs.max(1),
            window_size,
        )?,
        _ => backend::winit::init(
            &mut event_loop,
            display,
            config,
            cli.socket.clone(),
            window_size,
        )?,
    };

    std::env::set_var("WAYLAND_DISPLAY", &state.lxb.socket_name);
    tracing::info!(socket = %state.lxb.socket_name, "wayland socket ready");

    // Before the loop takes over: a session that is asked to stop has to stop
    // tidily, or every application it put to sleep stays asleep. See
    // [`LxbState::end_the_session_on_a_signal`].
    state.end_the_session_on_a_signal();

    let autostart = state.lxb.config.general.autostart.clone();
    let shell = cli.shell.then(|| state.lxb.config.general.shell.clone());
    state.start_xwayland(autostart, shell, background_handoff);

    let result = event_loop.run(std::time::Duration::from_millis(16), &mut state, |state| {
        if !state.lxb.running {
            state.lxb.loop_signal.stop();
            return;
        }
        state.lxb.space.refresh();
        state.lxb.popups.cleanup();
        // Landed flights stop being flights, and burnt-out flashes stop being
        // flashes. Kept here rather than in the render pass, which sees this
        // state immutably and runs per display.
        state.lxb.restores.prune(std::time::Instant::now());
        state.lxb.flashes.prune(std::time::Instant::now());
        // And the same for a display that has finished coming back off the
        // black it was resting behind — see [`blackout`]. It takes the live
        // display list because a screen can be unplugged while it rests, and
        // the one that comes back must not come back dead.
        {
            let live: Vec<smithay::output::Output> = state.lxb.space.outputs().cloned().collect();
            state.lxb.blackouts.prune(&live, std::time::Instant::now());
        }
        // And a curtain that has finished coming back up stops being one,
        // which is what gives the session its input back — beside them for the
        // same reason they are here.
        state.lxb.curtain.prune(std::time::Instant::now());
        // Then the one question the render pass cannot answer: whether the
        // black is on *every* display. A frame is drawn per display, and this
        // is the pass that sees all of them.
        state.tell_the_shell_when_the_screen_is_black();
        // One place to notice that the window stack changed, rather than a
        // hook on every path that can map, unmap or retitle a window.
        state.refresh_foreground();
        // And, in the same breath, whether one of those retitlings was a window
        // becoming — or ceasing to be — the one that floats over everything.
        // See [`pip`].
        state.refresh_floating_windows();
        // And, for the same reason and in the same breath, whether each
        // application can still be seen at all — which is what decides whether
        // it goes on running. It belongs here rather than beside
        // `refresh_window_activation`, which is where the same question is
        // answered for the screen: that one is driven by focus changes and
        // layer commits, and both of those stop happening the moment an
        // application covers the shell, so the one pass that mattered — the
        // one just after the covering window was placed — was the pass that
        // never ran. See [`sleep`].
        state.refresh_application_sleep();
        // Answers to "what colour is this?" that could not be finished inside
        // the request that asked, because the event that ends one destroys the
        // object carrying it. See [`colour_management::finish_information`].
        colour_management::finish_information(state);
        let _ = state.lxb.display_handle.flush_clients();
    });

    // The first thing, before a single question about how the session ended is
    // asked: anything this compositor stopped has to be started again. Nothing
    // else can do it — `SIGSTOP` is undone by `SIGCONT` and by nothing else,
    // and the process that would send it is the one exiting here. See
    // [`sleep`].
    state.wake_every_sleeping_application("the session is ending");

    // Before anything below can return, and deliberately not behind the `?` on
    // that result: a display left in BT.2020/PQ outlives the session that asked
    // for it, and what the user gets back is a console encoded for a transfer
    // function it knows nothing about. The session ending badly is exactly when
    // they are least able to put it right by hand.
    //
    // Unless the displays are not being given back at all. Handing them to
    // another compositor is the one case where undoing this session's colour is
    // the wrong thing to do: the compositor that follows sets its own within a
    // frame of taking over, and turning HDR off in between makes the panel
    // re-sync — a black screen of the display's own making, arriving at exactly
    // the moment the rest of this is spent removing one. The night light is
    // kept across this boundary for the same reason and has been since it was
    // measured; this is that decision applied to the rest of the pipeline.
    let holding = handover::wanted();
    if !holding {
        backend::udev::restore_displays(&mut state);
    }
    // And for the same reason, on the same terms: what this session started on
    // the user's bus must not be inherited by the one they log into next.
    state.release_session_services();
    result?;

    if let Some(error) = state.lxb.fatal_error.take() {
        anyhow::bail!(error);
    }

    tracing::info!("shutting down");

    if holding {
        let displays = backend::udev::hold_displays(&state);
        tracing::info!(
            displays,
            "holding the picture for the compositor that follows"
        );
        // Deliberately not returning. Unwinding out of `main` drops this
        // process's DRM state, and destroying a framebuffer that a plane is
        // still scanning out is itself what takes the picture off the display
        // — the keeper just forked is holding the descriptor open precisely so
        // that never happens. See `handover`.
        std::process::exit(0);
    }
    Ok(())
}

/// Resolve `auto` into a concrete backend.
///
/// Running inside a session means nesting; a bare TTY means we own the
/// hardware.
fn resolve_backend(choice: BackendChoice) -> BackendChoice {
    if choice != BackendChoice::Auto {
        return choice;
    }
    if std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some() {
        BackendChoice::Winit
    } else {
        BackendChoice::Udev
    }
}

/// Parse a `WIDTHxHEIGHT` nested window size.
///
/// Bounded at both ends, because the X11 backend hands these to the X server as
/// `u16` and used to do it with an `as` cast. A number that does not fit one was
/// silently truncated while the *output mode* kept the number as written, so
/// `--window-size 70000x70000` opened a 4464px window and then told every client
/// it was 70000px — a disagreement that surfaces much later, as a client drawing
/// at a size nothing asked for. Zero is refused for the same reason: a window
/// that cannot be mapped, advertised as a mode nothing can render into.
fn parse_size(raw: &str) -> anyhow::Result<(i32, i32)> {
    let (w, h) = raw
        .split_once('x')
        .ok_or_else(|| anyhow::anyhow!("window size must look like 1280x800, got {raw:?}"))?;
    let side = |which: &str, value: &str| -> anyhow::Result<i32> {
        let value = value.trim();
        let n: i32 = value
            .parse()
            .map_err(|_| anyhow::anyhow!("window {which} must be a whole number, got {value:?}"))?;
        if !(1..=i32::from(u16::MAX)).contains(&n) {
            anyhow::bail!("window {which} must be between 1 and {}, got {n}", u16::MAX);
        }
        Ok(n)
    };
    Ok((side("width", w)?, side("height", h)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_window_size() {
        assert_eq!(parse_size("1920x1080").unwrap(), (1920, 1080));
        assert!(parse_size("1920").is_err());
    }

    /// The default has to be a value this same parser accepts, or every
    /// session that says nothing about its size fails to start.
    #[test]
    fn the_default_window_size_parses() {
        assert_eq!(parse_size(DEFAULT_WINDOW_SIZE).unwrap(), (1280, 800));
    }

    /// Both ends are refused rather than cast. The X11 backend narrows these
    /// to `u16` for the X server while advertising the number as written as an
    /// output mode, so anything outside that range used to make the window and
    /// the mode disagree instead of failing.
    #[test]
    fn refuses_a_size_no_window_could_have() {
        for raw in ["0x600", "800x0", "70000x600", "800x70000", "-5x-5"] {
            assert!(parse_size(raw).is_err(), "{raw} should be refused");
        }
        assert_eq!(parse_size("1x1").unwrap(), (1, 1));
        assert_eq!(parse_size("65535x65535").unwrap(), (65535, 65535));
    }

    /// Nonsense is refused whatever the backend, which is the whole point of
    /// parsing it before one is chosen.
    #[test]
    fn refuses_a_size_that_is_not_one() {
        for raw in ["utter nonsense", "1280x", "x800", "1280x800x600", ""] {
            assert!(parse_size(raw).is_err(), "{raw:?} should be refused");
        }
    }
}
