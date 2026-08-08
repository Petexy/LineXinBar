//! Which pointers libinput hands the compositor, and which one is moving.
//!
//! Scratch diagnostic, for the case where a device the kernel is plainly
//! emitting events on reaches nothing on screen. The kernel's own nodes are one
//! thing and libinput's view of them is another: it opens what udev tagged,
//! groups the interfaces of one physical device together, and can decide a node
//! is not a pointer at all. Between an evdev capture and the compositor there
//! is nothing else left to be wrong.
//!
//! So this starts libinput exactly as `backend::udev` does — the same udev
//! seat, the same enumeration — lists every device it took, and then totals the
//! motion each one produces. A device missing from the list is one the
//! compositor never had; a device listed but silent is one libinput is
//! swallowing the events of.
//!
//! Run it from a session that can open `/dev/input/*`; no seat manager is
//! involved here, so nothing is taken away from a running compositor. What it
//! could *not* open is printed first and matters as much as the rest: the
//! nodes' ACLs follow the active seat, so a probe run from a terminal on
//! another session sees almost nothing, and a device absent for that reason
//! must not be read as a device libinput refused.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use smithay::reexports::input::event::pointer::Axis;
use smithay::reexports::input::event::{EventTrait as _, PointerEvent};
use smithay::reexports::input::{DeviceCapability, Event, Libinput, LibinputInterface};

/// Valve's vendor ID and the second-generation Steam Controller's product ID,
/// the pair `backend::udev` matches the lizard keyboard on.
const VALVE_VENDOR: u32 = 0x28de;
const STEAM_CONTROLLER_2: u32 = 0x1304;

const WINDOW: Duration = Duration::from_secs(20);

/// Opening devices by hand rather than through libseat: this is a diagnostic
/// run beside a session, not a compositor, and taking devices from a seat
/// manager would be taking them from whatever already holds them.
///
/// Every refusal is kept. libinput drops a device it cannot open without a
/// word, and a silently shorter device list is the one way this probe could
/// answer its own question wrongly.
#[derive(Clone, Default)]
struct Uaccess {
    refused: Arc<Mutex<Vec<(PathBuf, std::io::Error)>>>,
}

impl LibinputInterface for Uaccess {
    fn open_restricted(&mut self, path: &Path, flags: i32) -> Result<OwnedFd, i32> {
        match OpenOptions::new()
            .custom_flags(flags)
            .read(true)
            .write((flags & libc::O_RDWR) != 0 || (flags & libc::O_WRONLY) != 0)
            .open(path)
        {
            Ok(file) => {
                let fd = file.as_raw_fd();
                std::mem::forget(file);
                // SAFETY: `fd` was just opened here and is forgotten rather
                // than closed, so this is its only owner.
                Ok(unsafe { OwnedFd::from_raw_fd(fd) })
            }
            Err(err) => {
                let code = -err.raw_os_error().unwrap_or(libc::EIO);
                if let Ok(mut refused) = self.refused.lock() {
                    refused.push((path.to_path_buf(), err));
                }
                Err(code)
            }
        }
    }

    fn close_restricted(&mut self, fd: OwnedFd) {
        drop(File::from(fd));
    }
}

#[derive(Default)]
struct Motion {
    events: u64,
    dx: f64,
    dy: f64,
    scroll: f64,
    buttons: u64,
}

fn main() {
    let seat = std::env::var("XDG_SEAT").unwrap_or_else(|_| "seat0".to_string());
    let interface = Uaccess::default();
    let refused = Arc::clone(&interface.refused);
    let mut libinput = Libinput::new_with_udev(interface);
    if libinput.udev_assign_seat(&seat).is_err() {
        println!("libinput would not take seat {seat}");
        return;
    }

    // The first dispatch is where libinput reports everything it enumerated.
    let _ = libinput.dispatch();

    // The refusals first, because they bound what everything below can mean.
    let refused = refused.lock().map(|refused| refused.len()).unwrap_or(0);
    if refused > 0 {
        println!(
            "# {refused} node(s) could not be opened, and are missing from everything below.\n\
             # The ACLs on /dev/input follow the *active* seat, so this is the ordinary\n\
             # result of probing from a terminal on another session. Run it from the\n\
             # session that owns the seat before reading a short list as a verdict.\n"
        );
    }

    let mut pointers = 0;
    println!("# what libinput opened on seat {seat}\n");
    for event in libinput.by_ref() {
        let Event::Device(added) = &event else {
            continue;
        };
        let device = added.device();
        let pointer = device.has_capability(DeviceCapability::Pointer);
        let keyboard = device.has_capability(DeviceCapability::Keyboard);
        if !pointer {
            continue;
        }
        pointers += 1;
        let valve = device.id_vendor() == VALVE_VENDOR && device.id_product() == STEAM_CONTROLLER_2;
        println!(
            "  {:<48} vendor={:#06x} product={:#06x}{}{}",
            device.name(),
            device.id_vendor(),
            device.id_product(),
            if keyboard { " +keyboard" } else { "" },
            if valve {
                "   <- the Steam Controller"
            } else {
                ""
            },
        );
    }
    println!("\n{pointers} pointer(s) in all");
    if pointers == 0 {
        println!("nothing to measure — libinput opened no pointer at all");
        return;
    }

    println!(
        "\n>>> MOVE THE RIGHT TOUCHPAD for {} seconds, then your ordinary mouse as a control\n",
        WINDOW.as_secs()
    );

    let mut totals: HashMap<String, Motion> = HashMap::new();
    let end = Instant::now() + WINDOW;
    while Instant::now() < end {
        let _ = libinput.dispatch();
        for event in libinput.by_ref() {
            let Event::Pointer(pointer) = &event else {
                continue;
            };
            let name = pointer.device().name().to_string();
            let motion = totals.entry(name).or_default();
            motion.events += 1;
            match pointer {
                PointerEvent::Motion(event) => {
                    motion.dx += event.dx();
                    motion.dy += event.dy();
                }
                PointerEvent::Button(_) => motion.buttons += 1,
                PointerEvent::ScrollWheel(event) => {
                    motion.scroll += event.scroll_value_v120(Axis::Vertical);
                }
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }

    println!("# what actually arrived");
    if totals.is_empty() {
        println!("  nothing, from any device libinput opened");
    }
    let mut rows: Vec<_> = totals.into_iter().collect();
    rows.sort_by_key(|(_, motion)| std::cmp::Reverse(motion.events));
    for (name, motion) in rows {
        println!(
            "  {:<48} events={:6} dx={:9.1} dy={:9.1} scroll={:7.0} buttons={}",
            name, motion.events, motion.dx, motion.dy, motion.scroll, motion.buttons
        );
    }
}
