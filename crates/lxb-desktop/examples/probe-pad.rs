//! Which bit in a pad's HID report is which button, established by counting.
//!
//! The shell reads the second-generation Steam Controller two ways: through
//! GilRs, which needs a kernel gamepad node the puck does not have, and — for
//! its Steam button — straight off hidraw. When a button stops working the
//! question is which of those paths went quiet, and Steam changes both of them
//! the moment it claims the pad. So this watches both.
//!
//! Nothing about the report's layout is assumed. Two things make the answer
//! survive a pad that is never really still:
//!
//! * A pad at rest is still talking — a sequence counter, and analogue axes
//!   that jitter a digit or two. The run opens by watching an untouched pad and
//!   calls everything that moves noise.
//! * A pad in a hand is *worse*: the grip touch sensors and the gyro chatter
//!   continuously, and they will out-shout the button in any capture that only
//!   asks what changed. So each control is pressed a **fixed number of times**
//!   and the bit that went down exactly that many times is the button. Noise
//!   does not count to three.
//!
//! Run it twice — once with Steam closed, once with Steam running — and compare
//! the two logs. Both are written to a file, because the interesting part is
//! the comparison and a terminal scrollback is not a record.

use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Write as _};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Valve's vendor ID and the puck's product ID, as `HID_ID` spells them. Kept
/// in step with `crate::steam_hid`, which this exists to check.
const HID_ID_MATCH: &str = "0003:000028DE:00001304";
const REPORT_ID: u8 = 0x42;
const REPORT_LEN: usize = 54;

/// How long to sit still at the start working out what the pad does on its own.
const CALIBRATE: Duration = Duration::from_secs(3);

/// How many times each control is pressed. Three is enough to be unmistakable
/// among chatter and few enough to stay easy to count by hand.
const PRESSES: u32 = 3;

/// Long enough to find the next control before its window opens.
const READY: Duration = Duration::from_secs(3);

/// How long each control has. Three unhurried presses fit in six seconds; a
/// stick has four directions to be pushed in, so it gets longer.
const WINDOW: Duration = Duration::from_secs(6);
const ANALOGUE_WINDOW: Duration = Duration::from_secs(9);

/// Every control the shell has a use for, in the order they are asked for.
///
/// `analogue` controls are not counted — a stick has no edges to count — and
/// are reported as the excursion each byte made instead.
struct Control {
    name: &'static str,
    hint: &'static str,
    analogue: bool,
}

const CONTROLS: &[Control] = &[
    Control {
        name: "A",
        hint: "the bottom face button",
        analogue: false,
    },
    Control {
        name: "B",
        hint: "the right face button",
        analogue: false,
    },
    Control {
        name: "X",
        hint: "the left face button",
        analogue: false,
    },
    Control {
        name: "Y",
        hint: "the top face button",
        analogue: false,
    },
    Control {
        name: "D-pad Up",
        hint: "",
        analogue: false,
    },
    Control {
        name: "D-pad Down",
        hint: "",
        analogue: false,
    },
    Control {
        name: "D-pad Left",
        hint: "",
        analogue: false,
    },
    Control {
        name: "D-pad Right",
        hint: "",
        analogue: false,
    },
    Control {
        name: "L1",
        hint: "left bumper",
        analogue: false,
    },
    Control {
        name: "R1",
        hint: "right bumper",
        analogue: false,
    },
    Control {
        name: "View",
        hint: "the small left button",
        analogue: false,
    },
    Control {
        name: "Menu",
        hint: "the small right button",
        analogue: false,
    },
    Control {
        name: "Steam",
        hint: "the Steam button",
        analogue: false,
    },
    Control {
        name: "L3",
        hint: "click the LEFT stick in",
        analogue: false,
    },
    Control {
        name: "R3",
        hint: "click the RIGHT stick in",
        analogue: false,
    },
    Control {
        name: "L2",
        hint: "squeeze the left trigger fully",
        analogue: true,
    },
    Control {
        name: "R2",
        hint: "squeeze the right trigger fully",
        analogue: true,
    },
    Control {
        name: "Left stick",
        hint: "push it fully left, right, up, then down",
        analogue: true,
    },
    Control {
        name: "Right stick",
        hint: "push it fully left, right, up, then down",
        analogue: true,
    },
];

fn main() {
    let label = std::env::args().nth(1).unwrap_or_else(|| "run".to_string());
    let log_path = PathBuf::from(format!("/tmp/lxb-pad-{label}.log"));
    let mut out = match Log::create(&log_path) {
        Ok(log) => log,
        Err(err) => {
            eprintln!("cannot write {}: {err}", log_path.display());
            return;
        }
    };

    out.line(&format!("# pad capture: {label}"));
    out.line(&format!("# {PRESSES} presses per control"));

    let mut gilrs = report_gilrs(&mut out);
    let mut pads = open_hidraw(&mut out);
    if pads.is_empty() {
        out.line("no pad to read — stopping.");
        return;
    }

    out.line(&format!(
        "\n=== calibrating: DO NOT TOUCH THE PAD for {}s ===",
        CALIBRATE.as_secs()
    ));
    let deadline = Instant::now() + CALIBRATE;
    while Instant::now() < deadline {
        drain(&mut gilrs, &mut pads, &mut out, false);
        std::thread::sleep(Duration::from_millis(2));
    }
    for pad in pads.iter_mut() {
        pad.finish_calibration(&mut out);
    }

    // Nothing here reads the terminal, and that is the point. With Steam shut
    // the pad is a *keyboard*: A is Enter and B is Escape, so a prompt that
    // waited for a keypress would be answered by the very button it was asking
    // about — the A window used to close on the first press and score it once.
    // The windows are timed instead, and the keys the pad types are swallowed.
    let _quiet = TerminalEcho::quiet();

    out.line("\n=== go ===");
    out.line("each control gets a countdown, then a window. no typing needed.");
    out.line("miss one and it will say so — the run does not stop for it.\n");

    for control in CONTROLS {
        let asked = if control.analogue {
            format!("{} — {}", control.name, control.hint)
        } else if control.hint.is_empty() {
            format!("{} — press it {PRESSES} times", control.name)
        } else {
            format!(
                "{} ({}) — press it {PRESSES} times",
                control.name, control.hint
            )
        };
        out.line(&format!("--- {asked}"));

        // A moment to find the control, with the pad still ignored: a thumb
        // travelling to a button must not be counted as having arrived.
        countdown("get ready", READY, &mut gilrs, &mut pads, &mut out, false);

        for pad in pads.iter_mut() {
            pad.begin_window();
        }
        let window = if control.analogue {
            ANALOGUE_WINDOW
        } else {
            WINDOW
        };
        countdown("NOW", window, &mut gilrs, &mut pads, &mut out, true);

        // Only the node that is actually talking. The dongle presents one
        // interface per pad slot and four of them stay silent all session;
        // letting each say so once per control buries the answer.
        let mut heard = 0;
        for pad in pads.iter_mut().filter(|pad| pad.reports > 0) {
            heard += 1;
            pad.report_window(control, &mut out);
        }
        if heard == 0 {
            out.line("    no pad node is sending anything at all");
        }
    }

    out.line("\n=== end ===");
    if let Some(gilrs) = gilrs.as_ref() {
        out.line(&format!(
            "gilrs pads at the end: {}",
            gilrs.gamepads().count()
        ));
    }
    println!("\nwritten to {}", log_path.display());
}

/// Read both paths, folding anything new into the pads' counters.
fn drain(gilrs: &mut Option<gilrs::Gilrs>, pads: &mut Vec<Pad>, out: &mut Log, live: bool) {
    if let Some(gilrs) = gilrs.as_mut() {
        while let Some(event) = gilrs.next_event() {
            match event.event {
                gilrs::EventType::ButtonPressed(button, code) => out.line(&format!(
                    "    gilrs press   {button:?} code {:#x}",
                    code.into_u32()
                )),
                gilrs::EventType::Connected => {
                    out.line(&format!("    gilrs CONNECTED [{}]", event.id))
                }
                gilrs::EventType::Disconnected => {
                    out.line(&format!("    gilrs DISCONNECTED [{}]", event.id))
                }
                _ => {}
            }
        }
    }

    let mut buf = [0u8; 64];
    for pad in pads.iter_mut() {
        loop {
            match pad.file.read(&mut buf) {
                Ok(len) if len == REPORT_LEN && buf[0] == REPORT_ID => {
                    pad.observe(&buf[..REPORT_LEN], live)
                }
                // Shorter reports are the pad's own housekeeping: battery, and
                // a heartbeat every half second.
                Ok(len) if len > 0 => continue,
                Ok(_) => break,
                Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                Err(err) => {
                    out.line(&format!("  {} closed: {err}", pad.path.display()));
                    pad.dead = true;
                    break;
                }
            }
        }
    }
    pads.retain(|pad| !pad.dead);
}

/// Keep reading the pad for `how_long`, showing the seconds left.
///
/// The countdown is written to the terminal only. It is a carriage return
/// redrawing one line, which belongs on a screen and not in a log.
fn countdown(
    caption: &str,
    how_long: Duration,
    gilrs: &mut Option<gilrs::Gilrs>,
    pads: &mut Vec<Pad>,
    out: &mut Log,
    live: bool,
) {
    let end = Instant::now() + how_long;
    let mut shown = u64::MAX;
    while Instant::now() < end {
        let left = (end - Instant::now()).as_secs() + 1;
        if left != shown {
            shown = left;
            print!("\r    {caption}: {left}s   ");
            let _ = std::io::stdout().flush();
        }
        drain(gilrs, pads, out, live);
        std::thread::sleep(Duration::from_millis(2));
    }
    print!("\r{:40}\r", "");
    let _ = std::io::stdout().flush();
}

/// Stops the terminal echoing while the capture runs.
///
/// In lizard mode the pad *is* a keyboard, so every button pressed for the
/// capture is also typed at the shell: escape sequences all over the screen
/// during the run, and a command line full of them afterwards. Echo goes off
/// for the duration and the pending input is thrown away at the end.
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

/// What GilRs can see, which on this pad is the whole question: with no kernel
/// driver it sees nothing at all until something else creates a pad for it.
fn report_gilrs(out: &mut Log) -> Option<gilrs::Gilrs> {
    match gilrs::GilrsBuilder::new()
        .with_force_feedback(false)
        .build()
    {
        Ok(gilrs) => {
            out.line("gilrs sees:");
            let mut any = false;
            for (id, gamepad) in gilrs.gamepads() {
                any = true;
                out.line(&format!(
                    "  [{id}] {:?}  mapping {:?}",
                    gamepad.name(),
                    gamepad.mapping_source()
                ));
            }
            if !any {
                out.line("  (no gamepads — the puck has no kernel driver of its own)");
            }
            Some(gilrs)
        }
        Err(err) => {
            out.line(&format!("gilrs failed to start: {err}"));
            None
        }
    }
}

struct Pad {
    path: PathBuf,
    file: File,
    last: [u8; REPORT_LEN],
    primed: bool,
    /// Bits seen moving while nobody was touching the pad: the sequence counter
    /// and the low bits of every analogue axis. Ignored from then on.
    noise: [u8; REPORT_LEN],
    calibrated: bool,
    /// Rising edges per bit in the window just opened — the count that names a
    /// button, because noise does not arrive in threes.
    edges: [[u32; 8]; REPORT_LEN],
    /// The excursion each byte made in the window, which is what names an axis.
    low: [u8; REPORT_LEN],
    high: [u8; REPORT_LEN],
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
                "nothing — silent at rest".to_string()
            } else {
                noisy.join(" ")
            }
        ));
    }

    fn begin_window(&mut self) {
        self.edges = [[0; 8]; REPORT_LEN];
        self.low = [0xff; REPORT_LEN];
        self.high = [0; REPORT_LEN];
    }

    fn observe(&mut self, report: &[u8], live: bool) {
        self.reports += 1;
        if !self.primed {
            self.last.copy_from_slice(report);
            self.primed = true;
            return;
        }

        for (index, byte) in report.iter().enumerate() {
            if live {
                self.low[index] = self.low[index].min(*byte);
                self.high[index] = self.high[index].max(*byte);
            }
            let changed = byte ^ self.last[index];
            if changed == 0 {
                continue;
            }
            if !self.calibrated {
                self.noise[index] |= changed;
                continue;
            }
            if !live {
                continue;
            }
            // Rising edges only: a press is a bit going down, and counting both
            // edges would turn three presses into six.
            let rising = changed & *byte & !self.noise[index];
            for bit in 0..8 {
                if rising & (1u8 << bit) != 0 {
                    self.edges[index][bit] += 1;
                }
            }
        }
        self.last.copy_from_slice(report);
    }

    /// Name the bit that moved the right number of times, and show the field.
    fn report_window(&self, control: &Control, out: &mut Log) {
        if control.analogue {
            let mut swings: Vec<(usize, u8, u8)> = self
                .low
                .iter()
                .zip(self.high.iter())
                .enumerate()
                .filter(|(index, (low, high))| {
                    **high as i16 - **low as i16 > 40 && self.noise[*index] != 0xff
                })
                .map(|(index, (low, high))| (index, *low, *high))
                .collect();
            swings.sort_by_key(|(_, low, high)| std::cmp::Reverse(*high as i16 - *low as i16));
            if swings.is_empty() {
                out.line("    nothing moved");
                return;
            }
            for (index, low, high) in swings.iter().take(6) {
                out.line(&format!(
                    "    byte {index:>2}  {low:#04x}..{high:#04x}  (swing {})",
                    *high as i16 - *low as i16
                ));
            }
            return;
        }

        let mut hits: Vec<(usize, usize, u32)> = Vec::new();
        for (index, bits) in self.edges.iter().enumerate() {
            for (bit, count) in bits.iter().enumerate() {
                if *count > 0 {
                    hits.push((index, bit, *count));
                }
            }
        }
        if hits.is_empty() {
            out.line("    NOTHING MOVED — this control sends no bit on this node");
            return;
        }
        // Exactly right first, then near misses, then the chatter.
        hits.sort_by_key(|(_, _, count)| {
            (
                (*count as i64 - PRESSES as i64).abs(),
                std::cmp::Reverse(*count as i64),
            )
        });
        for (index, bit, count) in hits.iter().take(6) {
            let verdict = if *count == PRESSES { "  <== THIS" } else { "" };
            out.line(&format!(
                "    byte {index:>2} bit {bit}  mask {:#04x}  x{count}{verdict}",
                1u8 << bit
            ));
        }
        if hits.len() > 6 {
            out.line(&format!("    (+{} noisier bits)", hits.len() - 6));
        }
    }
}

/// Everything printed, kept in a file as well as on screen.
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

fn open_hidraw(out: &mut Log) -> Vec<Pad> {
    let mut pads = Vec::new();
    let Ok(entries) = std::fs::read_dir("/sys/class/hidraw") else {
        out.line("no /sys/class/hidraw");
        return pads;
    };
    let mut nodes: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| is_puck(&entry.path()))
        .map(|entry| Path::new("/dev").join(entry.file_name()))
        .filter(|node| node.exists())
        .collect();
    nodes.sort();

    out.line(&format!("\nhidraw nodes for the puck ({HID_ID_MATCH}):"));
    if nodes.is_empty() {
        out.line("  (none — is it plugged in?)");
    }
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
                    edges: [[0; 8]; REPORT_LEN],
                    low: [0xff; REPORT_LEN],
                    high: [0; REPORT_LEN],
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
