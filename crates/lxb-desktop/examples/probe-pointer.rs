//! Which device is actually moving the pointer, and how far.
//!
//! When the cursor and an application disagree about where the pointer is, the
//! first thing to establish is how many things are moving it. Steam drives the
//! Steam Controller's trackpad itself once it has claimed the pad, and it has
//! more than one way to do that: a virtual mouse of its own through `uinput`,
//! which every compositor sees as an ordinary pointer, or X11 synthetic events,
//! which only reach X clients. The two behave identically until they do not.
//!
//! So this watches *every* pointer the machine has, names it, and totals the
//! relative motion each one emits per window. A window where the cursor moved
//! and nothing here counted anything is motion that never touched evdev.

use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Write as _};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// `struct input_event` on a 64-bit kernel: a 16-byte timeval, then type, code
/// and value.
const INPUT_EVENT_LEN: usize = 24;
const EV_REL: u16 = 0x02;
const REL_X: u16 = 0x00;
const REL_Y: u16 = 0x01;

/// `EVIOCGNAME(256)`, which asks a node what it calls itself.
const EVIOCGNAME_256: libc::c_ulong = 0x8100_4506;

const WINDOW: Duration = Duration::from_secs(8);

fn main() {
    let label = std::env::args().nth(1).unwrap_or_else(|| "run".to_string());
    let log_path = PathBuf::from(format!("/tmp/lxb-pointer-{label}.log"));
    let mut out = match Log::create(&log_path) {
        Ok(log) => log,
        Err(err) => {
            eprintln!("cannot write {}: {err}", log_path.display());
            return;
        }
    };

    out.line(&format!("# pointer capture: {label}"));
    let mut pointers = open_pointers(&mut out);
    if pointers.is_empty() {
        out.line("no readable input devices at all — stopping.");
        return;
    }

    let quiet = TerminalEcho::quiet();
    for (name, hint) in [
        ("SETTLE", "hands off everything — this is the baseline"),
        (
            "FLICK",
            "flick the RIGHT touchpad hard and LIFT YOUR FINGER, a few times",
        ),
        (
            "SLOW",
            "move the RIGHT touchpad slowly, finger down throughout",
        ),
        ("MOUSE", "move your normal mouse, for comparison"),
    ] {
        out.line(&format!("\n--- {name}: {hint}"));
        for pointer in pointers.iter_mut() {
            pointer.begin();
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
            for pointer in pointers.iter_mut() {
                pointer.pump();
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        print!("\r{:46}\r", "");
        let _ = std::io::stdout().flush();

        let mut anything = false;
        for pointer in pointers.iter().filter(|pointer| pointer.events > 0) {
            anything = true;
            pointer.report(&mut out);
        }
        if !anything {
            out.line(
                "    nothing on any *readable* evdev pointer — check the UNREADABLE list above \
                 before concluding this motion never touched evdev",
            );
        }
    }
    drop(quiet);

    println!("\nwritten to {}", log_path.display());
}

struct Pointer {
    path: PathBuf,
    name: String,
    file: File,
    events: u64,
    dx: i64,
    dy: i64,
    /// The largest single step seen, which is what tells a flick from a creep.
    peak: i32,
}

impl Pointer {
    fn begin(&mut self) {
        self.events = 0;
        self.dx = 0;
        self.dy = 0;
        self.peak = 0;
    }

    fn pump(&mut self) {
        let mut buf = [0u8; INPUT_EVENT_LEN * 64];
        loop {
            match self.file.read(&mut buf) {
                Ok(0) => break,
                Ok(len) => {
                    for chunk in buf[..len].chunks_exact(INPUT_EVENT_LEN) {
                        let kind = u16::from_ne_bytes([chunk[16], chunk[17]]);
                        let code = u16::from_ne_bytes([chunk[18], chunk[19]]);
                        let value =
                            i32::from_ne_bytes([chunk[20], chunk[21], chunk[22], chunk[23]]);
                        if kind != EV_REL {
                            continue;
                        }
                        self.events += 1;
                        self.peak = self.peak.max(value.abs());
                        match code {
                            REL_X => self.dx += i64::from(value),
                            REL_Y => self.dy += i64::from(value),
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
            "    {:<44} {:>5} events  dx {:>7}  dy {:>7}  peak step {:>4}   [{}]",
            self.name,
            self.events,
            self.dx,
            self.dy,
            self.peak,
            self.path.display()
        ));
    }
}

fn open_pointers(out: &mut Log) -> Vec<Pointer> {
    let mut pointers = Vec::new();
    out.line("\nevdev devices this user can read:");
    let Ok(entries) = std::fs::read_dir("/dev/input") else {
        return pointers;
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
    // `event10` must not sort before `event2`, or the log reads as nonsense.
    paths.sort_by_key(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix("event"))
            .and_then(|number| number.parse::<u32>().ok())
            .unwrap_or(u32::MAX)
    });

    for path in paths {
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)
        {
            Ok(file) => file,
            Err(err) => {
                // Said out loud, and never skipped quietly. A device this
                // cannot open is a device whose motion this cannot count, and
                // "nothing moved" would then be indistinguishable from "I was
                // not allowed to look" — which is the wrong conclusion drawn
                // confidently.
                out.line(&format!("  UNREADABLE {} ({err})", path.display()));
                continue;
            }
        };
        let mut raw = [0u8; 256];
        // Safety: EVIOCGNAME writes at most the length encoded in the request
        // into the buffer given, and the fd is a live evdev node this owns.
        let len = unsafe { libc::ioctl(file.as_raw_fd(), EVIOCGNAME_256, raw.as_mut_ptr()) };
        let name = if len > 0 {
            String::from_utf8_lossy(&raw[..(len as usize).saturating_sub(1)]).into_owned()
        } else {
            "?".to_string()
        };
        out.line(&format!("  {:<44} [{}]", name, path.display()));
        pointers.push(Pointer {
            path,
            name,
            file,
            events: 0,
            dx: 0,
            dy: 0,
            peak: 0,
        });
    }
    pointers
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

/// Stops the terminal echoing: the pad may still be typing while it is used.
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
