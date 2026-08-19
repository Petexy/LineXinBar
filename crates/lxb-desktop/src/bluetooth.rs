//! What this machine is paired with: the controller in it, and the things in
//! the air around it that answer to it.
//!
//! The sister of [`crate::network`], and it makes the same bargain for the same
//! reason. A radio that pairs is a radio speaking a security manager protocol
//! at the other end of a link — key distribution, numeric comparison, bonding
//! keys kept across reboots — and nothing in the kernel does that on its own.
//! What every Linux machine with Bluetooth in it actually has is **BlueZ**, one
//! daemon that owns the controller, remembers every device the machine has ever
//! bonded with, and hands the profiles out to whoever wants them. So this module
//! talks to `bluetoothd` and to nothing else, and a session without one gets a
//! row saying so.
//!
//! ## What it does, and what it deliberately does not
//!
//! It reads the controllers, turns one on and off, lists what is in the air,
//! pairs with something new, connects and disconnects what is already paired,
//! and takes a pairing off the machine. It writes nothing down: a bond belongs
//! to BlueZ, which is what every other program on this computer will ask about
//! it, and a shell with its own copy would be a second opinion about what the
//! machine is paired with at every login.
//!
//! It also names this machine, says whether anything nearby may find it, and
//! decides what happens to the radio when a session starts. Those three are the
//! page's own rather than BlueZ's — see [`crate::settings::Startup`] — because
//! they are the questions BlueZ has no opinion about.
//!
//! Visibility is the one of them worth arguing over, and it is off by default
//! and stays off until somebody says otherwise. A discoverable machine is one
//! announcing its name to every radio in the building for as long as it is
//! switched on. It is offered at all because there is one thing that cannot be
//! done without it: pairing from the *other* end, which is how a phone sends a
//! file to a console and how anything with no screen and no list of its own
//! pairs at all.
//!
//! It does not offer per-profile connection, send files, or share the network
//! over Bluetooth.
//!
//! ## Pairing needs somebody to ask, and that is this shell
//!
//! Here is the one thing this module has that its sister does not. When
//! `NetworkManager` cannot join a network it gives up and reports why, and the
//! shell notices and asks — see [`crate::network::Worker::notice_refusals`].
//! BlueZ does not work that way. Pairing is a *conversation*: the other end may
//! want a number compared, a passkey typed here, or a passkey typed over there,
//! and BlueZ carries out none of it itself. It calls an **agent** — a D-Bus
//! object this session registers — and waits for the answer. A machine with no
//! agent cannot pair with anything at all.
//!
//! So this module is one, exactly as [`crate::polkit`] is polkit's: an
//! `org.bluez.Agent1` served on the connection the worker already holds, with
//! `KeyboardDisplay` capability, because a console has a screen and an
//! on-screen keyboard and can therefore answer any of the questions. What comes
//! back is a [`Wanted`] in the listing and what goes out is [`Bt::agree`],
//! [`Bt::answer`] or [`Bt::refuse`] — the same shape the wireless password
//! arrives in, and for the same reason: the worker cannot draw and the shell
//! cannot block.
//!
//! ## Everything happens off the drawing thread
//!
//! One worker thread reads BlueZ and carries out presses, on the terms
//! [`crate::network`]'s does: only while somebody is looking at the page. Two
//! presses are the exception and get a thread each — pairing and connecting are
//! *conversations with another machine* and take seconds when they go well and
//! the better part of a minute when they do not, and a worker blocked on one
//! would be a page that stopped saying anything at the moment it had the most
//! to say. See [`Worker::begin`].
//!
//! **Looking around is narrower than any of that.** It happens while the Search
//! to pair column is open and at no other time — not while the paired devices
//! are being read, not while Settings is on screen. A scanning controller shares
//! its radio with whatever it is already carrying, so the cost is paid only by
//! somebody who pressed the row that means "find me something new". See
//! [`Bt::watch`], which takes the two questions separately.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

/// How often the listing is read again while the page is on screen.
///
/// Two seconds, which is what the network pages and the sound devices are read
/// at. It is one D-Bus call — BlueZ publishes the whole tree through
/// `GetManagedObjects` — so this is cheaper than either of them.
const REFRESH: Duration = Duration::from_secs(2);

/// The pace while something is being paired with or connected to.
const HURRY: Duration = Duration::from_millis(400);

/// How long a press is watched closely before the ordinary pace resumes.
///
/// Longer than the network's half-minute, because a pairing that is waiting for
/// somebody to read a number off a headset and press Accept is a pairing that
/// has not failed. BlueZ gives up before this does.
const WORKING: Duration = Duration::from_secs(90);

/// How wide a band of signal counts as the same distance, in dBm, when the
/// devices are put in order.
///
/// The same trick [`crate::network`] sorts networks by, and it matters more
/// here: a Bluetooth RSSI moves several dBm between one advertisement and the
/// next while nothing at all has happened, and a list that reordered on every
/// one of those would be a list where the press lands on whatever slid under
/// the cursor.
const BANDS: i16 = 15;

const BLUEZ: &str = "org.bluez";
const ROOT: &str = "/";
const MANAGER_PATH: &str = "/org/bluez";
const ADAPTER_IFACE: &str = "org.bluez.Adapter1";
const DEVICE_IFACE: &str = "org.bluez.Device1";
const BATTERY_IFACE: &str = "org.bluez.Battery1";
const AGENT_MANAGER_IFACE: &str = "org.bluez.AgentManager1";
const OBJECTS_IFACE: &str = "org.freedesktop.DBus.ObjectManager";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

/// Where this session's pairing agent hangs on the bus.
///
/// Under LineXinBar's own name for the reason [`crate::polkit`]'s is: the path
/// is ours to choose, BlueZ is handed it at registration, and a path in
/// somebody else's namespace is a claim on a name we do not own.
const AGENT_PATH: &str = "/org/linexinbar/bluetooth/Agent";

/// What this shell tells BlueZ it can do when it is asked something.
///
/// The whole of it. A console has a screen to show a number on and an on-screen
/// keyboard to type one into, so there is no question in the protocol it has to
/// refuse — and the alternative, `NoInputNoOutput`, is not a smaller version of
/// this page but a different and worse one: it would pair without ever showing
/// the user the number that proves they are pairing with the thing in their
/// hand rather than with whatever else is in the room, and a Bluetooth keyboard,
/// which cannot pair without displaying a passkey, would simply never work.
const CAPABILITY: &str = "KeyboardDisplay";

/// How many times the pairing agent is offered to BlueZ before the worker
/// stops asking of its own accord.
///
/// It normally lands on the first: `bluetoothd` is socket-activated on most
/// machines, and the registration *is* the call that starts it. The retries are
/// for the machine where it is not — where Bluetooth arrives a moment after the
/// session does — and they stop, because a machine with no Bluetooth in it
/// would otherwise wake a thread every half minute for the length of the
/// session to be told the same thing. A visit to the page wakes the worker
/// anyway, so nothing is permanently given up on.
const TRIES: u32 = 10;

/// How long between those.
const RETRY: Duration = Duration::from_secs(30);

/// What `DiscoverableTimeout` is put back to when visibility is turned off.
///
/// BlueZ's own default, in seconds. Restored rather than left at nothing so
/// that this shell does not decide, on its way past, that a machine made
/// visible by something else next week stays visible forever.
const DISCOVERABLE_TIMEOUT: u32 = 180;

/// The most characters this machine's Bluetooth name may be.
///
/// The advertisement carries a name in a fixed number of bytes and the rest is
/// simply not sent, so a name longer than this is one the machine does not
/// really have. BlueZ's own limit on the field is two hundred and forty-eight;
/// this is far below it and is also a bound on what a held key can do to a
/// field the panel has to draw.
const LONGEST_NAME: usize = 64;

/// Where the kernel publishes the switches on the outside of the machine.
///
/// Read rather than asked of BlueZ, because BlueZ has no property for it: an
/// adapter killed by the switch above a laptop's keyboard is reported as an
/// ordinary adapter that is merely off, and a page built from that alone would
/// offer an On that cannot happen. This is the same fact
/// `NetworkManager.WirelessHardwareEnabled` is for the radio beside it.
const RFKILL: &str = "/sys/class/rfkill";

/// What sort of thing a device is, as far as it says.
///
/// From BlueZ's own `Icon` where there is one, which is the name of a themed
/// icon and therefore already an answer to this question, and from the class of
/// device where there is not. It is used for one line of text and nothing else:
/// every row on the page wears the same mark, exactly as every wireless network
/// wears the same arcs, because what tells two headsets apart is their names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Headset,
    Headphones,
    Speaker,
    Controller,
    Keyboard,
    Mouse,
    Tablet,
    Phone,
    Computer,
    Watch,
    Printer,
    Camera,
    Display,
    /// Something that did not say, which is most of what a scan turns up: a
    /// beacon in a shop, a car, a thermometer. Nameless rather than guessed at.
    Unknown,
}

impl Kind {
    /// What the row calls it, or nothing at all where it did not say.
    pub fn title(self) -> Option<&'static str> {
        match self {
            Kind::Headset => Some("Headset"),
            Kind::Headphones => Some("Headphones"),
            Kind::Speaker => Some("Speaker"),
            Kind::Controller => Some("Game controller"),
            Kind::Keyboard => Some("Keyboard"),
            Kind::Mouse => Some("Mouse"),
            Kind::Tablet => Some("Tablet"),
            Kind::Phone => Some("Phone"),
            Kind::Computer => Some("Computer"),
            Kind::Watch => Some("Watch"),
            Kind::Printer => Some("Printer"),
            Kind::Camera => Some("Camera"),
            Kind::Display => Some("Display"),
            Kind::Unknown => None,
        }
    }
}

/// Something the shell has asked for that is still happening.
///
/// Carried on the device rather than worked out from BlueZ's properties,
/// because BlueZ has none for it: `Connected` is false right up to the moment
/// it is true, and there is nothing at all that says a pairing is in flight. A
/// page without this is a page where pressing a pair of headphones does
/// nothing visible for five seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Doing {
    Pairing,
    Connecting,
    Disconnecting,
    Forgetting,
}

impl Doing {
    /// What the row says while it is happening.
    pub fn title(self) -> &'static str {
        match self {
            Doing::Pairing => "Pairing…",
            Doing::Connecting => "Connecting…",
            Doing::Disconnecting => "Disconnecting…",
            Doing::Forgetting => "Removing…",
        }
    }
}

/// One thing in the air, or one thing this machine remembers.
///
/// The two are one list, which is the whole shape of the page: a device the
/// machine is bonded with is listed whether or not it is switched on, and one
/// that has never been seen before appears the moment the controller hears it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// BlueZ's own handle on it. What naming one to BlueZ names, and what a
    /// [`crate::settings::Setting`] carries.
    pub path: String,
    /// The controller that can hear it.
    pub controller: String,
    /// Its hardware address, which is the one name it certainly has.
    pub address: String,
    /// What to call it: the alias the user or the device chose, falling back to
    /// the name it advertises, falling back to the address.
    pub name: String,
    pub kind: Kind,
    /// Whether the machine holds a bonding key for it, and so whether
    /// connecting will ask anything.
    pub paired: bool,
    /// Whether it is connected now.
    pub connected: bool,
    /// How well it is heard, in dBm, or nothing where it has not been heard at
    /// all — which is the ordinary state of a paired device that is switched
    /// off in a drawer.
    pub strength: Option<i16>,
    /// What is left in it, in per cent, for the devices that report it.
    pub battery: Option<u8>,
    /// What this shell is in the middle of doing to it.
    pub doing: Option<Doing>,
}

/// One Bluetooth controller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Controller {
    pub path: String,
    /// What the kernel calls it — `hci0`. The only name that tells two of them
    /// apart: BlueZ names both after the machine.
    pub interface: String,
    /// What BlueZ calls it, which is this machine's own name.
    pub name: String,
    pub address: String,
    pub powered: bool,
    /// Whether it can be turned on at all, or whether a switch on the machine
    /// has it off.
    pub switchable: bool,
    /// Whether it is looking around at this moment.
    pub discovering: bool,
    /// Whether anything nearby can find this machine.
    pub discoverable: bool,
}

/// What BlueZ is waiting to be told before a pairing can go on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Question {
    /// Both ends are showing the same number, and the user says whether they
    /// match. The commonest of them, and the only one that proves the thing
    /// being paired with is the thing in the room.
    Confirm { code: String },
    /// Nothing to compare: the other end has no screen and no keys, so all
    /// there is to ask is whether this was meant.
    Authorize,
    /// The other end is showing a number to be typed here.
    Passkey,
    /// The other end wants a PIN typed here — the old four digits printed in a
    /// headset's manual.
    Pin,
    /// The number to type over *there*. Nothing comes back from this one: it is
    /// the shell being told what to show, and how a Bluetooth keyboard is
    /// paired with at all.
    Show { code: String },
}

impl Question {
    /// Whether answering it means typing something rather than agreeing.
    pub fn typed(&self) -> bool {
        matches!(self, Question::Passkey | Question::Pin)
    }

    /// Whether it is a thing to read rather than a thing to answer.
    pub fn shown(&self) -> bool {
        matches!(self, Question::Show { .. })
    }
}

/// One question, as the panel needs it.
///
/// Published rather than asked directly, for the reason
/// [`crate::network::Wanted`] is: BlueZ asks on a thread that cannot draw, and
/// the shell notices it in the listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wanted {
    /// BlueZ's handle on the device, so that giving up can cancel the pairing
    /// it belongs to.
    pub device: String,
    /// What the panel is titled after.
    pub name: String,
    pub question: Question,
    /// Whether this pairing was started from this shell.
    ///
    /// It nearly always was — somebody pressed a device's row — but not always:
    /// this session is BlueZ's *default* agent, so a controller whose pairing
    /// button somebody held down arrives here too, with nothing on screen
    /// leading up to it. The panel is the same panel; what changes is which
    /// button it stands on. A question the user asked for stands on the one
    /// that finishes it, and a question that appeared out of the air stands on
    /// Cancel, because a panel nobody was expecting must not be answerable by
    /// the press somebody was about to make anyway.
    pub ours: bool,
    /// Which ask this is. The shell raises a panel when this changes rather
    /// than merely when the field is set, so a second question in one pairing
    /// is a fresh panel rather than one that quietly stayed up.
    pub asked: u64,
}

/// Everything the Bluetooth pages are drawn from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listing {
    /// Whether BlueZ answered at all.
    ///
    /// An empty listing means two different things and the page has to say
    /// which: a machine with no Bluetooth in it, or a session where nothing is
    /// running that could be in charge of it.
    pub manager: bool,
    /// Every controller, in the order BlueZ lists them.
    pub controllers: Vec<Controller>,
    /// What each controller can hear or remembers, per controller path.
    pub devices: Vec<(String, Vec<Device>)>,
    /// The question BlueZ is waiting on, if it is waiting on one.
    pub wanted: Option<Wanted>,
}

impl Listing {
    /// Nothing, before anything has been read — and what a session with no
    /// worker thread keeps.
    pub const fn none() -> Self {
        Self {
            manager: false,
            controllers: Vec::new(),
            devices: Vec::new(),
            wanted: None,
        }
    }

    /// What one controller can hear, in the order the page lists them.
    pub fn devices_of(&self, controller: &str) -> &[Device] {
        self.devices
            .iter()
            .find(|(path, _)| path == controller)
            .map(|(_, devices)| devices.as_slice())
            .unwrap_or(&[])
    }
}

/// What the user has asked for and the worker has not carried out yet.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Ask {
    /// Turn one controller on, or off.
    Power {
        controller: String,
        on: bool,
    },
    /// Pair with this device if it is new, and connect to it either way.
    Connect {
        device: String,
    },
    Disconnect {
        device: String,
    },
    /// Take the pairing off the machine.
    Forget {
        device: String,
    },
    /// Stop a pairing that is in flight, because the user closed its question.
    Stop {
        device: String,
    },
    /// Let anything nearby find this machine, or only what it already knows.
    Visible {
        controller: String,
        on: bool,
    },
    /// Change what this machine calls itself over Bluetooth.
    Rename {
        controller: String,
        name: String,
    },
}

/// How a pairing ended.
///
/// Three answers rather than two, because "it paired" and "it connected" are
/// two things and they can come apart: a headset can bond with the machine and
/// then fail to bring up a profile, which leaves it on the Devices page as
/// something the machine knows and is not using. Telling the user that it
/// worked would be a lie, and telling them it failed would send them off to
/// pair something they are already paired with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    /// Bonded and connected: the whole of what the press asked for.
    Connected,
    /// Bonded, and then it did not come on.
    Paired,
    /// It would not bond at all, so nothing was added to the machine.
    Failed,
}

/// One pairing that has finished, waiting for the shell to say so.
///
/// Published as an event rather than as part of the [`Listing`], and the
/// difference is not a detail. The listing is *state* — it is compared against
/// the last one to decide whether anything moved — and this is a thing that
/// happened once. Folding it in would mean a listing that differs from itself
/// for a frame and then differs back, and an announcement that arrived twice if
/// anything else changed in between.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// What to call the device, as the page called it when the user pressed it.
    ///
    /// Carried rather than looked up when the announcement is drawn: a device
    /// that would not pair is one BlueZ may have already forgotten about by
    /// then, and "could not pair with" followed by nothing is worse than not
    /// saying it.
    pub name: String,
    pub ending: Ending,
}

/// What the shell answers a question with.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Reply {
    /// The numbers match, or yes, this was meant.
    Yes,
    /// No, or the panel was closed.
    No,
    /// What was typed into the field.
    Typed(String),
}

/// The listing, the worker that keeps it true, and the agent BlueZ asks.
pub struct Bt {
    shared: Arc<Shared>,
}

struct Shared {
    state: Mutex<State>,
    /// Woken whenever the page arrives, by every press, and by a thread that
    /// has finished pairing. Between those the worker sleeps.
    signal: Condvar,
}

#[derive(Default)]
struct State {
    listing: Listing,
    /// Bumped every time a *different* listing is put here — by the worker, by
    /// the agent asking something, and by an answer taking the question away.
    ///
    /// Every writer has to bump it. The shell only copies the listing out when
    /// this moves, so a write that did not would be one no reader ever sees:
    /// the panel would go up again on the next frame from a cached question
    /// that has already been answered.
    published: u64,
    /// Whether the pages that show it are on screen.
    watching: bool,
    /// Whether the page that lists what is in the air is open.
    ///
    /// Separate from [`State::watching`], and the separation is the whole
    /// point: reading the listing costs one D-Bus call, and looking around
    /// costs the radio. See [`Bt::watch`].
    looking: bool,
    /// Presses waiting to be carried out, in the order they were made.
    asks: Vec<Ask>,
    /// What the shell has asked for that has not finished, per device.
    ///
    /// Written by the worker when it starts one and by the thread carrying it
    /// out when it ends, which is why it is here rather than in the worker: the
    /// two are different threads and the page is drawn from what both of them
    /// know.
    working: Vec<(String, Doing, Instant)>,
    /// Where the answer to the question on screen goes.
    ///
    /// `None` while nothing is being asked — and also while the *shown* passkey
    /// is up, which is the one question with no answer to send. See
    /// [`Question::Show`].
    answering: Option<async_channel::Sender<Reply>>,
    /// How many questions have been asked. Only ever compared; see
    /// [`Wanted::asked`].
    asked: u64,
    /// Pairings that have finished and have not been announced yet.
    ///
    /// A queue and not a slot: two devices can be pairing at once — the page
    /// allows it, one per device — and a second finishing must not overwrite
    /// the first's answer before the shell has drained it.
    endings: Vec<Outcome>,
    dirty: bool,
    done: bool,
}

impl Bt {
    /// Start looking.
    ///
    /// Nothing is read until somebody opens the page — see [`Bt::watch`] — but
    /// the agent is registered on the first pass all the same, because a
    /// pairing question can arrive from a device pressing its own button while
    /// nobody is in Settings at all.
    pub fn start() -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            signal: Condvar::new(),
        });
        let worker = Arc::clone(&shared);
        if let Err(err) = std::thread::Builder::new()
            .name("lxb-bluetooth".to_string())
            .spawn(move || Worker::new(worker).run())
        {
            tracing::warn!(?err, "no worker thread; the Bluetooth pages are off");
        }
        Self { shared }
    }

    /// What the machine's Bluetooth looks like, as the worker last found it.
    pub fn listing(&self) -> Listing {
        self.held().listing.clone()
    }

    /// How many times the listing has changed. Ask before [`Bt::listing`], for
    /// the reason [`crate::network::Net::published`] exists.
    pub fn published(&self) -> u64 {
        self.held().published
    }

    /// The pairings that have finished since this was last asked.
    ///
    /// Taken rather than read, because each of them is announced once. Asked
    /// every frame and empty on almost all of them, which costs one lock and
    /// a look at an empty vector — the same as [`Bt::published`] beside it, and
    /// for the same reason: this cannot be folded into the listing's counter,
    /// since what a pairing *ended as* is not part of what the machine looks
    /// like now.
    pub fn endings(&self) -> Vec<Outcome> {
        let mut state = self.held();
        std::mem::take(&mut state.endings)
    }

    /// Say what is on screen: the Settings column, and the Bluetooth page
    /// inside it.
    ///
    /// Two answers rather than one, and they are two because they cost two
    /// different things. Reading the listing is a single D-Bus call and is
    /// worth doing wherever in Settings the user is standing, exactly as the
    /// network's is — a device that comes back on should be listed by the time
    /// they arrive.
    ///
    /// Looking around is the other one, and it is the one place this module
    /// costs the machine something while nobody is pressing anything. A
    /// controller that is scanning is a controller sharing its radio with
    /// whatever it is already carrying, and a pair of headphones can be *heard*
    /// to mind. So the scan is tied to the **Search to pair** column being open
    /// and to nothing else: not to Settings, where somebody adjusting the night
    /// light with music playing would pay for a list they are not looking at,
    /// and not even to the Bluetooth page, where somebody who came to rename
    /// the machine would pay the same. It is paid by whoever pressed the row
    /// that means "find me something new". See
    /// [`crate::settings::SEARCH_PAGE`].
    pub fn watch(&self, listing: bool, looking: bool) {
        let mut state = self.held();
        if state.watching == listing && state.looking == looking {
            return;
        }
        state.watching = listing;
        state.looking = looking;
        state.dirty = true;
        // Woken either way, unlike the network's: the page *leaving* is what
        // stops the scan, and a scan left running because the worker was asleep
        // would be one that ran until something else happened to wake it.
        self.shared.signal.notify_one();
    }

    /// Turn one controller on, or off.
    pub fn set_power(&self, controller: &str, on: bool) {
        self.ask(Ask::Power {
            controller: controller.to_string(),
            on,
        });
    }

    /// Connect to a device, pairing with it first if this machine has never
    /// been paired with it.
    ///
    /// One press, whichever of those it turns out to be — the whole of what
    /// pressing a device's row does. Whether pairing is needed is BlueZ's
    /// answer rather than the shell's, and what it takes is the *device's*: a
    /// headset just agrees, a phone shows a number, a keyboard wants one typed
    /// on it. What comes back is a [`Wanted`] in the listing, or a device that
    /// connects.
    pub fn connect(&self, device: &str) {
        self.ask(Ask::Connect {
            device: device.to_string(),
        });
    }

    /// Take a device off this machine, keeping the pairing.
    pub fn disconnect(&self, device: &str) {
        self.ask(Ask::Disconnect {
            device: device.to_string(),
        });
    }

    /// Remove the pairing from the machine.
    ///
    /// Named for what it is to the user rather than for what it does to BlueZ:
    /// what goes is the bond, which is the whole of what this machine remembers
    /// about the device — that it was paired, the keys that got it on, and the
    /// name it was given here.
    pub fn forget(&self, device: &str) {
        self.ask(Ask::Forget {
            device: device.to_string(),
        });
    }

    /// Let anything nearby find this machine, or only what it already knows.
    pub fn set_visible(&self, controller: &str, on: bool) {
        self.ask(Ask::Visible {
            controller: controller.to_string(),
            on,
        });
    }

    /// Change what this machine calls itself over Bluetooth.
    ///
    /// Checked before it gets here — see [`fault`], which the panel calls while
    /// it is still on screen and can still say what is wrong with it.
    pub fn rename(&self, controller: &str, name: &str) {
        self.ask(Ask::Rename {
            controller: controller.to_string(),
            name: name.to_string(),
        });
    }

    /// Answer a question that only needed agreement: the numbers match, or yes,
    /// this was meant.
    pub fn agree(&self) {
        self.reply(Reply::Yes);
    }

    /// Answer one that needed something typed.
    pub fn answer(&self, typed: &str) {
        self.reply(Reply::Typed(typed.to_string()));
    }

    /// Say that the question is not going to be answered.
    ///
    /// Which is a refusal to BlueZ where there is something waiting for one,
    /// and a cancelled pairing where there is not: the passkey being *shown* is
    /// the one question with nothing to answer, and closing its panel can only
    /// mean the user has given up on the pairing it belongs to.
    pub fn refuse(&self) {
        let stop = {
            let state = self.held();
            match (&state.answering, &state.listing.wanted) {
                (None, Some(wanted)) => Some(wanted.device.clone()),
                _ => None,
            }
        };
        if let Some(device) = stop {
            self.ask(Ask::Stop { device });
        }
        self.reply(Reply::No);
    }

    /// Hand an answer to whoever is waiting for it, and take the question off
    /// the listing.
    fn reply(&self, reply: Reply) {
        let mut state = self.held();
        if let Some(sender) = state.answering.take() {
            // Never blocks: the channel holds one and is used once.
            let _ = sender.try_send(reply);
        }
        // The question goes straight away rather than waiting for the agent's
        // thread to wake, for the reason [`crate::network::Net::refuse`] does
        // it here: the panel has closed, and a listing that still asked would
        // have the shell raise it again on the next frame.
        if state.listing.wanted.take().is_some() {
            state.published += 1;
        }
        state.dirty = true;
        self.shared.signal.notify_one();
    }

    /// Queue a press, collapsing one that says the same thing again.
    fn ask(&self, ask: Ask) {
        let mut state = self.held();
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

impl Drop for Bt {
    fn drop(&mut self) {
        let mut state = self.held();
        state.done = true;
        self.shared.signal.notify_one();
    }
}

// --- the agent BlueZ asks --------------------------------------------------

/// What a refused question is answered with.
///
/// BlueZ's own error names, because that is what every other agent returns and
/// what `bluetoothd` logs. It acts on the difference: `Rejected` is the user
/// saying no, which ends the pairing, and `Canceled` is the question going away
/// under it.
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.bluez.Error")]
enum Refused {
    Rejected(String),
    Canceled(String),
}

/// The object `bluetoothd` calls when it needs a person.
struct Listener {
    shared: Arc<Shared>,
}

#[zbus::interface(name = "org.bluez.Agent1")]
impl Listener {
    /// BlueZ has unregistered this agent — another one took the default, or the
    /// daemon is going away.
    async fn release(&self) {
        tracing::info!("bluetoothd released the pairing agent");
        self.withdraw();
    }

    /// The old four digits, printed in a headset's manual.
    async fn request_pin_code(&self, device: OwnedObjectPath) -> Result<String, Refused> {
        match self.put(&device, Question::Pin).await {
            Some(Reply::Typed(typed)) => Ok(typed),
            Some(_) => Err(Refused::Rejected("no PIN was given".to_string())),
            None => Err(unanswered()),
        }
    }

    /// The same, to be typed on the device rather than here.
    async fn display_pin_code(
        &self,
        device: OwnedObjectPath,
        pincode: String,
    ) -> Result<(), Refused> {
        self.show(&device, Question::Show { code: pincode });
        Ok(())
    }

    /// A number the other end is showing.
    async fn request_passkey(&self, device: OwnedObjectPath) -> Result<u32, Refused> {
        match self.put(&device, Question::Passkey).await {
            // Anything that is not a number is not an answer. The field takes
            // digits, so this is the case where somebody pressed Accept over an
            // empty one — refused rather than sent as a zero, which is a real
            // passkey and would be the shell answering for the user.
            Some(Reply::Typed(typed)) => typed
                .trim()
                .parse::<u32>()
                .map_err(|_| Refused::Rejected("that is not a passkey".to_string())),
            Some(_) => Err(Refused::Rejected("no passkey was given".to_string())),
            None => Err(unanswered()),
        }
    }

    /// A number for the user to type on the device.
    ///
    /// `entered` is how many digits it has seen so far, which this does not
    /// use: the panel would then be rebuilt on every keystroke somebody makes
    /// on a keyboard across the room, and what it says — type this — does not
    /// change while they do.
    async fn display_passkey(&self, device: OwnedObjectPath, passkey: u32, entered: u16) {
        let _ = entered;
        self.show(
            &device,
            Question::Show {
                code: code(passkey),
            },
        );
    }

    /// Both ends are showing the same number.
    async fn request_confirmation(
        &self,
        device: OwnedObjectPath,
        passkey: u32,
    ) -> Result<(), Refused> {
        match self
            .put(
                &device,
                Question::Confirm {
                    code: code(passkey),
                },
            )
            .await
        {
            Some(Reply::Yes) => Ok(()),
            Some(_) => Err(Refused::Rejected(
                "the codes were not confirmed".to_string(),
            )),
            None => Err(unanswered()),
        }
    }

    /// Nothing to compare, so all there is to ask is whether this was meant.
    async fn request_authorization(&self, device: OwnedObjectPath) -> Result<(), Refused> {
        match self.put(&device, Question::Authorize).await {
            Some(Reply::Yes) => Ok(()),
            Some(_) => Err(Refused::Rejected("the pairing was not agreed".to_string())),
            None => Err(unanswered()),
        }
    }

    /// A device that is already paired wants to use a profile it has not been
    /// trusted with.
    ///
    /// Answered without asking anybody, and this is the one place in the module
    /// that decides something on the user's behalf, so it is worth saying what
    /// the decision is. The question only reaches an agent for a device that is
    /// **paired and not trusted**; everything this shell pairs with is trusted
    /// in the same breath — see [`Worker::join`] — so what arrives here was
    /// bonded somewhere else, by another desktop or by `bluetoothctl`. A bond is
    /// the whole of the trust decision Bluetooth has: refusing here would refuse
    /// the headset the machine is already paired with the moment it played
    /// anything, and there is no third answer between the two that a row on a
    /// settings page could hold.
    async fn authorize_service(
        &self,
        device: OwnedObjectPath,
        uuid: String,
    ) -> Result<(), Refused> {
        let paired = self.paired(device.as_str());
        tracing::info!(
            device = device.as_str(),
            uuid,
            paired,
            "a service was asked for"
        );
        match paired {
            true => Ok(()),
            false => Err(Refused::Rejected(
                "this machine is not paired with that device".to_string(),
            )),
        }
    }

    /// The question has gone away: the device stopped waiting, or the pairing
    /// was cancelled from the other end.
    async fn cancel(&self) {
        tracing::info!("bluetoothd withdrew a pairing question");
        if let Some(sender) = self.take() {
            let _ = sender.try_send(Reply::No);
        }
        self.withdraw();
    }
}

impl Listener {
    /// Put a question on the listing and wait for the answer.
    ///
    /// `None` where there is nothing to wait on: a second question while one is
    /// already up, or a shell that has gone. Both are refusals — the caller
    /// turns them into one — because the alternative is a `bluetoothd` held
    /// open on a panel that will never be raised.
    async fn put(&self, device: &OwnedObjectPath, question: Question) -> Option<Reply> {
        let (sender, receiver) = async_channel::bounded(1);
        {
            let mut state = self.held();
            if state.answering.is_some() || state.listing.wanted.is_some() {
                tracing::warn!(
                    device = device.as_str(),
                    "a second pairing question arrived while one was up"
                );
                return None;
            }
            state.answering = Some(sender);
            state.asked += 1;
            state.listing.wanted = Some(Wanted {
                device: device.as_str().to_string(),
                name: self.name_of(&mut state, device.as_str()),
                question,
                ours: ours(&state, device.as_str()),
                asked: state.asked,
            });
            state.published += 1;
            state.dirty = true;
        }
        self.shared.signal.notify_one();

        // And now nothing happens here until the panel has been answered. zbus
        // runs each call on a task of its own, so a question standing for a
        // minute stops neither `Cancel` nor the listing being read.
        let reply = receiver.recv().await.ok();
        self.withdraw();
        reply
    }

    /// Put something on the listing that is read rather than answered.
    fn show(&self, device: &OwnedObjectPath, question: Question) {
        {
            let mut state = self.held();
            state.asked += 1;
            state.listing.wanted = Some(Wanted {
                device: device.as_str().to_string(),
                name: self.name_of(&mut state, device.as_str()),
                question,
                ours: ours(&state, device.as_str()),
                asked: state.asked,
            });
            // No sender: there is nothing to answer. What takes this panel away
            // is the pairing finishing — see [`Worker::begin`] — or the user
            // giving up on it, which cancels the pairing rather than answering
            // anything.
            state.answering = None;
            state.published += 1;
            state.dirty = true;
        }
        self.shared.signal.notify_one();
    }

    /// Take the question off the listing.
    fn withdraw(&self) {
        {
            let mut state = self.held();
            state.answering = None;
            if state.listing.wanted.take().is_some() {
                state.published += 1;
            }
            state.dirty = true;
        }
        self.shared.signal.notify_one();
    }

    fn take(&self) -> Option<async_channel::Sender<Reply>> {
        self.held().answering.take()
    }

    /// Whether the machine is bonded with this device, as the last pass found
    /// it.
    fn paired(&self, device: &str) -> bool {
        let state = self.held();
        state
            .listing
            .devices
            .iter()
            .flat_map(|(_, devices)| devices)
            .any(|listed| listed.path == device && listed.paired)
    }

    /// What to call the device a question is about.
    ///
    /// Looked up in the listing rather than asked of BlueZ, and that is not an
    /// optimisation: this runs on the connection's own executor, and a blocking
    /// call made from inside a method that connection is dispatching is a
    /// deadlock. The device was in the last listing — the pairing started from
    /// its row — and where it was not, the address out of the path is a name
    /// too, which is more than BlueZ itself has for something that has not
    /// advertised one.
    fn name_of(&self, state: &mut State, device: &str) -> String {
        state
            .listing
            .devices
            .iter()
            .flat_map(|(_, devices)| devices)
            .find(|listed| listed.path == device)
            .map(|listed| listed.name.clone())
            .unwrap_or_else(|| address_in(device))
    }

    fn held(&self) -> std::sync::MutexGuard<'_, State> {
        self.shared.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// What a question nobody could be asked is refused with.
///
/// Not `Rejected`, which is the user saying no: this is the shell being unable
/// to put the question on screen at all — a second one arriving while the first
/// is still up, or a shell on its way out. BlueZ tells the two apart in its own
/// log, and so does anyone reading it.
fn unanswered() -> Refused {
    Refused::Canceled("there was nobody to ask".to_string())
}

/// What is wrong with a name that has been typed, or nothing if it will do.
///
/// Asked while the panel is still on screen and can still say so, which is what
/// [`crate::network::fault`] exists for; this is the same thing for the same
/// reason. A field that accepted anything and failed silently a second later is
/// a field with no answer to "why did nothing happen".
///
/// Two things can be wrong with it and neither is a matter of taste. An empty
/// name is not a name — BlueZ takes it as "go back to the machine's own", so
/// the row would come back saying something the user did not type — and a name
/// longer than the advertisement can carry is one the machine does not really
/// have.
pub fn fault(text: &str) -> Option<&'static str> {
    let name = text.trim();
    if name.is_empty() {
        return Some("A name cannot be empty.");
    }
    if name.chars().count() > LONGEST_NAME {
        return Some("That name is too long to be sent.");
    }
    None
}

/// Whether this shell is the one that started the pairing being asked about.
///
/// Which is to say: is there a press of ours still in flight on that device.
/// Nothing else can tell the two apart — BlueZ asks an agent the same way
/// whoever started it — and the difference decides which button the panel
/// stands on. See [`Wanted::ours`].
fn ours(state: &State, device: &str) -> bool {
    state
        .working
        .iter()
        .any(|(path, doing, _)| path == device && matches!(doing, Doing::Pairing))
}

/// A passkey as it is shown, which is six digits with the leading zeros kept.
///
/// BlueZ hands it over as a number, and the number 12 is the passkey `000012`
/// on the screen of the thing being paired with. A panel that said `12` would
/// be asking the user to compare two things that do not look alike.
fn code(passkey: u32) -> String {
    format!("{:06}", passkey.min(999_999))
}

/// The address in a device's object path, as BlueZ spells one.
///
/// `/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF` is an address with underscores, and
/// putting the colons back is the whole of it.
fn address_in(path: &str) -> String {
    match path.rsplit_once("/dev_") {
        Some((_, address)) => address.replace('_', ":"),
        None => path.to_string(),
    }
}

// --- the worker ------------------------------------------------------------

struct Worker {
    shared: Arc<Shared>,
    /// The system bus with this session's agent already on it, opened on the
    /// first pass that needs it. `None` on a machine with no D-Bus at all,
    /// where the page says Bluetooth is not available — which is true, because
    /// nothing could be reached to provide it.
    bus: Option<zbus::blocking::Connection>,
    /// Whether BlueZ has been told about the agent.
    registered: bool,
    /// Whether the machine has been read once, whatever anybody is looking at.
    ///
    /// One pass is owed to the session rather than to the page: the shell has a
    /// startup policy to carry out — Bluetooth on, off, or as it was left — and
    /// a policy that waited for somebody to open Settings would be one that
    /// never ran on the sessions it is for. See
    /// [`crate::settings::Startup`].
    read_once: bool,
    /// How many times it has been offered. See [`TRIES`].
    tries: u32,
    /// The controllers this shell has asked to look around, so that it stops
    /// only what it started.
    looking: Vec<String>,
}

impl Worker {
    fn new(shared: Arc<Shared>) -> Self {
        Self {
            shared,
            bus: None,
            registered: false,
            read_once: false,
            tries: 0,
            looking: Vec::new(),
        }
    }

    fn run(mut self) {
        while self.tick() {
            self.wait();
        }
        self.stop_looking();
        self.unregister();
    }

    /// One pass. `false` when the shell has gone away.
    fn tick(&mut self) -> bool {
        let (watching, looking, asks, working) = {
            let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.done {
                return false;
            }
            state.dirty = false;
            (
                state.watching,
                state.looking,
                std::mem::take(&mut state.asks),
                !state.working.is_empty(),
            )
        };

        // The connection is opened on the first pass whatever is happening, and
        // kept: it carries the agent, and an agent registered only while
        // somebody is in Settings would be a machine that could not be paired
        // with from the device's own button.
        self.connect();

        for ask in asks {
            self.carry_out(ask);
        }

        // A press in flight counts as something to watch even with the page
        // shut, exactly as a join does: the user pressed a row and is owed the
        // answer, and they may well have walked out of Settings to see whether
        // the headphones came on.
        if watching || working || !self.read_once {
            let listing = self.read();
            // Counted as the session's one pass only when something answered.
            // A listing that says BlueZ is not there is not a reading of the
            // machine, and treating it as one would spend the startup policy on
            // a daemon that had not finished starting.
            self.read_once |= listing.manager;
            self.publish(listing);
            // After the listing rather than before it: what may be told to look
            // around is the controllers this pass found, so asking first would
            // mean the first visit to the page waited a refresh before anything
            // started scanning.
            self.look(looking);
        } else if !self.looking.is_empty() {
            self.stop_looking();
        }
        true
    }

    /// Sleep until something happens, or until it is time to look again.
    fn wait(&self) {
        let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.done || state.dirty {
            return;
        }
        let hurrying = state
            .working
            .iter()
            .any(|(_, _, since)| since.elapsed() < WORKING);
        if hurrying {
            let _held = self.shared.signal.wait_timeout(state, HURRY);
        } else if state.watching {
            let _held = self.shared.signal.wait_timeout(state, REFRESH);
        } else if self.still_offering() {
            let _held = self.shared.signal.wait_timeout(state, RETRY);
        } else {
            let _held = self.shared.signal.wait(state);
        }
    }

    /// Whether the agent is still being offered to a BlueZ that has not taken
    /// it — which is the one thing worth waking for with nobody looking at
    /// anything. See [`TRIES`].
    fn still_offering(&self) -> bool {
        !self.registered && self.tries < TRIES
    }

    /// Put the listing in front of the shell, if it has changed.
    ///
    /// The question is never written from here. It belongs to the agent's
    /// thread, which may have raised or answered one between this listing being
    /// read and this line — so it is carried across from whatever is there now
    /// rather than from what was true a moment ago.
    fn publish(&mut self, mut listing: Listing) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        listing.wanted = state.listing.wanted.clone();
        if state.listing != listing {
            state.listing = listing;
            state.published += 1;
        }
    }

    /// Open the system bus with the agent on it, and tell BlueZ about it.
    fn connect(&mut self) {
        if self.bus.is_none() {
            let listener = Listener {
                shared: Arc::clone(&self.shared),
            };
            match zbus::blocking::connection::Builder::system()
                .and_then(|builder| builder.serve_at(AGENT_PATH, listener))
                .and_then(|builder| builder.build())
            {
                Ok(bus) => self.bus = Some(bus),
                // Said at debug: a session with no system bus is a session
                // where this is the expected answer every pass, and the page
                // says it in words the user can read.
                Err(err) => {
                    tracing::debug!(?err, "no system bus; the Bluetooth pages have nothing");
                    return;
                }
            }
        }
        self.register();
    }

    /// Register the pairing agent, once BlueZ is there to take it.
    ///
    /// Retried on every pass until it lands rather than tried once at startup,
    /// because `bluetoothd` is socket-activated on most machines and may well
    /// not be running when the shell starts. The default agent is asked for as
    /// well as registered: a registered agent is one BlueZ *may* use, and the
    /// default is the one it asks when a device pairs on its own account —
    /// which is how a controller with a pairing button gets a panel.
    fn register(&mut self) {
        if self.registered || self.tries >= TRIES {
            return;
        }
        // Counted here rather than on the way out, so that every reason it did
        // not land counts the same — including there being no bus to try on.
        self.tries += 1;
        let Some(bus) = self.bus.as_ref() else {
            return;
        };
        let Some(path) = object(AGENT_PATH) else {
            return;
        };
        if call(
            bus,
            MANAGER_PATH,
            AGENT_MANAGER_IFACE,
            "RegisterAgent",
            &(path.clone(), CAPABILITY),
        )
        .is_none()
        {
            return;
        }
        self.registered = true;
        // Not fatal if it is refused. Another agent holding the default is a
        // machine where this shell is running inside somebody else's desktop —
        // pairing started from *this* page still reaches this agent, because
        // BlueZ asks the agent of whoever called `Pair`. What is lost is only
        // the panel for a device that pairs on its own account.
        if call(
            bus,
            MANAGER_PATH,
            AGENT_MANAGER_IFACE,
            "RequestDefaultAgent",
            &(path,),
        )
        .is_none()
        {
            tracing::info!("another pairing agent is this machine's default");
        }
        tracing::info!("registered as this session's Bluetooth pairing agent");
    }

    /// Take the agent off the bus on the way out.
    ///
    /// Tidiness rather than necessity — BlueZ watches the name and drops an
    /// agent whose owner disconnects, so a shell that crashes unregisters
    /// exactly as well as one that exits.
    fn unregister(&mut self) {
        if !self.registered {
            return;
        }
        let (Some(bus), Some(path)) = (self.bus.as_ref(), object(AGENT_PATH)) else {
            return;
        };
        call(
            bus,
            MANAGER_PATH,
            AGENT_MANAGER_IFACE,
            "UnregisterAgent",
            &(path,),
        );
    }

    // --- carrying out a press ---------------------------------------------

    fn carry_out(&mut self, ask: Ask) {
        let Some(bus) = self.bus.clone() else {
            return;
        };
        match ask {
            Ask::Power { controller, on } => {
                tracing::info!(controller, on, "Bluetooth controller");
                if !set_property(&bus, &controller, ADAPTER_IFACE, "Powered", Value::Bool(on)) {
                    tracing::warn!(controller, on, "the controller would not be set");
                }
                // A controller that has just been turned off is one nothing is
                // looking around on any more, whoever asked for it.
                if !on {
                    self.looking.retain(|path| *path != controller);
                }
            }
            Ask::Connect { device } => {
                let paired = self.is_paired(&device);
                let doing = match paired {
                    true => Doing::Connecting,
                    false => Doing::Pairing,
                };
                let named = device.clone();
                self.begin(&device, doing, move |bus| join(bus, &named, paired));
            }
            Ask::Disconnect { device } => {
                let named = device.clone();
                self.begin(&device, Doing::Disconnecting, move |bus| {
                    tracing::info!(device = named, "disconnecting");
                    if call(bus, &named, DEVICE_IFACE, "Disconnect", &()).is_none() {
                        tracing::warn!(device = named, "the device would not disconnect");
                    }
                    // Nothing announced: coming off a device is a press whose
                    // answer is the row itself, a moment later.
                    None
                });
            }
            Ask::Forget { device } => self.remove(&device),
            Ask::Stop { device } => {
                tracing::info!(device, "cancelling the pairing");
                call(&bus, &device, DEVICE_IFACE, "CancelPairing", &());
            }
            Ask::Visible { controller, on } => self.set_visible(&bus, &controller, on),
            Ask::Rename { controller, name } => {
                tracing::info!(controller, name, "naming this machine");
                if !set_property(
                    &bus,
                    &controller,
                    ADAPTER_IFACE,
                    "Alias",
                    Value::Str(name.as_str().into()),
                ) {
                    tracing::warn!(controller, "the name would not be set");
                }
            }
        }
    }

    /// Take a pairing off the machine.
    ///
    /// The controller owns the bond rather than the device object — `Pair` is
    /// asked of the device and `RemoveDevice` of the adapter it belongs to,
    /// which is BlueZ's own asymmetry and not this module's — so the controller
    /// has to be found before anything can be removed.
    ///
    /// Nothing is disconnected first, and nothing needs to be: removing a
    /// device that is connected takes the connection with it, and a shell that
    /// disconnected first would have a moment where the device was off and
    /// still paired, which is neither of the two states the page can draw.
    fn remove(&mut self, device: &str) {
        let Some(controller) = self.controller_of(device) else {
            // Not a warning. A device with no controller behind it is one that
            // went between the page being drawn and the press landing, and the
            // machine is already in the state that was asked for.
            tracing::debug!(device, "nothing to forget");
            return;
        };
        let Some(path) = object(device) else {
            return;
        };
        let named = device.to_string();
        self.begin(device, Doing::Forgetting, move |bus| {
            tracing::info!(device = named, "forgetting");
            if call(
                bus,
                &controller,
                ADAPTER_IFACE,
                "RemoveDevice",
                &(path.clone(),),
            )
            .is_none()
            {
                tracing::warn!(device = named, "the pairing would not be removed");
            }
            // Nothing announced: the row going off the page is the answer, and
            // it is the page the user is standing on.
            None
        });
    }

    /// Let anything nearby find this machine, or stop.
    ///
    /// Three properties for one switch, and every one of them has to move or
    /// the row lies about what it did.
    ///
    /// `DiscoverableTimeout` first, and it is the interesting one: BlueZ's own
    /// default is three minutes, after which it turns `Discoverable` off again
    /// on its own. A switch that quietly became false while the user was
    /// walking across the room with a phone is worse than no switch, so this
    /// one means *until it is turned off* — which is what a switch on a page
    /// means everywhere else in this tree. It is set back to BlueZ's own three
    /// minutes on the way off, so that a machine made visible by something else
    /// next week is not left permanently so by this shell.
    ///
    /// `Pairable` because visibility without it is a machine that can be seen
    /// and not paired with, which is not a state anybody means by "visible" and
    /// not one this page has a row for.
    fn set_visible(&self, bus: &zbus::blocking::Connection, controller: &str, on: bool) {
        tracing::info!(controller, on, "visibility");
        let timeout = match on {
            true => 0,
            false => DISCOVERABLE_TIMEOUT,
        };
        set_property(
            bus,
            controller,
            ADAPTER_IFACE,
            "DiscoverableTimeout",
            Value::U32(timeout),
        );
        if on {
            set_property(
                bus,
                controller,
                ADAPTER_IFACE,
                "Pairable",
                Value::Bool(true),
            );
        }
        if !set_property(
            bus,
            controller,
            ADAPTER_IFACE,
            "Discoverable",
            Value::Bool(on),
        ) {
            tracing::warn!(controller, on, "the machine's visibility would not be set");
        }
    }

    /// Start something that talks to another machine, on a thread of its own.
    ///
    /// This is the difference between this module and its sister. Every press
    /// under Network is a message to a daemon that answers at once and gets on
    /// with the work; `Pair` and `Connect` are held open by BlueZ for as long as
    /// the *other end* takes, which is seconds when a headset is awake and the
    /// better part of a minute when it is in a drawer. A worker blocked on one
    /// is a page that stops refreshing, a switch that stops answering, and a
    /// Cancel that cannot be sent — at exactly the moment the user is watching
    /// to see whether it worked.
    ///
    /// So the connection is cloned and the call made elsewhere, the device is
    /// marked as busy for as long as it runs, and the mark coming off is what
    /// wakes the worker to say what happened.
    /// Whatever the job returns is announced: see [`Outcome`], and [`join`],
    /// which is the only one of them that returns anything.
    fn begin<F>(&mut self, device: &str, doing: Doing, job: F)
    where
        F: FnOnce(&zbus::blocking::Connection) -> Option<Ending> + Send + 'static,
    {
        let Some(bus) = self.bus.clone() else {
            return;
        };
        // Read before the thread starts, and that is the point of reading it
        // here at all: a pairing that fails is one BlueZ may have dropped the
        // device object for by the time the thread is finished, and a name
        // looked up then would be gone exactly when it is needed.
        let name = self.name_of(device);
        {
            let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            // One at a time per device. Two presses on one row while the first
            // is still in flight is one press: BlueZ would refuse the second
            // with `InProgress` anyway, and the row already says what is
            // happening.
            if state.working.iter().any(|(path, _, _)| path == device) {
                return;
            }
            state
                .working
                .push((device.to_string(), doing, Instant::now()));
            state.dirty = true;
        }
        let shared = Arc::clone(&self.shared);
        let named = device.to_string();
        let spawned = std::thread::Builder::new()
            .name("lxb-bluetooth-pair".to_string())
            .spawn(move || {
                let ending = job(&bus);
                let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                state.working.retain(|(path, _, _)| *path != named);
                if let Some(ending) = ending {
                    state.endings.push(Outcome { name, ending });
                }
                // A passkey the user was asked to type on the device is a
                // question about *this* pairing, and the pairing is over — well
                // or badly. Nothing else takes that panel away: it is the one
                // question with no answer to send, so BlueZ never calls back
                // about it.
                let over = state
                    .listing
                    .wanted
                    .as_ref()
                    .is_some_and(|wanted| wanted.device == named && wanted.question.shown());
                if over {
                    state.listing.wanted = None;
                    state.published += 1;
                }
                state.dirty = true;
                shared.signal.notify_one();
            });
        if spawned.is_err() {
            tracing::warn!(device, "no thread to carry the press out on");
            let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            state.working.retain(|(path, _, _)| path != device);
        }
    }

    // --- looking around ----------------------------------------------------

    /// Start every powered controller looking, or stop the ones this shell
    /// started.
    ///
    /// Only the ones it started, because BlueZ counts discovery per client: a
    /// `StopDiscovery` from here drops this session's own request and leaves
    /// whatever else on the machine asked for one running. Stopping something
    /// somebody else started is not this shell's to do.
    fn look(&mut self, looking: bool) {
        if !looking {
            self.stop_looking();
            return;
        }
        let Some(bus) = self.bus.as_ref() else {
            return;
        };
        let powered: Vec<String> = {
            let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            state
                .listing
                .controllers
                .iter()
                .filter(|controller| controller.powered)
                .map(|controller| controller.path.clone())
                .collect()
        };
        for controller in powered {
            if self.looking.contains(&controller) {
                continue;
            }
            if call(bus, &controller, ADAPTER_IFACE, "StartDiscovery", &()).is_some() {
                self.looking.push(controller);
            }
        }
    }

    fn stop_looking(&mut self) {
        let Some(bus) = self.bus.as_ref() else {
            self.looking.clear();
            return;
        };
        for controller in std::mem::take(&mut self.looking) {
            call(bus, &controller, ADAPTER_IFACE, "StopDiscovery", &());
        }
    }

    // --- reading the machine ----------------------------------------------

    /// Everything the pages are drawn from, in one call.
    ///
    /// One, and that is BlueZ being kinder than `NetworkManager`: the whole
    /// tree — every controller, every device, every battery — comes back from a
    /// single `GetManagedObjects`, where the network's listing is a call per
    /// device and a call per access point.
    fn read(&mut self) -> Listing {
        let Some(bus) = self.bus.as_ref() else {
            return Listing::none();
        };
        let Some(objects) = managed(bus) else {
            // BlueZ is not on this bus. Not a warning: it is the steady state
            // of a machine that has none, and the page says so.
            return Listing::none();
        };
        let working = {
            let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            state.working.clone()
        };

        let mut controllers = Vec::new();
        for (path, interfaces) in &objects {
            let Some(properties) = interfaces.get(ADAPTER_IFACE) else {
                continue;
            };
            let interface = leaf(path);
            controllers.push(Controller {
                name: text(properties, "Alias")
                    .or_else(|| text(properties, "Name"))
                    .unwrap_or_else(|| interface.clone()),
                address: text(properties, "Address").unwrap_or_default(),
                powered: flag(properties, "Powered").unwrap_or(false),
                switchable: !hard_blocked(&interface),
                discovering: flag(properties, "Discovering").unwrap_or(false),
                discoverable: flag(properties, "Discoverable").unwrap_or(false),
                path: path.clone(),
                interface,
            });
        }
        controllers.sort_by(|left, right| left.interface.cmp(&right.interface));

        let mut devices: Vec<(String, Vec<Device>)> = controllers
            .iter()
            .map(|controller| (controller.path.clone(), Vec::new()))
            .collect();
        for (path, interfaces) in &objects {
            let Some(properties) = interfaces.get(DEVICE_IFACE) else {
                continue;
            };
            let controller = match self::path(properties, "Adapter") {
                Some(controller) => controller,
                // A device with no adapter is one BlueZ is in the middle of
                // taking away.
                None => continue,
            };
            let Some((_, listed)) = devices.iter_mut().find(|(at, _)| *at == controller) else {
                continue;
            };
            let address = text(properties, "Address").unwrap_or_else(|| address_in(path));
            listed.push(Device {
                name: text(properties, "Alias")
                    .or_else(|| text(properties, "Name"))
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| address.clone()),
                kind: kind_of(
                    text(properties, "Icon").as_deref(),
                    number(properties, "Class").unwrap_or(0),
                ),
                paired: flag(properties, "Paired").unwrap_or(false),
                connected: flag(properties, "Connected").unwrap_or(false),
                strength: signed(properties, "RSSI"),
                battery: interfaces
                    .get(BATTERY_IFACE)
                    .and_then(|battery| number(battery, "Percentage"))
                    .map(|percentage| percentage.min(100) as u8),
                doing: working
                    .iter()
                    .find(|(at, _, _)| at == path)
                    .map(|(_, doing, _)| *doing),
                path: path.clone(),
                controller,
                address,
            });
        }
        for (_, listed) in &mut devices {
            order(listed);
        }

        Listing {
            manager: true,
            controllers,
            devices,
            // Filled in by [`Self::publish`], which is the only place that may
            // read it: it belongs to the agent's thread.
            wanted: None,
        }
    }

    /// Whether the machine is bonded with this device, as the last pass found
    /// it.
    fn is_paired(&self, device: &str) -> bool {
        let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .listing
            .devices
            .iter()
            .flat_map(|(_, devices)| devices)
            .any(|listed| listed.path == device && listed.paired)
    }

    /// What the page calls a device, as of the last pass.
    ///
    /// The address out of the path where nothing is known about it, which is
    /// what the page would have shown too — see [`address_in`].
    fn name_of(&self, device: &str) -> String {
        let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .listing
            .devices
            .iter()
            .flat_map(|(_, devices)| devices)
            .find(|listed| listed.path == device)
            .map(|listed| listed.name.clone())
            .unwrap_or_else(|| address_in(device))
    }

    /// Which controller a device belongs to.
    fn controller_of(&self, device: &str) -> Option<String> {
        let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .listing
            .devices
            .iter()
            .flat_map(|(_, devices)| devices)
            .find(|listed| listed.path == device)
            .map(|listed| listed.controller.clone())
    }
}

/// Pair with a device if the machine has never been paired with it, and connect
/// to it either way.
///
/// Trusted in the same breath as it is paired, and that is the one thing done
/// here the user did not ask for in so many words. An untrusted device has to
/// be authorised by an agent every time it comes back — which for a controller
/// switched on across the room means a panel appearing on a television because
/// somebody pressed a button on a gamepad. Pairing *is* the decision; trusting
/// is what makes it hold.
///
/// The answer comes back as an [`Ending`] for a *pairing* and as nothing at all
/// for a reconnection. That is not an oversight: pressing a device the machine
/// already knows is a press whose answer is on the row a moment later, and the
/// row is what the user is looking at. A pairing is the other case — it takes
/// long enough to walk away from, it is the one press here that can leave the
/// machine changed, and if it fails there is nothing on the page afterwards
/// that says so.
fn join(bus: &zbus::blocking::Connection, device: &str, paired: bool) -> Option<Ending> {
    if paired {
        tracing::info!(device, "connecting");
        if call(bus, device, DEVICE_IFACE, "Connect", &()).is_none() {
            tracing::warn!(device, "the device would not connect");
        }
        return None;
    }
    tracing::info!(device, "pairing");
    if call(bus, device, DEVICE_IFACE, "Pair", &()).is_none() {
        tracing::warn!(device, "the device would not pair");
        return Some(Ending::Failed);
    }
    set_property(bus, device, DEVICE_IFACE, "Trusted", Value::Bool(true));
    tracing::info!(device, "connecting");
    // Bonded either way from here: what is being reported is whether the thing
    // is usable now, and a device the machine is paired with and not on is a
    // third answer rather than a failure. See [`Ending`].
    match call(bus, device, DEVICE_IFACE, "Connect", &()).is_some() {
        true => Some(Ending::Connected),
        false => {
            tracing::warn!(device, "paired, but the device would not connect");
            Some(Ending::Paired)
        }
    }
}

/// The order the devices are listed in.
///
/// What the machine is on first, then what it remembers, then everything else
/// by how well it is heard — and inside all of that by name, so that the list
/// somebody is reading does not rearrange itself under their thumb. The
/// strength is banded for the reason the networks' is: an RSSI moves several
/// dBm between one advertisement and the next while nothing whatever has
/// happened.
fn order(devices: &mut [Device]) {
    devices.sort_by(|left, right| {
        let band = |device: &Device| device.strength.map(|rssi| rssi / BANDS);
        (
            !left.connected,
            !left.paired,
            std::cmp::Reverse(band(left)),
            left.name.to_lowercase(),
        )
            .cmp(&(
                !right.connected,
                !right.paired,
                std::cmp::Reverse(band(right)),
                right.name.to_lowercase(),
            ))
    });
}

/// What sort of thing a device says it is.
///
/// BlueZ's `Icon` first, because it is BlueZ's own answer to this question and
/// it already knows about the devices that lie about their class. The class of
/// device is the fallback, read as the major class and the minor bits under it
/// — the numbers are the Bluetooth assigned-numbers document's and are written
/// here rather than looked up anywhere on this machine.
fn kind_of(icon: Option<&str>, class: u32) -> Kind {
    match icon {
        Some("audio-headset") => return Kind::Headset,
        Some("audio-headphones") => return Kind::Headphones,
        Some("audio-speakers" | "audio-card") => return Kind::Speaker,
        Some("input-gaming") => return Kind::Controller,
        Some("input-keyboard") => return Kind::Keyboard,
        Some("input-mouse") => return Kind::Mouse,
        Some("input-tablet") => return Kind::Tablet,
        Some("phone") => return Kind::Phone,
        Some("computer") => return Kind::Computer,
        Some("printer") => return Kind::Printer,
        Some("camera-photo" | "camera-video") => return Kind::Camera,
        Some("video-display") => return Kind::Display,
        _ => {}
    }
    // Bits 8..12 are the major class and bits 2..8 the minor one under it.
    let major = (class >> 8) & 0x1f;
    let minor = (class >> 2) & 0x3f;
    match (major, minor) {
        (1, _) => Kind::Computer,
        (2, _) => Kind::Phone,
        (4, 1 | 2) => Kind::Headset,
        (4, 6) => Kind::Headphones,
        (4, 5 | 7 | 8) => Kind::Speaker,
        (4, 15..=18) => Kind::Display,
        (4, _) => Kind::Speaker,
        (5, 16) => Kind::Keyboard,
        (5, 32) => Kind::Mouse,
        (5, 48) => Kind::Keyboard,
        (5, 2 | 4 | 8) => Kind::Controller,
        (5, 20) => Kind::Tablet,
        (6, _) => Kind::Printer,
        (7, 3) => Kind::Watch,
        _ => Kind::Unknown,
    }
}

/// Whether a switch on the outside of the machine has this controller off.
///
/// The kernel's rfkill class, matched by the name it gives each entry, which is
/// the controller's own `hci0`. Nothing here is particular to any computer: the
/// directory is a kernel interface and every line of it is read at runtime.
fn hard_blocked(interface: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(RFKILL) else {
        return false;
    };
    for entry in entries.flatten() {
        let at = entry.path();
        let named = |file: &str| {
            std::fs::read_to_string(at.join(file))
                .map(|read| read.trim().to_string())
                .unwrap_or_default()
        };
        if named("name") != interface {
            continue;
        }
        return named("hard") == "1";
    }
    false
}

// --- talking to BlueZ ------------------------------------------------------

/// One object's interfaces, and every property of each. What BlueZ reports one
/// controller or one device as.
type Interfaces = HashMap<String, HashMap<String, OwnedValue>>;

/// Every object BlueZ has, with every interface on it and every property of
/// each.
fn managed(bus: &zbus::blocking::Connection) -> Option<HashMap<String, Interfaces>> {
    let reply = call(bus, ROOT, OBJECTS_IFACE, "GetManagedObjects", &())?;
    let objects: HashMap<OwnedObjectPath, Interfaces> = reply.body().deserialize().ok()?;
    Some(
        objects
            .into_iter()
            .map(|(path, interfaces)| (path.as_str().to_string(), interfaces))
            .collect(),
    )
}

/// Write one property.
fn set_property(
    bus: &zbus::blocking::Connection,
    path: &str,
    interface: &str,
    name: &str,
    value: Value<'_>,
) -> bool {
    call(bus, path, PROPERTIES, "Set", &(interface, name, value)).is_some()
}

/// One call to BlueZ, with whatever went wrong logged and swallowed.
///
/// Swallowed for the reason [`crate::network`]'s is: every one of these has an
/// answer on the page — a listing that stays as it was, a device that does not
/// connect — and there is no level of this module at which a D-Bus error is
/// anything but that. What it must never do is take the session down.
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
    match bus.call_method(Some(BLUEZ), path, Some(interface), method, body) {
        Ok(reply) => Some(reply),
        Err(err) => {
            tracing::debug!(path, interface, method, ?err, "BlueZ refused");
            None
        }
    }
}

/// The last component of an object path — `hci0` out of `/org/bluez/hci0`.
fn leaf(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

fn number(properties: &HashMap<String, OwnedValue>, name: &str) -> Option<u32> {
    let value = properties.get(name)?;
    // Through every width BlueZ uses for a small number: a class is a `u`, a
    // battery a `y`, an appearance a `q`.
    u32::try_from(value)
        .or_else(|_| u8::try_from(value).map(u32::from))
        .or_else(|_| u16::try_from(value).map(u32::from))
        .ok()
}

/// A number that can be below zero — an RSSI, which is always is.
fn signed(properties: &HashMap<String, OwnedValue>, name: &str) -> Option<i16> {
    i16::try_from(properties.get(name)?).ok()
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
    (path != ROOT).then_some(path)
}

/// A D-Bus object path, or nothing if the string is not one.
fn object(path: &str) -> Option<zbus::zvariant::ObjectPath<'static>> {
    zbus::zvariant::ObjectPath::try_from(path.to_string()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every number here is written by hand from the Bluetooth assigned-numbers
    /// document. Nothing this machine is paired with belongs in this file: an
    /// address or a name pasted out of a live listing is a test that says the
    /// page was built for one desk.
    fn class(major: u32, minor: u32) -> u32 {
        (major << 8) | (minor << 2)
    }

    /// BlueZ's own answer wins, because it already knows about the devices that
    /// lie about their class.
    #[test]
    fn what_a_device_says_it_is() {
        assert_eq!(kind_of(Some("audio-headset"), 0), Kind::Headset);
        assert_eq!(kind_of(Some("input-gaming"), 0), Kind::Controller);
        assert_eq!(kind_of(Some("input-keyboard"), 0), Kind::Keyboard);
        // The icon is taken over the class, whatever the class says.
        assert_eq!(
            kind_of(Some("audio-headphones"), class(5, 16)),
            Kind::Headphones
        );
        // An icon this shell has no word for falls through to the class rather
        // than being given up on.
        assert_eq!(kind_of(Some("network-wireless"), class(2, 3)), Kind::Phone);
    }

    /// The class of device is the fallback, and it is enough for the things a
    /// console is actually paired with.
    #[test]
    fn what_a_device_is_when_it_did_not_say() {
        assert_eq!(kind_of(None, class(1, 3)), Kind::Computer);
        assert_eq!(kind_of(None, class(2, 3)), Kind::Phone);
        assert_eq!(kind_of(None, class(4, 1)), Kind::Headset);
        assert_eq!(kind_of(None, class(4, 6)), Kind::Headphones);
        assert_eq!(kind_of(None, class(5, 16)), Kind::Keyboard);
        assert_eq!(kind_of(None, class(5, 32)), Kind::Mouse);
        assert_eq!(kind_of(None, class(5, 8)), Kind::Controller);
        assert_eq!(kind_of(None, class(6, 4)), Kind::Printer);
        // And nothing invented for something that said nothing at all: a row
        // that guessed would be worse than one that only gives the name.
        assert_eq!(kind_of(None, 0), Kind::Unknown);
        assert_eq!(kind_of(None, 0).title(), None);
    }

    fn heard(name: &str, paired: bool, connected: bool, strength: Option<i16>) -> Device {
        Device {
            path: format!("/org/bluez/hci-test0/dev_{name}"),
            controller: "/org/bluez/hci-test0".to_string(),
            address: "00:00:5E:00:53:00".to_string(),
            name: name.to_string(),
            kind: Kind::Unknown,
            paired,
            connected,
            strength,
            battery: None,
            doing: None,
        }
    }

    /// What the machine is on first, then what it remembers, then everything
    /// else by how well it is heard.
    ///
    /// The last of those is banded, and that is the point of the test rather
    /// than a detail of it: an RSSI moves several dBm between one advertisement
    /// and the next while nothing whatever has happened, and a list that
    /// reordered on every one of those would be a list where the press lands on
    /// whatever slid under the cursor.
    #[test]
    fn what_the_machine_is_on_is_at_the_top() {
        let mut devices = vec![
            heard("far", false, false, Some(-90)),
            heard("near", false, false, Some(-40)),
            heard("paired", true, false, None),
            heard("on", true, true, Some(-70)),
        ];
        order(&mut devices);
        assert_eq!(
            devices
                .iter()
                .map(|device| device.name.as_str())
                .collect::<Vec<_>>(),
            ["on", "paired", "near", "far"]
        );

        // Two readings a few dBm apart are one distance, and the list is then
        // in the one order that does not move: by name.
        let mut devices = vec![
            heard("b", false, false, Some(-52)),
            heard("a", false, false, Some(-45)),
        ];
        order(&mut devices);
        assert_eq!(
            devices
                .iter()
                .map(|device| device.name.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );

        // And a real difference does move it.
        let mut devices = vec![
            heard("b", false, false, Some(-30)),
            heard("a", false, false, Some(-95)),
        ];
        order(&mut devices);
        assert_eq!(
            devices
                .iter()
                .map(|device| device.name.as_str())
                .collect::<Vec<_>>(),
            ["b", "a"]
        );
    }

    /// A passkey is six digits with the leading zeros kept: the number 12 is
    /// `000012` on the screen of the thing being paired with, and a panel that
    /// said `12` would be asking the user to compare two things that do not look
    /// alike.
    #[test]
    fn a_passkey_is_shown_the_way_the_device_shows_it() {
        assert_eq!(code(12), "000012");
        assert_eq!(code(0), "000000");
        assert_eq!(code(123_456), "123456");
    }

    /// The name a device has when it has no name: the address out of its own
    /// object path.
    #[test]
    fn the_address_in_a_path() {
        assert_eq!(
            address_in("/org/bluez/hci0/dev_00_00_5E_00_53_01"),
            "00:00:5E:00:53:01"
        );
        // Anything that is not one is left alone rather than mangled.
        assert_eq!(address_in("/org/bluez/hci0"), "/org/bluez/hci0");
    }

    /// What the kernel calls a controller, which is the only name that tells
    /// two of them apart — BlueZ names both after the machine.
    #[test]
    fn the_controller_in_a_path() {
        assert_eq!(leaf("/org/bluez/hci0"), "hci0");
        assert_eq!(leaf("hci0"), "hci0");
    }

    /// Which questions are answered by typing, and which by agreeing. It
    /// decides whether the on-screen keyboard comes up with the panel, and a
    /// keyboard over a question with no field is a keyboard that cannot be
    /// dismissed.
    #[test]
    fn which_questions_have_a_field() {
        assert!(Question::Passkey.typed());
        assert!(Question::Pin.typed());
        assert!(!Question::Authorize.typed());
        assert!(!Question::Confirm { code: code(1) }.typed());
        assert!(!Question::Show { code: code(1) }.typed());
        // And the one with nothing to answer at all, which is what takes its
        // own panel away when the pairing ends.
        assert!(Question::Show { code: code(1) }.shown());
        assert!(!Question::Confirm { code: code(1) }.shown());
    }
}
