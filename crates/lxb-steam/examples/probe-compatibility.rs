//! Which Steam Play compatibility tools Valve's client offers, and which one it
//! is set to use.
//!
//! The one half of the integration that cannot be read off the disk. A machine
//! with three Protons installed is offered eleven, because what the client
//! lists is what the *account* may use rather than what is unpacked in
//! `steamapps/common` — so this asks the client, through exactly the calls the
//! shell's Compatibility row is made of.
//!
//! ```console
//! $ cargo run -p lxb-steam --example probe-compatibility
//! $ cargo run -p lxb-steam --example probe-compatibility -- 220200
//! $ cargo run -p lxb-steam --example probe-compatibility -- 220200 --force proton_experimental
//! ```
//!
//! Without `--force` nothing is written: it asks, prints and stops. With it,
//! the tool named is set and then read back — and the honest way to run that on
//! somebody's own machine is to name **the tool the game is already set to**,
//! which is a call that goes the whole way down and changes nothing.
//!
//! Asking needs the client's own interface, so the first run of a session may
//! restart a Steam that was already up: the marker it is exposed with is read
//! once, on the way up. See [`lxb_steam::webui`].

use std::time::{Duration, Instant};

use lxb_steam::webui::Which;
use lxb_steam::Event;

/// Long enough to wake a cold client and ask it twice, and short enough that a
/// client which will not answer is a failure rather than a hang.
const PATIENCE: Duration = Duration::from_secs(180);

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "lxb_steam=info".into()),
        )
        .init();

    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let app_id: Option<u32> = arguments
        .iter()
        .find_map(|argument| argument.parse::<u32>().ok());
    let forcing = arguments
        .iter()
        .position(|argument| argument == "--force")
        .and_then(|at| arguments.get(at + 1))
        .map(String::from);
    if forcing.is_some() && app_id.is_none() {
        eprintln!("--force needs an app id to force it on");
        std::process::exit(2);
    }

    let mut steam = lxb_steam::Steam::start();
    if !signed_in(&mut steam) {
        return;
    }

    let asked: Vec<Which> = match app_id {
        Some(app_id) => vec![Which::OtherTitles, Which::Game(app_id)],
        None => vec![Which::OtherTitles],
    };
    for which in &asked {
        println!("\nasking Steam what runs {which}…");
        steam.compatibility(*which);
        if !answered(&mut steam, *which) {
            return;
        }
    }

    let (Some(app_id), Some(tool)) = (app_id, forcing) else {
        println!("\npass --force <tool> with an app id to set one");
        return;
    };
    let which = Which::Game(app_id);
    println!("\ntelling Steam to run {which} under {tool}…");
    steam.force_compatibility(which, Some(tool));
    answered(&mut steam, which);
}

/// Wait for the worker to restore whatever session is on the disk. Nothing here
/// can be asked of a client that is not signed in as somebody.
fn signed_in(steam: &mut lxb_steam::Steam) -> bool {
    println!("asking this session who is signed in…");
    let deadline = Instant::now() + Duration::from_secs(45);
    while Instant::now() < deadline {
        for event in steam.take() {
            match event {
                Event::SignedIn(who) => {
                    println!("signed in as {} ({})", who.name, who.steam_id);
                    return true;
                }
                Event::SignedOut => {
                    println!("nobody is signed in; sign in from the shell first");
                    return false;
                }
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    println!("this session did not settle within 45 seconds");
    false
}

/// Print the next answer about `which`, and say whether one arrived.
fn answered(steam: &mut lxb_steam::Steam, which: Which) -> bool {
    let began = Instant::now();
    while began.elapsed() < PATIENCE {
        for event in steam.take() {
            match event {
                Event::Compatibility { which: about, said } if about == which => {
                    println!(
                        "  {:>4.0}s  {} tool(s), forced: {}",
                        began.elapsed().as_secs_f32(),
                        said.tools.len(),
                        said.forced_display().unwrap_or("nothing"),
                    );
                    for tool in &said.tools {
                        let mark = match said.forced.as_deref() == Some(tool.name.as_str()) {
                            true => "*",
                            false => " ",
                        };
                        println!("   {mark} {:<28} {}", tool.display, tool.name);
                    }
                    return true;
                }
                Event::CompatibilityUnavailable { which: about, why } if about == which => {
                    println!(
                        "  {:>4.0}s  could not be asked — {why}",
                        began.elapsed().as_secs_f32()
                    );
                    return false;
                }
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    println!("  it did not answer within three minutes");
    false
}
