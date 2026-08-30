//! The controllers on this machine, in the order another program will find
//! them.
//!
//! Not for reading a pad — that is [`crate::controller`], and it is GilRs's
//! job. This is for *naming* one to somebody else: an emulator binds one device
//! to each player port, and which device ends up on player one decides whether
//! the pad in somebody's hands does anything at all.
//!
//! ## Why the shell has to answer this
//!
//! Because on a machine running this shell there is more than one of every pad.
//! The guide button is taken away from applications by grabbing the pad and
//! standing a copy of it in the pad's place — see [`crate::pad_guard`] — so
//! every real controller appears twice: the original, which is grabbed and
//! therefore silent to everything that is not this shell, and the copy, which
//! is the one that works. They have the same name, the same vendor and the same
//! product id, so nothing downstream can tell them apart. And with Steam
//! running there is a third: Steam Input mirrors each pad as a virtual Xbox
//! controller of its own.
//!
//! Four devices, one controller, and an emulator that binds the first one it
//! finds to player one. What it finds first is the grabbed original, which is
//! the whole of why a game answers nothing.
//!
//! ## The order
//!
//! `libudev` hands its caller an enumeration sorted by sysfs path, and the
//! emulator takes them in that order, so the order is knowable here — and it
//! *is* the answer, because it is what an index in a config file counts. Read
//! from `/proc/bus/input/devices`, which is one file, world-readable, and says
//! everything needed: the name, the ids, the sysfs path and which nodes the
//! kernel gave the device.

use std::cmp::Ordering;
use std::path::PathBuf;

/// Where the kernel lists every input device it has.
const DEVICES: &str = "/proc/bus/input/devices";

/// One controller, as the thing another program will open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pad {
    /// The event node, which is what an emulator opens.
    pub node: PathBuf,
    /// Where it hangs in `/sys`, which is what the order is by.
    pub sysfs: String,
    pub name: String,
    pub vendor: u16,
    pub product: u16,
    /// Every key code the device says it has, lowest first.
    pub keys: Vec<u16>,
    /// Every absolute axis code it says it has, lowest first. Hats are in
    /// here too — they are absolute axes to the kernel.
    pub axes: Vec<u16>,
}

/// Valve's vendor id, and the one product id every controller Steam Input
/// invents answers to. See [`Pad::is_steam_input`].
const STEAM_INPUT_VENDOR: u16 = 0x28de;
const STEAM_INPUT_PRODUCT: u16 = 0x11ff;

/// The first code the kernel calls a button rather than a key.
const BTN_MISC: u16 = 0x100;
/// The first and last of the four hats, which are absolute axes with two
/// positions and a middle.
const ABS_HAT0X: u16 = 0x10;
const ABS_HAT3Y: u16 = 0x17;
/// The end of the axes an emulator counts.
const ABS_MISC: u16 = 0x28;

impl Pad {
    /// The number an emulator counts this button as.
    ///
    /// Which is its place in the device's own list of buttons, lowest code
    /// first, starting from zero — and *only* the codes the kernel calls
    /// buttons. A pad that also reports keys below [`BTN_MISC`], as some
    /// report a `back` key, does not shift its buttons by them.
    ///
    /// `None` for a code this device never said it had, which is the answer
    /// that matters: a mapping database can name a control the device does not
    /// have, and a number invented for one would be a number belonging to
    /// another button.
    pub fn button(&self, code: u16) -> Option<u32> {
        if code < BTN_MISC {
            return None;
        }
        let mut at = 0;
        for &have in &self.keys {
            if have < BTN_MISC {
                continue;
            }
            if have == code {
                return Some(at);
            }
            at += 1;
        }
        None
    }

    /// The number an emulator counts this axis as.
    ///
    /// The same counting, over the absolute axes — except that the four hats
    /// are left out of it, because an emulator names those a third way again
    /// (see [`hat`]), and so is everything from [`ABS_MISC`] up, which it does
    /// not read at all.
    pub fn axis(&self, code: u16) -> Option<u32> {
        let counted = |code: u16| code < ABS_MISC && !(ABS_HAT0X..=ABS_HAT3Y).contains(&code);
        if !counted(code) {
            return None;
        }
        let mut at = 0;
        for &have in &self.axes {
            if !counted(have) {
                continue;
            }
            if have == code {
                return Some(at);
            }
            at += 1;
        }
        None
    }

    /// Whether this is one of the controllers Steam Input makes up.
    ///
    /// Steam does not pass a pad through. It takes the real one over and puts
    /// a virtual Xbox controller of its own on the machine in front of it,
    /// which is what everything else then sees. Every one of them has Valve's
    /// vendor id and this single product id, whatever the pad behind it is and
    /// whatever it calls itself.
    pub fn is_steam_input(&self) -> bool {
        self.vendor == STEAM_INPUT_VENDOR && self.product == STEAM_INPUT_PRODUCT
    }

    /// Whether the device really has this control at all.
    pub fn has(&self, at: At) -> bool {
        match at {
            At::Key(code) => self.keys.contains(&code),
            At::Axis(code) => self.axes.contains(&code),
        }
    }
}

/// Which hat an absolute axis is, and whether it is the across one.
///
/// The hats are `ABS_HAT0X`, `ABS_HAT0Y`, `ABS_HAT1X` and so on, in pairs, so
/// the pair is the code above the first divided by two and the axis within it
/// is whether the code is even.
pub fn hat(code: u16) -> Option<(u16, bool)> {
    if !(ABS_HAT0X..=ABS_HAT3Y).contains(&code) {
        return None;
    }
    let from_first = code - ABS_HAT0X;
    Some((from_first / 2, from_first % 2 == 0))
}

/// Where one of a controller's controls is, as the code the kernel sends for
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum At {
    /// A key: a face button, a shoulder, a stick pressed in.
    Key(u16),
    /// An absolute axis: a stick, an analogue trigger, or one axis of a hat.
    Axis(u16),
}

/// One of the controls a mapping database can name on a pad.
///
/// Named for where a control *is* rather than what is printed on it, which is
/// the only way of naming them that survives crossing between two pads: the
/// button below the thumb is [`Control::South`] on all of them, and whether
/// that button says `A`, `B` or a cross is a fact about one make.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    South,
    East,
    North,
    West,
    LeftBumper,
    RightBumper,
    LeftTrigger,
    RightTrigger,
    Select,
    Start,
    LeftStick,
    RightStick,
    DPadUp,
    DPadDown,
    DPadLeft,
    DPadRight,
    /// The across axis of the hat the D-pad is, where it is one.
    DPadX,
    /// And the up-and-down one.
    DPadY,
    LeftX,
    LeftY,
    RightX,
    RightY,
}

/// What a mapping database says one pad's controls are.
///
/// Empty for a pad no database knows — which is not the same as a pad with no
/// controls, and is the reason this is worth having at all. See
/// [`crate::controller::ControllerInput::mapping`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Mapping {
    at: Vec<(Control, At)>,
}

impl Mapping {
    pub fn new(at: Vec<(Control, At)>) -> Self {
        Self { at }
    }

    /// Where that control is on this pad, if the database named it.
    pub fn at(&self, control: Control) -> Option<At> {
        self.at
            .iter()
            .find(|(which, _)| *which == control)
            .map(|(_, at)| *at)
    }
}

/// The codes an XInput controller sends, which are not the names the kernel
/// gives them.
///
/// The kernel calls `0x133` `BTN_NORTH` and `0x134` `BTN_WEST`, meaning the
/// button on the top and the button on the left. On a controller of Xbox's
/// shape that is exactly backwards: its driver has sent `0x133` for the button
/// marked X, which is on the *left*, since before those names existed, and
/// `0x134` for Y, which is on the top. Sony's driver sends the same two codes
/// the other way round, and means the kernel's names by them.
///
/// So the codes are named here for the shape they belong to rather than for
/// what the kernel calls them, because that is the fact that decides where
/// somebody's jump lands. RetroArch has known this all along: its own profile
/// for a controller of this shape puts `0x133` on the button it calls `y`,
/// which is its left one.
const XBOX_WEST: u16 = 0x133;
const XBOX_NORTH: u16 = 0x134;
const BTN_SOUTH: u16 = 0x130;
const BTN_EAST: u16 = 0x131;
const BTN_TL: u16 = 0x136;
const BTN_TR: u16 = 0x137;
/// The shoulder buttons *under* the bumpers. An Xbox controller has no such
/// buttons — its triggers are axes and nothing else — and a PlayStation one
/// has both, which is what tells the two shapes apart. See [`xinput`].
const BTN_TL2: u16 = 0x138;
const BTN_TR2: u16 = 0x139;
const BTN_SELECT: u16 = 0x13a;
const BTN_START: u16 = 0x13b;
const BTN_THUMBL: u16 = 0x13d;
const BTN_THUMBR: u16 = 0x13e;
const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_Z: u16 = 0x02;
const ABS_RX: u16 = 0x03;
const ABS_RY: u16 = 0x04;
const ABS_RZ: u16 = 0x05;
const ABS_HAT0Y: u16 = 0x11;

/// Where the controls are on a controller of Xbox's shape, worked out from
/// what the device itself says it has.
///
/// This is the answer for every XInput controller, and it needs no database at
/// all — which is the point of it. A mapping database only knows the pads
/// somebody has already added to it, and there is always a controller newer
/// than the list: RetroArch's own has no entry for an 8BitDo Pro 3, and a pad
/// nothing recognises gets no buttons whatsoever. But an XInput controller is
/// not a pad with an unknown layout. It is a pad with *the* layout — the one
/// its driver has been required to send since the first Xbox controller — and
/// a device that declares that set of codes has told us where everything is.
///
/// ## Being sure it really is one
///
/// Only the codes are visible here, and a PlayStation controller declares very
/// nearly the same ones while meaning the opposite by two of them, so guessing
/// wrong swaps the button somebody jumps with for the button beside it. Two
/// things separate them, and both are required:
///
/// * The triggers are *axes* — `ABS_Z` and `ABS_RZ` — because Xbox triggers
///   are analogue and always have been.
/// * There are no [`BTN_TL2`] or [`BTN_TR2`] buttons. Sony's driver reports
///   its triggers as an axis *and* a button; Xbox's has never had the button.
///
/// A pad failing either test is left alone rather than guessed at, which is
/// the same answer this gives any pad it does not recognise: no lines, and
/// RetroArch decides for itself exactly as it does today.
///
/// `None` for anything that is not one.
pub fn xinput(pad: &Pad) -> Option<Mapping> {
    // Everything a controller of this shape always has. A pad missing any of
    // it is not one, and is not worth a guess.
    let required = [
        At::Key(BTN_SOUTH),
        At::Key(BTN_EAST),
        At::Key(XBOX_WEST),
        At::Key(XBOX_NORTH),
        At::Key(BTN_TL),
        At::Key(BTN_TR),
        At::Key(BTN_SELECT),
        At::Key(BTN_START),
        At::Axis(ABS_X),
        At::Axis(ABS_Y),
        At::Axis(ABS_Z),
        At::Axis(ABS_RX),
        At::Axis(ABS_RY),
        At::Axis(ABS_RZ),
        At::Axis(ABS_HAT0X),
        At::Axis(ABS_HAT0Y),
    ];
    if !required.iter().all(|at| pad.has(*at)) {
        return None;
    }
    // And the two buttons that say it is somebody else's shape.
    if pad.has(At::Key(BTN_TL2)) || pad.has(At::Key(BTN_TR2)) {
        return None;
    }

    // The stick presses and the guide button are not required above, because a
    // pad without them is still one of these; they are named here and dropped
    // below if this one has not got them.
    let mut at = vec![
        (Control::South, At::Key(BTN_SOUTH)),
        (Control::East, At::Key(BTN_EAST)),
        (Control::West, At::Key(XBOX_WEST)),
        (Control::North, At::Key(XBOX_NORTH)),
        (Control::LeftBumper, At::Key(BTN_TL)),
        (Control::RightBumper, At::Key(BTN_TR)),
        (Control::Select, At::Key(BTN_SELECT)),
        (Control::Start, At::Key(BTN_START)),
        (Control::LeftStick, At::Key(BTN_THUMBL)),
        (Control::RightStick, At::Key(BTN_THUMBR)),
        (Control::LeftTrigger, At::Axis(ABS_Z)),
        (Control::RightTrigger, At::Axis(ABS_RZ)),
        (Control::LeftX, At::Axis(ABS_X)),
        (Control::LeftY, At::Axis(ABS_Y)),
        (Control::RightX, At::Axis(ABS_RX)),
        (Control::RightY, At::Axis(ABS_RY)),
        (Control::DPadX, At::Axis(ABS_HAT0X)),
        (Control::DPadY, At::Axis(ABS_HAT0Y)),
    ];
    at.retain(|(_, code)| pad.has(*code));
    Some(Mapping::new(at))
}

/// Every controller on this machine, in the order a `libudev` enumeration
/// hands them over.
///
/// Which is the order an emulator numbers them in, and therefore the order the
/// indices written into its configuration count in.
pub fn joysticks() -> Vec<Pad> {
    let listing = std::fs::read_to_string(DEVICES).unwrap_or_else(|err| {
        tracing::warn!(%err, "could not read the list of input devices");
        String::new()
    });
    parse(&listing)
}

/// The same, from a listing already read. Split out for the tests, which have
/// a real machine's listing to read rather than a machine.
fn parse(listing: &str) -> Vec<Pad> {
    let mut pads: Vec<Pad> = Vec::new();
    for block in listing.split("\n\n") {
        if let Some(pad) = one(block) {
            pads.push(pad);
        }
    }
    pads.sort_by(|a, b| before(&a.sysfs, &b.sysfs));
    pads
}

/// One device out of the listing, if it is a controller at all.
fn one(block: &str) -> Option<Pad> {
    let mut name = None;
    let mut sysfs = None;
    let mut vendor = 0;
    let mut product = 0;
    let mut event = None;
    let mut joystick = false;
    let mut keys = Vec::new();
    let mut axes = Vec::new();

    for line in block.lines() {
        let (kind, rest) = line.split_at(line.find(':')? + 1);
        let rest = rest.trim();
        match kind {
            "I:" => {
                for field in rest.split_whitespace() {
                    let Some((key, value)) = field.split_once('=') else {
                        continue;
                    };
                    let value = u16::from_str_radix(value, 16).unwrap_or(0);
                    match key {
                        "Vendor" => vendor = value,
                        "Product" => product = value,
                        _ => {}
                    }
                }
            }
            "N:" => {
                name = rest
                    .strip_prefix("Name=")
                    .map(|name| name.trim_matches('"').to_string());
            }
            "S:" => sysfs = rest.strip_prefix("Sysfs=").map(str::to_string),
            "H:" => {
                for handler in rest.strip_prefix("Handlers=")?.split_whitespace() {
                    if handler.starts_with("event") {
                        event = Some(handler.to_string());
                    }
                    // What says this is a controller rather than a keyboard or
                    // a lid switch. The kernel gives a joystick node to what it
                    // reads as a pad, which is the same test `udev` sets
                    // `ID_INPUT_JOYSTICK` from — and that property is what the
                    // emulator enumerates by.
                    joystick |= handler.starts_with("js");
                }
            }
            "B:" => {
                if let Some(bitmap) = rest.strip_prefix("KEY=") {
                    keys = codes(bitmap);
                } else if let Some(bitmap) = rest.strip_prefix("ABS=") {
                    axes = codes(bitmap);
                }
            }
            _ => {}
        }
    }

    let (name, sysfs, event) = (name?, sysfs?, event?);
    if !joystick {
        return None;
    }
    Some(Pad {
        node: PathBuf::from("/dev/input").join(&event),
        // The event node's own path, not the device's: that is what is
        // enumerated, and `input52/event259` and `input52/js0` do not sort to
        // the same place.
        sysfs: format!("{sysfs}/{event}"),
        name,
        vendor,
        product,
        keys,
        axes,
    })
}

/// The codes a `B:` line says a device has, lowest first.
///
/// The kernel prints these as a row of machine words in hexadecimal, the
/// highest word first, with words above the last set bit left out and nothing
/// padded — so the *last* word is always the lowest one, and counting back
/// from the end is the only way to read them. A word is as wide as this
/// machine's, which is the width the kernel wrote them at.
fn codes(bitmap: &str) -> Vec<u16> {
    let width = usize::BITS as usize;
    let words: Vec<&str> = bitmap.split_whitespace().collect();
    let mut codes = Vec::new();
    for (from_end, word) in words.iter().rev().enumerate() {
        let Ok(bits) = u64::from_str_radix(word, 16) else {
            continue;
        };
        for bit in 0..width.min(64) {
            if bits & (1 << bit) == 0 {
                continue;
            }
            let Ok(code) = u16::try_from(from_end * width + bit) else {
                continue;
            };
            codes.push(code);
        }
    }
    codes.sort_unstable();
    codes
}

/// Which of two sysfs paths `libudev` hands over first.
///
/// Component by component, each compared as bytes — which is not the same as
/// comparing the whole path as one string, and is not numeric either: `input10`
/// comes before `input9`, because `1` is before `9`. Both of those are how the
/// enumeration actually behaves, and an order that disagreed with it would put
/// somebody's controller on the wrong player.
fn before(a: &str, b: &str) -> Ordering {
    let mut left = a.split('/');
    let mut right = b.split('/');
    loop {
        match (left.next(), right.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(one), Some(two)) => match one.cmp(two) {
                Ordering::Equal => continue,
                other => return other,
            },
        }
    }
}

/// The pads to put on the player ports, best first.
///
/// The answer is a list of *positions* in `pads`, because that is what an
/// emulator's configuration counts in — see the module note.
///
/// Four rules, in this order:
///
/// 1. **The one in somebody's hands leads**, if the shell has seen a hand on
///    one. It is the whole point: player one is the pad being held.
/// 2. **Every other live pad follows**, in the order they were found.
/// 3. **Steam Input's inventions are left out**, when there is a real pad to
///    play on. See below.
/// 4. **The grabbed originals come last.** They are the pads this shell holds
///    the guide button of, and a grabbed device delivers nothing to anybody
///    else — so a player port bound to one is a player that cannot move. They
///    are still listed, because a port left empty is a port an emulator may
///    fill by itself with exactly the device this was avoiding.
///
/// ## Why Steam's controllers are left out
///
/// Steam Input does not pass a controller through; it takes the real one over
/// and stands a virtual Xbox pad in front of it. So while Steam is running,
/// every pad on the machine is on it twice over, and the copy is a device
/// whose behaviour depends on a layer nobody here controls — a Steam profile
/// somebody set for some other game can remap it, hold buttons back, or send
/// a keyboard instead of a pad. An emulator binding one device per player has
/// no way to prefer the real one, and the shell does, so it does.
///
/// The exception is the controller Steam Input is the *only* driver for. A
/// Steam Controller has no kernel driver at all — nothing but Steam can read
/// it, and its virtual pad is not a duplicate of anything but the whole of it.
/// So the inventions are kept when there is no real pad behind them, and when
/// the pad somebody is actually holding is one of them; dropping those would
/// hand a game no controller rather than the wrong one.
pub fn order(pads: &[Pad], grabbed: &[PathBuf], in_hand: Option<&InHand>) -> Vec<usize> {
    let held = |pad: &Pad| grabbed.iter().any(|node| node == &pad.node);
    let mut real: Vec<usize> = Vec::new();
    let mut invented: Vec<usize> = Vec::new();
    let mut silent: Vec<usize> = Vec::new();
    for (at, pad) in pads.iter().enumerate() {
        if held(pad) {
            silent.push(at);
        } else if pad.is_steam_input() {
            invented.push(at);
        } else {
            real.push(at);
        }
    }

    // Whether the hand the shell last saw was on one of Steam's, which is what
    // a controller only Steam can read looks like from here.
    let holding_an_invented_one = in_hand.is_some_and(|hand| {
        invented
            .iter()
            .any(|at| hand.is(&pads[*at]) && !real.iter().any(|at| hand.is(&pads[*at])))
    });
    let mut live = real;
    if live.is_empty() || holding_an_invented_one {
        live.extend(invented);
    }

    // The pad in hand, found among the ones being handed over by what GilRs can
    // say about it. A grabbed original answers to the same name and ids as the
    // copy standing in for it, which is exactly why it is looked for among the
    // live ones only.
    if let Some(hand) = in_hand {
        if let Some(at) = live.iter().position(|at| hand.is(&pads[*at])) {
            let first = live.remove(at);
            live.insert(0, first);
        }
    }

    live.into_iter().chain(silent).collect()
}

/// What the shell knows about the pad somebody last touched.
///
/// Names and ids rather than a device node, because that is all GilRs will say
/// — see [`crate::controller::ControllerInput::in_hand`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InHand {
    pub name: String,
    pub vendor: Option<u16>,
    pub product: Option<u16>,
}

impl InHand {
    /// Whether this is that pad.
    ///
    /// The ids where the pad has them, because a name is a string somebody
    /// chose and two pads of one model share it. The name is the fallback for a
    /// pad whose ids GilRs never learned, and is compared loosely at the ends:
    /// the kernel's name for a device and the name a mapping database gives it
    /// differ by a trailing word often enough to matter — "8BitDo Ultimate 2
    /// Wireless" against "8BitDo Ultimate 2 Wireless Controller".
    fn is(&self, pad: &Pad) -> bool {
        if let (Some(vendor), Some(product)) = (self.vendor, self.product) {
            return vendor == pad.vendor && product == pad.product;
        }
        let (mine, theirs) = (self.name.trim(), pad.name.trim());
        !mine.is_empty() && (theirs.starts_with(mine) || mine.starts_with(theirs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This machine, with the shell running on it: one 8BitDo plugged in, and
    /// four controllers in `/proc`. Captured rather than invented — every
    /// number in it is one this shell has to get right.
    const REAL: &str = "\
I: Bus=0003 Vendor=2dc8 Product=310b Version=0100
N: Name=\"8BitDo Ultimate 2 Wireless Controller\"
P: Phys=usb-0000:10:00.0-4/input0
S: Sysfs=/devices/pci0000:00/0000:00:08.1/0000:10:00.0/usb5/5-4/5-4:1.0/0003:2DC8:310B.000A/input/input52
U: Uniq=
H: Handlers=event259 js0
B: PROP=0
B: EV=20000b
B: KEY=7cdb000000000000 0 0 0 0
B: ABS=3003f

I: Bus=0003 Vendor=28de Product=11ff Version=0001
N: Name=\"Microsoft X-Box 360 pad 1\"
P: Phys=
S: Sysfs=/devices/virtual/input/input60
U: Uniq=
H: Handlers=event262 js1
B: PROP=0
B: EV=20000b
B: KEY=7cdb000000000000 0 0 0 0
B: ABS=3003f

I: Bus=0003 Vendor=2dc8 Product=310b Version=0100
N: Name=\"8BitDo Ultimate 2 Wireless Controller\"
P: Phys=
S: Sysfs=/devices/virtual/input/input61
U: Uniq=
H: Handlers=event263 js2
B: PROP=0
B: EV=20000b
B: KEY=7cdb000000000000 0 0 0 0
B: ABS=3003f

I: Bus=0003 Vendor=28de Product=11ff Version=0001
N: Name=\"Microsoft X-Box 360 pad 2\"
P: Phys=
S: Sysfs=/devices/virtual/input/input62
U: Uniq=
H: Handlers=event264 js3
B: PROP=0
B: EV=20000b
B: KEY=7cdb000000000000 0 0 0 0
B: ABS=3003f

I: Bus=0019 Vendor=0000 Product=0005 Version=0000
N: Name=\"Lid Switch\"
P: Phys=PNP0C0D/button/input0
S: Sysfs=/devices/LNXSYSTM:00/LNXSYBUS:00/PNP0C0D:00/input/input1
U: Uniq=
H: Handlers=event1
B: PROP=0
";

    /// The same machine, with one controller that also reports a key from
    /// below the buttons — a `back` key, which several pads have and which
    /// would shift every button on the pad if it were counted as one. The
    /// bitmap is the one above with bit 158 set.
    const WITH_A_KEY: &str = "\
I: Bus=0003 Vendor=2dc8 Product=310b Version=0100
N: Name=\"8BitDo Ultimate 2 Wireless Controller\"
P: Phys=usb-0000:10:00.0-4/input0
S: Sysfs=/devices/virtual/input/input52
U: Uniq=
H: Handlers=event259 js0
B: PROP=0
B: EV=20000b
B: KEY=7cdb000000000000 0 40000000 0 0
B: ABS=3003f
";

    /// The numbers an emulator counts this machine's controllers' buttons in.
    ///
    /// Checked against RetroArch's own answer for a pad of this shape: every
    /// number below is one out of the profile it ships for `Microsoft X-Box
    /// 360 pad`, which is the layout all four of these report.
    #[test]
    fn a_pads_buttons_are_numbered_the_way_an_emulator_will_number_them() {
        let pads = parse(REAL);
        let pad = &pads[0];
        assert_eq!(pad.button(0x130), Some(0), "the button under the thumb");
        assert_eq!(pad.button(0x131), Some(1), "the one to the right of it");
        assert_eq!(pad.button(0x133), Some(2), "the left-hand one");
        assert_eq!(pad.button(0x134), Some(3), "the top one");
        assert_eq!(pad.button(0x136), Some(4), "the left shoulder");
        assert_eq!(pad.button(0x137), Some(5), "the right shoulder");
        assert_eq!(pad.button(0x13a), Some(6), "select");
        assert_eq!(pad.button(0x13b), Some(7), "start");
        assert_eq!(pad.button(0x13c), Some(8), "the guide button");
        assert_eq!(pad.button(0x13d), Some(9), "the left stick pressed in");
        assert_eq!(pad.button(0x13e), Some(10), "the right stick pressed in");
        assert_eq!(
            pad.button(0x220),
            None,
            "a D-pad button this pad has not got"
        );
    }

    /// The axes are numbered the same way, and the hat is not one of them —
    /// an emulator names a hat a third way again.
    #[test]
    fn a_pads_axes_are_numbered_with_its_hat_left_out() {
        let pads = parse(REAL);
        let pad = &pads[0];
        assert_eq!(pad.axis(0x00), Some(0), "the left stick, across");
        assert_eq!(pad.axis(0x01), Some(1), "the left stick, down");
        assert_eq!(pad.axis(0x02), Some(2), "the left trigger");
        assert_eq!(pad.axis(0x03), Some(3), "the right stick, across");
        assert_eq!(pad.axis(0x04), Some(4), "the right stick, down");
        assert_eq!(pad.axis(0x05), Some(5), "the right trigger");
        assert_eq!(pad.axis(0x10), None, "the hat, across");
        assert_eq!(pad.axis(0x11), None, "the hat, down");
    }

    /// A key from below the buttons is read — the device really has it — and
    /// counted as none of them, which is what an emulator does with it. A pad
    /// whose `back` key moved its face buttons along one would answer every
    /// press with the button beside the one that was pressed.
    #[test]
    fn a_key_from_below_the_buttons_moves_none_of_them() {
        let pads = parse(WITH_A_KEY);
        let pad = &pads[0];
        assert!(pad.has(At::Key(158)), "the key is there to be read");
        assert_eq!(pad.button(158), None, "and is not one of the buttons");
        assert_eq!(pad.button(0x130), Some(0), "which still start at nought");
        assert_eq!(pad.button(0x13e), Some(10));
    }

    /// The four hats, each a pair of axes: the across one first.
    #[test]
    fn the_hats_come_in_pairs() {
        assert_eq!(hat(0x10), Some((0, true)));
        assert_eq!(hat(0x11), Some((0, false)));
        assert_eq!(hat(0x12), Some((1, true)));
        assert_eq!(hat(0x17), Some((3, false)));
        assert_eq!(hat(0x0f), None, "the axis below the first hat");
        assert_eq!(hat(0x18), None, "and the one above the last");
    }

    /// A mapping answers for the controls it was given and nothing else.
    #[test]
    fn a_mapping_says_only_what_it_was_told() {
        let mapping = Mapping::new(vec![
            (Control::South, At::Key(0x130)),
            (Control::LeftTrigger, At::Axis(0x02)),
        ]);
        assert_eq!(mapping.at(Control::South), Some(At::Key(0x130)));
        assert_eq!(mapping.at(Control::LeftTrigger), Some(At::Axis(0x02)));
        assert_eq!(mapping.at(Control::North), None);
    }

    /// The four controllers, in the order an emulator will find them — and
    /// nothing that is not a controller.
    #[test]
    fn the_pads_come_back_in_the_order_they_are_enumerated_in() {
        let pads = parse(REAL);
        let names: Vec<&str> = pads.iter().map(|pad| pad.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "8BitDo Ultimate 2 Wireless Controller",
                "Microsoft X-Box 360 pad 1",
                "8BitDo Ultimate 2 Wireless Controller",
                "Microsoft X-Box 360 pad 2",
            ],
            "the pad on the bus comes before anything virtual"
        );
        assert_eq!(pads[0].node, PathBuf::from("/dev/input/event259"));
        assert_eq!(pads[0].vendor, 0x2dc8);
        assert_eq!(pads[0].product, 0x310b);
        assert!(
            pads.iter().all(|pad| pad.name != "Lid Switch"),
            "a lid is not a controller"
        );
    }

    /// And that order is the one the emulator reported: with nothing said about
    /// it, this machine put the copy of the 8BitDo on player three — which is
    /// position 2, counting from zero, in exactly this list.
    #[test]
    fn the_order_matches_what_the_emulator_did() {
        let pads = parse(REAL);
        assert_eq!(pads[2].name, "8BitDo Ultimate 2 Wireless Controller");
        assert_eq!(pads[2].node, PathBuf::from("/dev/input/event263"));
    }

    /// The pad in hand leads, and the grabbed original — the one that answers
    /// to the same name and cannot answer anything else — goes last.
    #[test]
    fn the_pad_in_somebodys_hands_is_player_one() {
        let pads = parse(REAL);
        let grabbed = vec![PathBuf::from("/dev/input/event259")];
        let hand = InHand {
            name: "8BitDo Ultimate 2 Wireless".to_string(),
            vendor: Some(0x2dc8),
            product: Some(0x310b),
        };

        // Two of the four are Steam's, and there is a real pad to play on, so
        // they are not handed over at all. What is left is the copy standing in
        // for the pad in hand, and the grabbed original of it last.
        let players = order(&pads, &grabbed, Some(&hand));
        assert_eq!(players, vec![2, 0]);
        assert_eq!(pads[players[0]].node, PathBuf::from("/dev/input/event263"));

        // Nobody has touched anything yet, and the answer is the same: the one
        // live pad, then the silent one.
        assert_eq!(order(&pads, &grabbed, None), vec![2, 0]);

        // And a machine where the guard took nothing keeps both real pads in
        // the order they were found, Steam's two still left out.
        assert_eq!(order(&pads, &[], Some(&hand)), vec![0, 2]);
    }

    /// A controller only Steam can read is still handed over, because leaving
    /// it out would hand the game nothing.
    ///
    /// A Steam Controller has no kernel driver: the invented pad is not a
    /// duplicate of a real one, it is the only thing there is. The rule is
    /// about preferring the real pad, so with no real pad there is nothing to
    /// prefer.
    #[test]
    fn the_only_pad_there_is_is_handed_over_even_if_steam_made_it() {
        let pads = parse(REAL);
        // Both 8BitDos gone; only Steam's two remain.
        let steam: Vec<Pad> = pads.into_iter().filter(Pad::is_steam_input).collect();
        assert_eq!(steam.len(), 2);
        assert_eq!(order(&steam, &[], None), vec![0, 1]);

        // And so is the one somebody is holding, even beside a real pad: it is
        // the pad they picked up, and the real one is not the pad they meant.
        let pads = parse(REAL);
        let hand = InHand {
            name: "Microsoft X-Box 360 pad 2".to_string(),
            vendor: Some(0x28de),
            product: Some(0x11ff),
        };
        let players = order(&pads, &[], Some(&hand));
        assert!(pads[players[0]].is_steam_input(), "{players:?}");
    }

    /// A pad GilRs could only name is matched by name, at either end of it:
    /// the kernel's name for a device and a mapping database's differ by a
    /// trailing word often enough to matter.
    #[test]
    fn a_pad_with_no_ids_is_found_by_name() {
        let pads = parse(REAL);
        let named = |name: &str| InHand {
            name: name.to_string(),
            vendor: None,
            product: None,
        };
        assert!(named("8BitDo Ultimate 2 Wireless").is(&pads[0]));
        assert!(named("8BitDo Ultimate 2 Wireless Controller").is(&pads[0]));
        assert!(!named("8BitDo Pro 3").is(&pads[0]));
        assert!(!named("").is(&pads[0]));
    }

    /// The paths sort the way the enumeration does, which is by component and
    /// by bytes — not as one string, and not by number.
    #[test]
    fn the_order_is_the_enumerations_own() {
        assert_eq!(
            before("/devices/pci0000:00/x", "/devices/virtual/y"),
            Ordering::Less
        );
        // Bytes, not numbers: this is the enumeration's own answer, and a
        // "sensible" numeric sort here would be a different order from the one
        // the indices are counted in.
        assert_eq!(
            before(
                "/devices/virtual/input/input100",
                "/devices/virtual/input/input60"
            ),
            Ordering::Less
        );
        // Component by component: a shorter path is not a smaller string once
        // the separator is what differs.
        assert_eq!(
            before("/devices/virtual/input", "/devices/virtual/input/input1"),
            Ordering::Less
        );
    }
}
