//! LineXinBar's desktop portal: how an application outside the session asks
//! for a piece of it.
//!
//! Screen sharing on Wayland is deliberately not something an application can
//! simply do. It asks `xdg-desktop-portal` over D-Bus, that hands the question
//! to whichever backend the desktop installed, and the backend is the only part
//! of the chain that may read the screen — which is why every desktop ships one
//! of its own. This is LineXinBar's.
//!
//! What it does is join two protocols that know nothing about each other:
//! `wlr-screencopy`, which is how the compositor hands out frames, and
//! PipeWire, which is what OBS, a browser and Discord all read. See
//! [`cast`] for the join itself.

mod cast;
mod consent;
mod screencast;

use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "lxb-portal", version, about, long_about = None)]
struct Cli {
    /// List the displays this session has, by the names the compositor uses,
    /// and exit.
    #[arg(long)]
    list_outputs: bool,

    /// Share one display straight away, without D-Bus or a portal in front of
    /// it, and print the PipeWire node it lands on.
    ///
    /// For proving the pipeline by hand: `pw-cat`, `gst-launch-1.0
    /// pipewiresrc` or OBS can then be pointed at that node. Takes a display
    /// name, or nothing for the first one.
    #[arg(long, value_name = "DISPLAY", num_args = 0..=1, default_missing_value = "")]
    debug_cast: Option<String>,

    /// Put the pointer in the picture. Only meaningful with `--debug-cast`;
    /// over the portal it is the application's own choice.
    #[arg(long)]
    cursor: bool,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    if cli.list_outputs {
        for name in cast::outputs()? {
            println!("{name}");
        }
        return Ok(());
    }

    if let Some(output) = cli.debug_cast {
        let wanted = cast::Wanted {
            output: (!output.is_empty()).then_some(output),
            cursor: cli.cursor,
        };
        // Nothing ever sends on this: a cast started by hand ends with the
        // process.
        let (_stop, receiver) = pipewire::channel::channel::<()>();
        return cast::run(
            wanted,
            |live| {
                println!(
                    "pipewire node {} — {}x{}",
                    live.node, live.width, live.height
                );
            },
            receiver,
        );
    }

    // Otherwise this is the session's portal backend: it answers D-Bus until
    // the session ends.
    zbus::block_on(screencast::serve())
}
