//! The Steam button on a second-generation Steam Controller, read from hidraw.
//!
//! That pad has no kernel gamepad driver. `hid-steam` claims `1102`, `1142` and
//! `1205` — the original controller, its receiver, and the Deck — so the puck's
//! `1304` falls through to `hid-generic` and stays in the firmware's lizard
//! mode: a mouse and a keyboard, and no joystick node at all. GilRs enumerates
//! nothing, which leaves [`crate::controller`] with no button to map.
//!
//! Most of the pad still works anyway, because lizard mode is a *real* USB
//! keyboard and those keys reach the shell through the compositor the way any
//! keyboard's do. The Steam button is the one control with no keyboard
//! spelling: the firmware reports it only in the HID input report, which
//! nothing was reading. So this reads it, and does nothing else — every other
//! button already arrives by a path that works.
//!
//! Opening hidraw is read-only and does not disturb lizard mode; taking the pad
//! out of it means *writing* feature reports, which is Steam's business and not
//! ours. So this costs the buttons that currently work nothing.

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

/// Where the Steam button lives in that report — byte 4, bit 0.
///
/// Confirmed two ways, because a bit read off one capture is a guess: against
/// the published `TritonButtons` layout, and against this pad, where it is the
/// only bit that moves when the button is pressed and nothing else does.
const STEAM_BYTE: usize = 4;
const STEAM_MASK: u8 = 0x01;

/// How often to look for a pad that was not there last time.
///
/// The dongle is a USB device the user can plug in mid-session, and four of its
/// five interfaces stay silent until a puck actually pairs to one. Rescanning
/// is a directory listing, so the interval only has to be short enough that
/// plugging a pad in feels like it worked.
const RESCAN_INTERVAL: Duration = Duration::from_secs(2);

/// Reads the Steam button off every second-generation pad the machine has.
pub struct SteamButton {
    devices: Vec<Device>,
    next_scan: Option<Duration>,
    /// Whether the "found one" line has already been logged, so a pad that
    /// disconnects and comes back does not narrate itself every two seconds.
    announced: bool,
}

struct Device {
    path: PathBuf,
    file: File,
    /// Whether the button was down at the last report, so the press is reported
    /// as the edge it is rather than once per report for as long as it is held.
    held: bool,
}

impl SteamButton {
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

    /// Whether the Steam button went down since the last poll.
    ///
    /// Drains every pending report: at 8 ms between polls a pad sending a
    /// report every 4 ms would otherwise fall steadily further behind, and the
    /// button would answer late by however long the session had been running.
    pub fn pressed(&mut self, now: Duration) -> bool {
        self.rescan_if_due(now);

        let mut pressed = false;
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
                        let down = buf[STEAM_BYTE] & STEAM_MASK != 0;
                        // The press only, never the release: the guide overlay
                        // is a toggle, and answering both edges would open it
                        // on the way down and close it on the way back up.
                        pressed |= down && !device.held;
                        device.held = down;
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
        }

        for index in lost.into_iter().rev() {
            self.devices.remove(index);
        }
        pressed
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
                            "Steam Controller 2 found; reading its Steam button from hidraw \
                             because the kernel has no gamepad driver for it"
                        );
                    }
                    tracing::debug!(path = %path.display(), "steam controller hidraw opened");
                    self.devices.push(Device {
                        path,
                        file,
                        held: false,
                    });
                }
                Err(err) => {
                    // Losing this is not fatal — it costs the Steam button and
                    // nothing else — but it is invisible without a word here,
                    // which is exactly how the bug that prompted this looked.
                    tracing::warn!(
                        path = %path.display(),
                        %err,
                        "cannot read Steam Controller hidraw; its Steam button will not work"
                    );
                }
            }
        }
    }
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

    /// The one number that matters, spelled out so a careless edit to the
    /// constants has to break a test that names the button.
    #[test]
    fn the_steam_button_is_byte_four_bit_zero() {
        let mut report = [0u8; REPORT_LEN];
        report[0] = REPORT_ID;
        assert!(report[STEAM_BYTE] & STEAM_MASK == 0, "released at rest");

        report[4] = 0x01;
        assert!(report[STEAM_BYTE] & STEAM_MASK != 0, "0x04 bit 0 is Steam");

        // The bits either side of it are other buttons — L4 and, in the byte
        // before, the face buttons — and none of them is the Steam button.
        let mut neighbours = [0u8; REPORT_LEN];
        neighbours[0] = REPORT_ID;
        neighbours[2] = 0xff; // A, B, X, Y, QAM, R3, View, R4
        neighbours[3] = 0xff; // R5, RB, the D-pad, Menu, L3
        neighbours[4] = 0xfe; // everything in Steam's byte except Steam
        neighbours[5] = 0xff; // the left-hand touch and click bits
        assert!(neighbours[STEAM_BYTE] & STEAM_MASK == 0);
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
        let mut button = SteamButton::new(false);
        assert!(!button.pressed(Duration::ZERO));
        assert!(!button.pressed(Duration::from_secs(3600)));
        assert!(button.devices.is_empty(), "nothing opened while disabled");
    }

    /// A machine with no such pad is the normal case, and it has to cost
    /// nothing and say nothing.
    #[test]
    fn absent_hardware_is_quiet() {
        let mut button = SteamButton::new(true);
        assert!(!button.pressed(Duration::ZERO));
    }

    /// The bug the edge tracking exists for: the pad repeats its report
    /// continuously, so a held button is present in every one of them. Reported
    /// as a level rather than an edge it would toggle the guide overlay at the
    /// poll rate for as long as a thumb rested on it.
    #[test]
    fn a_held_button_is_one_press() {
        let mut held = false;
        let mut presses = 0;
        for down in [false, true, true, true, false, false, true, false] {
            if down && !held {
                presses += 1;
            }
            held = down;
        }
        assert_eq!(presses, 2, "two distinct presses, not five reports");
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
