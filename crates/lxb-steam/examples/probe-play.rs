//! Ask the background client to start one game, and watch for it.
//!
//! The press path without the shell around it: wake Valve's client if it is
//! not up, hand it the game, and then wait — the same wait the loading screen
//! is drawing, and the reason that wait is measured in minutes rather than
//! seconds. Nothing here reads a window; what it watches is the game's own
//! process appearing under the client.
//!
//! ```console
//! $ cargo run -p lxb-steam --example probe-play -- 504230
//! ```
//!
//! It reports what it saw. A game that never starts is reported as one, not
//! explained away.

use std::time::{Duration, Instant};

use lxb_steam::client;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "lxb_steam=info".into()),
        )
        .init();

    let Some(app_id) = std::env::args().nth(1) else {
        println!("usage: probe-play <app id>");
        return;
    };
    let Some(options) = client::Options::found() else {
        println!("there is no Steam installation on this machine");
        return;
    };
    let Some(where_it_is) = client::Where::find() else {
        println!("there is no Steam client on this machine");
        return;
    };

    let state = client::state(Some(&where_it_is), &options);
    println!("the client is {state:?}");
    if !state.signed_in() {
        println!("it is not signed in; run probe-client -- --start first");
        return;
    }

    let began = Instant::now();
    if let Err(error) = client::tell(&where_it_is, &format!("steam://rungameid/{app_id}")) {
        println!("the client would not take it: {error}");
        return;
    }
    println!(
        "handed over in {:.0}ms; watching for the game",
        began.elapsed().as_millis()
    );

    // The game is the client's child and this process never sees it, so what
    // is watched for is any new process holding the app id in its environment
    // — which is how Steam tells a game which game it is.
    let deadline = began + Duration::from_secs(240);
    while Instant::now() < deadline {
        if let Some((pid, name)) = running_game(&app_id) {
            println!(
                "\n{name} (pid {pid}) came up after {:.1}s",
                began.elapsed().as_secs_f32()
            );
            return;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    println!("\nnothing came up within four minutes");
}

/// Any process Steam has told it is this app.
fn running_game(app_id: &str) -> Option<(u32, String)> {
    let wanted = format!("SteamAppId={app_id}");
    for entry in std::fs::read_dir("/proc").ok()? {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|name| name.parse::<u32>().ok()) else {
            continue;
        };
        let Ok(environment) = std::fs::read(format!("/proc/{pid}/environ")) else {
            continue;
        };
        if environment
            .split(|byte| *byte == 0)
            .any(|value| value == wanted.as_bytes())
        {
            let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
            return Some((pid, comm.trim().to_string()));
        }
    }
    None
}
