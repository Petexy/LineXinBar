//! A second-generation Steam Controller, read from hidraw.
//!
//! That pad has no kernel gamepad driver. `hid-steam` claims `1102`, `1142` and
//! `1205` — the original controller, its receiver, and the Deck — so the puck's
//! `1304` falls through to `hid-generic` and stays in the firmware's lizard
//! mode: a mouse and a keyboard, and no joystick node at all. GilRs enumerates
//! nothing, which leaves [`crate::controller`] with no pad to map.
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
//! Opening hidraw read-only does not disturb lizard mode; leaving it means
//! *writing* feature reports, which is Steam's business and not ours. The
//! duplicate that read-only leaves behind — lizard mode still typing the same
//! buttons as keys — is settled in the compositor, which drops the pad's
//! keyboard so this is the only path in.

use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Valve's vendor ID and the puck's product ID, as `HID_ID` spells them.
const HID_ID_MATCH: &str = "0003:000028DE:00001304";

/// The pad's input report: `0x42` in the first byte, 54 bytes long.
const REPORT_ID: u8 = 0x42;
const REPORT_LEN: usize = 54;

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

    const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// What is down here and was not down in `before`.
    const fn newly_down(self, before: Self) -> Self {
        Self(self.0 & !before.0)
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

/// Whether the report's vertical axes count upwards.
///
/// The shell's convention is positive-up, as every gamepad API normalises it,
/// and Valve's pads report the same way — `hid-steam` negates their Y to get
/// evdev's positive-down. If vertical navigation or the pointer ever comes out
/// inverted on this pad, this is the single line to flip; the D-pad is decoded
/// digitally and is unaffected either way.
const STICK_Y_IS_UP: bool = true;

/// How far off centre a stick has to sit before it is believed.
///
/// The sticks rest a little away from zero — measured around 2% of full travel
/// on this hardware, and it is not the same offset on each axis. Everything
/// inside this is reported as centred, which keeps a resting pad from slowly
/// walking the menu on its own.
const STICK_DEADZONE: f32 = 0.08;

/// One poll's worth of the pad.
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
}

/// Reads every second-generation pad the machine has.
pub struct SteamPad {
    devices: Vec<Device>,
    next_scan: Option<Duration>,
    /// Whether the "found one" line has already been logged, so a pad that
    /// disconnects and comes back does not narrate itself every two seconds.
    announced: bool,
}

struct Device {
    path: PathBuf,
    file: File,
    /// The buttons at the last report, so presses are reported as the edges
    /// they are rather than once per report for as long as one is held.
    held: Buttons,
    left_stick: (f32, f32),
    right_stick: (f32, f32),
    /// Whether this node has ever sent an input report. The dongle presents one
    /// interface per pad slot and four of them stay silent all session.
    speaking: bool,
}

impl SteamPad {
    /// Watches for pads, or does nothing at all when controller input is off.
    pub fn new(enabled: bool) -> Self {
        Self {
            devices: Vec::new(),
            // `None` means "scan on the first poll"; every later scan is
            // scheduled from the clock the caller passes in.
            next_scan: if enabled { None } else { Some(Duration::MAX) },
            announced: false,
        }
    }

    /// What the pad is doing, or `None` when there is no pad to ask.
    ///
    /// Drains every pending report: at 8 ms between polls a pad sending a
    /// report every 4 ms would otherwise fall steadily further behind, and the
    /// buttons would answer late by however long the session had been running.
    pub fn poll(&mut self, now: Duration) -> Option<Frame> {
        self.rescan_if_due(now);

        let mut frame = Frame::default();
        let mut heard = false;
        let mut lost = Vec::new();
        let mut buf = [0u8; 64];

        for (index, device) in self.devices.iter_mut().enumerate() {
            loop {
                match device.file.read(&mut buf) {
                    Ok(0) => break,
                    Ok(len) => {
                        // Reports that are not the input report are the pad's
                        // own housekeeping — battery on `0x43`, and `0x7b`
                        // every half second. Only `0x42` carries the buttons.
                        if len != REPORT_LEN || buf[0] != REPORT_ID {
                            continue;
                        }
                        device.speaking = true;
                        let held = decode_buttons(&buf[..REPORT_LEN]);
                        frame.pressed = frame.pressed.union(held.newly_down(device.held));
                        frame.released = frame.released.union(device.held.newly_down(held));
                        device.held = held;
                        device.left_stick =
                            decode_stick(&buf[..REPORT_LEN], LEFT_STICK_X, LEFT_STICK_Y);
                        device.right_stick =
                            decode_stick(&buf[..REPORT_LEN], RIGHT_STICK_X, RIGHT_STICK_Y);
                    }
                    Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                    Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                    Err(err) => {
                        // Unplugged, almost always. Drop it and let the next
                        // scan pick the pad up if it comes back.
                        tracing::debug!(
                            path = %device.path.display(),
                            %err,
                            "steam controller hidraw closed"
                        );
                        lost.push(index);
                        break;
                    }
                }
            }

            if !device.speaking {
                continue;
            }
            heard = true;
            // Two pads plugged in are merged rather than one being chosen, so
            // a session is drivable from either.
            frame.held = frame.held.union(device.held);
            frame.left_stick = further(frame.left_stick, device.left_stick);
            frame.right_stick = further(frame.right_stick, device.right_stick);
        }

        for index in lost.into_iter().rev() {
            self.devices.remove(index);
        }
        heard.then_some(frame)
    }

    fn rescan_if_due(&mut self, now: Duration) {
        match self.next_scan {
            // Disabled outright.
            Some(deadline) if deadline == Duration::MAX => return,
            Some(deadline) if now < deadline => return,
            _ => {}
        }
        self.next_scan = Some(now + RESCAN_INTERVAL);

        for path in hidraw_nodes() {
            if self.devices.iter().any(|device| device.path == path) {
                continue;
            }
            match OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&path)
            {
                Ok(file) => {
                    if !self.announced {
                        self.announced = true;
                        tracing::info!(
                            "Steam Controller 2 found; reading it from hidraw because the \
                             kernel has no gamepad driver for it"
                        );
                    }
                    tracing::debug!(path = %path.display(), "steam controller hidraw opened");
                    self.devices.push(Device {
                        path,
                        file,
                        held: Buttons::empty(),
                        left_stick: (0.0, 0.0),
                        right_stick: (0.0, 0.0),
                        speaking: false,
                    });
                }
                Err(err) => {
                    // Losing this is not fatal on a machine with a keyboard,
                    // but it is the whole controller now rather than one
                    // button, and it is invisible without a word here.
                    tracing::warn!(
                        path = %path.display(),
                        %err,
                        "cannot read Steam Controller hidraw; the pad will not work"
                    );
                }
            }
        }
    }
}

/// Which buttons a report says are down.
fn decode_buttons(report: &[u8]) -> Buttons {
    let mut buttons = Buttons::empty();
    for (byte, mask, button) in BUTTON_BITS {
        if report[*byte] & mask != 0 {
            buttons = buttons.union(*button);
        }
    }
    buttons
}

/// One stick, as `(x, y)` in −1.0..=1.0 with y positive up.
fn decode_stick(report: &[u8], x_at: usize, y_at: usize) -> (f32, f32) {
    let x = axis(report, x_at);
    let y = axis(report, y_at);
    let y = if STICK_Y_IS_UP { y } else { -y };
    (deadzone(x), deadzone(y))
}

/// One signed 16-bit little-endian axis, normalised.
fn axis(report: &[u8], at: usize) -> f32 {
    let raw = i16::from_le_bytes([report[at], report[at + 1]]);
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

/// Whichever of two stick positions is further from rest, axis by axis.
fn further(current: (f32, f32), candidate: (f32, f32)) -> (f32, f32) {
    let pick = |a: f32, b: f32| if b.abs() > a.abs() { b } else { a };
    (pick(current.0, candidate.0), pick(current.1, candidate.1))
}

/// Every hidraw node belonging to a second-generation Steam Controller.
///
/// Matched on `HID_ID` from sysfs rather than on the device name, which is a
/// string the firmware picks, or on the node number, which is whatever order
/// the machine happened to enumerate its USB devices in.
fn hidraw_nodes() -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir("/sys/class/hidraw") else {
        return Vec::new();
    };
    let mut nodes: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| is_steam_controller(&entry.path()))
        .map(|entry| Path::new("/dev").join(entry.file_name()))
        .filter(|node| node.exists())
        .collect();
    // The dongle presents one interface per pad slot, and `read_dir` is in no
    // particular order. Sorting only makes the logs reproducible.
    nodes.sort();
    nodes
}

fn is_steam_controller(sysfs: &Path) -> bool {
    let Ok(uevent) = std::fs::read_to_string(sysfs.join("device/uevent")) else {
        return false;
    };
    uevent
        .lines()
        .any(|line| line.strip_prefix("HID_ID=") == Some(HID_ID_MATCH))
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
        assert_eq!(decode_stick(&rest, LEFT_STICK_X, LEFT_STICK_Y), (0.0, 0.0));
    }

    /// The offsets the capture found, and the fact that the four axes are four
    /// distinct 16-bit fields rather than overlapping ones.
    #[test]
    fn the_sticks_are_four_separate_little_endian_pairs() {
        for (x_at, y_at) in [(LEFT_STICK_X, LEFT_STICK_Y), (RIGHT_STICK_X, RIGHT_STICK_Y)] {
            let mut pushed = report();
            // Full deflection on x only.
            pushed[x_at..x_at + 2].copy_from_slice(&i16::MAX.to_le_bytes());
            let (x, y) = decode_stick(&pushed, x_at, y_at);
            assert!((x - 1.0).abs() < 1e-3, "x reached full travel: {x}");
            assert_eq!(y, 0.0, "the other axis did not move");
        }

        // And the two sticks do not share a byte.
        assert!(LEFT_STICK_Y >= LEFT_STICK_X + 2);
        assert!(RIGHT_STICK_X >= LEFT_STICK_Y + 2);
        assert!(RIGHT_STICK_Y >= RIGHT_STICK_X + 2);
        assert!(RIGHT_STICK_Y + 2 <= REPORT_LEN);
    }

    #[test]
    fn the_vertical_axis_counts_upwards() {
        let mut pushed = report();
        pushed[LEFT_STICK_Y..LEFT_STICK_Y + 2].copy_from_slice(&i16::MAX.to_le_bytes());
        let (_, y) = decode_stick(&pushed, LEFT_STICK_X, LEFT_STICK_Y);
        assert!(y > 0.0, "a positive raw axis is up");
    }

    /// A resting stick sits a little off zero on this hardware, and without a
    /// dead zone that is a menu that walks on its own.
    #[test]
    fn a_stick_resting_off_centre_still_reads_as_centred() {
        let drift = (i16::MAX as f32 * 0.02) as i16;
        let mut resting = report();
        resting[LEFT_STICK_X..LEFT_STICK_X + 2].copy_from_slice(&drift.to_le_bytes());
        assert_eq!(decode_stick(&resting, LEFT_STICK_X, LEFT_STICK_Y).0, 0.0);

        // But a real push is not swallowed.
        let push = (i16::MAX as f32 * 0.5) as i16;
        let mut pushed = report();
        pushed[LEFT_STICK_X..LEFT_STICK_X + 2].copy_from_slice(&push.to_le_bytes());
        assert!(decode_stick(&pushed, LEFT_STICK_X, LEFT_STICK_Y).0 > 0.4);
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
    fn disabled_never_scans() {
        let mut pad = SteamPad::new(false);
        assert!(pad.poll(Duration::ZERO).is_none());
        assert!(pad.poll(Duration::from_secs(3600)).is_none());
        assert!(pad.devices.is_empty(), "nothing opened while disabled");
    }

    /// A node that has never sent a report is not a pad.
    ///
    /// This is what keeps the dongle's four silent interfaces — one per pad
    /// slot, and empty until a puck actually pairs to one — from being taken
    /// for a controller that is there.
    ///
    /// Built from `/dev/null` rather than by asking the machine what it has
    /// plugged in: the box this was written on has a puck on it, so a test
    /// meaning "nothing is there" would otherwise pass or fail depending on
    /// which machine ran it, and on whether a report happened to be waiting.
    #[test]
    fn a_node_that_has_never_spoken_is_not_a_pad() {
        let mut pad = SteamPad::new(false);
        pad.devices.push(Device {
            path: PathBuf::from("/dev/null"),
            file: File::open("/dev/null").expect("/dev/null is always openable"),
            held: Buttons::empty(),
            left_stick: (0.0, 0.0),
            right_stick: (0.0, 0.0),
            speaking: false,
        });

        assert!(
            pad.poll(Duration::ZERO).is_none(),
            "a silent node is no pad"
        );
        // And still nothing later: silence is not a state that expires.
        assert!(pad.poll(Duration::from_secs(3600)).is_none());
    }

    #[test]
    fn two_pads_merge_rather_than_one_winning() {
        assert_eq!(Buttons::A.union(Buttons::B), Buttons(0b11));
        assert_eq!(further((0.2, 0.0), (-0.9, 0.1)), (-0.9, 0.1));
        assert_eq!(further((0.2, -0.5), (0.1, 0.4)), (0.2, -0.5));
    }

    #[test]
    fn the_hid_id_is_the_puck_and_not_the_pads_the_kernel_drives() {
        assert_eq!(HID_ID_MATCH, "0003:000028DE:00001304");
        // The three `hid-steam` already claims. Reading those here would be
        // duplicate input, because the kernel gives them a real gamepad node.
        for driven in ["00001102", "00001142", "00001205"] {
            assert!(!HID_ID_MATCH.ends_with(driven), "{driven}");
        }
    }
}
