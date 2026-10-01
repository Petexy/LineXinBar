//! What is left in this machine's battery, for the mark beside the clock.
//!
//! The one thing below the desktop this shell reaches for that has no daemon in
//! front of it, and deliberately. UPower is what a desktop environment would
//! ask, and it is a service that may not be installed and may not be running;
//! what it reads is `/sys/class/power_supply`, which is the kernel and is
//! always there. A console shell that showed no battery on a laptop because a
//! daemon was missing would be wrong about the hardware, so this reads the
//! hardware. It is the same bargain [`crate::system`] makes about the backlight
//! and the opposite of the one [`crate::network`] is forced into — see that
//! module's header for why joining a wireless network leaves no such choice.
//!
//! ## What is a battery, and what only calls itself one
//!
//! Everything the machine is powered by or powers is listed in that directory
//! together: the mains brick, the battery under the keyboard, and the cell in
//! the wireless mouse on the desk beside it. The last of those is a battery by
//! `type` and is emphatically not this machine's — a desktop whose corner
//! quietly reported the charge of a Logitech mouse would be worse than one that
//! showed nothing. The kernel says which is which in `scope`: `Device` is a
//! peripheral's own cell, and anything else — including the file being absent,
//! which is what most laptop drivers do — is the system's. See [`is_system`].
//!
//! ## On the worker thread, for a reason that is not the usual one
//!
//! Reading four small files cannot be compared to a pass over NetworkManager,
//! and on most machines this would be cheap enough to do on the frame loop. The
//! trouble is which files: on a laptop, `capacity` and `status` are answered by
//! the embedded controller over ACPI, and a slow controller turns a read into
//! tens of milliseconds of blocking. That is frames, and it would be frames
//! dropped on the machines this feature exists for and on no others. So the
//! shell reads the last answer and the worker keeps it true, on the terms the
//! corner's wireless mark is kept true: only while the corner is on screen.
//!
//! ## Told, and not only asked
//!
//! A clock alone was not enough, and a Steam Deck is where that showed: the
//! charger went in and the corner went on drawing a battery emptying for up to
//! twenty seconds, until the next tick — or until the user left for another
//! application and came back, which is the other thing that reads it. A cable
//! going in is exactly the moment somebody looks at the corner to see whether
//! it took.
//!
//! The kernel says so the instant it happens: every supply in that directory
//! sends a `change` uevent when it changes, on a netlink socket anyone may
//! listen to. So a second thread listens — see [`listen_for_supplies`] — and
//! each `power_supply` event is a read now, and then one a second for a while,
//! because the event is not the end of it: see [`SETTLING`]. The clock stays,
//! for the machines whose drivers do not send them and for the per cent that
//! moves on its own.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

/// Where the kernel lists everything that supplies this machine with power.
///
/// Discovered from here at runtime and never named further in: which batteries
/// a machine has, and what they are called, is the machine's business. `BAT0`
/// appears nowhere in this shell.
const SUPPLIES: &str = "/sys/class/power_supply";

/// How often the charge is read again while the corner is on screen.
///
/// Far slower than the wireless mark's five seconds, because a battery is far
/// slower than a radio: a laptop discharges a per cent in minutes, and the
/// mark has five drawings for the whole range. Twenty seconds is a level that
/// has really changed showing up long before the user could have noticed it,
/// and on a machine with a slow embedded controller it is that controller
/// asked three times a minute rather than sixty times a second.
const REFRESH: Duration = Duration::from_secs(20);

/// How often it is read while the corner is not on screen.
///
/// It used to be not at all, which was right while the mark was the only
/// reader. It no longer is: the idle policy sleeps the machine on a different
/// wait on the battery than on the mains, and the low-battery warnings have to
/// arrive in the middle of a game — which is exactly when the corner is not
/// showing. A minute is a charge that has really moved long before anybody
/// could be told about it too late, and a slow embedded controller asked once
/// a minute is not a cost anybody can measure.
const IN_THE_BACKGROUND: Duration = Duration::from_secs(60);

/// How long after the kernel reports a supply changing the battery is watched
/// closely, and how often in that time it is read.
///
/// The event is the mains coming up, not the battery starting to fill. Measured
/// on a Steam Deck whose charger comes through a USB-C hub: the kernel sent the
/// mains and the battery within a millisecond of each other and the battery
/// once more two and a half seconds later — both times saying `Not charging` —
/// and the battery turned to `Charging` some time after that with no event of
/// its own. A read on each event would have drawn a battery that was not
/// filling until the clock came round. So after an event the battery is read
/// every second for half a minute, which follows the charger however long it
/// takes to negotiate, at a cost of thirty small reads per cable.
const SETTLING: Duration = Duration::from_secs(30);
const SETTLING_EVERY: Duration = Duration::from_secs(1);

/// What is in the battery, as the worker last read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Charge {
    /// How full it is, 0 to 100.
    pub percent: u8,
    /// Whether it is filling. Sitting on the mains at full is *not* this: the
    /// kernel says `Full` or `Not charging` there, and what the corner should
    /// show then is a full battery rather than one that is forever filling.
    pub charging: bool,
}

/// Which of the five drawings a charge is shown with.
///
/// Five, for the reason the wireless mark has three: what the corner has to say
/// is which picture the battery is, not a number. The number is the setting
/// under Appearance, and it is off unless somebody asked for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Empty,
    Low,
    Half,
    High,
    Full,
}

/// Where one drawing gives way to the next.
///
/// Not even fifths. The two that matter are at the bottom: a battery under a
/// tenth is one the user has minutes of, and it gets a drawing of its own that
/// is plainly emptier than the one above it. The top three are the wide middle
/// of the range, where the difference between 62 and 71 per cent is nothing
/// anybody acts on.
const LOW_AT: u8 = 10;
const HALF_AT: u8 = 35;
const HIGH_AT: u8 = 60;
const FULL_AT: u8 = 85;

impl Level {
    /// Which drawing a reading falls in.
    ///
    /// Plain edges, with none of the hysteresis [`crate::network::band_of`]
    /// guards its bands with, and the difference is in what is being read. A
    /// radio reports the same unchanged link as a number that wanders a point
    /// or two between readings, so a mark on a boundary would flicker. A
    /// battery does not wander: it walks one way and it walks slowly. And the
    /// number can be *on screen beside the mark* — a guarded edge would then be
    /// a drawing that disagreed with the digits next to it, which is a worse
    /// fault than the one the guard exists to prevent.
    pub fn of(percent: u8) -> Self {
        match percent {
            p if p >= FULL_AT => Level::Full,
            p if p >= HIGH_AT => Level::High,
            p if p >= HALF_AT => Level::Half,
            p if p >= LOW_AT => Level::Low,
            _ => Level::Empty,
        }
    }
}

/// The charge, and the worker that keeps it true.
pub struct Power {
    shared: Arc<Shared>,
}

struct Shared {
    state: Mutex<State>,
    /// Woken when the corner arrives. Between those the worker sleeps: a
    /// machine with a game on the screen has nowhere to show a battery.
    signal: Condvar,
    /// The directory the supplies are read out of.
    ///
    /// Held rather than reached for so that the tests — and
    /// `--debug-power-supply`, which is how this was ever looked at on a
    /// desktop — can point it at a tree they built themselves. Nothing else
    /// varies: what is read out of it is what would be read out of the
    /// kernel's.
    root: PathBuf,
}

#[derive(Default)]
struct State {
    /// What is in the battery, or `None` for a machine that has none — which
    /// is what keeps the mark, and the setting that would turn its number on,
    /// off the screen entirely.
    charge: Option<Charge>,
    /// Whether the machine is running on its battery: a battery of its own
    /// says it is discharging. Beside the charge rather than in it, because
    /// the mark does not draw it — a battery sitting at full on the mains and
    /// one slowly emptying on it can look the same — and the idle policy needs
    /// nothing else. See [`crate::idle`].
    on_battery: bool,
    /// Whether the start screen's corner is on screen, and so whether this is
    /// worth keeping true at all.
    corner: bool,
    dirty: bool,
    /// Until when the battery is being watched closely, after the kernel said
    /// a supply changed. See [`SETTLING`].
    settling_until: Option<Instant>,
    done: bool,
}

impl Power {
    /// Start looking, at the kernel's own directory.
    ///
    /// Nothing is read until the corner is on screen — see [`Power::watch`] —
    /// so this costs a thread and a sleeping condition variable on a session
    /// that spends its life inside a game.
    pub fn start() -> Self {
        Self::rooted(PathBuf::from(SUPPLIES))
    }

    /// The same, reading a directory named by the caller. See [`Shared::root`].
    pub fn rooted(root: PathBuf) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            signal: Condvar::new(),
            root,
        });
        let worker = Arc::clone(&shared);
        if let Err(err) = std::thread::Builder::new()
            .name("lxb-power".to_string())
            .spawn(move || Worker { shared: worker }.run())
        {
            tracing::warn!(?err, "no worker thread; the battery mark is off");
        }
        // Weak, so that the listener is not what keeps the worker's state
        // alive: when the shell lets go of this, the listener notices within a
        // second and goes too.
        let listener = Arc::downgrade(&shared);
        if let Err(err) = std::thread::Builder::new()
            .name("lxb-power-events".to_string())
            .spawn(move || listen_for_supplies(listener))
        {
            tracing::warn!(
                ?err,
                "no uevent thread; the battery is read on its clock alone"
            );
        }
        Self { shared }
    }

    /// What is in the battery, as the worker last found it, or `None` for a
    /// machine with none.
    ///
    /// Copied out whole rather than counted the way a network listing is:
    /// there is nothing here to copy. It is two small numbers, and the shell
    /// compares them against what it drew last frame for the cost of the
    /// comparison.
    pub fn charge(&self) -> Option<Charge> {
        self.held().charge
    }

    /// Whether the machine is running on its battery, as last read.
    pub fn on_battery(&self) -> bool {
        self.held().on_battery
    }

    /// Read again now rather than at the next tick — the machine has just
    /// woken up, and the battery it woke with is not the one it went to sleep
    /// with.
    pub fn look_again(&self) {
        let mut state = self.held();
        state.dirty = true;
        self.shared.signal.notify_one();
    }

    /// Say whether the start screen's corner is on screen.
    ///
    /// The counterpart of [`crate::network::Net::watch_signal`], and on the
    /// same terms and for the same reason: the corner is showing whenever no
    /// application covers the start screen, and a machine that has just come
    /// back out of a two-hour game has a battery that is not the one it went
    /// in with. Which is why arriving is what wakes the worker rather than
    /// merely letting it start ticking.
    pub fn watch(&self, corner: bool) {
        let mut state = self.held();
        if state.corner == corner {
            return;
        }
        state.corner = corner;
        if corner {
            state.dirty = true;
            self.shared.signal.notify_one();
        }
    }

    fn held(&self) -> std::sync::MutexGuard<'_, State> {
        self.shared.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Drop for Power {
    fn drop(&mut self) {
        self.held().done = true;
        self.shared.signal.notify_all();
    }
}

struct Worker {
    shared: Arc<Shared>,
}

impl Worker {
    fn run(self) {
        loop {
            {
                let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
                if state.done {
                    return;
                }
                state.dirty = false;
                if state
                    .settling_until
                    .is_some_and(|until| until <= Instant::now())
                {
                    state.settling_until = None;
                }
            }
            let charge = read_charge(&self.shared.root);
            let on_battery = charge.is_some() && discharging(&self.shared.root);
            {
                let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
                // Said when the battery starts or stops filling, which is the
                // one change of reading worth a line: it is what somebody
                // plugging a cable in is looking for, and the journal is where
                // "it did not show" gets checked.
                let was = state.charge.map(|charge| charge.charging);
                let now = charge.map(|charge| charge.charging);
                if was.is_some() && was != now {
                    tracing::info!(
                        charging = now,
                        percent = charge.map(|charge| charge.percent),
                        "the battery's charging changed"
                    );
                }
                state.charge = charge;
                state.on_battery = on_battery;
            }
            self.wait();
        }
    }

    /// Sleep until the corner arrives, or until it is time to look again.
    ///
    /// A machine with no battery sleeps on exactly the same terms as one with
    /// a flat battery, and goes on scanning at [`REFRESH`] while the corner
    /// shows. That is a directory listing every twenty seconds for a desktop
    /// that will never have one, which is nothing — and it is what lets a
    /// laptop that had its battery taken out grow the mark back when it is put
    /// in again, without a second mechanism for noticing.
    fn wait(&self) {
        let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.done || state.dirty {
            return;
        }
        let every = if state.corner {
            REFRESH
        } else {
            IN_THE_BACKGROUND
        };
        let every = until_the_next_read(every, state.settling_until, Instant::now());
        let _held = self.shared.signal.wait_timeout(state, every);
    }
}

/// How long the worker sleeps: its own clock, or [`SETTLING_EVERY`] while a
/// supply has just changed.
fn until_the_next_read(every: Duration, settling_until: Option<Instant>, now: Instant) -> Duration {
    match settling_until {
        Some(until) if until > now => every.min(SETTLING_EVERY),
        _ => every,
    }
}

/// A supply changed, as far as the kernel is concerned: read now, and closely
/// for a while after. See [`SETTLING`].
fn supply_changed(shared: &Shared) {
    let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
    state.dirty = true;
    state.settling_until = Some(Instant::now() + SETTLING);
    shared.signal.notify_one();
}

/// Listen for the kernel saying a power supply changed, for as long as the
/// shell holds the reading.
///
/// A netlink socket in the kernel's own uevent group, which any process may
/// open and which needs neither udev nor a daemon — the same bargain the rest
/// of this module makes. A receive timeout of a second is what lets the thread
/// notice the shell has let go of the reading; a socket that cannot be opened
/// leaves the battery on its clock, exactly as it was before this existed.
fn listen_for_supplies(shared: Weak<Shared>) {
    let Some(socket) = uevent_socket() else {
        tracing::info!("no kernel uevents here; the battery is read on its clock alone");
        return;
    };
    let mut message = vec![0u8; 16 * 1024];
    loop {
        // SAFETY: receiving into a buffer this function owns, of the length
        // given, from a socket it owns.
        let received = unsafe {
            libc::recv(
                socket.as_raw_fd(),
                message.as_mut_ptr().cast(),
                message.len(),
                0,
            )
        };
        let Some(held) = shared.upgrade() else {
            return;
        };
        if held.state.lock().unwrap_or_else(|e| e.into_inner()).done {
            return;
        }
        if received < 0 {
            match std::io::Error::last_os_error().raw_os_error() {
                // The timeout, and a signal: nothing arrived.
                Some(libc::EAGAIN) | Some(libc::EINTR) => continue,
                // More arrived than the socket could hold, so whatever was lost
                // may have been a supply: read, rather than guess it was not.
                Some(libc::ENOBUFS) => {
                    supply_changed(&held);
                    continue;
                }
                _ => {
                    tracing::warn!(
                        err = %std::io::Error::last_os_error(),
                        "the kernel's uevents stopped; the battery is read on its clock alone"
                    );
                    return;
                }
            }
        }
        if is_power_supply_event(&message[..received as usize]) {
            supply_changed(&held);
        }
    }
}

/// A netlink socket bound to the kernel's uevent group, with a receive timeout.
fn uevent_socket() -> Option<OwnedFd> {
    // SAFETY: plain socket creation; the descriptor is owned below.
    let raw = unsafe {
        libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_DGRAM | libc::SOCK_CLOEXEC,
            libc::NETLINK_KOBJECT_UEVENT,
        )
    };
    if raw < 0 {
        return None;
    }
    // SAFETY: `raw` is a fresh descriptor nothing else owns.
    let socket = unsafe { OwnedFd::from_raw_fd(raw) };

    // SAFETY: a zeroed `sockaddr_nl` is a valid value of it.
    let mut address: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
    address.nl_family = libc::AF_NETLINK as libc::sa_family_t;
    // Group 1 is the kernel's own announcements; group 2 is udev's
    // rebroadcast, which comes with a header of its own and is not needed.
    address.nl_groups = 1;
    // SAFETY: binding a socket this function owns to an address it built.
    let bound = unsafe {
        libc::bind(
            socket.as_raw_fd(),
            std::ptr::addr_of!(address).cast(),
            std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
        )
    };
    if bound < 0 {
        return None;
    }

    let timeout = libc::timeval {
        tv_sec: 1,
        tv_usec: 0,
    };
    // SAFETY: setting an option on a socket this function owns, from a value
    // that outlives the call.
    let set = unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            std::ptr::addr_of!(timeout).cast(),
            std::mem::size_of::<libc::timeval>() as libc::socklen_t,
        )
    };
    if set < 0 {
        return None;
    }
    Some(socket)
}

/// Whether one kernel uevent is about a power supply.
///
/// The message is `action@devpath` followed by `KEY=value` pairs, each ended
/// by a NUL. The subsystem is asked for by name rather than read off the path,
/// because the path names the device and the device can be called anything.
fn is_power_supply_event(message: &[u8]) -> bool {
    message
        .split(|byte| *byte == 0)
        .skip(1)
        .any(|field| field == b"SUBSYSTEM=power_supply")
}

/// Read the machine's charge out of a `power_supply` directory.
///
/// `None` when there is nothing there this shell would call a battery, and
/// also when there is one it cannot get a number out of: a mark drawn from a
/// reading that failed would be a lie about the hardware, and the corner has a
/// perfectly good answer for not knowing, which is to draw nothing.
fn read_charge(root: &Path) -> Option<Charge> {
    let batteries = batteries(root);
    if batteries.is_empty() {
        return None;
    }

    // Summed rather than averaged where the energies are there to sum, because
    // a machine with two batteries has one charge: a ThinkPad with a full
    // internal cell and an empty travel cell twice its size is not half full,
    // and averaging the two per cents says it is. `energy_*` is µWh and
    // `charge_*` is µAh; a machine reports one pair or the other, and either
    // divides out to the same fraction.
    let mut stored = 0u64;
    let mut capacity = 0u64;
    let mut percents: Vec<u8> = Vec::new();
    let mut charging = false;

    for battery in &batteries {
        if read(battery, "status").as_deref() == Some("Charging") {
            charging = true;
        }
        match (
            number(battery, "energy_now").or_else(|| number(battery, "charge_now")),
            number(battery, "energy_full").or_else(|| number(battery, "charge_full")),
        ) {
            (Some(now), Some(full)) if full > 0 => {
                stored += now;
                capacity += full;
            }
            // A driver that reports only `capacity` — some do — still gets its
            // reading used. It cannot be weighed against the others, so a
            // machine that mixes the two kinds falls back to the mean below.
            _ => {
                if let Some(percent) = number(battery, "capacity") {
                    percents.push(percent.min(100) as u8);
                }
            }
        }
    }

    // Two readings, and each is `None` when there was nothing to work it out
    // from: what the energies weigh out to, and the plain mean of whatever
    // reported only a per cent.
    let weighed = stored
        .saturating_mul(100)
        .checked_div(capacity)
        .map(|percent| percent.min(100) as u32);
    let mean = percents
        .iter()
        .map(|percent| u32::from(*percent))
        .sum::<u32>()
        .checked_div(percents.len() as u32);

    let percent = match (weighed, mean) {
        (Some(weighed), None) => weighed,
        (None, Some(mean)) => mean,
        // Both kinds at once, which is a machine nobody has. The energies are
        // the better answer and the bare per cents cannot be joined to them, so
        // the two are averaged as equals rather than one of them thrown away.
        (Some(weighed), Some(mean)) => (weighed + mean) / 2,
        (None, None) => return None,
    };

    Some(Charge {
        percent: percent as u8,
        charging,
    })
}

/// Whether any of this machine's own batteries says it is discharging, which is
/// what running on the battery is. "Not charging" and "Full" are a battery on
/// the mains that is not taking any more, and "Unknown" is what some firmware
/// says there too; none of those is the machine living off its battery.
fn discharging(root: &Path) -> bool {
    batteries(root)
        .iter()
        .any(|battery| read(battery, "status").as_deref() == Some("Discharging"))
}

/// Every supply in that directory that is this machine's own battery.
///
/// Sorted, so that a machine with two of them reads them in the same order
/// every pass and the answer cannot depend on what order the filesystem
/// happened to hand them over in.
fn batteries(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        // No such directory is the ordinary answer on anything that is not
        // Linux-with-a-power-supply-class, and it is not worth a warning every
        // twenty seconds for the length of a session.
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| is_system(path))
        .collect();
    found.sort();
    found
}

/// Whether one supply is a battery of this machine's.
///
/// Three questions, and each of them has bitten a shell somewhere. Is it a
/// battery at all, or the mains brick beside it. Is it *this machine's*, or the
/// cell in a wireless mouse — `scope` is `Device` for those, and absent for
/// nearly every laptop, so absent has to mean the system. And is there anything
/// in the bay: a laptop with the battery taken out still lists it, with
/// `present` set to zero, and a corner drawing an empty battery for a machine
/// that has none would be reporting hardware that is not in it.
fn is_system(path: &Path) -> bool {
    if read(path, "type").as_deref() != Some("Battery") {
        return false;
    }
    if read(path, "scope").as_deref() == Some("Device") {
        return false;
    }
    read(path, "present").as_deref() != Some("0")
}

/// One attribute, trimmed. `None` for a file that is not there or will not
/// read — which in this directory is ordinary rather than exceptional: the
/// attributes a supply publishes depend on its driver.
fn read(path: &Path, name: &str) -> Option<String> {
    std::fs::read_to_string(path.join(name))
        .ok()
        .map(|held| held.trim().to_string())
}

/// The same, as a number.
fn number(path: &Path, name: &str) -> Option<u64> {
    read(path, name)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `power_supply` directory: `(name, [(attribute, value)])`.
    fn tree(test: &str, supplies: &[(&str, &[(&str, &str)])]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("lxb-power-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        for (name, attributes) in supplies {
            let supply = root.join(name);
            std::fs::create_dir_all(&supply).unwrap();
            for (attribute, value) in *attributes {
                std::fs::write(supply.join(attribute), format!("{value}\n")).unwrap();
            }
        }
        root
    }

    /// The whole point of `scope`, and the case this machine is: a desktop with
    /// a wireless mouse on it lists a battery that is emphatically not its own.
    /// It also lists the mains brick, which is not a battery at all.
    #[test]
    fn a_mouses_cell_and_the_mains_are_not_this_machines_battery() {
        let root = tree(
            "peripheral",
            &[
                (
                    "hidpp_battery_0",
                    &[
                        ("type", "Battery"),
                        ("scope", "Device"),
                        ("capacity", "55"),
                        ("status", "Discharging"),
                    ],
                ),
                ("AC", &[("type", "Mains"), ("online", "1")]),
            ],
        );
        assert_eq!(read_charge(&root), None, "a desktop has no battery");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A laptop, which is the ordinary case: one battery, no `scope` file at
    /// all, and the reading comes back as it stands.
    #[test]
    fn a_laptop_battery_reads_even_with_no_scope_file() {
        let root = tree(
            "laptop",
            &[(
                "BAT0",
                &[
                    ("type", "Battery"),
                    ("present", "1"),
                    ("capacity", "96"),
                    ("status", "Discharging"),
                ],
            )],
        );
        assert_eq!(
            read_charge(&root),
            Some(Charge {
                percent: 96,
                charging: false,
            })
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An empty bay still lists the battery it has not got. Drawing an empty
    /// battery for it would be reporting hardware that is not in the machine.
    #[test]
    fn a_bay_with_nothing_in_it_is_not_a_battery() {
        let root = tree(
            "absent",
            &[(
                "BAT0",
                &[
                    ("type", "Battery"),
                    ("present", "0"),
                    ("capacity", "0"),
                    ("status", "Unknown"),
                ],
            )],
        );
        assert_eq!(read_charge(&root), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Two batteries are one charge, and it is the energies that say what it
    /// is. A full 24 Wh cell beside an empty 72 Wh one is a quarter full; the
    /// average of the two per cents would call it half.
    #[test]
    fn two_batteries_are_weighed_rather_than_averaged() {
        let root = tree(
            "pair",
            &[
                (
                    "BAT0",
                    &[
                        ("type", "Battery"),
                        ("capacity", "100"),
                        ("energy_now", "24000000"),
                        ("energy_full", "24000000"),
                        ("status", "Discharging"),
                    ],
                ),
                (
                    "BAT1",
                    &[
                        ("type", "Battery"),
                        ("capacity", "0"),
                        ("energy_now", "0"),
                        ("energy_full", "72000000"),
                        ("status", "Discharging"),
                    ],
                ),
            ],
        );
        assert_eq!(
            read_charge(&root).map(|charge| charge.percent),
            Some(25),
            "the mean of the two per cents would be 50"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `charge_*` in µAh is the other pair a driver may report, and it divides
    /// out to the same fraction.
    #[test]
    fn a_battery_reporting_amp_hours_reads_the_same_way() {
        let root = tree(
            "amps",
            &[(
                "BAT0",
                &[
                    ("type", "Battery"),
                    ("charge_now", "1500000"),
                    ("charge_full", "3000000"),
                    ("status", "Charging"),
                ],
            )],
        );
        assert_eq!(
            read_charge(&root),
            Some(Charge {
                percent: 50,
                charging: true,
            })
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Sitting on the mains at full is not charging. The kernel says so, and
    /// the corner has a drawing for full that is not the one that means
    /// filling.
    #[test]
    fn full_on_the_mains_is_not_charging() {
        let root = tree(
            "topped-up",
            &[(
                "BAT0",
                &[("type", "Battery"), ("capacity", "100"), ("status", "Full")],
            )],
        );
        assert_eq!(
            read_charge(&root),
            Some(Charge {
                percent: 100,
                charging: false,
            })
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A battery that answers nothing at all is not a reading of zero. Nothing
    /// is drawn rather than a flat battery invented for the user.
    #[test]
    fn a_battery_with_no_numbers_in_it_says_nothing() {
        let root = tree("mute", &[("BAT0", &[("type", "Battery")])]);
        assert_eq!(read_charge(&root), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A machine with no such directory — anything that is not Linux with a
    /// power supply class — is a machine with no battery, quietly.
    #[test]
    fn no_such_directory_is_no_battery() {
        let root = std::env::temp_dir().join(format!("lxb-power-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(read_charge(&root), None);
    }

    /// What the kernel sent when a Steam Deck's charger went in, shaped as it
    /// arrives: the mains and the battery each announce themselves, and a
    /// device of another subsystem on the same socket does not count.
    #[test]
    fn a_supply_changing_is_told_apart_from_everything_else_on_the_socket() {
        let mains = b"change@/devices/LNXSYSTM:00/LNXSYBUS:00/ACPI0003:00/power_supply/ACAD\0\
ACTION=change\0DEVPATH=/devices/LNXSYSTM:00/LNXSYBUS:00/ACPI0003:00/power_supply/ACAD\0\
SUBSYSTEM=power_supply\0POWER_SUPPLY_NAME=ACAD\0POWER_SUPPLY_TYPE=Mains\0\
POWER_SUPPLY_ONLINE=1\0SEQNUM=5063\0";
        let battery = b"change@/devices/LNXSYSTM:00/LNXSYBUS:00/PNP0C0A:00/power_supply/BAT1\0\
ACTION=change\0SUBSYSTEM=power_supply\0POWER_SUPPLY_NAME=BAT1\0\
POWER_SUPPLY_STATUS=Charging\0SEQNUM=5064\0";
        let display = b"change@/devices/pci0000:00/0000:00:08.1/0000:04:00.0/drm/card1\0\
ACTION=change\0SUBSYSTEM=drm\0HOTPLUG=1\0SEQNUM=5065\0";
        // A device whose *name* mentions it is still not one.
        let impostor = b"add@/devices/virtual/misc/power_supply\0ACTION=add\0\
SUBSYSTEM=misc\0SEQNUM=5066\0";

        assert!(is_power_supply_event(mains));
        assert!(is_power_supply_event(battery));
        assert!(!is_power_supply_event(display));
        assert!(!is_power_supply_event(impostor));
        assert!(!is_power_supply_event(b""));
    }

    /// A change is a read now and one a second for half a minute after — the
    /// Deck's battery said `Not charging` in both of the events its charger
    /// sent and turned to `Charging` later, with none — and then the clock
    /// again. The watch shortens the sleep and never lengthens it.
    #[test]
    fn a_supply_that_changed_is_watched_closely_for_a_while() {
        let now = Instant::now();
        assert_eq!(until_the_next_read(REFRESH, None, now), REFRESH);
        assert_eq!(
            until_the_next_read(REFRESH, Some(now + SETTLING), now),
            SETTLING_EVERY,
            "watched every second while it settles"
        );
        assert_eq!(
            until_the_next_read(REFRESH, Some(now + SETTLING), now + SETTLING),
            REFRESH,
            "and on the clock again once it has"
        );
        assert_eq!(
            until_the_next_read(Duration::from_millis(200), Some(now + SETTLING), now),
            Duration::from_millis(200),
            "a clock already quicker than the watch is left alone"
        );

        let shared = Shared {
            state: Mutex::new(State::default()),
            signal: Condvar::new(),
            root: PathBuf::new(),
        };
        supply_changed(&shared);
        let state = shared.state.lock().unwrap();
        assert!(state.dirty, "read now");
        assert!(
            state
                .settling_until
                .is_some_and(|until| until >= now + SETTLING - Duration::from_secs(1)),
            "and closely for half a minute after"
        );
    }

    /// The edges, each side of each of them.
    #[test]
    fn every_drawing_covers_the_readings_it_is_for() {
        assert_eq!(Level::of(0), Level::Empty);
        assert_eq!(Level::of(LOW_AT - 1), Level::Empty);
        assert_eq!(Level::of(LOW_AT), Level::Low);
        assert_eq!(Level::of(HALF_AT - 1), Level::Low);
        assert_eq!(Level::of(HALF_AT), Level::Half);
        assert_eq!(Level::of(HIGH_AT - 1), Level::Half);
        assert_eq!(Level::of(HIGH_AT), Level::High);
        assert_eq!(Level::of(FULL_AT - 1), Level::High);
        assert_eq!(Level::of(FULL_AT), Level::Full);
        assert_eq!(Level::of(100), Level::Full);
    }
}
