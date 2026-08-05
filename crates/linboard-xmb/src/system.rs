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
use std::sync::{Arc, Condvar, Mutex};
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
const BACKLIGHT_OVERRIDE: &str = "LINBOARD_BACKLIGHT";

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

impl State {
    fn slot(&mut self, knob: Knob) -> &mut Slot {
        match knob {
            Knob::Volume => &mut self.volume,
            Knob::Brightness => &mut self.brightness,
        }
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
            .name("linboard-quick".to_string())
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

    /// Say which display the shell is on, and whether its bars are on screen.
    pub fn watch(&self, display: Option<&str>, on_screen: bool) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let moved = state.display.as_deref() != display;
        if !moved && state.on_screen == on_screen {
            return;
        }
        state.display = display.map(str::to_string);
        state.on_screen = on_screen;
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

        while self.tick() {
            self.wait();
        }
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
            }
        };

        if work.display != self.on {
            self.shared.forget(Knob::Brightness);
            self.on = work.display.clone();
            self.screen = self.on.as_deref().and_then(|name| self.screens.on(name));
            self.asked[1] = false;
            match &self.screen {
                Some(screen) => tracing::info!(display = ?self.on, how = screen.name(), "brightness"),
                None => tracing::debug!(display = ?self.on, "nothing can dim this screen"),
            }
            // The epoch moved under `forget`, so the snapshot taken above is
            // stale for this control. Next pass reads it properly.
            self.mark_dirty();
            return true;
        }

        self.drive_volume(&work);
        self.drive_brightness(&work);
        true
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

    /// Sleep until there is something to do — or, while the bars are on
    /// screen, until it is time to read them again.
    fn wait(&self) {
        let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.done || state.dirty {
            return;
        }
        if state.on_screen {
            let _ = self.shared.signal.wait_timeout(state, REFRESH);
        } else {
            let _ = self.shared.signal.wait(state);
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
                let value: f32 = out.split_once("Volume:")?.1.split_whitespace().next()?.parse().ok()?;
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
            Audio::Pulse => drop(run("pactl", &["set-sink-volume", "@DEFAULT_SINK@", &percent])),
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
                let out = run("ddcutil", &["--bus", &bus.to_string(), "--terse", "getvcp", "10"])?;
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
    let mut found = Vec::new();
    let mut bus = None;
    for line in out.lines().map(str::trim) {
        if line.starts_with("Display ") {
            bus = None;
        } else if let Some(rest) = line.strip_prefix("I2C bus:") {
            bus = rest.trim().rsplit_once("i2c-").and_then(|(_, n)| n.parse().ok());
        } else if let Some(connector) = drm_connector(line) {
            if let Some(bus) = bus.take() {
                found.push((connector, bus));
            }
        }
    }
    tracing::info!(monitors = ?found, "ddc/ci");
    found
}

/// The connector out of a `DRM connector: card1-HDMI-A-1` line, as Wayland
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
            first_percent("Volume: front-left: 19661 /  30% / -31.37 dB,   front-right: 19661 /  30%"),
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
    #[test]
    fn a_monitor_is_matched_to_its_wayland_output_by_connector() {
        assert_eq!(
            drm_connector("DRM connector:    card1-HDMI-A-1"),
            Some("HDMI-A-1".to_string())
        );
        // `detect` and `detect --terse` disagree about the spelling.
        assert_eq!(
            drm_connector("DRM_connector:    card1-DP-2"),
            Some("DP-2".to_string())
        );
        // Nothing in front to strip.
        assert_eq!(
            drm_connector("DRM connector: DP-2"),
            Some("DP-2".to_string())
        );
        assert_eq!(drm_connector("I2C bus:   /dev/i2c-6"), None);
        assert_eq!(drm_connector("Display 1"), None);
    }

    #[test]
    fn the_two_monitors_of_a_desktop_are_each_found_on_their_own_bus() {
        // What `ddcutil detect --terse` prints, verbatim.
        let out = "Display 1\n   I2C bus:          /dev/i2c-6\n   DRM connector:    card1-HDMI-A-1\n   drm_connector_id: 454\n   Monitor:          SAM:LC27G5xT:H4ZRB04657\n\nDisplay 2\n   I2C bus:          /dev/i2c-9\n   DRM connector:    card1-DP-2\n   Monitor:          GSM:LG ULTRAGEAR+:412NTZN5Y021\n";
        let mut found = Vec::new();
        let mut bus = None;
        for line in out.lines().map(str::trim) {
            if line.starts_with("Display ") {
                bus = None;
            } else if let Some(rest) = line.strip_prefix("I2C bus:") {
                bus = rest
                    .trim()
                    .rsplit_once("i2c-")
                    .and_then(|(_, n)| n.parse::<u32>().ok());
            } else if let Some(connector) = drm_connector(line) {
                if let Some(bus) = bus.take() {
                    found.push((connector, bus));
                }
            }
        }
        assert_eq!(
            found,
            vec![("HDMI-A-1".to_string(), 6), ("DP-2".to_string(), 9)]
        );
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
        let root = std::env::temp_dir().join(format!("linboard-backlight-{}", std::process::id()));
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
