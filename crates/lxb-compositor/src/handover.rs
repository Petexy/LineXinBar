//! Keeping the picture on the display while the next compositor starts.
//!
//! A login is two compositors and a gap between them. greetd will not start
//! the session until the greeter has exited, and the session's compositor then
//! needs the better part of a second to open the GPU and set a mode. Measured
//! on a real login on this project's own display manager: 866 ms from the
//! greeter's compositor saying goodbye to the session's first modeset, of
//! which about 390 ms is greetd and the session wrapper and about 425 ms is
//! the compositor's own start-up. Neither half is worth shaving.
//!
//! What makes that gap *black* is not the gap. Two things in the kernel take
//! the picture down the moment the last handle on the DRM device closes, and
//! neither is anything the compositor asked for:
//!
//! * `drm_fb_release` destroys every framebuffer the file created, and
//!   removing one that a plane is still scanning out disables that plane —
//!   and, for a primary plane, the CRTC behind it. The screen goes black by
//!   kernel action, however politely the compositor shut down.
//! * `drm_lastclose` then restores the framebuffer console's own mode on top,
//!   which on a machine that never took the console over is a cleared buffer.
//!
//! Both are keyed to the *file description*, not to the process holding it. So
//! what removes them is to not let it close: fork a child that holds the same
//! descriptor and does nothing else with it. The framebuffers stay alive, the
//! CRTC keeps scanning one out, and the display goes on showing the last frame
//! of the outgoing session until the incoming compositor commits its own —
//! which, being the same mode on the same connector, is a plane update rather
//! than a modeset, and does not blank.
//!
//! This is the shape every other display manager arrived at from the other
//! end: SDDM's greeter and GDM's both stay alive across the switch, so the
//! outgoing framebuffer is still there to be replaced. Holding the descriptor
//! buys the same thing for about forty lines and no second process tree.
//!
//! The other half of a seamless hand-over is on the *incoming* side and lives
//! in [`crate::backend::udev`]: a compositor that resets the device to a known
//! state on start-up blanks whatever it was handed before it has anything to
//! put there. See `device_added`.

use std::os::fd::RawFd;
use std::time::Duration;

/// Set by a display manager on both sides of a login to say that the displays
/// are being passed between compositors rather than given back to a console.
///
/// It is deliberately not a command-line flag. The session compositor is
/// started from a desktop entry the display manager does not write, so an
/// environment variable is the only thing it can reach; and the answer is a
/// property of how the session was started, not of what the user configured.
pub const ENV: &str = "LXB_HOLD_DISPLAY";

/// Longest the picture is held after this compositor has gone.
///
/// Long enough to cover a hand-over several times over, short enough that a
/// session which fails to start leaves a machine that plainly needs looking
/// at rather than one frozen on a picture of a login screen.
const LONGEST: Duration = Duration::from_secs(10);

/// How often the keeper asks whether the next compositor has taken over.
///
/// One frame at 60 Hz. The answer is one ioctl per display, and it is only
/// asked during the second or so this process outlives its parent.
const POLL: Duration = Duration::from_millis(16);

/// How many displays one keeper will watch. Four CRTCs is a common ceiling per
/// GPU and this covers several of them; a machine with more keeps the picture
/// on the first [`WATCHED`] and loses nothing else.
const WATCHED: usize = 32;

/// How far [`close_between`] walks by hand on a kernel with no `close_range`,
/// which means one older than 5.9. Descriptor numbers are handed out
/// lowest-first, so everything a compositor opened for itself — the seat, the
/// bus, the journal, the GPUs — is far below this.
const CEILING: u32 = 4096;

/// A CRTC to keep lit, and the framebuffer it was last seen scanning out.
#[derive(Clone, Copy)]
struct Held {
    fd: RawFd,
    crtc: u32,
    fb: u32,
}

/// Whether this session was started by something that will take the displays
/// straight back.
pub fn wanted() -> bool {
    asked_for(std::env::var(ENV).ok().as_deref())
}

/// [`wanted`] with the environment lifted out, so it can be tested without
/// setting a variable other tests in the same process can see.
fn asked_for(value: Option<&str>) -> bool {
    matches!(value, Some("1" | "yes" | "true"))
}

/// Fork a child that holds `displays` open until the compositor that follows
/// has them, and answer how many displays it was left watching.
///
/// Called after the event loop has stopped and immediately before the process
/// exits. The caller must leave without unwinding — see [`crate::main`] — as
/// dropping this process's DRM state would destroy the very framebuffers the
/// child was forked to keep.
pub fn hold(displays: &[(RawFd, u32)]) -> usize {
    let mut held = [Held {
        fd: -1,
        crtc: 0,
        fb: 0,
    }; WATCHED];
    let mut count = 0;

    for &(fd, crtc) in displays.iter().take(WATCHED) {
        // A CRTC with nothing on it is not a picture worth keeping, and
        // watching one would stop the keeper noticing that every *other*
        // display has been taken over.
        match scanning_out(fd, crtc) {
            Some(fb) if fb != 0 => {
                held[count] = Held { fd, crtc, fb };
                count += 1;
            }
            _ => {}
        }
    }

    if count == 0 {
        return 0;
    }

    // SAFETY: `fork` in a process that has had threads is safe for the child
    // only while it stays inside async-signal-safe calls, which is the whole
    // contract of `keep` below: no allocation, no locks, no tracing, nothing
    // of the Rust runtime. The parent does nothing unusual at all.
    match unsafe { libc::fork() } {
        // Forking failed. The hand-over is no worse than it was before this
        // existed, and there is nothing useful to say about it from here.
        -1 => 0,
        0 => unsafe { keep(&held[..count]) },
        _ => count,
    }
}

/// The framebuffer a CRTC is scanning out, or `None` if it is not driving a
/// display at all.
///
/// `DRM_IOCTL_MODE_GETCRTC` needs neither DRM master nor privilege — it is a
/// read of state the kernel is happy to show any client that can open the
/// node, which is what lets the keeper watch a device another compositor has
/// since taken master on.
fn scanning_out(fd: RawFd, crtc: u32) -> Option<u32> {
    // SAFETY: every field of `drm_mode_crtc` is an integer or a byte array,
    // for which all-zeroes is a valid value.
    let mut request: DrmModeCrtc = unsafe { std::mem::zeroed() };
    request.crtc_id = crtc;
    // SAFETY: `request` is a live, correctly laid out `drm_mode_crtc` — see
    // the assertions below — and GETCRTC only fills it in. `set_connectors_ptr`
    // is left null with `count_connectors` zero, which asks for no connector
    // list to be written anywhere.
    if unsafe { libc::ioctl(fd, GETCRTC as _, &mut request) } != 0 {
        return None;
    }
    (request.mode_valid != 0).then_some(request.fb_id)
}

/// The forked child.
///
/// # Safety
///
/// Runs after `fork` in a process that had threads, so everything here is
/// restricted to async-signal-safe calls: `setsid`, `sigaction` via `signal`,
/// `close`, `close_range`, `ioctl`, `nanosleep` and `_exit`. Nothing allocates,
/// takes a lock, or touches the Rust runtime, and nothing here may start doing
/// so.
unsafe fn keep(held: &[Held]) -> ! {
    // Leave the session's process group and terminal, so that whatever tidies
    // up after the compositor — greetd's session worker signalling the group,
    // logind stopping the scope — sweeps past this rather than through it.
    libc::setsid();
    for signal in [libc::SIGTERM, libc::SIGHUP, libc::SIGINT] {
        libc::signal(signal, libc::SIG_IGN);
    }
    let_go(held);

    let nap = libc::timespec {
        tv_sec: POLL.as_secs() as libc::time_t,
        tv_nsec: POLL.subsec_nanos() as libc::c_long,
    };
    let mut waited = Duration::ZERO;

    loop {
        libc::nanosleep(&nap, std::ptr::null_mut());
        waited += POLL;

        let mut taken = 0;
        for display in held {
            // Somebody else's framebuffer on our CRTC: the next compositor has
            // this display, the picture is theirs now, and ours is no longer
            // being scanned out by anybody.
            match scanning_out(display.fd, display.crtc) {
                Some(fb) if fb != 0 && fb != display.fb => taken += 1,
                _ => {}
            }
        }

        if taken == held.len() || waited >= LONGEST {
            libc::_exit(0);
        }
    }
}

/// Close everything this fork inherited except the descriptors being held.
///
/// A `fork` copies the whole descriptor table, and a compositor's table has a
/// great deal more in it than the picture. Two of those matter here.
///
/// The journal pipe on 1 and 2 is the obvious one: a hand-over must not be left
/// waiting on a process whose only job is to say nothing for a second.
///
/// The one that bites is the connection to logind. logind counts that
/// connection as the *controller* of the session the compositor was running in,
/// and holds the login VT on its behalf for as long as it is open. The keeper
/// outlives its compositor by design and inherits that connection with
/// everything else, so the session it belonged to stays logind's owner of the
/// terminal for the whole hand-over — and when the keeper finally exits, having
/// waited for exactly the moment the *next* compositor took the displays,
/// logind restores a terminal that compositor had already configured for
/// itself. `KDSKBMODE` goes back to translate mode, `KDSETMODE` back to
/// `KD_TEXT`, `VT_SETMODE` back to `VT_AUTO`, underneath a desktop that is
/// running and has been told none of it.
///
/// From the other side of the screen that is Ctrl+C closing the session instead
/// of copying the selection: the kernel is translating keys again, so it turns
/// ^C into a `SIGINT` to the terminal's foreground process group, which is the
/// display manager's session worker and everything it started. Ctrl+\ and
/// Ctrl+Z are the same sentence with a different signal, and VT switching stops
/// going through the compositor at all.
///
/// So the keeper lets go of the session before it settles down to watch, and
/// logind restores the VT at once — while the display manager is still starting
/// the next session, rather than a second after that session has come up.
/// Nothing is given away by leaving early: greetd puts the terminal back into
/// `KD_TEXT` itself before every session it starts, so it was going to be reset
/// inside this window regardless, and the picture is held by the descriptors
/// kept below rather than by anything logind owns. What logind does on the way
/// out — dropping DRM master on a device this process has already stopped
/// driving, closing its own copy of the descriptor — leaves the framebuffers
/// alone, because they belong to the open file description and this process is
/// still holding one.
///
/// # Safety
///
/// Async-signal-safe, and called only from [`keep`]: see its contract.
unsafe fn let_go(held: &[Held]) {
    let mut keep = [0; WATCHED];
    let kept = kept_descriptors(held, &mut keep);

    let mut first: u32 = 0;
    for &descriptor in &keep[..kept] {
        let Ok(descriptor) = u32::try_from(descriptor) else {
            continue;
        };
        if descriptor > first {
            close_between(first, descriptor - 1);
        }
        match descriptor.checked_add(1) {
            Some(next) => first = next,
            // Held on the very last descriptor number there is, so there is
            // nothing above it to close.
            None => return,
        }
    }
    close_between(first, u32::MAX);
}

/// The held descriptors, in order and without repeats.
///
/// One DRM device drives several of the CRTCs being watched, so the same
/// descriptor arrives once per display, and [`let_go`] needs each of them once
/// and in order to close the gaps between them. Answers how many of `into` were
/// written. An insertion sort because there are at most [`WATCHED`] of them and
/// nothing here may allocate.
fn kept_descriptors(held: &[Held], into: &mut [RawFd; WATCHED]) -> usize {
    let mut kept = 0;
    for display in held.iter().take(WATCHED) {
        let mut at = kept;
        let mut seen = false;
        for (index, &descriptor) in into[..kept].iter().enumerate() {
            if descriptor == display.fd {
                seen = true;
                break;
            }
            if descriptor > display.fd {
                at = index;
                break;
            }
        }
        if seen {
            continue;
        }
        into.copy_within(at..kept, at + 1);
        into[at] = display.fd;
        kept += 1;
    }
    kept
}

/// Close every descriptor from `first` to `last`, both ends included.
///
/// # Safety
///
/// Async-signal-safe, and called only from [`let_go`]: see [`keep`].
unsafe fn close_between(first: u32, last: u32) {
    if first > last {
        return;
    }
    if libc::syscall(libc::SYS_close_range, first, last, 0_u32) == 0 {
        return;
    }
    // No `close_range` in this kernel, which means one older than 5.9. Walking
    // it by hand instead, as far as [`CEILING`]: an unbounded loop here would
    // be four billion system calls in the middle of a login.
    for descriptor in first..=last.min(CEILING) {
        libc::close(descriptor as libc::c_int);
    }
}

/// `DRM_IOCTL_MODE_GETCRTC`, as `_IOWR('d', 0xA1, struct drm_mode_crtc)`
/// works out to. Checked against `libdrm`'s own header rather than trusted:
/// see the assertions in the tests below.
const GETCRTC: u64 = 0xC068_64A1;

/// `struct drm_mode_crtc` from `drm_mode.h`.
#[repr(C)]
struct DrmModeCrtc {
    set_connectors_ptr: u64,
    count_connectors: u32,
    crtc_id: u32,
    fb_id: u32,
    x: u32,
    y: u32,
    gamma_size: u32,
    mode_valid: u32,
    mode: DrmModeModeinfo,
}

/// `struct drm_mode_modeinfo` from `drm_mode.h`. Never read here, but it is
/// part of the size the ioctl number encodes and of what the kernel writes.
#[repr(C)]
struct DrmModeModeinfo {
    clock: u32,
    hdisplay: u16,
    hsync_start: u16,
    hsync_end: u16,
    htotal: u16,
    hskew: u16,
    vdisplay: u16,
    vsync_start: u16,
    vsync_end: u16,
    vtotal: u16,
    vscan: u16,
    vrefresh: u32,
    flags: u32,
    kind: u32,
    name: [u8; 32],
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ioctl number encodes the size of the struct it carries, so a
    /// layout that has drifted from the kernel's is a number the kernel will
    /// reject rather than a field read from the wrong offset. Both are checked
    /// here against values taken from `libdrm`'s headers on the machine this
    /// was written on.
    #[test]
    fn the_getcrtc_call_matches_the_kernels_own_header() {
        assert_eq!(std::mem::size_of::<DrmModeModeinfo>(), 68);
        assert_eq!(std::mem::size_of::<DrmModeCrtc>(), 104);
        assert_eq!(std::mem::offset_of!(DrmModeCrtc, fb_id), 16);
        assert_eq!(std::mem::offset_of!(DrmModeCrtc, mode_valid), 32);
        assert_eq!(std::mem::offset_of!(DrmModeCrtc, mode), 36);

        // _IOWR(type, nr, size) = (read|write) << 30 | size << 16 | type << 8 | nr
        let computed = (3 << 30)
            | ((std::mem::size_of::<DrmModeCrtc>() as u64) << 16)
            | (u64::from(b'd') << 8)
            | 0xA1;
        assert_eq!(GETCRTC, computed);
    }

    /// The variable says the displays are being handed to another compositor.
    /// Anything that does not say so is a session that ends by giving the
    /// console back, and holding a picture over it would be a machine that
    /// looks like it has hung.
    #[test]
    fn only_a_display_manager_saying_yes_holds_the_picture() {
        assert!(asked_for(Some("1")));
        assert!(asked_for(Some("yes")));
        assert!(asked_for(Some("true")));

        assert!(!asked_for(None));
        assert!(!asked_for(Some("")));
        assert!(!asked_for(Some("0")));
        assert!(!asked_for(Some("no")));
        // Not a word this compositor knows, so not a promise it can rely on.
        assert!(!asked_for(Some("Yes")));
        assert!(!asked_for(Some("maybe")));
    }

    fn watching(descriptors: &[RawFd]) -> Vec<Held> {
        descriptors
            .iter()
            .map(|&fd| Held { fd, crtc: 0, fb: 0 })
            .collect()
    }

    fn kept(held: &[Held]) -> Vec<RawFd> {
        let mut into = [0; WATCHED];
        let count = kept_descriptors(held, &mut into);
        into[..count].to_vec()
    }

    /// [`let_go`] closes the gaps between the descriptors it is keeping, so it
    /// needs each of them once and in ascending order. They do not arrive that
    /// way: one GPU drives several of the CRTCs being watched and hands over
    /// the same descriptor once per display, in whatever order the outputs
    /// were enumerated.
    #[test]
    fn the_held_descriptors_are_counted_once_each_and_in_order() {
        assert_eq!(kept(&watching(&[])), Vec::<RawFd>::new());
        assert_eq!(kept(&watching(&[7, 7, 7])), vec![7]);
        assert_eq!(kept(&watching(&[9, 4, 9, 12, 4])), vec![4, 9, 12]);
        // Two GPUs, three displays each, interleaved as the outputs came.
        assert_eq!(kept(&watching(&[11, 8, 11, 8, 11, 8])), vec![8, 11]);
        // More displays than one keeper watches: the rest were already
        // dropped by `hold`, and nothing may run off the end of the array.
        let many: Vec<RawFd> = (0..WATCHED as RawFd + 8).rev().collect();
        assert_eq!(kept(&watching(&many)).len(), WATCHED);
    }

    /// The property the hand-over turns on: a keeper holds the picture and
    /// nothing else. Written as a `fork` because that is the only place the
    /// property exists — what the child inherits is the whole descriptor
    /// table, with the connection to logind somewhere in it, and letting that
    /// one live is what leaves the login VT owned by the session that just
    /// ended. See [`let_go`].
    #[test]
    fn a_keeper_lets_go_of_everything_it_is_not_holding() {
        // One pipe stands in for the connection the keeper must drop, one for
        // the GPU it must not.
        let mut session = [0; 2];
        let mut picture = [0; 2];
        assert_eq!(unsafe { libc::pipe(session.as_mut_ptr()) }, 0);
        assert_eq!(unsafe { libc::pipe(picture.as_mut_ptr()) }, 0);
        let held = [Held {
            fd: picture[1],
            crtc: 0,
            fb: 0,
        }];

        let child = unsafe { libc::fork() };
        assert_ne!(child, -1, "could not fork");
        if child == 0 {
            // SAFETY: the same contract as the keeper itself — nothing here
            // allocates, locks or returns into the Rust runtime.
            unsafe {
                let_go(&held);
                // Only reachable through the descriptor that was kept, so the
                // parent reading this byte is the descriptor still working.
                libc::write(picture[1], b"p".as_ptr().cast(), 1);
                // Stay alive while the parent looks at the other one. A child
                // that exited first would close every descriptor by exiting,
                // which is the very thing being tested for.
                let nap = libc::timespec {
                    tv_sec: 10,
                    tv_nsec: 0,
                };
                libc::nanosleep(&nap, std::ptr::null_mut());
                libc::_exit(0);
            }
        }

        // The parent's own copies of the write ends, so that what the child
        // does with its copies is the only thing left keeping them open.
        unsafe {
            libc::close(session[1]);
            libc::close(picture[1]);
            // Non-blocking, so a keeper that never let go is a test that fails
            // rather than one that waits for the child to exit and then agrees
            // with itself.
            libc::fcntl(session[0], libc::F_SETFL, libc::O_NONBLOCK);
        }

        let mut byte = [0_u8; 1];
        let read = unsafe { libc::read(picture[0], byte.as_mut_ptr().cast(), 1) };
        assert_eq!((read, byte[0]), (1, b'p'), "the picture was not held");

        let read = unsafe { libc::read(session[0], byte.as_mut_ptr().cast(), 1) };
        assert_eq!(
            read, 0,
            "the keeper is still holding a descriptor it does not need"
        );

        unsafe {
            libc::kill(child, libc::SIGKILL);
            libc::waitpid(child, std::ptr::null_mut(), 0);
            libc::close(session[0]);
            libc::close(picture[0]);
        }
    }

    /// A CRTC that is not driving anything cannot be watched for a change, and
    /// asking for none at all must not fork a process with nothing to do.
    #[test]
    fn nothing_on_screen_forks_nothing() {
        assert_eq!(hold(&[]), 0);
        // A closed descriptor answers no CRTC, which is the same answer as a
        // dark one and must be treated the same way.
        assert_eq!(hold(&[(-1, 1)]), 0);
    }
}
