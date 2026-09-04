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
//! explained away — and where the client is exposing its interface, it says
//! what the client thinks it is doing, including the case that looks exactly
//! like slowness and is not: a launch stopped on a question nobody can see.

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
    let Some(where_it_is) = client::Where::find() else {
        println!("there is no Steam client on this machine");
        return;
    };
    let Some(options) = client::Options::for_client(&where_it_is) else {
        println!("there is no home directory to find Steam's own in");
        return;
    };

    let state = client::state(Some(&where_it_is), &options);
    println!("the client is {state:?}");
    if !state.signed_in() {
        println!("it is not signed in; run probe-client -- --start first");
        return;
    }

    let began = Instant::now();
    if let Err(error) = client::open(
        &where_it_is,
        Some(&options),
        &format!("steam://rungameid/{app_id}"),
    ) {
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
    //
    // And beside it, what the client says it is doing. A launch that never
    // arrives is otherwise a wait with no account of itself, and the commonest
    // reason for one is not slowness at all: the client has stopped to ask
    // something, in a window the shell is holding off the screen. This is the
    // one run that says so.
    let wanted: u32 = app_id.parse().unwrap_or_default();
    let mut said = String::new();
    let deadline = began + Duration::from_secs(240);
    while Instant::now() < deadline {
        if let Some((pid, name)) = running_game(&app_id) {
            println!(
                "\n{name} (pid {pid}) came up after {:.1}s",
                began.elapsed().as_secs_f32()
            );
            return;
        }
        if let Ok(launches) = lxb_steam::webui::launching() {
            let stopped = launches.iter().find(|one| one.app_id == wanted);
            let now = match stopped {
                Some(one) if one.waiting_for_a_person => {
                    format!("{} — WAITING FOR A PERSON ({})", one.task, one.details)
                }
                Some(one) => one.task.clone(),
                None => "nothing on the client's launch list".to_string(),
            };
            if now != said {
                println!("  {:>4.0}s  {now}", began.elapsed().as_secs_f32());
                said = now;
                // And, where the shell has a panel for it, the panel — every
                // word of it in the language the client is running in. This is
                // what a person holding a controller would be reading.
                if let Some(one) = stopped.filter(|one| one.waiting_for_a_person) {
                    match lxb_steam::webui::the_question(one) {
                        Ok(Some(question)) => {
                            println!("\n  the shell would ask:");
                            for line in &question.body {
                                println!("    {line}");
                            }
                            for answer in &question.answers {
                                println!("    [ {} ] -> {:?}", answer.label, answer.carry);
                            }
                            println!();
                        }
                        Ok(None) => println!("  (no panel for this one; Steam is shown instead)"),
                        Err(problem) => println!("  (could not be asked: {problem})"),
                    }
                }
            }
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
