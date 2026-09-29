//! The machine's side of power: the login manager, the power-profiles daemon,
//! and the two session-bus interfaces programs ask to keep the machine awake
//! through.
//!
//! Everything [`crate::idle`] decides is carried out or learned here, over
//! D-Bus, on threads of this module's own — a bus call must be answered, and a
//! signal read, whether or not the shell is drawing a frame. The shell reads
//! what arrived once a pass of its loop, exactly as it reads the notification
//! daemon's news. Nothing here decides anything.
//!
//! ## On the system bus
//!
//! * **logind's `PrepareForSleep`**, both edges. The shell needs the second
//!   one: a machine that went to sleep with its screens switched off wakes up
//!   with them still off, and nothing but this says it has woken.
//! * **A `handle-power-key` inhibitor**, taken once and held for the session,
//!   so a press of the button is the shell's to answer rather than logind's
//!   `HandlePowerKey` — which on most machines is *power off*, the one thing a
//!   console's button must never do on a single press. Only in a session on
//!   the machine's own displays: a nested session sharing the developer's
//!   login manager would otherwise take the button off their desktop.
//! * **Suspend**, asked of logind directly and only after `BlockInhibited`
//!   says nothing holds sleep. A suspend that would have to override somebody's
//!   inhibitor is one logind answers with a password prompt, and a machine
//!   that asks for a password because it was left alone is a machine nobody
//!   left alone twice.
//! * **Sleep by hand** — the power menu, the power button — which is the other
//!   way round: somebody asked, so what this account's own programs hold is
//!   set aside, through [`crate::machine_power::ask_root_to_sleep`] where
//!   logind would otherwise want a password for it. A lock of the system's is
//!   still honoured, and the person is told why the machine stayed awake.
//! * **Whether the machine can hibernate**, asked once, for the power
//!   button's Hibernate answer.
//! * **power-profiles-daemon**, under either of the names it has had, for
//!   Settings > Power > Power mode: what it offers, which is in force, and a
//!   way to change it. The daemon remembers the choice; this shell does not.
//!
//! ## On the session bus
//!
//! `org.freedesktop.ScreenSaver` and `org.freedesktop.PowerManagement.Inhibit`,
//! the two interfaces a program that is not speaking Wayland asks through: a
//! film player under Xwayland, a browser, most of what SDL builds. The first
//! holds the screen, the second holds sleep. Each is taken without replacing
//! whatever already answers it — a LineXinBar run inside somebody's desktop
//! for testing leaves that desktop's screen saver alone — and an inhibition is
//! dropped by itself when the program that asked for it leaves the bus, which
//! is how most of them "release" one.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use zbus::zvariant::{OwnedValue, Value};

const LOGIND: &str = "org.freedesktop.login1";
const LOGIND_PATH: &str = "/org/freedesktop/login1";
const LOGIND_MANAGER: &str = "org.freedesktop.login1.Manager";

/// power-profiles-daemon's name since 0.20, and the one it had before. Both are
/// still in use by the distributions this shell ships on.
const PROFILES: [(&str, &str, &str); 2] = [
    (
        "org.freedesktop.UPower.PowerProfiles",
        "/org/freedesktop/UPower/PowerProfiles",
        "org.freedesktop.UPower.PowerProfiles",
    ),
    (
        "net.hadess.PowerProfiles",
        "/net/hadess/PowerProfiles",
        "net.hadess.PowerProfiles",
    ),
];

/// What the machine said, for the shell to pick up between frames.
#[derive(Debug)]
pub enum Event {
    /// logind is about to put the machine to sleep (`true`), or it has just
    /// woken up (`false`).
    Sleeping(bool),
    /// What the power-profiles daemon offers now, or `None` where there is no
    /// daemon.
    Profiles(Option<crate::settings::Profiles>),
    /// A sleep the shell asked for did not happen, and why — for the log.
    NotAsleep(String),
    /// A sleep somebody asked for did not happen, because a lock the system
    /// holds is in force — something the person is told about.
    HeldByTheSystem,
    /// Whether the machine can hibernate.
    CanHibernate(bool),
}

/// How a sleep somebody asked for sleeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sleep {
    Suspend,
    Hibernate,
}

enum Command {
    /// Put the machine to sleep, holding the updater's permit until logind has
    /// answered — see [`crate::idle`].
    Suspend(Option<lxb_updates::service::PowerPermit>),
    /// Put the machine to sleep because somebody asked.
    SleepNow(Sleep, Option<lxb_updates::service::PowerPermit>),
    SetProfile(&'static str),
}

/// The handle the shell keeps.
pub struct PowerBus {
    commands: Option<Sender<Command>>,
    events: Receiver<Event>,
    inhibitions: Arc<Inhibitions>,
    /// Whether the shell holds the power button: the inhibitor was taken.
    button: Arc<AtomicBool>,
    /// Kept so the session-bus names stay taken for as long as the shell runs.
    _session: Option<zbus::blocking::Connection>,
}

impl PowerBus {
    /// Start the workers. `owns_the_machine` is whether this is a session on
    /// the machine's own displays, which is the only kind that may take the
    /// power button.
    pub fn start(owns_the_machine: bool) -> Self {
        let (event_tx, events) = mpsc::channel();
        let inhibitions = Arc::new(Inhibitions::default());
        let button = Arc::new(AtomicBool::new(false));
        let commands = match zbus::blocking::Connection::system() {
            Ok(system) => Some(start_system(system, event_tx, owns_the_machine, &button)),
            Err(err) => {
                tracing::info!(%err, "no system bus: the machine's power is not the shell's");
                None
            }
        };
        let session = serve_session(&inhibitions);
        PowerBus {
            commands,
            events,
            inhibitions,
            button,
            _session: session,
        }
    }

    /// Everything the machine has said since this was last asked.
    pub fn drain(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }

    /// Put the machine to sleep, unless something holds it off. Answered, if
    /// it is refused, with [`Event::NotAsleep`].
    pub fn suspend(&self, permit: Option<lxb_updates::service::PowerPermit>) {
        self.send(Command::Suspend(permit));
    }

    /// Put the machine to sleep because somebody asked, past whatever this
    /// account's own programs hold. Answered, if it is refused, with
    /// [`Event::HeldByTheSystem`] or [`Event::NotAsleep`].
    pub fn sleep_now(&self, how: Sleep, permit: Option<lxb_updates::service::PowerPermit>) {
        self.send(Command::SleepNow(how, permit));
    }

    /// Change the power mode to one of the daemon's names.
    pub fn set_profile(&self, name: &'static str) {
        self.send(Command::SetProfile(name));
    }

    /// Whether the power button's press is this shell's to answer.
    pub fn holds_the_button(&self) -> bool {
        self.button.load(Ordering::Acquire)
    }

    /// Whether any program on the session bus is holding the screen on.
    pub fn screen_held(&self) -> bool {
        self.inhibitions.held(What::Screen)
    }

    /// Whether any program on the session bus is holding sleep off.
    pub fn sleep_held(&self) -> bool {
        self.inhibitions.held(What::Sleep)
    }

    /// Whether a program asked for the screen saver to be poked since this was
    /// last asked — `SimulateUserActivity`, which counts as somebody pressing
    /// something.
    pub fn poked(&self) -> bool {
        self.inhibitions.poked.swap(false, Ordering::AcqRel)
    }

    fn send(&self, command: Command) {
        match &self.commands {
            Some(commands) => {
                if commands.send(command).is_err() {
                    tracing::warn!("the power worker has gone");
                }
            }
            None => tracing::info!("no system bus to ask"),
        }
    }
}

// --- the system bus ---------------------------------------------------------

fn start_system(
    system: zbus::blocking::Connection,
    events: Sender<Event>,
    owns_the_machine: bool,
    button: &Arc<AtomicBool>,
) -> Sender<Command> {
    let (tx, rx) = mpsc::channel();

    // The two edges of sleep, on a thread of their own: the iterator blocks.
    {
        let system = system.clone();
        let events = events.clone();
        spawn("lxb-sleep-watch", move || watch_sleep(&system, &events));
    }

    // The power modes, read once and then again whenever the daemon says one
    // of its properties changed.
    {
        let system = system.clone();
        let events = events.clone();
        spawn("lxb-profiles", move || watch_profiles(&system, &events));
    }

    let button = Arc::clone(button);
    spawn("lxb-power-bus", move || {
        // Held for as long as this thread lives, which is as long as the shell
        // does. Dropping it gives the button back to logind.
        let _held = owns_the_machine
            .then(|| take_the_power_button(&system))
            .flatten();
        button.store(_held.is_some(), Ordering::Release);
        let can_hibernate = system
            .call_method(
                Some(LOGIND),
                LOGIND_PATH,
                Some(LOGIND_MANAGER),
                "CanHibernate",
                &(),
            )
            .ok()
            .and_then(|reply| reply.body().deserialize::<String>().ok());
        let _ = events.send(Event::CanHibernate(can_hibernate.as_deref() == Some("yes")));
        for command in rx {
            match command {
                Command::Suspend(permit) => suspend(&system, &events, permit),
                Command::SleepNow(how, permit) => {
                    sleep_now(&system, &events, how, permit, owns_the_machine)
                }
                Command::SetProfile(name) => set_profile(&system, &events, name),
            }
        }
    });
    tx
}

fn spawn(name: &str, body: impl FnOnce() + Send + 'static) {
    if let Err(err) = std::thread::Builder::new()
        .name(name.to_string())
        .spawn(body)
    {
        tracing::warn!(%err, name, "could not start a power worker");
    }
}

/// Ask logind for the power button. `None`, with the reason in the log, where
/// it will not hand it over — in which case logind goes on answering it and the
/// shell must not answer it as well.
fn take_the_power_button(system: &zbus::blocking::Connection) -> Option<zbus::zvariant::OwnedFd> {
    let reply = system.call_method(
        Some(LOGIND),
        LOGIND_PATH,
        Some(LOGIND_MANAGER),
        "Inhibit",
        &(
            "handle-power-key",
            "LineXinBar",
            "The session answers the power button itself",
            "block",
        ),
    );
    match reply.and_then(|reply| reply.body().deserialize::<zbus::zvariant::OwnedFd>()) {
        Ok(fd) => {
            tracing::info!("the power button is the session's to answer");
            Some(fd)
        }
        Err(err) => {
            tracing::info!(%err, "logind keeps the power button");
            None
        }
    }
}

fn watch_sleep(system: &zbus::blocking::Connection, events: &Sender<Event>) {
    let proxy = match zbus::blocking::Proxy::new(system, LOGIND, LOGIND_PATH, LOGIND_MANAGER) {
        Ok(proxy) => proxy,
        Err(err) => {
            tracing::info!(%err, "no login manager to hear sleep from");
            return;
        }
    };
    let signals = match proxy.receive_signal("PrepareForSleep") {
        Ok(signals) => signals,
        Err(err) => {
            tracing::info!(%err, "cannot hear the machine go to sleep");
            return;
        }
    };
    for message in signals {
        let Ok(going) = message.body().deserialize::<bool>() else {
            continue;
        };
        tracing::info!(going, "the machine is going to sleep, or has woken");
        if events.send(Event::Sleeping(going)).is_err() {
            return;
        }
    }
}

/// Put the machine to sleep if nothing holds sleep off.
///
/// `BlockInhibited` first, because logind would otherwise answer an inhibited
/// suspend with a password prompt — see the module's own notes. "idle" is
/// counted as well as "sleep": a program that has asked for the machine not to
/// go idle has asked for exactly what this is about to do.
fn suspend(
    system: &zbus::blocking::Connection,
    events: &Sender<Event>,
    permit: Option<lxb_updates::service::PowerPermit>,
) {
    let blocked = property(
        system,
        LOGIND,
        LOGIND_PATH,
        LOGIND_MANAGER,
        "BlockInhibited",
    )
    .and_then(|value| String::try_from(value).ok())
    .unwrap_or_default();
    if blocked
        .split(':')
        .any(|what| what == "sleep" || what == "idle")
    {
        let _ = events.send(Event::NotAsleep(format!("held by an inhibitor: {blocked}")));
        return;
    }
    tracing::info!("putting the machine to sleep");
    let result = system.call_method(
        Some(LOGIND),
        LOGIND_PATH,
        Some(LOGIND_MANAGER),
        "Suspend",
        &(false,),
    );
    // Held until logind has answered, which is after it has decided — the
    // updater cannot start a transaction between the check and the sleep.
    drop(permit);
    if let Err(err) = result {
        let _ = events.send(Event::NotAsleep(err.to_string()));
    }
}

/// Put the machine to sleep because somebody asked.
///
/// Nothing but a lock the machine itself holds may stop it. Where there is no
/// lock at all, logind is asked plainly — interactively, because a person is
/// there to answer a question it may have, such as another account being
/// signed in. Where every lock is this account's own programs' — a game that
/// keeps the machine awake while nobody touches it — they are set aside
/// through the root half, which logind would otherwise want an administrator's
/// password for. A lock of anybody else's is honoured, and the person is told.
fn sleep_now(
    system: &zbus::blocking::Connection,
    events: &Sender<Event>,
    how: Sleep,
    permit: Option<lxb_updates::service::PowerPermit>,
    owns_the_machine: bool,
) {
    if !owns_the_machine {
        tracing::info!(
            ?how,
            "a session inside another desktop does not put the machine to sleep"
        );
        return;
    }
    let locks = crate::machine_power::blocking_locks(system).unwrap_or_else(|err| {
        tracing::info!(%err, "the login manager's locks could not be listed");
        Vec::new()
    });
    let me = unsafe { libc::getuid() };
    if let Some(theirs) = locks.iter().find(|lock| lock.uid != me) {
        tracing::info!(?theirs, "sleep is held off by a lock of the system's");
        let _ = events.send(Event::HeldByTheSystem);
        return;
    }
    let (word, method) = match how {
        Sleep::Suspend => ("suspend", "Suspend"),
        Sleep::Hibernate => ("hibernate", "Hibernate"),
    };
    if locks.is_empty() {
        tracing::info!(?how, "putting the machine to sleep, as asked");
        let result = system.call_method(
            Some(LOGIND),
            LOGIND_PATH,
            Some(LOGIND_MANAGER),
            method,
            &(true,),
        );
        drop(permit);
        if let Err(err) = result {
            let _ = events.send(Event::NotAsleep(err.to_string()));
        }
        return;
    }
    tracing::info!(
        ?how,
        ?locks,
        "putting the machine to sleep, past this account's own locks"
    );
    let answered = crate::machine_power::ask_root_to_sleep(word);
    drop(permit);
    match answered {
        crate::machine_power::RootSleep::Done => {}
        crate::machine_power::RootSleep::HeldByTheSystem => {
            let _ = events.send(Event::HeldByTheSystem);
        }
        crate::machine_power::RootSleep::Failed(why) => {
            let _ = events.send(Event::NotAsleep(why));
        }
    }
}

fn watch_profiles(system: &zbus::blocking::Connection, events: &Sender<Event>) {
    let Some((name, path, interface)) = PROFILES.into_iter().find(|(name, path, interface)| {
        property(system, name, path, interface, "ActiveProfile").is_some()
    }) else {
        tracing::info!("no power-profiles daemon: the power mode is not offered");
        let _ = events.send(Event::Profiles(None));
        return;
    };
    tracing::info!(daemon = name, "the power modes come from here");
    let _ = events.send(Event::Profiles(read_profiles(
        system, name, path, interface,
    )));

    let proxy =
        match zbus::blocking::Proxy::new(system, name, path, "org.freedesktop.DBus.Properties") {
            Ok(proxy) => proxy,
            Err(err) => {
                tracing::info!(%err, "cannot hear the power mode change");
                return;
            }
        };
    let Ok(signals) = proxy.receive_signal("PropertiesChanged") else {
        return;
    };
    for _ in signals {
        let listing = read_profiles(system, name, path, interface);
        if events.send(Event::Profiles(listing)).is_err() {
            return;
        }
    }
}

fn read_profiles(
    system: &zbus::blocking::Connection,
    name: &str,
    path: &str,
    interface: &str,
) -> Option<crate::settings::Profiles> {
    let active = property(system, name, path, interface, "ActiveProfile")
        .and_then(|value| String::try_from(value).ok())?;
    let offered: Vec<HashMap<String, OwnedValue>> =
        property(system, name, path, interface, "Profiles")
            .and_then(|value| Vec::try_from(value).ok())
            .unwrap_or_default();
    let offered = offered
        .iter()
        .filter_map(|profile| profile.get("Profile"))
        .filter_map(|value| value.try_clone().ok())
        .filter_map(|value| String::try_from(value).ok())
        .map(|name| crate::settings::intern(&name))
        .collect();
    Some(crate::settings::Profiles {
        offered,
        active: Some(crate::settings::intern(&active)),
    })
}

fn set_profile(system: &zbus::blocking::Connection, events: &Sender<Event>, profile: &str) {
    for (name, path, interface) in PROFILES {
        let set = system.call_method(
            Some(name),
            path,
            Some("org.freedesktop.DBus.Properties"),
            "Set",
            &(interface, "ActiveProfile", Value::from(profile)),
        );
        match set {
            Ok(_) => {
                tracing::info!(profile, "the power mode");
                let _ = events.send(Event::Profiles(read_profiles(
                    system, name, path, interface,
                )));
                return;
            }
            Err(zbus::Error::MethodError(error, _, _))
                if error.as_str() == "org.freedesktop.DBus.Error.ServiceUnknown" =>
            {
                continue
            }
            Err(err) => {
                tracing::warn!(%err, profile, "the power mode could not be changed");
                return;
            }
        }
    }
}

/// One property, read fresh rather than out of a proxy's cache: the ones read
/// here change underneath this process, and a cached "nothing inhibits sleep"
/// would be the worst stale answer this module could give.
fn property(
    system: &zbus::blocking::Connection,
    name: &str,
    path: &str,
    interface: &str,
    property: &str,
) -> Option<OwnedValue> {
    system
        .call_method(
            Some(name),
            path,
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &(interface, property),
        )
        .ok()?
        .body()
        .deserialize::<OwnedValue>()
        .ok()
}

// --- the session bus --------------------------------------------------------

/// What an inhibition holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum What {
    /// The screen: `org.freedesktop.ScreenSaver`.
    Screen,
    /// Sleep: `org.freedesktop.PowerManagement.Inhibit`.
    Sleep,
}

#[derive(Debug)]
struct Held {
    cookie: u32,
    /// The unique bus name of the program that asked, which is what goes away
    /// when it does.
    owner: String,
    what: What,
}

#[derive(Debug, Default)]
struct Inhibitions {
    held: Mutex<Vec<Held>>,
    next: Mutex<u32>,
    poked: AtomicBool,
}

impl Inhibitions {
    fn inhibit(&self, owner: String, what: What, application: &str, reason: &str) -> u32 {
        let cookie = {
            let mut next = self.next.lock().unwrap();
            *next = next.wrapping_add(1).max(1);
            *next
        };
        tracing::info!(
            application,
            reason,
            ?what,
            cookie,
            "a program asked to stay awake"
        );
        self.held.lock().unwrap().push(Held {
            cookie,
            owner,
            what,
        });
        cookie
    }

    fn release(&self, cookie: u32) {
        self.held
            .lock()
            .unwrap()
            .retain(|held| held.cookie != cookie);
        tracing::info!(cookie, "a program no longer asks to stay awake");
    }

    fn forget_owner(&self, owner: &str) {
        let mut held = self.held.lock().unwrap();
        let before = held.len();
        held.retain(|held| held.owner != owner);
        if held.len() != before {
            tracing::info!(owner, "a program that asked to stay awake has left");
        }
    }

    fn held(&self, what: What) -> bool {
        self.held
            .lock()
            .unwrap()
            .iter()
            .any(|held| held.what == what)
    }
}

/// The owner of a call, by its unique name.
fn sender(header: &zbus::message::Header<'_>) -> String {
    header
        .sender()
        .map(|name| name.to_string())
        .unwrap_or_default()
}

struct ScreenSaver {
    shared: Arc<Inhibitions>,
}

#[zbus::interface(name = "org.freedesktop.ScreenSaver")]
impl ScreenSaver {
    fn inhibit(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        application_name: String,
        reason_for_inhibit: String,
    ) -> u32 {
        self.shared.inhibit(
            sender(&header),
            What::Screen,
            &application_name,
            &reason_for_inhibit,
        )
    }

    fn un_inhibit(&self, cookie: u32) {
        self.shared.release(cookie);
    }

    /// There is no screen saver to be active: the screen goes dark instead.
    fn get_active(&self) -> bool {
        false
    }

    fn get_active_time(&self) -> u32 {
        0
    }

    fn simulate_user_activity(&self) {
        self.shared.poked.store(true, Ordering::Release);
    }
}

struct PowerManagement {
    shared: Arc<Inhibitions>,
}

#[zbus::interface(name = "org.freedesktop.PowerManagement.Inhibit")]
impl PowerManagement {
    fn inhibit(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        application: String,
        reason: String,
    ) -> u32 {
        self.shared
            .inhibit(sender(&header), What::Sleep, &application, &reason)
    }

    fn un_inhibit(&self, cookie: u32) {
        self.shared.release(cookie);
    }

    fn has_inhibit(&self) -> bool {
        self.shared.held(What::Sleep)
    }
}

/// Serve both interfaces on the session bus, and take each name that nobody
/// else already answers to.
fn serve_session(inhibitions: &Arc<Inhibitions>) -> Option<zbus::blocking::Connection> {
    let built = zbus::blocking::connection::Builder::session()
        .and_then(|builder| {
            builder.serve_at(
                "/org/freedesktop/ScreenSaver",
                ScreenSaver {
                    shared: Arc::clone(inhibitions),
                },
            )
        })
        // The path KDE answers at, which some programs ask on instead.
        .and_then(|builder| {
            builder.serve_at(
                "/ScreenSaver",
                ScreenSaver {
                    shared: Arc::clone(inhibitions),
                },
            )
        })
        .and_then(|builder| {
            builder.serve_at(
                "/org/freedesktop/PowerManagement/Inhibit",
                PowerManagement {
                    shared: Arc::clone(inhibitions),
                },
            )
        })
        .and_then(|builder| builder.build());
    let session = match built {
        Ok(session) => session,
        Err(err) => {
            tracing::info!(%err, "no session bus: programs cannot ask the screen to stay on");
            return None;
        }
    };
    let mut taken = false;
    for name in [
        "org.freedesktop.ScreenSaver",
        "org.freedesktop.PowerManagement",
    ] {
        match session.request_name_with_flags(name, zbus::fdo::RequestNameFlags::DoNotQueue.into())
        {
            Ok(zbus::fdo::RequestNameReply::PrimaryOwner)
            | Ok(zbus::fdo::RequestNameReply::AlreadyOwner) => {
                tracing::info!("serving {name} for this session");
                taken = true;
            }
            Ok(reply) => tracing::info!(?reply, "{name} is somebody else's"),
            Err(err) => tracing::info!(%err, "{name} is somebody else's"),
        }
    }
    if !taken {
        return None;
    }

    // And drop whatever a program held when it leaves the bus, which is how
    // most of them let go.
    let watched = session.clone();
    let shared = Arc::clone(inhibitions);
    spawn("lxb-inhibit-watch", move || {
        let Ok(bus) = zbus::blocking::fdo::DBusProxy::new(&watched) else {
            return;
        };
        let Ok(changes) = bus.receive_name_owner_changed() else {
            return;
        };
        for change in changes {
            let Ok(args) = change.args() else {
                continue;
            };
            if args.new_owner().is_none() {
                shared.forget_owner(args.name().as_str());
            }
        }
    });
    Some(session)
}

/// How long the shell waits between two suspend attempts that were refused,
/// before trying again while still idle.
pub const RETRY_SLEEP: Duration = Duration::from_secs(60);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_inhibition_is_held_until_released_or_its_owner_leaves() {
        let inhibitions = Inhibitions::default();
        assert!(!inhibitions.held(What::Screen));
        let film = inhibitions.inhibit(":1.7".into(), What::Screen, "mpv", "playing");
        let copy = inhibitions.inhibit(":1.9".into(), What::Sleep, "rsync", "copying");
        assert_ne!(film, copy);
        assert!(inhibitions.held(What::Screen) && inhibitions.held(What::Sleep));

        inhibitions.release(film);
        assert!(!inhibitions.held(What::Screen));
        assert!(inhibitions.held(What::Sleep));

        inhibitions.forget_owner(":1.9");
        assert!(!inhibitions.held(What::Sleep));
    }

    #[test]
    fn a_cookie_is_never_zero() {
        let inhibitions = Inhibitions::default();
        *inhibitions.next.lock().unwrap() = u32::MAX;
        assert_ne!(inhibitions.inhibit(String::new(), What::Screen, "", ""), 0);
    }
}
