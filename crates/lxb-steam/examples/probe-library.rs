//! What this machine's Steam client says is installed, read the way the shell
//! reads it.
//!
//! The disk half of the library, on its own and with no account involved: the
//! Steam libraries this machine has, the manifests inside them, and the rows
//! those turn into. Nothing here signs in, asks Steam anything, or touches the
//! network — so it is the one part of the integration that can be checked
//! against a real machine without an account.
//!
//! ```console
//! $ cargo run -p lxb-steam --example probe-library
//! $ cargo run -p lxb-steam --example probe-library -- --watch
//! ```
//!
//! Everything printed comes off the disk as it is. Nothing is invented for the
//! sake of having something to show: a machine with no Steam client on it
//! prints that it found none.
//!
//! `--watch` answers the one question a single look cannot: **how often Valve's
//! client actually rewrites a manifest**. The percentage on a row and the bar
//! under it can only be as fine as that file is, and the client's own log
//! records state changes it may not have written to disk yet — so the only
//! honest way to know is to watch the file while a download runs. It prints a
//! line whenever any row's words change, with the second it changed, and
//! touches nothing. Start it, start a download in the shell or in Steam, and
//! read the gaps.

fn main() {
    if std::env::args().any(|argument| argument == "--watch") {
        watch();
        return;
    }
    look();
}

/// Print what a row would say every time it changes, until interrupted.
fn watch() {
    // The rate the shell itself looks at the disk while something is moving.
    // Asking faster would only prove this probe can outrun the file.
    let step = std::time::Duration::from_secs(2);
    let mut said: std::collections::BTreeMap<u32, String> = std::collections::BTreeMap::new();
    let mut first = true;
    println!("watching every {} seconds; ^C to stop", step.as_secs());
    loop {
        if let Some(backend) = lxb_steam::backend::Backend::chosen() {
            let installed = lxb_steam::library::installed_for(&backend);
            for game in lxb_steam::library::merge(Vec::new(), &installed) {
                let now = format!(
                    "{:<34}  {}",
                    game.note(),
                    match game.fraction() {
                        Some(share) => format!("bar {:.0}%", share * 100.0),
                        None => "no bar".to_string(),
                    }
                );
                let before = said.insert(game.app_id, now.clone());
                // Everything is new on the first pass, and a wall of it says
                // nothing about how often the file moves.
                if first || before.as_deref() == Some(now.as_str()) {
                    continue;
                }
                println!("{}  {:>8}  {now}", stamp(), game.app_id);
            }
        }
        first = false;
        std::thread::sleep(step);
    }
}

/// The wall clock, to the second, which is the whole point of the line.
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

fn look() {
    // The whole point of asking this first: on a machine with two Steams, what
    // is *read* has to be the one that would be *driven*. Printing which is
    // which is half of what this probe is for.
    let Some(backend) = lxb_steam::backend::Backend::chosen() else {
        println!("this machine has no Steam, and nothing one left behind");
        return;
    };
    match backend.client.as_ref() {
        Some(client) => println!("driving {client:?}"),
        None => println!("no Steam client is installed; reading what one left behind"),
    }
    println!("Steam is at {}", backend.root().display());
    let every = lxb_steam::client::Where::all();
    if every.len() > 1 {
        println!("this machine has more than one Steam: {every:?}");
    }

    let libraries = backend.libraries();
    println!("{} librar{}:", libraries.len(), plural(libraries.len()));
    for library in &libraries {
        println!("  {}", library.display());
    }

    // What the install panel would say before agreeing to a download.
    for room in lxb_steam::library::room_in_each(&backend) {
        println!(
            "  {}  {}",
            room.said()
                .unwrap_or_else(|| "free space unknown".to_string()),
            room.path.display()
        );
    }

    // Whether anything is *doing* what the manifests below describe. A
    // manifest is a record of what Valve's client was in the middle of when it
    // was last running, and it goes on saying so for as long as nobody starts
    // Steam again — so a row reading "Updating" under a line saying no client
    // is running is work that has stopped. See
    // [`lxb_steam::Steam::client_is_running`], which is the same question the
    // shell asks before it draws the column.
    let running = lxb_steam::client::Where::find()
        .and_then(|found| Some((lxb_steam::client::Options::for_client(&found)?, found)))
        .is_some_and(|(options, found)| lxb_steam::client::is_running(Some(&found), &options));
    println!(
        "\na client is {}running, so what these manifests describe {}",
        if running { "" } else { "not " },
        if running {
            "is being done"
        } else {
            "is not being done by anything"
        }
    );

    let installed = lxb_steam::library::installed_for(&backend);
    // Through the same merge the shell uses, with nothing owned, so what is
    // printed is the column the bar would build for somebody whose account
    // Steam cannot be asked about — which is also what a signed-out machine
    // with games on it shows.
    let rows = lxb_steam::library::merge(Vec::new(), &installed);
    // Both numbers, because they are not the same number and the difference is
    // the point: every Proton, every Steam Linux Runtime and Valve's shared
    // redistributables have manifests of their own, and none of them is a game
    // anybody presses. A count of manifests over a list of games reads as rows
    // gone missing.
    println!(
        "\n{} manifest{} on the disk, {} of them game{}:",
        installed.len(),
        if installed.len() == 1 { "" } else { "s" },
        rows.len(),
        if rows.len() == 1 { "" } else { "s" }
    );

    for game in rows {
        println!(
            "  {:>8}  {:<52}  {:<34}  {}",
            game.app_id,
            elide(&game.name, 52),
            // What the *shell* would draw, which on a machine with no client
            // running is not what the manifest says: work that has stopped
            // says it is waiting rather than going on counting.
            match !running && game.standing.moving() {
                true => game.note_waiting_for_steam(),
                false => game.note(),
            },
            // What the bar under that row would be drawn from, beside the
            // words, because the two disagreeing is exactly the shape of bug
            // this probe is for. A row saying "Installed" with a fill beside it
            // is `BytesDownloaded`/`BytesToDownload` being read on a standing
            // where they are the *last* operation's — see
            // `Standing::counting_bytes`.
            match game.fraction() {
                Some(share) => format!("bar {:.0}%", share * 100.0),
                None => "no bar".to_string(),
            }
        );
        // The second thing the manifest says about an update, and for the
        // first stretch of one the only thing. A row reading "Installed" with
        // this beside it is a game Steam is working on and has not written a
        // working bit for — the state a loading screen has to wait out. See
        // `Game::update_outstanding`.
        if game.update_outstanding {
            println!(
                "            ^ and Steam wants a different build on the disk than the one on it{}",
                match running {
                    true => ", with a client running to fetch it",
                    false => ", with no client running to fetch it",
                }
            );
            // And the two fields that say the client is not going to do
            // anything about it, which read identically without them. A
            // deferral is Steam having looked at the game and put the work in
            // the diary; a result is Steam having tried it and stopped.
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|since| since.as_secs())
                .unwrap_or_default();
            if game.scheduled_for > now {
                println!(
                    "              but it is scheduled for {} ({} away), so nothing is happening now",
                    game.scheduled_for,
                    roughly(game.scheduled_for - now),
                );
            }
            if game.last_result != 0 {
                println!(
                    "              and the last attempt at it ended UpdateResult {}",
                    game.last_result
                );
            }
        }
    }

    // What the client's own log says it has in hand, which is the one place
    // some of it is written down at all: a file check moves no bytes, so no
    // manifest above describes it. See `client::work_in_flight`.
    if let Some(options) = lxb_steam::client::Where::find()
        .as_ref()
        .and_then(lxb_steam::client::Options::for_client)
    {
        let mut jobs = lxb_steam::client::Jobs::default();
        jobs.look(&options.root);
        let doing: Vec<_> = jobs.each().collect();
        if doing.is_empty() {
            println!("\nits own log says it has nothing in hand");
        } else {
            println!(
                "\nand its own log says it has these in hand, which no manifest above need be"
            );
            println!("saying anything about. A game's own content is what a row is drawn from; a");
            println!("shader cache is not, and all of it stops the client being shut down:");
            for (track, app_id, phase) in doing {
                println!(
                    "  {app_id:<9} {:<9} {}",
                    format!("{track:?}"),
                    match (track, phase) {
                        (lxb_steam::client::Track::Shaders, _) =>
                            "a cache Steam fetches while you play — no word on the row",
                        (_, lxb_steam::client::InHand::Checking) =>
                            "reading the disk back — \"Checking files\"",
                        (_, lxb_steam::client::InHand::Working) =>
                            "a job of some other kind — \"Updating\"",
                    }
                );
            }
        }
    }

    match lxb_steam::client::Where::find() {
        Some(where_it_is) => {
            println!("\nValve's client is here: {where_it_is:?}; it is what starts these.")
        }
        None => println!(
            "\nthere is no Valve client on this machine, so nothing in this library can be started"
        ),
    }
}

/// A span of seconds as something a person can read at a glance.
fn roughly(seconds: u64) -> String {
    let (count, unit) = match seconds {
        ..3600 => (seconds / 60, "minute"),
        3600..86400 => (seconds / 3600, "hour"),
        _ => (seconds / 86400, "day"),
    };
    match count {
        1 => format!("1 {unit}"),
        _ => format!("{count} {unit}s"),
    }
}

fn plural(count: usize) -> &'static str {
    if count == 1 {
        "y"
    } else {
        "ies"
    }
}

/// Keep the columns lined up when a game has a very long name.
fn elide(name: &str, width: usize) -> String {
    if name.chars().count() <= width {
        return name.to_string();
    }
    name.chars().take(width - 1).collect::<String>() + "…"
}
