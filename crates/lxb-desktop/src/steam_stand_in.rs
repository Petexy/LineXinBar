//! The gamepad the kernel does not give the second-generation Steam Controller.
//!
//! [`crate::steam_hid`] reads that pad off `hidraw`, because `hid-steam` does
//! not claim it and no joystick node is ever made for it. That is enough for
//! the shell, which can read whatever it likes — and it is nothing at all for
//! everything else on the machine. A game does not read `hidraw`; it opens
//! `/dev/input`, finds a keyboard and a mouse where the controller should be,
//! and reports no controller connected. The pad works in the menu and dies the
//! moment somebody launches something with it.
//!
//! So the driver makes the missing device. A `uinput` gamepad stands where the
//! kernel's would be, saying what the pad says, and every program on the
//! machine finds a controller exactly where it looks for one.
//!
//! ## The shape it takes
//!
//! An Xbox controller's, to the code. Not because the pad is one — it has two
//! trackpads, a gyro and grip buttons that no Xbox pad has — but because that
//! shape is the one thing every program already agrees about. `xpad` has sent
//! the same set of codes since the first Xbox controller, so a device
//! declaring that set has told SDL, GilRs and RetroArch where every control is
//! without any of them having to have heard of this pad. See
//! [`crate::pads::xinput`], which recognises a controller from exactly these
//! codes and is what tells an emulator where the buttons are.
//!
//! The controls the shape has no room for are left off rather than invented:
//! the trackpads, the gyro, the grip buttons and the pad's own haptics. Those
//! reach a game through Steam, which drives the pad over `hidraw` itself and
//! can do all four; this stand-in cannot, because doing any of it means
//! *writing* to the pad, and writing to it is what takes it away from Steam.
//!
//! ## The button that is never sent
//!
//! `BTN_MODE` is declared and never emitted, which is the same bargain
//! [`crate::pad_guard`] strikes with every other controller on the machine.
//! Declaring it matters: SDL numbers a pad's buttons by walking its capability
//! bitmap, so a stand-in missing one would shift every button above it down a
//! place. Declaring a button is not information about anybody pressing it, and
//! the pressing is the part that stays with the shell.
//!
//! Which leaves the Steam button reaching this shell and nothing else — not
//! because an application is asked to be polite about it, but because the only
//! device an application can find never mentions it.

use std::io;
use std::path::PathBuf;

use evdev::uinput::VirtualDevice;
use evdev::{
    AbsInfo, AbsoluteAxisCode, AbsoluteAxisEvent, AttributeSet, BusType, InputEvent, InputId,
    KeyCode, KeyEvent, SynchronizationCode, SynchronizationEvent, UinputAbsSetup,
};

use crate::steam_hid::{Buttons, Report, STICK_DEADZONE, VALVE_VENDOR};

/// The button in the middle of the pad with the logo on it.
///
/// Declared below and emitted nowhere. See the module's own note, and
/// [`crate::pad_guard`], which withholds the same button from every controller
/// the kernel *does* drive.
const GUIDE: KeyCode = KeyCode::BTN_MODE;

/// Which kernel button each of the pad's buttons is.
///
/// Two of these are not what their names suggest, and it is deliberate. The
/// kernel's `BTN_NORTH` is `0x133`, which is the code `xpad` has always sent
/// for the button an Xbox controller prints **X** on — its *left* one — and
/// `BTN_WEST` is `0x134`, which is its **Y**, the top one. Every program that
/// reads controllers knows that pair the way `xpad` sends it, so sending them
/// the way their names read would put somebody's jump on the button beside the
/// one they pressed. [`crate::pads`] carries the same swap under the names
/// `XBOX_WEST` and `XBOX_NORTH`, and the tests below check the two agree.
///
/// Absent on purpose: [`GUIDE`], which is declared with the rest and sent
/// nowhere.
const KEYS: &[(Buttons, KeyCode)] = &[
    (Buttons::A, KeyCode::BTN_SOUTH),
    (Buttons::B, KeyCode::BTN_EAST),
    (Buttons::X, KeyCode::BTN_NORTH),
    (Buttons::Y, KeyCode::BTN_WEST),
    (Buttons::L1, KeyCode::BTN_TL),
    (Buttons::R1, KeyCode::BTN_TR),
    (Buttons::VIEW, KeyCode::BTN_SELECT),
    (Buttons::MENU, KeyCode::BTN_START),
    (Buttons::L3, KeyCode::BTN_THUMBL),
    (Buttons::R3, KeyCode::BTN_THUMBR),
];

/// The sticks, over the range the report itself uses.
const STICK_MIN: i32 = i16::MIN as i32;
const STICK_MAX: i32 = i16::MAX as i32;

/// How much of a stick's travel is reported as noise rather than movement.
///
/// `flat` is a *statement about the hardware*, not a setting: SDL subtracts it
/// and rescales what is left, so a program reading this pad gets a centred
/// stick when the thumb is off it and still reaches full travel. The number is
/// the shell's own dead zone, converted, so that this pad has one dead zone and
/// not two — see [`crate::steam_hid::STICK_DEADZONE`], which is where it was
/// measured.
const STICK_FLAT: i32 = (STICK_DEADZONE * i16::MAX as f32) as i32;

/// The last bit of a stick, which moves on its own and says nothing.
const STICK_FUZZ: i32 = 16;

/// The triggers, over the range the pad actually sends.
///
/// `xpad` gives a trigger one byte, and matching it would have been the tidier
/// number — but this pad spends fifteen bits on a trigger, and throwing seven
/// of them away is throwing away the part of a trigger that is not a button.
/// Every reader scales by the range the device declares, so declaring the true
/// one costs nothing. See [`crate::steam_hid::TRIGGER_FULL_SCALE`].
const TRIGGER_MAX: i32 = crate::steam_hid::TRIGGER_FULL_SCALE as i32;

/// A hat is three positions and the middle one is rest.
const HAT_MIN: i32 = -1;
const HAT_MAX: i32 = 1;

/// The stand-in for one pad.
pub struct StandIn {
    device: VirtualDevice,
    /// What the device has been told, so each report emits the difference
    /// rather than the whole pad. A freshly made `uinput` device is every key
    /// up and every axis at zero, which is what this starts as — not an
    /// assumption, but what the kernel has just been asked for.
    sent: Report,
    /// Buttons let go of by a hand-back while still held on the pad, kept off
    /// the stand-in until they come up. See [`StandIn::hand_back`].
    withheld: Buttons,
    /// The node an application will find, kept for the log line that says so.
    node: PathBuf,
    /// What it is called, which is what the pad calls itself.
    name: String,
}

impl StandIn {
    /// Make the gamepad, and wait until it is one an application can open.
    ///
    /// `name`, `product`, the bus and `version` are the pad's own, read out of
    /// sysfs by the caller, so that the device somebody finds in a controller
    /// list is called what the hardware calls itself and has its ids — the
    /// controller on a cable, over Bluetooth or through its puck, whichever it
    /// came in by.
    pub fn new(name: &str, product: u16, bluetooth: bool, version: u16) -> io::Result<Self> {
        let mut keys: AttributeSet<KeyCode> = KEYS.iter().map(|(_, key)| *key).collect();
        keys.insert(GUIDE);

        let mut device = VirtualDevice::builder()?
            .name(name)
            // The pad's own ids, on the pad's own bus. Nothing here is
            // pretending to be another controller: what makes this readable is
            // the shape of what it declares, not a name a database recognises,
            // and a stand-in claiming to be somebody else's hardware would be a
            // lie the user reads in every controller list on the machine.
            .input_id(InputId::new(
                if bluetooth {
                    BusType::BUS_BLUETOOTH
                } else {
                    BusType::BUS_USB
                },
                VALVE_VENDOR,
                product,
                version,
            ))
            .with_keys(&keys)?;
        for axis in axes() {
            device = device.with_absolute_axis(&axis)?;
        }
        let mut device = device.build()?;

        let node = crate::pad_guard::wait_for_node(&mut device)?;
        Ok(Self {
            device,
            sent: Report::default(),
            withheld: Buttons::empty(),
            node,
            name: name.to_string(),
        })
    }

    /// Where an application will find this pad.
    pub fn node(&self) -> &std::path::Path {
        &self.node
    }

    /// What it is called in a controller list.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Say what the pad has just said, less the one button.
    ///
    /// Nothing is written when nothing moved. A report arrives every four
    /// milliseconds whether or not a thumb is on anything, and a device that
    /// repeated each one would wake every program reading it two hundred and
    /// fifty times a second to tell them nothing had happened.
    pub fn send(&mut self, report: Report) -> io::Result<()> {
        let report = withhold(report, &mut self.withheld);
        let events = changes(&self.sent, &report);
        if events.is_empty() {
            return Ok(());
        }
        self.sent = report;
        self.device.emit(&events)
    }

    /// The shell is letting go of the pad, which is at `report`: let go of
    /// every button held on it, and keep each off the stand-in until it comes
    /// up. See [`crate::steam_hid::SteamPad::hand_back`].
    ///
    /// Given the pad's own report rather than read off [`StandIn::sent`],
    /// which is what applications were told and so is missing whatever an
    /// earlier hand-back is still keeping back.
    pub fn hand_back(&mut self, report: Report) -> io::Result<()> {
        self.withheld = self.withheld.union(report.buttons);
        self.send(report)
    }
}

/// The pad as applications are to be told it is: every button still kept back
/// since a hand-back taken out, and each one that has come up no longer kept
/// back — so its release is never said, the stand-in having let go of it
/// already, and its next press is an ordinary press.
fn withhold(mut report: Report, withheld: &mut Buttons) -> Report {
    *withheld = withheld.intersection(report.buttons);
    report.buttons = report.buttons.without(*withheld);
    report
}

/// Every axis the stand-in has, as the kernel is told about it.
fn axes() -> Vec<UinputAbsSetup> {
    let stick = AbsInfo::new(0, STICK_MIN, STICK_MAX, STICK_FUZZ, STICK_FLAT, 0);
    let trigger = AbsInfo::new(0, 0, TRIGGER_MAX, 0, 0, 0);
    let hat = AbsInfo::new(0, HAT_MIN, HAT_MAX, 0, 0, 0);
    [
        (AbsoluteAxisCode::ABS_X, stick),
        (AbsoluteAxisCode::ABS_Y, stick),
        (AbsoluteAxisCode::ABS_RX, stick),
        (AbsoluteAxisCode::ABS_RY, stick),
        (AbsoluteAxisCode::ABS_Z, trigger),
        (AbsoluteAxisCode::ABS_RZ, trigger),
        (AbsoluteAxisCode::ABS_HAT0X, hat),
        (AbsoluteAxisCode::ABS_HAT0Y, hat),
    ]
    .into_iter()
    .map(|(code, info)| UinputAbsSetup::new(code, info))
    .collect()
}

/// What the stand-in has to say to go from one report to the next.
///
/// Empty when the two are the same, which is most of them. The whole
/// translation from the pad's report to a controller lives here, and it is a
/// pure function of two reports so that every part of it — the two swapped
/// face buttons, the vertical axes the kernel counts the other way, the D-pad
/// that becomes a hat, and the button that is never mentioned — can be checked
/// without a pad or a `uinput` device to hand.
fn changes(before: &Report, after: &Report) -> Vec<InputEvent> {
    let mut events: Vec<InputEvent> = Vec::new();

    for (button, key) in KEYS {
        let was = before.buttons.has(*button);
        let now = after.buttons.has(*button);
        if was != now {
            events.push(KeyEvent::new(*key, i32::from(now)).into());
        }
    }
    // And `Buttons::STEAM` is not in that table, so there is no line here that
    // could send it. See [`GUIDE`].

    let mut axis = |code: AbsoluteAxisCode, was: i32, now: i32| {
        if was != now {
            events.push(AbsoluteAxisEvent::new(code, now).into());
        }
    };
    axis(
        AbsoluteAxisCode::ABS_X,
        before.left.0.into(),
        after.left.0.into(),
    );
    axis(
        AbsoluteAxisCode::ABS_Y,
        downwards(before.left.1),
        downwards(after.left.1),
    );
    axis(
        AbsoluteAxisCode::ABS_RX,
        before.right.0.into(),
        after.right.0.into(),
    );
    axis(
        AbsoluteAxisCode::ABS_RY,
        downwards(before.right.1),
        downwards(after.right.1),
    );
    axis(
        AbsoluteAxisCode::ABS_Z,
        before.triggers.0.into(),
        after.triggers.0.into(),
    );
    axis(
        AbsoluteAxisCode::ABS_RZ,
        before.triggers.1.into(),
        after.triggers.1.into(),
    );

    let (was_x, was_y) = hat(before.buttons);
    let (now_x, now_y) = hat(after.buttons);
    axis(AbsoluteAxisCode::ABS_HAT0X, was_x, now_x);
    axis(AbsoluteAxisCode::ABS_HAT0Y, was_y, now_y);

    if !events.is_empty() {
        // One packet, ended the way the kernel ends one: everything above
        // happened at the same moment, and a reader is entitled to be told so
        // rather than to see the pad move one axis at a time.
        events.push(SynchronizationEvent::new(SynchronizationCode::SYN_REPORT, 0).into());
    }
    events
}

/// One vertical axis, the way the kernel counts them.
///
/// The report counts up and every Linux gamepad driver counts down — `xpad`
/// negates the same two axes for the same reason — so this is where the shell's
/// positive-up convention stops and the kernel's begins. Clamped because
/// negating the bottom of the range would put it one past the top.
fn downwards(value: i16) -> i32 {
    (-i32::from(value)).clamp(STICK_MIN, STICK_MAX)
}

/// The D-pad, as the hat an Xbox controller reports.
///
/// Opposite directions cancel rather than one winning: a D-pad that says left
/// and right at once has been pressed in a way it cannot mean, and the middle
/// is the only honest answer. Vertical counts down, as [`downwards`] does and
/// for the same reason.
fn hat(buttons: Buttons) -> (i32, i32) {
    let along =
        |less: Buttons, more: Buttons| i32::from(buttons.has(more)) - i32::from(buttons.has(less));
    (
        along(Buttons::LEFT, Buttons::RIGHT),
        along(Buttons::UP, Buttons::DOWN),
    )
}

/// The Steam Deck's gamepad, as the kernel would have made it.
///
/// The Deck, unlike the puck, *has* a kernel gamepad: `hid-steam` makes one
/// called "Steam Deck". But it takes it away the moment anything opens the
/// Deck's raw node — which is what Steam does, and what this shell now does to
/// read the Deck at all (see [`crate::steam_hid`]) — and while it is there it
/// says nothing until somebody holds ☰ for half a second to leave the
/// firmware's keyboard-and-mouse mode. So applications are given this in its
/// place.
///
/// And it is that device, to the code: its name, its ids, its buttons, its
/// axes, their ranges and resolutions, and the same report read the same way —
/// copied from `hid-steam`'s `steam_input_register` and
/// `steam_do_deck_input_event`. That is the whole reason it works. SDL, GilRs
/// and RetroArch all carry a mapping for the kernel's "Steam Deck", keyed on
/// exactly these ids, and a device wearing those ids in the shape of an Xbox
/// pad — which is what [`StandIn`] is for the puck — would be read through that
/// mapping with every button in the wrong place. Wearing the ids *and* the
/// shape is not pretending to be another controller: it is this controller,
/// described the way its own driver describes it.
///
/// One difference, the usual one: `BTN_MODE` is declared and never sent, so
/// the Steam button reaches this shell and nothing else. And the motion
/// sensors, which the kernel puts on a second device, are not made at all.
pub struct DeckStandIn {
    device: VirtualDevice,
    /// What the device has been told, as the keys down and the axes' values.
    sent: DeckState,
    /// Keys let go of by a hand-back while still held, kept off the device
    /// until they come up. See [`StandIn::hand_back`], whose rule this is.
    withheld: u32,
    node: PathBuf,
}

/// The kernel's own name for the device.
pub const DECK_NAME: &str = "Steam Deck";

/// Every key `hid-steam` declares for the Deck but the one never sent, with
/// where the report carries it: `(byte, mask, key)`, straight from its
/// `steam_deck_button_mappings`. The kernel's `BTN_A`, `BTN_B`, `BTN_X` and
/// `BTN_Y` are `BTN_SOUTH`, `BTN_EAST`, `BTN_NORTH` and `BTN_WEST` — the same
/// swap [`KEYS`] explains, sent here the way the kernel sends it.
const DECK_KEYS: &[(usize, u8, KeyCode)] = &[
    (8, 0x01, KeyCode::BTN_TR2),
    (8, 0x02, KeyCode::BTN_TL2),
    (8, 0x04, KeyCode::BTN_TR),
    (8, 0x08, KeyCode::BTN_TL),
    (8, 0x10, KeyCode::BTN_WEST),
    (8, 0x20, KeyCode::BTN_EAST),
    (8, 0x40, KeyCode::BTN_NORTH),
    (8, 0x80, KeyCode::BTN_SOUTH),
    (9, 0x10, KeyCode::BTN_SELECT),
    (9, 0x40, KeyCode::BTN_START),
    (9, 0x80, BTN_GRIPL2),
    (10, 0x01, BTN_GRIPR2),
    (10, 0x40, KeyCode::BTN_THUMBL),
    (11, 0x04, KeyCode::BTN_THUMBR),
    (9, 0x01, KeyCode::BTN_DPAD_UP),
    (9, 0x02, KeyCode::BTN_DPAD_RIGHT),
    (9, 0x04, KeyCode::BTN_DPAD_LEFT),
    (9, 0x08, KeyCode::BTN_DPAD_DOWN),
    (10, 0x02, KeyCode::BTN_THUMB),
    (10, 0x04, KeyCode::BTN_THUMB2),
    (13, 0x02, BTN_GRIPL),
    (13, 0x04, BTN_GRIPR),
    (14, 0x04, KeyCode::BTN_BASE),
];

/// The back grips, which `evdev` has no names for yet: the kernel's
/// `input-event-codes.h` since the Deck's driver learned them.
const BTN_GRIPL: KeyCode = KeyCode::new(0x224);
const BTN_GRIPR: KeyCode = KeyCode::new(0x225);
const BTN_GRIPL2: KeyCode = KeyCode::new(0x226);
const BTN_GRIPR2: KeyCode = KeyCode::new(0x227);

/// Where the report says the trackpads are touched, and so whether their
/// positions mean anything: byte 10, left and right.
const DECK_LEFT_PAD_TOUCHED: (usize, u8) = (10, 0x08);
const DECK_RIGHT_PAD_TOUCHED: (usize, u8) = (10, 0x10);

/// Every axis, as `hid-steam` declares it for the Deck — range, fuzz and
/// resolution — with the report offset it reads it from and the sign it gives
/// it. The trackpads are read only while touched, and are zero otherwise.
const DECK_AXES: &[DeckAxis] = &[
    DeckAxis::stick(AbsoluteAxisCode::ABS_X, 48, 1),
    DeckAxis::stick(AbsoluteAxisCode::ABS_Y, 50, -1),
    DeckAxis::stick(AbsoluteAxisCode::ABS_RX, 52, 1),
    DeckAxis::stick(AbsoluteAxisCode::ABS_RY, 54, -1),
    DeckAxis::pad(AbsoluteAxisCode::ABS_HAT0X, 16, DECK_LEFT_PAD_TOUCHED),
    DeckAxis::pad(AbsoluteAxisCode::ABS_HAT0Y, 18, DECK_LEFT_PAD_TOUCHED),
    DeckAxis::pad(AbsoluteAxisCode::ABS_HAT1X, 20, DECK_RIGHT_PAD_TOUCHED),
    DeckAxis::pad(AbsoluteAxisCode::ABS_HAT1Y, 22, DECK_RIGHT_PAD_TOUCHED),
    DeckAxis::trigger(AbsoluteAxisCode::ABS_HAT2Y, 44),
    DeckAxis::trigger(AbsoluteAxisCode::ABS_HAT2X, 46),
];

/// One of the Deck's axes. See [`DECK_AXES`].
struct DeckAxis {
    code: AbsoluteAxisCode,
    /// Where the report carries it, as a little-endian `s16`.
    at: usize,
    sign: i32,
    /// The byte and mask that say whether it means anything, for a trackpad.
    only_while: Option<(usize, u8)>,
    min: i32,
    max: i32,
    fuzz: i32,
    resolution: i32,
}

impl DeckAxis {
    /// `STEAM_DECK_JOYSTICK_RESOLUTION`.
    const fn stick(code: AbsoluteAxisCode, at: usize, sign: i32) -> Self {
        Self {
            code,
            at,
            sign,
            only_while: None,
            min: -32767,
            max: 32767,
            fuzz: 0,
            resolution: 6553,
        }
    }

    /// `STEAM_PAD_FUZZ` and `STEAM_PAD_RESOLUTION`.
    const fn pad(code: AbsoluteAxisCode, at: usize, touched: (usize, u8)) -> Self {
        Self {
            code,
            at,
            sign: 1,
            only_while: Some(touched),
            min: -32767,
            max: 32767,
            fuzz: 256,
            resolution: 1638,
        }
    }

    /// `STEAM_DECK_TRIGGER_RESOLUTION`.
    const fn trigger(code: AbsoluteAxisCode, at: usize) -> Self {
        Self {
            code,
            at,
            sign: 1,
            only_while: None,
            min: 0,
            max: 32767,
            fuzz: 0,
            resolution: 5461,
        }
    }

    /// Its value in one report, as the kernel would report it.
    fn read(&self, report: &[u8]) -> i32 {
        if let Some((byte, mask)) = self.only_while {
            if report[byte] & mask == 0 {
                return 0;
            }
        }
        let raw = i16::from_le_bytes([report[self.at], report[self.at + 1]]);
        (self.sign * i32::from(raw)).clamp(self.min, self.max)
    }
}

/// The Deck's gamepad as the device has been told it is: which of
/// [`DECK_KEYS`] are down, as bits in that order, and each of [`DECK_AXES`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct DeckState {
    keys: u32,
    axes: [i32; 10],
}

const _: () = assert!(DECK_KEYS.len() <= 32 && DECK_AXES.len() == 10);

impl DeckState {
    /// One raw Deck report, read the way `steam_do_deck_input_event` reads it.
    /// The caller has already checked it is one.
    fn of(report: &[u8]) -> Self {
        let mut keys = 0u32;
        for (index, (byte, mask, _)) in DECK_KEYS.iter().enumerate() {
            if report[*byte] & mask != 0 {
                keys |= 1 << index;
            }
        }
        let mut axes = [0i32; 10];
        for (value, axis) in axes.iter_mut().zip(DECK_AXES) {
            *value = axis.read(report);
        }
        Self { keys, axes }
    }
}

impl DeckStandIn {
    /// Make the gamepad, and wait until it is one an application can open.
    pub fn new() -> io::Result<Self> {
        let mut keys: AttributeSet<KeyCode> = DECK_KEYS.iter().map(|(_, _, key)| *key).collect();
        keys.insert(GUIDE);
        let mut device = VirtualDevice::builder()?
            .name(DECK_NAME)
            .input_id(InputId::new(
                BusType::BUS_USB,
                crate::steam_hid::VALVE_VENDOR,
                crate::steam_hid::DECK_PRODUCT,
                crate::steam_hid::firmware_version(crate::steam_hid::Pad::Deck).unwrap_or(0),
            ))
            .with_keys(&keys)?;
        for axis in DECK_AXES {
            let info = AbsInfo::new(0, axis.min, axis.max, axis.fuzz, 0, axis.resolution);
            device = device.with_absolute_axis(&UinputAbsSetup::new(axis.code, info))?;
        }
        let mut device = device.build()?;
        let node = crate::pad_guard::wait_for_node(&mut device)?;
        Ok(Self {
            device,
            sent: DeckState::default(),
            withheld: 0,
            node,
        })
    }

    /// Where an application will find this pad.
    pub fn node(&self) -> &std::path::Path {
        &self.node
    }

    /// What it is called in a controller list: the kernel's name for it.
    pub fn name(&self) -> &str {
        DECK_NAME
    }

    /// Say what the Deck has just said, less the one button. `report` is a
    /// Deck input report, already checked to be one.
    pub fn send(&mut self, report: &[u8]) -> io::Result<()> {
        let mut state = DeckState::of(report);
        self.withheld &= state.keys;
        state.keys &= !self.withheld;
        let events = deck_changes(&self.sent, &state);
        if events.is_empty() {
            return Ok(());
        }
        self.sent = state;
        self.device.emit(&events)
    }

    /// The shell is letting go of the pad, which is at `report`. See
    /// [`StandIn::hand_back`].
    pub fn hand_back(&mut self, report: &[u8]) -> io::Result<()> {
        self.withheld |= DeckState::of(report).keys;
        self.send(report)
    }
}

/// What the Deck's gamepad has to say to go from one state to the next, in one
/// packet. Empty when nothing changed.
fn deck_changes(before: &DeckState, after: &DeckState) -> Vec<InputEvent> {
    let mut events: Vec<InputEvent> = Vec::new();
    for (index, (_, _, key)) in DECK_KEYS.iter().enumerate() {
        let was = before.keys & (1 << index) != 0;
        let now = after.keys & (1 << index) != 0;
        if was != now {
            events.push(KeyEvent::new(*key, i32::from(now)).into());
        }
    }
    for ((axis, was), now) in DECK_AXES.iter().zip(before.axes).zip(after.axes) {
        if was != now {
            events.push(AbsoluteAxisEvent::new(axis.code, now).into());
        }
    }
    if !events.is_empty() {
        events.push(SynchronizationEvent::new(SynchronizationCode::SYN_REPORT, 0).into());
    }
    events
}

#[cfg(test)]
mod deck_tests {
    use super::*;

    /// A Deck report with only `(byte, mask)` set.
    fn report(set: &[(usize, u8)]) -> [u8; 64] {
        let mut report = [0u8; 64];
        report[..3].copy_from_slice(&[0x01, 0x00, 0x09]);
        for (byte, mask) in set {
            report[*byte] |= mask;
        }
        report
    }

    fn keys(events: &[InputEvent]) -> Vec<(u16, i32)> {
        events
            .iter()
            .filter(|event| event.event_type() == evdev::EventType::KEY)
            .map(|event| (event.code(), event.value()))
            .collect()
    }

    fn axis(events: &[InputEvent], code: AbsoluteAxisCode) -> Option<i32> {
        events
            .iter()
            .find(|event| {
                event.event_type() == evdev::EventType::ABSOLUTE && event.code() == code.0
            })
            .map(|event| event.value())
    }

    /// The Steam button is in the Deck's report and never reaches the
    /// stand-in: it stays with the shell, as every other pad's does.
    #[test]
    fn the_steam_button_is_never_sent() {
        let pressed = DeckState::of(&report(&[(9, 0x20)]));
        assert!(deck_changes(&DeckState::default(), &pressed).is_empty());
        assert!(!DECK_KEYS.iter().any(|(_, _, key)| *key == GUIDE));
    }

    /// The face buttons as the kernel sends them: A is `BTN_SOUTH`, B
    /// `BTN_EAST`, X `BTN_NORTH` and Y `BTN_WEST`, the `xpad` pair for X and Y.
    #[test]
    fn the_face_buttons_are_the_kernels_codes() {
        for (bit, key) in [
            ((8, 0x80), KeyCode::BTN_SOUTH),
            ((8, 0x20), KeyCode::BTN_EAST),
            ((8, 0x40), KeyCode::BTN_NORTH),
            ((8, 0x10), KeyCode::BTN_WEST),
        ] {
            let events = deck_changes(&DeckState::default(), &DeckState::of(&report(&[bit])));
            assert_eq!(keys(&events), vec![(key.0, 1)], "{bit:?}");
        }
    }

    /// Every key the kernel declares for the Deck is declared here, the one
    /// that is never sent among them — SDL numbers a pad's buttons by walking
    /// the whole set, so one missing would put every button above it one place
    /// down in a game.
    #[test]
    fn every_key_the_kernel_declares_is_declared() {
        let mut declared: Vec<u16> = DECK_KEYS.iter().map(|(_, _, key)| key.0).collect();
        declared.push(GUIDE.0);
        declared.sort_unstable();
        let mut kernel: Vec<u16> = vec![
            0x139, 0x138, 0x137, 0x136, 0x134, 0x131, 0x133, 0x130, // TR2 TL2 TR TL Y B X A
            0x220, 0x223, 0x222, 0x221, // D-pad
            0x13a, 0x13c, 0x13b, // SELECT MODE START
            0x13e, 0x13d, 0x121, 0x122, // THUMBR THUMBL THUMB THUMB2
            0x224, 0x225, 0x126, 0x226, 0x227, // GRIPL GRIPR BASE GRIPL2 GRIPR2
        ];
        kernel.sort_unstable();
        assert_eq!(declared, kernel);
    }

    /// The sticks' vertical axes counted down, as the kernel counts them, and
    /// the triggers on the two hats `hid-steam` puts them on.
    #[test]
    fn the_axes_are_the_kernels() {
        let mut raw = report(&[]);
        raw[48..50].copy_from_slice(&1000i16.to_le_bytes());
        raw[50..52].copy_from_slice(&2000i16.to_le_bytes());
        raw[54..56].copy_from_slice(&(-3000i16).to_le_bytes());
        raw[44..46].copy_from_slice(&0x1234u16.to_le_bytes());
        raw[46..48].copy_from_slice(&0x7fffu16.to_le_bytes());
        let events = deck_changes(&DeckState::default(), &DeckState::of(&raw));
        assert_eq!(axis(&events, AbsoluteAxisCode::ABS_X), Some(1000));
        assert_eq!(axis(&events, AbsoluteAxisCode::ABS_Y), Some(-2000));
        assert_eq!(axis(&events, AbsoluteAxisCode::ABS_RY), Some(3000));
        assert_eq!(axis(&events, AbsoluteAxisCode::ABS_HAT2Y), Some(0x1234));
        assert_eq!(axis(&events, AbsoluteAxisCode::ABS_HAT2X), Some(0x7fff));
    }

    /// A trackpad's position means something only while a finger is on it.
    #[test]
    fn a_trackpad_reads_only_while_it_is_touched() {
        let mut raw = report(&[]);
        raw[16..18].copy_from_slice(&500i16.to_le_bytes());
        let untouched = DeckState::of(&raw);
        assert_eq!(
            axis(
                &deck_changes(&DeckState::default(), &untouched),
                AbsoluteAxisCode::ABS_HAT0X
            ),
            None
        );
        raw[10] |= 0x08;
        let touched = DeckState::of(&raw);
        assert_eq!(
            axis(
                &deck_changes(&DeckState::default(), &touched),
                AbsoluteAxisCode::ABS_HAT0X
            ),
            Some(500)
        );
    }

    /// Nothing moved, nothing said.
    #[test]
    fn a_report_that_changes_nothing_says_nothing() {
        let state = DeckState::of(&report(&[(8, 0x80)]));
        assert!(deck_changes(&state, &state).is_empty());
    }

    /// Made for real, and read back the way an application reads it: the
    /// kernel's name and ids, the kernel's axis ranges, and a press of A and of
    /// the Steam button at once coming out as A alone.
    #[test]
    fn the_kernel_sees_the_decks_own_gamepad_less_the_steam_button() {
        use std::time::{Duration, Instant};
        if std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/uinput")
            .is_err()
        {
            crate::skipped("/dev/uinput cannot be opened here");
            return;
        }
        let _serial = crate::steam_hid::serial();
        let mut stand_in = DeckStandIn::new().expect("the stand-in was not made");
        let mut reading =
            evdev::Device::open(stand_in.node()).expect("its node could not be opened");
        reading.set_nonblocking(true).expect("non-blocking");

        assert_eq!(reading.name(), Some(DECK_NAME));
        let id = reading.input_id();
        assert_eq!(
            (id.vendor(), id.product()),
            (
                crate::steam_hid::VALVE_VENDOR,
                crate::steam_hid::DECK_PRODUCT
            )
        );
        let absinfo: Vec<(u16, i32, i32, i32)> = reading
            .get_absinfo()
            .expect("its axes could not be read")
            .map(|(code, info)| (code.0, info.minimum(), info.maximum(), info.resolution()))
            .collect();
        assert!(absinfo.contains(&(AbsoluteAxisCode::ABS_X.0, -32767, 32767, 6553)));
        assert!(absinfo.contains(&(AbsoluteAxisCode::ABS_HAT2Y.0, 0, 32767, 5461)));
        assert!(absinfo.contains(&(AbsoluteAxisCode::ABS_HAT1X.0, -32767, 32767, 1638)));

        stand_in
            .send(&report(&[(8, 0x80), (9, 0x20)]))
            .expect("the stand-in would not speak");
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut pressed = Vec::new();
        while Instant::now() < deadline && pressed.is_empty() {
            if let Ok(events) = reading.fetch_events() {
                pressed.extend(
                    events
                        .filter(|event| {
                            event.event_type() == evdev::EventType::KEY && event.value() == 1
                        })
                        .map(|event| event.code()),
                );
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            pressed,
            vec![KeyCode::BTN_SOUTH.0],
            "A, and not the Steam button"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::steam_hid::PUCK_PRODUCT;

    fn pressed(buttons: Buttons) -> Report {
        Report {
            buttons,
            ..Report::default()
        }
    }

    /// The whole point of the module, in one test.
    ///
    /// A report with every button on the pad down — the Steam button among
    /// them — and nothing the stand-in says mentions it.
    #[test]
    fn the_steam_button_is_never_written() {
        let every = KEYS
            .iter()
            .fold(Buttons::STEAM, |all, (button, _)| all.union(*button));
        let events = changes(&Report::default(), &pressed(every));

        assert!(!events.is_empty(), "the rest of the pad still arrives");
        for event in &events {
            assert!(
                !(event.event_type() == evdev::EventType::KEY && event.code() == GUIDE.0),
                "the guide button reached an application"
            );
        }

        // And letting go of it says nothing either, which is the half a naive
        // filter forgets: a release is as good as a press for noticing that a
        // button exists.
        assert!(changes(&pressed(Buttons::STEAM), &Report::default()).is_empty());
        assert!(changes(&Report::default(), &pressed(Buttons::STEAM)).is_empty());
    }

    /// The press that closed the guide, still held when the game got the keys:
    /// the stand-in says `A` is up for as long as the thumb stays on it, never
    /// says the release, and says the next press. A D-pad direction held with
    /// it goes the same way, which is its hat coming back to the middle.
    #[test]
    fn a_button_held_when_the_pad_is_handed_back_is_up_until_it_comes_up() {
        let held = pressed(Buttons::A.union(Buttons::LEFT));
        let mut withheld = held.buttons;

        let shown = withhold(held, &mut withheld);
        assert_eq!(shown.buttons, Buttons::empty());
        let events = changes(&held, &shown);
        let said: Vec<(u16, u16, i32)> = events
            .iter()
            .map(|event| (event.event_type().0, event.code(), event.value()))
            .collect();
        assert!(said.contains(&(evdev::EventType::KEY.0, KeyCode::BTN_SOUTH.0, 0)));
        assert!(said.contains(&(
            evdev::EventType::ABSOLUTE.0,
            AbsoluteAxisCode::ABS_HAT0X.0,
            0
        )));

        // Still held, and B pressed beside it: only B.
        let shown = withhold(pressed(Buttons::A.union(Buttons::B)), &mut withheld);
        assert_eq!(shown.buttons, Buttons::B);
        assert_eq!(withheld, Buttons::A, "LEFT came up, and is ordinary again");

        // A comes up, which is nothing to say; then down, which is a press.
        let shown = withhold(Report::default(), &mut withheld);
        assert_eq!(shown.buttons, Buttons::empty());
        assert!(withheld.is_empty());
        let shown = withhold(pressed(Buttons::A), &mut withheld);
        assert_eq!(shown.buttons, Buttons::A);
    }

    /// Declared, though. See the module note: a stand-in missing a button
    /// renumbers every button above it, and SDL's mapping counts places.
    #[test]
    fn the_guide_button_is_still_declared() {
        let pad = declared();
        assert!(
            pad.has(crate::pads::At::Key(GUIDE.0)),
            "an application must still be able to see that this pad has one"
        );
    }

    /// What the kernel would say about the stand-in, built from what it is
    /// declared with, so the downstream reader can be asked about it directly.
    fn declared() -> crate::pads::Pad {
        let mut keys: Vec<u16> = KEYS.iter().map(|(_, key)| key.0).collect();
        keys.push(GUIDE.0);
        keys.sort_unstable();
        let mut axes: Vec<u16> = axes().iter().map(|axis| axis.code()).collect();
        axes.sort_unstable();
        crate::pads::Pad {
            node: PathBuf::from("/dev/input/event0"),
            sysfs: "/devices/virtual/input/input0".into(),
            name: "Valve Software Steam Controller Puck".into(),
            vendor: VALVE_VENDOR,
            product: PUCK_PRODUCT,
            keys,
            axes,
        }
    }

    /// The reason the shape is the shape: an emulator works out where the
    /// controls are from what the device declares, and this is that same
    /// question asked of this device.
    ///
    /// It is also the test that catches the swapped pair, and catches it from
    /// the *other* side — [`crate::pads::xinput`] has its own idea of which
    /// code is the left-hand face button, and if the two ever disagree this
    /// fails rather than somebody's jump moving one button over.
    #[test]
    fn an_application_finds_a_controller_of_xboxs_shape() {
        use crate::pads::{At, Control};

        let pad = declared();
        let mapping = crate::pads::xinput(&pad)
            .expect("the stand-in was not recognised as a controller of Xbox's shape");

        // The left-hand face button, which is where the pad's `X` is.
        let x = KEYS
            .iter()
            .find(|(button, _)| *button == Buttons::X)
            .map(|(_, key)| key.0)
            .expect("X is mapped");
        assert_eq!(mapping.at(Control::West), Some(At::Key(x)));

        // And the top one, where `Y` is.
        let y = KEYS
            .iter()
            .find(|(button, _)| *button == Buttons::Y)
            .map(|(_, key)| key.0)
            .expect("Y is mapped");
        assert_eq!(mapping.at(Control::North), Some(At::Key(y)));

        assert_eq!(
            mapping.at(Control::South),
            Some(At::Key(KeyCode::BTN_SOUTH.0))
        );
        assert_eq!(
            mapping.at(Control::East),
            Some(At::Key(KeyCode::BTN_EAST.0))
        );
        assert_eq!(
            mapping.at(Control::LeftTrigger),
            Some(At::Axis(AbsoluteAxisCode::ABS_Z.0))
        );
        assert_eq!(
            mapping.at(Control::DPadY),
            Some(At::Axis(AbsoluteAxisCode::ABS_HAT0Y.0))
        );
    }

    /// The two codes whose names read backwards, written out so that a tidying
    /// edit to [`KEYS`] has to break a test that says what it is for.
    #[test]
    fn the_two_middle_face_buttons_are_sent_the_way_xpad_sends_them() {
        assert_eq!(KeyCode::BTN_NORTH.0, 0x133, "the code xpad sends for X");
        assert_eq!(KeyCode::BTN_WEST.0, 0x134, "the code xpad sends for Y");

        let x = changes(&Report::default(), &pressed(Buttons::X));
        assert!(x
            .iter()
            .any(|event| event.code() == 0x133 && event.value() == 1));
        let y = changes(&Report::default(), &pressed(Buttons::Y));
        assert!(y
            .iter()
            .any(|event| event.code() == 0x134 && event.value() == 1));
    }

    /// A pad at rest that keeps talking says nothing at all.
    #[test]
    fn an_unchanged_report_is_not_repeated() {
        let resting = Report::default();
        assert!(changes(&resting, &resting).is_empty());

        let holding = pressed(Buttons::A);
        assert!(changes(&holding, &holding).is_empty(), "a held button");
    }

    /// One control moving is one control moving, and a packet to say so.
    #[test]
    fn one_change_is_one_event_and_a_sync() {
        let events = changes(&Report::default(), &pressed(Buttons::A));
        assert_eq!(events.len(), 2, "the press, and the packet's end");
        assert_eq!(events[0].code(), KeyCode::BTN_SOUTH.0);
        assert_eq!(events[0].value(), 1);
        assert_eq!(
            events[1].event_type(),
            evdev::EventType::SYNCHRONIZATION,
            "a reader is told the two arrived together"
        );
    }

    /// The shell's convention is positive-up and the kernel's is positive-down.
    /// Getting this wrong is a game where pushing forward walks backwards.
    #[test]
    fn the_vertical_axes_are_turned_over() {
        let up = Report {
            left: (0, 20_000),
            right: (0, -20_000),
            ..Report::default()
        };
        let events = changes(&Report::default(), &up);
        let value = |code: AbsoluteAxisCode| {
            events
                .iter()
                .find(|event| {
                    event.event_type() == evdev::EventType::ABSOLUTE && event.code() == code.0
                })
                .map(|event| event.value())
        };
        assert_eq!(
            value(AbsoluteAxisCode::ABS_Y),
            Some(-20_000),
            "up is negative"
        );
        assert_eq!(
            value(AbsoluteAxisCode::ABS_RY),
            Some(20_000),
            "down is positive"
        );

        // The across axes are not touched, and are not reported for having not
        // been touched.
        assert_eq!(value(AbsoluteAxisCode::ABS_X), None);
    }

    /// Negating the bottom of the range would put it one past the top, which
    /// is a value outside what the device said it could send.
    #[test]
    fn a_stick_pushed_all_the_way_stays_inside_the_range_it_declared() {
        assert_eq!(downwards(i16::MIN), STICK_MAX);
        assert_eq!(downwards(i16::MAX), STICK_MIN + 1);
        for setup in axes() {
            let info = setup.absinfo();
            assert!(
                downwards(i16::MIN) <= info.maximum().max(STICK_MAX),
                "inside the declared range"
            );
        }
    }

    /// The D-pad an Xbox pad reports as a hat, in all four directions and in
    /// the one position a D-pad cannot really be in.
    #[test]
    fn the_dpad_becomes_a_hat() {
        assert_eq!(hat(Buttons::empty()), (0, 0));
        assert_eq!(hat(Buttons::LEFT), (-1, 0));
        assert_eq!(hat(Buttons::RIGHT), (1, 0));
        assert_eq!(hat(Buttons::UP), (0, -1), "up is negative, as everywhere");
        assert_eq!(hat(Buttons::DOWN), (0, 1));
        assert_eq!(
            hat(Buttons::UP.union(Buttons::DOWN)),
            (0, 0),
            "both at once is neither"
        );
        // And a diagonal is both axes, which is the case a naive match arm
        // would drop.
        assert_eq!(hat(Buttons::UP.union(Buttons::RIGHT)), (1, -1));
    }

    /// The D-pad is a hat and *not* four buttons. A stand-in that sent both
    /// would walk a menu two rows per press in anything reading it.
    #[test]
    fn the_dpad_is_not_also_sent_as_buttons() {
        for direction in [Buttons::UP, Buttons::DOWN, Buttons::LEFT, Buttons::RIGHT] {
            assert!(
                !KEYS.iter().any(|(button, _)| *button == direction),
                "a direction was given a button code as well"
            );
        }
        let events = changes(&Report::default(), &pressed(Buttons::UP));
        assert!(events
            .iter()
            .all(|event| event.event_type() != evdev::EventType::KEY));
    }

    #[test]
    fn the_triggers_carry_the_whole_travel_the_pad_sends() {
        let pulled = Report {
            triggers: (crate::steam_hid::TRIGGER_FULL_SCALE, 0x4000),
            ..Report::default()
        };
        let events = changes(&Report::default(), &pulled);
        let value = |code: AbsoluteAxisCode| {
            events
                .iter()
                .find(|event| {
                    event.event_type() == evdev::EventType::ABSOLUTE && event.code() == code.0
                })
                .map(|event| event.value())
        };
        assert_eq!(
            value(AbsoluteAxisCode::ABS_Z),
            Some(crate::steam_hid::TRIGGER_FULL_SCALE.into())
        );
        assert_eq!(value(AbsoluteAxisCode::ABS_RZ), Some(0x4000));

        // And the range they were declared over reaches both ends of that.
        let declared = axes()
            .into_iter()
            .find(|axis| axis.code() == AbsoluteAxisCode::ABS_Z.0)
            .expect("the left trigger is declared");
        assert_eq!(declared.absinfo().minimum(), 0);
        assert_eq!(declared.absinfo().maximum(), TRIGGER_MAX);
    }

    /// The dead zone the stand-in declares is the one the shell measured, and
    /// it has to be big enough for the drift that was measured with it.
    #[test]
    fn the_declared_dead_zone_is_the_shells_own() {
        assert_eq!(STICK_FLAT, (STICK_DEADZONE * i16::MAX as f32) as i32);
        // Two per cent of travel, which is what a resting stick on this
        // hardware was measured at.
        assert!(STICK_FLAT > (0.02 * i16::MAX as f32) as i32);
        // And not so much of the stick that a game cannot aim with it.
        assert!(STICK_FLAT < i16::MAX as i32 / 8);
    }

    /// Nothing in the shape is a controller of somebody else's: Sony's driver
    /// reports its triggers as an axis *and* a button, and a stand-in that
    /// declared those would be read as a PlayStation pad with its two middle
    /// face buttons the wrong way round. See [`crate::pads::xinput`].
    #[test]
    fn the_shoulder_buttons_under_the_triggers_are_not_declared() {
        for key in [KeyCode::BTN_TL2, KeyCode::BTN_TR2] {
            assert!(!KEYS.iter().any(|(_, code)| *code == key));
        }
    }

    /// Every button the pad has a code for has a *different* code.
    #[test]
    fn no_two_buttons_share_a_code() {
        let mut seen: Vec<u16> = KEYS.iter().map(|(_, key)| key.0).collect();
        seen.push(GUIDE.0);
        let before = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), before, "two controls on one code");
    }

    /// The same for the axes, which are declared in a list of their own.
    #[test]
    fn no_two_axes_share_a_code() {
        let mut seen: Vec<u16> = axes().iter().map(|axis| axis.code()).collect();
        let before = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), before, "two axes on one code");
    }

    /// What the stand-in actually is, on a machine that will let us make one.
    ///
    /// Everything above is a pure function; this is the device. It is the only
    /// thing that can answer whether the kernel accepts the shape, whether the
    /// node arrives, and whether what comes back out of `/dev/input` is what
    /// went in — which is the whole claim the module makes.
    ///
    /// Skipped where `/dev/uinput` cannot be opened, which is most build
    /// machines and every container. See `pad_guard`'s own hardware tests.
    #[cfg(test)]
    mod hardware_tests {
        use super::*;
        use std::time::{Duration, Instant};

        fn uinput_works() -> bool {
            std::fs::OpenOptions::new()
                .write(true)
                .open("/dev/uinput")
                .is_ok()
        }

        /// Read the stand-in back the way an application would: by opening its
        /// node and waiting for a packet.
        fn drain(device: &mut evdev::Device, patience: Duration) -> Vec<InputEvent> {
            let deadline = Instant::now() + patience;
            let mut seen = Vec::new();
            while Instant::now() < deadline {
                match device.fetch_events() {
                    Ok(events) => seen.extend(events),
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
                    Err(err) => panic!("reading the stand-in: {err}"),
                }
                if seen
                    .iter()
                    .any(|event| event.event_type() == evdev::EventType::SYNCHRONIZATION)
                {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            seen
        }

        #[test]
        fn a_press_goes_in_and_comes_out_of_dev_input_without_the_guide_button() {
            if !uinput_works() {
                crate::skipped("/dev/uinput cannot be opened here");
                return;
            }

            let mut stand_in = StandIn::new("lxb test pad", PUCK_PRODUCT, false, 0)
                .expect("the stand-in was not made");
            let node = stand_in.node().to_path_buf();
            let mut reading = evdev::Device::open(&node).expect("its node could not be opened");
            reading
                .set_nonblocking(true)
                .expect("the node would not go non-blocking");

            // Everything the pad has, at once, guide button included.
            let every = KEYS
                .iter()
                .fold(Buttons::STEAM, |all, (button, _)| all.union(*button));
            stand_in
                .send(Report {
                    buttons: every,
                    left: (12_345, 6_789),
                    right: (-1_000, -2_000),
                    triggers: (20_000, 10_000),
                })
                .expect("the stand-in would not speak");

            let events = drain(&mut reading, Duration::from_secs(2));
            assert!(!events.is_empty(), "nothing came out of the node");

            let keys: Vec<u16> = events
                .iter()
                .filter(|event| event.event_type() == evdev::EventType::KEY && event.value() == 1)
                .map(|event| event.code())
                .collect();
            assert!(
                !keys.contains(&GUIDE.0),
                "the guide button came out of /dev/input"
            );
            assert!(keys.contains(&KeyCode::BTN_SOUTH.0), "but the rest did");

            // And the device an application enumerates has the guide button on
            // it, unpressed — which is what keeps SDL's numbering right.
            assert!(reading
                .supported_keys()
                .is_some_and(|keys| keys.contains(GUIDE)));

            // The vertical axis, turned over on its way out.
            let abs = |code: AbsoluteAxisCode| {
                events
                    .iter()
                    .find(|event| {
                        event.event_type() == evdev::EventType::ABSOLUTE && event.code() == code.0
                    })
                    .map(|event| event.value())
            };
            assert_eq!(abs(AbsoluteAxisCode::ABS_Y), Some(-6_789));
            // Both triggers, because they are two calls with one offset
            // between them and a test that checked one would not notice the
            // other going nowhere.
            assert_eq!(abs(AbsoluteAxisCode::ABS_Z), Some(20_000));
            assert_eq!(abs(AbsoluteAxisCode::ABS_RZ), Some(10_000));
        }

        /// The shape, asked of the kernel rather than of the builder: this is
        /// what [`crate::pads`] will read out of `/proc` and what an emulator
        /// will read out of `udev`.
        #[test]
        fn the_kernel_agrees_it_is_a_controller_of_xboxs_shape() {
            if !uinput_works() {
                crate::skipped("/dev/uinput cannot be opened here");
                return;
            }

            let stand_in = StandIn::new("lxb test pad", PUCK_PRODUCT, false, 0)
                .expect("the stand-in was not made");
            let device =
                evdev::Device::open(stand_in.node()).expect("its node could not be opened");

            let mut keys: Vec<u16> = device
                .supported_keys()
                .expect("it declares buttons")
                .iter()
                .map(|key| key.0)
                .collect();
            keys.sort_unstable();
            let mut axes: Vec<u16> = device
                .supported_absolute_axes()
                .expect("it declares axes")
                .iter()
                .map(|axis| axis.0)
                .collect();
            axes.sort_unstable();

            let mut pad = super::tests::declared();
            pad.keys = keys;
            pad.axes = axes;
            assert!(
                crate::pads::xinput(&pad).is_some(),
                "the device the kernel made is not the shape it was asked for"
            );
        }
    }
}
