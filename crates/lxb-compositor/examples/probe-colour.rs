//! Print the colour pipeline every display on this machine can be driven
//! through — the five properties [`lxb::hdr::Pipeline`] is built out of.
//!
//! Scratch diagnostic: opens each DRM card read-only, without taking master or
//! committing anything, and reads exactly what the compositor reads. Nothing
//! here modesets, and nothing here changes a picture.
//!
//! What it is for: `Settings > Display > HDR > sRGB color intensity` is offered
//! only where the CRTC has both a `CTM` and a `DEGAMMA_LUT`, because a matrix
//! is a gamut conversion only when it acts on linear light. When that row says
//! the control is not available, this says which half is missing and on which
//! card.

use std::collections::HashMap;
use std::fs::File;
use std::os::fd::{AsFd, BorrowedFd};

use smithay::reexports::drm::control::{connector, crtc, Device as ControlDevice, ResourceHandle};
use smithay::reexports::drm::{ClientCapability, Device as BasicDevice};

struct Card(File);

impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}
impl BasicDevice for Card {}
impl ControlDevice for Card {}

/// Every property of one object, by name, with its current value — the same
/// shape `hdr::properties` builds, and for the same reason: a size is read from
/// the value, never from the declared range.
fn properties<T: ResourceHandle>(device: &impl ControlDevice, handle: T) -> HashMap<String, u64> {
    let Ok(set) = device.get_properties(handle) else {
        return HashMap::new();
    };
    let mut out = HashMap::new();
    for (handle, value) in set.iter() {
        let Ok(info) = device.get_property(*handle) else {
            continue;
        };
        let Ok(name) = info.name().to_str().map(str::to_owned) else {
            continue;
        };
        out.insert(name, *value);
    }
    out
}

fn main() {
    let Ok(entries) = std::fs::read_dir("/dev/dri") else {
        println!("this machine has no DRM devices to ask");
        return;
    };
    let mut cards: Vec<std::path::PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("card"))
        })
        .collect();
    cards.sort();

    for path in cards {
        let shown = path.display().to_string();
        let Ok(file) = File::open(&path) else {
            println!("{shown}: cannot be opened");
            continue;
        };
        let card = Card(file);
        // Asked for the same way the backend asks: everything the pipeline
        // sets is committed atomically, so a device that refuses this can be
        // told none of it.
        let atomic = card
            .set_client_capability(ClientCapability::Atomic, true)
            .is_ok();
        let Ok(resources) = card.resource_handles() else {
            continue;
        };
        println!("{shown}: atomic={atomic}");

        for handle in resources.crtcs() {
            let props = properties(&card, *handle);
            let has = |name: &str| props.contains_key(name);
            println!(
                "  crtc {}: DEGAMMA_LUT={} ({:?} entries) CTM={} GAMMA_LUT={} ({:?} entries)",
                u32::from(*handle),
                has("DEGAMMA_LUT"),
                props.get("DEGAMMA_LUT_SIZE"),
                has("CTM"),
                has("GAMMA_LUT"),
                props.get("GAMMA_LUT_SIZE"),
            );
        }

        for handle in resources.connectors() {
            let Ok(info) = card.get_connector(*handle, false) else {
                continue;
            };
            if info.state() != connector::State::Connected {
                continue;
            }
            let name = format!("{}-{}", info.interface().as_str(), info.interface_id());
            let props = properties(&card, *handle);
            let crtc: Option<u32> = info
                .current_encoder()
                .and_then(|encoder| card.get_encoder(encoder).ok())
                .and_then(|encoder| encoder.crtc())
                .map(|crtc: crtc::Handle| u32::from(crtc));
            println!(
                "  {name}: Colorspace={} HDR_OUTPUT_METADATA={} driven by crtc {crtc:?}",
                props.contains_key("Colorspace"),
                props.contains_key("HDR_OUTPUT_METADATA"),
            );
        }
    }
}
