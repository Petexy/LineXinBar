//! Print what every connected display reports, the way the compositor reads it.
//!
//! Scratch diagnostic: opens each DRM card read-only, without taking master or
//! touching a single property, and prints the connector's mode list exactly as
//! `lxb` snapshots it. Nothing here modesets or commits.

use std::fs::File;
use std::os::fd::{AsFd, BorrowedFd};

use smithay::reexports::drm::control::{connector, Device as ControlDevice, ModeTypeFlags};
use smithay::reexports::drm::Device as BasicDevice;

struct Card(File);

impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}
impl BasicDevice for Card {}
impl ControlDevice for Card {}

fn main() {
    // Whatever this machine has, discovered rather than guessed: a card index
    // is not something anybody may assume, here least of all.
    let Ok(devices) = std::fs::read_dir("/dev/dri") else {
        println!("this machine has no DRM devices to ask");
        return;
    };
    let mut cards: Vec<std::path::PathBuf> = devices
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("card"))
        })
        .collect();
    cards.sort();

    for path in cards {
        let path = path.display().to_string();
        let Ok(file) = File::open(&path) else {
            continue;
        };
        let card = Card(file);
        let Ok(resources) = card.resource_handles() else {
            continue;
        };
        for handle in resources.connectors() {
            // `false`: the cached list, which is what the compositor's scanner
            // hands it. `true` below, for comparison.
            for forced in [false, true] {
                let Ok(info) = card.get_connector(*handle, forced) else {
                    println!("{path}: connector {handle:?} could not be read (forced={forced})");
                    continue;
                };
                if info.state() != connector::State::Connected {
                    continue;
                }
                let name = format!("{}-{}", info.interface().as_str(), info.interface_id());
                println!(
                    "\n== {path} {name} (forced probe: {forced}) — {} modes",
                    info.modes().len()
                );
                for mode in info.modes() {
                    let converted = smithay::output::Mode::from(*mode);
                    println!(
                        "   {:>5}x{:<5} {:>7.3} Hz{}",
                        converted.size.w,
                        converted.size.h,
                        converted.refresh as f64 / 1000.0,
                        if mode.mode_type().contains(ModeTypeFlags::PREFERRED) {
                            "  (preferred)"
                        } else {
                            ""
                        }
                    );
                }
            }
        }
    }
}
