//! Move one game on or off the disk, the way a press on the bar does.
//!
//! The Install and Uninstall rows without the shell around them: the stored
//! session is restored, the catalogue arrives, and one title is asked for
//! through the same [`Steam::install`] and [`Steam::uninstall`] the menu rows
//! call. Everything after that — waking Valve's client, opening its interface,
//! driving its install flow, watching the disk — happens exactly as it does in
//! a session.
//!
//! ```console
//! $ cargo run -p lxb-steam --example probe-install -- 1812820
//! $ cargo run -p lxb-steam --example probe-install -- --remove 1812820
//! ```
//!
//! It reports what it saw, in the events the shell draws from. A game that
//! never comes down is reported as one, and so is one that Steam will not
//! fetch without asking somebody something.

use std::time::{Duration, Instant};

use lxb_steam::{Event, Steam};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "lxb_steam=info".into()),
        )
        .init();

    let mut arguments = std::env::args().skip(1).peekable();
    let remove = arguments.peek().is_some_and(|first| first == "--remove");
    if remove {
        arguments.next();
    }
    let Some(app_id) = arguments.next().and_then(|raw| raw.parse::<u32>().ok()) else {
        println!("usage: probe-install [--remove] <app id> [seconds]");
        return;
    };
    // How long to watch afterwards, because a cold client is most of a minute
    // before it has taken the request at all.
    let watch = Duration::from_secs(
        arguments
            .next()
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(180),
    );

    let mut steam = Steam::start();

    println!("waiting for the stored session…");
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut asked = false;
    while Instant::now() < deadline && !asked {
        for event in steam.take() {
            match event {
                Event::SignedIn(account) => println!("signed in as {}", account.name),
                Event::SignedOut => {
                    println!("no stored session: sign in through the shell first");
                    return;
                }
                Event::Library(games) => {
                    if asked {
                        continue;
                    }
                    let named = games
                        .iter()
                        .find(|game| game.app_id == app_id)
                        .map(|game| format!("{} — {}", game.name, game.note()))
                        .unwrap_or_else(|| "not in this library".to_string());
                    println!("{} games; {app_id} is {named}", games.len());
                    if remove {
                        println!("asking for it to go…");
                        steam.uninstall(app_id);
                    } else {
                        println!("asking for it…");
                        steam.install(app_id);
                    }
                    asked = true;
                }
                other => println!("  {other:?}"),
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    if !asked {
        println!("the library never arrived");
        return;
    }

    let until = Instant::now() + watch;
    while Instant::now() < until {
        for event in steam.take() {
            match event {
                // The whole library every ten seconds is noise here; what one
                // game says about itself is the answer.
                Event::Library(games) => {
                    if let Some(game) = games.iter().find(|game| game.app_id == app_id) {
                        println!("  library: {} — {}", game.name, game.note());
                    }
                }
                other => println!("  {other:?}"),
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}
