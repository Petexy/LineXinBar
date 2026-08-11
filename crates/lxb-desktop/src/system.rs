//! The two things a session with nothing else in it still has to be able to
//! change: how loud it is, and how bright the screen is.
//!
//! Both go through the interfaces that exist *below* a desktop — the kernel's
//! own, and whatever audio server the session happens to be running — rather
//! than through a settings daemon. A shell that is the whole session has
//! nobody to ask, so nothing here may depend on one being there: no
//! `org.kde.Solid`, no `org.gnome.SettingsDaemon`, no applet to forward a
//! keypress to. What is left is the mixer and the panel, and this is how you
//! reach those.
//!
//! Everything happens on a worker thread. A monitor answers a DDC/CI request
//! in anywhere between forty milliseconds and half a second, over an i2c bus
//! that does not care that a frame is due; asking on the render thread would
//! drop a dozen frames every time a bar moved, and holding a direction down
//! would be unusable. So the shell reads the last answer, and moves the bar
//! itself the instant a key is pressed — see [`Quick::nudge`].

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Which of the two bars.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Knob {
    Volume,
    Brightness,
}

/// Where a bar stands, as the sidebar draws it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Level {
    /// 0 to 1.
    pub value: f32,
    /// Silenced without being turned down. Volume only; a screen that is not
    /// lit is just a screen at zero.
    pub muted: bool,
}

/// One application's own sound, as the mixer panel lists it.
///
/// An application rather than a sound: a browser with three tabs playing has
/// three streams open in the server and is one thing the user wants to turn
/// down, so they are grouped here and the row moves all of them together. What
/// they are grouped by is the program behind them — two tabs are one `zen`, and
/// two games launched from one library are not.
#[derive(Debug, Clone, PartialEq)]
pub struct Stream {
    /// Stands for the application, not for any one of its sounds.
    ///
    /// Derived from the name rather than taken from the server, so that a tab
    /// falling silent does not renumber the panel under the user's hand: the
    /// row for an application keeps its identity for as long as that
    /// application is making any noise at all.
    pub key: u32,
    /// The sounds it has open, as the mixer numbers them.
    pub inputs: Vec<u32>,
    /// What the application calls itself, for a row that has no better name.
    pub name: String,
    /// The program behind it, which is what the installed catalogue is searched
    /// with for the icon and the name a user actually reads.
    pub binary: Option<String>,
    /// The loudest of its sounds, which is what the user is hearing.
    pub level: Level,
}

/// Which way sound is going.
///
/// The two questions the machine asks about a device are the same question
/// twice, and every one of these programs spells them `sink` and `source`. The
/// words the page uses are output and input, which are what they are called
/// everywhere a person rather than a server is reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Out of the machine: what everything on it plays through.
    Output,
    /// Into it: what everything on it records from.
    Input,
}

/// One device the machine can play through or record from, as the sound server
/// lists them.
///
/// A *device* as the server means it, which is a card in one of its profiles
/// rather than a socket on the back of the machine: the same sound card is two
/// of these when it can be driven as stereo or as surround, and choosing
/// between them is choosing how it is driven.
#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    /// The server's own name for it — `alsa_output.…`, stable across sessions
    /// and across replugs, and what naming one to the server names.
    pub id: String,
    /// What the device calls itself, in the words the machine's own mixer would
    /// show: the card, without the profile.
    pub title: String,
    /// How it is being driven, where the server says so: `Analog Stereo`. This
    /// is what separates two rows standing for one piece of hardware.
    pub profile: Option<String>,
    /// The one the machine is using, which is the whole of what this page sets.
    pub default: bool,
}

/// What the machine has, both ways round.
///
/// One value rather than two lists passed about separately, because they are
/// read in one go and are one answer: a session where the server has gone away
/// has neither, and a page that showed a stale half of it would be showing
/// devices that cannot be chosen.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Devices {
    pub outputs: Vec<Device>,
    pub inputs: Vec<Device>,
    /// Whether a sound server answered at all.
    ///
    /// An empty list means two very different things, and the page has to say
    /// which: a machine with no sound card in it, or a session with nothing
    /// running that could be asked. Only the first is about the hardware.
    pub server: bool,
}

impl Devices {
    /// Nothing, before anything has been read — and the value a session with no
    /// worker thread keeps.
    pub const fn none() -> Self {
        Self {
            outputs: Vec::new(),
            inputs: Vec::new(),
            server: false,
        }
    }

    pub fn of(&self, direction: Direction) -> &[Device] {
        match direction {
            Direction::Output => &self.outputs,
            Direction::Input => &self.inputs,
        }
    }

    fn of_mut(&mut self, direction: Direction) -> &mut Vec<Device> {
        match direction {
            Direction::Output => &mut self.outputs,
            Direction::Input => &mut self.inputs,
        }
    }
}

/// How far one press of Left or Right moves a bar.
///
/// A twentieth: coarse enough that crossing the whole range is a second of a
/// held key rather than five, fine enough that the smallest correction anyone
/// actually wants is one press.
const STEP: f32 = 0.05;

/// The lowest the brightness bar will go.
///
/// Not zero, unlike the volume. Silence is obvious and one press undoes it; a
/// screen turned all the way off has taken the bar with it, and there is
/// nothing left on the display to find the way back with.
const DIMMEST: f32 = 0.05;

/// How often a bar that is on screen is read back, so volume changed
/// elsewhere — by an application's own mixer, or by the keys on a keyboard
/// that has them — turns up here too.
const REFRESH: Duration = Duration::from_secs(2);

/// How long any one of these programs is given before it is killed off.
///
/// Generous, because i2c is slow, and finite because a monitor that has
/// stopped answering must not take the volume bar down with it.
const PATIENCE: Duration = Duration::from_secs(4);

/// How often the worker looks in on a program it is waiting for.
const POLL: Duration = Duration::from_millis(8);

/// Names a backlight device outright, for the machines where finding one is
/// the part that goes wrong: two devices for one panel, or a screen this
/// cannot know is built in. The value is a directory under
/// `/sys/class/backlight`, or any directory shaped like one.
const BACKLIGHT_OVERRIDE: &str = "LXB_BACKLIGHT";

/// Where the kernel lists the panels it can dim itself.
const BACKLIGHT_CLASS: &str = "/sys/class/backlight";

/// The mixer controls worth trying, in the order a machine with no sound
/// server is likely to want them. The first that exists wins.
const ALSA_CONTROLS: [&str; 4] = ["Master", "PCM", "Speaker", "Headphone"];

/// The bars, and the worker that keeps them true.
pub struct Quick {
    shared: Arc<Shared>,
}

struct Shared {
    state: Mutex<State>,
    /// Woken by every press, and whenever the sidebar arrives on a display.
    /// Between those the worker sleeps: a refresh nobody is looking at is a
    /// subprocess every two seconds for the length of the session.
    signal: Condvar,
}

#[derive(Default)]
struct State {
    volume: Slot,
    brightness: Slot,
    /// The applications making a noise, as the worker last found them, and what
    /// the user has asked of them since.
    ///
    /// One epoch for the whole list rather than one per row, because a listing
    /// is taken in one go: a press on any row throws away the reading that was
    /// already on its way back, whichever rows it was going to describe.
    streams: Vec<Stream>,
    stream_asks: Vec<StreamAsk>,
    streams_epoch: u64,
    /// What the machine can play through and record from, and what the user
    /// has asked it to use since.
    ///
    /// One epoch again, and for the listing's own reason: both directions come
    /// out of one pass over the server, so a press about the output makes the
    /// answer already on its way back stale about the input too.
    devices: Devices,
    device_asks: Vec<(Direction, String)>,
    devices_epoch: u64,
    /// Whether the page that lists the devices is on screen, which is the only
    /// time they are worth reading again. Its own flag rather than a use of
    /// [`State::on_screen`]: that one is the guide's sidebar, and what it keeps
    /// fresh is the bars and the mixer. These are two different things to be
    /// looking at, each costing its own subprocesses, and neither implies the
    /// other.
    watching_devices: bool,
    /// The display the shell is on. Tracked whether or not the menu is open,
    /// so the brightness bar has an answer the moment the menu appears rather
    /// than a few hundred milliseconds of i2c later.
    display: Option<String>,
    /// Whether the bars are being drawn, and so worth keeping fresh.
    on_screen: bool,
    /// Something arrived while the worker was busy; go round again rather
    /// than sleeping on a signal that has already been sent.
    dirty: bool,
    /// The shell is going away.
    done: bool,
}

/// One control, as the two threads share it.
#[derive(Default)]
struct Slot {
    /// What the sidebar draws, and `None` for a control this machine has not
    /// got. Written by the worker *and* by a press: a bar that waited for the
    /// hardware to confirm a keystroke would trail a held key by half a
    /// screen.
    level: Option<Level>,
    /// A position the user has asked for that the worker has not passed on
    /// yet. One value rather than a queue — holding Right asks for where the
    /// key has got to, not for every step it went through.
    wanted: Option<f32>,
    /// A mute state the user has asked for. A state and not a toggle: two
    /// presses that each say "the other one" cancel out if they arrive
    /// together, and two that each name a state do not.
    mute: Option<bool>,
    /// Bumped by every press. A reading that began before the last press is
    /// thrown away instead of published, which is what stops a refresh in
    /// flight from dragging the bar back to where it was.
    epoch: u64,
}

/// What the user has asked of one application's sound, waiting to be passed on.
///
/// Kept per application rather than merged into one, and replaced rather than
/// queued: holding Right on a row asks for where the key has got to on *that*
/// row, and says nothing about the row above it.
#[derive(Debug, Clone)]
struct StreamAsk {
    /// The sounds to act on, taken when the press landed. The worker cannot
    /// look them up itself: by the time it runs, the listing it would look them
    /// up in is the one this press is about to invalidate.
    inputs: Vec<u32>,
    wanted: Option<f32>,
    mute: Option<bool>,
}

impl State {
    fn slot(&mut self, knob: Knob) -> &mut Slot {
        match knob {
            Knob::Volume => &mut self.volume,
            Knob::Brightness => &mut self.brightness,
        }
    }

    /// Note what has been asked of one application, folding it into anything
    /// already waiting for it.
    fn ask_stream(&mut self, inputs: Vec<u32>, wanted: Option<f32>, mute: Option<bool>) {
        match self.stream_asks.iter_mut().find(|ask| ask.inputs == inputs) {
            Some(ask) => {
                ask.wanted = wanted.or(ask.wanted);
                ask.mute = mute.or(ask.mute);
            }
            None => self.stream_asks.push(StreamAsk {
                inputs,
                wanted,
                mute,
            }),
        }
        self.streams_epoch += 1;
        self.dirty = true;
    }
}

impl Quick {
    /// Start looking.
    ///
    /// Discovery costs a handful of subprocesses and, for a screen with no
    /// backlight, a conversation with every monitor on the machine's i2c
    /// buses. All of it happens on the worker, behind the first frame.
    pub fn start() -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            signal: Condvar::new(),
        });
        let worker = Arc::clone(&shared);
        if let Err(err) = std::thread::Builder::new()
            .name("lxb-quick".to_string())
            .spawn(move || Worker::new(worker).run())
        {
            tracing::warn!(?err, "no worker thread; the quick settings bars are off");
        }
        Self { shared }
    }

    /// Where a bar stands, or `None` for a control this machine has not got —
    /// which is also how the sidebar knows to leave the row out entirely.
    pub fn level(&self, knob: Knob) -> Option<Level> {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.slot(knob).level
    }

    /// The applications making a noise.
    ///
    /// Read from the moment the sidebar is on screen rather than from the
    /// moment the mixer panel is opened, and deliberately: the panel grows out
    /// of a tile on that sidebar, and one that opened holding only the row it
    /// could answer for immediately would visibly fill itself in afterwards.
    /// What it costs is one more subprocess every couple of seconds for as long
    /// as an overlay the user is looking at is up.
    pub fn streams(&self) -> Vec<Stream> {
        let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.streams.clone()
    }

    /// Say which display the shell is on, and whether its bars are on screen.
    pub fn watch(&self, display: Option<&str>, on_screen: bool) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let moved = state.display.as_deref() != display;
        if !moved && state.on_screen == on_screen {
            return;
        }
        state.display = display.map(str::to_string);
        state.on_screen = on_screen;
        // What was playing is not kept while nobody is looking: an application
        // that fell silent in the meantime must not be in the mixer the next
        // time the sidebar is opened.
        if !on_screen {
            state.streams.clear();
            state.stream_asks.clear();
            state.streams_epoch += 1;
        }
        // Only worth waking for something that changes what to read: arriving
        // on a display, or the bars coming into view.
        if moved || on_screen {
            state.dirty = true;
            self.shared.signal.notify_one();
        }
    }

    /// Move a bar by one step, and tell the worker to make it so.
    ///
    /// The move lands on screen immediately, before anything has been asked of
    /// the hardware. Reports whether it moved.
    pub fn nudge(&self, knob: Knob, delta: i32) -> bool {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let slot = state.slot(knob);
        let Some(level) = slot.level else {
            return false;
        };
        let floor = match knob {
            Knob::Volume => 0.0,
            Knob::Brightness => DIMMEST,
        };
        let value = (level.value + delta as f32 * STEP).clamp(floor, 1.0);
        // Turning it up is also how a muted session is brought back, which is
        // what every volume key on every machine does.
        let unmute = knob == Knob::Volume && level.muted && delta > 0;
        if value == level.value && !unmute {
            return false;
        }

        slot.level = Some(Level {
            value,
            muted: level.muted && !unmute,
        });
        slot.wanted = Some(value);
        if unmute {
            slot.mute = Some(false);
        }
        slot.epoch += 1;
        state.dirty = true;
        self.shared.signal.notify_one();
        true
    }

    /// Put a bar exactly where it has been clicked.
    ///
    /// The pointer's counterpart to [`Self::nudge`], and the same in every other
    /// respect: a direction can only ask for the next step, where a click names
    /// the value outright. Dragging a silenced session up brings it back, for
    /// the same reason turning it up does — it is the same gesture, made with a
    /// different instrument.
    pub fn set(&self, knob: Knob, value: f32) -> bool {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let slot = state.slot(knob);
        let Some(level) = slot.level else {
            return false;
        };
        let floor = match knob {
            Knob::Volume => 0.0,
            Knob::Brightness => DIMMEST,
        };
        let value = value.clamp(floor, 1.0);
        let unmute = knob == Knob::Volume && level.muted && value > level.value;
        if value == level.value && !unmute {
            return false;
        }

        slot.level = Some(Level {
            value,
            muted: level.muted && !unmute,
        });
        slot.wanted = Some(value);
        if unmute {
            slot.mute = Some(false);
        }
        slot.epoch += 1;
        state.dirty = true;
        self.shared.signal.notify_one();
        true
    }

    /// The same for one application's row.
    pub fn set_stream(&self, key: u32, value: f32) -> bool {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(stream) = state.streams.iter_mut().find(|stream| stream.key == key) else {
            return false;
        };
        let level = stream.level;
        let value = value.clamp(0.0, 1.0);
        let unmute = level.muted && value > level.value;
        if value == level.value && !unmute {
            return false;
        }
        stream.level = Level {
            value,
            muted: level.muted && !unmute,
        };
        let inputs = stream.inputs.clone();
        state.ask_stream(inputs, Some(value), unmute.then_some(false));
        self.shared.signal.notify_one();
        true
    }

    /// Move one application's row by one step, on exactly the terms the session
    /// bar moves on: the row lands on screen before the server has heard about
    /// it, and turning it up brings a silenced application back.
    ///
    /// Every sound the application has open goes to the same value. Two tabs
    /// left at different volumes are levelled by the first press, which is the
    /// price of the row being about the application rather than about whichever
    /// of its sounds the server listed first.
    pub fn nudge_stream(&self, key: u32, delta: i32) -> bool {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(stream) = state.streams.iter_mut().find(|stream| stream.key == key) else {
            return false;
        };
        let level = stream.level;
        let value = (level.value + delta as f32 * STEP).clamp(0.0, 1.0);
        let unmute = level.muted && delta > 0;
        if value == level.value && !unmute {
            return false;
        }
        stream.level = Level {
            value,
            muted: level.muted && !unmute,
        };
        let inputs = stream.inputs.clone();
        state.ask_stream(inputs, Some(value), unmute.then_some(false));
        self.shared.signal.notify_one();
        true
    }

    /// Silence one application, or bring it back. Reports whether there was a
    /// row to do it to.
    pub fn toggle_stream_mute(&self, key: u32) -> bool {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(stream) = state.streams.iter_mut().find(|stream| stream.key == key) else {
            return false;
        };
        let muted = !stream.level.muted;
        stream.level = Level {
            muted,
            ..stream.level
        };
        let inputs = stream.inputs.clone();
        state.ask_stream(inputs, None, Some(muted));
        self.shared.signal.notify_one();
        true
    }

    /// What the machine can play through and record from, as the worker last
    /// found them.
    pub fn devices(&self) -> Devices {
        let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.devices.clone()
    }

    /// Say whether the page that lists the devices is on screen, and so whether
    /// the listing is worth keeping fresh.
    ///
    /// The counterpart of [`Self::watch`] for a page rather than a sidebar. A
    /// device is plugged in and unplugged while the session runs, and the only
    /// moment that has to be noticed is while somebody is looking at the list of
    /// them; the rest of the time this is three subprocesses saying what they
    /// said two seconds ago.
    pub fn watch_devices(&self, listing: bool) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.watching_devices == listing {
            return;
        }
        state.watching_devices = listing;
        // Only worth waking for the page arriving. The page leaving means the
        // worker has one fewer thing to do next time it is up, which can wait
        // until something else wakes it.
        if listing {
            state.dirty = true;
            self.shared.signal.notify_one();
        }
    }

    /// Send everything the machine plays to this device from now on, or take
    /// everything it records from this one. Reports whether it was one of the
    /// devices listed.
    ///
    /// The list is marked here rather than when the server confirms, on exactly
    /// the terms a bar moves before the hardware has heard about it: the user
    /// has just pressed a button and the row they pressed is the one that has to
    /// look chosen. The epoch is what stops the listing already on its way back
    /// from marking the old one again.
    pub fn use_device(&self, direction: Direction, id: &str) -> bool {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let listed = state.devices.of_mut(direction);
        if !listed.iter().any(|device| device.id == id) {
            return false;
        }
        for device in listed.iter_mut() {
            device.default = device.id == id;
        }
        // Replaced rather than queued, per direction: two presses on this page
        // ask for the last device pressed, and a press about the output says
        // nothing about the input.
        match state
            .device_asks
            .iter_mut()
            .find(|(asked, _)| *asked == direction)
        {
            Some(ask) => ask.1 = id.to_string(),
            None => state.device_asks.push((direction, id.to_string())),
        }
        state.devices_epoch += 1;
        state.dirty = true;
        self.shared.signal.notify_one();
        true
    }

    /// Silence the session, or bring it back. Reports whether there was a
    /// volume control to do it to.
    pub fn toggle_mute(&self) -> bool {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let slot = state.slot(Knob::Volume);
        let Some(level) = slot.level else {
            return false;
        };
        let muted = !level.muted;
        slot.level = Some(Level { muted, ..level });
        slot.mute = Some(muted);
        slot.epoch += 1;
        state.dirty = true;
        self.shared.signal.notify_one();
        true
    }
}

impl Drop for Quick {
    fn drop(&mut self) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.done = true;
        // Told, not waited for: the worker may be a second into a conversation
        // with a monitor, and nothing is gained by holding the session open
        // until it finishes.
        self.shared.signal.notify_one();
    }
}

impl Shared {
    /// Hand a reading to the sidebar, unless a press overtook it.
    fn publish(&self, knob: Knob, epoch: u64, level: Option<Level>) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let slot = state.slot(knob);
        if slot.epoch == epoch {
            slot.level = level;
        }
    }

    /// Hand a listing to the mixer panel, unless a press overtook it.
    ///
    /// The levels the user has already moved are kept over the ones that have
    /// just been read: a press bumps the epoch, so this only ever lands on a
    /// listing taken after the last one.
    fn publish_streams(&self, epoch: u64, streams: Vec<Stream>) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.streams_epoch == epoch {
            state.streams = streams;
        }
    }

    /// Hand a device listing to the Settings column, unless a press overtook
    /// it. The same rule the mixer panel's listing is published under, and for
    /// the same reason: the row the user has just chosen must not be unmarked
    /// by an answer that was taken before they chose it.
    fn publish_devices(&self, epoch: u64, devices: Devices) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.devices_epoch == epoch {
            state.devices = devices;
        }
    }

    /// Take a control away, whatever is in flight for it.
    ///
    /// Unconditional, unlike a reading: the bar is not out of date, it belongs
    /// to a screen that is no longer the one in front of the user. Bumping the
    /// epoch is what discards the reading still on its way back.
    fn forget(&self, knob: Knob) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let slot = state.slot(knob);
        slot.level = None;
        slot.wanted = None;
        slot.mute = None;
        slot.epoch += 1;
    }
}

// --- the worker ------------------------------------------------------------

/// What one pass round the worker's loop has to do, taken under the lock in
/// one go so that the calls below — none of them quick — happen with the
/// render thread free to read.
struct Work {
    display: Option<String>,
    on_screen: bool,
    volume: Ask,
    brightness: Ask,
    streams: StreamWork,
    devices: DeviceWork,
}

/// What the mixer rows are asking for this pass.
struct StreamWork {
    asks: Vec<StreamAsk>,
    epoch: u64,
}

/// What the device page is asking for this pass, and whether it is up.
struct DeviceWork {
    asks: Vec<(Direction, String)>,
    epoch: u64,
    watching: bool,
}

/// What a control is being asked for this pass.
struct Ask {
    wanted: Option<f32>,
    mute: Option<bool>,
    epoch: u64,
}

impl Ask {
    fn any(&self) -> bool {
        self.wanted.is_some() || self.mute.is_some()
    }
}

struct Worker {
    shared: Arc<Shared>,
    audio: Option<Audio>,
    screens: Screens,
    /// The display [`Self::screen`] belongs to.
    on: Option<String>,
    screen: Option<Screen>,
    /// Whether each control has been looked for at all yet. Until it has, one
    /// reading is taken even with nothing on screen, so that the sidebar knows
    /// which rows to lay out before it is opened for the first time.
    asked: [bool; 2],
}

impl Worker {
    fn new(shared: Arc<Shared>) -> Self {
        Self {
            shared,
            audio: None,
            screens: Screens::default(),
            on: None,
            screen: None,
            asked: [false; 2],
        }
    }

    fn run(mut self) {
        self.audio = Audio::detect();
        match &self.audio {
            Some(audio) => tracing::info!(through = audio.name(), "volume"),
            None => tracing::info!("no mixer answered; the volume bar is off"),
        }
        self.first_device_listing();

        while self.tick() {
            self.wait();
        }
    }

    /// Read the devices once, before the first pass.
    ///
    /// Unlike everything else here, which is read when something asks for it.
    /// The Settings page that lists them is built from whatever is known at the
    /// moment the column is assembled, and a page that came up saying the
    /// machine has no sound devices and then filled itself in would have told
    /// the user something untrue first. This costs three subprocesses at
    /// startup, on the worker, behind the first frame — where discovery already
    /// costs a handful.
    fn first_device_listing(&mut self) {
        let Some(audio) = self.audio.as_ref() else {
            return;
        };
        let epoch = {
            let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            state.devices_epoch
        };
        let devices = audio.devices();
        tracing::info!(
            outputs = devices.outputs.len(),
            inputs = devices.inputs.len(),
            "sound devices"
        );
        self.shared.publish_devices(epoch, devices);
    }

    /// One pass. `false` once the shell has gone.
    fn tick(&mut self) -> bool {
        let work = {
            let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.done {
                return false;
            }
            state.dirty = false;
            Work {
                display: state.display.clone(),
                on_screen: state.on_screen,
                volume: take(&mut state.volume),
                brightness: take(&mut state.brightness),
                streams: StreamWork {
                    asks: std::mem::take(&mut state.stream_asks),
                    epoch: state.streams_epoch,
                },
                devices: DeviceWork {
                    asks: std::mem::take(&mut state.device_asks),
                    epoch: state.devices_epoch,
                    watching: state.watching_devices,
                },
            }
        };

        // Before the display check below, which returns early: sound devices
        // have nothing to do with which screen is in front of the user, and a
        // chosen device dropped because a monitor was plugged in during the
        // same pass would be a row left marked for a change nobody made.
        self.drive_devices(&work);

        if work.display != self.on {
            self.shared.forget(Knob::Brightness);
            self.on = work.display.clone();
            self.screen = self.on.as_deref().and_then(|name| self.screens.on(name));
            self.asked[1] = false;
            match &self.screen {
                Some(screen) => {
                    tracing::info!(display = ?self.on, how = screen.name(), "brightness")
                }
                None => tracing::debug!(display = ?self.on, "nothing can dim this screen"),
            }
            // The epoch moved under `forget`, so the snapshot taken above is
            // stale for this control. Next pass reads it properly.
            self.mark_dirty();
            return true;
        }

        self.drive_volume(&work);
        self.drive_streams(&work);
        self.drive_brightness(&work);
        true
    }

    /// Pass on the device the user has chosen, and read the list back while the
    /// page that shows it is up.
    ///
    /// The read is three programs — the two listings and the one answer saying
    /// which of them is in use — so it happens on the same terms the mixer's
    /// listing does: only while somebody is looking, and never on the pass that
    /// carried a press out, where the answer would have been taken before the
    /// press landed.
    fn drive_devices(&mut self, work: &Work) {
        let Some(audio) = self.audio.as_ref() else {
            return;
        };
        for (direction, id) in &work.devices.asks {
            audio.use_device(*direction, id);
        }
        if work.devices.watching && work.devices.asks.is_empty() {
            self.shared
                .publish_devices(work.devices.epoch, audio.devices());
        }
    }

    /// Pass on what the mixer rows have been asked for, and read back what
    /// every application is playing while the panel is up.
    fn drive_streams(&mut self, work: &Work) {
        let Some(audio) = self.audio.as_ref() else {
            return;
        };
        for ask in &work.streams.asks {
            for input in &ask.inputs {
                if let Some(value) = ask.wanted {
                    audio.set_stream(*input, value);
                }
                if let Some(muted) = ask.mute {
                    audio.mute_stream(*input, muted);
                }
            }
        }
        // Only while the sidebar the mixer opens from is up: this is a
        // subprocess of its own, and nothing is looking at the answer otherwise.
        //
        // And no read back on the pass that carried a press out, exactly as the
        // session bar does it: the panel already shows what was asked for, and
        // a server that has not caught up would pull the row backwards.
        if work.on_screen && work.streams.asks.is_empty() {
            self.shared
                .publish_streams(work.streams.epoch, audio.streams());
        }
    }

    fn drive_volume(&mut self, work: &Work) {
        let Some(audio) = self.audio.as_ref() else {
            self.settle(Knob::Volume, work.volume.epoch, None);
            return;
        };
        if work.volume.any() {
            if let Some(value) = work.volume.wanted {
                audio.set(value);
            }
            if let Some(muted) = work.volume.mute {
                audio.mute(muted);
            }
            // No read back this pass. What the bar shows is what was just
            // asked for, and a server that has not caught up would pull it
            // backwards; the next refresh will confirm it.
            return;
        }
        if self.asked[0] && !work.on_screen {
            return;
        }
        self.settle(Knob::Volume, work.volume.epoch, audio.read());
    }

    fn drive_brightness(&mut self, work: &Work) {
        let Some(screen) = self.screen.as_mut() else {
            self.settle(Knob::Brightness, work.brightness.epoch, None);
            return;
        };
        if let Some(value) = work.brightness.wanted {
            screen.set(value);
            return;
        }
        if self.asked[1] && !work.on_screen {
            return;
        }
        let level = screen.read().map(|value| Level {
            value,
            muted: false,
        });
        self.settle(Knob::Brightness, work.brightness.epoch, level);
    }

    /// Publish a reading and note that this control has now been looked for,
    /// so a machine without it is not asked again every couple of seconds.
    fn settle(&mut self, knob: Knob, epoch: u64, level: Option<Level>) {
        let slot = match knob {
            Knob::Volume => 0,
            Knob::Brightness => 1,
        };
        if self.asked[slot] && level.is_none() {
            return;
        }
        self.shared.publish(knob, epoch, level);
        self.asked[slot] = true;
    }

    fn mark_dirty(&self) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.dirty = true;
    }

    /// Sleep until there is something to do — or, while the bars or the device
    /// page are on screen, until it is time to read them again.
    fn wait(&self) {
        let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.done || state.dirty {
            return;
        }
        // Bound rather than dropped: the guard has to outlive the wait, or the
        // lock is released and immediately retaken and nothing has been
        // waited for.
        if state.on_screen || state.watching_devices {
            let _held = self.shared.signal.wait_timeout(state, REFRESH);
        } else {
            let _held = self.shared.signal.wait(state);
        }
    }
}

fn take(slot: &mut Slot) -> Ask {
    Ask {
        wanted: slot.wanted.take(),
        mute: slot.mute.take(),
        epoch: slot.epoch,
    }
}

// --- volume ----------------------------------------------------------------

/// The mixer this session actually has.
enum Audio {
    /// WirePlumber, PipeWire's session manager: the native control on a
    /// current Linux audio stack, and the one that agrees with what every
    /// other client sees.
    WirePlumber,
    /// PulseAudio's, which PipeWire answers to as well. Reached when `wpctl`
    /// is not installed.
    Pulse,
    /// The kernel mixer, by name of control. No sound server at all, which is
    /// the case this whole module exists for.
    Alsa(String),
}

impl Audio {
    /// Find one by asking, rather than by guessing from socket paths:
    /// `PULSE_SERVER` and `PIPEWIRE_REMOTE` both move the socket, and a server
    /// that answers is the only proof worth having.
    fn detect() -> Option<Self> {
        for candidate in [Audio::WirePlumber, Audio::Pulse] {
            if candidate.read().is_some() {
                return Some(candidate);
            }
        }
        ALSA_CONTROLS.iter().find_map(|control| {
            let candidate = Audio::Alsa((*control).to_string());
            candidate.read().is_some().then_some(candidate)
        })
    }

    fn name(&self) -> String {
        match self {
            Audio::WirePlumber => "wireplumber".to_string(),
            Audio::Pulse => "pulseaudio".to_string(),
            Audio::Alsa(control) => format!("alsa {control}"),
        }
    }

    fn read(&self) -> Option<Level> {
        match self {
            Audio::WirePlumber => {
                let out = run("wpctl", &["get-volume", SINK])?;
                // `Volume: 0.30 [MUTED]`
                let value: f32 = out
                    .split_once("Volume:")?
                    .1
                    .split_whitespace()
                    .next()?
                    .parse()
                    .ok()?;
                Some(Level {
                    value: value.clamp(0.0, 1.0),
                    muted: out.contains("[MUTED]"),
                })
            }
            Audio::Pulse => {
                let value = first_percent(&run("pactl", &["get-sink-volume", "@DEFAULT_SINK@"])?)?;
                let muted = run("pactl", &["get-sink-mute", "@DEFAULT_SINK@"])?.contains("yes");
                Some(Level { value, muted })
            }
            Audio::Alsa(control) => {
                // `-M` asks for the mapped scale, which is what a bar should
                // show: the raw one is in dB steps, and half of it is spent on
                // volumes too quiet to hear.
                let out = run("amixer", &["-M", "get", control])?;
                Some(Level {
                    value: first_percent(&out)?,
                    muted: out.contains("[off]"),
                })
            }
        }
    }

    fn set(&self, value: f32) {
        let percent = percent(value);
        match self {
            Audio::WirePlumber => drop(run("wpctl", &["set-volume", SINK, &percent])),
            Audio::Pulse => drop(run(
                "pactl",
                &["set-sink-volume", "@DEFAULT_SINK@", &percent],
            )),
            Audio::Alsa(control) => drop(run("amixer", &["-M", "-q", "set", control, &percent])),
        }
    }

    fn mute(&self, muted: bool) {
        let flag = if muted { "1" } else { "0" };
        match self {
            Audio::WirePlumber => drop(run("wpctl", &["set-mute", SINK, flag])),
            Audio::Pulse => drop(run("pactl", &["set-sink-mute", "@DEFAULT_SINK@", flag])),
            Audio::Alsa(control) => {
                let word = if muted { "mute" } else { "unmute" };
                drop(run("amixer", &["-q", "set", control, word]));
            }
        }
    }

    /// What every application is playing, one row per application.
    ///
    /// Asked of `pactl` on both sound servers, WirePlumber included. A stream
    /// is a thing PulseAudio invented and PipeWire answers for, and `wpctl`
    /// only reaches one by the node number buried in the tree `wpctl status`
    /// prints — a listing meant to be read rather than parsed. A PipeWire
    /// session without `pipewire-pulse` has no answer here and gets a mixer
    /// with the session's own output in it and nothing else, which is the
    /// truth about what this can reach.
    ///
    /// The kernel mixer has none at all: ALSA is the case where the
    /// applications are mixed before anything can see them separately.
    fn streams(&self) -> Vec<Stream> {
        match self {
            Audio::WirePlumber | Audio::Pulse => run("pactl", &["list", "sink-inputs"])
                .as_deref()
                .map(|listed| parse_sink_inputs(listed, own()))
                .unwrap_or_default(),
            Audio::Alsa(_) => Vec::new(),
        }
    }

    /// Everything the machine can play through and record from, and which of
    /// them it is using.
    ///
    /// Asked of `pactl` on both sound servers, as the mixer's listing is, and
    /// for a sharper version of the same reason. `wpctl status` marks the
    /// defaults with an asterisk in a tree drawn for a person to read; `wpctl
    /// set-default` then takes the node number that tree is indexed by. Reading
    /// a device list out of it would be parsing a drawing. PulseAudio's
    /// interface names a device by a name that outlives the session, which is
    /// also what makes the answer worth writing down anywhere.
    ///
    /// Three programs: the two listings, and `pactl info` for which of them is
    /// in use. The alternative is `get-default-sink` and `get-default-source`,
    /// which is one program more for the same two answers.
    ///
    /// The kernel mixer has none of this. ALSA has no notion of the device the
    /// *machine* uses — that is a sound server's job, and the case with no
    /// server is the case where every program picks its own.
    fn devices(&self) -> Devices {
        match self {
            Audio::WirePlumber | Audio::Pulse => {
                let info = run("pactl", &["info"]).unwrap_or_default();
                let listing = |direction: Direction| {
                    let out = run("pactl", &["list", direction.listing()]).unwrap_or_default();
                    parse_devices(&out, direction, in_use(&info, direction).as_deref())
                };
                Devices {
                    outputs: listing(Direction::Output),
                    inputs: listing(Direction::Input),
                    server: true,
                }
            }
            Audio::Alsa(_) => Devices::none(),
        }
    }

    /// Make one of them the machine's own, which is a statement about the whole
    /// session rather than about this shell: every application that opens the
    /// default device afterwards gets this one, and the server moves the ones
    /// already playing.
    ///
    /// The server is also what remembers it. Nothing here writes it down — see
    /// [`crate::settings`], where the same is said of the volume — because a
    /// shell that kept its own copy would be a second opinion about it at every
    /// login, and the one that lost would be whichever the user set last.
    fn use_device(&self, direction: Direction, id: &str) {
        if matches!(self, Audio::Alsa(_)) {
            return;
        }
        if run("pactl", &[direction.command(), id]).is_none() {
            tracing::warn!(
                ?direction,
                id,
                "the sound server would not take that device"
            );
            return;
        }
        tracing::info!(?direction, id, "sound device");
    }

    fn set_stream(&self, input: u32, value: f32) {
        if matches!(self, Audio::Alsa(_)) {
            return;
        }
        drop(run(
            "pactl",
            &["set-sink-input-volume", &input.to_string(), &percent(value)],
        ));
    }

    fn mute_stream(&self, input: u32, muted: bool) {
        if matches!(self, Audio::Alsa(_)) {
            return;
        }
        let flag = if muted { "1" } else { "0" };
        drop(run(
            "pactl",
            &["set-sink-input-mute", &input.to_string(), flag],
        ));
    }
}

impl Direction {
    /// What `pactl list` calls them.
    fn listing(self) -> &'static str {
        match self {
            Direction::Output => "sinks",
            Direction::Input => "sources",
        }
    }

    /// How each block of that listing begins. `Sink #` rather than `Sink`,
    /// which is also what keeps `Sink Input #` — the mixer's own listing, and
    /// the one thing in this file that starts with the same word — out of it.
    fn head(self) -> &'static str {
        match self {
            Direction::Output => "Sink #",
            Direction::Input => "Source #",
        }
    }

    /// The line of `pactl info` that names the one in use.
    fn in_use(self) -> &'static str {
        match self {
            Direction::Output => "Default Sink:",
            Direction::Input => "Default Source:",
        }
    }

    /// The subcommand that changes it.
    fn command(self) -> &'static str {
        match self {
            Direction::Output => "set-default-sink",
            Direction::Input => "set-default-source",
        }
    }
}

/// Which device `pactl info` says the machine is using.
fn in_use(info: &str, direction: Direction) -> Option<String> {
    let named = info
        .lines()
        .find_map(|line| line.trim().strip_prefix(direction.in_use()))?
        .trim();
    (!named.is_empty()).then(|| named.to_string())
}

/// What `pactl list sinks` or `pactl list sources` says, as one row per device.
///
/// The same shape as [`parse_sink_inputs`]: a block per device, `Sink #N` and
/// then indented lines, with the device's own description of itself in a
/// `Properties:` section under that.
///
/// Two of those properties are preferred to the `Description:` line above them,
/// which is the two of them run together — `Navi 48 HDMI/DP Audio Controller
/// Digital Stereo (HDMI 2)`. Split, they are a row: the card on the line the
/// eye lands on, and how it is being driven under it. A device that carries
/// neither keeps the whole description, and one that has not even that is named
/// by the server's own name for it, because a device that cannot be named is
/// still a device that can be chosen.
///
/// Monitors are left out of the inputs. Every output has one — it is that
/// output's own sound, offered back for recording — so listing them would
/// double the input page with rows that are not microphones, and the row a user
/// wants would be somewhere in the middle of them.
fn parse_devices(out: &str, direction: Direction, in_use: Option<&str>) -> Vec<Device> {
    let mut devices: Vec<Device> = Vec::new();
    let mut block: Option<Listed> = None;

    // Every block is finished by the next one starting, or by the end of the
    // output — hence the trailing empty line, which no block can begin with.
    for line in out.lines().chain(std::iter::once("")) {
        if line.starts_with(direction.head()) {
            if let Some(finished) = block.take() {
                finished.add_to(&mut devices, in_use);
            }
            block = Some(Listed::default());
            continue;
        }
        let Some(listed) = block.as_mut() else {
            continue;
        };
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Name:") {
            listed.id = Some(rest.trim().to_string());
        } else if let Some(rest) = trimmed.strip_prefix("Description:") {
            listed.described = Some(rest.trim().to_string());
        } else if let Some(rest) = trimmed.strip_prefix("Monitor of Sink:") {
            // `n/a` is a source that is not one, which is every real input.
            listed.monitors = (rest.trim() != "n/a").then(|| rest.trim().to_string());
        } else if let Some(said) = property(trimmed, "device.description") {
            listed.title = Some(said);
        } else if let Some(said) = property(trimmed, "device.profile.description") {
            listed.profile = Some(said);
        }
    }
    if let Some(finished) = block.take() {
        finished.add_to(&mut devices, in_use);
    }
    devices
}

/// One device, as the listing describes it, before it becomes a row.
#[derive(Default)]
struct Listed {
    id: Option<String>,
    /// The `Description:` line: the card and its profile run together.
    described: Option<String>,
    title: Option<String>,
    profile: Option<String>,
    /// The output this is the sound of, for a source that is one.
    monitors: Option<String>,
}

impl Listed {
    fn add_to(self, devices: &mut Vec<Device>, in_use: Option<&str>) {
        if self.monitors.is_some() {
            return;
        }
        let Some(id) = self.id else {
            return;
        };
        let (title, profile) = match (self.title, self.profile) {
            // The two halves the `Description:` line is made of, kept apart.
            (Some(title), Some(profile)) => (title, Some(profile)),
            // Anything less than both, and the line the server wrote is the
            // better answer: half a description, under a title that is the
            // other half of the same sentence, reads as a device the machine's
            // own mixer would call something else.
            (title, _) => (self.described.or(title).unwrap_or_else(|| id.clone()), None),
        };
        devices.push(Device {
            default: in_use == Some(id.as_str()),
            id,
            title,
            profile,
        });
    }
}

/// One sound, as the listing describes it, before it is folded into the row
/// for the application making it.
struct Block {
    id: u32,
    level: Level,
    name: Option<String>,
    binary: Option<String>,
    /// The process behind it, where the server says. See [`Own`].
    pid: Option<u32>,
}

/// How the shell's own sounds appear in that listing, so that they can be left
/// out of it.
///
/// They are in the mixer already, as `System`: that row is the shell's own
/// volume, kept in its settings and applied to every clip before it is played.
/// A second row for the same sounds would be two controls over one thing — and
/// the one the user reached for first would be the one that does not last, as
/// the sound server forgets a stream the moment it closes and the shell opens
/// a new one for the next click.
struct Own {
    pid: u32,
    /// The program's own file name, taken from the running binary rather than
    /// written down, so that it is still the shell's name after a rename.
    program: Option<String>,
}

impl Own {
    fn of_this_process() -> Self {
        Own {
            pid: std::process::id(),
            program: std::env::current_exe().ok().and_then(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            }),
        }
    }

    /// Whether one sound in the listing is the shell's own.
    ///
    /// The process settles it where the server gives one, and PulseAudio's ALSA
    /// plug-in does. PipeWire's own says nothing whatever about the process and
    /// names the stream `PipeWire ALSA [lxb-desktop]`, so the program is read
    /// out of the name as well — the brackets included, because they are what
    /// separates a plug-in playing *for* this program from an application that
    /// merely mentions it. A name that matches under some other process is
    /// this program again, running twice, and its clicks are no more one of the
    /// applications in the mixer than the first copy's are.
    fn made(&self, block: &Block) -> bool {
        if block.pid == Some(self.pid) {
            return true;
        }
        let Some(program) = self.program.as_deref() else {
            return false;
        };
        block.binary.as_deref() == Some(program)
            || block
                .name
                .as_deref()
                .is_some_and(|name| name.contains(&format!("[{program}]")))
    }
}

/// This process, worked out once: `current_exe` is a walk of `/proc` and the
/// listing is parsed every couple of seconds for as long as an overlay is up.
fn own() -> &'static Own {
    static OWN: OnceLock<Own> = OnceLock::new();
    OWN.get_or_init(Own::of_this_process)
}

/// What `pactl list sink-inputs` says, as one row per application — leaving out
/// `own`, whose sounds the mixer answers for under `System`.
///
/// The format is a block per sound, `Sink Input #N` and then indented lines,
/// with the application's own description of itself in a `Properties:` section
/// under that. Only five of those lines matter, and a sound missing any of them
/// is still listed: an application that names itself badly should turn up in
/// the mixer under a poor name rather than not turn up at all.
fn parse_sink_inputs(out: &str, own: &Own) -> Vec<Stream> {
    let mut streams: Vec<Stream> = Vec::new();
    let mut input: Option<Block> = None;

    // Every block is finished by the next one starting, or by the end of the
    // output — hence the trailing empty line, which no block can begin with.
    for line in out.lines().chain(std::iter::once("")) {
        let trimmed = line.trim();
        if let Some(head) = trimmed.strip_prefix("Sink Input #") {
            if let Some(finished) = input.take() {
                add_stream(&mut streams, finished, own);
            }
            input = head.trim().parse().ok().map(|id| Block {
                id,
                level: Level {
                    value: 1.0,
                    muted: false,
                },
                name: None,
                binary: None,
                pid: None,
            });
            continue;
        }
        let Some(block) = input.as_mut() else {
            continue;
        };
        if let Some(rest) = trimmed.strip_prefix("Volume:") {
            if let Some(value) = first_percent(rest) {
                block.level.value = value;
            }
        } else if let Some(rest) = trimmed.strip_prefix("Mute:") {
            block.level.muted = rest.trim() == "yes";
        } else if let Some(said) = property(trimmed, "application.name") {
            block.name = Some(said);
        } else if let Some(said) = property(trimmed, "application.process.binary") {
            block.binary = Some(said);
        } else if let Some(said) = property(trimmed, "application.process.id") {
            block.pid = said.parse().ok();
        }
    }
    if let Some(finished) = input.take() {
        add_stream(&mut streams, finished, own);
    }
    streams
}

/// Fold one sound into the row for the application that is making it, or drop
/// it if the application is this one.
///
/// The row shows the loudest of them, because that is what the user is hearing
/// and so what they are reaching for the mixer about, and it is silent only
/// when every one of its sounds is.
fn add_stream(streams: &mut Vec<Stream>, block: Block, own: &Own) {
    if own.made(&block) {
        return;
    }
    let Block {
        id,
        level,
        name,
        binary,
        ..
    } = block;
    // The program first: two tabs of one browser name themselves after the
    // browser, but an application that gives every sound its own
    // `application.name` would otherwise be several rows.
    let grouped = binary
        .clone()
        .or_else(|| name.clone())
        .unwrap_or_else(|| id.to_string());
    let key = key_of(&grouped);
    match streams.iter_mut().find(|stream| stream.key == key) {
        Some(stream) => {
            stream.inputs.push(id);
            stream.level = Level {
                value: stream.level.value.max(level.value),
                muted: stream.level.muted && level.muted,
            };
        }
        None => streams.push(Stream {
            key,
            inputs: vec![id],
            name: name.unwrap_or_else(|| grouped.clone()),
            binary,
            level,
        }),
    }
}

/// The value of one of the properties in a sink input's block:
/// `application.name = "Firefox"`, quotes and all.
fn property(line: &str, wanted: &str) -> Option<String> {
    let (key, value) = line.split_once('=')?;
    if key.trim() != wanted {
        return None;
    }
    let value = value.trim().trim_matches('"').trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// A number standing for an application's name, so that the row for it keeps
/// its identity across listings — and so the panel can name a row in a message
/// to the worker without carrying a string through it.
///
/// FNV-1a, which is enough for a handful of rows: two applications that
/// collided would share one row rather than lose one, and the names are the
/// dozen programs playing sound on one machine.
fn key_of(name: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in name.to_ascii_lowercase().bytes() {
        hash ^= byte as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// Whatever the session is playing through now, rather than a device chosen
/// once at startup: headphones plugged in halfway through move it.
const SINK: &str = "@DEFAULT_AUDIO_SINK@";

fn percent(value: f32) -> String {
    format!("{}%", (value.clamp(0.0, 1.0) * 100.0).round() as u32)
}

/// The first `NN%` in a program's output, as 0 to 1.
fn first_percent(text: &str) -> Option<f32> {
    let at = text.find('%')?;
    let start = text[..at]
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_ascii_digit())
        .map(|(index, _)| index)
        .last()?;
    let percent: f32 = text[start..at].parse().ok()?;
    Some((percent / 100.0).clamp(0.0, 1.0))
}

// --- brightness ------------------------------------------------------------

/// Everything on this machine that can dim a screen.
#[derive(Default)]
struct Screens {
    /// The kernel's backlight class: a panel wired *into* the machine rather
    /// than plugged into it. Looked for once, at no cost — it is a directory.
    backlight: Option<Backlight>,
    /// Monitors that answer DDC/CI, by DRM connector, which is the same name
    /// Wayland gives the output — and so is what lets the bar belong to the
    /// screen in front of the user rather than to whichever monitor answered
    /// first.
    ///
    /// `None` until a display turns up that the kernel has no backlight for:
    /// probing means talking to every i2c bus on the machine, and a laptop
    /// never needs to.
    ddc: Option<Vec<(String, u32)>>,
    /// Whether the backlight has been looked for yet.
    scanned: bool,
}

#[derive(Debug, Clone)]
struct Backlight {
    dir: PathBuf,
    max: u32,
    /// Named by [`BACKLIGHT_OVERRIDE`], and so used for any display rather
    /// than only for one that looks built in.
    forced: bool,
}

/// A screen, and the way it is dimmed.
enum Screen {
    /// A panel the kernel drives directly.
    Panel(Backlight),
    /// A monitor over DDC/CI. `max` is the scale it last said its brightness
    /// was on; every monitor this has met says 100, and the standard does not
    /// require it to.
    Monitor { bus: u32, max: u32 },
}

impl Screens {
    /// The way to dim the display Wayland calls `display`, if there is one.
    fn on(&mut self, display: &str) -> Option<Screen> {
        if !self.scanned {
            self.backlight = find_backlight();
            self.scanned = true;
        }

        // A panel the kernel dims itself. Which screens those are is in the
        // name: eDP, LVDS and DSI are the ways a display is wired into a
        // machine, and the backlight class only ever describes one of those.
        if let Some(panel) = self
            .backlight
            .clone()
            .filter(|light| light.forced || is_internal(display))
        {
            return Some(Screen::Panel(panel));
        }

        let ddc = self.ddc.get_or_insert_with(detect_ddc);
        let bus = ddc
            .iter()
            .find(|(connector, _)| connector == display)
            .map(|(_, bus)| *bus)?;
        Some(Screen::Monitor { bus, max: 100 })
    }
}

impl Screen {
    fn name(&self) -> &'static str {
        match self {
            Screen::Panel(_) => "backlight",
            Screen::Monitor { .. } => "ddc/ci",
        }
    }

    fn read(&mut self) -> Option<f32> {
        match self {
            Screen::Panel(panel) => {
                let raw = read_number(&panel.dir.join("brightness"))?;
                Some((raw as f32 / panel.max as f32).clamp(0.0, 1.0))
            }
            Screen::Monitor { bus, max } => {
                let out = run(
                    "ddcutil",
                    &["--bus", &bus.to_string(), "--terse", "getvcp", "10"],
                )?;
                let (value, scale) = parse_vcp(&out)?;
                *max = scale.max(1);
                Some((value as f32 / *max as f32).clamp(0.0, 1.0))
            }
        }
    }

    fn set(&mut self, value: f32) {
        match self {
            Screen::Panel(panel) => {
                let raw = (value.clamp(0.0, 1.0) * panel.max as f32).round() as u32;
                let file = panel.dir.join("brightness");
                if let Err(err) = std::fs::write(&file, raw.to_string()) {
                    tracing::warn!(path = %file.display(), ?err, "could not set the brightness");
                }
            }
            Screen::Monitor { bus, max } => {
                let raw = (value.clamp(0.0, 1.0) * *max as f32).round() as u32;
                // `--noverify` skips the read-back the monitor would otherwise
                // be asked for after every write, which doubles the time a
                // held key takes to answer for no gain: the next refresh reads
                // it anyway.
                drop(run(
                    "ddcutil",
                    &[
                        "--bus",
                        &bus.to_string(),
                        "--noverify",
                        "setvcp",
                        "10",
                        &raw.to_string(),
                    ],
                ));
            }
        }
    }
}

/// Whether a display is wired into the machine, going by the name of the
/// connector it is on.
fn is_internal(display: &str) -> bool {
    let name = display.to_ascii_uppercase();
    ["EDP", "LVDS", "DSI"]
        .iter()
        .any(|kind| name.starts_with(kind))
}

/// The panel backlight to use, if there is one that can actually be moved.
fn find_backlight() -> Option<Backlight> {
    if let Some(forced) = std::env::var_os(BACKLIGHT_OVERRIDE) {
        let dir = PathBuf::from(forced);
        let found = open_backlight(&dir, true);
        if found.is_none() {
            tracing::warn!(
                path = %dir.display(),
                "{BACKLIGHT_OVERRIDE} does not name a backlight this can write to"
            );
        }
        return found;
    }

    let mut devices: Vec<PathBuf> = std::fs::read_dir(BACKLIGHT_CLASS)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .collect();
    // The kernel's own order of preference: a device that drives the panel
    // beats one that asks the firmware to, which on the machines that have
    // both is the difference between a smooth ramp and four steps.
    devices.sort_by_key(|dir| (backlight_rank(dir), dir.clone()));
    devices.iter().find_map(|dir| open_backlight(dir, false))
}

fn backlight_rank(dir: &Path) -> u8 {
    match std::fs::read_to_string(dir.join("type"))
        .unwrap_or_default()
        .trim()
    {
        "raw" => 0,
        "platform" => 1,
        _ => 2,
    }
}

fn open_backlight(dir: &Path, forced: bool) -> Option<Backlight> {
    let max = read_number(&dir.join("max_brightness"))?;
    if max == 0 {
        return None;
    }
    // Offered only if it can be written to. The usual udev rule hands that to
    // the `video` group; without it the sidebar would carry a bar that does
    // nothing, which is worse than carrying no bar.
    std::fs::OpenOptions::new()
        .write(true)
        .open(dir.join("brightness"))
        .ok()?;
    Some(Backlight {
        dir: dir.to_path_buf(),
        max,
        forced,
    })
}

fn read_number(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// The monitors that answer DDC/CI, paired with the i2c bus each is on.
fn detect_ddc() -> Vec<(String, u32)> {
    let Some(out) = run("ddcutil", &["detect", "--terse"]) else {
        tracing::debug!("ddcutil found nothing, or is not installed");
        return Vec::new();
    };
    let found = parse_ddc_detect(&out);
    tracing::info!(monitors = ?found, "ddc/ci");
    found
}

/// What `ddcutil detect --terse` says, as connector-and-bus pairs.
///
/// A display is a run of lines starting at `Display N`; the bus and the
/// connector each arrive on their own line, in no guaranteed order, and a
/// display that gives one without the other is no use and is dropped.
fn parse_ddc_detect(out: &str) -> Vec<(String, u32)> {
    let mut found = Vec::new();
    let mut bus = None;
    for line in out.lines().map(str::trim) {
        if line.starts_with("Display ") {
            bus = None;
        } else if let Some(rest) = line.strip_prefix("I2C bus:") {
            bus = rest
                .trim()
                .rsplit_once("i2c-")
                .and_then(|(_, n)| n.parse().ok());
        } else if let Some(connector) = drm_connector(line) {
            if let Some(bus) = bus.take() {
                found.push((connector, bus));
            }
        }
    }
    found
}

/// The connector out of a `DRM connector: card0-HDMI-A-1` line, as Wayland
/// names it: the kernel puts the card the connector hangs off in front, and an
/// output is called the rest.
fn drm_connector(line: &str) -> Option<String> {
    let (head, rest) = line.split_once(':')?;
    let head = head.trim();
    if !head.eq_ignore_ascii_case("drm connector") && !head.eq_ignore_ascii_case("drm_connector") {
        return None;
    }
    let name = rest.trim();
    Some(match name.split_once('-') {
        Some((card, tail)) if card.starts_with("card") => tail.to_string(),
        _ => name.to_string(),
    })
}

/// `VCP 10 C 51 100` — the value and the scale it is on.
fn parse_vcp(out: &str) -> Option<(u32, u32)> {
    let mut fields = out.split_whitespace();
    if fields.next()? != "VCP" {
        return None;
    }
    fields.next()?;
    // `C` for a continuous feature, which brightness is. Anything else is a
    // monitor answering about something this did not ask for.
    if fields.next()? != "C" {
        return None;
    }
    Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
}

// --- running things --------------------------------------------------------

/// Run a program and give it a limited time to answer, returning its output
/// when it succeeds.
///
/// Killed rather than waited on: an i2c bus with a confused device on it can
/// hold a `ddcutil` call open indefinitely, and the volume bar must not be
/// stuck behind it.
///
/// The output is read after the child has exited, which is only safe because
/// everything here answers in a line or two — a program that filled the pipe
/// would block before it could exit, and be killed for it.
fn run(program: &str, args: &[&str]) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        // Everything here is a program being read rather than run, and every
        // one of them answers in the language it was asked in: `Mute: yes` is
        // `Stumm: ja` under a German session, and `Volume: 0.30` is `0,30`
        // under a Polish one — a number `parse` then refuses. The C locale is
        // the one output this can be written against.
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let deadline = Instant::now() + PATIENCE;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL),
            Ok(None) => {
                tracing::warn!(program, ?args, "gave up waiting and killed it");
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Err(err) => {
                tracing::debug!(program, ?err, "could not wait for it");
                return None;
            }
        }
    };
    if !status.success() {
        return None;
    }

    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_percentage_is_found_wherever_a_mixer_hides_it() {
        // pactl
        assert_eq!(
            first_percent(
                "Volume: front-left: 19661 /  30% / -31.37 dB,   front-right: 19661 /  30%"
            ),
            Some(0.30)
        );
        // amixer, whose first number is a raw one and must not be mistaken
        // for the percentage that comes after it
        assert_eq!(
            first_percent("  Front Left: Playback 19661 [30%] [on]"),
            Some(0.30)
        );
        assert_eq!(first_percent("[100%]"), Some(1.0));
        assert_eq!(first_percent("[0%]"), Some(0.0));

        assert_eq!(first_percent("Volume: 0.30"), None, "no percentage at all");
        assert_eq!(first_percent("%"), None, "nor a bare sign");
    }

    #[test]
    fn wireplumber_reports_a_fraction_and_says_so_when_it_is_silenced() {
        let level = |out: &str| {
            let value: f32 = out
                .split_once("Volume:")
                .unwrap()
                .1
                .split_whitespace()
                .next()
                .unwrap()
                .parse()
                .unwrap();
            Level {
                value,
                muted: out.contains("[MUTED]"),
            }
        };
        assert_eq!(
            level("Volume: 0.30\n"),
            Level {
                value: 0.30,
                muted: false
            }
        );
        assert_eq!(
            level("Volume: 0.30 [MUTED]\n"),
            Level {
                value: 0.30,
                muted: true
            }
        );
    }

    /// Two tabs of one browser, a game, and something that names itself
    /// nothing at all. Invented, but shaped exactly like `pactl list
    /// sink-inputs` — the blocks, the indentation and the four lines that
    /// matter.
    const SINK_INPUTS: &str = "\
Sink Input #196
\tDriver: PipeWire
\tCorked: no
\tMute: no
\tVolume: front-left: 19661 /  30% / -31.37 dB,   front-right: 19661 /  30%
\t        balance 0.00
\tProperties:
\t\tapplication.name = \"Zen\"
\t\tapplication.process.binary = \"zen\"
\t\tmedia.name = \"Home / X\"
\t\tmodule-stream-restore.id = \"sink-input-by-application-name:Zen\"

Sink Input #212
\tCorked: no
\tMute: yes
\tVolume: front-left: 45875 /  70% / -9.29 dB,   front-right: 45875 /  70%
\tProperties:
\t\tapplication.name = \"Zen\"
\t\tapplication.process.binary = \"zen\"
\t\tmedia.name = \"A video somewhere\"

Sink Input #904
\tCorked: no
\tMute: no
\tVolume: front-left: 65536 / 100% / 0.00 dB,   front-right: 65536 / 100%
\tProperties:
\t\tapplication.name = \"Some Game\"
\t\tapplication.process.binary = \"somegame.exe\"

Sink Input #905
\tMute: no
\tVolume: front-left: 32768 /  50% / -6.02 dB,   front-right: 32768 /  50%
";

    /// A shell that made none of the sounds in a listing, for the tests that
    /// are about everything except its own row.
    fn elsewhere() -> Own {
        Own {
            pid: 4242,
            program: Some("lxb-desktop".to_string()),
        }
    }

    #[test]
    fn one_applications_several_sounds_are_one_row() {
        let streams = parse_sink_inputs(SINK_INPUTS, &elsewhere());
        assert_eq!(streams.len(), 3, "{streams:#?}");

        let browser = &streams[0];
        assert_eq!(browser.name, "Zen");
        assert_eq!(browser.inputs, vec![196, 212]);
        // The loudest of them, because that is what the user is hearing and so
        // what they opened the mixer about.
        assert!((browser.level.value - 0.70).abs() < 1e-6, "{browser:?}");
        // And not silent: one of the two is still playing.
        assert!(!browser.level.muted);

        assert_eq!(streams[1].name, "Some Game");
        assert_eq!(streams[1].inputs, vec![904]);
        assert!((streams[1].level.value - 1.0).abs() < 1e-6);

        // A sound that says nothing about itself is still a row: an
        // application that names itself badly should turn up in the mixer
        // under a poor name rather than not turn up at all.
        assert_eq!(streams[2].inputs, vec![905]);
        assert_eq!(streams[2].name, "905");
        assert_eq!(streams[2].binary, None);
    }

    /// Silent only when every one of its sounds is.
    #[test]
    fn an_application_is_silenced_when_all_of_it_is() {
        let both_muted = SINK_INPUTS.replace(
            "\tMute: no\n\tVolume: front-left: 19661",
            "\tMute: yes\n\tVolume: front-left: 19661",
        );
        let streams = parse_sink_inputs(&both_muted, &elsewhere());
        assert!(streams[0].level.muted, "{:?}", streams[0]);
        assert!(!streams[1].level.muted, "the game was never silenced");
    }

    /// The row keeps its identity while the application does, so a tab falling
    /// silent does not move the highlight onto the row below it.
    #[test]
    fn a_rows_key_outlives_the_sounds_behind_it() {
        let all = parse_sink_inputs(SINK_INPUTS, &elsewhere());
        let one_left: String = SINK_INPUTS
            .split("\nSink Input #212")
            .next()
            .unwrap()
            .to_string();
        let fewer = parse_sink_inputs(&one_left, &elsewhere());
        assert_eq!(fewer[0].key, all[0].key);
        assert_eq!(fewer[0].inputs, vec![196]);
        // And two applications are never one row.
        assert_ne!(all[0].key, all[1].key);
    }

    /// Both spellings of the shell's own stream, copied from what the two sound
    /// servers were seen to say on 2026-08-10. The first is PulseAudio's ALSA
    /// plug-in, which names the process; the second is PipeWire's, which names
    /// nothing but the program, in brackets.
    const OWN_INPUTS: &str = "\
Sink Input #9498
\tCorked: no
\tMute: no
\tVolume: front-left: 65536 / 100% / 0.00 dB,   front-right: 65536 / 100%
\tProperties:
\t\tapplication.name = \"ALSA plug-in [lxb-desktop]\"
\t\tapplication.process.id = \"342662\"
\t\tapplication.process.binary = \"lxb-desktop\"
\t\tmedia.name = \"ALSA Playback\"

Sink Input #9504
\tCorked: no
\tMute: no
\tVolume: front-left: 65536 / 100% / 0.00 dB,   front-right: 65536 / 100%
\tProperties:
\t\tapplication.name = \"PipeWire ALSA [lxb-desktop]\"
\t\tnode.name = \"alsa_playback.lxb-desktop\"
";

    /// The shell's own clicks are not one of the applications in the mixer.
    /// They are `System`, the row the shell's own volume is under, and a second
    /// row for them would be two controls over one thing.
    #[test]
    fn the_shells_own_sounds_are_never_a_row() {
        let own = Own {
            pid: 342662,
            program: Some("lxb-desktop".to_string()),
        };
        let listing = format!("{SINK_INPUTS}\n{OWN_INPUTS}");
        let streams = parse_sink_inputs(&listing, &own);
        assert_eq!(streams.len(), 3, "{streams:#?}");
        assert!(
            !streams.iter().any(|stream| stream
                .inputs
                .iter()
                .any(|id| { *id == 9498 || *id == 9504 })),
            "{streams:#?}"
        );

        // Neither of them under a second copy of the shell, which is this
        // program's sounds either way.
        let twice = parse_sink_inputs(&listing, &elsewhere());
        assert_eq!(twice.len(), 3, "{twice:#?}");

        // And a server that reports no process at all leaves everyone else's
        // sounds alone: the name has to be this program's, in the brackets the
        // plug-in puts it in.
        let anonymous = Own {
            pid: 342662,
            program: None,
        };
        assert_eq!(parse_sink_inputs(SINK_INPUTS, &anonymous).len(), 3);
    }

    #[test]
    fn a_property_is_read_without_its_quotes_and_only_under_its_own_name() {
        assert_eq!(
            property("application.name = \"Zen\"", "application.name"),
            Some("Zen".to_string())
        );
        // The name has to match outright: `module-stream-restore.id` carries
        // the application's name inside its value and is not it.
        assert_eq!(
            property(
                "module-stream-restore.id = \"sink-input-by-application-name:Zen\"",
                "application.name"
            ),
            None
        );
        assert_eq!(
            property("application.name = \"\"", "application.name"),
            None
        );
        assert_eq!(property("Corked: no", "application.name"), None);
    }

    /// Two outputs, shaped exactly like `pactl list sinks` — the blocks, the
    /// indentation, the properties section and the ports under it — with
    /// invented hardware in them. Nothing here may depend on what is plugged
    /// into the machine this is built on.
    ///
    /// The second is deliberately impoverished: a device that describes itself
    /// with nothing but the `Description:` line, which is what a virtual output
    /// created by a module looks like.
    const SINKS: &str = "\
Sink #41
\tState: SUSPENDED
\tName: alsa_output.pci-0000_00_00.0.test-stereo
\tDescription: Test Audio Controller Digital Stereo (Test 1)
\tDriver: PipeWire
\tMute: no
\tVolume: front-left: 26214 /  40% / -23.88 dB,   front-right: 26214 /  40%
\t        balance 0.00
\tBase Volume: 65536 / 100% / 0.00 dB
\tMonitor Source: alsa_output.pci-0000_00_00.0.test-stereo.monitor
\tProperties:
\t\tdevice.icon_name = \"audio-card-analog\"
\t\tdevice.profile.description = \"Digital Stereo (Test 1)\"
\t\tdevice.description = \"Test Audio Controller\"
\t\tnode.name = \"alsa_output.pci-0000_00_00.0.test-stereo\"
\tPorts:
\t\ttest-output: Test port (type: HDMI, priority: 5900)
\tActive Port: test-output

Sink #42
\tState: RUNNING
\tName: test_virtual_output
\tDescription: Test Virtual Output
\tDriver: PipeWire
\tMute: no
\tProperties:
\t\tnode.name = \"test_virtual_output\"
";

    /// Two monitors and one microphone, in the shape `pactl list sources` says
    /// it — the monitors first, because that is where they turn up.
    const SOURCES: &str = "\
Source #41
\tState: SUSPENDED
\tName: alsa_output.pci-0000_00_00.0.test-stereo.monitor
\tDescription: Monitor of Test Audio Controller Digital Stereo (Test 1)
\tDriver: PipeWire
\tMonitor of Sink: alsa_output.pci-0000_00_00.0.test-stereo
\tProperties:
\t\tdevice.profile.description = \"Digital Stereo (Test 1)\"
\t\tdevice.description = \"Test Audio Controller\"

Source #42
\tState: RUNNING
\tName: test_virtual_output.monitor
\tDescription: Monitor of Test Virtual Output
\tDriver: PipeWire
\tMonitor of Sink: test_virtual_output
\tProperties:
\t\tnode.name = \"test_virtual_output.monitor\"

Source #43
\tState: SUSPENDED
\tName: alsa_input.usb-Test_Microphone-00.mono-fallback
\tDescription: Test Microphone Mono
\tDriver: PipeWire
\tMonitor of Sink: n/a
\tProperties:
\t\tdevice.profile.description = \"Mono\"
\t\tdevice.description = \"Test Microphone\"
";

    /// What `pactl info` says, cut down to the two lines that are read out of
    /// it and enough of its neighbours to prove they are found among them.
    const INFO: &str = "\
Server Name: PulseAudio (on PipeWire 1.6.8)
Default Sample Specification: float32le 2ch 48000Hz
Default Sink: test_virtual_output
Default Source: alsa_input.usb-Test_Microphone-00.mono-fallback
Cookie: 0000:0000
";

    /// A device is a row with the card on the line the eye lands on and the
    /// profile under it — and the one the machine is using is marked.
    #[test]
    fn every_output_is_listed_and_the_one_in_use_is_marked() {
        let devices = parse_devices(
            SINKS,
            Direction::Output,
            in_use(INFO, Direction::Output).as_deref(),
        );
        assert_eq!(
            devices,
            vec![
                Device {
                    id: "alsa_output.pci-0000_00_00.0.test-stereo".to_string(),
                    title: "Test Audio Controller".to_string(),
                    profile: Some("Digital Stereo (Test 1)".to_string()),
                    default: false,
                },
                Device {
                    // Nothing but the `Description:` line to go on, so that is
                    // the whole row rather than half of one.
                    id: "test_virtual_output".to_string(),
                    title: "Test Virtual Output".to_string(),
                    profile: None,
                    default: true,
                },
            ]
        );
    }

    /// Every output has a monitor — its own sound, offered back for recording —
    /// and none of them is an input device.
    #[test]
    fn the_inputs_are_the_ones_that_are_not_monitors() {
        let devices = parse_devices(
            SOURCES,
            Direction::Input,
            in_use(INFO, Direction::Input).as_deref(),
        );
        assert_eq!(devices.len(), 1, "{devices:#?}");
        assert_eq!(devices[0].title, "Test Microphone");
        assert_eq!(devices[0].profile.as_deref(), Some("Mono"));
        assert!(devices[0].default);

        // And the line that says an output *has* a monitor is not the line that
        // says a source *is* one: every sink carries the first.
        assert_eq!(parse_devices(SINKS, Direction::Output, None).len(), 2);
    }

    #[test]
    fn the_device_in_use_is_read_from_the_server_and_only_under_its_own_name() {
        assert_eq!(
            in_use(INFO, Direction::Output),
            Some("test_virtual_output".to_string())
        );
        assert_eq!(
            in_use(INFO, Direction::Input),
            Some("alsa_input.usb-Test_Microphone-00.mono-fallback".to_string())
        );
        // A server that names neither: no row is marked, which is not the same
        // as no rows.
        assert_eq!(in_use("Server Name: Test\n", Direction::Output), None);
        assert!(parse_devices(SINKS, Direction::Output, None)
            .iter()
            .all(|device| !device.default));
    }

    /// The listing's own listing is not a device: `Sink Input #N` is one
    /// application's sound, and the two are told apart by the `#`.
    #[test]
    fn the_applications_playing_are_not_output_devices() {
        assert!(parse_devices(SINK_INPUTS, Direction::Output, None).is_empty());
    }

    /// The same rule the volume bar and the mixer rows are moved under: the row
    /// the user pressed is marked before the sound server has heard about it,
    /// because they are looking at it now.
    #[test]
    fn choosing_a_device_marks_it_before_the_server_hears_about_it() {
        let quick = Quick::start();
        {
            let mut state = quick.shared.state.lock().unwrap();
            state.devices = Devices {
                outputs: parse_devices(
                    SINKS,
                    Direction::Output,
                    in_use(INFO, Direction::Output).as_deref(),
                ),
                inputs: parse_devices(
                    SOURCES,
                    Direction::Input,
                    in_use(INFO, Direction::Input).as_deref(),
                ),
                server: true,
            };
        }
        let first = quick.devices().outputs[0].id.clone();

        assert!(quick.use_device(Direction::Output, &first));
        let moved = quick.devices();
        assert!(moved.outputs[0].default, "{moved:#?}");
        // One default, not two: choosing is moving the mark rather than adding
        // one.
        assert!(!moved.outputs[1].default, "{moved:#?}");
        // And the other direction was not touched by a press about this one.
        assert!(moved.inputs[0].default);

        let state = quick.shared.state.lock().unwrap();
        assert_eq!(state.device_asks, vec![(Direction::Output, first.clone())]);

        // A device that is not in the listing is not chosen, and asks nothing
        // of the server: the row was drawn from a listing that has since
        // changed under it.
        drop(state);
        assert!(!quick.use_device(Direction::Output, "test_device_that_left"));
        assert!(
            !quick.use_device(Direction::Input, &first),
            "wrong way round"
        );
        let state = quick.shared.state.lock().unwrap();
        assert_eq!(state.device_asks.len(), 1);
    }

    /// The whole reason a mixer row's position lives on this side of the
    /// worker, the same as the session bar's: a press has to land on screen
    /// before anything has been asked of the server.
    #[test]
    fn a_press_moves_a_mixer_row_before_the_server_hears_about_it() {
        let quick = Quick::start();
        {
            let mut state = quick.shared.state.lock().unwrap();
            state.streams = parse_sink_inputs(SINK_INPUTS, &elsewhere());
        }
        let key = quick.streams()[0].key;

        assert!(quick.nudge_stream(key, -1));
        let moved = quick.streams();
        assert!((moved[0].level.value - 0.65).abs() < 1e-6, "{moved:#?}");
        // Every sound the application has open is asked for, because the row is
        // about the application rather than about one of its sounds.
        let state = quick.shared.state.lock().unwrap();
        assert_eq!(state.stream_asks.len(), 1);
        assert_eq!(state.stream_asks[0].inputs, vec![196, 212]);
        assert!((state.stream_asks[0].wanted.unwrap() - 0.65).abs() < 1e-6);
        // And a row nobody is playing cannot be moved.
        drop(state);
        assert!(!quick.nudge_stream(key.wrapping_add(1), 1));
    }

    /// Turning an application up brings it back, exactly as turning the session
    /// up does — and silencing one leaves the others alone.
    #[test]
    fn turning_an_application_up_unmutes_only_that_application() {
        let quick = Quick::start();
        {
            let mut state = quick.shared.state.lock().unwrap();
            state.streams = parse_sink_inputs(SINK_INPUTS, &elsewhere());
        }
        let [browser, game] = [quick.streams()[0].key, quick.streams()[1].key];

        assert!(quick.toggle_stream_mute(browser));
        assert!(quick.streams()[0].level.muted);
        assert!(!quick.streams()[1].level.muted, "the game was not touched");

        assert!(quick.nudge_stream(browser, 1));
        assert!(!quick.streams()[0].level.muted);

        // Turning it down does not: silencing something quiet still leaves it
        // silenced.
        quick.toggle_stream_mute(game);
        quick.nudge_stream(game, -1);
        assert!(quick.streams()[1].level.muted);
    }

    /// Nothing is kept while nobody is looking: an application that fell silent
    /// with the sidebar shut must not be in the mixer when it opens again.
    #[test]
    fn putting_the_sidebar_away_forgets_what_was_playing() {
        let quick = Quick::start();
        {
            let mut state = quick.shared.state.lock().unwrap();
            state.streams = parse_sink_inputs(SINK_INPUTS, &elsewhere());
            state.on_screen = true;
        }
        quick.watch(None, false);
        assert!(quick.streams().is_empty());
    }

    #[test]
    fn a_monitors_answer_is_read_as_a_value_and_the_scale_it_is_on() {
        assert_eq!(parse_vcp("VCP 10 C 51 100\n"), Some((51, 100)));
        // Not every monitor is on a scale of a hundred, which is why the
        // second number is carried rather than assumed.
        assert_eq!(parse_vcp("VCP 10 C 7 10\n"), Some((7, 10)));

        // A feature that is not a continuous one is not a brightness.
        assert_eq!(parse_vcp("VCP 10 SNC x01\n"), None);
        assert_eq!(parse_vcp("DDC communication failed\n"), None);
    }

    /// The name has to come out matching what Wayland calls the output, or the
    /// bar ends up belonging to whichever monitor answered first.
    ///
    /// The connector spellings below are the kernel's naming form, which is
    /// what this parses; the numbering is invented, and no test here may be
    /// tied to the monitors on any one desk.
    #[test]
    fn a_monitor_is_matched_to_its_wayland_output_by_connector() {
        assert_eq!(
            drm_connector("DRM connector:    card0-HDMI-A-1"),
            Some("HDMI-A-1".to_string())
        );
        // `detect` and `detect --terse` disagree about the spelling.
        assert_eq!(
            drm_connector("DRM_connector:    card0-DP-1"),
            Some("DP-1".to_string())
        );
        // A second card is stripped the same way, whatever its index.
        assert_eq!(
            drm_connector("DRM connector:    card3-DP-1"),
            Some("DP-1".to_string())
        );
        // Nothing in front to strip.
        assert_eq!(
            drm_connector("DRM connector: DP-1"),
            Some("DP-1".to_string())
        );
        assert_eq!(drm_connector("I2C bus:   /dev/i2c-0"), None);
        assert_eq!(drm_connector("Display 1"), None);
    }

    /// Made-up monitors: the shape of `ddcutil detect --terse` is what is
    /// being parsed, and a real dump would carry someone's serial numbers.
    const DDC_DETECT: &str = "\
Display 1
   I2C bus:          /dev/i2c-0
   DRM connector:    card0-HDMI-A-1
   drm_connector_id: 100
   Monitor:          AAA:Test Monitor A:0000000001

Display 2
   I2C bus:          /dev/i2c-4
   DRM connector:    card0-DP-1
   drm_connector_id: 101
   Monitor:          BBB:Test Monitor B:0000000002
";

    #[test]
    fn two_monitors_are_each_found_on_their_own_bus() {
        assert_eq!(
            parse_ddc_detect(DDC_DETECT),
            vec![("HDMI-A-1".to_string(), 0), ("DP-1".to_string(), 4)]
        );
    }

    /// Half an answer is no answer: without both halves the bar would be sent
    /// down a bus belonging to some other monitor.
    #[test]
    fn a_display_missing_either_half_is_dropped() {
        let no_connector = "Display 1\n   I2C bus:          /dev/i2c-0\n";
        assert!(parse_ddc_detect(no_connector).is_empty());

        let no_bus = "Display 1\n   DRM connector:    card0-DP-1\n";
        assert!(parse_ddc_detect(no_bus).is_empty());

        // The bus does not carry over from the display before it.
        let mixed = "Display 1\n   I2C bus:          /dev/i2c-0\n\
                     Display 2\n   DRM connector:    card0-DP-1\n";
        assert!(parse_ddc_detect(mixed).is_empty());
    }

    #[test]
    fn only_a_screen_wired_into_the_machine_is_taken_to_have_a_backlight() {
        for internal in ["eDP-1", "eDP-2", "LVDS-1", "DSI-1"] {
            assert!(is_internal(internal), "{internal}");
        }
        for external in ["HDMI-A-1", "DP-2", "DVI-D-1", "X11-1"] {
            assert!(!is_internal(external), "{external}");
        }
    }

    /// A backlight is offered only when it can be written to. A bar the user
    /// can move that changes nothing is worse than no bar at all.
    #[test]
    fn a_backlight_that_cannot_be_written_to_is_not_offered() {
        let root = std::env::temp_dir().join(format!("lxb-backlight-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("max_brightness"), "255\n").unwrap();
        std::fs::write(root.join("brightness"), "128\n").unwrap();
        assert!(open_backlight(&root, true).is_some());

        // Nothing to write to at all.
        std::fs::remove_file(root.join("brightness")).unwrap();
        assert!(open_backlight(&root, true).is_none());

        // Nor a device with nowhere to go.
        std::fs::write(root.join("brightness"), "0\n").unwrap();
        std::fs::write(root.join("max_brightness"), "0\n").unwrap();
        assert!(open_backlight(&root, true).is_none());

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The whole reason the bar's position lives on this side of the worker:
    /// a press has to land on screen before anything has been asked of the
    /// hardware.
    #[test]
    fn a_press_moves_the_bar_before_the_hardware_hears_about_it() {
        let quick = Quick::start();
        {
            let mut state = quick.shared.state.lock().unwrap();
            state.volume.level = Some(Level {
                value: 0.5,
                muted: false,
            });
        }

        assert!(quick.nudge(Knob::Volume, 1));
        let level = quick.level(Knob::Volume).unwrap();
        assert!((level.value - 0.55).abs() < 1e-6, "{level:?}");

        // And it is the position the key has reached that is asked for, not
        // every step it went through.
        for _ in 0..3 {
            quick.nudge(Knob::Volume, 1);
        }
        let state = quick.shared.state.lock().unwrap();
        assert!((state.volume.level.unwrap().value - 0.70).abs() < 1e-6);
        assert!((state.volume.wanted.unwrap() - 0.70).abs() < 1e-6);
    }

    #[test]
    fn there_is_nothing_to_move_on_a_machine_without_the_control() {
        let quick = Quick::start();
        // Whatever the worker finds, brightness is not available for a display
        // nobody has named yet.
        assert!(!quick.nudge(Knob::Brightness, 1));
        assert!(!quick.toggle_mute() || quick.level(Knob::Volume).is_some());
    }

    /// Silence is one press to undo, but a screen turned all the way off has
    /// taken the bar with it.
    #[test]
    fn the_screen_can_be_dimmed_but_not_put_out() {
        let quick = Quick::start();
        {
            let mut state = quick.shared.state.lock().unwrap();
            state.brightness.level = Some(Level {
                value: 0.2,
                muted: false,
            });
            state.volume.level = Some(Level {
                value: 0.2,
                muted: false,
            });
        }

        for _ in 0..10 {
            quick.nudge(Knob::Brightness, -1);
            quick.nudge(Knob::Volume, -1);
        }
        assert_eq!(quick.level(Knob::Brightness).unwrap().value, DIMMEST);
        assert_eq!(quick.level(Knob::Volume).unwrap().value, 0.0);
    }

    /// Turning it up is how a muted session is brought back — the same thing
    /// the volume key on any keyboard does.
    #[test]
    fn turning_the_volume_up_unmutes_it() {
        let quick = Quick::start();
        {
            let mut state = quick.shared.state.lock().unwrap();
            state.volume.level = Some(Level {
                value: 0.4,
                muted: true,
            });
        }

        assert!(quick.nudge(Knob::Volume, 1));
        assert!(!quick.level(Knob::Volume).unwrap().muted);

        // Turning it down does not: silencing something quiet still leaves it
        // silenced.
        quick.toggle_mute();
        assert!(quick.level(Knob::Volume).unwrap().muted);
        quick.nudge(Knob::Volume, -1);
        assert!(quick.level(Knob::Volume).unwrap().muted);
    }

    /// A reading that set off before the last press must not undo it. This is
    /// the race that makes a held key look like it is fighting the user.
    #[test]
    fn a_reading_overtaken_by_a_press_is_thrown_away() {
        let quick = Quick::start();
        {
            let mut state = quick.shared.state.lock().unwrap();
            state.volume.level = Some(Level {
                value: 0.5,
                muted: false,
            });
        }
        let stale = quick.shared.state.lock().unwrap().volume.epoch;

        quick.nudge(Knob::Volume, 1);
        quick.shared.publish(
            Knob::Volume,
            stale,
            Some(Level {
                value: 0.5,
                muted: false,
            }),
        );
        assert!((quick.level(Knob::Volume).unwrap().value - 0.55).abs() < 1e-6);

        // The next reading, taken after the press, is believed.
        let now = quick.shared.state.lock().unwrap().volume.epoch;
        quick.shared.publish(
            Knob::Volume,
            now,
            Some(Level {
                value: 0.55,
                muted: false,
            }),
        );
        assert!((quick.level(Knob::Volume).unwrap().value - 0.55).abs() < 1e-6);
    }
}
