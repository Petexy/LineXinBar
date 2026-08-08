//! LineXinBar — a micro Wayland compositor with multi-display support.

mod backend;
mod capture;
mod config;
mod cursor;
mod focus;
mod handlers;
mod hdr;
mod input;
mod outputs;
mod overview;
mod render;
mod restore;
mod shell_control;
mod state;
mod teardown;
mod text_input;
mod xwayland;

use clap::{Parser, ValueEnum};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;

use crate::config::Config;
use crate::state::LxbState;

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

    /// Boot straight into the XMB shell, as a session.
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

    /// Number of virtual outputs for the `x11` backend.
    #[arg(long, default_value_t = 1)]
    outputs: usize,

    /// Size of each virtual output window, as `WIDTHxHEIGHT`.
    #[arg(long, default_value = "1280x800")]
    window_size: String,

    /// Command to run once the compositor is up. Overrides `general.autostart`.
    #[arg(trailing_var_arg = true)]
    command: Vec<String>,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let config_path = cli
        .config
        .clone()
        .or_else(Config::default_path)
        .ok_or_else(|| anyhow::anyhow!("could not determine a config path"))?;
    let mut config = Config::load(&config_path)?;

    if !cli.command.is_empty() {
        config.general.autostart = vec![cli.command.join(" ")];
    }

    // Before any backend or child exists, so the compositor, its clients and
    // XWayland all end up drawing the same pointer.
    cursor::export_cursor_environment(
        config.general.cursor_theme.as_deref(),
        config.general.cursor_size,
    );

    let backend = resolve_backend(cli.backend);
    tracing::info!(?backend, "starting lxb");

    let mut event_loop: EventLoop<'static, LxbState> = EventLoop::try_new()?;
    let display: Display<LxbState> = Display::new()?;

    let mut state = match backend {
        BackendChoice::Udev => {
            backend::udev::init(&mut event_loop, display, config, cli.socket.clone())?
        }
        BackendChoice::X11 => backend::x11::init(
            &mut event_loop,
            display,
            config,
            cli.socket.clone(),
            cli.outputs.max(1),
            parse_size(&cli.window_size)?,
        )?,
        _ => backend::winit::init(&mut event_loop, display, config, cli.socket.clone())?,
    };

    std::env::set_var("WAYLAND_DISPLAY", &state.lxb.socket_name);
    tracing::info!(socket = %state.lxb.socket_name, "wayland socket ready");

    let autostart = state.lxb.config.general.autostart.clone();
    let shell = cli.shell.then(|| state.lxb.config.general.shell.clone());
    state.start_xwayland(autostart, shell);

    let result = event_loop.run(std::time::Duration::from_millis(16), &mut state, |state| {
        if !state.lxb.running {
            state.lxb.loop_signal.stop();
            return;
        }
        state.lxb.space.refresh();
        state.lxb.popups.cleanup();
        // Landed flights stop being flights. Kept here rather than in the
        // render pass, which sees this state immutably and runs per display.
        state.lxb.restores.prune(std::time::Instant::now());
        // One place to notice that the window stack changed, rather than a
        // hook on every path that can map, unmap or retitle a window.
        state.refresh_foreground();
        let _ = state.lxb.display_handle.flush_clients();
    });

    // Before anything below can return, and deliberately not behind the `?` on
    // that result: a display left in BT.2020/PQ outlives the session that asked
    // for it, and what the user gets back is a console encoded for a transfer
    // function it knows nothing about. The session ending badly is exactly when
    // they are least able to put it right by hand.
    backend::udev::restore_displays(&mut state);
    result?;

    if let Some(error) = state.lxb.fatal_error.take() {
        anyhow::bail!(error);
    }

    tracing::info!("shutting down");
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

fn parse_size(raw: &str) -> anyhow::Result<(i32, i32)> {
    let (w, h) = raw
        .split_once('x')
        .ok_or_else(|| anyhow::anyhow!("window size must look like 1280x800, got {raw:?}"))?;
    Ok((w.trim().parse()?, h.trim().parse()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_window_size() {
        assert_eq!(parse_size("1920x1080").unwrap(), (1920, 1080));
        assert!(parse_size("1920").is_err());
    }
}
