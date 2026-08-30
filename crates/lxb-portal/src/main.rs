//! LineXinBar's desktop portal: how an application outside the session asks
//! for a piece of it.
//!
//! Screen sharing on Wayland is deliberately not something an application can
//! simply do. It asks `xdg-desktop-portal` over D-Bus, that hands the question
//! to whichever backend the desktop installed, and the backend is the only part
//! of the chain that may read the screen — which is why every desktop ships one
//! of its own. This is LineXinBar's.
//!
//! What it does is two things, and they have almost nothing in common but the
//! bus name they answer on.
//!
//! Screen sharing joins two protocols that know nothing about each other:
//! `wlr-screencopy`, which is how the compositor hands out frames, and
//! PipeWire, which is what OBS, a browser and Discord all read. See
//! [`cast`] for the join itself.
//!
//! Choosing a file joins nothing — it carries a question to the session shell,
//! which is the only part of this desktop that can draw one, and carries the
//! answer back as URIs. See [`filechooser`], and [`pick`] for the road.

mod cast;
mod consent;
mod filechooser;
mod pick;
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

    /// Put a file question to the session shell straight away, without D-Bus
    /// or an application in front of it, and print what came back.
    ///
    /// The counterpart of `--debug-cast`, and it is here for the same reason:
    /// this half of the portal is a conversation with the shell that has
    /// nothing to do with the bus, and proving it by hand should not need a
    /// browser and an upload form. Takes what is being asked for — `one-file`,
    /// `many-files`, `folder` or `new-file`.
    #[arg(long, value_name = "PURPOSE", num_args = 0..=1, default_missing_value = "one-file")]
    debug_pick: Option<String>,

    /// One kind of file the question will accept, as `Name=pattern`, repeated
    /// once per pattern — `--kind 'Images=*.png' --kind 'Images=*.jpg'`. A
    /// pattern written `mime:image/png` is a media type rather than a name.
    /// Only meaningful with `--debug-pick`.
    #[arg(long = "kind", value_name = "NAME=PATTERN")]
    kinds: Vec<String>,

    /// A folder for the question to open in. Only meaningful with
    /// `--debug-pick`.
    #[arg(long, value_name = "DIRECTORY")]
    at: Option<String>,

    /// What a new file is called to begin with. Only meaningful with
    /// `--debug-pick new-file`.
    #[arg(long, value_name = "NAME")]
    called: Option<String>,
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

    if let Some(purpose) = cli.debug_pick {
        let purpose = match purpose.as_str() {
            "many-files" => pick::For::ManyFiles,
            "folder" => pick::For::AFolder,
            "new-file" => pick::For::ANewFile,
            "one-file" | "" => pick::For::OneFile,
            other => anyhow::bail!(
                "no such thing to ask for: {other} \
                 (one-file, many-files, folder or new-file)"
            ),
        };
        let mut kinds = Vec::new();
        for offered in &cli.kinds {
            let Some((name, pattern)) = offered.split_once('=') else {
                anyhow::bail!("a kind is written Name=pattern, not {offered}");
            };
            let (pattern, mime) = match pattern.strip_prefix("mime:") {
                Some(media) => (media, true),
                None => (pattern, false),
            };
            kinds.push(pick::Kind {
                name: name.to_string(),
                pattern: pattern.to_string(),
                mime,
            });
        }
        let wanted = pick::Wanted {
            // What an application with no `app_id` would say, which is a case
            // the shell has to answer for anyway.
            app_id: "lxb-portal".to_string(),
            purpose,
            title: String::new(),
            accept: String::new(),
            name: cli.called.unwrap_or_default(),
            at: cli.at.unwrap_or_default(),
            kinds,
        };
        // No patience worth the name: this is somebody standing at the machine
        // answering their own question.
        let chosen = pick::ask(&wanted, std::time::Duration::from_secs(30 * 60))?;
        if chosen.files.is_empty() {
            println!("nothing was chosen");
            return Ok(());
        }
        for path in &chosen.files {
            println!("{path}");
        }
        if let Some(kind) = chosen.kind {
            println!("(showing kind {kind})");
        }
        return Ok(());
    }

    // Otherwise this is the session's portal backend: it answers D-Bus until
    // the session ends.
    zbus::block_on(screencast::serve())
}
