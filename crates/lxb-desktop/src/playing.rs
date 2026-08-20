//! What the session is playing, and who is playing it.
//!
//! An application with nothing on screen is stopped outright — the compositor's
//! `sleep` module is the whole of that, and the complaint it exists for is a
//! game going on playing its music into the room from behind the start screen.
//! The same rule is wrong about exactly one thing: music somebody *put on*. A
//! player stopped mid-album is the same silence arrived at from the other side,
//! and unlike the game it is the silence nobody asked for.
//!
//! Nothing the compositor can see tells those apart. A game's soundtrack and an
//! album are both a program feeding a sink, from a process tree that looks the
//! same either way, behind a window that says nothing about it. The difference
//! is on the session bus, and the shell is the half of this session that is on
//! one — so this is here, and what it works out goes up over
//! `lxb_shell_v1.keep_awake`.
//!
//! ## What counts as playing
//!
//! Two facts, and both have to agree:
//!
//! 1. **The application says it is a media player, and says it is playing.**
//!    `org.mpris.MediaPlayer2` is the interface every player on a Linux desktop
//!    announces itself through — Spotify's client, a browser with a video in a
//!    tab, a film in VLC — and `PlaybackStatus` is the player's own word for
//!    what it is doing. No game exports it. That is what makes it the right
//!    question: it is not "is this making a noise", which a game answers yes to
//!    for the whole of an evening, but "is this a thing whose purpose is what
//!    it is playing".
//! 2. **Something is actually coming out of it.** [`crate::system::audible_applications`]
//!    reads the sound server for streams that are running rather than corked.
//!    A player left claiming `Playing` into a stream that ended — a tab closed
//!    without the page saying so, a client that crashed its decoder — would
//!    otherwise hold a whole process tree awake for the rest of the session on
//!    its own say-so.
//!
//! Neither on its own is enough, and the pair is deliberately narrow. A game
//! passes the second and never the first. A paused player passes the first only
//! until its stream corks, and then neither.
//!
//! ## Which application it is
//!
//! The bus and the sound server and the window each spell one application
//! differently, and none of the three is authoritative: a flatpak browser is
//! `app.zen_browser.zen` to the compositor, `zen` to the sound server, and
//! `org.mpris.MediaPlayer2.firefox.instance_1_145` on the bus. So every name a
//! player is known by is collected here, and the shell matches the lot against
//! the windows it already knows about with [`crate::apps::same_application`],
//! which is the function that already knows a reverse-DNS name from a program.
//!
//! ## Why a thread
//!
//! The same reason the notification daemon and the polkit agent have one: a bus
//! call must not happen on the frame loop. This one has a second reason, and it
//! is the sharper of the two — the applications this asks about are exactly the
//! ones that may have been *stopped*, and a stopped process never answers. The
//! connection is built with a [`PATIENCE`] of its own for that: without it a
//! single paused player that had already been put to sleep would wedge this
//! wherever the bus decided to give up, which for a session bus is minutes.

use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How often the bus and the sound server are asked.
///
/// Two seconds. The answer is not urgent in either direction: an application
/// that starts playing has [`crate::MEDIA_GRACE`] of cover before anything
/// could stop it, and one that has stopped playing has the same grace before
/// anything will. What this does decide is the width of the one race there is —
/// media started in the instant an application is being covered — and two
/// seconds is well inside the time it takes a game to map its first window.
const POLL: Duration = Duration::from_secs(2);

/// How long a bus call may take before the player is treated as having said
/// nothing.
///
/// The applications asked about here are the ones that may already have been
/// stopped, and a stopped process leaves a method call unanswered until the bus
/// gives up on it — which on a session bus is measured in minutes, if it
/// happens at all. Two seconds is far longer than a running player needs to
/// answer a property read on a local socket, and short enough that a stopped
/// one costs a single poll.
const PATIENCE: Duration = Duration::from_secs(2);

/// How long a player is given to have carried out a press before it is asked
/// what it is doing.
///
/// Short enough to be inside one frame of the guide's own animation, and long
/// enough for a local method call to have been delivered and acted on.
const SETTLE: Duration = Duration::from_millis(120);

/// The well-known name every media player on the bus takes, and what follows it
/// is the player.
const MPRIS: &str = "org.mpris.MediaPlayer2.";

/// Where a player keeps its object, which is the same for all of them.
const PLAYER_PATH: &str = "/org/mpris/MediaPlayer2";

/// The interface a player's transport is on.
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";

/// The interface a player says what it *is* on.
const MEDIA_PLAYER: &str = "org.mpris.MediaPlayer2";

/// One media player on the bus, as the shell knows it.
///
/// Several names rather than one because there is no single answer: see the
/// module documentation. Best first, which here means most specific to this
/// application — the desktop entry it named, then what the sound server calls
/// the program, then the name it took on the bus.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Player {
    /// Its bus name, which is the address the transport buttons act on. Not a
    /// name it is *matched* by — two copies of one browser are two bus names
    /// and one application.
    pub bus: String,
    pub names: Vec<String>,
    /// What the player says it is doing: `Playing`, and nothing else.
    ///
    /// Kept apart from [`Player::playing`] because the two answer different
    /// questions. This one is what the middle button's glyph is drawn from —
    /// what pressing it will do — and it is true of a player claiming to play
    /// into silence, which is exactly the case the other one exists to catch.
    pub claiming: bool,
    /// Claiming *and* audible: the reading the sleeper's exception is made on.
    /// See [`audible_of`].
    pub playing: bool,
    /// What is playing, for the card to print. Empty where the player says
    /// nothing, which a browser between two videos briefly does.
    pub title: String,
    /// Whether the player says there is anything to go back or forward to. A
    /// button for a thing the player cannot do is drawn as one that cannot be
    /// reached, the way the pointer tile is with no application in front.
    pub can_previous: bool,
    pub can_next: bool,
    /// How loud it is in the sound server, where it is audible at all.
    ///
    /// Read here rather than asked of the mixer because this listing is
    /// happening anyway and the mixer's rows are cleared whenever nobody is
    /// looking at them — see [`crate::system::Audible`]. It is what the guide's
    /// media bar shows until the menu opens and the mixer takes over, which is
    /// what keeps that bar from arriving after the card it belongs to.
    pub level: Option<crate::system::Level>,
}

impl Player {
    /// Add a name if it says anything and is not already there.
    fn also(&mut self, name: Option<String>) {
        let Some(name) = name else {
            return;
        };
        let name = name.trim().to_string();
        if !name.is_empty() && !self.names.contains(&name) {
            self.names.push(name);
        }
    }
}

/// One of the three things the guide's media card can ask a player to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Previous,
    PlayPause,
    Next,
}

impl Transport {
    /// The method on `org.mpris.MediaPlayer2.Player` that carries it.
    ///
    /// `PlayPause` rather than `Play` and `Pause`: the player knows which of
    /// the two it is in a position to do, and a shell that decided for itself
    /// would be deciding from a status it read up to two seconds ago.
    fn method(self) -> &'static str {
        match self {
            Transport::Previous => "Previous",
            Transport::PlayPause => "PlayPause",
            Transport::Next => "Next",
        }
    }
}

/// What the shell has asked the watch to do between two looks.
#[derive(Debug, Clone)]
enum Ask {
    Press { bus: String, what: Transport },
}

/// The watch, for as long as the session lasts.
///
/// Dropping it leaves the thread running to the end of the process, which is
/// the same bargain every other worker in this shell strikes: there is exactly
/// one of these and it lives as long as the shell does.
pub struct Watch {
    playing: Arc<Mutex<Vec<Player>>>,
    /// How a press reaches the thread. Bounded by nothing and never blocked
    /// on: the thread is waiting on this between polls, so a press is acted on
    /// at once rather than at the top of the next one — a transport button
    /// that answered up to two seconds later would read as one that had not
    /// been pressed at all.
    ask: mpsc::Sender<Ask>,
}

impl Watch {
    /// Open the session bus and start asking.
    ///
    /// `None` where there is no session bus to ask — a shell started from a
    /// tty with no bus, and the case a compositor running without a desktop
    /// around it can genuinely be in. That session keeps the old rule, where
    /// everything out of sight is stopped, and says so once rather than
    /// pretending to watch.
    pub fn start() -> Option<Watch> {
        let connection = match zbus::blocking::connection::Builder::session()
            .and_then(|builder| builder.method_timeout(PATIENCE).build())
        {
            Ok(connection) => connection,
            Err(err) => {
                tracing::info!(
                    %err,
                    "no session bus: an application out of sight is stopped even if it is playing"
                );
                return None;
            }
        };

        let playing = Arc::new(Mutex::new(Vec::new()));
        let shared = Arc::clone(&playing);
        let (ask, asked) = mpsc::channel();
        std::thread::Builder::new()
            .name("lxb-playing".to_string())
            .spawn(move || watch(&connection, &shared, &asked))
            .ok()?;
        tracing::info!("watching the bus for anything the session is playing");
        Some(Watch { playing, ask })
    }

    /// Ask the player at `bus` to do one of the three things.
    ///
    /// Best-effort and deliberately unanswered: the player may have gone
    /// between the frame that drew the button and the press that landed on it,
    /// which is not an error and is what the next look is for. What the user
    /// sees either way is the card telling them what is playing now.
    pub fn press(&self, bus: &str, what: Transport) {
        if self
            .ask
            .send(Ask::Press {
                bus: bus.to_string(),
                what,
            })
            .is_err()
        {
            tracing::debug!(bus, ?what, "nothing is watching the bus any more");
        }
    }

    /// Everything playing at the last look.
    ///
    /// Read rather than drained: this is a *state*, not a queue of things that
    /// happened, and the frame that asks wants the whole of it. A poisoned lock
    /// answers with what it holds — the thread panicking must not take the
    /// shell with it, and the worst this can then be is one stale answer, which
    /// the grace in [`crate::Shell::sync_media_awake`] already tolerates.
    pub fn playing(&self) -> Vec<Player> {
        match self.playing.lock() {
            Ok(playing) => playing.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}

/// Act on anything asked for, look, publish, wait, repeat.
///
/// The wait is on the channel rather than on the clock, so the two things this
/// thread does happen at the speeds they each want: a look every [`POLL`],
/// which nobody is waiting for, and a press the moment it is made, which
/// somebody is.
fn watch(
    connection: &zbus::blocking::Connection,
    shared: &Mutex<Vec<Player>>,
    asked: &mpsc::Receiver<Ask>,
) {
    // What each player calls itself, kept between polls: an application's name
    // does not change while it is running, and reading it back every two
    // seconds would be two bus calls per player per poll for an answer that is
    // already known. Everything else about a player — what it is doing and
    // what it is playing — is read every time, because all of it moves.
    let mut named: HashMap<String, Vec<String>> = HashMap::new();
    loop {
        match asked.recv_timeout(POLL) {
            Ok(ask) => {
                act(connection, &ask);
                // Give the player a moment to have done it before asking what
                // it is doing. Without this the look below reads the state the
                // press was about to change, and the card shows the old answer
                // until the next poll — a play button that visibly does
                // nothing for two seconds.
                std::thread::sleep(SETTLE);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            // The shell has gone. Nothing left to watch for.
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
        let found = look(connection, &mut named);
        match shared.lock() {
            Ok(mut playing) => {
                if *playing != found {
                    tracing::debug!(
                        players = ?found.iter().map(|p| (p.names.first(), p.playing)).collect::<Vec<_>>(),
                        "what the session is playing changed"
                    );
                    *playing = found;
                }
            }
            Err(poisoned) => *poisoned.into_inner() = found,
        }
    }
}

/// Carry out one press.
fn act(connection: &zbus::blocking::Connection, ask: &Ask) {
    let Ask::Press { bus, what } = ask;
    let Ok(proxy) = zbus::blocking::Proxy::new(connection, bus.as_str(), PLAYER_PATH, PLAYER)
    else {
        return;
    };
    match proxy.call::<_, _, ()>(what.method(), &()) {
        Ok(()) => tracing::debug!(%bus, ?what, "the player was asked"),
        // A player that has gone, or one that will not do this. Neither is
        // worth more than a line: the card is redrawn from whatever the next
        // look finds either way.
        Err(err) => tracing::debug!(%bus, ?what, %err, "the player would not"),
    }
}

/// One look at the bus, and — only if it was worth it — at the sound server.
///
/// Every player is reported, not only the ones playing: the guide's card has to
/// go on saying what is in it while the thing is paused, and a player that has
/// just dropped out of `playing` is exactly the one the user is reaching for.
fn look(
    connection: &zbus::blocking::Connection,
    named: &mut HashMap<String, Vec<String>>,
) -> Vec<Player> {
    let Ok(bus) = zbus::blocking::fdo::DBusProxy::new(connection) else {
        return Vec::new();
    };
    let Ok(names) = bus.list_names() else {
        return Vec::new();
    };
    let buses: Vec<String> = names
        .into_iter()
        .map(|name| name.as_str().to_string())
        .filter(|name| name.starts_with(MPRIS))
        .collect();

    // A player that has left the bus takes its names with it, or the map grows
    // by one entry per browser tab for the length of the session.
    named.retain(|name, _| buses.contains(name));

    let mut found: Vec<Player> = buses
        .into_iter()
        .map(|bus_name| {
            let names = named
                .entry(bus_name.clone())
                .or_insert_with(|| names_of(connection, &bus_name))
                .clone();
            let mut player = read_player(connection, &bus_name);
            player.bus = bus_name;
            player.names = names;
            player
        })
        .collect();

    if !found.iter().any(|player| player.claiming) {
        // The whole point of asking the bus first: a session with nothing
        // claiming to play never runs the sound server's listing at all, which
        // is the expensive half and the one that starts a process.
        return found;
    }

    // And the corroboration. Every name here is one the sound server is
    // playing something under right now.
    audible_of(&mut found, &crate::system::audible_applications());
    found
}

/// Mark the players that are not only claiming to play but can be heard doing
/// it, and give each the name it was heard under.
///
/// The second half of the rule, kept apart from the bus and the sound server so
/// that it can be read and tested as what it is: an agreement between two lists
/// of names that spell the same applications differently.
///
/// A player that is heard keeps the name it was heard under, because that name
/// came off the process itself and is the closest of the three to what the
/// window in front of that process will call itself.
fn audible_of(players: &mut [Player], audible: &[crate::system::Audible]) {
    for player in players.iter_mut() {
        if !player.claiming {
            continue;
        }
        let heard: Vec<&crate::system::Audible> = audible
            .iter()
            .filter(|said| {
                said.names.iter().any(|said| {
                    player
                        .names
                        .iter()
                        .any(|name| crate::apps::same_application(name, said))
                })
            })
            .collect();
        player.playing = !heard.is_empty();
        // The loudest of them, which is the reading a mixer row takes and so
        // the one the bar has to agree with.
        player.level = heard
            .iter()
            .map(|said| said.level)
            .reduce(|a, b| crate::system::Level {
                value: a.value.max(b.value),
                muted: a.muted && b.muted,
            });
        for said in heard {
            for name in said.names.clone() {
                player.also(Some(name));
            }
        }
    }
}

/// What one player is doing and playing, asked fresh every look.
fn read_player(connection: &zbus::blocking::Connection, bus: &str) -> Player {
    let mut player = Player::default();
    let Ok(proxy) = zbus::blocking::Proxy::new(connection, bus, PLAYER_PATH, PLAYER) else {
        return player;
    };
    player.claiming = says_it_is_playing(&proxy);
    player.title = title_of(&proxy);
    // A player that will not say is taken at its word only in the direction
    // that offers less: an unanswered `CanGoNext` draws a button that cannot be
    // pressed, which is recoverable, where assuming it draws one that does
    // nothing.
    player.can_previous = proxy.get_property::<bool>("CanGoPrevious").unwrap_or(false);
    player.can_next = proxy.get_property::<bool>("CanGoNext").unwrap_or(false);
    player
}

/// What the player says is playing: `xesam:title` out of its metadata.
///
/// The title alone. The card has one line for this under three buttons, and
/// everything else the metadata carries — the artist, the album, the art, the
/// URL that says whether this is YouTube or Twitch — is either too long for
/// that line or is not what somebody glancing at it is asking.
fn title_of(proxy: &zbus::blocking::Proxy<'_>) -> String {
    use zbus::zvariant::OwnedValue;
    let Ok(metadata) = proxy.get_property::<HashMap<String, OwnedValue>>("Metadata") else {
        return String::new();
    };
    metadata
        .get("xesam:title")
        .and_then(|value| <&str>::try_from(value).ok())
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// Whether one player says it is playing, right now.
///
/// Anything other than a plain `Playing` is not: `Paused`, `Stopped`, a player
/// that answers with something this does not understand, and a player that does
/// not answer at all. The last is the one worth naming — see [`PATIENCE`] — and
/// it is deliberately read as silence rather than retried, because a player
/// that cannot answer is either gone or already stopped, and neither is
/// something to hold a process tree awake for.
fn says_it_is_playing(proxy: &zbus::blocking::Proxy<'_>) -> bool {
    match proxy.get_property::<String>("PlaybackStatus") {
        Ok(status) => status == "Playing",
        Err(err) => {
            tracing::trace!(%err, "a media player did not say what it was doing");
            false
        }
    }
}

/// Every name one player might be recognised by, asked once.
fn names_of(connection: &zbus::blocking::Connection, name: &str) -> Vec<String> {
    let mut player = Player::default();
    if let Ok(proxy) = zbus::blocking::Proxy::new(connection, name, PLAYER_PATH, MEDIA_PLAYER) {
        // The desktop entry first: it is the one name a player gives that is
        // meant to be matched to something, and it is the same string the
        // catalogue of installed applications is keyed on.
        player.also(proxy.get_property::<String>("DesktopEntry").ok());
        player.also(proxy.get_property::<String>("Identity").ok());
    }
    // And what it took on the bus, which is all there is for a player that
    // answers neither. `org.mpris.MediaPlayer2.firefox.instance_1_145` is
    // Firefox: everything from the second dot on is the bus keeping two copies
    // of one program apart, and none of it is the program.
    player.also(bus_name_program(name));
    player.names
}

/// The program out of a player's bus name.
fn bus_name_program(name: &str) -> Option<String> {
    let tail = name.strip_prefix(MPRIS)?;
    let program = tail.split('.').next().unwrap_or(tail);
    (!program.is_empty()).then(|| program.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_players_program_is_read_out_of_its_bus_name() {
        assert_eq!(
            bus_name_program("org.mpris.MediaPlayer2.spotify").as_deref(),
            Some("spotify")
        );
        // The instance suffix is the bus keeping two copies apart, not part of
        // the program's name.
        assert_eq!(
            bus_name_program("org.mpris.MediaPlayer2.firefox.instance_1_145").as_deref(),
            Some("firefox")
        );
        assert_eq!(
            bus_name_program("org.mpris.MediaPlayer2.vlc.instance7花").as_deref(),
            Some("vlc")
        );
    }

    #[test]
    fn a_name_that_is_not_a_player_is_not_one() {
        assert_eq!(bus_name_program("org.freedesktop.Notifications"), None);
        // The bare interface name with nothing after it names no program.
        assert_eq!(bus_name_program("org.mpris.MediaPlayer2."), None);
    }

    /// A player that says it is playing, which is the only kind
    /// [`audible_of`] has anything to decide about.
    fn player(names: &[&str]) -> Player {
        Player {
            names: names.iter().map(|name| name.to_string()).collect(),
            claiming: true,
            ..Player::default()
        }
    }

    /// What `audible_of` made of one player, for a test that asks about one.
    fn heard(mut players: Vec<Player>, audible: &[crate::system::Audible]) -> Vec<Player> {
        audible_of(&mut players, audible);
        players.retain(|player| player.playing);
        players
    }

    fn audible(names: &[&str]) -> Vec<crate::system::Audible> {
        names
            .iter()
            .map(|name| crate::system::Audible {
                names: vec![name.to_string()],
                level: crate::system::Level {
                    value: 0.5,
                    muted: false,
                },
            })
            .collect()
    }

    /// The whole of the second half of the rule. A player saying `Playing` is
    /// not enough on its own: a tab closed without the page saying so, or a
    /// client that lost its decoder, leaves a player claiming to play into
    /// nothing, and that must not hold a process tree awake all evening.
    #[test]
    fn a_player_nothing_can_be_heard_from_is_not_playing() {
        let kept = heard(vec![player(&["spotify"])], &audible(&["zen", "Zen"]));
        assert!(kept.is_empty());
    }

    /// And the case this exists for. The bus spells a flatpak browser one way,
    /// the sound server another, and the window a third; the reverse-DNS rule
    /// in [`crate::apps::same_application`] is what makes the three one
    /// application.
    #[test]
    fn a_player_is_heard_under_whatever_the_sound_server_calls_it() {
        let kept = heard(
            vec![player(&["app.zen_browser.zen", "firefox"])],
            &audible(&["zen", "Zen"]),
        );
        assert_eq!(kept.len(), 1);
        // Both spellings kept, and the ones off the process at the end: a
        // window may go by any of them.
        assert!(kept[0].names.contains(&"zen".to_string()));
        assert!(kept[0].names.contains(&"Zen".to_string()));
        assert!(kept[0].names.contains(&"app.zen_browser.zen".to_string()));
    }

    /// A game is the other side of the same test: it is audible, and it never
    /// appears on the bus, so it never reaches this function at all — and if
    /// something else on the bus happens to be playing, the game's sound is not
    /// what spares it.
    #[test]
    fn being_audible_is_not_enough_to_be_a_player() {
        let kept = heard(
            vec![player(&["spotify"])],
            &audible(&["TEKKEN 8", "spotify"]),
        );
        assert_eq!(kept.len(), 1);
        assert!(!kept[0].names.iter().any(|name| name.starts_with("TEKKEN")));
    }

    /// A player that never said it was playing is not asked about at all: its
    /// stream may well be running — a game's is — and that is the whole point.
    #[test]
    fn a_player_that_says_nothing_is_never_marked_playing() {
        let mut quiet = vec![Player {
            names: vec!["spotify".to_string()],
            ..Player::default()
        }];
        audible_of(&mut quiet, &audible(&["spotify"]));
        assert!(!quiet[0].playing);
    }

    #[test]
    fn a_player_collects_every_name_it_is_known_by_once() {
        let mut player = Player::default();
        player.also(Some("zen".to_string()));
        player.also(Some("Zen".to_string()));
        // Said twice, kept once: the bus and the sound server routinely agree,
        // and a name repeated is a name matched twice for nothing.
        player.also(Some("zen".to_string()));
        // And nothing at all is not a name.
        player.also(Some("   ".to_string()));
        player.also(None);
        assert_eq!(player.names, vec!["zen".to_string(), "Zen".to_string()]);
    }
}
