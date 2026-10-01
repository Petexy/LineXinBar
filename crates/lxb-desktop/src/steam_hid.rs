//! A driver for the second-generation Steam Controller and the Steam Deck's
//! controls, on `hidraw`.
//!
//! ## The Deck
//!
//! The Deck's controller has a kernel driver, and it is no use to a console
//! shell. `hid-steam` leaves the Deck's firmware in its keyboard-and-mouse mode
//! — `A` is Enter, the D-pad is the arrow keys — and makes a gamepad that says
//! nothing until somebody holds ☰ for half a second. So on a Deck the shell was
//! driven by keystrokes: every button told it a keyboard was in somebody's
//! hands, and the on-screen keyboard was never offered. And the moment anything
//! opens the Deck's raw node — Steam does, whenever it runs — the kernel takes
//! its gamepad away altogether (`steam_client_ll_open`).
//!
//! That is the puck's situation, arrived at from the other side, and it has the
//! puck's answer: read the report. The Deck's comes on its raw node at 250 Hz
//! whatever mode the firmware is in and whoever else is reading, 64 bytes
//! beginning `01 00 09`, laid out as `hid-steam`'s own
//! `steam_deck_button_mappings` says. Opening the node is what makes the kernel
//! step aside, so this driver becomes the Deck's for the session, and gives
//! everything else the kernel's gamepad back in the shape the kernel made it —
//! see [`crate::steam_stand_in::DeckStandIn`]. The keystrokes are dropped by
//! the compositor, as the puck's are.
//!
//! ## The Steam Controller 2
//!
//! The 2026 Steam Controller comes in four ways — on a cable (`28de:1302`),
//! over Bluetooth (`1303`), through its puck (`1304`), and through a Steam
//! Machine's own receiver (`1305`) — and carries the same report on every one:
//! `0x42`, 54 bytes, or over Bluetooth `0x45`, the same bytes less the last
//! eight. It is the Deck's case over again on a kernel that drives it, and has
//! no kernel gamepad at all on one that does not. `hid-steam` learned it in
//! Linux 7.3 — handheld kernels carry it earlier, and CachyOS's `deckify` 7.2.3
//! claims all four — and before that claims only `1102`, `1142` and `1205`, the
//! original controller, its receiver and the Deck. There the controller falls
//! through to `hid-generic` and stays in the firmware's lizard mode: a mouse and
//! a keyboard, and no joystick node at all. Where `hid-steam` has it, it is in
//! lizard mode all the same, and its gamepad says nothing until Start is held
//! and goes away the moment anything opens the raw node, as the Deck's does.
//!
//! ## Which driver
//!
//! Decided for every raw node on every look, from what the machine says rather
//! than from a kernel version — see [`reads_the_raw_node`]:
//!
//! * **No kernel driver for it** — `hid-generic`, which is where the controller
//!   is on every kernel before 7.3, Debian's for years to come. The raw node is
//!   the only thing that answers, so this driver is the controller's.
//! * **`hid-steam`, with its `lizard_mode` on** — the module's default, and a
//!   Steam Deck's on every kernel. Its gamepad stays silent until ☰ or Start is
//!   held for half a second, a hold it ignores while anything reads the raw
//!   node, so a shell that left the controller to it would leave somebody
//!   holding a pad that does nothing. The raw node it is.
//! * **`hid-steam`, with `lizard_mode` off** — set by whoever set the machine
//!   up. Its gamepad answers from the first press, with the rumble and motion a
//!   stand-in does not have, so the raw node is left alone and the kernel's
//!   gamepad is read like any other controller, its Steam button taken by
//!   [`crate::pad_guard`]. Until another program — Steam, as a rule — opens the
//!   raw node, which takes that gamepad away from everybody: then this driver
//!   reads the raw node too, and lets go once it is the only one left holding
//!   it, so the kernel can make its gamepad again.
//!
//! For a while only the Steam button was read here, because lizard mode is a
//! *real* USB keyboard and every other button reached the shell as a keystroke
//! through the compositor. That stopped being true the moment Steam was
//! launched: Steam claims the pad and writes lizard mode off, the keystrokes
//! stop, and the shell is left with one working button on a dead controller.
//!
//! So the whole report is decoded. It is the only source that survives both
//! states — captured on hardware with Steam running and with Steam closed, the
//! button bits are identical — and reading it costs nothing, because hidraw
//! reads are not exclusive and run happily alongside Steam's own.
//!
//! ## The two things it feeds
//!
//! * The **shell**, which reads a [`Frame`] every poll. Directly, and not
//!   through the stand-in below: the report's layout was captured on this
//!   hardware, so which button is which is known here exactly, where a mapping
//!   database would be guessing at a pad it has never heard of.
//! * A **stand-in gamepad**, which is what everything else on the machine
//!   finds — see [`crate::steam_stand_in`]. Without it the pad is a controller
//!   only this shell can read, which is a controller that stops working the
//!   moment somebody launches a game with it.
//!
//! The Steam button goes to the first and never to the second. That is the same
//! bargain [`crate::pad_guard`] strikes with every controller the kernel does
//! drive, arrived at from the other end: there the pad is taken away and handed
//! back a button short, and here the only pad an application can find never had
//! the button on it.
//!
//! ## What it deliberately does not do
//!
//! Write to the pad. Leaving lizard mode, rumbling, reading the gyro, lighting
//! it up: all of it means *writing* feature and output reports, which is what
//! Steam does when it claims the pad, and two programs configuring one
//! controller is one controller doing neither. Read-only is what lets this
//! driver and Steam hold the same pad at the same time, and that is worth more
//! than any of it — see the module note in [`crate::steam_stand_in`].
//!
//! The duplicate that read-only leaves behind — lizard mode still typing the
//! same buttons as keys — is settled in the compositor, which drops the pad's
//! keyboard so this is the only path in.
//!
//! ## Why it has a thread
//!
//! Because the stand-in is a *controller*, and a game's input latency must not
//! be this shell's frame pacing. The reading happens as the pad reports, and
//! the shell picks up whatever has accumulated whenever it next asks. Same
//! reason [`crate::pad_guard`] has one.

use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::steam_stand_in::{DeckStandIn, StandIn};

/// Valve, and the Deck.
pub const VALVE_VENDOR: u16 = 0x28de;
pub const DECK_PRODUCT: u16 = 0x1205;

/// The Steam Controller 2's puck, the way in this shell met it by.
pub const PUCK_PRODUCT: u16 = 0x1304;

/// The Steam Controller 2's four ways in, as `hid-steam` names them in Linux
/// 7.3: a cable (Ibex), Bluetooth (Ibex BLE), its puck (Proteus), and a Steam
/// Machine's own receiver (Nereid). See the module's notes.
pub const CONTROLLER_PRODUCTS: [u16; 4] = [0x1302, 0x1303, PUCK_PRODUCT, 0x1305];

/// The two buses a `HID_ID` names these on.
const BUS_USB: u16 = 0x0003;
const BUS_BLUETOOTH: u16 = 0x0005;

/// Which of the two pads a raw node belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pad {
    /// The Steam Controller 2, by the product id it came in by — one of
    /// [`CONTROLLER_PRODUCTS`].
    Controller(u16),
    /// The Steam Deck's built-in controls, `28de:1205`.
    Deck,
}

impl Pad {
    /// Which pad a HID device's sysfs directory is, by its `HID_ID`.
    fn of(sysfs: &Path) -> Option<Self> {
        let (_, vendor, product) = hid_id(&uevent(sysfs))?;
        Self::by_ids(vendor, product)
    }

    /// Which pad a vendor and product are, if either.
    pub fn by_ids(vendor: u16, product: u16) -> Option<Self> {
        if vendor != VALVE_VENDOR {
            return None;
        }
        if product == DECK_PRODUCT {
            return Some(Pad::Deck);
        }
        CONTROLLER_PRODUCTS
            .contains(&product)
            .then_some(Pad::Controller(product))
    }

    pub const fn product(self) -> u16 {
        match self {
            Pad::Controller(product) => product,
            Pad::Deck => DECK_PRODUCT,
        }
    }

    /// What the log calls it.
    const fn name(self) -> &'static str {
        match self {
            Pad::Controller(_) => "Steam Controller 2",
            Pad::Deck => "Steam Deck controls",
        }
    }

    /// One report off this pad's node, decoded — or `None` for anything else
    /// the node says: housekeeping, or, on the Deck, the keyboard and mouse
    /// its firmware pretends to be, which arrive on nodes of their own.
    fn decode(self, report: &[u8]) -> Option<Report> {
        match self {
            Pad::Controller(_) => is_a_controller_report(report).then(|| decode(report)),
            Pad::Deck => decode_deck(report),
        }
    }
}

/// A `HID_ID` line's bus, vendor and product, out of a HID device's uevent.
///
/// Spelled `BBBB:VVVVVVVV:PPPPPPPP` in hexadecimal, and read as numbers rather
/// than matched as a string, so that the same pad on a cable and over
/// Bluetooth is one pad.
fn hid_id(uevent: &str) -> Option<(u16, u16, u16)> {
    let id = uevent
        .lines()
        .find_map(|line| line.strip_prefix("HID_ID="))?;
    let mut parts = id.trim().split(':');
    let mut next = || u32::from_str_radix(parts.next()?, 16).ok();
    let (bus, vendor, product) = (next()?, next()?, next()?);
    let bus = u16::try_from(bus).ok()?;
    if bus != BUS_USB && bus != BUS_BLUETOOTH {
        return None;
    }
    Some((
        bus,
        u16::try_from(vendor).ok()?,
        u16::try_from(product).ok()?,
    ))
}

/// The Deck's input report: 64 bytes, `01 00 09` at the front — a type of 1,
/// then `ID_CONTROLLER_DECK_STATE`, as `steam_raw_event` checks them.
const DECK_REPORT_LEN: usize = 64;
const DECK_REPORT_HEAD: [u8; 3] = [0x01, 0x00, 0x09];

/// Where each of the shell's buttons is in the Deck's report: byte, mask, and
/// what it is. `hid-steam`'s `steam_deck_button_mappings`, for the buttons the
/// shell has names for; the back grips, the trackpad clicks and the
/// quick-access button are the stand-in's, not the shell's.
const DECK_BUTTON_BITS: &[(usize, u8, Buttons)] = &[
    (8, 0x80, Buttons::A),
    (8, 0x20, Buttons::B),
    (8, 0x40, Buttons::X),
    (8, 0x10, Buttons::Y),
    (8, 0x08, Buttons::L1),
    (8, 0x04, Buttons::R1),
    (9, 0x01, Buttons::UP),
    (9, 0x02, Buttons::RIGHT),
    (9, 0x04, Buttons::LEFT),
    (9, 0x08, Buttons::DOWN),
    (9, 0x10, Buttons::VIEW),
    (9, 0x20, Buttons::STEAM),
    (9, 0x40, Buttons::MENU),
    (10, 0x40, Buttons::L3),
    (11, 0x04, Buttons::R3),
];

/// The Deck's sticks, as signed 16-bit little-endian pairs counting up, and its
/// triggers, `0..=0x7fff` — `hid-steam`'s `steam_deck_axis_mappings`.
const DECK_LEFT_STICK_X: usize = 48;
const DECK_LEFT_STICK_Y: usize = 50;
const DECK_RIGHT_STICK_X: usize = 52;
const DECK_RIGHT_STICK_Y: usize = 54;
const DECK_LEFT_TRIGGER: usize = 44;
const DECK_RIGHT_TRIGGER: usize = 46;

const _: () = {
    assert!(DECK_RIGHT_TRIGGER >= DECK_LEFT_TRIGGER + 2);
    assert!(DECK_LEFT_STICK_X >= DECK_RIGHT_TRIGGER + 2);
    assert!(DECK_RIGHT_STICK_Y + 2 <= DECK_REPORT_LEN);
};

/// Which pads this shell is reading off a raw node right now, one bit for each
/// of [`DRIVABLE`]. See [`is_a_stand_in`], which is the one question it
/// answers.
static DRIVEN: AtomicU8 = AtomicU8::new(0);

/// Every product this driver reads, in the order [`DRIVEN`]'s bits are in.
const DRIVABLE: [u16; 5] = [
    DECK_PRODUCT,
    CONTROLLER_PRODUCTS[0],
    CONTROLLER_PRODUCTS[1],
    CONTROLLER_PRODUCTS[2],
    CONTROLLER_PRODUCTS[3],
];

/// A product's bit in [`DRIVEN`].
fn driven_bit(product: u16) -> Option<u8> {
    DRIVABLE
        .iter()
        .position(|drivable| *drivable == product)
        .map(|at| 1 << at)
}

/// The controller's input report: `0x42` in the first byte, 54 bytes long.
const REPORT_ID: u8 = 0x42;
const REPORT_LEN: usize = 54;

/// And the one it sends over Bluetooth: `0x45`, 46 bytes — the same bytes less
/// the eight at the end, which carry nothing this driver reads. `hid-steam`
/// reads both with one function.
const SHORT_REPORT_ID: u8 = 0x45;
const SHORT_REPORT_LEN: usize = 46;

/// Whether a report off a controller's node is its input report, of either
/// length.
fn is_a_controller_report(report: &[u8]) -> bool {
    matches!(
        (report.first(), report.len()),
        (Some(&REPORT_ID), REPORT_LEN) | (Some(&SHORT_REPORT_ID), SHORT_REPORT_LEN)
    )
}

/// How often to look for a pad that was not there last time.
///
/// The dongle is a USB device the user can plug in mid-session, and four of its
/// five interfaces stay silent until a puck actually pairs to one. Rescanning
/// is a directory listing, so the interval only has to be short enough that
/// plugging a pad in feels like it worked.
const RESCAN_INTERVAL: Duration = Duration::from_secs(2);

/// One button, as a bit of its own.
///
/// A set rather than a struct of `bool`s because that is what the work is: two
/// pads are merged by OR-ing them, and the presses since the last report are
/// `now & !before`. Both are one instruction on a bitfield and a loop over
/// named fields.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Buttons(u16);

impl Buttons {
    pub const A: Self = Self(1 << 0);
    pub const B: Self = Self(1 << 1);
    pub const X: Self = Self(1 << 2);
    pub const Y: Self = Self(1 << 3);
    pub const UP: Self = Self(1 << 4);
    pub const DOWN: Self = Self(1 << 5);
    pub const LEFT: Self = Self(1 << 6);
    pub const RIGHT: Self = Self(1 << 7);
    pub const L1: Self = Self(1 << 8);
    pub const R1: Self = Self(1 << 9);
    pub const VIEW: Self = Self(1 << 10);
    pub const MENU: Self = Self(1 << 11);
    pub const STEAM: Self = Self(1 << 12);
    pub const L3: Self = Self(1 << 13);
    pub const R3: Self = Self(1 << 14);

    pub const fn empty() -> Self {
        Self(0)
    }

    /// Whether every button in `which` is down. Used with one button at a time.
    pub const fn has(self, which: Self) -> bool {
        self.0 & which.0 == which.0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Both sets at once — two pads merged, or the several buttons of a chord
    /// written down together.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// What is down here and was not down in `before`.
    const fn newly_down(self, before: Self) -> Self {
        Self(self.0 & !before.0)
    }

    /// Every button here that is not in `other`.
    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// Every button that is in both.
    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }
}

/// Where each button lives in the report: byte, mask, and what it is.
///
/// Captured on the hardware rather than taken from a document — three presses
/// of each control, twice over, once with Steam running and once without. Every
/// entry scored exactly three both times; the pad's gyro throws bits that score
/// three *once*, and disagreeing across the two runs is what rejected them.
///
/// `STEAM` is the one value that predates the capture, and the capture agreed
/// with it, which is the best evidence available that the rest are right too.
const BUTTON_BITS: &[(usize, u8, Buttons)] = &[
    (2, 0x01, Buttons::A),
    (2, 0x02, Buttons::B),
    (2, 0x04, Buttons::X),
    (2, 0x08, Buttons::Y),
    (2, 0x20, Buttons::R3),
    (2, 0x40, Buttons::MENU),
    (3, 0x02, Buttons::R1),
    (3, 0x04, Buttons::DOWN),
    (3, 0x08, Buttons::RIGHT),
    (3, 0x10, Buttons::LEFT),
    (3, 0x20, Buttons::UP),
    (3, 0x40, Buttons::VIEW),
    (3, 0x80, Buttons::L3),
    (4, 0x01, Buttons::STEAM),
    (4, 0x08, Buttons::L1),
];

/// The sticks, as signed 16-bit little-endian pairs.
const LEFT_STICK_X: usize = 10;
const LEFT_STICK_Y: usize = 12;
const RIGHT_STICK_X: usize = 14;
const RIGHT_STICK_Y: usize = 16;

/// The triggers, as signed 16-bit little-endian pairs like the sticks — low
/// byte first.
///
/// Captured 2026-08-23 by pulling each trigger slowly through its whole travel
/// and reading the two bytes back. Which byte is which was the whole question,
/// and the answer is not the obvious one: byte 6 is the **low** half, and on
/// one slow squeeze it sweeps its full range more than sixty times while byte 7
/// climbs once. Read the other way round a pull is a saw, not a squeeze — see
/// [`tests::a_captured_trigger_pull_is_one_sweep_and_not_a_saw`], which is that
/// capture, kept.
const LEFT_TRIGGER: usize = 6;
const RIGHT_TRIGGER: usize = 8;

/// What a trigger reads held all the way down.
///
/// `0x7fff` and not `0xffff`: the pad spends fifteen bits on a trigger, using
/// the positive half of the same signed range the sticks use. Measured at both
/// ends of both triggers.
pub const TRIGGER_FULL_SCALE: u16 = 0x7fff;

/// Four separate stick pairs and two triggers, in order, inside the report —
/// checked while the crate is compiled rather than while it is tested.
///
/// These are numbers taken off a capture, and the way they go wrong is somebody
/// editing one of them: two axes that overlap read the same bytes, so a stick
/// pushed sideways moves diagonally, and a pair past the end of the report
/// panics on the first frame the pad sends. Neither is anything the hardware
/// could tell us — the offsets are constants, so the answer is known before the
/// program runs, and a build is a far better place to learn it than a hand on a
/// stick.
const _: () = {
    assert!(RIGHT_TRIGGER >= LEFT_TRIGGER + 2);
    assert!(LEFT_STICK_X >= RIGHT_TRIGGER + 2);
    assert!(LEFT_STICK_Y >= LEFT_STICK_X + 2);
    assert!(RIGHT_STICK_X >= LEFT_STICK_Y + 2);
    assert!(RIGHT_STICK_Y >= RIGHT_STICK_X + 2);
    assert!(RIGHT_STICK_Y + 2 <= REPORT_LEN);
    // The Bluetooth report carries everything this driver reads too.
    assert!(RIGHT_STICK_Y + 2 <= SHORT_REPORT_LEN);
};

/// Whether the report's vertical axes count upwards.
///
/// The shell's convention is positive-up, as every gamepad API normalises it,
/// and Valve's pads report the same way — `hid-steam` negates their Y to get
/// evdev's positive-down, and so does [`crate::steam_stand_in`]. If vertical
/// navigation or the pointer ever comes out inverted on this pad, this is the
/// single line to flip; the D-pad is decoded digitally and is unaffected either
/// way.
const STICK_Y_IS_UP: bool = true;

/// How far off centre a stick has to sit before it is believed.
///
/// The sticks rest a little away from zero — measured around 2% of full travel
/// on this hardware, and it is not the same offset on each axis. Everything
/// inside this is reported as centred, which keeps a resting pad from slowly
/// walking the menu on its own.
///
/// The stand-in declares the same number as its `flat`, so an application gets
/// this pad's dead zone rather than inventing a second one.
pub const STICK_DEADZONE: f32 = 0.08;

/// One report, decoded and otherwise untouched.
///
/// Raw on purpose. This is what the stand-in sends on, and a driver that ate
/// part of its own input before passing it along would be deciding for every
/// game on the machine what counts as a thumb resting on a stick. The shell's
/// own dead zone is applied in [`Frame`], where it belongs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Report {
    pub buttons: Buttons,
    /// Each stick as `(x, y)`, signed, with y positive **up**.
    pub left: (i16, i16),
    pub right: (i16, i16),
    /// The triggers as `(left, right)`, each `0..=`[`TRIGGER_FULL_SCALE`].
    pub triggers: (u16, u16),
}

/// One poll's worth of the pad, as the shell reads it.
#[derive(Debug, Clone, Copy, Default)]
pub struct Frame {
    /// What is held down now.
    pub held: Buttons,
    /// What went down since the last poll. Accumulated across every report
    /// drained, so a press and release inside one poll interval still counts.
    pub pressed: Buttons,
    /// What came back up since the last poll. Only the pointer's borrowed
    /// buttons need this — a menu row activated on the way back up would fire
    /// twice — but a click held down has to be let go of eventually.
    pub released: Buttons,
    /// The sticks, each `(x, y)` in −1.0..=1.0 with y positive up, dead zone
    /// already applied.
    pub left_stick: (f32, f32),
    pub right_stick: (f32, f32),
    /// The triggers as `(left, right)`, each 0.0..=1.0 of their travel.
    ///
    /// How far they are pulled, not whether they are pulled: where a trigger
    /// becomes a press is a question about what the press is *for* — the
    /// pointer's two mouse buttons, on this pad and on every other — and it is
    /// answered once, in [`crate::controller`], so that both pads click at the
    /// same depth. This module's job is to say what the hardware is doing.
    pub triggers: (f32, f32),
}

/// The stand-in devices this shell is currently presenting, for the parts of
/// the shell that have to name one to somebody else.
///
/// A static for the same two reasons [`crate::pad_guard`]'s is: it is read
/// where an application's environment is assembled rather than anywhere the
/// driver is threaded through, and there is one set of controllers on a machine
/// however many parts of the shell want to know about them.
static STAND_INS: Mutex<Vec<StandInId>> = Mutex::new(Vec::new());

/// One stand-in, as anything downstream needs to recognise it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandInId {
    pub name: String,
    pub vendor: u16,
    pub product: u16,
}

/// The stand-in a controller list would show, if this shell is presenting one.
///
/// What wants it is the question of which pad somebody is holding: a hand on
/// this controller is a hand on the device an emulator will bind, and that
/// device is the stand-in. See [`crate::pads::order`].
pub fn stand_in() -> Option<StandInId> {
    STAND_INS
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .first()
        .cloned()
}

/// The pads whose raw HID nodes an application should ignore, because this
/// shell is standing a gamepad in front of them.
///
/// The other half of never sending the guide button. SDL prefers its own
/// HIDAPI drivers to `/dev/input` for the controllers it recognises, and reads
/// those over `hidraw` — where this driver's read-only hold on the pad stops
/// nothing, because `hidraw` cannot be held exclusively at all. A game whose
/// SDL knows this pad would find the Steam button there after all.
///
/// Only listed while a stand-in actually exists, which is what makes it safe:
/// asking an application to ignore the pad's raw node is asking it to use the
/// gamepad instead, and on a machine where the gamepad could not be made there
/// is no instead. Steam is unaffected either way — it drives this pad through
/// its own client rather than through SDL, which is exactly why the trackpads,
/// the gyro and the haptics keep working there and nowhere else.
pub fn hidapi_ignore_ids() -> Vec<(u16, u16)> {
    let mut ids: Vec<(u16, u16)> = Vec::new();
    for stand_in in STAND_INS
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .iter()
    {
        let id = (stand_in.vendor, stand_in.product);
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

/// Whether a controller GilRs has found is one of this driver's stand-ins.
///
/// The shell must not read its own stand-in: it reads this pad from the report,
/// which is the exact reading, and taking the same presses back through GilRs
/// as well would act on every one of them twice. Applications read the stand-in
/// and the shell reads the pad, and this is the line between them.
///
/// Matched on the ids, and on whether this driver is reading a pad that has
/// them, because the kernel may drive both: `hid-steam` has always had the
/// Deck, and has the Steam Controller 2 from Linux 7.3 — see the module's own
/// notes. Its gamepad carries the very ids the stand-in does. So the ids mean
/// "this shell's own" only while this driver is reading such a pad, which is
/// also exactly when the kernel's gamepad is gone: opening the raw node is what
/// takes it away. Where the raw node is left to the kernel, or could not be
/// opened, the kernel's gamepad is the one there is, and the shell reads it
/// like any other controller. Under `hid-generic` the controller has no gamepad
/// but the stand-in, and the answer comes out the same.
pub fn is_a_stand_in(vendor: Option<u16>, product: Option<u16>) -> bool {
    if vendor != Some(VALVE_VENDOR) {
        return false;
    }
    product
        .and_then(driven_bit)
        .is_some_and(|bit| DRIVEN.load(Ordering::Relaxed) & bit != 0)
}

/// A pad's firmware revision, as USB reports it, or `None`.
///
/// Read out of sysfs rather than written down, like every other fact about
/// what is plugged into this machine. Only the stand-in's device id is built
/// out of it.
pub fn firmware_version(pad: Pad) -> Option<u16> {
    let (node, _) = hidraw_nodes().into_iter().find(|(_, of)| *of == pad)?;
    node_firmware_version(&node)
}

/// The same, for one raw node: the USB device it is on, or `None` for one
/// that is on no USB device at all, such as a controller over Bluetooth.
fn node_firmware_version(node: &Path) -> Option<u16> {
    let mut at = std::fs::canonicalize(
        node.file_name()
            .map(|node| Path::new("/sys/class/hidraw").join(node).join("device"))?,
    )
    .ok()?;
    // Up through the HID device and its USB interface to the USB device, which
    // is the first ancestor that has a firmware revision at all.
    for _ in 0..4 {
        if let Ok(text) = std::fs::read_to_string(at.join("bcdDevice")) {
            return u16::from_str_radix(text.trim(), 16).ok();
        }
        at = at.parent()?.to_path_buf();
    }
    None
}

/// The shell's handle on the driver.
///
/// The work is on a thread of its own; this is the end of it the shell holds,
/// and dropping it stops the thread and takes every stand-in off the machine
/// with it.
pub struct SteamPad {
    shared: Option<Arc<Shared>>,
}

/// What the driver's thread and the shell's thread say to each other.
#[derive(Debug, Default)]
struct Shared {
    /// Set when the shell is going away.
    stop: AtomicBool,
    /// Set by [`SteamPad::hand_back`] and taken by the thread at its next
    /// report, which is never more than four milliseconds away: this pad
    /// reports whether or not anything on it moved, so the thread needs no
    /// knock of the kind [`crate::pad_guard`] has.
    hand_back: AtomicBool,
    state: Mutex<State>,
}

/// The pad as the shell will next read it.
#[derive(Debug, Default)]
struct State {
    /// Whether any pad has ever sent a report. The dongle presents one
    /// interface per pad slot and four of them stay silent all session, so a
    /// node that has never spoken is not a controller that is there.
    speaking: bool,
    held: Buttons,
    /// Accumulated between the shell's polls and taken by it, so a press and a
    /// release inside one poll interval both still count.
    pressed: Buttons,
    released: Buttons,
    left: (f32, f32),
    right: (f32, f32),
    triggers: (f32, f32),
}

impl SteamPad {
    /// Start the driver, or do nothing at all when controller input is off.
    pub fn new(enabled: bool) -> Self {
        if !enabled {
            // `--no-gamepad` means this shell reads no controller, and it has
            // no business putting one on the machine for everything else
            // either.
            return Self { shared: None };
        }

        let shared = Arc::new(Shared::default());
        let worker = Arc::clone(&shared);
        match std::thread::Builder::new()
            .name("lxb-steam-pad".into())
            .spawn(move || Worker::new(worker).run())
        {
            Ok(_) => Self {
                shared: Some(shared),
            },
            Err(err) => {
                tracing::warn!(%err, "could not start the Steam Controller driver; that pad will not work");
                Self { shared: None }
            }
        }
    }

    /// What the pad is doing, or `None` when there is no pad to ask.
    pub fn poll(&mut self) -> Option<Frame> {
        let shared = self.shared.as_ref()?;
        let mut state = shared.state.lock().unwrap_or_else(|err| err.into_inner());
        if !state.speaking {
            return None;
        }
        Some(Frame {
            held: state.held,
            pressed: std::mem::take(&mut state.pressed),
            released: std::mem::take(&mut state.released),
            left_stick: state.left,
            right_stick: state.right,
            triggers: state.triggers,
        })
    }

    /// The shell is letting go of the pad: let go on the stand-in of whatever
    /// is down on it, and keep it up there until the thumb comes off.
    ///
    /// The same rule, for the same reason, as
    /// [`crate::pad_guard::PadGuard::hand_back`]. What it reaches is less: an
    /// application reading this pad through the stand-in gets the rule, and
    /// Valve's client does not, because it reads this pad from its own raw
    /// node and never through anything the shell makes.
    pub fn hand_back(&self) {
        if let Some(shared) = &self.shared {
            shared.hand_back.store(true, Ordering::Release);
        }
    }
}

impl Drop for SteamPad {
    fn drop(&mut self) {
        if let Some(shared) = &self.shared {
            shared.stop.store(true, Ordering::Relaxed);
        }
        // Deliberately not joined, for the reason `pad_guard` gives: the thread
        // is asleep for at most one rescan, and whether it wakes or the process
        // ends, closing the `uinput` descriptor is what takes the stand-in off
        // the machine.
    }
}

/// The driver's own thread.
struct Worker {
    shared: Arc<Shared>,
    pucks: Vec<Puck>,
    /// What has been said about which driver each pad has, so a pad that
    /// disconnects and comes back does not narrate itself every two seconds,
    /// and a change of driver is said once.
    said: Vec<(Pad, Source)>,
}

/// One pad, and the gamepad standing in for it.
struct Puck {
    path: PathBuf,
    /// Which pad this node is.
    pad: Pad,
    /// Whether it came in over Bluetooth, which its stand-in says too.
    bluetooth: bool,
    file: File,
    /// What the last report said, so presses are reported as the edges they are
    /// rather than once per report for as long as one is held.
    report: Report,
    /// The last report as it came, for the Deck's stand-in, which reads more of
    /// it than [`Report`] carries.
    raw: [u8; DECK_REPORT_LEN],
    /// Whether this node has ever sent an input report.
    speaking: bool,
    /// What every other program on the machine finds. `None` where one could
    /// not be made — no `/dev/uinput`, or no permission — which costs
    /// applications this controller and costs the shell nothing.
    stand_in: Option<Standing>,
    /// Whether making one has been attempted, so that a failure is a warning
    /// once rather than one per report for the rest of the session.
    attempted: bool,
}

/// The gamepad standing in for one pad: the puck's in the shape of an Xbox
/// pad, which is the one shape every program agrees on, and the Deck's in the
/// shape its own kernel driver gives it, which is the one every program already
/// has a mapping for. See [`crate::steam_stand_in`].
enum Standing {
    Puck(StandIn),
    Deck(DeckStandIn),
}

impl Standing {
    fn name(&self) -> &str {
        match self {
            Standing::Puck(stand_in) => stand_in.name(),
            Standing::Deck(stand_in) => stand_in.name(),
        }
    }

    fn node(&self) -> &Path {
        match self {
            Standing::Puck(stand_in) => stand_in.node(),
            Standing::Deck(stand_in) => stand_in.node(),
        }
    }

    fn send(&mut self, report: Report, raw: &[u8]) -> std::io::Result<()> {
        match self {
            Standing::Puck(stand_in) => stand_in.send(report),
            Standing::Deck(stand_in) => stand_in.send(raw),
        }
    }

    fn hand_back(&mut self, report: Report, raw: &[u8]) -> std::io::Result<()> {
        match self {
            Standing::Puck(stand_in) => stand_in.hand_back(report),
            Standing::Deck(stand_in) => stand_in.hand_back(raw),
        }
    }
}

impl Worker {
    fn new(shared: Arc<Shared>) -> Self {
        Self {
            shared,
            pucks: Vec::new(),
            said: Vec::new(),
        }
    }

    fn run(mut self) {
        let mut next_scan = Instant::now();
        while !self.shared.stop.load(Ordering::Relaxed) {
            if Instant::now() >= next_scan {
                self.scan();
                next_scan = Instant::now() + RESCAN_INTERVAL;
            }
            self.wait(next_scan.saturating_duration_since(Instant::now()));
            self.pump();
        }
        // Every stand-in goes with the thread — dropping `pucks` closes the
        // `uinput` descriptors, which is what takes the gamepads off the
        // machine — so what the rest of the shell has been told about them has
        // to go too. Otherwise an application started afterwards is asked to
        // ignore the pad's raw node in favour of a device that no longer
        // exists, which is the one thing this must never do. See
        // [`hidapi_ignore_ids`].
        self.pucks.clear();
        self.publish();
        // And the ids stop being this shell's own: with the raw node closed, a
        // kernel that drives the pad makes its gamepad again.
        DRIVEN.store(0, Ordering::Relaxed);
        tracing::info!("Steam Controller driver stopping; its gamepads go with it");
    }

    /// Sleep until the pad has something to say, or until the next rescan.
    fn wait(&self, timeout: Duration) {
        let mut fds: Vec<libc::pollfd> = self
            .pucks
            .iter()
            .map(|puck| libc::pollfd {
                fd: puck.file.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        let timeout = timeout.as_millis().min(i32::MAX as u128) as libc::c_int;
        // SAFETY: `fds` is a valid slice of that many pollfds for the call's
        // duration, and every descriptor in it is owned by a pad this driver
        // still holds. With no pads the count is zero, which poll reads as a
        // plain sleep and never dereferences the pointer for.
        unsafe {
            libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout);
        }
    }

    /// Read every pad, feed every stand-in, and leave the shell what it is
    /// owed.
    ///
    /// Every pending report is drained rather than one: a pad reporting every
    /// four milliseconds would otherwise fall steadily further behind, and the
    /// buttons would answer late by however long the session had been running.
    fn pump(&mut self) {
        let mut pressed = Buttons::empty();
        let mut released = Buttons::empty();
        let mut lost = Vec::new();
        let mut arrived = false;
        let mut buf = [0u8; 64];

        // Before the reports are read, as the guard does it: what the shell
        // asked about is the pad as its last report left it, which is what the
        // stand-in has been told of it.
        if self.shared.hand_back.swap(false, Ordering::AcqRel) {
            for puck in &mut self.pucks {
                let Some(stand_in) = puck.stand_in.as_mut() else {
                    continue;
                };
                if let Err(err) = stand_in.hand_back(puck.report, &puck.raw) {
                    tracing::warn!(path = %puck.path.display(), %err, "could not let go of the buttons held when the pad was handed back");
                }
            }
        }

        for (index, puck) in self.pucks.iter_mut().enumerate() {
            loop {
                match puck.file.read(&mut buf) {
                    Ok(0) => break,
                    Ok(len) => {
                        // Reports that are not the input report are the pad's
                        // own housekeeping — on the puck, battery on `0x43` and
                        // `0x7b` every half second — or, on a Deck, the keyboard
                        // and mouse its firmware pretends to be, on nodes of
                        // their own. Only the input report carries the buttons.
                        let Some(report) = puck.pad.decode(&buf[..len]) else {
                            continue;
                        };
                        puck.speaking = true;
                        pressed = pressed.union(report.buttons.newly_down(puck.report.buttons));
                        released = released.union(puck.report.buttons.newly_down(report.buttons));
                        puck.report = report;
                        puck.raw[..len].copy_from_slice(&buf[..len]);
                        arrived |= puck.speak(report);
                    }
                    Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                    Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                    Err(err) => {
                        // Unplugged, almost always. Drop it and let the next
                        // scan pick the pad up if it comes back.
                        tracing::debug!(
                            path = %puck.path.display(),
                            %err,
                            "steam controller hidraw closed"
                        );
                        lost.push(index);
                        break;
                    }
                }
            }
        }

        for index in lost.into_iter().rev() {
            let puck = self.pucks.remove(index);
            if puck.stand_in.is_some() {
                arrived = true;
                tracing::info!(path = %puck.path.display(), "Steam Controller unplugged; its gamepad goes with it");
            }
        }
        DRIVEN.store(driven(&self.pucks), Ordering::Relaxed);
        if arrived {
            // Published the moment the set changes rather than once a scan: an
            // application started in between would otherwise be told to ignore
            // a raw node with nothing standing in front of it yet, or not told
            // about one that is.
            self.publish();
        }

        self.report(pressed, released);
    }

    /// Merge every pad into the one answer the shell asks for.
    ///
    /// Two pads plugged in are merged rather than one being chosen, so a
    /// session is drivable from either.
    fn report(&self, pressed: Buttons, released: Buttons) {
        let speaking = self.pucks.iter().any(|puck| puck.speaking);
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        state.speaking = speaking;
        state.pressed = state.pressed.union(pressed);
        state.released = state.released.union(released);
        state.held = Buttons::empty();
        state.left = (0.0, 0.0);
        state.right = (0.0, 0.0);
        state.triggers = (0.0, 0.0);
        for puck in self.pucks.iter().filter(|puck| puck.speaking) {
            state.held = state.held.union(puck.report.buttons);
            state.left = further(state.left, normalise(puck.report.left));
            state.right = further(state.right, normalise(puck.report.right));
            // Merged the way the sticks are, and for the same reason: two pads
            // plugged in are one pad to drive the session with, and the answer
            // is whichever of them is being pulled.
            state.triggers = further(state.triggers, pulled(puck.report.triggers));
        }
    }

    /// Look at every raw node there is, and read the ones this driver should.
    ///
    /// Asked of every node on every look, not only of new ones, because the
    /// answer moves under a node that stays: another program opening it takes
    /// the kernel's gamepad away, and letting go of it gives the gamepad back.
    /// See [`reads_the_raw_node`].
    fn scan(&mut self) {
        let machine = Machine::real();
        let lizard_mode = machine.lizard_mode();
        for (path, pad) in hidraw_nodes() {
            let held_here = self.pucks.iter().any(|puck| puck.path == path);
            let facts = machine.facts(&path, lizard_mode);
            let source = source(facts);
            self.say(pad, source);
            match (reads_the_raw_node(facts), held_here) {
                (true, false) => self.open(path, pad),
                (false, true) => self.let_go(&path),
                _ => {}
            }
        }
    }

    /// Say which driver a pad has, the first time and whenever it changes.
    fn say(&mut self, pad: Pad, source: Source) {
        let known = |said: &(Pad, Source)| said.0.product() == pad.product();
        if self.said.iter().any(|said| known(said) && said.1 == source) {
            return;
        }
        self.said.retain(|said| !known(said));
        self.said.push((pad, source));
        let name = pad.name();
        match source {
            Source::Nothing => {}
            Source::NoKernelDriver => tracing::info!(
                "{name} found: no kernel driver here takes it, so the shell drives it from \
                 its raw node, which answers in the firmware's keyboard mode and while Steam \
                 holds it"
            ),
            Source::KernelKeepsItTyping => tracing::info!(
                "{name} found: hid-steam drives it in lizard mode, whose gamepad is silent \
                 until the menu button is held, so the shell reads its raw node instead"
            ),
            Source::KernelGamepad => tracing::info!(
                "{name} found: hid-steam drives it with lizard mode off, so the shell leaves \
                 its raw node alone and uses the kernel's gamepad"
            ),
            Source::KernelGamepadTaken => tracing::info!(
                "{name}: another program has its raw node open, which takes the kernel's \
                 gamepad away, so the shell reads the raw node until it is the only one left"
            ),
        }
    }

    /// Start reading one raw node.
    fn open(&mut self, path: PathBuf, pad: Pad) {
        match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)
        {
            Ok(file) => {
                tracing::debug!(path = %path.display(), ?pad, "steam controller hidraw opened");
                let bluetooth = node_bus(&path) == Some(BUS_BLUETOOTH);
                self.pucks.push(Puck {
                    path,
                    pad,
                    bluetooth,
                    file,
                    report: Report::default(),
                    raw: [0u8; DECK_REPORT_LEN],
                    speaking: false,
                    stand_in: None,
                    attempted: false,
                });
            }
            Err(err) => {
                // Losing this is not fatal on a machine with a keyboard,
                // but it is the whole controller — for the shell and for
                // everything else — and it is invisible without a word
                // here.
                tracing::warn!(
                    path = %path.display(),
                    %err,
                    "cannot read Steam Controller hidraw; the pad will not work"
                );
            }
        }
    }

    /// Stop reading one raw node, so the kernel can make its gamepad again.
    ///
    /// Its stand-in goes with it, and the shell is told at once rather than at
    /// the next report, which a node nobody reads will never send.
    fn let_go(&mut self, path: &Path) {
        let before = self.pucks.len();
        self.pucks.retain(|puck| puck.path != path);
        if self.pucks.len() != before {
            tracing::debug!(path = %path.display(), "steam controller hidraw let go of");
            DRIVEN.store(driven(&self.pucks), Ordering::Relaxed);
            self.publish();
        }
    }

    /// Write the stand-ins where the rest of the shell can read them.
    fn publish(&self) {
        let mut ids: Vec<StandInId> = Vec::new();
        for puck in &self.pucks {
            if let Some(stand_in) = &puck.stand_in {
                ids.push(StandInId {
                    name: stand_in.name().to_string(),
                    vendor: VALVE_VENDOR,
                    product: puck.pad.product(),
                });
            }
        }
        *STAND_INS.lock().unwrap_or_else(|err| err.into_inner()) = ids;
    }
}

impl Puck {
    /// Pass one report on to the gamepad every other program reads, making that
    /// gamepad if this is the first thing the pad has said.
    ///
    /// Made on the first report rather than when the node is opened, because
    /// the dongle presents one hidraw interface per pad slot and four of them
    /// are empty: a stand-in each would put five controllers on a machine with
    /// one.
    ///
    /// Answers whether that just happened, which is when the rest of the shell
    /// has to be told what is on the machine.
    fn speak(&mut self, report: Report) -> bool {
        let mut arrived = false;
        if self.stand_in.is_none() && !self.attempted {
            self.attempted = true;
            let made = match self.pad {
                Pad::Controller(product) => StandIn::new(
                    &hid_name(&self.path),
                    product,
                    self.bluetooth,
                    node_firmware_version(&self.path).unwrap_or(0),
                )
                .map(Standing::Puck),
                Pad::Deck => DeckStandIn::new().map(Standing::Deck),
            };
            match made {
                Ok(stand_in) => {
                    tracing::info!(
                        pad = %stand_in.name(),
                        node = %stand_in.node().display(),
                        "Steam Controller given a gamepad; applications get every button but Steam"
                    );
                    self.stand_in = Some(stand_in);
                    arrived = true;
                }
                Err(err) => {
                    // The shell still has the pad; nothing else does. Said once
                    // per pad, because retrying every report would be a warning
                    // every four milliseconds.
                    tracing::warn!(
                        path = %self.path.display(),
                        %err,
                        "could not give the Steam Controller a gamepad; the shell can read it \
                         but applications cannot"
                    );
                }
            }
        }
        let Some(stand_in) = self.stand_in.as_mut() else {
            // Not retried: the one reason making one fails is `/dev/uinput`,
            // which does not appear partway through a session, and a shell that
            // asked again every four milliseconds would be a warning every four
            // milliseconds. The shell still has the pad; nothing else does.
            return arrived;
        };
        if let Err(err) = stand_in.send(report, &self.raw) {
            tracing::warn!(path = %self.path.display(), %err, "could not pass a report to the gamepad");
        }
        arrived
    }
}

/// [`DRIVEN`]'s bits for the nodes this driver is reading: a bit for every
/// pad with a node that has spoken.
fn driven(pucks: &[Puck]) -> u8 {
    pucks
        .iter()
        .filter(|puck| puck.speaking)
        .filter_map(|puck| driven_bit(puck.pad.product()))
        .fold(0, |bits, bit| bits | bit)
}

/// One report, decoded.
fn decode(report: &[u8]) -> Report {
    Report {
        buttons: decode_buttons(report),
        left: decode_stick(report, LEFT_STICK_X, LEFT_STICK_Y),
        right: decode_stick(report, RIGHT_STICK_X, RIGHT_STICK_Y),
        triggers: (
            decode_trigger(report, LEFT_TRIGGER),
            decode_trigger(report, RIGHT_TRIGGER),
        ),
    }
}

/// One Deck report, decoded, or `None` for anything that is not one.
///
/// Checked the way `hid-steam` checks it before it reads a byte: the length,
/// then the three bytes at the front. The Deck's other nodes are its firmware's
/// keyboard and mouse, whose reports are a few bytes long and never begin like
/// this, so they are simply not the Deck as far as this driver is concerned.
fn decode_deck(report: &[u8]) -> Option<Report> {
    if report.len() != DECK_REPORT_LEN || report[..3] != DECK_REPORT_HEAD {
        return None;
    }
    Some(Report {
        buttons: buttons_from(report, DECK_BUTTON_BITS),
        left: decode_stick(report, DECK_LEFT_STICK_X, DECK_LEFT_STICK_Y),
        right: decode_stick(report, DECK_RIGHT_STICK_X, DECK_RIGHT_STICK_Y),
        triggers: (
            decode_trigger(report, DECK_LEFT_TRIGGER),
            decode_trigger(report, DECK_RIGHT_TRIGGER),
        ),
    })
}

/// Which buttons a report says are down.
fn decode_buttons(report: &[u8]) -> Buttons {
    buttons_from(report, BUTTON_BITS)
}

/// Which of the buttons in `table` a report says are down.
fn buttons_from(report: &[u8], table: &[(usize, u8, Buttons)]) -> Buttons {
    let mut buttons = Buttons::empty();
    for (byte, mask, button) in table {
        if report[*byte] & mask != 0 {
            buttons = buttons.union(*button);
        }
    }
    buttons
}

/// One stick, as the report sends it, with y positive up.
fn decode_stick(report: &[u8], x_at: usize, y_at: usize) -> (i16, i16) {
    let x = axis(report, x_at);
    let y = axis(report, y_at);
    // `saturating_neg` rather than `-`, which would overflow on exactly one of
    // the 65,536 values a stick can send.
    (x, if STICK_Y_IS_UP { y } else { y.saturating_neg() })
}

/// One signed 16-bit little-endian axis.
fn axis(report: &[u8], at: usize) -> i16 {
    i16::from_le_bytes([report[at], report[at + 1]])
}

/// One trigger, `0..=`[`TRIGGER_FULL_SCALE`].
///
/// Clamped rather than trusted: the range was measured, and a trigger reading
/// past the end of what the stand-in declared would be a value no application
/// was told this pad could send.
fn decode_trigger(report: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([report[at], report[at + 1]]).min(TRIGGER_FULL_SCALE)
}

/// One stick as the shell reads it: −1.0..=1.0, dead zone applied.
fn normalise((x, y): (i16, i16)) -> (f32, f32) {
    (deadzone(scale(x)), deadzone(scale(y)))
}

fn scale(raw: i16) -> f32 {
    // i16::MIN would give a shade over 1.0 the other way, which nothing here
    // wants to have to think about.
    (raw as f32 / i16::MAX as f32).clamp(-1.0, 1.0)
}

fn deadzone(value: f32) -> f32 {
    if value.abs() < STICK_DEADZONE {
        0.0
    } else {
        value
    }
}

/// The two triggers as the shell reads them: 0.0..=1.0 of their travel.
///
/// No dead zone. A stick's is about a thumb resting on it; a trigger rests
/// against its own stop and reads zero, and what is done with a part-pulled one
/// is the caller's question.
fn pulled((left, right): (u16, u16)) -> (f32, f32) {
    let travel = |raw: u16| f32::from(raw) / f32::from(TRIGGER_FULL_SCALE);
    (travel(left), travel(right))
}

/// Whichever of two stick positions is further from rest, axis by axis.
fn further(current: (f32, f32), candidate: (f32, f32)) -> (f32, f32) {
    let pick = |a: f32, b: f32| if b.abs() > a.abs() { b } else { a };
    (pick(current.0, candidate.0), pick(current.1, candidate.1))
}

/// Every hidraw node belonging to a second-generation Steam Controller or a
/// Steam Deck, with which it is.
///
/// Matched on `HID_ID` from sysfs rather than on the device name, which is a
/// string the firmware picks, or on the node number, which is whatever order
/// the machine happened to enumerate its USB devices in. A Deck has three —
/// its firmware's keyboard, its mouse, and the controls — and all three are
/// opened; only the one that speaks the Deck's report is ever read as a pad.
fn hidraw_nodes() -> Vec<(PathBuf, Pad)> {
    let Ok(entries) = std::fs::read_dir("/sys/class/hidraw") else {
        return Vec::new();
    };
    let mut nodes: Vec<(PathBuf, Pad)> = entries
        .flatten()
        .filter_map(|entry| {
            let pad = Pad::of(&entry.path())?;
            Some((Path::new("/dev").join(entry.file_name()), pad))
        })
        .filter(|(node, _)| node.exists())
        .collect();
    // The dongle presents one interface per pad slot, and `read_dir` is in no
    // particular order. Sorting only makes the logs reproducible.
    nodes.sort_by(|a, b| a.0.cmp(&b.0));
    nodes
}

/// What the pad calls itself, for the stand-in to be called the same.
///
/// Read from the HID device rather than written down: the name in a controller
/// list should be the hardware's, and a shell that invented one would be a
/// shell somebody cannot find their controller in. Falls back to something
/// plain rather than to nothing — a device with no name at all is a row of
/// blank space in every controller list on the machine.
fn hid_name(node: &Path) -> String {
    let sysfs = node
        .file_name()
        .map(|name| Path::new("/sys/class/hidraw").join(name))
        .unwrap_or_default();
    uevent(&sysfs)
        .lines()
        .find_map(|line| line.strip_prefix("HID_NAME=").map(str::to_string))
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "Steam Controller".to_string())
}

fn uevent(sysfs: &Path) -> String {
    std::fs::read_to_string(sysfs.join("device/uevent")).unwrap_or_default()
}

/// The bus a raw node's pad is on, by its `HID_ID`.
fn node_bus(node: &Path) -> Option<u16> {
    let name = node.file_name()?;
    let (bus, _, _) = hid_id(&uevent(&Path::new("/sys/class/hidraw").join(name)))?;
    Some(bus)
}

/// Which kernel driver has a raw node's device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Driver {
    /// `hid-steam`, which makes a gamepad of its own for these pads.
    HidSteam,
    /// Anything else — `hid-generic`, on a kernel that does not know the pad —
    /// which makes nothing a game could read as a controller.
    Other,
}

/// What one look at the machine says about one raw node: everything
/// [`reads_the_raw_node`] decides by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Facts {
    driver: Driver,
    /// `hid-steam`'s `lizard_mode`, or `None` where the module is not loaded.
    lizard_mode: Option<bool>,
    /// Whether the kernel has a gamepad up for this node's pad right now.
    kernel_gamepad: bool,
    /// Whether a program other than this one has the node open.
    held_elsewhere: bool,
}

/// Where the shell gets a pad's input from, and why — what [`Worker::say`]
/// logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// The raw node, because no kernel driver makes a gamepad of it here.
    NoKernelDriver,
    /// The raw node, because `hid-steam`'s gamepad is silent in lizard mode.
    KernelKeepsItTyping,
    /// The kernel's own gamepad.
    KernelGamepad,
    /// The raw node, while another program holds it and so has taken the
    /// kernel's gamepad away.
    KernelGamepadTaken,
    /// Nothing: the kernel has no gamepad up and nobody holds the node — an
    /// empty slot on a receiver, or the Deck's keyboard and mouse.
    Nothing,
}

/// Where a raw node's pad is read from. See the module's notes, "Which
/// driver", which this is.
fn source(facts: Facts) -> Source {
    match facts.driver {
        Driver::Other => Source::NoKernelDriver,
        Driver::HidSteam if facts.lizard_mode != Some(false) => Source::KernelKeepsItTyping,
        Driver::HidSteam if facts.kernel_gamepad => Source::KernelGamepad,
        Driver::HidSteam if facts.held_elsewhere => Source::KernelGamepadTaken,
        Driver::HidSteam => Source::Nothing,
    }
}

/// Whether this driver reads a raw node, or leaves its pad to the kernel.
///
/// Every answer here was a choice between two drivers for one controller, and
/// what decides it is which of them answers the first press. Without a kernel
/// driver only the raw node does. With `hid-steam` in lizard mode, its gamepad
/// says nothing until somebody holds ☰ or Start — a hold it does not hear while
/// anything reads the raw node, which is the whole session here — and that is
/// the Steam Deck report of 2026-09-30, a shell driven by keystrokes with no
/// controller to offer games. With `lizard_mode` off, the kernel's gamepad
/// answers at once and carries what a stand-in does not, rumble and motion; it
/// is the one to use, until a program that reads the raw node itself takes it
/// away, and then the raw node is what is left.
fn reads_the_raw_node(facts: Facts) -> bool {
    matches!(
        source(facts),
        Source::NoKernelDriver | Source::KernelKeepsItTyping | Source::KernelGamepadTaken
    )
}

/// Where sysfs and `/proc` are — the real ones, or a test's.
struct Machine {
    sys: PathBuf,
    proc: PathBuf,
    /// This process, which is never "another program".
    me: u32,
}

impl Machine {
    fn real() -> Self {
        Self {
            sys: PathBuf::from("/sys"),
            proc: PathBuf::from("/proc"),
            me: std::process::id(),
        }
    }

    /// `hid-steam`'s `lizard_mode`, or `None` where the module is not loaded.
    fn lizard_mode(&self) -> Option<bool> {
        let path = self.sys.join("module/hid_steam/parameters/lizard_mode");
        match std::fs::read_to_string(path).ok()?.trim() {
            "Y" | "y" | "1" => Some(true),
            "N" | "n" | "0" => Some(false),
            _ => None,
        }
    }

    /// Everything [`reads_the_raw_node`] needs about one node, `/dev/hidrawN`.
    fn facts(&self, node: &Path, lizard_mode: Option<bool>) -> Facts {
        let device = node
            .file_name()
            .map(|name| self.sys.join("class/hidraw").join(name).join("device"));
        let driver = device
            .as_ref()
            .and_then(|device| std::fs::read_link(device.join("driver")).ok())
            .filter(|driver| driver.file_name().is_some_and(|name| name == "hid-steam"))
            .map_or(Driver::Other, |_| Driver::HidSteam);
        let kernel_gamepad = driver == Driver::HidSteam
            && device
                .as_ref()
                .is_some_and(|device| self.has_a_kernel_gamepad(device));
        // A walk of every process, so only where it can change the answer.
        let held_elsewhere = driver == Driver::HidSteam
            && lizard_mode == Some(false)
            && !kernel_gamepad
            && self.held_by_another_program(node);
        Facts {
            driver,
            lizard_mode,
            kernel_gamepad,
            held_elsewhere,
        }
    }

    /// Whether the kernel has a gamepad up for the pad a raw node's HID device
    /// belongs to.
    ///
    /// The node is not on the device the gamepad hangs off. `hid-steam` gives
    /// the raw node to a *client* device of its own, beside the real one under
    /// the same USB interface (or Bluetooth link) and with the same ids, and
    /// the gamepad and its motion sensors are the real one's. So every HID
    /// device there with the node's ids is looked in, for an input device that
    /// has a gamepad's buttons and sticks and is not an accelerometer.
    fn has_a_kernel_gamepad(&self, device: &Path) -> bool {
        let Ok(device) = std::fs::canonicalize(device) else {
            return false;
        };
        let Some(ids) = device
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.split('.').next())
            .map(str::to_string)
        else {
            return false;
        };
        let Some(parent) = device.parent() else {
            return false;
        };
        let Ok(siblings) = std::fs::read_dir(parent) else {
            return false;
        };
        siblings.flatten().any(|sibling| {
            let name = sibling.file_name();
            let same = name
                .to_str()
                .is_some_and(|name| name.split('.').next() == Some(ids.as_str()));
            same && std::fs::read_dir(sibling.path().join("input"))
                .is_ok_and(|inputs| inputs.flatten().any(|input| is_a_gamepad(&input.path())))
        })
    }

    /// Whether a process other than this one has `node` open.
    ///
    /// A walk of every process's descriptors, so only ever asked of a pad the
    /// kernel would otherwise have — see [`Machine::facts`]. Processes that
    /// belong to somebody else cannot be looked into and are not counted, which
    /// costs nothing that matters: the program that takes a pad's gamepad away
    /// in a session is that session's own Steam.
    fn held_by_another_program(&self, node: &Path) -> bool {
        let me = self.me.to_string();
        let Ok(processes) = std::fs::read_dir(&self.proc) else {
            return false;
        };
        processes.flatten().any(|process| {
            let name = process.file_name();
            let Some(pid) = name.to_str() else {
                return false;
            };
            if pid == me || !pid.bytes().all(|byte| byte.is_ascii_digit()) {
                return false;
            }
            std::fs::read_dir(process.path().join("fd")).is_ok_and(|fds| {
                fds.flatten()
                    .any(|fd| std::fs::read_link(fd.path()).is_ok_and(|open| open == node))
            })
        })
    }
}

/// Whether an input device's sysfs directory is a gamepad: the south face
/// button and a left stick, and not the accelerometer `hid-steam` puts beside
/// the gamepad with sticks of its own.
fn is_a_gamepad(input: &Path) -> bool {
    let has = |file: &str, bit: usize| {
        std::fs::read_to_string(input.join(file)).is_ok_and(|bits| bitmap_has(&bits, bit))
    };
    const BTN_SOUTH: usize = 0x130;
    const ABS_X: usize = 0x00;
    const INPUT_PROP_ACCELEROMETER: usize = 0x06;
    has("capabilities/key", BTN_SOUTH)
        && has("capabilities/abs", ABS_X)
        && !has("properties", INPUT_PROP_ACCELEROMETER)
}

/// Whether one bit is set in a bitmap as sysfs prints an input device's:
/// hexadecimal words of the kernel's `long`, highest first, and leading words
/// that are zero left out.
fn bitmap_has(bitmap: &str, bit: usize) -> bool {
    let word_bits = std::mem::size_of::<libc::c_ulong>() * 8;
    let words: Vec<&str> = bitmap.split_whitespace().collect();
    let Some(at) = words.len().checked_sub(bit / word_bits + 1) else {
        return false;
    };
    u64::from_str_radix(words[at], 16).is_ok_and(|word| (word >> (bit % word_bits)) & 1 == 1)
}

/// The stand-in list is one list for the whole process, and a hardware test
/// puts a real controller on a machine that has one of those too. Tests that
/// touch either take this first.
#[cfg(test)]
static SERIAL: Mutex<()> = Mutex::new(());

#[cfg(test)]
pub(crate) fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|err| err.into_inner())
}

/// Put a stand-in on the published list, or take it off, without a pad.
///
/// For [`crate::pad_guard`]'s test of the list both modules write into. The
/// caller holds [`serial`].
#[cfg(test)]
pub(crate) fn pretend_a_stand_in_exists(present: bool) {
    let mut stand_ins = STAND_INS.lock().unwrap_or_else(|err| err.into_inner());
    *stand_ins = if present {
        vec![StandInId {
            name: "Valve Software Steam Controller Puck".into(),
            vendor: VALVE_VENDOR,
            product: PUCK_PRODUCT,
        }]
    } else {
        Vec::new()
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> [u8; REPORT_LEN] {
        let mut report = [0u8; REPORT_LEN];
        report[0] = REPORT_ID;
        report
    }

    /// The one number that predates the capture, spelled out so a careless
    /// edit to the table has to break a test that names the button.
    #[test]
    fn the_steam_button_is_byte_four_bit_zero() {
        let mut at_rest = report();
        assert!(!decode_buttons(&at_rest).has(Buttons::STEAM), "released");

        at_rest[4] = 0x01;
        assert!(decode_buttons(&at_rest).has(Buttons::STEAM));

        // The bits either side of it are other buttons, and none of them is
        // the Steam button.
        let mut neighbours = report();
        neighbours[4] = 0xfe;
        assert!(!decode_buttons(&neighbours).has(Buttons::STEAM));
    }

    /// Every button, at the offset the hardware capture put it at. This is the
    /// map; if it is wrong the pad is wrong, so it is written out in full.
    #[test]
    fn every_button_decodes_from_the_bit_it_was_captured_at() {
        for (byte, mask, button) in BUTTON_BITS {
            let mut one = report();
            one[*byte] = *mask;
            let decoded = decode_buttons(&one);
            assert!(decoded.has(*button), "byte {byte} mask {mask:#04x}");
            // And nothing else came with it.
            assert_eq!(
                decoded, *button,
                "byte {byte} mask {mask:#04x} decoded extra"
            );
        }
    }

    #[test]
    fn a_resting_report_is_no_buttons_at_all() {
        assert!(decode_buttons(&report()).is_empty());
    }

    /// A resting pad, as this hardware actually sends it — captured from
    /// `/dev/hidraw` rather than written by hand, sticks a little off centre
    /// and all.
    #[test]
    fn a_captured_resting_report_is_a_pad_nobody_is_touching() {
        let mut resting = report();
        resting[1] = 0xbe; // the sequence counter
        resting[10..18].copy_from_slice(&[0x74, 0x02, 0x72, 0x01, 0x5c, 0x01, 0x73, 0x03]);
        // The gyro, which chatters all session and means nothing here.
        resting[30..38].copy_from_slice(&[0xbb, 0x99, 0x24, 0x35, 0x8d, 0x00, 0x0d, 0xf5]);

        let decoded = decode(&resting);
        assert!(decoded.buttons.is_empty(), "no button is down");
        assert_eq!(decoded.triggers, (0, 0), "no trigger is pulled");
        // Off centre in the report, and centred by the time the shell sees it.
        assert_ne!(decoded.left, (0, 0), "the raw reading keeps the drift");
        assert_eq!(normalise(decoded.left), (0.0, 0.0));
        assert_eq!(normalise(decoded.right), (0.0, 0.0));
    }

    /// The D-pad's four bits are four *different* bits. They sit in one byte
    /// next to the shoulder and the small buttons, and a transposed pair there
    /// would make the menu walk sideways when asked to go down.
    #[test]
    fn the_dpad_directions_do_not_collide() {
        let mut seen = Vec::new();
        for direction in [Buttons::UP, Buttons::DOWN, Buttons::LEFT, Buttons::RIGHT] {
            let (byte, mask, _) = BUTTON_BITS
                .iter()
                .find(|(_, _, button)| *button == direction)
                .expect("every direction is mapped");
            assert!(!seen.contains(&(byte, mask)), "two directions on one bit");
            seen.push((byte, mask));
        }

        // And pressing one is exactly one direction.
        let mut down = report();
        down[3] = 0x04;
        let decoded = decode_buttons(&down);
        assert!(decoded.has(Buttons::DOWN));
        assert!(!decoded.has(Buttons::UP));
        assert!(!decoded.has(Buttons::LEFT));
        assert!(!decoded.has(Buttons::RIGHT));
    }

    /// Several buttons at once is the normal case — the keyboard chord is two
    /// of them — and a decode that only ever found the first would break it.
    #[test]
    fn buttons_combine() {
        let mut both = report();
        both[3] = 0x40; // View
        both[2] = 0x04; // X
        let decoded = decode_buttons(&both);
        assert!(decoded.has(Buttons::VIEW));
        assert!(decoded.has(Buttons::X));
    }

    #[test]
    fn presses_are_the_edge_and_not_the_hold() {
        let down = Buttons::A;
        assert_eq!(
            down.newly_down(Buttons::empty()),
            Buttons::A,
            "first report"
        );
        assert_eq!(down.newly_down(Buttons::A), Buttons::empty(), "still held");
        assert_eq!(
            Buttons::empty().newly_down(Buttons::A),
            Buttons::empty(),
            "letting go is not a press"
        );
    }

    #[test]
    fn a_centred_stick_reads_as_centred() {
        let rest = report();
        assert_eq!(decode_stick(&rest, LEFT_STICK_X, LEFT_STICK_Y), (0, 0));
    }

    /// The offsets the capture found: each axis reads its own two bytes and
    /// leaves the next axis alone.
    ///
    /// That the fields do not overlap at all is not asserted here — it is
    /// arithmetic on constants, and it is checked where they are declared, on
    /// every build rather than on every test run.
    #[test]
    fn the_sticks_are_four_separate_little_endian_pairs() {
        for (x_at, y_at) in [(LEFT_STICK_X, LEFT_STICK_Y), (RIGHT_STICK_X, RIGHT_STICK_Y)] {
            let mut pushed = report();
            // Full deflection on x only.
            pushed[x_at..x_at + 2].copy_from_slice(&i16::MAX.to_le_bytes());
            let (x, y) = decode_stick(&pushed, x_at, y_at);
            assert_eq!(x, i16::MAX, "x reached full travel");
            assert_eq!(y, 0, "the other axis did not move");
        }
    }

    #[test]
    fn the_vertical_axis_counts_upwards() {
        let mut pushed = report();
        pushed[LEFT_STICK_Y..LEFT_STICK_Y + 2].copy_from_slice(&i16::MAX.to_le_bytes());
        let (_, y) = decode_stick(&pushed, LEFT_STICK_X, LEFT_STICK_Y);
        assert!(y > 0, "a positive raw axis is up");
    }

    /// Each trigger is its own pair of bytes, and pulling one does not move the
    /// other.
    #[test]
    fn the_triggers_are_a_pair_each_and_do_not_run_into_one_another() {
        let mut pulled = report();
        pulled[LEFT_TRIGGER..LEFT_TRIGGER + 2].copy_from_slice(&TRIGGER_FULL_SCALE.to_le_bytes());
        assert_eq!(decode(&pulled).triggers, (TRIGGER_FULL_SCALE, 0));

        let mut other = report();
        other[RIGHT_TRIGGER..RIGHT_TRIGGER + 2].copy_from_slice(&0x4000u16.to_le_bytes());
        assert_eq!(decode(&other).triggers, (0, 0x4000));

        // And neither of them is a stick, which is the neighbour they would run
        // into if an offset were edited.
        assert_eq!(decode(&pulled).left, (0, 0));

        // A reading past the end of the travel is clamped to it: the stand-in
        // told applications what this axis can send.
        let mut past = report();
        past[LEFT_TRIGGER..LEFT_TRIGGER + 2].copy_from_slice(&0xffffu16.to_le_bytes());
        assert_eq!(decode(&past).triggers.0, TRIGGER_FULL_SCALE);
    }

    /// One slow squeeze of the right trigger, as the pad actually sent it.
    ///
    /// Twenty-six samples off a capture made 2026-08-23 by pulling the trigger
    /// evenly from rest to the stop — the two bytes verbatim, evenly spaced
    /// through the pull. Kept because the layout note this was first written
    /// from said "byte 6, byte 7 is its low half", which reads exactly the
    /// wrong way round, and the code shipped that way for an afternoon.
    ///
    /// What tells the two apart is not one reading looking odd. It is that a
    /// squeeze is *monotonic*: a hand closing on a trigger only ever moves it
    /// one way. Read low byte first this climbs from rest to the stop without
    /// once going backwards; read the other way round the same bytes are noise
    /// — 24832, 61447, 26637, 46864 — which is a trigger that flutters through
    /// its whole range sixty times on the way down. Both halves are asserted,
    /// because a test that only checked the right answer would pass just as
    /// happily on a decode that had thrown the low byte away.
    #[test]
    fn a_captured_trigger_pull_is_one_sweep_and_not_a_saw() {
        const PULL: &[(u8, u8)] = &[
            (0x61, 0x00),
            (0xf0, 0x07),
            (0x68, 0x0d),
            (0xb7, 0x10),
            (0x1a, 0x15),
            (0xc2, 0x19),
            (0xd2, 0x1e),
            (0x9d, 0x23),
            (0x65, 0x27),
            (0xf8, 0x2a),
            (0x6a, 0x2e),
            (0x2e, 0x31),
            (0x91, 0x35),
            (0x67, 0x38),
            (0x06, 0x3a),
            (0xbf, 0x3e),
            (0x56, 0x43),
            (0xdf, 0x48),
            (0xb0, 0x4f),
            (0xca, 0x57),
            (0xe5, 0x5a),
            (0xcf, 0x5e),
            (0x7e, 0x65),
            (0x30, 0x6d),
            (0x17, 0x7b),
            (0xff, 0x7f),
        ];

        let pulled: Vec<u16> = PULL
            .iter()
            .map(|(low, high)| {
                let mut frame = report();
                frame[RIGHT_TRIGGER] = *low;
                frame[RIGHT_TRIGGER + 1] = *high;
                let decoded = decode(&frame);
                // The other trigger did not move while this one was pulled.
                assert_eq!(decoded.triggers.0, 0);
                decoded.triggers.1
            })
            .collect();

        for pair in pulled.windows(2) {
            assert!(
                pair[1] >= pair[0],
                "the trigger went backwards during a squeeze: {pair:?}"
            );
        }
        assert_eq!(pulled[0], 0x61, "it starts just off rest");
        assert_eq!(
            *pulled.last().expect("samples"),
            TRIGGER_FULL_SCALE,
            "and reaches the stop"
        );

        // The reading this is here to rule out. Nothing in the shell computes
        // it; it is spelled out so the test can say what wrong looks like.
        let backwards: Vec<u16> = PULL
            .iter()
            .map(|(low, high)| u16::from_be_bytes([*low, *high]))
            .collect();
        assert!(
            backwards.windows(2).any(|pair| pair[1] < pair[0]),
            "the two readings cannot be told apart on this capture, so it is \
             no longer the evidence it was kept for"
        );
    }

    /// A resting stick sits a little off centre on this hardware, and without
    /// a dead zone that is a menu that walks on its own.
    #[test]
    fn a_stick_resting_off_centre_still_reads_as_centred() {
        let drift = (i16::MAX as f32 * 0.02) as i16;
        assert_eq!(normalise((drift, 0)).0, 0.0);

        // But a real push is not swallowed.
        let push = (i16::MAX as f32 * 0.5) as i16;
        assert!(normalise((push, 0)).0 > 0.4);
    }

    #[test]
    fn the_report_is_the_one_the_pad_actually_sends() {
        // Measured off the hardware and matching the published layout: 54
        // bytes, `0x42` first. The battery and housekeeping reports the pad
        // also sends are shorter, which is what the length check rejects.
        assert_eq!(REPORT_LEN, 54);
        assert_eq!(REPORT_ID, 0x42);
        assert_ne!(REPORT_LEN, 13, "0x7b housekeeping");
        assert_ne!(REPORT_LEN, 15, "0x43 battery");
    }

    #[test]
    fn disabled_starts_nothing_at_all() {
        let mut pad = SteamPad::new(false);
        assert!(pad.poll().is_none());
        assert!(pad.shared.is_none(), "no thread, and no stand-in");
    }

    /// A driver that has found nothing has no pad to report, however long it
    /// runs.
    #[test]
    fn a_driver_that_has_heard_nothing_reports_nothing() {
        let shared = Arc::new(Shared::default());
        let mut pad = SteamPad {
            shared: Some(Arc::clone(&shared)),
        };
        assert!(pad.poll().is_none());

        // And once something has spoken, it does — and the edges are drained
        // by whoever asks first, which is what keeps a press from arriving
        // twice.
        {
            let mut state = shared.state.lock().unwrap();
            state.speaking = true;
            state.pressed = Buttons::A;
            state.held = Buttons::A;
        }
        let frame = pad.poll().expect("a pad that has spoken");
        assert_eq!(frame.pressed, Buttons::A);
        assert_eq!(frame.held, Buttons::A);
        let again = pad.poll().expect("still there");
        assert_eq!(again.pressed, Buttons::empty(), "the press was taken");
        assert_eq!(again.held, Buttons::A, "the hold was not");
    }

    #[test]
    fn two_pads_merge_rather_than_one_winning() {
        assert_eq!(Buttons::A.union(Buttons::B), Buttons(0b11));
        assert_eq!(further((0.2, 0.0), (-0.9, 0.1)), (-0.9, 0.1));
        assert_eq!(further((0.2, -0.5), (0.1, 0.4)), (0.2, -0.5));
    }

    /// The ids a node is found by are read as numbers off its `HID_ID`, on a
    /// cable and over Bluetooth alike, and they are the ids each stand-in is
    /// built with.
    #[test]
    fn a_pad_is_found_by_its_ids_on_either_bus() {
        let id = |line: &str| hid_id(&format!("DRIVER=hid-steam\nHID_ID={line}\nHID_NAME=x\n"));
        assert_eq!(
            id("0003:000028DE:00001304"),
            Some((BUS_USB, VALVE_VENDOR, 0x1304))
        );
        assert_eq!(
            id("0005:000028DE:00001303"),
            Some((BUS_BLUETOOTH, VALVE_VENDOR, 0x1303))
        );
        // A bus nothing of Valve's arrives on is not looked at.
        assert_eq!(id("0018:000028DE:00001304"), None);
        assert_eq!(hid_id("DRIVER=hid-steam\n"), None);

        for product in CONTROLLER_PRODUCTS {
            assert_eq!(
                Pad::by_ids(VALVE_VENDOR, product),
                Some(Pad::Controller(product))
            );
            assert_eq!(Pad::Controller(product).product(), product);
        }
        assert_eq!(Pad::by_ids(VALVE_VENDOR, DECK_PRODUCT), Some(Pad::Deck));
        // The 2015 controller and its receiver are left to the kernel: their
        // gamepad is `hid-steam`'s on every kernel there is, and it turns their
        // lizard mode off by itself when a program opens it.
        assert_eq!(Pad::by_ids(VALVE_VENDOR, 0x1102), None);
        assert_eq!(Pad::by_ids(VALVE_VENDOR, 0x1142), None);
        // Steam Input's own invented pad, and somebody else's.
        assert_eq!(Pad::by_ids(VALVE_VENDOR, 0x11ff), None);
        assert_eq!(Pad::by_ids(0x045e, 0x1304), None);
    }

    /// Over Bluetooth the controller sends `0x45`, the same report less the
    /// eight bytes at the end, and it reads exactly as the long one does.
    #[test]
    fn the_bluetooth_report_reads_as_the_long_one() {
        let mut long = report();
        long[2] = 0x01; // A
        long[4] = 0x01; // Steam
        long[10..12].copy_from_slice(&1234i16.to_le_bytes());
        long[16..18].copy_from_slice(&(-4321i16).to_le_bytes());
        long[8..10].copy_from_slice(&0x7fffu16.to_le_bytes());
        let mut short = [0u8; SHORT_REPORT_LEN];
        short.copy_from_slice(&long[..SHORT_REPORT_LEN]);
        short[0] = SHORT_REPORT_ID;

        let pad = Pad::Controller(0x1303);
        let read = pad.decode(&long).expect("the long report");
        assert_eq!(pad.decode(&short), Some(read));
        assert!(read.buttons.has(Buttons::A) && read.buttons.has(Buttons::STEAM));
        assert_eq!(read.left.0, 1234);
        assert_eq!(read.triggers.1, TRIGGER_FULL_SCALE);
        // Either id at the other's length is something else.
        let mut wrong = short;
        wrong[0] = REPORT_ID;
        assert!(pad.decode(&wrong).is_none());
        assert!(pad.decode(&long[..50]).is_none());
    }

    /// Which driver a pad is read through, for every case the module's notes
    /// name — the choice between the two is decided by which of them answers
    /// the first press.
    #[test]
    fn the_driver_is_whichever_answers_the_first_press() {
        let facts = |driver, lizard_mode, kernel_gamepad, held_elsewhere| Facts {
            driver,
            lizard_mode,
            kernel_gamepad,
            held_elsewhere,
        };
        // No kernel driver: the raw node, whatever else is true.
        let generic = facts(Driver::Other, None, false, false);
        assert_eq!(source(generic), Source::NoKernelDriver);
        assert!(reads_the_raw_node(generic));
        assert!(reads_the_raw_node(facts(
            Driver::Other,
            Some(false),
            false,
            true
        )));
        // hid-steam in lizard mode — its default, and the Deck's: the raw node,
        // even with the kernel's gamepad up, which is silent.
        let typing = facts(Driver::HidSteam, Some(true), true, false);
        assert_eq!(source(typing), Source::KernelKeepsItTyping);
        assert!(reads_the_raw_node(typing));
        // And a parameter that cannot be read is taken for the default.
        assert!(reads_the_raw_node(facts(
            Driver::HidSteam,
            None,
            true,
            false
        )));
        // Lizard mode off: the kernel's gamepad, and the raw node left alone.
        let kernel = facts(Driver::HidSteam, Some(false), true, false);
        assert_eq!(source(kernel), Source::KernelGamepad);
        assert!(!reads_the_raw_node(kernel));
        // Until another program opens the raw node and the gamepad goes.
        let taken = facts(Driver::HidSteam, Some(false), false, true);
        assert_eq!(source(taken), Source::KernelGamepadTaken);
        assert!(reads_the_raw_node(taken));
        // And nothing at all where there is neither: an empty receiver slot,
        // or this shell the last one holding the node, letting go so the
        // kernel can make its gamepad again.
        let empty = facts(Driver::HidSteam, Some(false), false, false);
        assert_eq!(source(empty), Source::Nothing);
        assert!(!reads_the_raw_node(empty));
    }

    /// A bitmap as sysfs prints an input device's capabilities: words of the
    /// kernel's `long`, highest first, leading zero words left out.
    #[test]
    fn a_capability_bitmap_is_read_from_its_last_word() {
        // BTN_SOUTH, 0x130, is bit 48 of the fifth word.
        assert!(bitmap_has("1000000000000 0 0 0 0", 0x130));
        assert!(!bitmap_has("1000000000000 0 0 0 0", 0x131));
        assert!(!bitmap_has("1000000000000 0 0 0", 0x130));
        assert!(bitmap_has("3", 0) && bitmap_has("3", 1) && !bitmap_has("3", 2));
        assert!(!bitmap_has("3", 64));
        assert!(!bitmap_has("", 0));
        assert!(bitmap_has("40\n", 6), "properties: the accelerometer bit");
    }

    /// The facts, read off a machine laid out the way a Steam Deck's sysfs is:
    /// the raw node on `hid-steam`'s client device, and the gamepad and its
    /// motion sensors on the real one beside it.
    #[test]
    fn a_machine_says_which_driver_has_a_node() {
        let root = std::env::temp_dir().join(format!("lxb-steam-hid-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let sys = root.join("sys");
        let proc = root.join("proc");
        let usb = sys.join("devices/usb3/3-3/3-3:1.2");
        let client = usb.join("0003:28DE:1205.0004");
        let real = usb.join("0003:28DE:1205.0003");
        let drivers = sys.join("bus/hid/drivers");
        for dir in [
            &client,
            &real,
            &drivers.join("hid-steam"),
            &drivers.join("hid-generic"),
        ] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let input = |name: &str, key: &str, abs: &str, properties: &str| {
            let at = real.join("input").join(name);
            std::fs::create_dir_all(at.join("capabilities")).unwrap();
            std::fs::write(at.join("capabilities/key"), key).unwrap();
            std::fs::write(at.join("capabilities/abs"), abs).unwrap();
            std::fs::write(at.join("properties"), properties).unwrap();
        };
        // The motion sensors: sticks of their own and no buttons.
        input("input53", "0", "3f", "40");
        let hidraw = sys.join("class/hidraw/hidraw2");
        std::fs::create_dir_all(&hidraw).unwrap();
        std::os::unix::fs::symlink(&client, hidraw.join("device")).unwrap();
        std::os::unix::fs::symlink(drivers.join("hid-steam"), client.join("driver")).unwrap();
        let params = sys.join("module/hid_steam/parameters");
        std::fs::create_dir_all(&params).unwrap();
        std::fs::write(params.join("lizard_mode"), "N\n").unwrap();
        std::fs::create_dir_all(proc.join("4242/fd")).unwrap();
        std::fs::create_dir_all(proc.join("7/fd")).unwrap();
        std::fs::create_dir_all(proc.join("self")).unwrap();

        let machine = Machine {
            sys: sys.clone(),
            proc: proc.clone(),
            me: 7,
        };
        let node = Path::new("/dev/hidraw2");
        assert_eq!(machine.lizard_mode(), Some(false));
        // No gamepad up and nobody holding the node: nothing to read.
        let facts = machine.facts(node, Some(false));
        assert_eq!(facts.driver, Driver::HidSteam);
        assert!(
            !facts.kernel_gamepad,
            "the motion sensors are not a gamepad"
        );
        assert!(!facts.held_elsewhere);
        // This program holding it is not somebody else holding it.
        std::os::unix::fs::symlink(node, proc.join("7/fd/41")).unwrap();
        assert!(!machine.facts(node, Some(false)).held_elsewhere);
        // Another program holding it is.
        std::os::unix::fs::symlink(node, proc.join("4242/fd/91")).unwrap();
        assert!(machine.facts(node, Some(false)).held_elsewhere);
        // The gamepad itself: BTN_SOUTH and ABS_X, and no accelerometer.
        input("input52", "1000000000000 0 0 0 0", "3", "0");
        let facts = machine.facts(node, Some(false));
        assert!(facts.kernel_gamepad);
        assert!(
            !facts.held_elsewhere,
            "only asked where it can change the answer"
        );
        assert_eq!(source(facts), Source::KernelGamepad);
        // Lizard mode on, and the same machine is read from the raw node.
        std::fs::write(params.join("lizard_mode"), "Y\n").unwrap();
        assert_eq!(machine.lizard_mode(), Some(true));
        assert!(reads_the_raw_node(
            machine.facts(node, machine.lizard_mode())
        ));
        // And the same node under hid-generic has no kernel driver to speak of.
        std::fs::remove_file(client.join("driver")).unwrap();
        std::os::unix::fs::symlink(drivers.join("hid-generic"), client.join("driver")).unwrap();
        let facts = machine.facts(node, Some(false));
        assert_eq!(facts.driver, Driver::Other);
        assert!(!facts.kernel_gamepad);
        assert_eq!(source(facts), Source::NoKernelDriver);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A Deck report with only `(byte, mask)` set.
    fn deck_report(set: &[(usize, u8)]) -> [u8; DECK_REPORT_LEN] {
        let mut report = [0u8; DECK_REPORT_LEN];
        report[..3].copy_from_slice(&DECK_REPORT_HEAD);
        for (byte, mask) in set {
            report[*byte] |= mask;
        }
        report
    }

    /// Every button the shell has a name for, at the bit `hid-steam` reads it
    /// from — and nothing else set by any of them.
    #[test]
    fn every_deck_button_decodes_from_the_bit_the_kernel_reads_it_at() {
        for (byte, mask, button) in DECK_BUTTON_BITS {
            let decoded = decode_deck(&deck_report(&[(*byte, *mask)])).unwrap();
            assert_eq!(decoded.buttons, *button, "byte {byte} mask {mask:#04x}");
        }
        // The kernel's table, spelled out for the ones a wrong guess would
        // hurt: A at 8.7 and B at 8.5, the Steam button at 9.5, and the Menu
        // button (☰) at 9.6.
        let at = |byte, mask| decode_deck(&deck_report(&[(byte, mask)])).unwrap().buttons;
        assert_eq!(at(8, 0x80), Buttons::A);
        assert_eq!(at(8, 0x20), Buttons::B);
        assert_eq!(at(9, 0x20), Buttons::STEAM);
        assert_eq!(at(9, 0x40), Buttons::MENU);
        // The back grips, the trackpad clicks and the quick-access button are
        // the stand-in's alone.
        for (byte, mask) in [
            (9, 0x80),
            (10, 0x01),
            (10, 0x02),
            (10, 0x04),
            (13, 0x02),
            (13, 0x04),
            (14, 0x04),
        ] {
            assert!(at(byte, mask).is_empty(), "byte {byte} mask {mask:#04x}");
        }
    }

    /// The Deck's other nodes are its firmware's keyboard and mouse, and a
    /// report of theirs is not the Deck.
    #[test]
    fn only_the_decks_own_report_is_read_as_the_deck() {
        assert!(decode_deck(&deck_report(&[])).is_some());
        // A keyboard report: eight bytes.
        assert!(decode_deck(&[0, 0, 0x28, 0, 0, 0, 0, 0]).is_none());
        // The right length with the wrong front: a report of another type.
        let mut other = deck_report(&[]);
        other[2] = 0x01;
        assert!(decode_deck(&other).is_none());
        // And the puck's report is not read on a Deck's node, nor the other
        // way round.
        assert!(Pad::Deck.decode(&report()).is_none());
        assert!(Pad::Controller(PUCK_PRODUCT)
            .decode(&deck_report(&[]))
            .is_none());
    }

    /// Sticks and triggers where `steam_deck_axis_mappings` puts them, with
    /// the vertical counting up as it does on the puck — the kernel negates it
    /// on the way to evdev, which is the stand-in's business, not the shell's.
    #[test]
    fn the_decks_sticks_and_triggers_are_where_the_kernel_reads_them() {
        let mut report = deck_report(&[]);
        report[DECK_LEFT_STICK_X..DECK_LEFT_STICK_X + 2].copy_from_slice(&1000i16.to_le_bytes());
        report[DECK_LEFT_STICK_Y..DECK_LEFT_STICK_Y + 2].copy_from_slice(&2000i16.to_le_bytes());
        report[DECK_RIGHT_STICK_X..DECK_RIGHT_STICK_X + 2]
            .copy_from_slice(&(-3000i16).to_le_bytes());
        report[DECK_RIGHT_STICK_Y..DECK_RIGHT_STICK_Y + 2]
            .copy_from_slice(&(-4000i16).to_le_bytes());
        report[DECK_LEFT_TRIGGER..DECK_LEFT_TRIGGER + 2].copy_from_slice(&0x4000u16.to_le_bytes());
        report[DECK_RIGHT_TRIGGER..DECK_RIGHT_TRIGGER + 2]
            .copy_from_slice(&0x7fffu16.to_le_bytes());
        let decoded = decode_deck(&report).unwrap();
        assert_eq!(decoded.left, (1000, 2000));
        assert_eq!(decoded.right, (-3000, -4000));
        assert_eq!(decoded.triggers, (0x4000, TRIGGER_FULL_SCALE));
        assert!(decoded.buttons.is_empty());
    }

    /// The shell reads this pad from its report; the stand-in is for everybody
    /// else. Reading both would act on every press twice.
    #[test]
    fn the_shell_knows_its_own_stand_in_when_gilrs_offers_it_one() {
        // Either pad's ids are the shell's own only while it is reading that
        // pad; otherwise they are the kernel's gamepad — the Deck's always, the
        // puck's from Linux 7.3 — which the shell reads.
        {
            let _serial = serial();
            for product in DRIVABLE {
                let bit = driven_bit(product).expect("every drivable product has a bit");
                DRIVEN.store(0, Ordering::Relaxed);
                assert!(!is_a_stand_in(Some(VALVE_VENDOR), Some(product)));
                DRIVEN.store(bit, Ordering::Relaxed);
                assert!(is_a_stand_in(Some(VALVE_VENDOR), Some(product)));
                // One pad driven is not another pad driven.
                for other in DRIVABLE.into_iter().filter(|other| *other != product) {
                    assert!(!is_a_stand_in(Some(VALVE_VENDOR), Some(other)));
                }
                DRIVEN.store(0, Ordering::Relaxed);
            }
        }
        // An Xbox pad, which the shell very much does read through GilRs.
        assert!(!is_a_stand_in(Some(0x045e), Some(0x028e)));
        // Steam Input's own invented controller, which is Valve's but is not
        // this pad.
        assert!(!is_a_stand_in(Some(VALVE_VENDOR), Some(0x11ff)));
        // And a pad GilRs could say nothing about is not assumed to be ours.
        assert!(!is_a_stand_in(None, None));
        assert!(!is_a_stand_in(Some(VALVE_VENDOR), None));
    }

    /// The driver, against the pad itself.
    ///
    /// Everything above is a decode or a lock; this is the claim the module
    /// makes — that a machine with this controller plugged into it ends up with
    /// a controller on it. Nothing in it presses a button, because nothing here
    /// can; what it checks is the part a person cannot see by looking at the
    /// pad, which is whether anything but this shell can find it at all.
    ///
    /// Skipped on a machine with no puck attached, or where `/dev/uinput`
    /// cannot be opened. See `pad_guard`'s own hardware tests.
    mod hardware_tests {
        use super::*;

        fn a_pad_is_plugged_in() -> bool {
            hidraw_nodes()
                .iter()
                .any(|(_, pad)| matches!(pad, Pad::Controller(_)))
                && std::fs::OpenOptions::new()
                    .write(true)
                    .open("/dev/uinput")
                    .is_ok()
        }

        /// Start the driver on the pad this machine has, and hand back the
        /// gamepad it made — or nothing at all, where there is no pad here to
        /// drive.
        ///
        /// Skipping is decided by *listening*, not by a directory listing. A
        /// puck that has idled out keeps every one of its hidraw nodes, so
        /// `a_pad_is_plugged_in` goes on saying yes long after the pad has
        /// stopped saying anything — and a test that took that for a pad would
        /// fail a suite because somebody put the controller down twenty minutes
        /// ago. What these tests need is a pad that is *reporting*, and the
        /// only way to know is to wait for a report.
        ///
        /// Once one has arrived the skipping stops: a pad that spoke and got no
        /// gamepad is the failure this whole module exists to prevent, and that
        /// is a panic rather than a shrug.
        fn driving() -> Option<(SteamPad, crate::pads::Pad)> {
            if !a_pad_is_plugged_in() {
                crate::skipped("no Steam Controller here, or no /dev/uinput");
                return None;
            }

            let mut pad = SteamPad::new(true);
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut heard = false;
            while Instant::now() < deadline {
                if pad.poll().is_some() {
                    heard = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            if !heard {
                crate::skipped(
                    "the Steam Controller is asleep — its hidraw nodes are here \
                     but it is sending nothing. Press a button on it and run again.",
                );
                stop(pad);
                return None;
            }

            // It spoke, so there must be a gamepad. Only udev's few tens of
            // milliseconds stand between the two.
            let deadline = Instant::now() + Duration::from_secs(6);
            while in_proc().is_none() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(50));
            }
            let listed = in_proc().expect("the pad reported and no gamepad was made for it");
            Some((pad, listed))
        }

        /// Stop a driver, and wait until its thread has let go of the machine.
        ///
        /// Dropping a [`SteamPad`] asks its thread to stop and deliberately
        /// does not wait for it, so a test that simply returned here would
        /// leave a thread still on its way out — and on its way out it
        /// publishes its own empty list of stand-ins over whatever the next
        /// test has just made. One driver at a time is a fact about the shell,
        /// which has one; in a test process it has to be arranged.
        fn stop(pad: SteamPad) {
            drop(pad);
            let deadline = Instant::now() + Duration::from_secs(6);
            while !hidapi_ignore_ids().is_empty() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
        }

        /// The stand-in this driver is currently presenting, as `/proc` has it.
        fn in_proc() -> Option<crate::pads::Pad> {
            let name = stand_in()?.name;
            crate::pads::joysticks().into_iter().find(|pad| {
                pad.vendor == VALVE_VENDOR && pad.product == PUCK_PRODUCT && pad.name == name
            })
        }

        /// The stand-in, as the library the shell itself reads controllers
        /// with sees it — which is the same library, and the same mapping
        /// database, a great many games use.
        ///
        /// Two things, and they are opposite halves of one rule. A game must
        /// find this pad as a controller with every control on it. The shell
        /// must *not* read it, because it has already read the pad exactly,
        /// from the report; taking the same presses again through a database
        /// that has never heard of this controller would act on each of them
        /// twice, and get two of the face buttons the wrong way round doing it.
        #[test]
        fn a_game_finds_the_whole_controller_and_the_shell_leaves_it_alone() {
            let _serial = serial();
            let Some((pad, listed)) = driving() else {
                return;
            };

            let gilrs = gilrs::Gilrs::new().expect("gilrs would not start");
            let (_, found) = gilrs
                .gamepads()
                .find(|(_, gamepad)| gamepad.os_name() == listed.name)
                .expect("a game reading controllers this way finds nothing");

            // Every control a controller has, named. `Mode` among them: it is
            // *declared*, which is what keeps the numbering of the buttons
            // around it right, and it is never sent.
            for button in [
                gilrs::Button::South,
                gilrs::Button::East,
                gilrs::Button::North,
                gilrs::Button::West,
                gilrs::Button::LeftTrigger,
                gilrs::Button::RightTrigger,
                gilrs::Button::Select,
                gilrs::Button::Start,
                gilrs::Button::LeftThumb,
                gilrs::Button::RightThumb,
                gilrs::Button::Mode,
            ] {
                assert!(found.button_code(button).is_some(), "no {button:?}");
                assert!(!found.is_pressed(button), "{button:?} arrived held down");
            }
            for axis in [
                gilrs::Axis::LeftStickX,
                gilrs::Axis::LeftStickY,
                gilrs::Axis::RightStickX,
                gilrs::Axis::RightStickY,
                gilrs::Axis::LeftZ,
                gilrs::Axis::RightZ,
                gilrs::Axis::DPadX,
                gilrs::Axis::DPadY,
            ] {
                assert!(found.axis_code(axis).is_some(), "no {axis:?}");
            }

            // And the shell knows this one is its own.
            assert!(is_a_stand_in(found.vendor_id(), found.product_id()));

            // As does the guard, which would otherwise grab the stand-in and
            // stand a stand-in in front of it — see [`crate::pad_guard`], whose
            // rule is that nothing virtual is ever guarded.
            assert!(
                crate::pad_guard::is_virtual(&listed.node),
                "the pad guard would take this shell's own gamepad away from applications"
            );

            stop(pad);
        }

        #[test]
        fn the_pad_becomes_a_controller_every_other_program_can_find() {
            let _serial = serial();
            let Some((mut pad, listed)) = driving() else {
                return;
            };
            // Being in that list at all is the first half: `joysticks()` keeps
            // only devices the kernel gave a `js` node, which is the same test
            // udev sets `ID_INPUT_JOYSTICK` from and what an emulator
            // enumerates by. This is the node it would then open. See
            // [`crate::pads`].
            assert!(
                listed
                    .node
                    .file_name()
                    .is_some_and(|node| node.as_encoded_bytes().starts_with(b"event")),
                "no event node: {}",
                listed.node.display()
            );
            assert!(
                crate::pads::xinput(&listed).is_some(),
                "the machine does not read it as a controller of Xbox's shape"
            );
            // And it carries the guide button, unpressed, so that SDL's count
            // of this pad's buttons is the count of a real one.
            assert!(
                listed.keys.contains(&0x13c),
                "BTN_MODE is not declared on the device the machine found"
            );

            // The pad's raw node is hidden only now that there is somewhere
            // else to read it.
            assert_eq!(hidapi_ignore_ids(), vec![(VALVE_VENDOR, PUCK_PRODUCT)]);
            assert!(crate::pad_guard::hidapi_ignore_list()
                .is_some_and(|list| list.contains("0x28de/0x1304")));

            // And the shell still has the pad, from the report, which is the
            // half the stand-in must not have cost it.
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut heard = false;
            while Instant::now() < deadline {
                if pad.poll().is_some() {
                    heard = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            assert!(heard, "the shell stopped hearing the pad it drives");

            // Dropping the driver takes the controller back off the machine,
            // rather than leaving one behind for the next program to find.
            //
            // Looked for by its node rather than by asking `stand_in()` again:
            // that answer is cleared on the way out, so it would say the pad
            // had gone whether or not it had.
            let node = listed.node.clone();
            stop(pad);
            let gone = |node: &std::path::Path| {
                !crate::pads::joysticks().iter().any(|pad| pad.node == node)
            };
            let deadline = Instant::now() + Duration::from_secs(6);
            while !gone(&node) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(50));
            }
            assert!(gone(&node), "the gamepad outlived the driver that made it");
            assert!(
                hidapi_ignore_ids().is_empty(),
                "applications are still being told to ignore a pad with nothing standing in for it"
            );
        }
    }

    /// Nothing is asked to ignore the pad's raw node until there is a gamepad
    /// standing in front of it.
    #[test]
    fn the_raw_node_is_only_hidden_once_there_is_something_else_to_read() {
        let _serial = serial();
        *STAND_INS.lock().unwrap() = Vec::new();
        assert!(hidapi_ignore_ids().is_empty());
        assert_eq!(stand_in(), None);

        *STAND_INS.lock().unwrap() = vec![
            StandInId {
                name: "Valve Software Steam Controller Puck".into(),
                vendor: VALVE_VENDOR,
                product: PUCK_PRODUCT,
            },
            // A second pad is one entry, not two: the list is read by SDL and
            // one that repeated itself would be a list read twice.
            StandInId {
                name: "Valve Software Steam Controller Puck".into(),
                vendor: VALVE_VENDOR,
                product: PUCK_PRODUCT,
            },
        ];
        assert_eq!(hidapi_ignore_ids(), vec![(VALVE_VENDOR, PUCK_PRODUCT)]);
        assert_eq!(stand_in().map(|id| id.vendor), Some(VALVE_VENDOR));

        *STAND_INS.lock().unwrap() = Vec::new();
    }
}
