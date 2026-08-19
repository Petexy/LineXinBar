//! What this machine is on: the socket in the back of it, and the air around
//! it.
//!
//! Everything else the shell reaches for below a desktop it reaches for
//! directly — the kernel's backlight, the sound server's own mixer, the
//! connector's colour pipeline. This one cannot be, and it is worth saying why
//! rather than leaving it looking like an inconsistency.
//!
//! A wired socket could be: `getifaddrs` says whether it has an address, and
//! the kernel would carry a DHCP client if the session started one. Wireless
//! could not. Joining a network means speaking the four-way handshake against
//! an access point, which is a *supplicant* — `wpa_supplicant` or `iwd` — and
//! nothing in the kernel does it. So a shell that wanted to offer Wi-Fi without
//! depending on anything would have to become one, and a shell that is a
//! supplicant is a shell that fights the one the machine already has running.
//!
//! What is actually on a Linux machine that has Wi-Fi is NetworkManager, and it
//! owns both halves — the supplicant underneath it and the wired socket beside
//! it — so this module talks to that and to nothing else. The cost is stated
//! plainly on the page: a session with no network manager gets one row saying
//! so, exactly as a session with no sound server does. It is the same bargain
//! [`crate::system`] makes with `wpctl`, one level further out.
//!
//! ## What it does, and what it deliberately does not
//!
//! It reads what the machine has, turns the wireless radio on and off, brings a
//! wired socket up and down, and joins a wireless network — collecting the
//! password for one when there is no saved profile to use, or when the saved
//! one is refused. It writes no configuration of its own: every profile it
//! creates it hands to NetworkManager, which is what remembers them, for the
//! reason the sound device is remembered by the sound server. A shell with its
//! own copy would be a second opinion about the machine's network at every
//! login.
//!
//! It does not offer static addresses, proxies, VPNs, hotspots or enterprise
//! authentication. Those are not one press and a password — they are forms —
//! and a console shell that offered half a form would be worse than one that
//! says plainly that the network it cannot join has to be set up elsewhere.
//! An 802.1X network is listed and marked as such rather than hidden, because
//! seeing it and being told why is the answer to "my network is not here".
//!
//! ## Everything happens on a worker thread
//!
//! A D-Bus round trip to NetworkManager is fast, but a pass over the machine is
//! a call per device and a call per access point — thirty of them in a block of
//! flats — and asking for a scan is a radio going quiet for a moment. None of
//! that may happen on the thread that has a frame due. So the shell reads the
//! last answer and the worker keeps it true, on exactly the terms the quick
//! settings do: only while somebody is looking at the page.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::secret::Secret;

/// How often the listing is read again while the page is on screen.
///
/// Two seconds is what the sound devices are read at, and this is the same kind
/// of watching: a cable pulled out, a network that has come into range, a join
/// that has just completed. It costs a handful of D-Bus calls, which is far less
/// than the subprocesses that page costs.
const REFRESH: Duration = Duration::from_secs(2);

/// How often the radio is asked to look around while the page is on screen.
///
/// Much rarer than the listing is read, and deliberately: a scan takes the
/// radio off whatever it is carrying for a moment, so a session that scanned
/// every two seconds would be one where the page listing the networks made the
/// connection it is showing worse. NetworkManager scans on its own account too
/// and rate-limits what it is asked for; this only makes sure a page somebody
/// opened is not showing what was in the air a quarter of an hour ago.
const RESCAN: Duration = Duration::from_secs(20);

/// How often the mark beside the clock is brought up to date while nothing but
/// that mark is looking.
///
/// Slower than the page's own pace, because the two are looking at different
/// things. The page is a list that rows appear in and leave, and it is being
/// read a row at a time; the mark is three bands of one number, and a band is a
/// third of the whole range. Five seconds is a signal that has really moved
/// showing up before the user has finished noticing it themselves, at three
/// D-Bus calls a time — see [`Worker::read_signal`], which is the whole of what
/// this pace pays for.
const SIGNAL_REFRESH: Duration = Duration::from_secs(5);

/// How often a machine that has no wireless radio is asked whether it has one
/// yet. See [`Worker::find_radios`].
const RADIO_SEARCH: Duration = Duration::from_secs(30);

/// How long a join is watched closely before the ordinary pace resumes.
///
/// A join goes through five device states in a couple of seconds and the user is
/// watching every one of them, so the worker turns faster while one is in
/// flight. Past this it has either succeeded, failed, or is waiting on something
/// slow enough that half a second of latency does not show.
const JOINING: Duration = Duration::from_secs(30);

/// The pace while a join is in flight.
const HURRY: Duration = Duration::from_millis(400);

/// How wide a band of signal strength counts as the same strength, in per cent,
/// when the list of networks is put in order.
///
/// Twenty, which is the five bars a signal is drawn with everywhere else. See
/// the sort in [`Worker::read_networks`]: what this buys is a list that only
/// reorders when something really moved, under a cursor that may be standing
/// inside one of its rows.
const BANDS: u8 = 20;

const NM: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";
const NM_IFACE: &str = "org.freedesktop.NetworkManager";
const DEVICE_IFACE: &str = "org.freedesktop.NetworkManager.Device";
const WIRELESS_IFACE: &str = "org.freedesktop.NetworkManager.Device.Wireless";
const WIRED_IFACE: &str = "org.freedesktop.NetworkManager.Device.Wired";
const POINT_IFACE: &str = "org.freedesktop.NetworkManager.AccessPoint";
const IP4_IFACE: &str = "org.freedesktop.NetworkManager.IP4Config";
const IP6_IFACE: &str = "org.freedesktop.NetworkManager.IP6Config";
const ACTIVE_IFACE: &str = "org.freedesktop.NetworkManager.Connection.Active";
const CONNECTION_IFACE: &str = "org.freedesktop.NetworkManager.Settings.Connection";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

/// `NMDeviceType`, the two this shell has pages for.
const TYPE_ETHERNET: u32 = 1;
const TYPE_WIFI: u32 = 2;

/// `NMDeviceState`, the milestones worth telling apart.
const STATE_UNMANAGED: u32 = 10;
const STATE_UNAVAILABLE: u32 = 20;
const STATE_DISCONNECTED: u32 = 30;
const STATE_NEED_AUTH: u32 = 60;
const STATE_ACTIVATED: u32 = 100;
const STATE_DEACTIVATING: u32 = 110;
const STATE_FAILED: u32 = 120;

/// `NMDeviceStateReason::NO_SECRETS` — the password was wrong, or there was
/// none. The one reason this module acts on rather than merely reports.
const REASON_NO_SECRETS: u32 = 7;

/// Which of the two kinds of link a device is.
///
/// Only two, because only two have pages here. Everything else NetworkManager
/// manages — a Bluetooth tether, a modem, the bridges a container runtime
/// leaves lying about — is left out of the listing entirely rather than being
/// carried as a third kind nothing knows what to do with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Wired,
    Wireless,
}

/// What a device is doing, in the few states a page has anything to say about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    /// Nothing can be done with it: no cable in the socket, the radio switched
    /// off, or NetworkManager not in charge of it.
    Unavailable,
    /// Ready, and on nothing.
    Idle,
    /// On its way on or off.
    Working,
    /// On a network.
    Up,
    /// It tried and did not get there.
    Failed,
}

/// How a wireless network is protected, and therefore what joining it takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Security {
    /// Nothing at all: anybody within range is on it.
    Open,
    /// WEP, which has been broken for twenty years and is still in the air on
    /// hardware nobody has replaced. Offered because refusing to join a network
    /// the user's own router is running would be this shell deciding something
    /// that is not its to decide.
    Wep,
    /// WPA and WPA2 with a shared password: what nearly every home network is.
    Personal,
    /// WPA3 with a shared password. Joined exactly as the above is, from the
    /// user's side; it differs in the handshake underneath and in nothing they
    /// can see, so the page does not make a point of it beyond the name.
    Modern,
    /// 802.1X: a user name, a certificate and a server that decides. Not a
    /// password, and not one press.
    Enterprise,
}

impl Security {
    /// Whether joining it needs something typed.
    pub fn needs_password(self) -> bool {
        matches!(self, Security::Wep | Security::Personal | Security::Modern)
    }

    /// Whether this shell can join it at all.
    pub fn joinable(self) -> bool {
        !matches!(self, Security::Enterprise)
    }

    /// What the row says it is.
    pub fn title(self) -> &'static str {
        match self {
            Security::Open => "Open",
            Security::Wep => "WEP",
            Security::Personal => "WPA2",
            Security::Modern => "WPA3",
            Security::Enterprise => "Enterprise",
        }
    }

    /// What NetworkManager calls the key agreement, for the profile this module
    /// builds. `None` for the two that are never built here.
    fn key_management(self) -> Option<&'static str> {
        match self {
            // WEP has no key management: the key *is* the authentication, and
            // NetworkManager wants the setting present saying so.
            Security::Wep => Some("none"),
            Security::Personal => Some("wpa-psk"),
            Security::Modern => Some("sae"),
            Security::Open | Security::Enterprise => None,
        }
    }
}

/// One wireless network, as the page lists it.
///
/// A network and not an access point, although that is what NetworkManager
/// reports: a house with a repeater in it has the same name in the air twice,
/// and the two are one row here because they are one thing to join. The
/// strength is the best of them, which is the one the radio would use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Network {
    /// The name in the air. The handle on it, too — see [`Worker::point_for`],
    /// which finds the strongest access point carrying it at the moment of
    /// joining, so a device may roam between them without the page having said
    /// which was meant.
    pub ssid: String,
    /// 0 to 100, as the radio hears it.
    pub strength: u8,
    pub security: Security,
    /// Whether NetworkManager already has a profile for it, and so whether
    /// joining will ask for anything.
    pub saved: bool,
    /// Whether this is the one the device is on.
    pub joined: bool,
    /// The band it was heard on, in MHz. What separates the two halves of a
    /// router that publishes one name on both.
    pub frequency: u32,
}

/// How strong a wireless link is, in the three bands the start screen's corner
/// draws it in.
///
/// Three, because the mark is a fan of three arcs over a bead and each band
/// lights one more of them. A percentage is what NetworkManager reports and
/// what the Wi-Fi page prints, and it is deliberately not what travels here: a
/// number that moves a point or two on every reading would be a mark that
/// changed on every reading, and what the corner has to say is which of three
/// pictures the connection is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Weak,
    Fair,
    Strong,
}

/// Where one band ends and the next begins, as NetworkManager reports strength.
///
/// `nmcli` draws the same number as four blocks and steps at 30, 55 and 80.
/// This is that convention read down to three: a link at seventy per cent is
/// one nothing is wrong with, and one under forty is a link that is about to
/// start costing the user something.
const FAIR_AT: u8 = 40;
const STRONG_AT: u8 = 70;

/// How far past an edge a reading has to go before the mark follows it.
///
/// A radio's own report of the same unchanged connection wanders a point or two
/// between readings, so a machine sitting still on a boundary would have a mark
/// that flickered between two pictures for as long as it sat there. See
/// [`band_of`], which is where a band is therefore harder to leave than it was
/// to enter.
const SIGNAL_GUARD: u8 = 5;

/// Which band a reading falls in, given the band the mark is in already.
///
/// The band in force is the second argument because the answer depends on it:
/// the edges move against the direction of travel by [`SIGNAL_GUARD`], so a
/// signal has to rise past a raised edge to light another arc and fall below a
/// lowered one to put one out. A jump of two bands at once is still a jump of
/// two — the reading is taken as far as the guarded edges let it go, which is
/// what keeps a radio coming back into range from having to climb the fan one
/// pass at a time.
fn band_of(strength: u8, was: Option<Signal>) -> Signal {
    let band = |fair: u8, strong: u8| match strength {
        strength if strength >= strong => Signal::Strong,
        strength if strength >= fair => Signal::Fair,
        _ => Signal::Weak,
    };
    let rank = |signal: Signal| match signal {
        Signal::Weak => 0,
        Signal::Fair => 1,
        Signal::Strong => 2,
    };
    let Some(was) = was else {
        return band(FAIR_AT, STRONG_AT);
    };
    let risen = band(FAIR_AT + SIGNAL_GUARD, STRONG_AT + SIGNAL_GUARD);
    let fallen = band(
        FAIR_AT.saturating_sub(SIGNAL_GUARD),
        STRONG_AT.saturating_sub(SIGNAL_GUARD),
    );
    if rank(risen) > rank(was) {
        risen
    } else if rank(fallen) < rank(was) {
        fallen
    } else {
        was
    }
}

/// Which value of an interface's addressing is being typed into.
///
/// Three of them, and every one is a value with a *shape* rather than a choice
/// off a list — which is what makes them the only settings in this shell that
/// are typed. See [`fault`], which is where each one's shape is stated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    /// The address this machine takes, with the size of the network after it.
    Address,
    /// The machine on that network that leads off it.
    Router,
    /// The name servers to ask, in the order they should be asked.
    ///
    /// Spelt `Dns` rather than `NameServers` because that is what it is called
    /// on screen, and the screen is right: "DNS" is the word every router's own
    /// page uses and the one anybody looking for this setting will be looking
    /// for. The prose here goes on saying *name servers*, which is what the
    /// three letters stand for and what the value actually is.
    Dns,
}

impl Field {
    /// What the row is called, which is also what the panel is titled.
    ///
    /// Named in full even where the column it stands in is called the same
    /// thing. The panel a typed row opens covers the trail that would say what
    /// the user walked in through, so a heading reading `Addresses` would be a
    /// field with no subject at all — see [`crate::apps::Typed::whose`], which
    /// is the other half of the same answer.
    pub fn title(self) -> &'static str {
        match self {
            Field::Address => "Address",
            Field::Router => "Router",
            Field::Dns => "DNS servers",
        }
    }

    /// What to type, for somebody looking at an empty field on a television.
    pub fn note(self) -> &'static str {
        match self {
            Field::Address => {
                "This machine's address and the size of the network, as 192.168.1.50/24."
            }
            Field::Router => "The address of the router this network goes out through.",
            Field::Dns => "The name servers to ask, separated by commas.",
        }
    }
}

/// What one interface's profile asks for of IPv4.
///
/// IPv4 and not IPv6, and that is a real limit rather than an oversight: what
/// somebody means by "give this machine a static address" is an IPv4 address,
/// and IPv6 addressing is a form with a great deal more in it — prefix
/// delegation, privacy addresses, whether to accept router advertisements at
/// all. This page leaves IPv6 exactly as `NetworkManager` had it, which is
/// automatic, so a machine given a static IPv4 address still gets whatever the
/// network offers it over IPv6.
///
/// Read from the *profile* rather than from what the interface is running,
/// because that is what the page is about: a socket that asked for an address
/// and did not get one has to say what it asked for. What it is actually
/// running is [`Device::address`], which is what the information panel shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ipv4 {
    /// Whether the address is asked for from the network — DHCP.
    pub automatic: bool,
    /// The address the profile pins, with its prefix. `None` under automatic
    /// addressing that has never been given one to keep.
    pub address: Option<String>,
    pub gateway: Option<String>,
    /// Whether the name servers are taken from the network too.
    ///
    /// Only ever asked under automatic addressing. A manual profile runs no
    /// DHCP, so there is nothing for its name servers to be automatic *from* —
    /// see the page, which offers the choice only where it exists.
    pub dns_automatic: bool,
    /// The name servers the profile names, in the order it names them.
    pub dns: Vec<String>,
    /// Whether Manual can be chosen at all.
    ///
    /// `NetworkManager` refuses a manual profile with no address in it, so
    /// there has to be an address to pin: the one the profile already keeps, or
    /// the one the interface was given. A machine that has neither — a socket
    /// that has never been up — cannot be switched to Manual, and the page says
    /// so rather than offering a press that silently fails.
    pub can_pin: bool,
}

/// One interface, as the pages under Network describe it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// NetworkManager's own handle on it. What naming one to NetworkManager
    /// names, and what a [`crate::settings::Setting`] carries.
    pub path: String,
    /// What the kernel calls it — `enp8s0`, `wlan0`. The only name a user has
    /// for a socket, and the one that tells two of them apart.
    pub interface: String,
    pub kind: Kind,
    pub link: Link,
    /// Why it is not up, when there is something worth saying. `None` when it
    /// is up, or when it is idle for no reason but that nobody has asked.
    pub trouble: Option<String>,
    /// The profile it is on, by the name NetworkManager files it under — which
    /// for a wireless network is the network's own name.
    pub connection: Option<String>,
    /// The address this machine answers on through it.
    pub address: Option<String>,
    pub gateway: Option<String>,
    /// The name servers it was given, in order.
    pub nameservers: Vec<String>,
    /// Its hardware address.
    pub hardware: Option<String>,
    /// Whether there is a cable in it. Wired only; `None` on a radio, which has
    /// no such question.
    pub carrier: Option<bool>,
    /// How fast the link negotiated, in Mb/s. Zero where nothing has.
    pub speed: u32,
    /// The profile this interface's addressing belongs to, if it has one.
    ///
    /// `NetworkManager`'s own handle on it. Addressing is a property of a
    /// *profile* and not of an interface — that is what makes it survive a
    /// reboot and follow a laptop between networks — so a device with none has
    /// nothing on this page to configure, which is what `None` says.
    pub profile: Option<String>,
    /// What that profile asks for.
    pub ipv4: Ipv4,
}

impl Device {
    /// Whether there is a saved profile this socket could be brought up on
    /// without asking anything. Wired sockets only — a wireless one is brought
    /// up by choosing a network.
    pub fn up(&self) -> bool {
        matches!(self.link, Link::Up | Link::Working)
    }
}

/// A password the worker needs before it can go on.
///
/// Published rather than asked for directly, because the worker cannot draw and
/// the shell cannot block. The shell notices one of these in the listing, raises
/// the panel, and answers with [`Net::answer`] or [`Net::refuse`] — the same
/// shape the `polkitd` question arrives in, and for the same reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wanted {
    /// The network it is for, which is what the panel is titled after.
    pub ssid: String,
    /// Whether this is a second attempt, because the last password was refused.
    pub retry: bool,
    /// Which ask this is. The shell raises a panel when this changes and not
    /// merely when the field is set, so a refusal after a refusal is a fresh
    /// question rather than a panel that quietly stayed up.
    pub asked: u64,
}

/// Everything the Network pages are drawn from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listing {
    /// Whether a network manager answered at all.
    ///
    /// An empty listing means two different things and the page has to say
    /// which: a machine with no network hardware in it, or a session with
    /// nothing running that could be asked about it.
    pub manager: bool,
    /// Whether the wireless radio is on. Meaningless with no wireless device,
    /// and not read in that case.
    pub radio: bool,
    /// Whether the radio can be turned on at all, or whether a switch on the
    /// machine has it off. A page that offered On against a hardware kill
    /// switch would be offering something that cannot happen.
    pub radio_switchable: bool,
    /// Every wired and wireless interface, wired first and each half in the
    /// order NetworkManager lists them.
    pub devices: Vec<Device>,
    /// The networks in the air, per wireless device path.
    pub networks: Vec<(String, Vec<Network>)>,
    /// A password the worker is waiting for, if it is waiting for one.
    pub wanted: Option<Wanted>,
}

impl Listing {
    /// Nothing, before anything has been read — and what a session with no
    /// worker thread keeps.
    pub const fn none() -> Self {
        Self {
            manager: false,
            radio: false,
            radio_switchable: false,
            devices: Vec::new(),
            networks: Vec::new(),
            wanted: None,
        }
    }

    /// The devices of one kind, in the order they are listed.
    pub fn of(&self, kind: Kind) -> Vec<&Device> {
        self.devices
            .iter()
            .filter(|device| device.kind == kind)
            .collect()
    }

    /// The networks one wireless device can hear, strongest first.
    pub fn networks_of(&self, device: &str) -> &[Network] {
        self.networks
            .iter()
            .find(|(path, _)| path == device)
            .map(|(_, networks)| networks.as_slice())
            .unwrap_or(&[])
    }
}

/// What the user has asked for and the worker has not carried out yet.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Ask {
    /// Turn the wireless radio on, or off.
    Radio(bool),
    /// Put this device on this network.
    Join { device: String, ssid: String },
    /// Take it off whatever it is on.
    Leave { device: String },
    /// Delete the saved profile for this network on this device.
    Forget { device: String, ssid: String },
    /// Bring a wired socket up on its saved profile, or take it down.
    Wire { device: String, up: bool },
    /// Take this interface's address from the network, or pin it.
    Addressing { device: String, automatic: bool },
    /// Take its name servers from the network, or use the ones it names.
    Dns { device: String, automatic: bool },
    /// Put what was typed into one of the values that is typed.
    Write {
        device: String,
        field: Field,
        text: String,
    },
}

/// The listing, and the worker that keeps it true.
pub struct Net {
    shared: Arc<Shared>,
}

struct Shared {
    state: Mutex<State>,
    /// Woken whenever the page arrives, and by every press. Between those the
    /// worker sleeps: a listing nobody is looking at is a dozen D-Bus calls
    /// every two seconds for the length of the session.
    signal: Condvar,
}

#[derive(Default)]
struct State {
    listing: Listing,
    /// Bumped every time the worker puts a *different* listing here.
    ///
    /// So that the shell can ask "has anything changed" once a frame without
    /// copying the whole listing to find out. It matters: the listing is a
    /// vector of devices and a vector of networks with a handful of strings in
    /// each, and cloning that sixty times a second to compare it against itself
    /// would be a few thousand allocations a second for an answer that is
    /// almost always no.
    published: u64,
    /// Whether the pages that show it are on screen, and so whether it is worth
    /// keeping fresh.
    watching: bool,
    /// Whether the start screen's own corner is on screen, and so whether the
    /// one thing drawn there — how strong the wireless link is — is worth
    /// keeping true.
    ///
    /// A separate question from `watching` and a much cheaper one, because the
    /// corner is on screen nearly all session: it is three D-Bus calls about
    /// one radio, where the pages are a call per device and a call per access
    /// point. See [`Worker::read_signal`].
    corner: bool,
    /// Which band the wireless link is in, or `None` when this machine is on no
    /// wireless network — which is what makes the corner draw nothing rather
    /// than draw an empty fan.
    signal: Option<Signal>,
    /// Presses waiting to be carried out, in the order they were made.
    ///
    /// A queue rather than one slot, unlike the quick settings' bars: those are
    /// a position that the last press supersedes, and these are each a separate
    /// thing done to a separate device. Two presses on one row do collapse —
    /// see [`Net::ask`] — because asking twice for the same thing is asking
    /// once.
    asks: Vec<Ask>,
    /// The password the panel collected, on its way to the worker.
    ///
    /// It lives here for the length of one lock and no longer: the worker takes
    /// it out on its next pass, hands it to NetworkManager, and drops it. See
    /// [`crate::secret::Secret`], which is what makes that worth doing.
    answer: Option<Secret>,
    /// The user closed the panel without typing anything.
    refused: bool,
    dirty: bool,
    done: bool,
}

impl Net {
    /// Start looking.
    ///
    /// Nothing is read until somebody opens the page — see [`Net::watch`] — so
    /// this costs a thread and a sleeping condition variable on a session that
    /// never opens Settings.
    pub fn start() -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            signal: Condvar::new(),
        });
        let worker = Arc::clone(&shared);
        if let Err(err) = std::thread::Builder::new()
            .name("lxb-network".to_string())
            .spawn(move || Worker::new(worker).run())
        {
            tracing::warn!(?err, "no worker thread; the Network pages are off");
        }
        Self { shared }
    }

    /// What the machine's network looks like, as the worker last found it.
    pub fn listing(&self) -> Listing {
        self.held().listing.clone()
    }

    /// How many times the listing has changed. Ask before [`Net::listing`]:
    /// the same number twice is the same listing, and copying it out to find
    /// that out is the copy this exists to avoid.
    pub fn published(&self) -> u64 {
        self.held().published
    }

    /// Which band the wireless link is in, as the worker last found it, or
    /// `None` for a machine that is on no wireless network.
    ///
    /// Copied out whole rather than counted like [`Net::published`], because
    /// there is nothing here to copy: it is one number, and the shell compares
    /// it against what it drew last frame for the cost of the comparison.
    pub fn signal(&self) -> Option<Signal> {
        self.held().signal
    }

    /// Say whether the start screen's corner is on screen.
    ///
    /// The counterpart of [`Net::watch`] for the one mark this module puts
    /// outside Settings. Asked for separately because it is answered
    /// separately: the corner is showing whenever no application covers the
    /// start screen, which is most of a session, and reading the whole machine
    /// at the page's pace for the sake of one mark would be the cost this
    /// module's header refuses.
    pub fn watch_signal(&self, corner: bool) {
        let mut state = self.held();
        if state.corner == corner {
            return;
        }
        state.corner = corner;
        // Woken for the corner arriving, and for the same reason the page
        // wakes it: a game that has just been left may have been left on a
        // network that went away while it had the display, and the mark the
        // user comes back to has to be this machine's rather than the one it
        // was on five minutes ago.
        if corner {
            state.dirty = true;
            self.shared.signal.notify_one();
        }
    }

    /// Say whether the pages that show it are on screen.
    ///
    /// The counterpart of [`crate::system::Quick::watch_devices`], and on the
    /// same terms: a cable is pulled out and a network comes into range while
    /// the session runs, and the only moment that has to be noticed is while
    /// somebody is looking at the list.
    pub fn watch(&self, listing: bool) {
        let mut state = self.held();
        if state.watching == listing {
            return;
        }
        state.watching = listing;
        // Only worth waking for the page arriving. The page leaving means the
        // worker has one fewer thing to do next time it is up, which can wait.
        if listing {
            state.dirty = true;
            self.shared.signal.notify_one();
        }
    }

    /// Turn the wireless radio on, or off.
    pub fn set_radio(&self, on: bool) {
        self.ask(Ask::Radio(on));
    }

    /// Put a wireless device on a network.
    ///
    /// The whole of what a press on a network's row does. Whether that needs a
    /// password — and whether the one NetworkManager has saved is any good — is
    /// the worker's to work out, because the worker is what knows the saved
    /// profiles; a shell that decided it here would be deciding it from a
    /// listing that is by then a moment old. What comes back is a [`Wanted`] in
    /// the listing, or a device that joins.
    pub fn join(&self, device: &str, ssid: &str) {
        self.ask(Ask::Join {
            device: device.to_string(),
            ssid: ssid.to_string(),
        });
    }

    /// Take a device off whatever it is on.
    pub fn leave(&self, device: &str) {
        self.ask(Ask::Leave {
            device: device.to_string(),
        });
    }

    /// Delete the saved profile for a network, password and all.
    ///
    /// Named for what it is to the user rather than for what it does to
    /// `NetworkManager`: what goes is the profile, and the profile is the whole
    /// of what this machine remembers about a network — that it was joined,
    /// what key got it on, and any address pinned on it.
    pub fn forget(&self, device: &str, ssid: &str) {
        self.ask(Ask::Forget {
            device: device.to_string(),
            ssid: ssid.to_string(),
        });
    }

    /// Bring a wired socket up on its saved profile, or take it down.
    pub fn set_wired(&self, device: &str, up: bool) {
        self.ask(Ask::Wire {
            device: device.to_string(),
            up,
        });
    }

    /// Take this interface's address from the network, or pin the one it has.
    pub fn set_addressing(&self, device: &str, automatic: bool) {
        self.ask(Ask::Addressing {
            device: device.to_string(),
            automatic,
        });
    }

    /// The same for its name servers.
    pub fn set_dns(&self, device: &str, automatic: bool) {
        self.ask(Ask::Dns {
            device: device.to_string(),
            automatic,
        });
    }

    /// Put what was typed into one of the values that is typed.
    ///
    /// Checked before it gets here — see [`fault`], which the panel calls while
    /// it is still on screen and can still say what is wrong with it. Anything
    /// that arrives here has a shape `NetworkManager` will take.
    pub fn write(&self, device: &str, field: Field, text: &str) {
        self.ask(Ask::Write {
            device: device.to_string(),
            field,
            text: text.to_string(),
        });
    }

    /// Hand over the password the panel collected.
    ///
    /// Taken whole rather than copied: the secret moves from the panel's field
    /// to the worker and is overwritten when the worker drops it, so there is
    /// never a second copy of it anywhere in the process.
    pub fn answer(&self, password: Secret) {
        let mut state = self.held();
        state.answer = Some(password);
        state.refused = false;
        state.dirty = true;
        self.shared.signal.notify_one();
    }

    /// Say that nothing is going to be typed.
    pub fn refuse(&self) {
        let mut state = self.held();
        state.answer = None;
        state.refused = true;
        // The question goes off the listing straight away rather than waiting
        // for the worker to wake: the panel has closed, and a listing that
        // still asked would have the shell raise it again on the next frame.
        // Counted as a publication, because it is one — this is the only place
        // anything but the worker writes the listing, and a change that did not
        // bump the count would be one no reader ever looks at.
        state.listing.wanted = None;
        state.published += 1;
        state.dirty = true;
        self.shared.signal.notify_one();
    }

    /// Queue a press, collapsing one that says the same thing again.
    fn ask(&self, ask: Ask) {
        let mut state = self.held();
        // The same ask twice is one ask. Two *different* asks about one device
        // are both kept and carried out in order — a user who turned the radio
        // off and on again meant both, and the second is not the first.
        if state.asks.last() != Some(&ask) {
            state.asks.push(ask);
        }
        state.dirty = true;
        self.shared.signal.notify_one();
    }

    fn held(&self) -> std::sync::MutexGuard<'_, State> {
        self.shared.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Drop for Net {
    fn drop(&mut self) {
        let mut state = self.held();
        state.done = true;
        self.shared.signal.notify_one();
    }
}

// --- the worker ------------------------------------------------------------

/// A join that has been started and is waiting for a password.
///
/// Held by the worker rather than in the shared state, because it is the
/// worker's half of the conversation: what the shell sees of it is the
/// [`Wanted`] published in the listing.
struct Asking {
    device: String,
    ssid: String,
    security: Security,
    /// The saved profile the password is for, when there is one. `None` means a
    /// network that has never been joined, which is a profile to create rather
    /// than one to correct.
    profile: Option<String>,
    /// Whether the last password was refused, which is what the panel says.
    retry: bool,
}

struct Worker {
    shared: Arc<Shared>,
    /// The system bus, opened on the first pass that needs it and reopened if
    /// it is ever lost. `None` on a machine with no D-Bus at all, where the
    /// page says there is no network manager — which is true, because nothing
    /// could be reached to be one.
    bus: Option<zbus::blocking::Connection>,
    /// The password this join is waiting for.
    asking: Option<Asking>,
    /// How many passwords have been asked for. Only ever compared, never
    /// counted for its own sake — see [`Wanted::asked`].
    asked: u64,
    /// The device a join was last started on, and when. What makes the worker
    /// turn faster while the user is watching a network being joined.
    joining: Option<(String, Instant)>,
    /// When each wireless device was last asked to look around.
    scanned: Vec<(String, Instant)>,
    /// The radios in this machine, once they have been found.
    ///
    /// Kept because finding them means asking the manager for every device it
    /// has and then asking each device what kind it is, and the answer changes
    /// only when hardware does. The corner's mark is read at
    /// [`SIGNAL_REFRESH`] for the whole of a session, so the difference
    /// between remembering this and asking again every time is most of what
    /// that mark costs. A path that stops answering — a card pulled out,
    /// NetworkManager restarted — empties this and it is looked for again.
    radios: Vec<String>,
    /// When the radios were last looked for, so a machine with none is not
    /// asked about them every pass. See [`Self::find_radios`].
    sought: Option<Instant>,
    /// The band last published, which is what makes the next one hysteretic:
    /// see [`band_of`], which needs the band in force to know how far a reading
    /// has to move before the mark does.
    band: Option<Signal>,
}

impl Worker {
    fn new(shared: Arc<Shared>) -> Self {
        Self {
            shared,
            bus: None,
            asking: None,
            asked: 0,
            joining: None,
            scanned: Vec::new(),
            radios: Vec::new(),
            sought: None,
            band: None,
        }
    }

    fn run(mut self) {
        while self.tick() {
            self.wait();
        }
    }

    /// One pass. `false` when the shell has gone away.
    fn tick(&mut self) -> bool {
        let (watching, corner, asks, answer, refused) = {
            let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.done {
                return false;
            }
            state.dirty = false;
            (
                state.watching,
                state.corner,
                std::mem::take(&mut state.asks),
                state.answer.take(),
                std::mem::take(&mut state.refused),
            )
        };

        // Nothing to do and nobody looking: the bus connection is kept, because
        // dropping and reopening it on every visit to the page would cost more
        // than the socket it holds.
        //
        // A join in flight counts as something to do even with the page shut.
        // The user pressed a row and is owed the answer, and they may well have
        // walked out of Settings to watch for it — this is the same reason
        // [`Self::read`] is run for one, and the two conditions have to agree
        // or the loop would hurry round doing nothing for half a minute.
        //
        // The corner counts as somebody looking too, on its own much narrower
        // terms: it is one mark about one radio, so what it asks for is
        // [`Self::read_signal`] and not the whole machine.
        if !watching
            && !corner
            && asks.is_empty()
            && answer.is_none()
            && !refused
            && self.asking.is_none()
            && self.joining.is_none()
        {
            return true;
        }

        self.connect();
        if refused && self.asking.take().is_some() {
            tracing::info!("the network password was not given");
        }
        for ask in asks {
            self.carry_out(ask);
        }
        if let Some(password) = answer {
            self.answer(password);
        }
        if watching || self.joining.is_some() {
            let listing = self.read();
            self.publish(listing);
        }
        // Read whether or not the pages are, and from its own three calls
        // rather than out of the listing beside it. The listing has the same
        // fact in it — the joined network's strength, on the radio that is up —
        // but taking it from there would mean the mark had two ways of being
        // worked out and only one of them was true with Settings shut. One
        // decision in one place, at the cost of three calls on the passes where
        // somebody is on the Wi-Fi page.
        if corner {
            let signal = self.read_signal();
            self.publish_signal(signal);
        }
        true
    }

    /// Sleep until something happens, or until it is time to look again.
    fn wait(&self) {
        let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.done || state.dirty {
            return;
        }
        // A join in flight keeps the loop turning at its own pace whether or not
        // the page is on screen: the user pressed a row and is owed the answer,
        // and they may well have walked out of Settings to watch the icon.
        let hurrying = self
            .joining
            .as_ref()
            .is_some_and(|(_, since)| since.elapsed() < JOINING);
        if hurrying {
            let _held = self.shared.signal.wait_timeout(state, HURRY);
        } else if state.watching {
            let _held = self.shared.signal.wait_timeout(state, REFRESH);
        } else if state.corner {
            let _held = self.shared.signal.wait_timeout(state, SIGNAL_REFRESH);
        } else {
            let _held = self.shared.signal.wait(state);
        }
    }

    /// Put the listing in front of the shell, if it has changed.
    fn publish(&mut self, listing: Listing) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.listing != listing {
            state.listing = listing;
            state.published += 1;
        }
    }

    /// Put the corner's band in front of the shell, and keep the worker's own
    /// copy of it: the next reading is measured against this one.
    fn publish_signal(&mut self, signal: Option<Signal>) {
        self.band = signal;
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.signal = signal;
    }

    /// Open the system bus, or keep the one that is open.
    fn connect(&mut self) {
        if self.bus.is_some() {
            return;
        }
        match zbus::blocking::Connection::system() {
            Ok(bus) => self.bus = Some(bus),
            // Said at debug, not warn: a session with no system bus is a
            // session where this is the expected answer every pass, and the
            // page says it in words the user can read.
            Err(err) => tracing::debug!(?err, "no system bus; the Network pages have nothing"),
        }
    }

    // --- carrying out a press ---------------------------------------------

    fn carry_out(&mut self, ask: Ask) {
        let Some(bus) = self.bus.as_ref() else {
            return;
        };
        match ask {
            Ask::Radio(on) => {
                tracing::info!(on, "wireless radio");
                if !set_property(bus, NM_PATH, NM_IFACE, "WirelessEnabled", Value::Bool(on)) {
                    tracing::warn!(on, "the wireless radio would not be set");
                }
            }
            Ask::Leave { device } => {
                tracing::info!(device, "leaving the network");
                if call(bus, &device, DEVICE_IFACE, "Disconnect", &()).is_none() {
                    tracing::warn!(device, "the device would not disconnect");
                }
                self.joining = None;
            }
            Ask::Forget { device, ssid } => self.forget(&device, &ssid),
            Ask::Wire { device, up } => self.wire(&device, up),
            Ask::Join { device, ssid } => self.begin_join(&device, &ssid),
            Ask::Addressing { device, automatic } => self.set_addressing(&device, automatic),
            Ask::Dns { device, automatic } => self.set_dns(&device, automatic),
            Ask::Write {
                device,
                field,
                text,
            } => self.write_field(&device, field, &text),
        }
    }

    /// Delete the saved profile for one network on one device.
    ///
    /// One call, and the profile is gone from the machine rather than from this
    /// shell: `NetworkManager` is what remembers a network — see
    /// [`crate::settings::Setting::Network`] — so what is deleted here is
    /// deleted for every program on the machine, which is what forgetting a
    /// network has always meant and the only version of it that is not a
    /// second opinion.
    ///
    /// Nothing is disconnected here, and nothing needs to be. Deleting the
    /// profile a device is up on takes the device down with it, because
    /// `NetworkManager` has nothing left to hold the connection open — and a
    /// shell that disconnected first would have a moment where the network was
    /// left and still saved, which is neither of the two states the page can
    /// draw.
    ///
    /// The profile is looked up through the device rather than through the
    /// whole settings list, for the reason [`saved`] gives: the device's answer
    /// is already narrowed to the profiles that could be used here, so a second
    /// network of the same name saved for another card is not what gets
    /// deleted.
    fn forget(&mut self, device: &str, ssid: &str) {
        let Some(profile) = self.profile_for(device, ssid) else {
            // Not a warning. A network with nothing saved for it has no Forget
            // row, so arriving here means the profile went between the page
            // being drawn and the press landing — somebody else deleted it, and
            // the machine is already in the state that was asked for.
            tracing::debug!(device, ssid, "nothing saved for this network to forget");
            return;
        };
        let Some(bus) = self.bus.as_ref() else {
            return;
        };
        tracing::info!(device, ssid, "forgetting the network");
        if call(bus, &profile, CONNECTION_IFACE, "Delete", &()).is_none() {
            tracing::warn!(device, ssid, "the saved network would not be deleted");
        }
        // A join in flight on this device is over either way: it was either for
        // this network, whose profile has just gone, or it is about to be
        // interrupted by the disconnect that deleting an active profile causes.
        if self.joining.as_ref().is_some_and(|(on, _)| on == device) {
            self.joining = None;
        }
    }

    /// Bring a wired socket up on a saved profile, or take it down.
    ///
    /// Up is the interesting half. A socket with a profile is activated on it;
    /// one with none at all is handed an empty connection, which is
    /// NetworkManager's way of being asked for the obvious thing — a wired
    /// profile that takes its address from the network. That is what a user
    /// plugging a cable into a console means, and asking them to fill in a form
    /// first would be this shell inventing a question.
    fn wire(&mut self, device: &str, up: bool) {
        let Some(bus) = self.bus.as_ref() else {
            return;
        };
        if !up {
            tracing::info!(device, "taking the wired socket down");
            if call(bus, device, DEVICE_IFACE, "Disconnect", &()).is_none() {
                tracing::warn!(device, "the wired socket would not come down");
            }
            return;
        }
        let Some(path) = object(device) else {
            return;
        };
        let Some(nothing) = object("/") else {
            return;
        };
        let profile = available(bus, device).into_iter().next();
        tracing::info!(device, ?profile, "bringing the wired socket up");
        let done = match profile.as_deref().and_then(object) {
            Some(profile) => call(
                bus,
                NM_PATH,
                NM_IFACE,
                "ActivateConnection",
                &(profile, path, nothing),
            )
            .is_some(),
            None => {
                let blank: HashMap<&str, HashMap<&str, Value<'_>>> = HashMap::new();
                call(
                    bus,
                    NM_PATH,
                    NM_IFACE,
                    "AddAndActivateConnection",
                    &(blank, path, nothing),
                )
                .is_some()
            }
        };
        if !done {
            tracing::warn!(device, "the wired socket would not come up");
        }
        self.joining = Some((device.to_string(), Instant::now()));
    }

    // --- addressing --------------------------------------------------------

    /// Take an interface's address from the network, or pin the one it has.
    ///
    /// Pinning is the half with something to work out. `NetworkManager` refuses
    /// a manual profile with no address in it, so switching to Manual has to
    /// supply one — and the only address that is not an invention is the one
    /// this machine already has. So the profile keeps whatever it was pinning
    /// before, and where it was pinning nothing the interface's own address and
    /// router are written in. That is also what somebody means by the press:
    /// *keep this*.
    ///
    /// Going the other way changes nothing but the method. The pinned address
    /// stays in the profile — `NetworkManager` keeps it and ignores it under
    /// automatic addressing — so a user who turns DHCP on to see whether it
    /// works still has their static settings when they turn it off again.
    fn set_addressing(&mut self, device: &str, automatic: bool) {
        let pin = (!automatic).then(|| self.running_address(device));
        self.change_ipv4(device, |ipv4| {
            ipv4.insert(
                "method".to_string(),
                text_value(if automatic { "auto" } else { "manual" }),
            );
            let Some(running) = pin else {
                return true;
            };
            // Only where the profile has nothing of its own. A profile that
            // already pins an address is being switched *back* to Manual, and
            // overwriting what it kept with whatever DHCP happened to hand out
            // would be the shell throwing away the setting.
            if !dicts(ipv4, "address-data").is_empty() {
                return true;
            }
            let Some((address, prefix)) = running.address.as_deref().and_then(split_address) else {
                tracing::warn!("nothing to pin: this interface has no address of its own");
                return false;
            };
            write_address(ipv4, &address, prefix);
            if let Some(gateway) = running.gateway.as_deref() {
                ipv4.insert("gateway".to_string(), text_value(gateway));
            }
            true
        });
    }

    /// Take an interface's name servers from the network, or use its own.
    fn set_dns(&mut self, device: &str, automatic: bool) {
        self.change_ipv4(device, |ipv4| {
            // The list is left alone either way, for the reason the pinned
            // address is: turning this back on is a user asking what the
            // network offers, not a user throwing away the servers they chose.
            ipv4.insert("ignore-auto-dns".to_string(), bool_value(!automatic));
            true
        });
    }

    /// Put what was typed into one of the three values that are typed.
    fn write_field(&mut self, device: &str, field: Field, text: &str) {
        let text = text.trim().to_string();
        self.change_ipv4(device, move |ipv4| match field {
            Field::Address => {
                let Some((address, prefix)) = split_address(&text) else {
                    // Cleared. The method goes back to automatic with it: a
                    // manual profile with no address is one NetworkManager
                    // refuses outright, so the alternative to writing this is
                    // writing nothing and leaving the page describing an
                    // address that is not there.
                    if text.is_empty() {
                        ipv4.remove("address-data");
                        ipv4.remove("addresses");
                        ipv4.remove("gateway");
                        ipv4.insert("method".to_string(), text_value("auto"));
                        return true;
                    }
                    return false;
                };
                write_address(ipv4, &address, prefix);
                // Typing an address *is* asking for it to be used. A page that
                // took the address and left the method on automatic would have
                // the user set a value and watch nothing happen.
                ipv4.insert("method".to_string(), text_value("manual"));
                true
            }
            Field::Router => {
                match text.is_empty() {
                    true => ipv4.remove("gateway"),
                    false => ipv4.insert("gateway".to_string(), text_value(&text)),
                };
                true
            }
            Field::Dns => {
                let servers: Vec<u32> = servers_of(&text).into_iter().map(packed).collect();
                ipv4.insert("dns".to_string(), numbers_value(&servers));
                // And the same again: naming servers is asking for them, so the
                // switch above follows the field rather than having to be set
                // twice. Clearing the field hands the question back to the
                // network, which is the only other answer there is.
                ipv4.insert(
                    "ignore-auto-dns".to_string(),
                    bool_value(!servers.is_empty()),
                );
                true
            }
        });
    }

    /// Read one interface's profile, change its IPv4 section, write it back,
    /// and put it into force.
    ///
    /// `Reapply` rather than a deactivate and activate, which is what makes a
    /// static address something the user can set without the link going down
    /// under whatever they were doing. It is the one call `NetworkManager` has
    /// for exactly this — take the profile as it now stands and bring the
    /// running interface into line with it — and where it is refused, the
    /// profile has still been written and comes into force the next time the
    /// interface is brought up.
    fn change_ipv4(
        &mut self,
        device: &str,
        change: impl FnOnce(&mut HashMap<String, OwnedValue>) -> bool,
    ) {
        let Some(profile) = self.profile_of(device) else {
            tracing::warn!(device, "no profile to configure the addressing of");
            return;
        };
        let written = self.edit_profile(&profile, |settings| {
            let ipv4 = settings.entry("ipv4".to_string()).or_default();
            if !change(ipv4) {
                return false;
            }
            // The deprecated spelling goes whenever the current one is written.
            // Both in one profile is two answers to the same question, and
            // which of them NetworkManager believes is not something to depend
            // on.
            if ipv4.contains_key("address-data") {
                ipv4.remove("addresses");
            }
            true
        });
        if !written {
            return;
        }
        tracing::info!(device, "addressing written");
        let Some(bus) = self.bus.as_ref() else {
            return;
        };
        let blank: HashMap<&str, HashMap<&str, Value<'_>>> = HashMap::new();
        // Version zero is "do not check": the profile was read, changed and
        // written in the last few milliseconds, and a version that had moved in
        // between is a change somebody else made that this one is now on top of.
        if call(bus, device, DEVICE_IFACE, "Reapply", &(blank, 0u64, 0u32)).is_none() {
            tracing::info!(
                device,
                "the interface would not take the change without being brought up again"
            );
        }
    }

    /// Read a profile, change it, and write it back whole.
    ///
    /// ## The secrets have to be carried across, and this is why
    ///
    /// `Update` *replaces* a profile. `GetSettings` deliberately leaves the
    /// secrets out of what it hands over — that is the whole point of it, so
    /// that any program may read a profile without being trusted with the
    /// password in it — so a read-change-write built out of those two alone
    /// writes back a profile with no password in it. On a wireless profile that
    /// is the saved network's key: changing a name server would silently
    /// unlearn the password for the very network the change was made on, and
    /// the user would find out at the next reboot.
    ///
    /// So the secrets are asked for as well and put back before the write.
    /// `GetSecrets` with no setting named is all of them; a profile that has
    /// none answers with an error, which is not one — that is the ordinary case
    /// for a wired socket, where there is nothing to carry across.
    ///
    /// Nothing here is ever logged. What comes back is merged and handed
    /// straight to `Update`, and the one thing this function must not do is
    /// print what it is holding.
    fn edit_profile(
        &self,
        profile: &str,
        change: impl FnOnce(&mut HashMap<String, HashMap<String, OwnedValue>>) -> bool,
    ) -> bool {
        let Some(bus) = self.bus.as_ref() else {
            return false;
        };
        let Some(reply) = call(bus, profile, CONNECTION_IFACE, "GetSettings", &()) else {
            return false;
        };
        let Ok(mut settings) = reply
            .body()
            .deserialize::<HashMap<String, HashMap<String, OwnedValue>>>()
        else {
            tracing::warn!("the profile could not be read back");
            return false;
        };
        if let Some(secrets) =
            call(bus, profile, CONNECTION_IFACE, "GetSecrets", &("")).and_then(|reply| {
                reply
                    .body()
                    .deserialize::<HashMap<String, HashMap<String, OwnedValue>>>()
                    .ok()
            })
        {
            for (section, held) in secrets {
                settings.entry(section).or_default().extend(held);
            }
        }
        if !change(&mut settings) {
            return false;
        }
        if call(bus, profile, CONNECTION_IFACE, "Update", &(settings,)).is_none() {
            tracing::warn!("the profile would not take the change");
            return false;
        }
        true
    }

    /// The profile an interface's addressing belongs to.
    ///
    /// The one in force, and failing that the one it would come up on. Both are
    /// the same profile in every ordinary case; they differ for a socket with a
    /// cable in it and autoconnect off, where there is something to configure
    /// and nothing running it — which is a machine being set up before it is
    /// plugged into anything, and exactly when somebody is typing an address in.
    fn profile_of(&self, device: &str) -> Option<String> {
        let bus = self.bus.as_ref()?;
        let properties = get_all(bus, device, DEVICE_IFACE)?;
        if let Some(active) = path(&properties, "ActiveConnection") {
            if let Some(profile) = get_all(bus, &active, ACTIVE_IFACE)
                .as_ref()
                .and_then(|active| path(active, "Connection"))
            {
                return Some(profile);
            }
        }
        // Only where there is exactly one. A device with several is one where
        // "the profile" is a question with more than one answer, and picking
        // the first would be the shell configuring whichever `NetworkManager`
        // happened to list first.
        match paths(&properties, "AvailableConnections").as_slice() {
            [only] => Some(only.clone()),
            _ => None,
        }
    }

    /// What one interface is actually running, as opposed to what its profile
    /// asks for.
    fn running_address(&self, device: &str) -> Addresses {
        let Some(bus) = self.bus.as_ref() else {
            return Addresses::default();
        };
        let Some(properties) = get_all(bus, device, DEVICE_IFACE) else {
            return Addresses::default();
        };
        self.read_addresses(&properties)
    }

    /// Start joining a wireless network: either straight away, or by asking for
    /// a password first.
    fn begin_join(&mut self, device: &str, ssid: &str) {
        let Some(bus) = self.bus.as_ref() else {
            return;
        };
        let Some(point) = self.point_for(device, ssid) else {
            tracing::warn!(device, ssid, "that network is no longer in the air");
            return;
        };
        let security = point.security;
        if !security.joinable() {
            tracing::info!(ssid, "this shell cannot join an enterprise network");
            return;
        }
        let profile = self.profile_for(device, ssid);

        // A saved profile is activated as it stands. Its password, right or
        // wrong, is NetworkManager's already — asking for one the machine
        // has would be this shell making the user type what it could have
        // looked up. If it turns out to be wrong the join comes back wanting
        // secrets, and *that* is when the panel goes up; see [`Self::read`].
        if let Some(profile) = profile.as_deref().and_then(object) {
            let (Some(path), Some(at)) = (object(device), object(&point.path)) else {
                return;
            };
            tracing::info!(device, ssid, "joining on the saved profile");
            if call(
                bus,
                NM_PATH,
                NM_IFACE,
                "ActivateConnection",
                &(profile, path, at),
            )
            .is_none()
            {
                tracing::warn!(device, ssid, "the network would not be joined");
            }
            self.joining = Some((device.to_string(), Instant::now()));
            return;
        }

        // An open network needs nothing typed, so it is joined with an empty
        // connection: NetworkManager fills the whole profile in from the access
        // point, which knows more about it than this shell does.
        if !security.needs_password() {
            let (Some(path), Some(at)) = (object(device), object(&point.path)) else {
                return;
            };
            let blank: HashMap<&str, HashMap<&str, Value<'_>>> = HashMap::new();
            tracing::info!(device, ssid, "joining an open network");
            if call(
                bus,
                NM_PATH,
                NM_IFACE,
                "AddAndActivateConnection",
                &(blank, path, at),
            )
            .is_none()
            {
                tracing::warn!(device, ssid, "the open network would not be joined");
            }
            self.joining = Some((device.to_string(), Instant::now()));
            return;
        }

        self.want_password(device, ssid, security, None, false);
    }

    /// Ask the shell to collect a password, and stop until it has.
    fn want_password(
        &mut self,
        device: &str,
        ssid: &str,
        security: Security,
        profile: Option<String>,
        retry: bool,
    ) {
        self.asked += 1;
        tracing::info!(ssid, retry, "asking for the network password");
        self.asking = Some(Asking {
            device: device.to_string(),
            ssid: ssid.to_string(),
            security,
            profile,
            retry,
        });
        // The join is no longer in flight: nothing will happen until somebody
        // types, and a worker turning at the hurried pace in the meantime would
        // be spinning against a panel.
        self.joining = None;
    }

    /// Hand the password NetworkManager was waiting for to whichever half of
    /// the join wanted it.
    fn answer(&mut self, password: Secret) {
        let Some(asking) = self.asking.take() else {
            tracing::debug!("a network password arrived with nothing waiting for it");
            return;
        };
        let Some(bus) = self.bus.as_ref() else {
            return;
        };
        let Some(point) = self.point_for(&asking.device, &asking.ssid) else {
            tracing::warn!(
                ssid = asking.ssid,
                "that network went off the air while it was being typed for"
            );
            return;
        };
        let (Some(path), Some(at)) = (object(&asking.device), object(&point.path)) else {
            return;
        };
        let Some(management) = asking.security.key_management() else {
            return;
        };

        // The password is handed over as text for exactly as long as the call
        // takes, and never copied into anything that outlives it — which is why
        // the whole message is built inside the closure rather than the string
        // being taken out of it. See [`Secret::as_text`].
        let joined = password.as_text(|secret| match asking.profile.as_deref() {
            // A profile that exists is corrected rather than replaced. Somebody
            // may have given this network a fixed address or a name server of
            // their own in another desktop, and a shell that deleted the profile
            // to change one word in it would take those with it.
            Some(profile) => {
                self.correct(profile, &asking, management, secret)
                    && object(profile).is_some_and(|profile| {
                        call(
                            bus,
                            NM_PATH,
                            NM_IFACE,
                            "ActivateConnection",
                            &(profile, path.clone(), at.clone()),
                        )
                        .is_some()
                    })
            }
            None => {
                let settings = wireless_profile(&asking.ssid, management, secret);
                call(
                    bus,
                    NM_PATH,
                    NM_IFACE,
                    "AddAndActivateConnection",
                    &(settings, path.clone(), at.clone()),
                )
                .is_some()
            }
        });
        match joined {
            Some(true) => {
                self.joining = Some((asking.device.clone(), Instant::now()));
            }
            Some(false) => tracing::warn!(ssid = asking.ssid, "the network would not be joined"),
            None => tracing::warn!("the password was not text; nothing was sent"),
        }
    }

    /// Put a new password into a saved profile, leaving everything else in it
    /// alone.
    ///
    /// What comes back from `GetSettings` is the profile without its secrets,
    /// which is exactly the thing to put the new one into: everything the user
    /// set survives, and the only key that changes is the one they just typed.
    fn correct(&self, profile: &str, asking: &Asking, management: &str, secret: &str) -> bool {
        let key = match asking.security {
            Security::Wep => "wep-key0",
            _ => "psk",
        };
        self.edit_profile(profile, |settings| {
            let security = settings.entry(WIRELESS_SECURITY.to_string()).or_default();
            security.insert("key-mgmt".to_string(), text_value(management));
            security.insert(key.to_string(), text_value(secret));
            true
        })
    }

    /// The strongest access point in the air carrying this name, on this device.
    ///
    /// Read afresh rather than taken from the published listing, because a press
    /// is answered against what is in the air *now*: the listing the user
    /// pressed against is up to two seconds old, and a repeater may have taken
    /// over in between.
    fn point_for(&self, device: &str, ssid: &str) -> Option<Point> {
        let bus = self.bus.as_ref()?;
        points(bus, device)
            .into_iter()
            .filter(|point| point.ssid == ssid)
            .max_by_key(|point| point.strength)
    }

    /// The saved profile for this network on this device, if there is one.
    fn profile_for(&self, device: &str, ssid: &str) -> Option<String> {
        let bus = self.bus.as_ref()?;
        saved(bus, device)
            .into_iter()
            .find(|(name, _)| name == ssid)
            .map(|(_, profile)| profile)
    }

    // --- reading the machine ----------------------------------------------

    /// Everything the pages are drawn from, in one pass over NetworkManager.
    fn read(&mut self) -> Listing {
        let Some(bus) = self.bus.as_ref() else {
            return Listing::none();
        };
        let Some(manager) = get_all(bus, NM_PATH, NM_IFACE) else {
            // NetworkManager is not on this bus. Not a warning: it is the
            // steady state of a machine that has none, and the page says so.
            return Listing::none();
        };
        let radio = flag(&manager, "WirelessEnabled").unwrap_or(false);
        let radio_switchable = flag(&manager, "WirelessHardwareEnabled").unwrap_or(true);

        let mut devices = Vec::new();
        let mut networks = Vec::new();
        for path in paths(&manager, "Devices") {
            let Some(device) = self.read_device(&path) else {
                continue;
            };
            if device.kind == Kind::Wireless {
                networks.push((path.clone(), self.read_networks(&path)));
            }
            devices.push(device);
        }
        // Wired first, then wireless, each half in NetworkManager's own order.
        // The socket is above the radio for the reason the Resolution page is
        // above HDR: it is the plainer thing, it is the one that needs nothing
        // chosen, and a machine with a cable in it is answered by the first row
        // on the page.
        devices.sort_by_key(|device| match device.kind {
            Kind::Wired => 0,
            Kind::Wireless => 1,
        });

        self.notice_refusals(&devices);
        self.stop_watching_a_join(&devices);

        Listing {
            manager: true,
            radio,
            radio_switchable,
            devices,
            networks,
            wanted: self.asking.as_ref().map(|asking| Wanted {
                ssid: asking.ssid.clone(),
                retry: asking.retry,
                asked: self.asked,
            }),
        }
    }

    /// Notice a join that came back wanting a password, and turn it into a
    /// question.
    ///
    /// This is the one place a panel goes up without the user having pressed
    /// anything on it, and it is where the saved-profile case is answered: a
    /// network whose password has been changed on the router is a profile
    /// NetworkManager has and cannot use, and the only way out of that is to be
    /// asked. Without a secret agent registered — which this shell deliberately
    /// is not, see the module header — NetworkManager cannot ask anybody, so it
    /// gives up with this reason and waits.
    fn notice_refusals(&mut self, devices: &[Device]) {
        if self.asking.is_some() {
            return;
        }
        let Some((device, _)) = self.joining.clone() else {
            return;
        };
        let Some(refused) = devices
            .iter()
            .find(|listed| listed.path == device && listed.kind == Kind::Wireless)
            .filter(|listed| listed.link == Link::Failed && listed.trouble.is_some())
        else {
            return;
        };
        // Only the one reason. Every other failure is reported on the page and
        // left there: a network out of range or a router that stopped answering
        // is not something another password fixes, and a shell that asked for
        // one anyway would be blaming the user for the radio.
        let Some(ssid) = refused.connection.clone() else {
            return;
        };
        let Some(point) = self.point_for(&device, &ssid) else {
            return;
        };
        if !point.security.needs_password() {
            return;
        }
        let profile = self.profile_for(&device, &ssid);
        self.want_password(&device, &ssid, point.security, profile, true);
    }

    /// Let go of a join once it has landed, or once it has had long enough.
    ///
    /// What takes the loop back to its ordinary pace, and what lets it go to
    /// sleep again when the page it was started from is shut. Without it a
    /// single press would leave the worker turning every four hundred
    /// milliseconds until the patience ran out, whether or not anything was
    /// still happening.
    fn stop_watching_a_join(&mut self, devices: &[Device]) {
        let Some((device, since)) = self.joining.as_ref() else {
            return;
        };
        let landed = devices
            .iter()
            .any(|listed| listed.path == *device && listed.link == Link::Up);
        if landed || since.elapsed() >= JOINING {
            self.joining = None;
        }
    }

    /// One interface, or `None` for one of the many kinds this shell has no
    /// page for.
    fn read_device(&self, path: &str) -> Option<Device> {
        let bus = self.bus.as_ref()?;
        let properties = get_all(bus, path, DEVICE_IFACE)?;
        let kind = match number(&properties, "DeviceType")? {
            TYPE_ETHERNET => Kind::Wired,
            TYPE_WIFI => Kind::Wireless,
            _ => return None,
        };
        // An interface NetworkManager is not in charge of is one nothing on
        // these pages could do anything to. It is left out rather than listed
        // and dimmed: a row that cannot be used and cannot be explained is
        // worse than a row that is not there.
        if !flag(&properties, "Managed").unwrap_or(false) {
            return None;
        }
        let state = number(&properties, "State").unwrap_or(0);
        let reason = state_reason(&properties);
        let (carrier, speed, hardware) = match kind {
            Kind::Wired => {
                let wired = get_all(bus, path, WIRED_IFACE).unwrap_or_default();
                (
                    flag(&wired, "Carrier"),
                    number(&wired, "Speed").unwrap_or(0),
                    text(&wired, "HwAddress"),
                )
            }
            Kind::Wireless => {
                let wireless = get_all(bus, path, WIRELESS_IFACE).unwrap_or_default();
                (
                    None,
                    // Reported in kb/s on a radio and in Mb/s on a socket. One
                    // number on the page, so the radio's is brought to the
                    // socket's unit rather than the row being taught which kind
                    // of device it is about.
                    number(&wireless, "Bitrate").unwrap_or(0) / 1000,
                    text(&wireless, "HwAddress"),
                )
            }
        };
        let addresses = self.read_addresses(&properties);
        let profile = self.profile_of(path);
        let ipv4 = match profile.as_deref() {
            Some(profile) => self.read_ipv4(profile, &addresses),
            None => Ipv4::default(),
        };
        Some(Device {
            path: path.to_string(),
            interface: text(&properties, "Interface").unwrap_or_default(),
            kind,
            link: link_of(state),
            trouble: trouble_of(state, reason, kind, carrier),
            connection: self.read_connection(&properties),
            address: addresses.address,
            gateway: addresses.gateway,
            nameservers: addresses.nameservers,
            hardware,
            carrier,
            speed,
            profile,
            ipv4,
        })
    }

    /// What one profile asks for of IPv4.
    ///
    /// `method` absent is automatic, because that is what `NetworkManager`
    /// means by it: a profile that has never said gets DHCP. Everything else it
    /// carries is read as it is written, and nothing is filled in — a profile
    /// with no gateway has no gateway, rather than the one the interface
    /// happens to be using.
    fn read_ipv4(&self, profile: &str, running: &Addresses) -> Ipv4 {
        let Some(bus) = self.bus.as_ref() else {
            return Ipv4::default();
        };
        let Some(reply) = call(bus, profile, CONNECTION_IFACE, "GetSettings", &()) else {
            return Ipv4::default();
        };
        let Ok(settings) = reply
            .body()
            .deserialize::<HashMap<String, HashMap<String, OwnedValue>>>()
        else {
            return Ipv4::default();
        };
        let empty = HashMap::new();
        let ipv4 = settings.get("ipv4").unwrap_or(&empty);
        let address = dicts(ipv4, "address-data").into_iter().find_map(|entry| {
            let address = text(&entry, "address")?;
            match number(&entry, "prefix") {
                Some(prefix) => Some(format!("{address}/{prefix}")),
                None => Some(address),
            }
        });
        Ipv4 {
            automatic: text(ipv4, "method").as_deref().unwrap_or("auto") != "manual",
            can_pin: address.is_some() || running.address.is_some(),
            address,
            gateway: text(ipv4, "gateway").filter(|gateway| !gateway.is_empty()),
            dns_automatic: !flag(ipv4, "ignore-auto-dns").unwrap_or(false),
            dns: numbers(ipv4, "dns")
                .into_iter()
                .map(|packed| unpacked(packed).to_string())
                .collect(),
        }
    }

    /// What the profile in force is called.
    ///
    /// For a wireless device that is the network's own name, which is what
    /// makes it worth reading: it is how the page says which network is joined
    /// without having to match an access point against the listing.
    fn read_connection(&self, properties: &HashMap<String, OwnedValue>) -> Option<String> {
        let bus = self.bus.as_ref()?;
        let active = path(properties, "ActiveConnection")?;
        let properties = get_all(bus, &active, ACTIVE_IFACE)?;
        text(&properties, "Id")
    }

    /// The address this machine answers on through one device, and how it gets
    /// off it.
    ///
    /// IPv4 first and IPv6 only where there is no IPv4 at all, which is the same
    /// rule [`crate::machine`] reads the machine's own address by: the address a
    /// person means when they ask a console what it is on is the one they would
    /// type into another machine beside it.
    fn read_addresses(&self, properties: &HashMap<String, OwnedValue>) -> Addresses {
        let Some(bus) = self.bus.as_ref() else {
            return Addresses::default();
        };
        for (name, interface) in [("Ip4Config", IP4_IFACE), ("Ip6Config", IP6_IFACE)] {
            let Some(config) = path(properties, name) else {
                continue;
            };
            let Some(read) = get_all(bus, &config, interface) else {
                continue;
            };
            let found = Addresses {
                address: dicts(&read, "AddressData").into_iter().find_map(|entry| {
                    let address = text(&entry, "address")?;
                    match number(&entry, "prefix") {
                        Some(prefix) => Some(format!("{address}/{prefix}")),
                        None => Some(address),
                    }
                }),
                gateway: text(&read, "Gateway").filter(|gateway| !gateway.is_empty()),
                nameservers: dicts(&read, "NameserverData")
                    .into_iter()
                    .filter_map(|entry| text(&entry, "address"))
                    .collect(),
            };
            if found.address.is_some() {
                return found;
            }
        }
        Addresses::default()
    }

    /// Every network one radio can hear, strongest first and one row per name.
    ///
    /// See the sort at the end of it, and [`BANDS`].
    fn read_networks(&mut self, device: &str) -> Vec<Network> {
        self.rescan(device);
        let Some(bus) = self.bus.as_ref() else {
            return Vec::new();
        };
        let saved: Vec<(String, String)> = saved(bus, device);
        let joined = get_all(bus, device, WIRELESS_IFACE)
            .as_ref()
            .and_then(|wireless| path(wireless, "ActiveAccessPoint"));

        let mut networks: Vec<Network> = Vec::new();
        for point in points(bus, device) {
            // A name nobody published is a hidden network, which cannot be
            // joined by pressing a row: what joining it takes is typing the
            // name, and there is nothing to press. Left out rather than listed
            // as a blank row.
            if point.ssid.is_empty() {
                continue;
            }
            let on_it = joined.as_deref() == Some(point.path.as_str());
            // One row per name. A house with a repeater in it publishes the
            // same network from two radios, and they are one thing to join —
            // so the row keeps the strongest of them, which is the one the
            // radio would actually use, and is marked as joined if either of
            // them is the one in force.
            match networks
                .iter_mut()
                .find(|network| network.ssid == point.ssid)
            {
                Some(network) => {
                    network.joined |= on_it;
                    if point.strength > network.strength {
                        network.strength = point.strength;
                        network.frequency = point.frequency;
                        network.security = point.security;
                    }
                }
                None => networks.push(Network {
                    saved: saved.iter().any(|(name, _)| *name == point.ssid),
                    ssid: point.ssid,
                    strength: point.strength,
                    security: point.security,
                    joined: on_it,
                    frequency: point.frequency,
                }),
            }
        }
        // The one in force first, then by strength, and by name inside a band
        // of it. A list that put the joined network wherever its signal
        // happened to fall would move the row the user came to look at every
        // time the radio breathed.
        //
        // `BANDS` is the whole of why this is not a plain comparison of
        // strengths. The page is a live list — it is rebuilt every time the
        // radio reports, which is constantly — and two networks a percent apart
        // trade places on every breath, under a cursor that may be standing in
        // one of them. That was survivable while every row here was a press
        // that joins; it is not, now that one of them is stepped into and one of
        // the things behind it deletes something. Rounding to the five bars a
        // signal is drawn with anywhere else makes the order change only when
        // the signal really does, and puts names rather than noise in charge of
        // the rest.
        networks.sort_by(|left, right| {
            right
                .joined
                .cmp(&left.joined)
                .then((right.strength / BANDS).cmp(&(left.strength / BANDS)))
                .then(left.ssid.cmp(&right.ssid))
        });
        networks
    }

    /// How strong the wireless link is, in the bands the corner draws it in —
    /// or `None` for a machine that is on no wireless network at all, which is
    /// what makes the corner say nothing about one.
    ///
    /// Three calls on the ordinary machine: what the radio is doing, which
    /// access point it is on, and how strongly that point is heard. That is the
    /// whole reason this exists beside [`Self::read`], which answers the same
    /// question among thirty others — the mark is on screen for the length of a
    /// session and the page is on screen for a minute of it.
    ///
    /// A machine with two radios shows the better of them, because that is the
    /// one the traffic is on. `Link::Up` and not merely an access point:
    /// NetworkManager names the point it is *associating* with well before the
    /// network can carry anything, and a mark that appeared then would be
    /// telling the user they were on a network they could not yet use.
    fn read_signal(&mut self) -> Option<Signal> {
        self.find_radios();
        let bus = self.bus.as_ref()?;
        let mut answered = false;
        let mut strength: Option<u8> = None;
        for radio in &self.radios {
            let Some(device) = get_all(bus, radio, DEVICE_IFACE) else {
                continue;
            };
            answered = true;
            if link_of(number(&device, "State").unwrap_or(0)) != Link::Up {
                continue;
            }
            let Some(wireless) = get_all(bus, radio, WIRELESS_IFACE) else {
                continue;
            };
            let Some(point) = path(&wireless, "ActiveAccessPoint") else {
                continue;
            };
            let Some(point) = get_all(bus, &point, POINT_IFACE) else {
                continue;
            };
            let heard = number(&point, "Strength").unwrap_or(0).min(100) as u8;
            strength = Some(strength.map_or(heard, |had| had.max(heard)));
        }
        // Every radio this was holding has stopped answering: the card is out,
        // or NetworkManager has been restarted and its objects are on new
        // paths. Look again on the next pass rather than in thirty seconds —
        // this is a machine that had a radio a moment ago.
        if !self.radios.is_empty() && !answered {
            self.radios.clear();
            self.sought = None;
        }
        Some(band_of(strength?, self.band))
    }

    /// Find the radios in this machine, unless they are already known.
    ///
    /// Looked for again after [`RADIO_SEARCH`] on a machine where there are
    /// none, and not on every pass: walking the manager's device list is a call
    /// per device, and a desktop with a socket and no radio would pay it for
    /// the whole of a session to be told the same thing. Looked for *at all*,
    /// though, because a dongle plugged in halfway through a session is a radio
    /// this machine now has.
    fn find_radios(&mut self) {
        if !self.radios.is_empty() {
            return;
        }
        if self
            .sought
            .is_some_and(|when| when.elapsed() < RADIO_SEARCH)
        {
            return;
        }
        let Some(bus) = self.bus.as_ref() else {
            return;
        };
        self.sought = Some(Instant::now());
        let Some(manager) = get_all(bus, NM_PATH, NM_IFACE) else {
            return;
        };
        self.radios = paths(&manager, "Devices")
            .into_iter()
            .filter(|device| {
                get_all(bus, device, DEVICE_IFACE)
                    .and_then(|properties| number(&properties, "DeviceType"))
                    == Some(TYPE_WIFI)
            })
            .collect();
    }

    /// Ask a radio to look around, if it has not lately.
    fn rescan(&mut self, device: &str) {
        let Some(bus) = self.bus.as_ref() else {
            return;
        };
        let now = Instant::now();
        match self.scanned.iter_mut().find(|(path, _)| path == device) {
            Some((_, when)) if when.elapsed() < RESCAN => return,
            Some((_, when)) => *when = now,
            None => self.scanned.push((device.to_string(), now)),
        }
        // The empty dictionary is the whole request: scan everything. Failure is
        // ordinary and is not reported — NetworkManager refuses a scan that
        // follows too closely on the last one, and a radio that is busy joining
        // refuses outright. Both are answered by the listing being what it was.
        let options: HashMap<&str, Value<'_>> = HashMap::new();
        if call(bus, device, WIRELESS_IFACE, "RequestScan", &(options,)).is_none() {
            tracing::trace!(device, "the radio would not look around");
        }
    }
}

// --- what a device is doing, in words --------------------------------------

/// Where the addresses a device was given live, gathered in one value because
/// they are read in one go and are one answer.
#[derive(Debug, Default)]
struct Addresses {
    address: Option<String>,
    gateway: Option<String>,
    nameservers: Vec<String>,
}

/// One access point, as NetworkManager reports it — before the ones publishing
/// the same name are folded into one [`Network`].
#[derive(Debug, Clone)]
struct Point {
    path: String,
    ssid: String,
    strength: u8,
    security: Security,
    frequency: u32,
}

/// `NMDeviceState`, reduced to what a page has anything to say about.
fn link_of(state: u32) -> Link {
    match state {
        STATE_UNMANAGED | STATE_UNAVAILABLE => Link::Unavailable,
        STATE_DISCONNECTED => Link::Idle,
        STATE_FAILED => Link::Failed,
        STATE_ACTIVATED => Link::Up,
        STATE_DEACTIVATING => Link::Working,
        // Everything between disconnected and activated is a step on the way
        // there — preparing, configuring, waiting for an address — and they are
        // one thing to the person watching. `NEED_AUTH` is among them: it is
        // where NetworkManager stops to ask for a secret, which for this shell
        // is a moment and not a state.
        state if state > STATE_DISCONNECTED && state < STATE_ACTIVATED => Link::Working,
        _ => Link::Unavailable,
    }
}

/// Why a device is not up, in the words the row has room for.
///
/// `None` where there is nothing worth saying: a device that is up, and a
/// device that is merely idle because nobody has asked it for anything. An idle
/// socket is not in trouble, and a row that explained it would be a shell
/// apologising for doing what it was told.
fn trouble_of(state: u32, reason: u32, kind: Kind, carrier: Option<bool>) -> Option<String> {
    // The cable outranks everything. A socket with nothing in it has one fact
    // about it and every other explanation is downstream of that one.
    if kind == Kind::Wired && carrier == Some(false) {
        return Some("No cable".to_string());
    }
    match (state, reason) {
        (STATE_FAILED, REASON_NO_SECRETS) | (STATE_NEED_AUTH, REASON_NO_SECRETS) => {
            Some("The password was not accepted".to_string())
        }
        (STATE_FAILED, _) => Some("Could not connect".to_string()),
        (STATE_UNAVAILABLE, _) if kind == Kind::Wireless => {
            Some("The wireless radio is off".to_string())
        }
        (STATE_UNAVAILABLE, _) => Some("Not ready".to_string()),
        (STATE_UNMANAGED, _) => Some("Something else is in charge of it".to_string()),
        _ => None,
    }
}

/// Which of the five kinds of protection an access point is advertising.
///
/// Read from the three numbers NetworkManager publishes rather than from any
/// one of them, because none says the whole thing: the flags say only that
/// there is *some* privacy, and which of WPA2 and WPA3 it is lives in the key
/// management bits of the RSN half.
fn security_of(flags: u32, wpa: u32, rsn: u32) -> Security {
    const PRIVACY: u32 = 0x1;
    const KEY_MGMT_PSK: u32 = 0x100;
    const KEY_MGMT_802_1X: u32 = 0x200;
    const KEY_MGMT_SAE: u32 = 0x400;
    const KEY_MGMT_OWE: u32 = 0x800;
    const KEY_MGMT_OWE_TM: u32 = 0x1000;
    const KEY_MGMT_EAP_SUITE_B: u32 = 0x2000;

    let both = wpa | rsn;
    if both & (KEY_MGMT_802_1X | KEY_MGMT_EAP_SUITE_B) != 0 {
        return Security::Enterprise;
    }
    // Opportunistic wireless encryption: encrypted, and joined by anybody, so
    // it is an open network from the only side the user is on.
    if both & (KEY_MGMT_OWE | KEY_MGMT_OWE_TM) != 0 {
        return Security::Open;
    }
    // A network offering both is joined as WPA2, which is what every device on
    // it is speaking; the WPA3 name is kept for the ones that offer only that.
    if both & KEY_MGMT_PSK != 0 {
        return Security::Personal;
    }
    if both & KEY_MGMT_SAE != 0 {
        return Security::Modern;
    }
    // Privacy with neither a WPA nor an RSN element is WEP, which is the only
    // thing left that it can be.
    if flags & PRIVACY != 0 {
        return Security::Wep;
    }
    Security::Open
}

/// The profile a new wireless network is joined on.
///
/// Deliberately the four keys and no more. NetworkManager fills in everything
/// else — the mode, the band, the UUID, whether to come back to it — and every
/// key written here is one this shell would then own for ever.
fn wireless_profile<'a>(
    ssid: &'a str,
    management: &'a str,
    secret: &'a str,
) -> HashMap<&'a str, HashMap<&'a str, Value<'a>>> {
    let mut settings = HashMap::new();
    settings.insert(
        "connection",
        HashMap::from([
            ("id", Value::from(ssid)),
            ("type", Value::from("802-11-wireless")),
        ]),
    );
    settings.insert(
        "802-11-wireless",
        HashMap::from([("ssid", Value::from(ssid.as_bytes()))]),
    );
    // WEP has no pre-shared key: the key itself is the whole of it, and it goes
    // in a differently named field.
    let key = if management == "none" {
        "wep-key0"
    } else {
        "psk"
    };
    settings.insert(
        WIRELESS_SECURITY,
        HashMap::from([
            ("key-mgmt", Value::from(management)),
            (key, Value::from(secret)),
        ]),
    );
    settings
}

const WIRELESS_SECURITY: &str = "802-11-wireless-security";

// --- addresses, as text and as NetworkManager keeps them --------------------

/// What is wrong with what somebody typed, if anything is.
///
/// Checked in the shell while the panel is still on screen, rather than by
/// handing it to `NetworkManager` and reporting what came back. Two reasons and
/// both matter on a console: the answer arrives at the moment of the press
/// instead of a second later with the panel already gone, and it is a sentence
/// about what was typed rather than a daemon's complaint about a property name.
///
/// Empty is never a fault. Every one of these can be cleared, and clearing one
/// is a thing to want — see [`Worker::write_field`], which says what each of
/// them means when it is empty.
pub fn fault(field: Field, text: &str) -> Option<&'static str> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    match field {
        Field::Address => match split_address(text) {
            // A bare address with no prefix is the commonest thing to type and
            // is not an address: 192.168.1.50 says nothing about how much of
            // the network is local, and NetworkManager would have to guess.
            None if !text.contains('/') => Some("Add the size of the network, as 192.168.1.50/24."),
            None => Some("That is not an address. Type it as 192.168.1.50/24."),
            Some(_) => None,
        },
        Field::Router => match text.parse::<std::net::Ipv4Addr>() {
            Ok(_) => None,
            Err(_) => Some("That is not an address. Type it as 192.168.1.1."),
        },
        Field::Dns => {
            let named = text
                .split([',', ' '])
                .filter(|part| !part.trim().is_empty());
            match named.count() == servers_of(text).len() {
                true => None,
                false => Some("Those are not all addresses. Separate them with commas."),
            }
        }
    }
}

/// An address and the size of its network, out of what was typed.
///
/// Both halves or neither: an address with no prefix is not an address this
/// page can use, and inventing 24 for it would be the shell deciding how much
/// of somebody's network is local.
fn split_address(text: &str) -> Option<(String, u32)> {
    let (address, prefix) = text.trim().split_once('/')?;
    let address: std::net::Ipv4Addr = address.trim().parse().ok()?;
    let prefix: u32 = prefix.trim().parse().ok()?;
    (prefix <= 32).then(|| (address.to_string(), prefix))
}

/// The name servers in a typed list, in the order they were typed.
///
/// Commas or spaces, because both are what people type and neither is worth
/// correcting them about. Anything that is not an address is dropped here and
/// reported by [`fault`], which counts what it dropped.
fn servers_of(text: &str) -> Vec<std::net::Ipv4Addr> {
    text.split([',', ' '])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse().ok())
        .collect()
}

/// One IPv4 address as `NetworkManager` keeps it in a profile: the four octets
/// in network order, held in a number.
///
/// Native byte order and not little-endian, though on every machine this runs
/// on those are the same thing. What is on the other end is `memcpy`ed into an
/// `in_addr`, whose four bytes have to come out as the address reads — so what
/// has to be sent is the number those four bytes *are* on this machine, which
/// is what `from_ne_bytes` says and `from_le_bytes` only says by accident.
fn packed(address: std::net::Ipv4Addr) -> u32 {
    u32::from_ne_bytes(address.octets())
}

fn unpacked(packed: u32) -> std::net::Ipv4Addr {
    std::net::Ipv4Addr::from(packed.to_ne_bytes())
}

/// Write an address into a profile's IPv4 section, in the spelling
/// `NetworkManager` reads.
fn write_address(ipv4: &mut HashMap<String, OwnedValue>, address: &str, prefix: u32) {
    let entry: HashMap<&str, Value<'_>> = HashMap::from([
        ("address", Value::from(address.to_string())),
        ("prefix", Value::from(prefix)),
    ]);
    if let Ok(value) = OwnedValue::try_from(Value::from(vec![entry])) {
        ipv4.insert("address-data".to_string(), value);
    }
}

fn text_value(text: &str) -> OwnedValue {
    OwnedValue::from(zbus::zvariant::Str::from(text).to_owned())
}

fn bool_value(flag: bool) -> OwnedValue {
    OwnedValue::from(flag)
}

fn numbers_value(numbers: &[u32]) -> OwnedValue {
    OwnedValue::try_from(Value::from(numbers.to_vec())).unwrap_or_else(|_| bool_value(false))
}

// --- reading NetworkManager ------------------------------------------------

/// Every access point one radio can hear.
fn points(bus: &zbus::blocking::Connection, device: &str) -> Vec<Point> {
    let Some(wireless) = get_all(bus, device, WIRELESS_IFACE) else {
        return Vec::new();
    };
    paths(&wireless, "AccessPoints")
        .into_iter()
        .filter_map(|path| {
            let properties = get_all(bus, &path, POINT_IFACE)?;
            Some(Point {
                ssid: String::from_utf8(bytes(&properties, "Ssid")?).ok()?,
                strength: number(&properties, "Strength").unwrap_or(0).min(100) as u8,
                security: security_of(
                    number(&properties, "Flags").unwrap_or(0),
                    number(&properties, "WpaFlags").unwrap_or(0),
                    number(&properties, "RsnFlags").unwrap_or(0),
                ),
                frequency: number(&properties, "Frequency").unwrap_or(0),
                path,
            })
        })
        .collect()
}

/// The profiles this device could be brought up on right now.
fn available(bus: &zbus::blocking::Connection, device: &str) -> Vec<String> {
    get_all(bus, device, DEVICE_IFACE)
        .map(|properties| paths(&properties, "AvailableConnections"))
        .unwrap_or_default()
}

/// The wireless networks this machine already has a profile for, of the ones
/// this radio can reach: the name in the air, and the profile it is filed under.
///
/// Asked of the device rather than of the whole settings list, because the
/// device's own answer is already narrowed to the profiles that could be used
/// here — which is the only sense in which a network is "saved" from the page's
/// point of view.
fn saved(bus: &zbus::blocking::Connection, device: &str) -> Vec<(String, String)> {
    available(bus, device)
        .into_iter()
        .filter_map(|profile| {
            let reply = call(bus, &profile, CONNECTION_IFACE, "GetSettings", &())?;
            let settings = reply
                .body()
                .deserialize::<HashMap<String, HashMap<String, OwnedValue>>>()
                .ok()?;
            let wireless = settings.get("802-11-wireless")?;
            let ssid = String::from_utf8(bytes(wireless, "ssid")?).ok()?;
            Some((ssid, profile))
        })
        .collect()
}

/// Every property of one interface on one object, in one call.
fn get_all(
    bus: &zbus::blocking::Connection,
    path: &str,
    interface: &str,
) -> Option<HashMap<String, OwnedValue>> {
    call(bus, path, PROPERTIES, "GetAll", &(interface,))?
        .body()
        .deserialize()
        .ok()
}

/// Write one property.
fn set_property(
    bus: &zbus::blocking::Connection,
    path: &str,
    interface: &str,
    name: &str,
    value: Value<'_>,
) -> bool {
    // The value goes in as a `Value` and not wrapped in anything: `Set` takes a
    // variant, and that is what a `Value` serialises as.
    call(bus, path, PROPERTIES, "Set", &(interface, name, value)).is_some()
}

/// One call to NetworkManager, with whatever went wrong logged and swallowed.
///
/// Swallowed on purpose. Every one of these has an answer on the page — a
/// listing that stays as it was, a device that does not come up — and there is
/// no level of this module at which a D-Bus error is anything but that. What it
/// must never do is take the session down: `polkitd` refusing a change and
/// NetworkManager going away mid-session are both ordinary.
fn call<Body>(
    bus: &zbus::blocking::Connection,
    path: &str,
    interface: &str,
    method: &str,
    body: &Body,
) -> Option<zbus::Message>
where
    Body: serde::ser::Serialize + zbus::zvariant::DynamicType,
{
    match bus.call_method(Some(NM), path, Some(interface), method, body) {
        Ok(reply) => Some(reply),
        Err(err) => {
            tracing::debug!(path, interface, method, ?err, "NetworkManager refused");
            None
        }
    }
}

fn number(properties: &HashMap<String, OwnedValue>, name: &str) -> Option<u32> {
    let value = properties.get(name)?;
    // Read through every width NetworkManager uses for a small number, because
    // one property's `y` is another's `u` and the page does not care which.
    u32::try_from(value)
        .or_else(|_| u8::try_from(value).map(u32::from))
        .or_else(|_| i32::try_from(value).map(|number| number.max(0) as u32))
        .ok()
}

fn flag(properties: &HashMap<String, OwnedValue>, name: &str) -> Option<bool> {
    bool::try_from(properties.get(name)?).ok()
}

fn text(properties: &HashMap<String, OwnedValue>, name: &str) -> Option<String> {
    String::try_from(properties.get(name)?.try_clone().ok()?).ok()
}

fn path(properties: &HashMap<String, OwnedValue>, name: &str) -> Option<String> {
    let value = OwnedObjectPath::try_from(properties.get(name)?.try_clone().ok()?).ok()?;
    let path = value.as_str().to_string();
    // The empty path — spelled `/` — is how NetworkManager says "nothing", and
    // a caller that treated it as an object would ask for the properties of the
    // bus root.
    (path != "/").then_some(path)
}

fn paths(properties: &HashMap<String, OwnedValue>, name: &str) -> Vec<String> {
    let Some(value) = properties
        .get(name)
        .and_then(|value| value.try_clone().ok())
    else {
        return Vec::new();
    };
    Vec::<OwnedObjectPath>::try_from(value)
        .map(|paths| {
            paths
                .into_iter()
                .map(|path| path.as_str().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// An array of numbers — how `NetworkManager` keeps a profile's name servers.
fn numbers(properties: &HashMap<String, OwnedValue>, name: &str) -> Vec<u32> {
    let Some(value) = properties
        .get(name)
        .and_then(|value| value.try_clone().ok())
    else {
        return Vec::new();
    };
    Vec::<u32>::try_from(value).unwrap_or_default()
}

fn bytes(properties: &HashMap<String, OwnedValue>, name: &str) -> Option<Vec<u8>> {
    let value = properties.get(name)?.try_clone().ok()?;
    Vec::<u8>::try_from(value).ok()
}

/// An array of dictionaries — how NetworkManager reports addresses and name
/// servers, one dictionary per entry.
fn dicts(properties: &HashMap<String, OwnedValue>, name: &str) -> Vec<HashMap<String, OwnedValue>> {
    let Some(value) = properties
        .get(name)
        .and_then(|value| value.try_clone().ok())
    else {
        return Vec::new();
    };
    Vec::<HashMap<String, OwnedValue>>::try_from(value).unwrap_or_default()
}

/// The reason a device is in the state it is in.
///
/// `StateReason` is a pair — the state it applies to and the reason — and only
/// the second half is of any use here, because the state itself is read from
/// the property beside it.
fn state_reason(properties: &HashMap<String, OwnedValue>) -> u32 {
    let Some(value) = properties
        .get("StateReason")
        .and_then(|value| value.try_clone().ok())
    else {
        return 0;
    };
    <(u32, u32)>::try_from(value)
        .map(|(_, why)| why)
        .unwrap_or(0)
}

/// A D-Bus object path, or nothing if the string is not one.
fn object(path: &str) -> Option<zbus::zvariant::ObjectPath<'static>> {
    zbus::zvariant::ObjectPath::try_from(path.to_string()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every one of these is written by hand from the numbers in
    /// `NetworkManager`'s own headers. Nothing this machine is on belongs in
    /// this file: a network name or an access point pasted out of a live scan
    /// is a test that says the feature was built for one desk.
    const PRIVACY: u32 = 0x1;
    const PAIR_CCMP: u32 = 0x8;
    const GROUP_CCMP: u32 = 0x80;
    const PSK: u32 = 0x100;
    const EAP: u32 = 0x200;
    const SAE: u32 = 0x400;
    const OWE: u32 = 0x800;

    /// Three bands, from a percentage: which of the corner's three fans a
    /// reading is drawn with when there is no band in force yet.
    #[test]
    fn how_strong_a_link_is_read_as_three_bands() {
        assert_eq!(band_of(100, None), Signal::Strong);
        assert_eq!(band_of(STRONG_AT, None), Signal::Strong);
        assert_eq!(band_of(STRONG_AT - 1, None), Signal::Fair);
        assert_eq!(band_of(FAIR_AT, None), Signal::Fair);
        assert_eq!(band_of(FAIR_AT - 1, None), Signal::Weak);
        // Nothing at all is still a band, because it is still a connection:
        // whether one is drawn at all is decided by there being a strength to
        // read, not by the number in it. See [`Worker::read_signal`].
        assert_eq!(band_of(0, None), Signal::Weak);
    }

    /// A band is harder to leave than it was to enter.
    ///
    /// This is the whole of what keeps the mark still. A radio's own report of
    /// one unchanged connection wanders a point or two between readings, so a
    /// machine sitting on a boundary would otherwise have a corner that flicked
    /// between two pictures every five seconds for as long as it sat there —
    /// which is exactly the fault the network list's own ordering had to be
    /// banded to avoid.
    #[test]
    fn a_band_is_harder_to_leave_than_it_was_to_enter() {
        // On the edge of Strong, in Strong: it stays there until the reading
        // has fallen clear of the edge by the guard.
        let held = Some(Signal::Strong);
        assert_eq!(band_of(STRONG_AT, held), Signal::Strong);
        assert_eq!(band_of(STRONG_AT - 1, held), Signal::Strong);
        assert_eq!(band_of(STRONG_AT - SIGNAL_GUARD, held), Signal::Strong);
        assert_eq!(band_of(STRONG_AT - SIGNAL_GUARD - 1, held), Signal::Fair);

        // And from below, the same edge has to be cleared by the guard before
        // another arc lights.
        let held = Some(Signal::Fair);
        assert_eq!(band_of(STRONG_AT, held), Signal::Fair);
        assert_eq!(band_of(STRONG_AT + SIGNAL_GUARD - 1, held), Signal::Fair);
        assert_eq!(band_of(STRONG_AT + SIGNAL_GUARD, held), Signal::Strong);

        // A jump of two bands is still a jump of two: a radio that has just come
        // back into range is not made to climb the fan a pass at a time.
        assert_eq!(
            band_of(100, Some(Signal::Weak)),
            Signal::Strong,
            "a signal that has arrived in full is drawn in full",
        );
        // But only as far as the guarded edges allow. A reading inside Strong by
        // less than the guard, from Weak, is Fair — the band it has properly
        // cleared into — rather than either end of the range.
        assert_eq!(
            band_of(STRONG_AT + 1, Some(Signal::Weak)),
            Signal::Fair,
            "an edge is an edge from however far below it",
        );
    }

    /// The five answers, from the three numbers that carry them.
    ///
    /// The one that matters most is the last: privacy with neither a WPA nor an
    /// RSN element is WEP and nothing else, and a reading that called it open
    /// would have the shell join without a password and then report that the
    /// network refused it.
    #[test]
    fn what_a_network_is_protected_by() {
        assert_eq!(security_of(0, 0, 0), Security::Open);
        assert_eq!(
            security_of(PRIVACY, 0, PAIR_CCMP | GROUP_CCMP | PSK),
            Security::Personal
        );
        assert_eq!(
            security_of(PRIVACY, PAIR_CCMP | PSK, PAIR_CCMP | GROUP_CCMP | PSK),
            Security::Personal,
            "a WPA element as well as an RSN one is still one shared password"
        );
        assert_eq!(security_of(PRIVACY, 0, SAE), Security::Modern);
        assert_eq!(
            security_of(PRIVACY, 0, PSK | SAE),
            Security::Personal,
            "a network offering both is joined the way every device on it is"
        );
        assert_eq!(security_of(PRIVACY, 0, EAP), Security::Enterprise);
        assert_eq!(
            security_of(PRIVACY, 0, EAP | PSK),
            Security::Enterprise,
            "802.1X outranks a shared password: the password would not get on"
        );
        assert_eq!(
            security_of(PRIVACY, 0, OWE),
            Security::Open,
            "encrypted and joined by anybody is open from the only side we are on"
        );
        assert_eq!(
            security_of(PRIVACY, 0, 0),
            Security::Wep,
            "privacy with no WPA and no RSN can only be WEP"
        );
    }

    /// What each of them takes, and which of them this shell will attempt at
    /// all.
    #[test]
    fn what_joining_one_takes() {
        for security in [Security::Wep, Security::Personal, Security::Modern] {
            assert!(security.needs_password(), "{security:?}");
            assert!(security.joinable(), "{security:?}");
            assert!(security.key_management().is_some(), "{security:?}");
        }
        assert!(!Security::Open.needs_password());
        assert!(Security::Open.joinable());
        assert!(Security::Open.key_management().is_none());

        // The one the page lists and will not join. It must not be reported as
        // needing a password: a panel collecting one for it would be collecting
        // something nothing is ever going to be given.
        assert!(!Security::Enterprise.joinable());
        assert!(!Security::Enterprise.needs_password());
        assert!(Security::Enterprise.key_management().is_none());
    }

    /// The device states, reduced to what a page has anything to say about.
    #[test]
    fn what_a_device_is_doing() {
        assert_eq!(link_of(STATE_UNMANAGED), Link::Unavailable);
        assert_eq!(link_of(STATE_UNAVAILABLE), Link::Unavailable);
        assert_eq!(link_of(STATE_DISCONNECTED), Link::Idle);
        assert_eq!(link_of(STATE_ACTIVATED), Link::Up);
        assert_eq!(link_of(STATE_FAILED), Link::Failed);
        assert_eq!(link_of(STATE_DEACTIVATING), Link::Working);
        // Every step between disconnected and activated is one thing to the
        // person watching: preparing, configuring, waiting for an address, and
        // the moment `NetworkManager` stops to want a secret.
        for state in [40, 50, STATE_NEED_AUTH, 70, 80, 90] {
            assert_eq!(link_of(state), Link::Working, "state {state}");
        }
        // A state nothing has ever reported is not a state to guess about.
        assert_eq!(link_of(0), Link::Unavailable);
    }

    /// What is said about a device that is not up — and, as much to the point,
    /// what is not said about one that is.
    #[test]
    fn why_a_device_is_not_up() {
        // An empty socket has one fact about it, and it outranks every
        // explanation downstream of it.
        assert_eq!(
            trouble_of(STATE_UNAVAILABLE, 0, Kind::Wired, Some(false)).as_deref(),
            Some("No cable")
        );
        assert_eq!(
            trouble_of(STATE_FAILED, REASON_NO_SECRETS, Kind::Wired, Some(false)).as_deref(),
            Some("No cable"),
            "the cable is the answer even when something else failed first"
        );
        assert_eq!(
            trouble_of(STATE_FAILED, REASON_NO_SECRETS, Kind::Wireless, None).as_deref(),
            Some("The password was not accepted")
        );
        assert_eq!(
            trouble_of(STATE_FAILED, 1, Kind::Wireless, None).as_deref(),
            Some("Could not connect")
        );
        assert_eq!(
            trouble_of(STATE_UNAVAILABLE, 0, Kind::Wireless, None).as_deref(),
            Some("The wireless radio is off")
        );
        assert_eq!(
            trouble_of(STATE_UNMANAGED, 0, Kind::Wired, Some(true)).as_deref(),
            Some("Something else is in charge of it")
        );

        // Nothing at all for a device that is up, and nothing for one that is
        // merely idle: a socket nobody has asked for anything is not in
        // trouble, and a row explaining it would be the shell apologising for
        // doing what it was told.
        assert_eq!(
            trouble_of(STATE_ACTIVATED, 0, Kind::Wired, Some(true)),
            None
        );
        assert_eq!(
            trouble_of(STATE_DISCONNECTED, 0, Kind::Wireless, None),
            None
        );
        assert_eq!(trouble_of(STATE_NEED_AUTH, 0, Kind::Wireless, None), None);
    }

    /// The profile a new network is joined on carries the four keys it needs
    /// and nothing else: every key written here is one this shell would then
    /// own for ever on the user's machine.
    #[test]
    fn the_profile_a_new_network_is_joined_on() {
        let profile = wireless_profile("A Network", "wpa-psk", "hunter2");
        assert_eq!(
            {
                let mut sections: Vec<&str> = profile.keys().copied().collect();
                sections.sort();
                sections
            },
            ["802-11-wireless", WIRELESS_SECURITY, "connection"]
        );
        assert_eq!(
            profile["connection"]["type"],
            Value::from("802-11-wireless")
        );
        assert_eq!(profile["connection"]["id"], Value::from("A Network"));
        // The name goes in as the bytes it is in the air, not as text: an SSID
        // is a byte string and `NetworkManager` will only take it as one.
        assert_eq!(
            profile["802-11-wireless"]["ssid"],
            Value::from("A Network".as_bytes())
        );
        assert_eq!(profile[WIRELESS_SECURITY]["psk"], Value::from("hunter2"));
        assert!(!profile[WIRELESS_SECURITY].contains_key("wep-key0"));

        // WEP has no pre-shared key: the key itself is the whole of it, and it
        // goes in a differently named field. A profile that put a WEP key under
        // `psk` is one `NetworkManager` rejects outright.
        let old = wireless_profile("A Network", "none", "hunter2");
        assert_eq!(old[WIRELESS_SECURITY]["wep-key0"], Value::from("hunter2"));
        assert!(!old[WIRELESS_SECURITY].contains_key("psk"));
    }

    /// A listing answers about one kind of device and one device's air without
    /// the caller having to search either.
    #[test]
    fn a_listing_answers_about_one_device() {
        let listing = Listing {
            manager: true,
            radio: true,
            radio_switchable: true,
            devices: vec![wired_device("a-socket"), wireless_device("a-radio")],
            networks: vec![("a-radio".to_string(), vec![network("Upstairs")])],
            wanted: None,
        };
        assert_eq!(
            listing
                .of(Kind::Wired)
                .iter()
                .map(|device| device.path.as_str())
                .collect::<Vec<_>>(),
            ["a-socket"]
        );
        assert_eq!(
            listing
                .of(Kind::Wireless)
                .iter()
                .map(|device| device.path.as_str())
                .collect::<Vec<_>>(),
            ["a-radio"]
        );
        assert_eq!(listing.networks_of("a-radio").len(), 1);
        // A device with no air of its own hears nothing, rather than hearing
        // the first entry in the list.
        assert!(listing.networks_of("a-socket").is_empty());
        assert!(listing.networks_of("nothing of the sort").is_empty());
    }

    /// Nothing, which is what a session with no worker thread keeps, says
    /// nothing about the machine rather than saying the machine has nothing.
    #[test]
    fn nothing_read_yet_is_not_a_machine_with_no_network() {
        let nothing = Listing::none();
        assert!(!nothing.manager);
        assert!(nothing.devices.is_empty());
        assert_eq!(nothing, Listing::default());
    }

    /// What is refused, and what is not.
    ///
    /// The bare address is the case this exists for: `192.168.1.50` is what
    /// everybody types and is not an address this page can use, because nothing
    /// in it says how much of the network is local. Answering it with the
    /// general complaint would leave the user retyping the same thing.
    #[test]
    fn what_is_wrong_with_what_was_typed() {
        assert_eq!(fault(Field::Address, "192.168.1.50/24"), None);
        assert_eq!(fault(Field::Address, "  10.0.0.1/8  "), None);
        assert!(fault(Field::Address, "192.168.1.50")
            .is_some_and(|said| said.contains("size of the network")));
        assert!(fault(Field::Address, "192.168.1.50/33").is_some());
        assert!(fault(Field::Address, "not an address/24").is_some());
        assert!(fault(Field::Address, "192.168.1.50/").is_some());

        assert_eq!(fault(Field::Router, "192.168.1.1"), None);
        assert!(fault(Field::Router, "192.168.1.1/24").is_some());
        assert!(fault(Field::Router, "gateway").is_some());

        assert_eq!(fault(Field::Dns, "9.9.9.9"), None);
        assert_eq!(fault(Field::Dns, "9.9.9.9, 1.1.1.1"), None);
        assert_eq!(fault(Field::Dns, "9.9.9.9 1.1.1.1"), None);
        assert!(fault(Field::Dns, "9.9.9.9, nonsense").is_some());

        // Empty is never a fault. Every one of these can be cleared, and
        // clearing one is a thing to want.
        for field in [Field::Address, Field::Router, Field::Dns] {
            assert_eq!(fault(field, ""), None, "{field:?}");
            assert_eq!(fault(field, "   "), None, "{field:?}");
        }
    }

    /// An address and its prefix, or neither.
    #[test]
    fn an_address_carries_the_size_of_its_network() {
        assert_eq!(
            split_address("192.168.1.50/24"),
            Some(("192.168.1.50".to_string(), 24))
        );
        assert_eq!(
            split_address("10.0.0.1/0"),
            Some(("10.0.0.1".to_string(), 0))
        );
        assert_eq!(
            split_address("10.0.0.1/32"),
            Some(("10.0.0.1".to_string(), 32))
        );
        // A prefix wider than an IPv4 address is not one.
        assert_eq!(split_address("10.0.0.1/33"), None);
        // And an address with no prefix is not an address this page can use.
        // Guessing 24 for it would be the shell deciding how much of somebody's
        // network is local.
        assert_eq!(split_address("10.0.0.1"), None);
    }

    /// Commas or spaces, because both are what people type.
    #[test]
    fn name_servers_are_read_in_the_order_they_were_typed() {
        let listed = |text| {
            servers_of(text)
                .into_iter()
                .map(|address| address.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(listed("9.9.9.9,1.1.1.1"), ["9.9.9.9", "1.1.1.1"]);
        assert_eq!(listed(" 9.9.9.9 ,  1.1.1.1 "), ["9.9.9.9", "1.1.1.1"]);
        assert_eq!(listed("9.9.9.9 1.1.1.1"), ["9.9.9.9", "1.1.1.1"]);
        assert!(listed("").is_empty());
    }

    /// The four octets, in the order they read, held in a number.
    ///
    /// This is the one thing in this module that could be wrong without
    /// anything failing: a reversed address is still an address, and what it
    /// would do is quietly send somebody's name server lookups to 4.3.2.1. The
    /// round trip is what pins it, and the fixture is the one number that tells
    /// the two orders apart.
    #[test]
    fn an_address_survives_the_number_networkmanager_keeps_it_in() {
        let address: std::net::Ipv4Addr = "1.2.3.4".parse().unwrap();
        assert_eq!(packed(address), u32::from_ne_bytes([1, 2, 3, 4]));
        assert_eq!(unpacked(packed(address)), address);
        for text in ["0.0.0.0", "255.255.255.255", "9.9.9.9", "192.168.1.50"] {
            let address: std::net::Ipv4Addr = text.parse().unwrap();
            assert_eq!(unpacked(packed(address)), address, "{text}");
        }
    }

    /// What each of the three is called, and what it tells somebody to type.
    #[test]
    fn every_typed_value_says_what_it_is_for() {
        for field in [Field::Address, Field::Router, Field::Dns] {
            assert!(!field.title().is_empty(), "{field:?}");
            // The note is an instruction with an example in it, because what is
            // on screen is an empty well and a keyboard.
            assert!(field.note().ends_with('.'), "{field:?}");
        }
        assert!(Field::Address.note().contains("192.168.1.50/24"));
    }

    fn wired_device(path: &str) -> Device {
        Device {
            path: path.to_string(),
            interface: "a-socket".to_string(),
            kind: Kind::Wired,
            link: Link::Up,
            trouble: None,
            connection: None,
            address: None,
            gateway: None,
            nameservers: Vec::new(),
            hardware: None,
            carrier: Some(true),
            speed: 1000,
            profile: Some("a-profile".to_string()),
            ipv4: Ipv4::default(),
        }
    }

    fn wireless_device(path: &str) -> Device {
        Device {
            kind: Kind::Wireless,
            interface: "a-radio".to_string(),
            carrier: None,
            speed: 0,
            ..wired_device(path)
        }
    }

    fn network(ssid: &str) -> Network {
        Network {
            ssid: ssid.to_string(),
            strength: 70,
            security: Security::Personal,
            saved: false,
            joined: false,
            frequency: 5180,
        }
    }
}
