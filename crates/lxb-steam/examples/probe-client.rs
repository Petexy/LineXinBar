//! What Valve's client is doing, and whether this session can sign it in.
//!
//! The background client is the half of the integration with no window to look
//! at, so this is how it is looked at: where it is, whether it is running,
//! whether it has signed in, and — with `--start` — what happens when it is
//! given the credential this session already holds and asked to come up.
//!
//! ```console
//! $ cargo run -p lxb-steam --example probe-client
//! $ cargo run -p lxb-steam --example probe-client -- --start
//! $ cargo run -p lxb-steam --example probe-client -- --context
//! ```
//!
//! `--start` is an ordinary press: on a client that can sign itself in it opens
//! nothing at all, and the marker and the port should both be absent either
//! side of it. `--context` is the install path, the one thing that needs the
//! client's own interface — it says what the exposure looked like afterwards,
//! which is how the two halves of that policy get checked rather than assumed.
//!
//! Nothing is invented. A machine with no client says so, a session with
//! nobody signed in says so, and `--start` reports what the client's own log
//! says about itself rather than what this crate hoped would happen.

use std::time::{Duration, Instant};

use lxb_steam::{client, Event};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "lxb_steam=info".into()),
        )
        .init();

    let context = std::env::args().any(|argument| argument == "--context");
    // Reaching the interface means bringing the client up first, so asking for
    // one is asking for the other.
    let start = context || std::env::args().any(|argument| argument == "--start");

    let found = client::Where::find();
    match &found {
        Some(client::Where::Native(path)) => println!("client: {}", path.display()),
        Some(client::Where::Flatpak) => println!("client: the Flatpak"),
        None => println!("client: none is installed"),
    }
    let Some(where_it_is) = found.as_ref() else {
        return;
    };
    let Some(options) = client::Options::for_client(where_it_is) else {
        println!("there is no home directory to find Steam's own in");
        return;
    };
    println!("root: {}", options.root.display());
    println!("home: {}", options.home.display());
    if !lxb_steam::library::looks_like_a_root(&options.root) {
        println!("this client has never been run; the first press would start it");
    }
    // What the client's debugging interface is doing on this machine, before
    // anything here has touched it. Asked here rather than under `running()`
    // because half of it is a file on the disk and answers with no client up at
    // all — which is exactly the state the fortnight-old marker was found in.
    let exposure = lxb_steam::webui::exposure(&options.root);
    println!(
        "interface exposure: {} (port open: {}, marker: {:?})",
        exposure.said(),
        exposure.open,
        exposure.marker
    );
    let state = client::state(found.as_ref(), &options);
    println!("state: {state:?}");

    // Both of these are only answerable about a client that is up, and both are
    // ways a press can fail while looking like it worked.
    if state.running() {
        // The first leaves no other trace at all. A client on another session
        // takes the URL, starts the game and opens it over there — so the
        // game's log says it launched, Steam's says it handed over, and the
        // only thing that says otherwise is this line.
        if client::in_this_session(&options) {
            println!("session: the running client is this session's");
        } else {
            println!(
                "session: the running client belongs to ANOTHER session — its games open there"
            );
        }

        // And what this shell calls into the client are Valve's own internals,
        // not a promised interface, so a client update can take one away — the
        // shape of *that* failure is a press that silently does nothing.
        match lxb_steam::webui::missing_methods() {
            Ok(missing) if missing.is_empty() => {
                println!("interface: every method this shell calls is there")
            }
            Ok(missing) => println!("interface: MISSING {}", missing.join(", ")),
            Err(problem) => println!("interface: could not be asked — {problem}"),
        }
        // And which client that was true of, because the answer above is only
        // worth anything attached to one. A client updates itself in the
        // background; a report saying a method went missing, with no build
        // beside it, cannot be checked by anybody afterwards.
        match lxb_steam::webui::build() {
            Ok(build) => println!("client build: {build}"),
            Err(problem) => println!("client build: could not be asked — {problem}"),
        }
        // What the shell's controller chord has to send. The overlay lives
        // inside the game rather than in the client, so a keystroke is the only
        // way to raise one, and this setting is kept in memory and written to no
        // file — the interface being open is the one chance to read it.
        match lxb_steam::webui::overlay_key() {
            Ok(key) if key.is_shift_tab() && key.enabled => {
                println!("overlay key: {key} — what the shell's Guide+Select chord sends")
            }
            Ok(key) => println!(
                "overlay key: {key} — the shell's Guide+Select chord sends Shift+Tab \
                 and will do nothing"
            ),
            Err(problem) => println!("overlay key: could not be asked — {problem}"),
        }
    }

    // Starting the worker restores whatever session is on the disk, and it is
    // the act of Steam accepting that session that hands the client its own
    // credential — so this is also what proves the hand-over happens.
    println!("\nasking this session who is signed in…");
    let mut steam = lxb_steam::Steam::start();
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut account = None;
    while Instant::now() < deadline && account.is_none() {
        for event in steam.take() {
            match event {
                Event::SignedIn(who) => {
                    println!("signed in as {} ({})", who.name, who.steam_id);
                    account = Some(who);
                }
                Event::SignedOut => {
                    println!("nobody is signed in to this session; sign in from the shell first");
                    return;
                }
                Event::SignInFailed(why) => println!("sign-in failed: {why}"),
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    if account.is_none() {
        println!("this session did not settle within 45 seconds");
        return;
    }

    let Some(where_it_is) = found.as_ref() else {
        return;
    };
    if !start {
        println!("\npass --start to bring the client up and watch it sign itself in");
        return;
    }

    // `--context` is the install path: the one thing that needs the client's
    // own interface rather than a `steam:` URL, and therefore the one thing
    // that opens it. Run without it to see an ordinary press, which on a
    // client that can sign itself in exposes nothing at all.
    if context {
        // Asking to stop an install that is not running is the cheapest thing
        // in the crate that goes the whole way down the `Need::Context` path:
        // the client is woken, its interface opened if it is not already
        // answering, and the call made. Whether Steam had anything to stop is
        // beside the point — what is being watched is the exposure either side.
        println!("\nasking for the client's JS context, as installing does");
        let began = Instant::now();
        steam.stop_installing(0);
        let deadline = began + Duration::from_secs(180);
        while Instant::now() < deadline {
            for event in steam.take() {
                match event {
                    Event::InstallStopped { .. } | Event::InstallFailed { .. } => {
                        println!("  {:>4.0}s  {event:?}", began.elapsed().as_secs_f32());
                        println!(
                            "\nmarker now {}, port {}",
                            if lxb_steam::webui::available(&options.root) {
                                "PRESENT — it was left behind"
                            } else {
                                "gone, as it should be"
                            },
                            if lxb_steam::webui::reachable() {
                                "open for this client's lifetime"
                            } else {
                                "CLOSED"
                            },
                        );
                        return;
                    }
                    _ => {}
                }
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        println!("  it did not answer within three minutes");
        return;
    }

    println!("\nwaking the client — this is a cold start, so give it a minute");
    let asked = steam.wake_client();
    let began = Instant::now();
    let deadline = began + Duration::from_secs(180);
    let mut last = None;
    while Instant::now() < deadline {
        for event in steam.take() {
            // Only this probe's own wake. Every answer names the request it
            // belongs to, and one belonging to anything else is not this one's
            // to report on. See `lxb_steam::Ticket`.
            if let Event::Client { ticket, report } = event {
                if ticket.request != asked {
                    continue;
                }
                println!("  {:>4.0}s  {report:?}", began.elapsed().as_secs_f32());
                match report {
                    lxb_steam::ClientReport::Ready => {
                        println!("\nthe client is signed in, with no window and nothing asked");
                        return;
                    }
                    lxb_steam::ClientReport::Unavailable(why) => {
                        println!("\nit could not be: {why}");
                        return;
                    }
                    // The refusal that is not a failure: there is a client
                    // running and it is not this session's to move. The shell
                    // asks; this probe only reports.
                    lxb_steam::ClientReport::SomebodyElses(why) => {
                        println!("\nit was left alone: {why}");
                        return;
                    }
                    lxb_steam::ClientReport::Waking => {}
                }
            }
        }
        let now = client::state(Some(where_it_is), &options);
        if Some(now) != last {
            println!("  {:>4.0}s  {now:?}", began.elapsed().as_secs_f32());
            last = Some(now);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    println!("\nit did not settle within three minutes.");
}
