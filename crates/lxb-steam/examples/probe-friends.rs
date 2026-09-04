//! Who Steam says is around, and — the reason this exists — what it says about
//! *this* account.
//!
//! The friends half of the integration has no disk to read and no log to grep:
//! a roster is a thing Steam pushes over a live connection and nothing writes
//! it down. So the only way to see what the panel would draw, or to watch a
//! status change actually land, is to hold a connection open and print what
//! arrives on it.
//!
//! ```console
//! $ cargo run -p lxb-steam --example probe-friends
//! $ cargo run -p lxb-steam --example probe-friends -- --watch
//! $ cargo run -p lxb-steam --example probe-friends -- --status away
//! ```
//!
//! `--watch` prints a line every time the account's own persona state changes
//! and nothing otherwise, which is what makes it a readback: start it, change
//! the status somewhere else — Valve's own client, a `steam:` URL, the shell's
//! panel — and read whether Steam agreed. **This connection announces itself**
//! like any other, so the first line it prints is its own announcement rather
//! than news; everything after it is somebody else moving.
//!
//! `--status <online|away|invisible|offline|busy|snooze>` sets it through this
//! session's own connection, which is the shell's path, and then goes on
//! watching so the answer can be read on the same clock.
//!
//! Nothing is invented. A machine with nobody signed in prints that.

use std::time::{Duration, Instant};

use lxb_steam::{Event, Presence, Steam};

/// Long enough for a roster to arrive, for a status set elsewhere to come back
/// round, and for a cold Valve client started beside it to finish signing in —
/// which is the slowest thing this is ever used to watch, and takes a minute
/// and a half on the machine it was measured on.
const PATIENCE: Duration = Duration::from_secs(4 * 60);

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "lxb_steam=info".into()),
        )
        .init();

    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let watch = arguments.iter().any(|argument| argument == "--watch");
    let wanted = arguments
        .iter()
        .position(|argument| argument == "--status")
        .and_then(|at| arguments.get(at + 1))
        .map(|said| match named(said) {
            Some(status) => status,
            None => {
                eprintln!("{said:?} is not a status; try online, away or invisible");
                std::process::exit(2);
            }
        });

    let mut steam = Steam::start();
    let began = Instant::now();
    let mut said: Option<Presence> = None;
    let mut asked = false;
    let mut listed = false;

    while began.elapsed() < PATIENCE {
        for event in steam.take() {
            match event {
                Event::SignedOut => {
                    println!("nobody is signed in to Steam on this machine");
                    return;
                }
                Event::Friends(roster) => {
                    let Some(me) = roster.me.as_ref() else {
                        continue;
                    };
                    if !listed {
                        println!(
                            "{}  {} — {} friends",
                            stamp(),
                            me.name,
                            roster.friends.len()
                        );
                        listed = true;
                    }
                    if said != Some(me.presence) {
                        println!("{}  status: {}", stamp(), me.presence.said());
                        said = Some(me.presence);
                    }
                    // The roster as the panel would draw it, once.
                    if !watch && wanted.is_none() && listed {
                        for friend in roster.friends.iter().take(12) {
                            println!("    {:<24} {}", friend.name, friend.doing());
                        }
                        if roster.friends.len() > 12 {
                            println!("    … and {} more", roster.friends.len() - 12);
                        }
                        return;
                    }
                }
                _ => {}
            }
        }
        // After the roster has arrived, so the line before it is the state this
        // session found rather than the state it asked for.
        if let (Some(status), true, false) = (wanted, listed, asked) {
            println!("{}  asking Steam for {}", stamp(), status.said());
            steam.set_status(status);
            asked = true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    if !listed {
        println!("Steam sent no roster within {PATIENCE:?}");
    }
}

fn named(said: &str) -> Option<Presence> {
    Some(match said.to_ascii_lowercase().as_str() {
        "online" => Presence::Online,
        "away" => Presence::Away,
        "busy" => Presence::Busy,
        "snooze" => Presence::Snooze,
        "invisible" => Presence::Invisible,
        "offline" => Presence::Offline,
        _ => return None,
    })
}

/// The wall clock to the second, which is the whole point of a watch line.
fn stamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or_default();
    format!(
        "{:02}:{:02}:{:02}",
        (now / 3600) % 24,
        (now / 60) % 60,
        now % 60
    )
}
