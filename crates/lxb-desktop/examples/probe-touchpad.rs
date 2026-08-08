//! Where the Steam Controller's touchpads are, and whether the cursor they
//! move is coming from anywhere at all.
//!
//! Two questions at once, because the answer to the second decides whether the
//! first matters:
//!
//! * **Is the lizard mouse alive?** In lizard mode the pad presents a real USB
//!   mouse and the right touchpad drives it. Steam claiming the pad turns that
//!   off, and then the touchpad moves nothing no matter what the compositor
//!   does with it. So the pad's own evdev mouse nodes are watched directly, and
//!   the relative motion they emit is counted.
//! * **Where is the touchpad in the HID report?** If the shell is to drive the
//!   pointer itself — the way it already drives it from the right stick — the
//!   touchpad has to be decoded out of the report, which works whether or not
//!   Steam has taken the mouse away.
//!
//! Run it with Steam closed and again with Steam running. A touchpad that moves
//! the report in both, and the evdev mouse in only one, is the whole story.

use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Write as _};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const HID_ID_MATCH: &str = "0003:000028DE:00001304";
const REPORT_ID: u8 = 0x42;
const REPORT_LEN: usize = 54;

const VALVE_VENDOR: u16 = 0x28de;
const PUCK_PRODUCT: u16 = 0x1304;

/// `EVIOCGID`, which answers which device an evdev node belongs to without
/// having to trust a name or a node number.
const EVIOCGID: libc::c_ulong = 0x8008_4502;

/// `struct input_event` on a 64-bit kernel: a 16-byte timeval, then type, code
/// and value.
const INPUT_EVENT_LEN: usize = 24;
const EV_REL: u16 = 0x02;
const EV_KEY: u16 = 0x01;

const CALIBRATE: Duration = Duration::from_secs(3);
const WINDOW: Duration = Duration::from_secs(10);

fn main() {
    let label = std::env::args().nth(1).unwrap_or_else(|| "run".to_string());
    let log_path = PathBuf::from(format!("/tmp/lxb-touchpad-{label}.log"));
    let mut out = match Log::create(&log_path) {
        Ok(log) => log,
        Err(err) => {
            eprintln!("cannot write {}: {err}", log_path.display());
            return;
        }
    };

    out.line(&format!("# touchpad capture: {label}"));

    let mut mice = open_lizard_mice(&mut out);
    let mut pads = open_hidraw(&mut out);
    if pads.is_empty() {
        out.line("no pad to read — stopping.");
        return;
    }

    out.line(&format!(
        "\n=== calibrating: HANDS OFF for {}s ===",
        CALIBRATE.as_secs()
    ));
    let end = Instant::now() + CALIBRATE;
    while Instant::now() < end {
        for pad in pads.iter_mut() {
            pad.pump(false);
        }
        for mouse in mice.iter_mut() {
            mouse.pump();
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    for pad in pads.iter_mut() {
        pad.finish_calibration(&mut out);
    }

    let quiet = TerminalEcho::quiet();
    for (name, hint) in [
        (
            "RIGHT touchpad",
            "slide a finger slowly around it — circles, then edge to edge",
        ),
        ("LEFT touchpad", "the same on the left one"),
        (
            "nothing",
            "hands off the pad entirely — this is the baseline",
        ),
    ] {
        out.line(&format!("\n--- {name}: {hint}"));
        for pad in pads.iter_mut() {
            pad.begin_window();
        }
        for mouse in mice.iter_mut() {
            mouse.begin_window();
        }

        let end = Instant::now() + WINDOW;
        let mut shown = u64::MAX;
        while Instant::now() < end {
            let left = (end - Instant::now()).as_secs() + 1;
            if left != shown {
                shown = left;
                print!("\r    {name}: {left}s   ");
                let _ = std::io::stdout().flush();
            }
            for pad in pads.iter_mut() {
                pad.pump(true);
            }
            for mouse in mice.iter_mut() {
                mouse.pump();
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        print!("\r{:44}\r", "");
        let _ = std::io::stdout().flush();

        for mouse in mice.iter_mut() {
            mouse.report(&mut out);
        }
        if mice.is_empty() {
            out.line("    (no evdev mouse node for the pad at all)");
        }
        for pad in pads.iter_mut().filter(|pad| pad.reports > 0) {
            pad.report(&mut out);
        }
    }
    drop(quiet);

    println!("\nwritten to {}", log_path.display());
}

/// The pad's lizard-mode mouse, as the kernel exposes it.
struct Mouse {
    path: PathBuf,
    file: File,
    motion: u64,
    buttons: u64,
}

impl Mouse {
    fn begin_window(&mut self) {
        self.motion = 0;
        self.buttons = 0;
    }

    fn pump(&mut self) {
        let mut buf = [0u8; INPUT_EVENT_LEN * 32];
        loop {
            match self.file.read(&mut buf) {
                Ok(0) => break,
                Ok(len) => {
                    for chunk in buf[..len].chunks_exact(INPUT_EVENT_LEN) {
                        let kind = u16::from_ne_bytes([chunk[16], chunk[17]]);
                        match kind {
                            EV_REL => self.motion += 1,
                            EV_KEY => self.buttons += 1,
                            _ => {}
                        }
                    }
                }
                Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    }

    fn report(&self, out: &mut Log) {
        out.line(&format!(
            "    evdev {}: {} relative-motion events, {} button events{}",
            self.path.display(),
            self.motion,
            self.buttons,
            if self.motion == 0 {
                "   <== THE LIZARD MOUSE IS SILENT"
            } else {
                ""
            }
        ));
    }
}

struct Pad {
    path: PathBuf,
    file: File,
    last: [u8; REPORT_LEN],
    primed: bool,
    /// What moves while nobody is touching the pad: the sequence counter and
    /// the sticks' own jitter.
    noise: [u8; REPORT_LEN],
    calibrated: bool,
    low: [u8; REPORT_LEN],
    high: [u8; REPORT_LEN],
    /// A few raw samples from the middle of the window, which is what shows the
    /// *shape* of a coordinate rather than just how far it travelled.
    samples: Vec<String>,
    last_sample: Option<Instant>,
    reports: u64,
    dead: bool,
}

impl Pad {
    fn finish_calibration(&mut self, out: &mut Log) {
        self.calibrated = true;
        let noisy: Vec<String> = self
            .noise
            .iter()
            .enumerate()
            .filter(|(_, mask)| **mask != 0)
            .map(|(index, mask)| format!("byte{index}={mask:#04x}"))
            .collect();
        out.line(&format!(
            "  {}: {} reports, ignoring {}",
            self.path.display(),
            self.reports,
            if noisy.is_empty() {
                "nothing".to_string()
            } else {
                noisy.join(" ")
            }
        ));
    }

    fn begin_window(&mut self) {
        self.low = [0xff; REPORT_LEN];
        self.high = [0; REPORT_LEN];
        self.samples.clear();
        self.last_sample = None;
    }

    fn pump(&mut self, live: bool) {
        let mut buf = [0u8; 64];
        loop {
            match self.file.read(&mut buf) {
                Ok(len) if len == REPORT_LEN && buf[0] == REPORT_ID => {
                    self.observe(&buf[..REPORT_LEN], live)
                }
                Ok(len) if len > 0 => continue,
                Ok(_) => break,
                Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                Err(_) => {
                    self.dead = true;
                    break;
                }
            }
        }
    }

    fn observe(&mut self, report: &[u8], live: bool) {
        self.reports += 1;
        if !self.primed {
            self.last.copy_from_slice(report);
            self.primed = true;
            return;
        }
        for (index, byte) in report.iter().enumerate() {
            if !self.calibrated {
                self.noise[index] |= byte ^ self.last[index];
            } else if live {
                self.low[index] = self.low[index].min(*byte);
                self.high[index] = self.high[index].max(*byte);
            }
        }
        if live {
            let due = self
                .last_sample
                .is_none_or(|at| at.elapsed() > Duration::from_millis(700));
            if due && self.samples.len() < 8 {
                self.last_sample = Some(Instant::now());
                self.samples.push(hex(&report[18..34]));
            }
        }
        self.last.copy_from_slice(report);
    }

    fn report(&self, out: &mut Log) {
        // Bytes the sticks and the counter own are not the touchpad's, and they
        // move constantly; saying so every window buries the answer.
        let mut moved: Vec<(usize, u8, u8)> = self
            .low
            .iter()
            .zip(self.high.iter())
            .enumerate()
            .filter(|(index, (low, high))| self.noise[*index] == 0 && **high > **low)
            .map(|(index, (low, high))| (index, *low, *high))
            .collect();
        moved.sort_by_key(|(_, low, high)| std::cmp::Reverse(*high as i16 - *low as i16));

        if moved.is_empty() {
            out.line("    hidraw: no quiet byte moved at all");
        } else {
            let listed: Vec<String> = moved
                .iter()
                .take(10)
                .map(|(index, low, high)| format!("byte{index}={low:#04x}..{high:#04x}"))
                .collect();
            out.line(&format!("    hidraw moved: {}", listed.join(" ")));
        }
        for sample in &self.samples {
            out.line(&format!("      bytes18..33: {sample}"));
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Every evdev node the puck presents as a mouse.
fn open_lizard_mice(out: &mut Log) -> Vec<Mouse> {
    let mut mice = Vec::new();
    out.line("\nlizard-mode mouse nodes:");
    let Ok(entries) = std::fs::read_dir("/dev/input") else {
        return mice;
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("event"))
        })
        .collect();
    paths.sort();

    for path in paths {
        let Ok(file) = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)
        else {
            continue;
        };
        let mut id = [0u16; 4];
        // Safety: EVIOCGID writes four u16 into the buffer given, and the fd is
        // a live evdev node this call owns.
        let ok = unsafe { libc::ioctl(file.as_raw_fd(), EVIOCGID, id.as_mut_ptr()) } == 0;
        if !ok || id[1] != VALVE_VENDOR || id[2] != PUCK_PRODUCT {
            continue;
        }
        // Only the mouse halves: the keyboard half is the buttons, which the
        // report already carries.
        if !is_mouse(&path) {
            continue;
        }
        out.line(&format!("  {} opened", path.display()));
        mice.push(Mouse {
            path,
            file,
            motion: 0,
            buttons: 0,
        });
    }
    if mice.is_empty() {
        out.line("  (none)");
    }
    mice
}

/// Whether udev tagged this node as a mouse, which is the same question
/// libinput asks before deciding it drives a pointer.
fn is_mouse(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let uevent = format!("/sys/class/input/{name}/device/uevent");
    // The node is a mouse when its sibling `mouseN` exists, which the kernel
    // only creates for devices that emit relative motion.
    std::fs::read_to_string(&uevent).is_ok()
        && std::fs::read_dir(format!("/sys/class/input/{name}/device"))
            .map(|entries| {
                entries.flatten().any(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| name.starts_with("mouse"))
                })
            })
            .unwrap_or(false)
}

struct Log {
    file: File,
}

impl Log {
    fn create(path: &Path) -> std::io::Result<Self> {
        Ok(Self {
            file: File::create(path)?,
        })
    }

    fn line(&mut self, text: &str) {
        println!("{text}");
        let _ = writeln!(self.file, "{text}");
        let _ = self.file.flush();
    }
}

/// Stops the terminal echoing: in lizard mode the pad types while it is used.
struct TerminalEcho {
    saved: Option<libc::termios>,
}

impl TerminalEcho {
    fn quiet() -> Self {
        // Safety: `termios` is plain data, and both calls are given a live fd
        // and a pointer to a local of the right type.
        unsafe {
            if libc::isatty(libc::STDIN_FILENO) != 1 {
                return Self { saved: None };
            }
            let mut term: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut term) != 0 {
                return Self { saved: None };
            }
            let saved = term;
            term.c_lflag &= !(libc::ECHO | libc::ICANON);
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &term);
            Self { saved: Some(saved) }
        }
    }
}

impl Drop for TerminalEcho {
    fn drop(&mut self) {
        let Some(saved) = self.saved else {
            return;
        };
        // Safety: restoring settings this type read from the same fd.
        unsafe {
            libc::tcflush(libc::STDIN_FILENO, libc::TCIFLUSH);
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &saved);
        }
    }
}

fn open_hidraw(out: &mut Log) -> Vec<Pad> {
    let mut pads = Vec::new();
    let Ok(entries) = std::fs::read_dir("/sys/class/hidraw") else {
        return pads;
    };
    let mut nodes: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| is_puck(&entry.path()))
        .map(|entry| Path::new("/dev").join(entry.file_name()))
        .filter(|node| node.exists())
        .collect();
    nodes.sort();

    out.line("\nhidraw nodes for the puck:");
    for path in nodes {
        match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)
        {
            Ok(file) => {
                out.line(&format!("  {} opened", path.display()));
                pads.push(Pad {
                    path,
                    file,
                    last: [0; REPORT_LEN],
                    primed: false,
                    noise: [0; REPORT_LEN],
                    calibrated: false,
                    low: [0xff; REPORT_LEN],
                    high: [0; REPORT_LEN],
                    samples: Vec::new(),
                    last_sample: None,
                    reports: 0,
                    dead: false,
                });
            }
            Err(err) => out.line(&format!("  {} CANNOT OPEN: {err}", path.display())),
        }
    }
    pads
}

fn is_puck(sysfs: &Path) -> bool {
    std::fs::read_to_string(sysfs.join("device/uevent"))
        .map(|uevent| {
            uevent
                .lines()
                .any(|line| line.strip_prefix("HID_ID=") == Some(HID_ID_MATCH))
        })
        .unwrap_or(false)
}
