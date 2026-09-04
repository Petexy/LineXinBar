//! What Steam actually does with a one-to-one conversation, over the shell's
//! own CM session.
//!
//! There is no disk to read and no log to grep here either — a conversation is
//! request/response and server push on a live socket — so the only way to see
//! whether `chat_mode = 2` was taken, whether a send comes back stamped, and
//! whether Steam pushes an echo of it at this session, is to hold a connection
//! open and print what arrives on it. This is [`probe-friends`] for the other
//! half of the panel.
//!
//! ```console
//! $ cargo run -p lxb-steam --example probe-chat -- --with 7656119…
//! $ cargo run -p lxb-steam --example probe-chat -- --with 7656119… --say "hello"
//! $ cargo run -p lxb-steam --example probe-chat -- --with 7656119… --measure-limit
//! ```
//!
//! **`--say` writes to another person's Steam account.** It is opt-in for that
//! reason, it sends exactly one message, and it prints the SteamID it is about
//! to write to before it does. Without it this reads and listens and writes
//! nothing at all.
//!
//! `--measure-limit` is the one thing here that cannot be answered any other
//! way: Steam publishes no message-size limit, and the only way to know it is
//! to send progressively longer messages and see which one is refused. It
//! **writes several messages** and should only ever be pointed at an account of
//! your own.
//!
//! Nothing is invented. Message bodies are printed by *length* rather than by
//! content, which is the same rule the rest of this crate is under: see
//! [`lxb_steam::chat`], where the reason no body is ever logged is written
//! down.

use std::time::{Duration, Instant};

use lxb_steam::chat::{self, Wanted, Word};
use lxb_steam::{Event, Steam};

/// Long enough for a sign-in, a roster and a round trip, with room for a slow
/// network.
const PATIENCE: Duration = Duration::from_secs(3 * 60);

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "lxb_steam=info".into()),
        )
        .init();

    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let argument = |name: &str| {
        arguments
            .iter()
            .position(|one| one == name)
            .and_then(|at| arguments.get(at + 1))
            .cloned()
    };
    let Some(with) = argument("--with").and_then(|said| said.parse::<u64>().ok()) else {
        eprintln!("usage: probe-chat --with <steamid64> [--say <message>] [--measure-limit]");
        std::process::exit(2);
    };
    let say = argument("--say");
    let measure = arguments.iter().any(|one| one == "--measure-limit");
    let _ = &measure;

    let mut steam = Steam::start();
    let began = Instant::now();
    let mut account: Option<u64> = None;
    let mut listening = false;
    let mut asked_history = false;
    // What has been sent and not yet answered, and the lengths being tried.
    let mut outstanding: Vec<(u64, usize)> = Vec::new();
    // Which lengths to try, in order. Steam publishes no limit, and the answer
    // it gives is a bare result number — so the way to tell a *size* refusal
    // from a rate limit is to follow a refused length with a short one and see
    // whether that goes. Hence a list, chosen at the command line.
    let mut lengths: Vec<usize> = match argument("--lengths") {
        Some(said) => said
            .split(',')
            .filter_map(|one| one.trim().parse().ok())
            .collect(),
        None if measure => vec![4_000, 4_001, 5_000, 8_000],
        None => Vec::new(),
    };
    let pause = argument("--pause")
        .and_then(|said| said.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(0));
    let mut last_send = Instant::now() - Duration::from_secs(600);
    let mut request = 0u64;
    let mut mint = move || {
        request += 1;
        request
    };

    while began.elapsed() < PATIENCE {
        for event in steam.take() {
            match event {
                Event::SignedOut => {
                    println!("nobody is signed in to Steam on this machine");
                    return;
                }
                Event::SignedIn(who) => {
                    account = Some(who.steam_id);
                    println!("signed in as …{}", chat::short(who.steam_id));
                }
                Event::Friends(roster) => {
                    if let Some(friend) =
                        roster.friends.iter().find(|friend| friend.steam_id == with)
                    {
                        println!(
                            "talking to {} (…{}) — {}",
                            friend.name,
                            chat::short(friend.steam_id),
                            friend.doing()
                        );
                    }
                }
                Event::Chat(heard) => {
                    if Some(heard.account) != account {
                        println!("!! an answer about another account arrived and was dropped");
                        continue;
                    }
                    match heard.word {
                        Word::Listening => {
                            println!("CM session {} is carrying chat", heard.generation);
                            listening = true;
                        }
                        Word::History {
                            with: about,
                            said: Ok(messages),
                            ..
                        } => {
                            println!(
                                "history for …{}: {} messages",
                                chat::short(about),
                                messages.len()
                            );
                            for message in messages.iter().rev().take(5).rev() {
                                println!(
                                    "  {} {} at {}.{} — {} characters",
                                    if message.from_me { "->" } else { "<-" },
                                    if message.from_me { "me" } else { "them" },
                                    message.key.at,
                                    message.key.ordinal,
                                    message.body.chars().count()
                                );
                            }
                        }
                        Word::History {
                            with: about,
                            said: Err(why),
                            ..
                        } => println!("history for …{} failed: {why}", chat::short(about)),
                        Word::Sent {
                            request,
                            said: Ok(message),
                            ..
                        } => {
                            let length = take(&mut outstanding, request);
                            println!(
                                "sent ({length:?} characters): Steam stamped it {}.{}, it came \
                                 back as {} characters",
                                message.key.at,
                                message.key.ordinal,
                                message.body.chars().count()
                            );
                        }
                        Word::Sent {
                            request,
                            said: Err(why),
                            ..
                        } => {
                            let length = take(&mut outstanding, request);
                            println!("REFUSED ({length:?} characters): {why}");
                        }
                        Word::Arrived { with: about, said } => println!(
                            "arrived from …{}: {} characters, {} at {}.{}",
                            chat::short(about),
                            said.body.chars().count(),
                            if said.from_me {
                                "an echo of this account's own"
                            } else {
                                "theirs"
                            },
                            said.key.at,
                            said.key.ordinal
                        ),
                        Word::Typing { with: about } => {
                            println!("…{} is typing", chat::short(about))
                        }
                    }
                }
                _ => {}
            }
        }

        if listening && !asked_history {
            asked_history = true;
            steam.chat(Wanted::History {
                with,
                request: mint(),
            });
            if let Some(body) = say.clone() {
                println!(
                    "about to write {} characters to SteamID {with} — this reaches a real account",
                    body.chars().count()
                );
                let request = mint();
                outstanding.push((request, body.chars().count()));
                steam.chat(Wanted::Send {
                    with,
                    request,
                    body,
                });
            }
            steam.chat(Wanted::Typing { with });
        }
        // One length at a time, so a refusal can be told from the one before it
        // — and with a wait between, because Steam rate-limits sends and a
        // refusal for going too fast reads exactly like a refusal for going too
        // long. Measured 2026-09-03: two sends back to back are taken and the
        // third is refused with result 84 whatever its length.
        if listening && asked_history && outstanding.is_empty() && last_send.elapsed() >= pause {
            if let Some(length) = lengths.first().copied() {
                last_send = Instant::now();
                lengths.remove(0);
                let request = mint();
                outstanding.push((request, length));
                println!("trying {length} characters…");
                steam.chat(Wanted::Send {
                    with,
                    request,
                    // A body that says what it is, repeated to length. Never
                    // anything that reads like a real message.
                    body: "probe "
                        .repeat(length.div_ceil(6))
                        .chars()
                        .take(length)
                        .collect(),
                });
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    println!("done");
}

/// Take one outstanding send off the list, and say how long it was.
fn take(outstanding: &mut Vec<(u64, usize)>, request: u64) -> Option<usize> {
    let at = outstanding.iter().position(|(one, _)| *one == request)?;
    Some(outstanding.remove(at).1)
}
