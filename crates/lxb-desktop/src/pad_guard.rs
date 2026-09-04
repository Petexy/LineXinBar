//! The guide button, kept away from everything that is not the shell.
//!
//! The compositor already withholds every other way of asking for the guide.
//! A `Home` key, a `Super` key, the mouse's side button: all of them are held
//! back from the focused client outright, both edges, because the guide is the
//! way *out* of an application that is holding everything else, and an
//! application that could take it over would be an application there is no way
//! out of.
//!
//! A controller cannot be held back that way, because a controller never
//! passes through the compositor at all. Wayland has no gamepad protocol, so a
//! game reads `/dev/input` itself, in its own process, and sees exactly what
//! the shell sees — the guide button included. Whoever holds the pad's device
//! node holds the guide button, and by default everybody does.
//!
//! So the pad is taken away and given back with one button missing. The guard
//! opens each pad, grabs it — `EVIOCGRAB`, which makes every other reader of
//! that node deaf — and creates a `uinput` device that says it is the same pad
//! and repeats everything it does, except that the guide button is never
//! repeated. Applications find the replacement where they would have found the
//! pad, and the only reader of the real one is this shell.
//!
//! ## What the replacement copies, and why all of it
//!
//! Name, bus, vendor, product, version, every button, every axis with its
//! range and resolution, the device properties, and force feedback. The name
//! and the four ids are the load-bearing ones: SDL builds a controller GUID out
//! of them and looks its mapping database up by that GUID, so a replacement
//! that differed in any of them would be a pad the database has never heard of
//! — the game would still have a controller, and every button on it would be
//! in the wrong place.
//!
//! The guide button is copied too — it is *declared*, and never sent. SDL
//! numbers a pad's buttons by walking its capability bitmap, so a replacement
//! missing one button would shift every button above it down a place and break
//! the mapping just as thoroughly. Declaring a button is not information about
//! anybody pressing it, which is the thing that has to stay here.
//!
//! ## The rules the guard holds itself to
//!
//! * **A pad is never taken without being given back.** The grab and the
//!   replacement are made together, and if any part of it fails — no
//!   `/dev/uinput`, no permission, a node that never appears — the grab is
//!   dropped and the pad is left exactly as it was found. A user whose guide
//!   button reaches applications has lost a rule; a user whose controller does
//!   nothing has lost the machine.
//! * **A pad already grabbed by somebody else is left alone**, for the same
//!   reason.
//! * **Nothing virtual is ever guarded.** A device `uinput` made is this
//!   guard's own replacement, or Steam Input's pad for a game, or another
//!   session's; cloning one would clone a clone, and grabbing one would take
//!   Steam's own pad out of Steam's hands.
//!
//! ## What this cannot reach
//!
//! `hidraw`. A pad that speaks HID also has a raw report node, reads of it are
//! not exclusive, and there is no kernel interface for making them so. SDL will
//! read a pad that way in preference to `/dev/input` when its HIDAPI drivers
//! recognise it, which would walk straight around all of this — so every
//! application the shell starts is told to ignore the raw nodes *of the pads
//! this guard holds*, and only those. See [`hidapi_ignore_list`].
//!
//! The second-generation Steam Controller is the pad none of *this* touches,
//! and it is guarded anyway, from the other end. It has no gamepad node to
//! grab, so [`crate::steam_hid`] drives it from its raw report and makes the
//! gamepad itself — one that has the guide button declared on it and never
//! sends it, which is the same bargain reached without a grab, because a device
//! this shell builds needs nothing taken away from it. Its ids join the ignore
//! list below on the same terms as any other pad the shell stands in front of.

use std::collections::{HashMap, HashSet};
use std::io::ErrorKind;
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use evdev::uinput::VirtualDevice;
use evdev::{
    Device, EventSummary, EventType, FFEffect, InputEvent, KeyCode, MiscCode, UInputCode,
    UinputAbsSetup,
};

/// The button in the middle of a pad with a logo on it: Xbox Guide, the
/// PlayStation button, a Deck's STEAM. Every driver spells it the same way,
/// because it is the one control the kernel's gamepad codes have never been
/// ambiguous about.
const GUIDE: KeyCode = KeyCode::BTN_MODE;

/// Where the kernel's event devices are.
const DEV_INPUT: &str = "/dev/input";

/// How often to look for a pad that was not there last time.
///
/// A directory listing, and an `open` of each node that has appeared since the
/// last one, which is nothing four times a second. It is also the longest a
/// freshly plugged-in pad can send the guide button to whatever is running
/// before the guard has it, so it is short rather than tidy.
const RESCAN: Duration = Duration::from_millis(250);

/// How long to wait for udev to give the replacement pad a device node and the
/// permissions to go with it.
///
/// The wait is what makes "never taken without being given back" true: until
/// this succeeds the replacement is not something an application can open, and
/// the pad's own node is already grabbed. It is measured in tens of
/// milliseconds in practice.
const REPLACEMENT_PATIENCE: Duration = Duration::from_secs(1);

/// How often to look, inside that wait.
const REPLACEMENT_POLL: Duration = Duration::from_millis(10);

/// How many rescans a device that cannot be opened yet is given before the
/// guard stops asking.
///
/// A node appears a moment before the ACL that makes it readable, so the first
/// attempt at a pad that has just been plugged in fails routinely. Everything
/// else in `/dev/input` that this shell may not open — every keyboard, for one
/// — must be given up on, or the guard would open the whole directory four
/// times a second forever.
const OPEN_ATTEMPTS: u8 = 8;

/// One poll's worth of the guide button.
///
/// Edges rather than a state, because that is what the shell acts on: a press
/// begins a hold that a chord may claim, and a release that no chord claimed is
/// what opens the guide. See [`crate::controller::ControllerInput::poll`].
///
/// Both can be true at once — the button was tapped inside one poll — and the
/// shell reads them in that order, press first.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Edges {
    pub pressed: bool,
    pub released: bool,
}

impl Edges {
    fn is_empty(self) -> bool {
        !self.pressed && !self.released
    }
}

/// Holds every pad on the machine, and hands the guide button to the shell.
///
/// The work happens on a thread of its own rather than in the shell's poll,
/// because everything a pad does that is *not* the guide button has to reach
/// the application on the pad's own timing. A game's input latency would
/// otherwise be this shell's frame pacing, which is not a trade anybody would
/// take for a button.
pub struct PadGuard {
    shared: Option<Arc<Shared>>,
}

/// What the guard's thread and the shell's thread say to each other.
#[derive(Debug, Default)]
struct Shared {
    /// Set when the shell is going away. The thread notices within one
    /// [`RESCAN`] and drops every pad it holds, which ungrabs each one and
    /// destroys its replacement.
    stop: AtomicBool,
    /// Whether the guide button is down *now*, which is what the screenshot
    /// chord asks. Separate from the edges below because a chord is spelled
    /// while the button is held, and the press that began the hold was drained
    /// by an earlier poll.
    held: AtomicBool,
    edges: Mutex<Edges>,
}

/// The pads the guard currently holds, for [`hidapi_ignore_list`] and
/// [`grabbed_nodes`].
///
/// A static because it is read where the environment of a launched application
/// is assembled ([`crate::model`]) rather than anywhere the guard is threaded
/// through, and because there is one set of controllers on a machine however
/// many parts of the shell want to know about them.
static GUARDED: Mutex<Vec<Held>> = Mutex::new(Vec::new());

/// One pad the guard has, as the two things anything else needs to know about
/// it: which pad it is, and which node is the one it has gone quiet on.
#[derive(Debug, Clone)]
struct Held {
    id: PadId,
    node: PathBuf,
}

/// A pad as USB names it. Two of them make one entry in SDL's ignore list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PadId {
    vendor: u16,
    product: u16,
}

impl PadGuard {
    /// Start guarding, unless controllers are switched off altogether.
    pub fn new(enabled: bool) -> Self {
        if !enabled {
            // `--no-gamepad` means this shell reads no controller, so it has no
            // business taking one away from anything else.
            return Self { shared: None };
        }

        let shared = Arc::new(Shared::default());
        let worker = Arc::clone(&shared);
        match std::thread::Builder::new()
            .name("lxb-pad-guard".into())
            .spawn(move || Worker::new(worker).run())
        {
            Ok(_) => Self {
                shared: Some(shared),
            },
            Err(err) => {
                tracing::warn!(%err, "could not start the pad guard; the guide button will also reach applications");
                Self { shared: None }
            }
        }
    }

    /// What the guide button did since the last time this was asked.
    pub fn take_edges(&self) -> Edges {
        let Some(shared) = &self.shared else {
            return Edges::default();
        };
        let mut edges = shared.edges.lock().unwrap_or_else(|err| err.into_inner());
        std::mem::take(&mut *edges)
    }

    /// Whether the guide button is down on a guarded pad — the screenshot
    /// chord's modifier.
    pub fn guide_held(&self) -> bool {
        self.shared
            .as_ref()
            .is_some_and(|shared| shared.held.load(Ordering::Relaxed))
    }
}

impl Drop for PadGuard {
    fn drop(&mut self) {
        if let Some(shared) = &self.shared {
            shared.stop.store(true, Ordering::Relaxed);
        }
        // Deliberately not joined. The thread is asleep in `poll` for at most
        // one rescan, and a shell that is shutting down should not wait a
        // quarter of a second for a tidy exit it gets for free either way:
        // whether the thread wakes or the process ends, the pad's file
        // descriptor closes, and closing it is what ungrabs the pad and
        // destroys its replacement.
    }
}

/// The device nodes the guard has grabbed — the pads that are silent to
/// everything on this machine that is not this shell.
///
/// What wants them is anything that has to tell another program *which* of two
/// identical controllers to listen to. A grab does not take a pad off the
/// machine: the node is still there, still enumerated, still opened by whatever
/// looks for controllers, and still answers with its name and its ids — it
/// simply never says anything again. So a program that binds the first pad it
/// finds to player one binds this one, and the copy that actually works ends up
/// on player three. See [`crate::pads`].
pub fn grabbed_nodes() -> Vec<PathBuf> {
    GUARDED
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .iter()
        .map(|held| held.node.clone())
        .collect()
}

/// Every pad this shell stands in front of, as `SDL_HIDAPI_IGNORE_DEVICES`
/// spells it — `None` when it stands in front of none.
///
/// This is the other half of the grab, and it exists because a grab only
/// covers `/dev/input`. SDL prefers its own HIDAPI drivers to the kernel's
/// gamepad node for the pads it recognises, and reads those over `hidraw`,
/// where no grab reaches — a game would see the guide button after all, on
/// exactly the popular controllers the HIDAPI drivers exist for.
///
/// Two sources, and the same rule for both: a pad is listed only while
/// something of this shell's is waiting for the application in `/dev/input`
/// with everything but the one button on it. The guard's replacements are one
/// source; [`crate::steam_hid::hidapi_ignore_ids`] is the other, for the pad
/// with no gamepad node at all, which this shell gives one. A pad neither
/// covers is not listed, so nothing is ever asked to ignore the only route a
/// controller has.
pub fn hidapi_ignore_list() -> Option<String> {
    spelled(pads_to_ignore(
        Reader::AnApplication,
        &guarded_pads(),
        &pads_this_shell_drives(),
    ))
}

/// The same list for Valve's client, which is not only a reader of pads.
///
/// Everything above holds for the client too — it walks around a grab over
/// `hidraw` like any other program, which is why it is told about the pads the
/// guard holds. It does *not* hold for the one pad this shell drives itself.
/// That pad has no kernel driver, so Valve's client is not one more program
/// looking for a controller: it is the controller's *other* driver, and the
/// only road a Steam game has to it. Naming it here does not move the client
/// onto the stand-in — nothing moves the client onto anything — it takes the
/// pad off the client altogether, and every game the client launches with it.
///
/// Measured, on the pad this was written for: `SDL_hid_enumerate` returns five
/// interfaces of `28de:1304` and none at all with the id on this list, and
/// Valve's client reads controllers through exactly that call. Its log says the
/// same from the other side — five `Local Device Found` lines in a session
/// started from a desktop, and not one in a session started from this shell.
///
/// The guide button, which is the whole reason the list exists, has its own
/// answer here and does not need this one: `lxb_steam::webui`'s
/// `leave_the_guide_button_alone` asks the client not to act on the button, and
/// the shell reads it from the pad's report either way.
pub fn hidapi_ignore_list_for_valves_client() -> Option<String> {
    spelled(pads_to_ignore(
        Reader::ValvesClient,
        &guarded_pads(),
        &pads_this_shell_drives(),
    ))
}

/// Who is being asked to leave a pad alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reader {
    /// Anything this shell starts, which reads controllers and nothing more.
    AnApplication,
    /// Valve's client, which reads them and drives one.
    ValvesClient,
}

/// Which of the pads this shell stands in front of `reader` is asked to ignore.
///
/// `guarded` are the pads taken from `/dev/input` and given back without their
/// guide button; `driven` are the ones with no kernel driver, which this shell
/// reads from `hidraw` and gives a gamepad to. The difference between the two
/// readers is the second list, and only the second list.
fn pads_to_ignore(reader: Reader, guarded: &[PadId], driven: &[PadId]) -> Vec<PadId> {
    let mut ids: Vec<PadId> = Vec::new();
    // One entry per *pad*, not per node: two identical controllers are one line
    // in this list, and a list that repeated itself would be a list SDL reads
    // twice.
    let mut add = |id: PadId| {
        if !ids.contains(&id) {
            ids.push(id);
        }
    };
    for id in guarded {
        add(*id);
    }
    if reader == Reader::AnApplication {
        for id in driven {
            add(*id);
        }
    }
    ids
}

/// The pads the guard is holding this moment.
fn guarded_pads() -> Vec<PadId> {
    GUARDED
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .iter()
        .map(|held| held.id)
        .collect()
}

/// The pads this shell is the driver of, because the kernel is not.
fn pads_this_shell_drives() -> Vec<PadId> {
    crate::steam_hid::hidapi_ignore_ids()
        .into_iter()
        .map(|(vendor, product)| PadId { vendor, product })
        .collect()
}

/// A list of pads the way `SDL_HIDAPI_IGNORE_DEVICES` reads — `None` for none.
fn spelled(ids: Vec<PadId>) -> Option<String> {
    if ids.is_empty() {
        return None;
    }
    Some(
        ids.iter()
            .map(|id| format!("0x{:04x}/0x{:04x}", id.vendor, id.product))
            .collect::<Vec<_>>()
            .join(","),
    )
}

/// What one packet from a pad turned into.
///
/// A packet is everything the pad sent between two `SYN_REPORT`s: one moment
/// of the controller, which the kernel delivers whole and which is repeated
/// whole or not at all.
#[derive(Debug, Default, PartialEq, Eq)]
struct Sifted {
    /// What the replacement pad is allowed to repeat.
    events: Vec<InputEvent>,
    /// What the guide button did, if it did anything: `Some(true)` down,
    /// `Some(false)` up.
    guide: Option<bool>,
}

/// Split one packet into what applications may see and what only the shell may.
///
/// Two things are dropped, not one. The guide button itself is obvious. The
/// other is `MSC_SCAN`, the scancode a driver sends immediately before a key to
/// say which physical control it came from: it names the button as plainly as
/// the button does, so a packet that carries a guide press loses its scancodes
/// too. That costs a simultaneous press its own scancode, which nothing reads —
/// SDL, GilRs and the kernel's own gamepad mapping all work from the key code.
///
/// A packet left with nothing in it is not sent at all. An empty frame is still
/// a frame: it tells whatever is reading that the pad reported *something*, and
/// the one thing it reported is the thing being kept.
fn sift(packet: &[InputEvent]) -> Sifted {
    let mut guide = None;
    for event in packet {
        if is_guide(event) {
            match event.value() {
                0 => guide = Some(false),
                1 => guide = Some(true),
                // 2 is the kernel repeating a held key, which a pad does not
                // do and which says nothing new if it did.
                _ => {}
            }
        }
    }

    let events = packet
        .iter()
        .copied()
        .filter(|event| !is_guide(event))
        .filter(|event| guide.is_none() || !is_scancode(event))
        .collect();

    Sifted { events, guide }
}

/// Whether a device is one this guard should take.
///
/// Two questions, and the second is the one that matters. The first is whether
/// there is anything here to keep from anybody: a device with no guide button
/// is left alone, because every grab is something that can go wrong for
/// whatever was reading it.
///
/// The second is whether taking it could cost more than the button is worth. A
/// grab takes a device away from the compositor as thoroughly as from a game,
/// so a node that can *type* is never taken, however many gamepad codes it
/// also declares. The worst this guard is allowed to do is leak a button; it
/// is not allowed to leave somebody with no keyboard.
fn worth_guarding(keys: Option<&evdev::AttributeSetRef<KeyCode>>) -> bool {
    let Some(keys) = keys else { return false };
    keys.contains(GUIDE) && !keys.contains(KeyCode::KEY_A) && !keys.contains(KeyCode::KEY_SPACE)
}

fn is_guide(event: &InputEvent) -> bool {
    event.event_type() == EventType::KEY && event.code() == GUIDE.0
}

fn is_scancode(event: &InputEvent) -> bool {
    event.event_type() == EventType::MISC && event.code() == MiscCode::MSC_SCAN.0
}

/// The guard's own thread.
struct Worker {
    shared: Arc<Shared>,
    /// Where to look for pads. Always [`DEV_INPUT`] in a running shell; a
    /// directory of the test's own where the guard is being made to guard a
    /// pad that does not exist.
    dir: PathBuf,
    /// Every event device the guard has made up its mind about, by the inode of
    /// its node.
    ///
    /// The inode rather than the path, because `/dev/input/event7` is a
    /// different device after a pad is unplugged and another is plugged in, and
    /// devtmpfs gives the new node a new inode. Keying on the path would have
    /// the guard skip the second pad as one it had already decided about.
    known: HashMap<u64, Slot>,
}

/// What the guard decided about one device node.
enum Slot {
    /// A pad, held: grabbed, with a replacement standing in for it.
    Guarded(Box<Pad>),
    /// Not a pad, or a pad that could not be taken. Either way it is never
    /// looked at again while its node lasts.
    LeftAlone,
    /// Could not be opened yet, and has this many attempts left.
    Waiting(u8),
}

/// One pad, and the replacement standing in for it.
struct Pad {
    path: PathBuf,
    /// The pad itself, grabbed. Dropping this closes the descriptor, which is
    /// what ungrabs it.
    device: Device,
    /// What every other reader on the machine finds instead.
    replacement: VirtualDevice,
    id: PadId,
    /// The packet being read, up to but not including the `SYN_REPORT` that
    /// will end it.
    packet: Vec<InputEvent>,
    /// Whether this pad's guide button is down. Kept on the pad rather than
    /// worked out from the edges, because an edge is drained by whichever poll
    /// happens to catch it and a hold outlasts it.
    guide_down: bool,
    /// Rumble the application uploaded to the replacement, by the effect id the
    /// replacement's own kernel side gave it. The value is the same effect on
    /// the real pad, which erases itself from the pad when it is dropped.
    effects: HashMap<u16, FFEffect>,
}

impl Worker {
    fn new(shared: Arc<Shared>) -> Self {
        Self {
            shared,
            dir: PathBuf::from(DEV_INPUT),
            known: HashMap::new(),
        }
    }

    fn run(mut self) {
        let mut next_scan = Instant::now();
        while !self.shared.stop.load(Ordering::Relaxed) {
            if Instant::now() >= next_scan {
                self.scan();
                next_scan = Instant::now() + RESCAN;
            }
            self.wait(next_scan.saturating_duration_since(Instant::now()));
            self.pump();
        }
        tracing::info!("pad guard stopping; every pad goes back to how it was found");
    }

    /// Sleep until a pad has something to say, or until the next rescan is due.
    fn wait(&self, timeout: Duration) {
        let mut fds: Vec<libc::pollfd> = Vec::new();
        for slot in self.known.values() {
            let Slot::Guarded(pad) = slot else { continue };
            // The pad, for what the user is doing with it, and the replacement,
            // because that is where rumble uploads from the application arrive.
            for fd in [pad.device.as_raw_fd(), pad.replacement.as_raw_fd()] {
                fds.push(libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                });
            }
        }
        let timeout = timeout.as_millis().min(i32::MAX as u128) as libc::c_int;
        // SAFETY: `fds` is a valid slice of that many pollfds for the call's
        // duration, and every descriptor in it is owned by a pad this guard
        // still holds. With no pads the count is zero, which poll reads as a
        // plain sleep and never dereferences the pointer for.
        unsafe {
            libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout);
        }
    }

    /// Read every pad, repeat what may be repeated, and answer any rumble.
    fn pump(&mut self) {
        let mut lost = Vec::new();
        let mut edges = Edges::default();

        for (ino, slot) in self.known.iter_mut() {
            let Slot::Guarded(pad) = slot else { continue };
            if let Err(err) = pad.pump(&mut edges) {
                tracing::info!(pad = %pad.path.display(), %err, "pad gone");
                lost.push(*ino);
            }
        }

        if !lost.is_empty() {
            self.known.retain(|ino, _| !lost.contains(ino));
            self.publish();
        }

        // Held on *any* pad, the way the shell asks it of the pads GilRs can
        // see: a machine with two controllers plugged in has one guide button
        // as far as the guide is concerned.
        let held = self.known.values().any(|slot| match slot {
            Slot::Guarded(pad) => pad.guide_down,
            _ => false,
        });
        self.shared.held.store(held, Ordering::Relaxed);

        if !edges.is_empty() {
            let mut shared = self
                .shared
                .edges
                .lock()
                .unwrap_or_else(|err| err.into_inner());
            shared.pressed |= edges.pressed;
            shared.released |= edges.released;
        }
    }

    /// Look for pads that were not here last time, and forget the ones that
    /// have gone.
    fn scan(&mut self) {
        let dir = match std::fs::read_dir(&self.dir) {
            Ok(dir) => dir,
            Err(err) => {
                // Once per rescan would be four warnings a second; this is the
                // one failure that says the whole guard is doing nothing, and
                // it is worth exactly one line either way.
                tracing::debug!(%err, dir = %self.dir.display(), "cannot list the input devices");
                return;
            }
        };

        let mut present = HashSet::new();
        for entry in dir.flatten() {
            if !entry.file_name().as_encoded_bytes().starts_with(b"event") {
                continue;
            }
            // The device the name leads to rather than the name: a node that
            // has gone takes its inode with it, which is how an unplugged pad
            // is told from the one plugged in after it. See [`Worker::known`].
            let Ok(meta) = std::fs::metadata(entry.path()) else {
                continue;
            };
            let ino = meta.ino();
            present.insert(ino);

            let attempts = match self.known.get(&ino) {
                Some(Slot::Waiting(left)) if *left > 0 => *left,
                Some(_) => continue,
                None => OPEN_ATTEMPTS,
            };
            let slot = self.consider(&entry.path(), attempts);
            let taken = matches!(slot, Slot::Guarded(_));
            self.known.insert(ino, slot);
            if taken {
                // Published as each pad is taken rather than at the end of the
                // scan: an application started in between would otherwise be
                // told it may read over hidraw a pad this guard is already
                // holding.
                self.publish();
            }
        }

        let before = self.known.len();
        self.known.retain(|ino, slot| {
            if present.contains(ino) {
                return true;
            }
            if let Slot::Guarded(pad) = slot {
                tracing::info!(pad = %pad.path.display(), "pad unplugged");
            }
            false
        });
        if self.known.len() != before {
            self.publish();
        }
    }

    /// Decide what one device node is, and take it if it is a pad.
    fn consider(&mut self, path: &Path, attempts: u8) -> Slot {
        if is_virtual(path) {
            return Slot::LeftAlone;
        }

        let device = match Device::open(path) {
            Ok(device) => device,
            Err(err) if err.kind() == ErrorKind::PermissionDenied => {
                // Every keyboard and mouse on the machine lands here, and so
                // does a pad for the moment between its node appearing and its
                // ACL arriving. Only the second is worth another look.
                return Slot::Waiting(attempts.saturating_sub(1));
            }
            Err(_) => return Slot::LeftAlone,
        };

        if !worth_guarding(device.supported_keys()) {
            return Slot::LeftAlone;
        }

        let name = device.name().unwrap_or_default().to_string();
        match take(device, path) {
            Ok(pad) => {
                tracing::info!(
                    pad = %name,
                    node = %path.display(),
                    vendor = format_args!("{:#06x}", pad.id.vendor),
                    product = format_args!("{:#06x}", pad.id.product),
                    "guide button taken; applications get the rest of this pad"
                );
                Slot::Guarded(Box::new(pad))
            }
            Err(err) => {
                tracing::warn!(
                    pad = %name,
                    node = %path.display(),
                    %err,
                    "could not take this pad's guide button; it will also reach applications"
                );
                Slot::LeftAlone
            }
        }
    }

    /// Write the guarded pads where the launcher can read them.
    fn publish(&self) {
        let mut held: Vec<Held> = Vec::new();
        for slot in self.known.values() {
            let Slot::Guarded(pad) = slot else { continue };
            held.push(Held {
                id: pad.id,
                node: pad.path.clone(),
            });
        }
        *GUARDED.lock().unwrap_or_else(|err| err.into_inner()) = held;
    }
}

impl Pad {
    /// Read everything the pad has sent, repeat what may be repeated, and
    /// answer any rumble the application has uploaded.
    ///
    /// Records the guide button's state on the pad itself, and reports device
    /// removal as an error so the caller can let go of the pad.
    fn pump(&mut self, edges: &mut Edges) -> std::io::Result<()> {
        loop {
            let events: Vec<InputEvent> = match self.device.fetch_events() {
                Ok(events) => events.collect(),
                Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                Err(err) => return Err(err),
            };
            if events.is_empty() {
                break;
            }
            for event in events {
                if event.event_type() != EventType::SYNCHRONIZATION {
                    self.packet.push(event);
                    continue;
                }
                let sifted = sift(&self.packet);
                self.packet.clear();
                match sifted.guide {
                    Some(true) => {
                        edges.pressed = true;
                        self.guide_down = true;
                    }
                    Some(false) => {
                        edges.released = true;
                        self.guide_down = false;
                    }
                    None => {}
                }
                if !sifted.events.is_empty() {
                    // `emit` ends what it writes with a `SYN_REPORT` of its
                    // own, so the replacement reports the same moments the pad
                    // did — one packet in, one packet out.
                    if let Err(err) = self.replacement.emit(&sifted.events) {
                        tracing::warn!(pad = %self.path.display(), %err, "could not repeat a pad packet");
                    }
                }
            }
        }

        self.rumble();
        Ok(())
    }

    /// Pass force feedback the other way: from the application, through the
    /// replacement, onto the pad that can actually shake.
    ///
    /// Rumble is the one thing that travels backwards here, and it is the
    /// reason the replacement declares force feedback at all. Without this a
    /// guarded pad would be a pad that had gone quiet in every game that shakes
    /// it, which is not a trade for one button.
    fn rumble(&mut self) {
        loop {
            let events: Vec<InputEvent> = match self.replacement.fetch_events() {
                Ok(events) => events.collect(),
                Err(err) if err.kind() == ErrorKind::WouldBlock => return,
                Err(err) => {
                    tracing::warn!(pad = %self.path.display(), %err, "could not read the replacement pad");
                    return;
                }
            };
            if events.is_empty() {
                return;
            }
            for event in events {
                match event.destructure() {
                    EventSummary::UInput(event, UInputCode::UI_FF_UPLOAD, _) => {
                        self.upload(event);
                    }
                    EventSummary::UInput(event, UInputCode::UI_FF_ERASE, _) => {
                        self.erase(event);
                    }
                    EventSummary::ForceFeedback(_, effect, value) => {
                        self.play(effect.0, value);
                    }
                    _ => {}
                }
            }
        }
    }

    /// An effect the application has just uploaded to the replacement, put on
    /// the real pad.
    fn upload(&mut self, event: evdev::UInputEvent) {
        let mut upload = match self.replacement.process_ff_upload(event) {
            Ok(upload) => upload,
            Err(err) => {
                tracing::warn!(pad = %self.path.display(), %err, "could not read a rumble upload");
                return;
            }
        };
        let id = upload.effect_id();
        let data = upload.effect();
        // A negative id would mean the replacement's own kernel side had not
        // allocated one, which does not happen: the force-feedback core picks
        // the slot before it asks us to fill it.
        let result = match self.effects.get_mut(&(id.max(0) as u16)) {
            // The same slot again is a game changing how hard the pad is
            // shaking, which happens continuously while it shakes. Updating the
            // effect in place is what keeps that from being an erase and an
            // upload per change.
            Some(effect) => effect.update(data),
            None => self.device.upload_ff_effect(data).map(|effect| {
                self.effects.insert(id.max(0) as u16, effect);
            }),
        };
        match result {
            Ok(()) => upload.set_retval(0),
            Err(err) => {
                tracing::debug!(pad = %self.path.display(), %err, "the pad refused a rumble effect");
                upload.set_retval(-1);
            }
        }
    }

    fn erase(&mut self, event: evdev::UInputEvent) {
        let mut erase = match self.replacement.process_ff_erase(event) {
            Ok(erase) => erase,
            Err(err) => {
                tracing::warn!(pad = %self.path.display(), %err, "could not read a rumble erase");
                return;
            }
        };
        // Dropping the effect is what erases it from the pad.
        self.effects.remove(&(erase.effect_id() as u16));
        erase.set_retval(0);
    }

    /// The application starting or stopping an effect, or setting the gain
    /// every effect is scaled by.
    fn play(&mut self, code: u16, value: i32) {
        if code == evdev::FFEffectCode::FF_GAIN.0 {
            let _ = self.device.set_ff_gain(value.clamp(0, 0xffff) as u16);
            return;
        }
        if code == evdev::FFEffectCode::FF_AUTOCENTER.0 {
            let _ = self.device.set_ff_autocenter(value.clamp(0, 0xffff) as u16);
            return;
        }
        let Some(effect) = self.effects.get_mut(&code) else {
            return;
        };
        // The pad's own id for this effect need not be the replacement's, so
        // the effect is played through the handle rather than by repeating the
        // event.
        let _ = if value > 0 {
            effect.play(value)
        } else {
            effect.stop()
        };
    }
}

/// Grab a pad and put a replacement in its place, or leave it exactly as it
/// was found.
///
/// The grab comes first on purpose. Building the replacement first would put a
/// second pad on the machine for as long as it took to fail — every game
/// watching for controllers would announce it, and announce its disappearance
/// afterwards. Grabbing first costs the few milliseconds of a pad that reports
/// nothing, which nobody can see, and every failure path below hands it back.
fn take(mut device: Device, path: &Path) -> std::io::Result<Pad> {
    let id = PadId {
        vendor: device.input_id().vendor(),
        product: device.input_id().product(),
    };
    device.grab()?;
    device.set_nonblocking(true)?;

    let mut replacement = build_replacement(&device)?;
    wait_for_node(&mut replacement)?;
    set_nonblocking(replacement.as_raw_fd())?;

    Ok(Pad {
        path: path.to_path_buf(),
        device,
        replacement,
        id,
        packet: Vec::new(),
        guide_down: false,
        effects: HashMap::new(),
    })
}

/// Build the pad an application sees: the same device in every respect an
/// application can ask about.
fn build_replacement(device: &Device) -> std::io::Result<VirtualDevice> {
    let name = device.name().unwrap_or("Gamepad").to_string();
    let mut builder = VirtualDevice::builder()?
        .name(&name)
        .input_id(device.input_id())
        .with_properties(device.properties())?;

    if let Some(keys) = device.supported_keys() {
        builder = builder.with_keys(keys)?;
    }
    if let Some(axes) = device.supported_relative_axes() {
        builder = builder.with_relative_axes(axes)?;
    }
    if let Some(misc) = device.misc_properties() {
        builder = builder.with_msc(misc)?;
    }
    if let Some(switches) = device.supported_switches() {
        builder = builder.with_switches(switches)?;
    }
    if let Some(ff) = device.supported_ff() {
        builder = builder
            .with_ff(ff)?
            .with_ff_effects_max(device.max_ff_effects() as u32);
    }
    for (axis, info) in device.get_absinfo()? {
        builder = builder.with_absolute_axis(&UinputAbsSetup::new(axis, info))?;
    }

    builder.build()
}

/// Wait until a `uinput` device has a node an application can open, and say
/// which node that is.
///
/// udev makes the node and then gives it the permissions that let a session
/// read it, and both happen after the device itself exists. Until they have,
/// the pad has been taken and nothing has been given back.
///
/// Shared with [`crate::steam_stand_in`], which makes a gamepad for a pad the
/// kernel drives no part of. Its wait is not this one's — nothing has been
/// taken away there, so a node that never arrives costs an application a
/// controller rather than the user their own — but the waiting is the same
/// waiting, and one of it is enough.
pub(crate) fn wait_for_node(device: &mut VirtualDevice) -> std::io::Result<PathBuf> {
    let deadline = Instant::now() + REPLACEMENT_PATIENCE;
    let mut last = std::io::Error::new(ErrorKind::NotFound, "the replacement pad got no node");
    loop {
        let nodes = device
            .enumerate_dev_nodes_blocking()
            .map(|nodes| nodes.flatten().collect::<Vec<_>>())
            .unwrap_or_default();
        for node in nodes {
            match std::fs::File::open(&node) {
                Ok(_) => return Ok(node),
                Err(err) => last = err,
            }
        }
        if Instant::now() >= deadline {
            return Err(last);
        }
        std::thread::sleep(REPLACEMENT_POLL);
    }
}

/// Whether a device node belongs to something `uinput` made.
///
/// Everything virtual hangs off `/sys/devices/virtual`, and a device node's
/// entry under `/sys/class/input` is a symlink into wherever its device really
/// is. Nothing else distinguishes a replacement pad from the pad it replaces —
/// that is the whole point of one — so this is the test that keeps the guard
/// from cloning its own work.
pub(crate) fn is_virtual(path: &Path) -> bool {
    let Some(name) = path.file_name() else {
        return false;
    };
    let link = Path::new("/sys/class/input").join(name);
    std::fs::read_link(link)
        .map(|target| {
            target
                .components()
                .any(|part| part.as_os_str() == "virtual")
        })
        .unwrap_or(false)
}

fn set_nonblocking(fd: std::os::fd::RawFd) -> std::io::Result<()> {
    // SAFETY: `fd` is owned by the caller and open for the duration of the
    // call; both fcntl commands here take no pointer arguments.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Two tests write the guarded-pad list, which is one list for the whole
/// process however many tests are running at once.
#[cfg(test)]
static SERIAL: Mutex<()> = Mutex::new(());

/// Holds [`SERIAL`], and leaves [`GUARDED`] the way it was found.
///
/// The clearing is the half that earns its keep after a failure. A test that
/// panics never reaches its own tidying, and what it leaves behind is a
/// process-wide list naming pads that are not there — which surfaces as a
/// failure in whichever test asks for that list next, about something that
/// test never touched. One broken thing should be one failing test, so the
/// list goes back to empty on the way out of every test that is allowed to
/// write it, panic or no panic, and while [`SERIAL`] is still held.
#[cfg(test)]
struct Serial(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

#[cfg(test)]
impl Drop for Serial {
    fn drop(&mut self) {
        GUARDED
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .clear();
    }
}

#[cfg(test)]
fn serial() -> Serial {
    Serial(SERIAL.lock().unwrap_or_else(|err| err.into_inner()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use evdev::AttributeSet;

    fn key(code: KeyCode, value: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, code.0, value)
    }

    fn scancode(value: i32) -> InputEvent {
        InputEvent::new(EventType::MISC.0, MiscCode::MSC_SCAN.0, value)
    }

    fn codes(events: &[InputEvent]) -> Vec<(u16, u16, i32)> {
        events
            .iter()
            .map(|event| (event.event_type().0, event.code(), event.value()))
            .collect()
    }

    /// A pad, spelled the way `SDL_HIDAPI_IGNORE_DEVICES` spells one.
    fn pad(vendor: u16, product: u16) -> PadId {
        PadId { vendor, product }
    }

    /// The list an application is handed holds both halves: the pads the guard
    /// took from `/dev/input`, and the pad this shell drives because the kernel
    /// does not. Both have somewhere else for a controller to be read.
    #[test]
    fn an_application_is_asked_to_ignore_every_pad_the_shell_stands_in_front_of() {
        let guarded = [pad(0x045e, 0x028e)];
        let driven = [pad(0x28de, 0x1304)];
        assert_eq!(
            spelled(pads_to_ignore(Reader::AnApplication, &guarded, &driven)).as_deref(),
            Some("0x045e/0x028e,0x28de/0x1304")
        );
    }

    /// Valve's client is handed the first half and never the second. The pad
    /// this shell drives is the pad the client drives, and a client that cannot
    /// see it is a client with no controller to give a game — which is the whole
    /// of why the two lists are not one.
    #[test]
    fn valves_client_is_never_asked_to_ignore_the_pad_it_drives() {
        let guarded = [pad(0x045e, 0x028e)];
        let driven = [pad(0x28de, 0x1304)];
        assert_eq!(
            spelled(pads_to_ignore(Reader::ValvesClient, &guarded, &driven)).as_deref(),
            Some("0x045e/0x028e")
        );
    }

    /// With nothing to name, nothing is said at all — an empty variable would
    /// be an argument with whatever the user set.
    #[test]
    fn a_reader_with_nothing_to_ignore_is_told_nothing() {
        assert_eq!(
            spelled(pads_to_ignore(Reader::AnApplication, &[], &[])),
            None
        );
        assert_eq!(
            spelled(pads_to_ignore(Reader::ValvesClient, &[], &[])),
            None
        );
    }

    /// Two identical controllers are one line: SDL reads the list once per
    /// entry, and a repeated pad would be read twice.
    #[test]
    fn the_same_pad_twice_is_one_entry() {
        let guarded = [pad(0x054c, 0x0ce6), pad(0x054c, 0x0ce6)];
        assert_eq!(
            spelled(pads_to_ignore(Reader::AnApplication, &guarded, &guarded)).as_deref(),
            Some("0x054c/0x0ce6")
        );
    }

    /// The whole rule, in one direction: an application sees the pad's other
    /// buttons.
    #[test]
    fn everything_but_the_guide_button_is_repeated() {
        let packet = [key(KeyCode::BTN_SOUTH, 1), scancode(0x90001)];
        let sifted = sift(&packet);
        assert_eq!(sifted.guide, None);
        assert_eq!(codes(&sifted.events), codes(&packet));
    }

    /// And in the other: it never sees the guide button, either edge of it.
    #[test]
    fn the_guide_button_is_never_repeated() {
        for (value, expected) in [(1, Some(true)), (0, Some(false))] {
            let sifted = sift(&[key(GUIDE, value)]);
            assert_eq!(sifted.guide, expected);
            assert!(
                sifted.events.is_empty(),
                "a packet that was only the guide button must leave nothing to send"
            );
        }
    }

    /// The scancode names the button as plainly as the button does, so it goes
    /// with it.
    #[test]
    fn the_scancode_of_a_guide_press_goes_with_it() {
        let sifted = sift(&[scancode(0x9000d), key(GUIDE, 1)]);
        assert_eq!(sifted.guide, Some(true));
        assert!(sifted.events.is_empty());
    }

    /// A guide press arriving in the same moment as another button costs that
    /// button its scancode and nothing else. The press itself gets through,
    /// because that one belongs to the application.
    #[test]
    fn a_button_pressed_with_the_guide_button_still_arrives() {
        let sifted = sift(&[
            scancode(0x9000d),
            key(GUIDE, 1),
            scancode(0x90001),
            key(KeyCode::BTN_SOUTH, 1),
        ]);
        assert_eq!(sifted.guide, Some(true));
        assert_eq!(codes(&sifted.events), codes(&[key(KeyCode::BTN_SOUTH, 1)]));
    }

    /// A held key repeating says nothing new, and must not be mistaken for a
    /// fresh press — which would clear a chord the hold had already been spent
    /// on.
    #[test]
    fn a_repeat_of_the_held_guide_button_is_not_an_edge() {
        let sifted = sift(&[key(GUIDE, 2)]);
        assert_eq!(sifted.guide, None);
        assert!(sifted.events.is_empty());
    }

    /// Sticks are the other half of a pad, and nothing here touches them.
    #[test]
    fn axes_pass_through_untouched() {
        let stick = InputEvent::new(EventType::ABSOLUTE.0, 0, -18000);
        let sifted = sift(&[key(GUIDE, 0), stick]);
        assert_eq!(sifted.guide, Some(false));
        assert_eq!(codes(&sifted.events), codes(&[stick]));
    }

    /// What the guard takes, and the one thing it must never take.
    #[test]
    fn only_a_pad_with_a_guide_button_and_no_letters_is_taken() {
        let mut none = AttributeSet::<KeyCode>::new();
        none.insert(KeyCode::BTN_SOUTH);
        assert!(
            !worth_guarding(Some(&none)),
            "a pad with no guide button has nothing to keep from anybody"
        );

        let mut pad = AttributeSet::<KeyCode>::new();
        pad.insert(KeyCode::BTN_SOUTH);
        pad.insert(GUIDE);
        assert!(worth_guarding(Some(&pad)));

        // The failure this rules out is a session with nothing to type on.
        for typing in [KeyCode::KEY_A, KeyCode::KEY_SPACE] {
            let mut keyboard = pad.clone();
            keyboard.insert(typing);
            assert!(
                !worth_guarding(Some(&keyboard)),
                "a device that can type is left alone, guide button or not"
            );
        }

        assert!(
            !worth_guarding(None),
            "and so is one with no buttons at all"
        );
    }

    /// The list handed to applications names pads the way SDL's hint is
    /// spelled, and says nothing at all while nothing stands in front of one.
    ///
    /// Both of its sources are held still for the length of this: the guard's
    /// own pads, and the gamepad [`crate::steam_hid`] makes for the pad with no
    /// gamepad node. The two locks are always taken in this order — the guard's
    /// tests reach for the driver's and never the other way about — so there is
    /// no pair of tests that could wait on each other.
    #[test]
    fn the_ignore_list_is_empty_until_a_pad_is_held() {
        let _serial = serial();
        let _driver = crate::steam_hid::serial();
        assert_eq!(hidapi_ignore_list(), None);
        let held = |vendor, product, node: &str| Held {
            id: PadId { vendor, product },
            node: PathBuf::from(node),
        };
        *GUARDED.lock().unwrap() = vec![
            held(0x28de, 0x1205, "/dev/input/event20"),
            held(0x054c, 0x0ce6, "/dev/input/event21"),
            // The same pad twice is one line, not two: a machine can have two
            // of one model, and SDL reads this hint as a list of *models*.
            held(0x054c, 0x0ce6, "/dev/input/event22"),
        ];
        assert_eq!(
            hidapi_ignore_list().as_deref(),
            Some("0x28de/0x1205,0x054c/0x0ce6")
        );

        // And the nodes themselves, which is what says which of two identical
        // controllers is the one that has gone quiet.
        assert_eq!(
            grabbed_nodes(),
            vec![
                PathBuf::from("/dev/input/event20"),
                PathBuf::from("/dev/input/event21"),
                PathBuf::from("/dev/input/event22"),
            ]
        );
        // And the driver's own stand-in joins the same list, on the same
        // terms, for a pad the guard could never take: it has no gamepad node
        // to grab, so what stands in front of it is a gamepad this shell built.
        GUARDED.lock().unwrap().clear();
        crate::steam_hid::pretend_a_stand_in_exists(true);
        assert_eq!(hidapi_ignore_list().as_deref(), Some("0x28de/0x1304"));
        crate::steam_hid::pretend_a_stand_in_exists(false);

        assert_eq!(hidapi_ignore_list(), None);
        assert!(grabbed_nodes().is_empty());
    }

    /// A guard that was never started is a guard that reports nothing, rather
    /// than one that has to be asked whether it is there.
    #[test]
    fn a_disabled_guard_reports_no_edges() {
        let guard = PadGuard::new(false);
        assert_eq!(guard.take_edges(), Edges::default());
        assert!(!guard.guide_held());
    }
}

/// The guard against a real pad, on a real kernel.
///
/// Everything above this is the rule; this is whether the machine keeps it.
/// A controller is made for the test with `uinput`, handed to the guard, and
/// then read back exactly the way an application reads a controller — which is
/// the only way to find out whether an application can still see the button.
///
/// Skipped, rather than failed, where `/dev/uinput` cannot be written: a build
/// machine without it is not a shell with a broken guard.
#[cfg(test)]
mod hardware_tests {
    use super::{serial, *};
    use evdev::{
        AbsInfo, AbsoluteAxisCode, AttributeSet, BusType, FFEffectCode, FFEffectData, FFEffectKind,
        FFReplay, InputId, KeyEvent,
    };
    use std::sync::atomic::AtomicUsize;

    /// The made-up pad's own identity. Never a real one: a test that took a
    /// vendor and product off this machine would be a test that only passes on
    /// it, and the guard is supposed to work by capability rather than by
    /// recognising anybody.
    fn test_id() -> InputId {
        InputId::new(BusType::BUS_USB, 0xf00d, 0x0bad, 0x0111)
    }
    const TEST_NAME: &str = "LineXinBar Test Pad";

    /// How long the test will wait for the kernel and udev to catch up.
    const PATIENCE: Duration = Duration::from_secs(2);

    fn uinput_is_available() -> bool {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/uinput")
            .is_ok()
    }

    /// A pad that does not exist, with a guide button, a stick and rumble.
    fn make_pad() -> std::io::Result<(VirtualDevice, PathBuf)> {
        let mut keys = AttributeSet::<KeyCode>::new();
        for key in [
            KeyCode::BTN_SOUTH,
            KeyCode::BTN_EAST,
            KeyCode::BTN_TR,
            GUIDE,
            KeyCode::BTN_THUMBL,
        ] {
            keys.insert(key);
        }
        let mut rumble = AttributeSet::<FFEffectCode>::new();
        rumble.insert(FFEffectCode::FF_RUMBLE);

        let mut pad = VirtualDevice::builder()?
            .name(TEST_NAME)
            .input_id(test_id())
            .with_keys(&keys)?
            // Two of them, because a stick is two and because the gamepad API
            // the shell reads pads with refuses anything with fewer — which is
            // also why the replacement copies every axis a pad has.
            .with_absolute_axis(&UinputAbsSetup::new(
                AbsoluteAxisCode::ABS_X,
                AbsInfo::new(0, -32768, 32767, 16, 128, 0),
            ))?
            .with_absolute_axis(&UinputAbsSetup::new(
                AbsoluteAxisCode::ABS_Y,
                AbsInfo::new(0, -32768, 32767, 16, 128, 0),
            ))?
            .with_ff(&rumble)?
            .with_ff_effects_max(16)
            .build()?;

        let node = first_node(&mut pad)?;
        Ok((pad, node))
    }

    /// The device node of a `uinput` device, once it has one that can be
    /// opened.
    fn first_node(device: &mut VirtualDevice) -> std::io::Result<PathBuf> {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let nodes: Vec<PathBuf> = device
                .enumerate_dev_nodes_blocking()
                .map(|nodes| nodes.flatten().collect())
                .unwrap_or_default();
            if let Some(node) = nodes.into_iter().find(|node| {
                std::fs::File::open(node).is_ok()
                    && node
                        .file_name()
                        .is_some_and(|name| name.as_encoded_bytes().starts_with(b"event"))
            }) {
                return Ok(node);
            }
            if Instant::now() >= deadline {
                return Err(std::io::Error::new(
                    ErrorKind::NotFound,
                    "the test pad never got a device node",
                ));
            }
            std::thread::sleep(REPLACEMENT_POLL);
        }
    }

    /// Read whatever the application can see, for as long as it takes something
    /// to arrive.
    fn drain(app: &mut Device, guard: &mut Pad, edges: &mut Edges) -> Vec<(u16, u16, i32)> {
        let deadline = Instant::now() + Duration::from_millis(400);
        let mut seen = Vec::new();
        while Instant::now() < deadline {
            guard.pump(edges).expect("the guarded pad is still there");
            match app.fetch_events() {
                Ok(events) => seen.extend(
                    events.map(|event| (event.event_type().0, event.code(), event.value())),
                ),
                Err(err) if err.kind() == ErrorKind::WouldBlock => {}
                Err(err) => panic!("the application could not read its pad: {err}"),
            }
            if !seen.is_empty() && !edges.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        seen
    }

    /// The rule itself, on hardware terms: the pad an application finds is the
    /// same pad, with the same identity, and the guide button never reaches it.
    #[test]
    fn an_application_gets_the_whole_pad_except_the_guide_button() {
        if !uinput_is_available() {
            crate::skipped("/dev/uinput cannot be opened here");
            return;
        }

        let (mut fake, node) = make_pad().expect("a test pad can be made");
        // The guard would never take this one by itself, and that is the point
        // of the rule it is being made to break here: everything `uinput` made
        // is left alone, or the guard would guard its own work.
        assert!(
            is_virtual(&node),
            "a device made by uinput must look virtual to the guard"
        );

        let device = Device::open(&node).expect("the test pad can be opened");
        let mut guard = take(device, &node).expect("the guard can take the test pad");

        let replacement = first_node(&mut guard.replacement).expect("the replacement has a node");
        let mut app = Device::open(&replacement).expect("an application can open the replacement");
        app.set_nonblocking(true).expect("nonblocking reads");

        // What SDL looks a controller up by. All of it has to match, or the
        // game gets a pad its mapping database has never heard of.
        assert_eq!(app.name(), Some(TEST_NAME));
        assert_eq!(app.input_id().vendor(), test_id().vendor());
        assert_eq!(app.input_id().product(), test_id().product());
        assert_eq!(app.input_id().version(), test_id().version());
        assert_eq!(app.input_id().bus_type(), test_id().bus_type());
        // Declared, and never sent — the two are different things, and it is
        // this one that keeps SDL's button numbering the same on both pads.
        assert!(
            app.supported_keys()
                .is_some_and(|keys| keys.contains(GUIDE)),
            "the replacement must still say it has a guide button"
        );
        assert!(
            app.supported_ff()
                .is_some_and(|ff| ff.contains(FFEffectCode::FF_RUMBLE)),
            "the replacement must still be able to rumble"
        );
        assert!(app
            .supported_absolute_axes()
            .is_some_and(|axes| axes.contains(AbsoluteAxisCode::ABS_X)));

        let mut edges = Edges::default();

        // An ordinary button: the application's, and it arrives.
        fake.emit(&[*KeyEvent::new(KeyCode::BTN_SOUTH, 1)])
            .expect("the test pad can report a button");
        let seen = drain(&mut app, &mut guard, &mut edges);
        assert!(
            seen.contains(&(EventType::KEY.0, KeyCode::BTN_SOUTH.0, 1)),
            "the application should have seen an ordinary button: {seen:?}"
        );
        assert!(edges.is_empty(), "and it is not the guide button");

        // The guide button: the shell's, and it does not.
        fake.emit(&[*KeyEvent::new(GUIDE, 1)])
            .expect("the test pad can report its guide button");
        let seen = drain(&mut app, &mut guard, &mut edges);
        assert!(
            !seen
                .iter()
                .any(|(kind, code, _)| *kind == EventType::KEY.0 && *code == GUIDE.0),
            "the application must never see the guide button: {seen:?}"
        );
        assert!(edges.pressed, "the shell must see it instead");
        assert!(guard.guide_down, "and know it is being held");

        fake.emit(&[*KeyEvent::new(GUIDE, 0)])
            .expect("the test pad can let its guide button go");
        drain(&mut app, &mut guard, &mut edges);
        assert!(edges.released);
        assert!(!guard.guide_down);

        // Letting go of the pad puts it back: the grab goes with the
        // descriptor, and the replacement goes with it.
        drop(guard);
        assert!(
            Device::open(&node).is_ok(),
            "the pad is openable again once the guard has finished with it"
        );
    }

    /// The shell's own half of the bargain: everything that is not the guide
    /// button still arrives, and arrives once.
    ///
    /// The shell reads pads through GilRs, and after the guard there are two
    /// devices for one controller — the pad, which is grabbed and silent, and
    /// the replacement, which is not. A pad reported twice would move the bar
    /// two rows on one press, which is the failure this rules out.
    #[test]
    fn the_shell_reads_every_other_button_exactly_once() {
        if !uinput_is_available() {
            crate::skipped("/dev/uinput cannot be opened here");
            return;
        }

        let (mut fake, node) = make_pad().expect("a test pad can be made");
        let mut gilrs = match gilrs::GilrsBuilder::new()
            .with_force_feedback(false)
            .build()
        {
            Ok(gilrs) => gilrs,
            Err(err) => {
                crate::skipped(&format!("no gamepad API here ({err})"));
                return;
            }
        };
        // Wait for GilRs to notice the pad at all before it is taken, which is
        // the order a running shell meets one in.
        if !settle(&mut gilrs, |gilrs| gilrs.gamepads().count() == 1) {
            crate::skipped("the gamepad API never saw the test pad");
            return;
        }

        let device = Device::open(&node).expect("the test pad can be opened");
        let mut guard = take(device, &node).expect("the guard can take the test pad");
        settle(&mut gilrs, |gilrs| gilrs.gamepads().count() >= 2);

        let mut edges = Edges::default();
        fake.emit(&[*KeyEvent::new(KeyCode::BTN_SOUTH, 1)])
            .expect("the test pad can report a button");
        fake.emit(&[*KeyEvent::new(GUIDE, 1)])
            .expect("and its guide button");

        let deadline = Instant::now() + PATIENCE;
        let mut presses = Vec::new();
        while Instant::now() < deadline {
            guard.pump(&mut edges).expect("the pad is still there");
            while let Some(event) = gilrs.next_event() {
                if let gilrs::EventType::ButtonPressed(button, code) = event.event {
                    presses.push((button, code.into_u32()));
                }
            }
            if edges.pressed && !presses.is_empty() {
                std::thread::sleep(Duration::from_millis(50));
                while let Some(event) = gilrs.next_event() {
                    if let gilrs::EventType::ButtonPressed(button, code) = event.event {
                        presses.push((button, code.into_u32()));
                    }
                }
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }

        assert!(edges.pressed, "the shell should have the guide button");
        assert_eq!(
            presses.len(),
            1,
            "one press of one button, however many devices carry it: {presses:?}"
        );
        assert!(
            !presses
                .iter()
                .any(|(button, code)| *button == gilrs::Button::Mode || *code == 0x1_013c),
            "and none of them is the guide button: {presses:?}"
        );
    }

    /// Give GilRs its hot-plug events until it agrees with `settled`.
    fn settle(gilrs: &mut gilrs::Gilrs, settled: impl Fn(&gilrs::Gilrs) -> bool) -> bool {
        let deadline = Instant::now() + PATIENCE;
        while Instant::now() < deadline {
            while gilrs.next_event().is_some() {}
            if settled(gilrs) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    /// The way a pad is actually found: a scan, and what the scan decides.
    ///
    /// The guard is pointed at a directory of the test's own holding one
    /// `eventN` name, because the pad it has to find here is one `uinput` made
    /// and the guard refuses those on sight — under its own name in
    /// `/dev/input` this pad would be left alone, correctly, and nothing about
    /// finding a pad would be exercised at all.
    #[test]
    fn a_scan_finds_a_pad_takes_it_and_lets_it_go() {
        if !uinput_is_available() {
            crate::skipped("/dev/uinput cannot be opened here");
            return;
        }
        // Both sources of the ignore list this asserts on, held still, and in
        // the order every test that needs both takes them. See
        // [`tests::the_ignore_list_is_empty_until_a_pad_is_held`].
        let _serial = serial();
        let _driver = crate::steam_hid::serial();

        let (fake, node) = make_pad().expect("a test pad can be made");
        let dir = std::env::temp_dir().join(format!("lxb-pad-guard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a directory to look in");
        let seen_as = dir.join("event0");
        std::os::unix::fs::symlink(&node, &seen_as).expect("the pad can be put there");

        let mut worker = Worker::new(Arc::new(Shared::default()));
        worker.dir = dir.clone();

        worker.scan();
        assert_eq!(
            worker
                .known
                .values()
                .filter(|slot| matches!(slot, Slot::Guarded(_)))
                .count(),
            1,
            "the scan should have found the pad and taken it"
        );
        assert_eq!(
            hidapi_ignore_list().as_deref(),
            Some("0xf00d/0x0bad"),
            "and told applications to read it through /dev/input"
        );

        // A pad already held is not taken twice: a second grab and a second
        // stand-in would be a second controller as far as every game is
        // concerned.
        worker.scan();
        assert_eq!(worker.known.len(), 1);

        // Unplugged. The node goes, and the guard lets go of everything it was
        // holding on the pad's behalf.
        //
        // Waited out by inode rather than by name, and then taken away rather
        // than left dangling, because `eventN` is handed straight back: the
        // kernel gives the very next `uinput` device made on this machine the
        // name this one just released, measured at forty tries out of forty,
        // with a new inode each time. The other hardware tests here make those
        // devices, in parallel, out of the same fixture — so a symlink left
        // pointing at the name resolves to somebody else's pad, which the scan
        // takes, correctly, and which this test would then read as its own pad
        // that the guard had failed to let go of.
        let was = std::fs::metadata(&seen_as).map(|meta| meta.ino()).ok();
        drop(fake);
        let deadline = Instant::now() + PATIENCE;
        while std::fs::metadata(&seen_as).map(|meta| meta.ino()).ok() == was
            && Instant::now() < deadline
        {
            std::thread::sleep(REPLACEMENT_POLL);
        }
        assert_ne!(
            std::fs::metadata(&seen_as).map(|meta| meta.ino()).ok(),
            was,
            "the test pad's node never went away"
        );
        std::fs::remove_file(&seen_as).expect("the node goes with the pad");
        worker.scan();
        assert!(
            !worker
                .known
                .values()
                .any(|slot| matches!(slot, Slot::Guarded(_))),
            "an unplugged pad should not still be held"
        );
        assert_eq!(hidapi_ignore_list(), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Rumble, the one thing that travels the other way. A game that shakes a
    /// guarded pad has to shake the real one.
    #[test]
    fn rumble_reaches_the_real_pad() {
        if !uinput_is_available() {
            crate::skipped("/dev/uinput cannot be opened here");
            return;
        }

        let (mut fake, node) = make_pad().expect("a test pad can be made");
        set_nonblocking(fake.as_raw_fd()).expect("nonblocking reads on the test pad");
        let device = Device::open(&node).expect("the test pad can be opened");
        let mut guard = take(device, &node).expect("the guard can take the test pad");
        let replacement = first_node(&mut guard.replacement).expect("the replacement has a node");

        // Three actors, because two of them block on each other: uploading an
        // effect waits for whoever owns the device to answer. The application
        // is this thread, the guard is one thread, and the pad that has to
        // answer *the guard* is another.
        let stop = Arc::new(AtomicBool::new(false));
        let uploads = Arc::new(AtomicUsize::new(0));
        let plays = Arc::new(AtomicUsize::new(0));

        let guard_stop = Arc::clone(&stop);
        let guarding = std::thread::spawn(move || {
            let mut edges = Edges::default();
            while !guard_stop.load(Ordering::Relaxed) {
                let _ = guard.pump(&mut edges);
                std::thread::sleep(Duration::from_millis(2));
            }
            guard
        });

        let pad_stop = Arc::clone(&stop);
        let seen_uploads = Arc::clone(&uploads);
        let seen_plays = Arc::clone(&plays);
        let padding = std::thread::spawn(move || {
            while !pad_stop.load(Ordering::Relaxed) {
                let events: Vec<InputEvent> = match fake.fetch_events() {
                    Ok(events) => events.collect(),
                    Err(_) => Vec::new(),
                };
                {
                    for event in events {
                        match event.destructure() {
                            EventSummary::UInput(event, UInputCode::UI_FF_UPLOAD, _) => {
                                if let Ok(mut upload) = fake.process_ff_upload(event) {
                                    upload.set_retval(0);
                                    seen_uploads.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                            EventSummary::UInput(event, UInputCode::UI_FF_ERASE, _) => {
                                if let Ok(mut erase) = fake.process_ff_erase(event) {
                                    erase.set_retval(0);
                                }
                            }
                            EventSummary::ForceFeedback(..) => {
                                seen_plays.fetch_add(1, Ordering::Relaxed);
                            }
                            _ => {}
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            fake
        });

        let mut app = Device::open(&replacement).expect("an application can open the replacement");
        let mut effect = app
            .upload_ff_effect(FFEffectData {
                direction: 0,
                trigger: Default::default(),
                replay: FFReplay {
                    length: 500,
                    delay: 0,
                },
                kind: FFEffectKind::Rumble {
                    strong_magnitude: 0xffff,
                    weak_magnitude: 0x8000,
                },
            })
            .expect("the application can upload rumble to the replacement pad");
        effect.play(1).expect("and play it");

        // The play travels as an event rather than an ioctl, so it takes a
        // turn of both threads to get there.
        let deadline = Instant::now() + PATIENCE;
        while plays.load(Ordering::Relaxed) == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }

        // Given back before either thread stops, and in this order. Erasing an
        // effect is a question asked of whoever owns the device, and the kernel
        // gives that question thirty seconds to be answered: an application
        // that let go of its rumble after the answering thread had gone would
        // wait out both of those timeouts.
        drop(effect);
        drop(app);
        std::thread::sleep(Duration::from_millis(50));

        stop.store(true, Ordering::Relaxed);
        let _ = guarding.join();
        let _ = padding.join();

        assert_eq!(
            uploads.load(Ordering::Relaxed),
            1,
            "the effect the application uploaded should have been put on the real pad"
        );
        assert!(
            plays.load(Ordering::Relaxed) >= 1,
            "and playing it should have reached the real pad too"
        );
    }
}
