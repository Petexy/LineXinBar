//! Putting a drive to use: mounting what nothing has mounted, unmounting it
//! again, and whether the machine mounts it by itself when it starts.
//!
//! [`crate::storage`] reads the drives out of the kernel, because a page that
//! only *reads* must not depend on a daemon. This module is the other half of
//! the bargain: everything here *changes* the machine, and the service that
//! owns a filesystem being mounted is UDisks — the same one GNOME's and KDE's
//! file managers ask. So the list is UDisks' own, read over the system bus,
//! and every change is a UDisks call with polkit answering through this
//! shell's own agent ([`crate::polkit`]). A machine without UDisks lists no
//! drive it could not mount anyway, which is the honest answer.
//!
//! ## The three things a press can do
//!
//! * **Mount.** A drive nothing has mounted is a row in Files; pressing it
//!   mounts it and steps into it. UDisks puts it at `/run/media/<user>/<label>`
//!   — or wherever the machine's mount table says, when it has an entry — and
//!   an internal disk makes polkit ask for an administrator's password, which
//!   the shell's own panel collects.
//! * **Unmount, or Safely remove.** On the menu of a mounted drive in Files,
//!   and on its page under Settings > Storage. A drive that can be unplugged is
//!   also turned off, so the person knows when it is safe to pull it out.
//! * **Mount at startup.** Settings > Storage > the drive. This is the
//!   system's setting rather than the shell's — an entry in the machine's
//!   mount table, written through UDisks exactly as GNOME Disks writes one — so
//!   the drive is there before anybody signs in, for every account, at the
//!   same place every time: `/mnt/<label>`, which is what keeps a Steam
//!   library on it valid from one boot to the next. See [`at_startup_as_root`]
//!   for why that one goes through `pkexec`.
//!
//! ## Plugged in, mounted
//!
//! A drive UDisks marks as one to mount by itself — a USB stick, an SD card:
//! its `HintAuto` — is mounted the moment it appears, and every one already
//! plugged in is mounted when the session starts, which is what GNOME and KDE
//! do. Without asking for anything: polkit lets the person at the machine
//! mount removable media without a password, and one that would have asked is
//! left alone rather than raising a panel nobody pressed anything for.
//!
//! Only a drive that has *appeared* is mounted this way. One the person
//! unmounted is still there, so it stays unmounted until they open it.
//!
//! ## On a worker, woken by UDisks
//!
//! A mount can wait on a disk spinning up and a password being typed, so the
//! calls are made on a thread of their own. A second thread listens to the
//! signals UDisks sends when anything about a drive changes and wakes the
//! first, which then reads the whole list again: UDisks describes a machine in
//! a few dozen objects, and one read after a burst of signals is simpler than
//! patching a copy with each of them.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

const UDISKS: &str = "org.freedesktop.UDisks2";
const MANAGER: &str = "/org/freedesktop/UDisks2";
const OBJECT_MANAGER: &str = "org.freedesktop.DBus.ObjectManager";
const BLOCK: &str = "org.freedesktop.UDisks2.Block";
const FILESYSTEM: &str = "org.freedesktop.UDisks2.Filesystem";
const PARTITION: &str = "org.freedesktop.UDisks2.Partition";
const LOOP: &str = "org.freedesktop.UDisks2.Loop";
const DRIVE: &str = "org.freedesktop.UDisks2.Drive";

/// Where a drive that is mounted at startup is put, under its own name.
///
/// The place GNOME Disks suggests, and the place this machine's other drives
/// already are — a drive the whole machine uses is not one account's, so it
/// does not belong under `/run/media/<user>`.
const STARTUP_ROOT: &str = "/mnt";

/// The mount options a drive mounted at startup is given.
///
/// What GNOME Disks writes, for the same reasons: `nosuid,nodev` because a
/// drive somebody plugged in is not trusted to carry setuid programs or device
/// nodes; `nofail` because a machine that cannot find the drive one morning
/// must still start; `x-gvfs-show` so other desktops list it as a drive rather
/// than as part of the system.
const STARTUP_OPTIONS: &str = "nosuid,nodev,nofail,x-gvfs-show";

/// Filesystems that have no owners of their own, so a drive mounted by the
/// machine would belong to root and nobody else could write to it. Mounted at
/// startup, they are given to the person who asked for it.
const NO_OWNERS: &[&str] = &["vfat", "exfat", "ntfs", "ntfs3", "msdos"];

/// How long the worker lets UDisks' signals settle before reading. A stick
/// being plugged in is a dozen signals over a few hundred milliseconds —
/// the drive, its table, each partition, then what probing found on it.
const SETTLE: Duration = Duration::from_millis(250);

/// The exit status [`at_startup_as_root`] uses for "written, but the drive
/// could not be moved to its new place now": something is using it where it
/// is, and it arrives there on the next start.
const LATER_STATUS: i32 = 4;

/// One filesystem UDisks knows about that somebody could mount.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Volume {
    /// The kernel's number for the device, `major << 8 | minor` as UDisks
    /// reports it. What a row carries to say which drive it is about — it is
    /// small and `Copy`, and it is the one name UDisks and the kernel agree on.
    pub number: u64,
    /// UDisks' object for the block device.
    pub object: String,
    /// The kernel's name for it, `sda1` — what [`crate::storage`] calls it,
    /// which is how the Storage page finds its own row here.
    pub device: String,
    pub uuid: Option<String>,
    /// The name the filesystem was given, when it was given one.
    pub label: Option<String>,
    /// The name the partition table gives it.
    pub part_name: Option<String>,
    pub size: u64,
    /// The filesystem: `ext4`, `ntfs`.
    pub kind: Option<String>,
    /// Everywhere it is mounted, the first being the one it is shown at.
    pub mounted_at: Vec<PathBuf>,
    /// Where the machine's mount table puts it, when it has an entry there —
    /// which is what "mount at startup" is.
    pub at_startup: Option<PathBuf>,
    /// Whether the mount table says it goes somewhere the machine keeps
    /// itself (`/var`, `/boot`) rather than somewhere somebody keeps things.
    pub systems: bool,
    /// Whether it is on a drive that can be unplugged: a stick, a card, a
    /// disk in a USB case.
    pub removable: bool,
    /// Whether that drive can be turned off from here, which is what makes
    /// Safely remove mean something.
    pub can_power_off: bool,
    /// Whether UDisks says it is the kind of thing to mount by itself as soon
    /// as it appears.
    pub automount: bool,
    /// The drive it is on.
    pub drive: Option<String>,
    /// Whether it is a file somebody attached as a disk — an image this
    /// account set up — rather than a partition of a real one.
    pub attached: bool,
}

impl Volume {
    /// What it is called: the name somebody gave it, then the partition
    /// table's, then its size — the same order Settings > Storage names a
    /// partition in, so a drive is called the same thing on both pages.
    pub fn title(&self) -> String {
        self.label
            .clone()
            .or_else(|| self.part_name.clone())
            .unwrap_or_else(|| {
                crate::message!("storage-unnamed", "size" => crate::appinfo::human_size(self.size))
            })
    }

    /// Where it is shown, when it is mounted.
    pub fn place(&self) -> Option<&Path> {
        self.mounted_at.first().map(PathBuf::as_path)
    }

    /// Whether the machine may be asked to mount it at startup: a partition
    /// of a real drive, with a name the mount table can use, that is not
    /// already somewhere the system keeps itself. A stick is left out — it is
    /// mounted the moment it is plugged in, and an entry for something that is
    /// usually not there is an entry to forget about.
    pub fn can_start_with_the_machine(&self) -> bool {
        !self.attached && !self.removable && !self.systems && self.uuid.is_some()
    }
}

/// What the worker is doing to a drive, while it does it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Doing {
    Mounting,
    Unmounting,
    Removing,
    AtStartup(bool),
}

/// Why a press did not do what it said.
///
/// Four words, not UDisks' own. What UDisks and polkit said goes to the
/// journal; what a person is told is what state the drive is in and whether
/// there is anything to do about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The person closed the password panel. Nothing to say: they know.
    Dismissed,
    /// The machine says this account may not.
    NotAllowed,
    /// Something is using the drive.
    Busy,
    /// Anything else.
    Failed,
    /// Mount at startup was written, and the drive moves to its new place on
    /// the next start because something is using it where it is now.
    Later,
}

/// What came of one press.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub number: u64,
    /// What the drive was called when it was pressed, for the panel that
    /// says what happened — a drive that was just turned off is not in the
    /// list any more to be asked.
    pub title: String,
    pub act: Act,
    /// Where it is mounted now, for a mount; nothing for the rest.
    pub result: Result<Option<PathBuf>, Refusal>,
    /// Whether the press was the one that opens it in Files, so the shell
    /// steps into it once it is there.
    pub open: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    Mount,
    Unmount,
    Remove,
    AtStartup(bool),
}

/// Every drive that could be mounted, and what is being done to which.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listing {
    pub volumes: Vec<Volume>,
    /// Drives a press is being carried out on, by number.
    pub doing: Vec<(u64, Doing)>,
    /// What the last Mount at startup press on a drive came to, when it did
    /// not simply work — the note its row carries until it is pressed again.
    pub startup_trouble: Vec<(u64, Refusal)>,
    /// Whether UDisks answered at all.
    pub service: bool,
}

impl Listing {
    pub fn volume(&self, number: u64) -> Option<&Volume> {
        self.volumes.iter().find(|volume| volume.number == number)
    }

    /// The one the kernel calls `device`.
    pub fn by_device(&self, device: &str) -> Option<&Volume> {
        self.volumes.iter().find(|volume| volume.device == device)
    }

    /// The one shown at `at`.
    pub fn mounted_at(&self, at: &Path) -> Option<&Volume> {
        self.volumes
            .iter()
            .find(|volume| volume.mounted_at.iter().any(|place| place == at))
    }

    pub fn doing(&self, number: u64) -> Option<Doing> {
        self.doing
            .iter()
            .find(|(of, _)| *of == number)
            .map(|(_, doing)| *doing)
    }

    pub fn startup_trouble(&self, number: u64) -> Option<Refusal> {
        self.startup_trouble
            .iter()
            .find(|(of, _)| *of == number)
            .map(|(_, trouble)| *trouble)
    }

    /// The drives Files offers to open: mounted nowhere, and not somewhere the
    /// system keeps itself.
    pub fn unmounted(&self) -> impl Iterator<Item = &Volume> {
        self.volumes
            .iter()
            .filter(|volume| volume.mounted_at.is_empty() && !volume.systems)
    }
}

// --- what the shell last heard -----------------------------------------------

/// The listing the shell last took from the worker, for the rows that are
/// written from it — Files' list of drives and Settings > Storage. Kept here
/// rather than handed to each, on the terms [`crate::users`] keeps its own:
/// both are built by functions that are not the shell, several times a minute.
static KNOWN: Mutex<Option<Listing>> = Mutex::new(None);

/// Hand the rows a fresh listing. `true` when it differs from the last one.
pub fn note(listing: Listing) -> bool {
    let mut held = KNOWN.lock().unwrap_or_else(|e| e.into_inner());
    let changed = held.as_ref() != Some(&listing);
    *held = Some(listing);
    changed
}

/// The listing the rows are written from. Empty until the worker has read
/// once, which is a few milliseconds into the session.
pub fn known() -> Listing {
    KNOWN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_default()
}

// --- the worker --------------------------------------------------------------

enum Ask {
    Mount { number: u64, open: bool },
    Unmount(u64),
    Remove(u64),
    AtStartup(u64, bool),
}

/// The drives, and the worker that keeps them true.
pub struct Drives {
    shared: Arc<Shared>,
}

struct Shared {
    state: Mutex<State>,
    signal: Condvar,
}

#[derive(Default)]
struct State {
    listing: Listing,
    /// Bumped whenever [`State::listing`] changes, so the shell can ask
    /// whether anything has once a frame without copying it.
    published: u64,
    asks: Vec<Ask>,
    outcomes: Vec<Outcome>,
    dirty: bool,
    done: bool,
}

impl Drives {
    /// Start the worker. It reads straight away — and mounts whatever stick
    /// was already plugged in — so Files has its drives before anybody can
    /// walk to it.
    pub fn start() -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            signal: Condvar::new(),
        });
        let worker = Arc::clone(&shared);
        if let Err(err) = std::thread::Builder::new()
            .name("lxb-drives".to_string())
            .spawn(move || Worker::new(worker).run())
        {
            tracing::warn!(
                ?err,
                "no worker thread; drives cannot be mounted from the shell"
            );
        }
        Self { shared }
    }

    pub fn published(&self) -> u64 {
        self.held().published
    }

    pub fn listing(&self) -> Listing {
        self.held().listing.clone()
    }

    /// What came of the presses that have finished since this was last asked.
    pub fn take_outcomes(&self) -> Vec<Outcome> {
        std::mem::take(&mut self.held().outcomes)
    }

    /// Mount it, and — when `open` — step into it once it is there.
    ///
    /// `false` when the drive is already being worked on, which makes a
    /// second press on a row that says "Mounting…" a press that is spent.
    pub fn mount(&self, number: u64, open: bool) -> bool {
        self.ask(number, Doing::Mounting, Ask::Mount { number, open })
    }

    pub fn unmount(&self, number: u64) -> bool {
        self.ask(number, Doing::Unmounting, Ask::Unmount(number))
    }

    /// Unmount everything on the drive it is on, and turn the drive off.
    pub fn remove(&self, number: u64) -> bool {
        self.ask(number, Doing::Removing, Ask::Remove(number))
    }

    pub fn at_startup(&self, number: u64, on: bool) -> bool {
        self.ask(number, Doing::AtStartup(on), Ask::AtStartup(number, on))
    }

    fn ask(&self, number: u64, doing: Doing, ask: Ask) -> bool {
        let mut state = self.held();
        if state.listing.doing(number).is_some() {
            return false;
        }
        // Said to be working straight away rather than when the worker wakes:
        // the row has to stop offering the press the moment it is made.
        state.listing.doing.push((number, doing));
        if matches!(doing, Doing::AtStartup(_)) {
            state
                .listing
                .startup_trouble
                .retain(|(of, _)| *of != number);
        }
        state.published += 1;
        state.asks.push(ask);
        state.dirty = true;
        self.shared.signal.notify_one();
        true
    }

    fn held(&self) -> std::sync::MutexGuard<'_, State> {
        self.shared.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Drop for Drives {
    fn drop(&mut self) {
        self.held().done = true;
        self.shared.signal.notify_all();
    }
}

struct Worker {
    shared: Arc<Shared>,
    bus: Option<zbus::blocking::Connection>,
    /// Every drive the last reading found, by what it is — so the next reading
    /// can tell a drive that has just appeared from one that was already here.
    /// `None` before the first, when everything is new.
    seen: Option<HashSet<(u64, Option<String>)>>,
}

impl Worker {
    fn new(shared: Arc<Shared>) -> Self {
        Self {
            shared,
            bus: None,
            seen: None,
        }
    }

    fn run(mut self) {
        self.connect();
        loop {
            // A burst of signals is one change, not a dozen, so a wake with no
            // press behind it waits for the burst to finish before reading.
            let settle = {
                let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
                state.asks.is_empty() && self.seen.is_some()
            };
            if settle {
                std::thread::sleep(SETTLE);
            }
            // The presses are taken and the wake is spent in one hold of the
            // lock. Taken before the pause and spent after it, a press made
            // during the pause was neither carried out nor able to wake the
            // worker again — found by a real Unmount that never happened.
            let asks = {
                let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
                if state.done {
                    return;
                }
                state.dirty = false;
                std::mem::take(&mut state.asks)
            };

            let outcomes: Vec<Outcome> = asks.into_iter().map(|ask| self.carry_out(ask)).collect();

            let mut listing = self.read();
            let fresh = to_mount_by_itself(self.seen.as_ref(), &listing.volumes);
            self.seen = Some(listing.volumes.iter().map(identity).collect());
            if !fresh.is_empty() {
                for object in &fresh {
                    self.mount_by_itself(object);
                }
                listing = self.read();
            }
            self.publish(listing, outcomes);
            self.wait();
        }
    }

    fn connect(&mut self) {
        match zbus::blocking::Connection::system() {
            Ok(bus) => {
                let watching = bus.clone();
                let shared = Arc::clone(&self.shared);
                if let Err(err) = std::thread::Builder::new()
                    .name("lxb-drives-watch".to_string())
                    .spawn(move || listen(watching, shared))
                {
                    tracing::warn!(
                        ?err,
                        "no thread to hear drives arrive; they are read on a press"
                    );
                }
                self.bus = Some(bus);
            }
            Err(err) => tracing::debug!(?err, "no system bus; no drive can be mounted"),
        }
    }

    fn wait(&self) {
        let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.done || state.dirty {
            return;
        }
        drop(self.shared.signal.wait(state));
    }

    fn read(&self) -> Listing {
        let Some(bus) = self.bus.as_ref() else {
            return Listing::default();
        };
        match managed_objects(bus) {
            Some(objects) => Listing {
                volumes: volumes(&objects, unsafe { libc::getuid() }),
                service: true,
                ..Listing::default()
            },
            None => Listing::default(),
        }
    }

    /// Put a reading where the shell will find it, with what came of every
    /// press carried out before it.
    ///
    /// In one hold of the lock, so the shell never sees a press finished
    /// against a listing that does not yet show it: a drive that has just
    /// been mounted is stepped into by the row that is now a folder, and that
    /// row is only there once the listing says it is mounted.
    fn publish(&self, listing: Listing, outcomes: Vec<Outcome>) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut doing = state.listing.doing.clone();
        let mut startup_trouble = std::mem::take(&mut state.listing.startup_trouble);
        for outcome in &outcomes {
            doing.retain(|(of, _)| *of != outcome.number);
            if let (Act::AtStartup(_), Err(refusal)) = (outcome.act, &outcome.result) {
                let refusal = *refusal;
                // A closed password panel is not trouble: the person knows.
                if refusal != Refusal::Dismissed {
                    startup_trouble.push((outcome.number, refusal));
                }
            }
        }
        // A drive that has gone takes what was said about it with it.
        startup_trouble.retain(|(of, _)| listing.volume(*of).is_some());
        let next = Listing {
            doing,
            startup_trouble,
            ..listing
        };
        if state.listing != next {
            state.listing = next;
            state.published += 1;
        }
        state.outcomes.extend(outcomes);
    }

    /// Mount a stick that has just been plugged in, asking nobody anything.
    fn mount_by_itself(&self, object: &str) {
        let Some(bus) = self.bus.as_ref() else {
            return;
        };
        match mount(bus, object, false) {
            Ok(at) => {
                tracing::info!(object, at = %at.display(), "mounted a drive that was plugged in")
            }
            // At debug: a drive the machine would have wanted a password for
            // is one nobody asked to open, and it waits in Files until they do.
            Err((refusal, why)) => {
                tracing::debug!(
                    object,
                    ?refusal,
                    why,
                    "a drive that was plugged in was left unmounted"
                )
            }
        }
    }

    fn carry_out(&self, ask: Ask) -> Outcome {
        let number = match &ask {
            Ask::Mount { number, .. }
            | Ask::Unmount(number)
            | Ask::Remove(number)
            | Ask::AtStartup(number, _) => *number,
        };
        let (act, open) = match &ask {
            Ask::Mount { open, .. } => (Act::Mount, *open),
            Ask::Unmount(_) => (Act::Unmount, false),
            Ask::Remove(_) => (Act::Remove, false),
            Ask::AtStartup(_, on) => (Act::AtStartup(*on), false),
        };
        // Read afresh rather than taken from the listing the press was made
        // on: it is the object UDisks has *now* that the call goes to.
        let listing = self.read();
        let Some(volume) = listing.volume(number).cloned() else {
            tracing::warn!(number, "the drive that was pressed has gone");
            return Outcome {
                number,
                title: String::new(),
                act,
                result: Err(Refusal::Failed),
                open,
            };
        };
        let result = match (self.bus.as_ref(), act) {
            (None, _) => Err(Refusal::Failed),
            (Some(bus), Act::Mount) => match mount(bus, &volume.object, true) {
                Ok(at) => {
                    tracing::info!(drive = volume.device, at = %at.display(), "mounted");
                    Ok(Some(at))
                }
                Err((refusal, why)) => {
                    tracing::warn!(
                        drive = volume.device,
                        ?refusal,
                        why,
                        "the drive was not mounted"
                    );
                    Err(refusal)
                }
            },
            (Some(bus), Act::Unmount) => unmount(bus, &volume.object)
                .map(|()| {
                    tracing::info!(drive = volume.device, "unmounted");
                    None
                })
                .map_err(|(refusal, why)| {
                    tracing::warn!(
                        drive = volume.device,
                        ?refusal,
                        why,
                        "the drive was not unmounted"
                    );
                    refusal
                }),
            (Some(bus), Act::Remove) => safely_remove(bus, &listing, &volume).map(|()| None),
            (Some(_), Act::AtStartup(on)) => ask_root_for_startup(&volume, on).map(|()| None),
        };
        Outcome {
            number,
            title: volume.title(),
            act,
            result,
            open,
        }
    }
}

/// Wake the worker whenever UDisks says anything has changed.
///
/// On a thread of its own because the iterator blocks, and for the life of the
/// session: there is nothing to stop it for, and it costs one sleeping thread.
fn listen(bus: zbus::blocking::Connection, shared: Arc<Shared>) {
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(UDISKS)
        .and_then(|rule| rule.path_namespace(MANAGER))
        .map(|rule| rule.build());
    let messages = match rule
        .and_then(|rule| zbus::blocking::MessageIterator::for_match_rule(rule, &bus, Some(256)))
    {
        Ok(messages) => messages,
        Err(err) => {
            tracing::warn!(
                ?err,
                "UDisks cannot be listened to; drives are read on a press"
            );
            return;
        }
    };
    for message in messages {
        if message.is_err() {
            continue;
        }
        let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.done {
            return;
        }
        state.dirty = true;
        shared.signal.notify_one();
    }
}

/// What a drive is, for telling one that has just appeared from one that was
/// here: a device number is given out again as soon as the last one with it
/// is unplugged, so the filesystem's own identity goes with it.
fn identity(volume: &Volume) -> (u64, Option<String>) {
    (volume.number, volume.uuid.clone())
}

/// The drives to mount without being asked: the ones UDisks says to mount by
/// themselves, that have appeared since `seen` was taken — every one of them
/// on the first reading, which is the session starting with a stick already
/// in — and that nothing has mounted yet.
///
/// Not one the machine's own mount table has an entry for, which is the
/// machine's to mount, and not a file somebody attached as a disk.
fn to_mount_by_itself(
    seen: Option<&HashSet<(u64, Option<String>)>>,
    volumes: &[Volume],
) -> Vec<String> {
    volumes
        .iter()
        .filter(|volume| volume.automount && !volume.attached)
        .filter(|volume| volume.mounted_at.is_empty() && volume.at_startup.is_none())
        .filter(|volume| !seen.is_some_and(|seen| seen.contains(&identity(volume))))
        .map(|volume| volume.object.clone())
        .collect()
}

// --- UDisks --------------------------------------------------------------------

type Properties = HashMap<String, OwnedValue>;
type Objects = HashMap<OwnedObjectPath, HashMap<String, Properties>>;

fn managed_objects(bus: &zbus::blocking::Connection) -> Option<Objects> {
    match bus.call_method(
        Some(UDISKS),
        MANAGER,
        Some(OBJECT_MANAGER),
        "GetManagedObjects",
        &(),
    ) {
        Ok(reply) => reply.body().deserialize().ok(),
        Err(err) => {
            tracing::debug!(?err, "UDisks did not answer, so no drive can be mounted");
            None
        }
    }
}

/// Every filesystem in UDisks' objects that somebody could mount.
///
/// Left out: what UDisks itself says to ignore (a firmware partition, Windows'
/// recovery, a swap area), whatever is not a filesystem — a partition table,
/// an encrypted container not yet opened, a RAID member — and a file attached
/// as a disk by anybody but `uid`.
fn volumes(objects: &Objects, uid: u32) -> Vec<Volume> {
    let mut found = Vec::new();
    for (path, interfaces) in objects {
        let (Some(block), Some(filesystem)) = (interfaces.get(BLOCK), interfaces.get(FILESYSTEM))
        else {
            continue;
        };
        if flag(block, "HintIgnore") || text(block, "IdUsage").as_deref() != Some("filesystem") {
            continue;
        }
        let attached = interfaces.contains_key(LOOP);
        if attached && number(interfaces.get(LOOP), "SetupByUID") != Some(u64::from(uid)) {
            continue;
        }
        let Some(device) = bytes(block, "Device").and_then(|device| {
            Path::new(&device)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        }) else {
            continue;
        };
        let drive = object_path(block, "Drive");
        let drive_facts = drive
            .as_deref()
            .and_then(|drive| objects.iter().find(|(path, _)| path.as_str() == drive))
            .and_then(|(_, interfaces)| interfaces.get(DRIVE));
        let startup = startup_entries(block);
        let options = startup
            .first()
            .map(|entry| entry.opts_text())
            .unwrap_or_default();
        if options.split(',').any(|option| option == "x-gvfs-hide") {
            continue;
        }
        let at_startup = startup.first().map(|entry| entry.dir());
        let removable = drive_facts
            .is_some_and(|drive| flag(drive, "Removable") || flag(drive, "MediaRemovable"));
        found.push(Volume {
            number: number(Some(block), "DeviceNumber").unwrap_or_default(),
            object: path.to_string(),
            device,
            uuid: text(block, "IdUUID").filter(|uuid| !uuid.is_empty()),
            label: text(block, "IdLabel").filter(|label| !label.trim().is_empty()),
            part_name: interfaces
                .get(PARTITION)
                .and_then(|partition| text(partition, "Name"))
                .filter(|name| !name.trim().is_empty()),
            size: number(Some(block), "Size").unwrap_or_default(),
            kind: text(block, "IdType").filter(|kind| !kind.is_empty()),
            mounted_at: byte_strings(filesystem, "MountPoints")
                .into_iter()
                .map(PathBuf::from)
                .collect(),
            systems: at_startup
                .as_deref()
                .is_some_and(|at| !crate::files::is_somewhere_a_drive_goes(at)),
            at_startup,
            removable,
            can_power_off: drive_facts.is_some_and(|drive| flag(drive, "CanPowerOff")),
            automount: flag(block, "HintAuto"),
            drive,
            attached,
        });
    }
    // Wherever else a drive is listed it is by name, and a list that came
    // back from a hash map in a new order on every reading would be a list
    // whose rows swap under a cursor for no reason at all.
    found.sort_by(|a, b| a.device.cmp(&b.device));
    found
}

/// One line of the machine's mount table, as UDisks hands it over.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    fsname: Vec<u8>,
    dir: Vec<u8>,
    kind: Vec<u8>,
    opts: Vec<u8>,
    freq: i32,
    passno: i32,
}

impl Entry {
    fn dir(&self) -> PathBuf {
        PathBuf::from(String::from_utf8_lossy(trimmed(&self.dir)).into_owned())
    }

    fn opts_text(&self) -> String {
        String::from_utf8_lossy(trimmed(&self.opts)).into_owned()
    }

    /// The item as UDisks wants it back: it finds the line to remove by every
    /// one of these being equal.
    fn item(&self) -> (String, HashMap<String, Value<'_>>) {
        let mut fields = HashMap::new();
        fields.insert("fsname".to_string(), Value::from(self.fsname.clone()));
        fields.insert("dir".to_string(), Value::from(self.dir.clone()));
        fields.insert("type".to_string(), Value::from(self.kind.clone()));
        fields.insert("opts".to_string(), Value::from(self.opts.clone()));
        fields.insert("freq".to_string(), Value::from(self.freq));
        fields.insert("passno".to_string(), Value::from(self.passno));
        ("fstab".to_string(), fields)
    }
}

/// The mount table's entries for a block device.
fn startup_entries(block: &Properties) -> Vec<Entry> {
    let Some(value) = block.get("Configuration").and_then(|v| v.try_clone().ok()) else {
        return Vec::new();
    };
    let Ok(items) = Vec::<(String, HashMap<String, OwnedValue>)>::try_from(value) else {
        return Vec::new();
    };
    items
        .into_iter()
        .filter(|(kind, _)| kind == "fstab")
        .map(|(_, fields)| Entry {
            fsname: bytes_raw(&fields, "fsname"),
            dir: bytes_raw(&fields, "dir"),
            kind: bytes_raw(&fields, "type"),
            opts: bytes_raw(&fields, "opts"),
            freq: fields
                .get("freq")
                .and_then(|v| i32::try_from(v).ok())
                .unwrap_or_default(),
            passno: fields
                .get("passno")
                .and_then(|v| i32::try_from(v).ok())
                .unwrap_or_default(),
        })
        .collect()
}

/// Mount it. `interactive` is whether polkit may ask for a password: a press
/// may, and a stick mounted as it is plugged in may not.
fn mount(
    bus: &zbus::blocking::Connection,
    object: &str,
    interactive: bool,
) -> Result<PathBuf, (Refusal, String)> {
    let mut options: HashMap<&str, Value> = HashMap::new();
    if !interactive {
        options.insert("auth.no_user_interaction", Value::from(true));
    }
    match call::<_, String>(bus, object, FILESYSTEM, "Mount", &(options,), interactive) {
        Ok(at) => Ok(PathBuf::from(at)),
        // Somebody else got there first, which is what was asked for.
        Err(err) if error_name(&err) == Some("AlreadyMounted") => {
            let at = managed_objects(bus)
                .and_then(|objects| {
                    volumes(&objects, unsafe { libc::getuid() })
                        .into_iter()
                        .find(|volume| volume.object == object)
                })
                .and_then(|volume| volume.place().map(Path::to_path_buf));
            at.ok_or_else(|| (Refusal::Failed, format!("{err}")))
        }
        Err(err) => Err((refusal(&err), format!("{err}"))),
    }
}

fn unmount(bus: &zbus::blocking::Connection, object: &str) -> Result<(), (Refusal, String)> {
    let options: HashMap<&str, Value> = HashMap::new();
    match call::<_, ()>(bus, object, FILESYSTEM, "Unmount", &(options,), true) {
        Ok(()) => Ok(()),
        Err(err) if error_name(&err) == Some("NotMounted") => Ok(()),
        Err(err) => Err((refusal(&err), format!("{err}"))),
    }
}

/// Unmount everything on the drive `volume` is on, then turn the drive off.
///
/// Every filesystem on it, not only the one pressed: a stick with two
/// partitions is one thing to pull out, and turning it off with the other
/// still mounted would be the very loss this row exists to prevent.
fn safely_remove(
    bus: &zbus::blocking::Connection,
    listing: &Listing,
    volume: &Volume,
) -> Result<(), Refusal> {
    let on_the_drive: Vec<&Volume> = match &volume.drive {
        Some(drive) => listing
            .volumes
            .iter()
            .filter(|other| other.drive.as_deref() == Some(drive.as_str()))
            .collect(),
        None => vec![volume],
    };
    for other in on_the_drive
        .iter()
        .filter(|other| !other.mounted_at.is_empty())
    {
        unmount(bus, &other.object).map_err(|(refusal, why)| {
            tracing::warn!(
                drive = other.device,
                ?refusal,
                why,
                "the drive was not unmounted"
            );
            refusal
        })?;
    }
    tracing::info!(drive = volume.device, "unmounted for removal");
    let Some(drive) = volume.drive.as_deref().filter(|_| volume.can_power_off) else {
        return Ok(());
    };
    let options: HashMap<&str, Value> = HashMap::new();
    call::<_, ()>(bus, drive, DRIVE, "PowerOff", &(options,), true)
        .map_err(|err| {
            // Unmounted is what makes it safe; a drive that would not turn off is
            // one whose light stays on, and it is still safe to pull out.
            tracing::info!(drive, ?err, "the drive was unmounted but not turned off");
        })
        .ok();
    Ok(())
}

/// One call to UDisks. `interactive` sets the D-Bus flag that says a password
/// may be asked for — see [`crate::users`], where the lack of it cost a
/// morning.
///
/// Leaving the flag off does **not** keep UDisks from asking. Unlike
/// accounts-daemon, UDisks reads its own `auth.no_user_interaction` option out
/// of the call's options instead, and without it polkit puts its question to
/// whatever agent the session has — a probe that expected to be refused put a
/// password prompt on somebody's desktop that way. A call that must not ask
/// carries the option, as [`Worker::mount_by_itself`]'s does.
fn call<Body, Reply>(
    bus: &zbus::blocking::Connection,
    path: &str,
    interface: &str,
    method: &str,
    body: &Body,
    interactive: bool,
) -> Result<Reply, zbus::Error>
where
    Body: serde::ser::Serialize + zbus::zvariant::DynamicType,
    Reply: for<'d> zbus::zvariant::DynamicDeserialize<'d>,
{
    let proxy = zbus::blocking::Proxy::new(bus, UDISKS, path, interface)?;
    let flags = match interactive {
        true => zbus::proxy::MethodFlags::AllowInteractiveAuth.into(),
        false => Default::default(),
    };
    proxy
        .call_with_flags::<_, _, Reply>(method, flags, body)?
        .ok_or_else(|| zbus::Error::Failure("UDisks answered nothing".into()))
}

fn error_name(err: &zbus::Error) -> Option<&str> {
    match err {
        zbus::Error::MethodError(name, ..) => name.as_str().rsplit('.').next(),
        _ => None,
    }
}

fn refusal(err: &zbus::Error) -> Refusal {
    refusal_named(error_name(err))
}

fn refusal_named(name: Option<&str>) -> Refusal {
    match name {
        Some("NotAuthorizedDismissed") => Refusal::Dismissed,
        Some("NotAuthorized" | "NotAuthorizedCanObtain") => Refusal::NotAllowed,
        Some("DeviceBusy") => Refusal::Busy,
        _ => Refusal::Failed,
    }
}

fn flag(properties: &Properties, name: &str) -> bool {
    properties
        .get(name)
        .and_then(|value| bool::try_from(value).ok())
        .unwrap_or(false)
}

fn text(properties: &Properties, name: &str) -> Option<String> {
    String::try_from(properties.get(name)?.try_clone().ok()?).ok()
}

fn number(properties: Option<&Properties>, name: &str) -> Option<u64> {
    let value = properties?.get(name)?;
    u64::try_from(value)
        .or_else(|_| u32::try_from(value).map(u64::from))
        .ok()
}

fn object_path(properties: &Properties, name: &str) -> Option<String> {
    let path = OwnedObjectPath::try_from(properties.get(name)?.try_clone().ok()?).ok()?;
    // UDisks says "no drive" as the root object rather than leaving it out.
    (path.as_str() != "/").then(|| path.to_string())
}

/// A byte string UDisks sends as `ay`, NUL and all, as text.
fn bytes(properties: &Properties, name: &str) -> Option<String> {
    let raw = bytes_raw(properties, name);
    let raw = trimmed(&raw);
    (!raw.is_empty()).then(|| String::from_utf8_lossy(raw).into_owned())
}

fn bytes_raw(properties: &HashMap<String, OwnedValue>, name: &str) -> Vec<u8> {
    properties
        .get(name)
        .and_then(|value| value.try_clone().ok())
        .and_then(|value| Vec::<u8>::try_from(value).ok())
        .unwrap_or_default()
}

fn byte_strings(properties: &Properties, name: &str) -> Vec<String> {
    properties
        .get(name)
        .and_then(|value| value.try_clone().ok())
        .and_then(|value| Vec::<Vec<u8>>::try_from(value).ok())
        .unwrap_or_default()
        .iter()
        .map(|raw| String::from_utf8_lossy(trimmed(raw)).into_owned())
        .filter(|text| !text.is_empty())
        .collect()
}

/// Everything before the NUL UDisks ends its byte strings with.
fn trimmed(raw: &[u8]) -> &[u8] {
    match raw.iter().position(|byte| *byte == 0) {
        Some(end) => &raw[..end],
        None => raw,
    }
}

// --- mount at startup ------------------------------------------------------------

/// Ask polkit to run this program as root to turn Mount at startup on or off.
///
/// Through `pkexec` rather than straight to UDisks, and the reason is the
/// number of passwords. Writing the mount table is one polkit action and
/// mounting an internal disk is another, and neither remembers the other's
/// answer — so going straight to UDisks asks twice for one press. A root
/// process is never asked at all, so [`at_startup_as_root`] does both halves
/// behind the one question polkit asks for this action.
fn ask_root_for_startup(volume: &Volume, on: bool) -> Result<(), Refusal> {
    let Some(uuid) = volume.uuid.as_deref() else {
        return Err(Refusal::Failed);
    };
    let Some(pkexec) = crate::locale::pkexec() else {
        tracing::warn!("no pkexec on this machine, so Mount at startup cannot be written");
        return Err(Refusal::Failed);
    };
    let exe = std::env::current_exe().map_err(|err| {
        tracing::warn!(?err, "this program cannot find itself to run as root");
        Refusal::Failed
    })?;
    let answered = std::process::Command::new(pkexec)
        .arg("--disable-internal-agent")
        .arg(&exe)
        .arg(STARTUP_FLAG)
        .arg(uuid)
        .arg(if on { "on" } else { "off" })
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|err| {
            tracing::warn!(?err, "pkexec could not be started");
            Refusal::Failed
        })?;
    let said = String::from_utf8_lossy(&answered.stderr).trim().to_string();
    match answered.status.code() {
        Some(0) => {
            tracing::info!(drive = volume.device, on, "mount at startup");
            Ok(())
        }
        Some(LATER_STATUS) => {
            tracing::info!(
                drive = volume.device,
                "mount at startup is written and waits for a restart"
            );
            Err(Refusal::Later)
        }
        // pkexec's own two: a panel somebody dismissed, and every other way
        // nobody was authorized.
        Some(126) => Err(Refusal::Dismissed),
        Some(127) => {
            tracing::warn!(said, "pkexec did not authorize mount at startup");
            Err(Refusal::NotAllowed)
        }
        _ => {
            tracing::warn!(said, "mount at startup was not written");
            Err(Refusal::Failed)
        }
    }
}

/// The flag the installed polkit action is bound to. See
/// `packaging/files/org.linexinbar.drives.policy.in`.
pub const STARTUP_FLAG: &str = "--mount-at-startup";

/// The privileged half of Mount at startup: one line of the machine's mount
/// table, added or removed through UDisks, and the drive moved to where that
/// line puts it.
///
/// Started by polkit, from [`ask_root_for_startup`], and by nothing else. What
/// bounds it is what it can do rather than who it thinks called it. It takes a
/// filesystem's UUID — checked to be shaped like one, and to name a filesystem
/// UDisks lists as one somebody could mount — and can do exactly two things
/// with it: add the line that mounts it at `/mnt/<its name>` with fixed,
/// conservative options, or remove the lines that already mount it. It cannot
/// choose where a drive goes, give it other options, or touch any other line.
pub fn at_startup_as_root(uuid: &str, on: &str) -> anyhow::Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        anyhow::bail!("this is polkit's half of mount at startup and only runs as root");
    }
    anyhow::ensure!(is_a_uuid(uuid), "{uuid:?} is not a filesystem's UUID");
    let on = match on {
        "on" => true,
        "off" => false,
        other => anyhow::bail!("{other:?} is neither on nor off"),
    };
    let bus = zbus::blocking::Connection::system()?;
    let objects = managed_objects(&bus).ok_or_else(|| anyhow::anyhow!("UDisks did not answer"))?;
    // Loop devices are left out by giving no account's number: a file somebody
    // attached is never written into the machine's table.
    let volume = volumes(&objects, u32::MAX)
        .into_iter()
        .find(|volume| volume.uuid.as_deref() == Some(uuid))
        .ok_or_else(|| anyhow::anyhow!("no filesystem that could be mounted has UUID {uuid}"))?;
    anyhow::ensure!(
        !volume.systems,
        "{} is mounted where the system keeps itself",
        volume.device
    );
    let block = objects
        .iter()
        .find(|(path, _)| path.as_str() == volume.object)
        .and_then(|(_, interfaces)| interfaces.get(BLOCK))
        .ok_or_else(|| anyhow::anyhow!("UDisks lost {}", volume.device))?;
    let entries = startup_entries(block);
    let options: HashMap<&str, Value> = HashMap::new();

    if !on {
        for entry in &entries {
            call::<_, ()>(
                &bus,
                &volume.object,
                BLOCK,
                "RemoveConfigurationItem",
                &(entry.item(), &options),
                false,
            )?;
            eprintln!("removed {} from the mount table", entry.dir().display());
        }
        return Ok(());
    }

    let dir = match entries.first() {
        Some(entry) => entry.dir(),
        None => {
            let taken = taken_places();
            let dir = startup_place(volume.label.as_deref(), uuid, &taken);
            let entry = Entry {
                fsname: nul_terminated(&format!("/dev/disk/by-uuid/{uuid}")),
                dir: nul_terminated(&dir.to_string_lossy()),
                kind: nul_terminated("auto"),
                opts: nul_terminated(&startup_options(volume.kind.as_deref(), owner())),
                freq: 0,
                passno: 0,
            };
            call::<_, ()>(
                &bus,
                &volume.object,
                BLOCK,
                "AddConfigurationItem",
                &(entry.item(), &options),
                false,
            )?;
            dir
        }
    };

    // And now, rather than on the next start: a drive the person turned this
    // on for is a drive they expect to find where it says, today.
    if volume.mounted_at.contains(&dir) {
        return Ok(());
    }
    if !volume.mounted_at.is_empty() {
        if let Err(err) = call::<_, ()>(
            &bus,
            &volume.object,
            FILESYSTEM,
            "Unmount",
            &(&options,),
            false,
        ) {
            eprintln!(
                "{} stays where it is until the next start: {err}",
                volume.device
            );
            std::process::exit(LATER_STATUS);
        }
    }
    call::<_, String>(
        &bus,
        &volume.object,
        FILESYSTEM,
        "Mount",
        &(&options,),
        false,
    )?;
    Ok(())
}

/// Whether a string is shaped like a filesystem's UUID: ext4's
/// `67eb84eb-7f8d-…`, FAT's `6F27-C22C`, NTFS's sixteen hex digits. Nothing in
/// that has a space, a slash or a comma, which is what keeps it from being
/// anything but a name in a mount table line.
fn is_a_uuid(uuid: &str) -> bool {
    (4..=64).contains(&uuid.len()) && uuid.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

/// Where a drive mounted at startup goes: `/mnt/<its name>`, or its UUID for a
/// drive nobody named, made unique against every place already in use.
///
/// A folder of that name that is already there is used, full or not, which is
/// what systemd does with one — it is the name the person will look for, and
/// whatever is in it is hidden while the drive is mounted rather than touched.
/// What is not used is a place something else is mounted at, or another line
/// of the table already names.
fn startup_place(label: Option<&str>, uuid: &str, taken: &HashSet<PathBuf>) -> PathBuf {
    let name = label
        .map(|label| {
            label
                .chars()
                .map(
                    |c| match c.is_alphanumeric() || matches!(c, '-' | '_' | '.') {
                        true => c,
                        false => '_',
                    },
                )
                .collect::<String>()
        })
        .filter(|name| !name.trim_matches(|c| c == '.' || c == '_').is_empty())
        .unwrap_or_else(|| uuid.to_string());
    let root = Path::new(STARTUP_ROOT);
    let first = root.join(&name);
    if !taken.contains(&first) {
        return first;
    }
    (2..)
        .map(|count| root.join(format!("{name}_{count}")))
        .find(|candidate| !taken.contains(candidate))
        .expect("there is always another number")
}

/// Every place the machine already uses: what is mounted now, and every place
/// the mount table names.
fn taken_places() -> HashSet<PathBuf> {
    let mut taken = HashSet::new();
    for line in std::fs::read_to_string("/proc/self/mountinfo")
        .unwrap_or_default()
        .lines()
    {
        if let Some(at) = line.split_whitespace().nth(4) {
            taken.insert(PathBuf::from(crate::files::unescaped(at)));
        }
    }
    for line in std::fs::read_to_string("/etc/fstab")
        .unwrap_or_default()
        .lines()
    {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some(at) = line.split_whitespace().nth(1) {
            taken.insert(PathBuf::from(crate::files::unescaped(at)));
        }
    }
    taken
}

/// The options a drive mounted at startup is given, and for a filesystem with
/// no owners of its own, whose it is.
fn startup_options(kind: Option<&str>, owner: Option<(u32, u32)>) -> String {
    match (kind, owner) {
        (Some(kind), Some((uid, gid))) if NO_OWNERS.contains(&kind) => {
            format!("{STARTUP_OPTIONS},uid={uid},gid={gid}")
        }
        _ => STARTUP_OPTIONS.to_string(),
    }
}

/// Who asked, as pkexec records it: `PKEXEC_UID` is set by pkexec itself, not
/// by the caller, so it is the one thing in this process's environment that
/// can be believed.
fn owner() -> Option<(u32, u32)> {
    let uid: u32 = std::env::var("PKEXEC_UID").ok()?.parse().ok()?;
    // SAFETY: getpwuid returns a pointer into static storage or null, and it
    // is read at once, on the only thread this process has.
    let entry = unsafe { libc::getpwuid(uid) };
    if entry.is_null() {
        return None;
    }
    Some((uid, unsafe { (*entry).pw_gid }))
}

fn nul_terminated(text: &str) -> Vec<u8> {
    let mut bytes = text.as_bytes().to_vec();
    bytes.push(0);
    bytes
}

/// A drive as UDisks would list it, for the tests of the pages that are
/// written from one: an internal disk nothing has mounted, called `label`.
#[cfg(test)]
pub fn a_drive(number: u64, device: &str, label: &str) -> Volume {
    Volume {
        number,
        object: format!("/org/freedesktop/UDisks2/block_devices/{device}"),
        device: device.to_string(),
        uuid: Some(format!("{number:08x}-7f8d-4027-9bd5-07951890dd96")),
        label: Some(label.to_string()),
        part_name: None,
        size: 1_000_204_123_136,
        kind: Some("ext4".to_string()),
        mounted_at: Vec::new(),
        at_startup: None,
        systems: false,
        removable: false,
        can_power_off: false,
        automount: false,
        drive: Some(format!("/org/freedesktop/UDisks2/drives/{label}")),
        attached: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A USB stick nothing has mounted yet.
    fn a_volume(number: u64, uuid: &str) -> Volume {
        Volume {
            uuid: Some(uuid.to_string()),
            size: 16 << 30,
            kind: Some("vfat".to_string()),
            removable: true,
            can_power_off: true,
            automount: true,
            ..a_drive(number, &format!("sd{number}"), "Stick")
        }
    }

    #[test]
    fn a_stick_already_in_is_mounted_when_the_session_starts() {
        let stick = a_volume(17, "6F27-C22C");
        assert_eq!(
            to_mount_by_itself(None, std::slice::from_ref(&stick)),
            vec![stick.object.clone()]
        );
    }

    #[test]
    fn only_a_drive_that_has_just_appeared_is_mounted_by_itself() {
        let stick = a_volume(17, "6F27-C22C");
        let seen: HashSet<_> = [identity(&stick)].into();
        assert!(
            to_mount_by_itself(Some(&seen), std::slice::from_ref(&stick)).is_empty(),
            "a stick somebody unmounted stays unmounted"
        );

        // Another stick given the same device number once the first was
        // pulled out is a new drive.
        let other = a_volume(17, "0A1B-2C3D");
        assert_eq!(
            to_mount_by_itself(Some(&seen), std::slice::from_ref(&other)),
            vec![other.object.clone()]
        );
    }

    #[test]
    fn what_is_mounted_by_itself_is_what_udisks_says_to_mount() {
        let mut internal = a_volume(1, "67eb84eb-7f8d-4027-9bd5-07951890dd96");
        internal.automount = false;
        let mut mounted = a_volume(2, "0000-0002");
        mounted.mounted_at = vec![PathBuf::from("/run/media/kate/Stick")];
        let mut tabled = a_volume(3, "0000-0003");
        tabled.at_startup = Some(PathBuf::from("/mnt/Stick"));
        let mut image = a_volume(4, "0000-0004");
        image.attached = true;
        assert!(to_mount_by_itself(None, &[internal, mounted, tabled, image]).is_empty());
    }

    #[test]
    fn a_drive_mounted_at_startup_goes_under_its_own_name() {
        let taken = HashSet::new();
        assert_eq!(
            startup_place(Some("GamesHDD"), "67eb", &taken),
            PathBuf::from("/mnt/GamesHDD")
        );
        // A name with a space or a slash in it is one a mount table cannot
        // carry as it is.
        assert_eq!(
            startup_place(Some("My Films/2"), "67eb", &taken),
            PathBuf::from("/mnt/My_Films_2")
        );
        assert_eq!(
            startup_place(Some("Zdjęcia"), "67eb", &taken),
            PathBuf::from("/mnt/Zdjęcia")
        );
        // Nobody named it, or named it nothing a folder can be called.
        assert_eq!(
            startup_place(None, "6F27-C22C", &taken),
            PathBuf::from("/mnt/6F27-C22C")
        );
        assert_eq!(
            startup_place(Some(".."), "6F27-C22C", &taken),
            PathBuf::from("/mnt/6F27-C22C")
        );
    }

    #[test]
    fn a_place_already_in_use_is_not_given_to_a_second_drive() {
        let taken: HashSet<PathBuf> =
            [PathBuf::from("/mnt/Games"), PathBuf::from("/mnt/Games_2")].into();
        assert_eq!(
            startup_place(Some("Games"), "67eb", &taken),
            PathBuf::from("/mnt/Games_3")
        );
    }

    #[test]
    fn a_drive_with_no_owners_of_its_own_is_given_to_whoever_asked() {
        assert_eq!(
            startup_options(Some("ext4"), Some((1000, 1000))),
            "nosuid,nodev,nofail,x-gvfs-show"
        );
        assert_eq!(
            startup_options(Some("exfat"), Some((1003, 1003))),
            "nosuid,nodev,nofail,x-gvfs-show,uid=1003,gid=1003"
        );
        assert_eq!(
            startup_options(Some("ntfs"), None),
            "nosuid,nodev,nofail,x-gvfs-show"
        );
    }

    #[test]
    fn only_a_uuid_reaches_the_mount_table() {
        assert!(is_a_uuid("67eb84eb-7f8d-4027-9bd5-07951890dd96"));
        assert!(is_a_uuid("6F27-C22C"));
        assert!(is_a_uuid("01D7A1B2C3D4E5F6"));
        assert!(!is_a_uuid(""));
        assert!(!is_a_uuid("6F27 C22C"));
        assert!(!is_a_uuid("../etc"));
        assert!(!is_a_uuid("6F27-C22C,uid=0"));
        assert!(!is_a_uuid("6F27-C22C\n/dev/sda1 / ext4"));
    }

    #[test]
    fn udisks_refusals_become_four_words() {
        assert_eq!(
            refusal_named(Some("NotAuthorizedDismissed")),
            Refusal::Dismissed
        );
        assert_eq!(
            refusal_named(Some("NotAuthorizedCanObtain")),
            Refusal::NotAllowed
        );
        assert_eq!(refusal_named(Some("NotAuthorized")), Refusal::NotAllowed);
        assert_eq!(refusal_named(Some("DeviceBusy")), Refusal::Busy);
        assert_eq!(refusal_named(Some("Failed")), Refusal::Failed);
        assert_eq!(refusal_named(None), Refusal::Failed);
    }

    #[test]
    fn a_stick_is_never_offered_to_the_mount_table() {
        let stick = a_volume(17, "6F27-C22C");
        assert!(!stick.can_start_with_the_machine());
        let mut internal = stick.clone();
        internal.removable = false;
        assert!(internal.can_start_with_the_machine());
        let mut image = internal.clone();
        image.attached = true;
        assert!(!image.can_start_with_the_machine());
        let mut nameless = internal.clone();
        nameless.uuid = None;
        assert!(!nameless.can_start_with_the_machine());
    }

    #[test]
    fn a_drive_is_named_as_the_storage_page_names_it() {
        let mut volume = a_volume(1, "6F27-C22C");
        assert_eq!(volume.title(), "Stick");
        volume.label = None;
        volume.part_name = Some("Basic data partition".to_string());
        assert_eq!(volume.title(), "Basic data partition");
    }

    #[test]
    fn the_mount_table_item_goes_back_as_it_came() {
        let entry = Entry {
            fsname: nul_terminated("/dev/disk/by-uuid/926e6656"),
            dir: nul_terminated("/mnt/GamesSSD"),
            kind: nul_terminated("auto"),
            opts: nul_terminated(STARTUP_OPTIONS),
            freq: 0,
            passno: 0,
        };
        assert_eq!(entry.dir(), PathBuf::from("/mnt/GamesSSD"));
        assert_eq!(entry.opts_text(), STARTUP_OPTIONS);
        let (kind, fields) = entry.item();
        assert_eq!(kind, "fstab");
        assert_eq!(fields.len(), 6);
        assert_eq!(
            Vec::<u8>::try_from(fields["dir"].try_clone().unwrap()).unwrap(),
            b"/mnt/GamesSSD\0"
        );
    }
}
