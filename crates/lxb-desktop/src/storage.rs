//! What is on this machine's drives, for Settings > Storage.
//!
//! Read out of the kernel rather than asked of UDisks, on the bargain
//! [`crate::power`] makes about the battery: the page is only ever *reading*,
//! and what UDisks would answer is what `/sys/class/block`, the mount table and
//! `statvfs` say anyway. A console that showed no drives because a daemon was
//! not running would be wrong about the hardware, so this reads the hardware.
//! The one thing taken from a daemon is the filesystem's own name, which udev
//! writes down under `/run/udev/data` where anybody can read it — and a machine
//! without it still gets every partition, named by where it is used.
//!
//! ## What a row is
//!
//! One partition somebody could keep things on, which is not quite one entry of
//! `/sys/class/block`:
//!
//! * A mounted filesystem is one row however many times it is mounted — a
//!   btrfs root is the same disk at `/`, `/home` and `/var/log` — and it is the
//!   row of whatever it is mounted *from*. On an encrypted disk that is the
//!   opened container rather than the partition under it, which is where the
//!   files are and where `statvfs` can be asked.
//! * A partition something else is built on (an opened container, a RAID
//!   member, a volume group) is not a row of its own, because it is already
//!   standing behind the row of whatever is built on it.
//! * Active swap is a row: it is a partition, and a person looking at their
//!   disk and finding sixteen gigabytes unaccounted for deserves to be told
//!   what it is doing.
//! * A partition nothing is using is a row, with no bar: how full a filesystem
//!   is can only be asked of one that is mounted.
//!
//! What is left out is what is not a drive at all — loop devices (every snap
//! is one), RAM disks, compressed swap in memory, optical discs — and the
//! partitions that are firmware plumbing rather than room: a few megabytes
//! with no filesystem on them, which is what a BIOS boot partition, Windows'
//! reserved partition and an extended partition's container all are.
//!
//! ## On a worker, and only while somebody is looking
//!
//! `statvfs` on a drive that has spun down, or on a USB stick that is going
//! away, can block — so the shell reads the last answer and a worker keeps it
//! true, reading once when Settings is arrived at (so the page has rows the
//! moment it is opened) and every few seconds while the page itself is open.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// How often the drives are read again while the Storage page is open.
///
/// Often enough that a game coming down in the background is seen eating the
/// room while somebody watches, and a stick plugged in appears before they
/// have wondered whether it will; rarely enough that it is nothing.
const REFRESH: Duration = Duration::from_secs(3);

/// The kernel names that are block devices and not drives: loop devices,
/// RAM disks, compressed swap in memory, optical discs and floppies.
const NOT_DRIVES: &[&str] = &["loop", "ram", "zram", "sr", "fd"];

/// The largest partition with no filesystem on it that is still left off the
/// page.
///
/// Windows' reserved partition is sixteen or a hundred and twenty-eight
/// megabytes, a BIOS boot partition one, an extended partition's container one
/// kilobyte — and none of them is anywhere a person keeps anything.
const PLUMBING: u64 = 128 * 1024 * 1024;

/// Where a mounted filesystem is used, which is what names it on the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    /// Mounted at `/`: the one the system runs from.
    System,
    /// Mounted at `/home`: where everybody's own files are.
    Home,
    /// Anywhere else.
    Other,
    /// Mounted at `/boot` or `/efi`: what the machine starts from.
    Startup,
}

/// How big a filesystem is and how much of it is left, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Room {
    pub whole: u64,
    /// What is left for the person reading it rather than for root: the
    /// difference is the reserve some filesystems keep back, and nobody on this
    /// page can write into that.
    pub free: u64,
}

impl Room {
    /// How much of it is taken, as a share of one.
    ///
    /// Taken as everything that is not free, the reserve included, so the bar
    /// and the "free of" beside it describe the same two numbers.
    pub fn used(self) -> f32 {
        if self.whole == 0 {
            return 0.0;
        }
        (1.0 - self.free as f64 / self.whole as f64).clamp(0.0, 1.0) as f32
    }
}

/// What a partition is being used for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Standing {
    /// Mounted. `room` is `None` when `statvfs` did not answer.
    Mounted {
        at: PathBuf,
        role: Role,
        room: Option<Room>,
    },
    /// Swap, in use.
    Memory,
    /// Nothing is using it.
    Unused,
}

/// One partition, as the page shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    /// The kernel's name for it: `nvme0n1p3`, `sda1`, `dm-0`.
    pub device: String,
    /// The name the filesystem was given, when it was given one.
    pub label: Option<String>,
    /// The name the partition table gives it, which is what a Windows
    /// partition with no label of its own is still called.
    pub part_name: Option<String>,
    /// How big the partition is, whatever is on it.
    pub size: u64,
    /// The filesystem on it: `ext4`, `ntfs`.
    pub kind: Option<String>,
    /// The drive it is on, as its maker names it.
    pub model: Option<String>,
    pub standing: Standing,
}

impl Part {
    /// Where it goes in the list: the system first, then everybody's files,
    /// the drives somebody mounted, what the machine starts from, swap, and
    /// what nothing is using.
    fn rank(&self) -> u8 {
        match &self.standing {
            Standing::Mounted { role, .. } => *role as u8,
            Standing::Memory => 4,
            Standing::Unused => 5,
        }
    }
}

/// The drives, and the worker that keeps them true.
pub struct Storage {
    shared: Arc<Shared>,
}

struct Shared {
    state: Mutex<State>,
    signal: Condvar,
}

#[derive(Default)]
struct State {
    listing: Option<Vec<Part>>,
    /// Bumped by every reading, so the shell can tell a new one from the one
    /// it already has without comparing them.
    read: u64,
    taken: u64,
    /// Whether Settings is on screen anywhere.
    settings: bool,
    /// Whether the Storage page is open anywhere.
    page: bool,
    dirty: bool,
    done: bool,
}

impl Storage {
    /// Start the worker. It reads once straight away, so the page is filled
    /// long before anybody can walk to it, and then sleeps until asked.
    pub fn start() -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            signal: Condvar::new(),
        });
        let worker = Arc::clone(&shared);
        if let Err(err) = std::thread::Builder::new()
            .name("lxb-storage".to_string())
            .spawn(move || Worker { shared: worker }.run())
        {
            tracing::warn!(?err, "no worker thread; Settings > Storage stays empty");
        }
        Self { shared }
    }

    /// Say where the cursor is: in Settings, and on the Storage page itself.
    ///
    /// Arriving at either is a reason to read again at once — a drive plugged
    /// in while the shell was behind a game is on the page by the time the
    /// page is opened.
    pub fn watch(&self, settings: bool, page: bool) {
        let mut state = self.held();
        let arrived = (settings && !state.settings) || (page && !state.page);
        state.settings = settings;
        state.page = page;
        if arrived {
            state.dirty = true;
            self.shared.signal.notify_one();
        }
    }

    /// The reading the worker has made since the last time this was asked,
    /// if it has made one.
    pub fn take(&self) -> Option<Vec<Part>> {
        let mut state = self.held();
        if state.read == state.taken {
            return None;
        }
        state.taken = state.read;
        state.listing.clone()
    }

    fn held(&self) -> std::sync::MutexGuard<'_, State> {
        self.shared.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Drop for Storage {
    fn drop(&mut self) {
        self.held().done = true;
        self.shared.signal.notify_all();
    }
}

struct Worker {
    shared: Arc<Shared>,
}

impl Worker {
    fn run(self) {
        loop {
            {
                let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
                if state.done {
                    return;
                }
                state.dirty = false;
            }
            let listing = read(Path::new("/"), &room);
            {
                let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
                state.listing = Some(listing);
                state.read += 1;
            }
            self.wait();
        }
    }

    /// Sleep until the cursor arrives somewhere worth reading for, or — while
    /// the page is open — until it is time to look again.
    fn wait(&self) {
        let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.done || state.dirty {
            return;
        }
        if state.page {
            let _held = self.shared.signal.wait_timeout(state, REFRESH);
        } else {
            let _held = self.shared.signal.wait(state);
        }
    }
}

/// The mounted partition a path is on: the one mounted at the longest part of
/// it, which is how a library at `/mnt/games/SteamLibrary` is found on the
/// drive mounted at `/mnt/games` rather than on the system drive under it.
///
/// By the one place each partition is shown at, so a filesystem mounted twice
/// is found through the place its row names — which is also what that row is
/// called, and the point of asking: the drive a Steam library is on is named
/// the way Settings > Storage names it.
pub fn holding<'a>(parts: &'a [Part], path: &Path) -> Option<&'a Part> {
    parts
        .iter()
        .filter_map(|part| match &part.standing {
            Standing::Mounted { at, .. } if path.starts_with(at) => {
                Some((at.components().count(), part))
            }
            _ => None,
        })
        .max_by_key(|(depth, _)| *depth)
        .map(|(_, part)| part)
}

/// How big the filesystem mounted at `at` is and how much of it is left.
pub fn room(at: &Path) -> Option<Room> {
    let path = std::ffi::CString::new(at.as_os_str().as_encoded_bytes()).ok()?;
    let mut facts: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `path` is a valid NUL-terminated string for the length of the
    // call, and `facts` is a live `statvfs` for the kernel to write into.
    if unsafe { libc::statvfs(path.as_ptr(), &mut facts) } != 0 {
        return None;
    }
    // `f_frsize` is what both counts are in; see [`crate::files::room`].
    let unit = facts.f_frsize as u64;
    let whole = facts.f_blocks as u64 * unit;
    (whole > 0).then(|| Room {
        whole,
        free: facts.f_bavail as u64 * unit,
    })
}

/// One entry of `/sys/class/block`, as far as this page cares.
#[derive(Debug, Default)]
struct Block {
    /// `major:minor`, which is how the mount table and udev name it.
    number: String,
    size: u64,
    partition: bool,
    /// Whether something is built on it.
    held: bool,
    /// What it is built on, for a device-mapper or RAID device.
    slaves: Vec<String>,
    part_name: Option<String>,
}

/// Every partition on the machine whose root is `root`, in the page's order.
///
/// `root` is `/` outside the tests, which build a tree of their own; `room` is
/// [`room`] outside them, because `statvfs` cannot be pointed at a fixture.
pub fn read(root: &Path, room: &dyn Fn(&Path) -> Option<Room>) -> Vec<Part> {
    let class = root.join("sys/class/block");
    let mut blocks: HashMap<String, Block> = HashMap::new();
    for entry in std::fs::read_dir(&class).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if NOT_DRIVES.iter().any(|prefix| name.starts_with(prefix)) {
            continue;
        }
        let dir = class.join(&name);
        let Some(number) = read_trimmed(&dir.join("dev")) else {
            continue;
        };
        blocks.insert(
            name,
            Block {
                number,
                // Always counted in 512-byte sectors, whatever the drive's own
                // sector size is.
                size: read_trimmed(&dir.join("size"))
                    .and_then(|sectors| sectors.parse::<u64>().ok())
                    .unwrap_or(0)
                    .saturating_mul(512),
                partition: dir.join("partition").exists(),
                held: listed(&dir.join("holders")).next().is_some(),
                slaves: listed(&dir.join("slaves")).collect(),
                part_name: std::fs::read_to_string(dir.join("uevent"))
                    .ok()
                    .and_then(|uevent| {
                        uevent
                            .lines()
                            .find_map(|line| line.strip_prefix("PARTNAME=").map(str::to_string))
                    })
                    .filter(|name| !name.is_empty()),
            },
        );
    }

    let by_number: HashMap<&str, &str> = blocks
        .iter()
        .map(|(name, block)| (block.number.as_str(), name.as_str()))
        .collect();
    let named = |number: &str, source: &str| -> Option<String> {
        by_number
            .get(number)
            .map(|name| name.to_string())
            .or_else(|| resolved(root, source))
            .filter(|name| blocks.contains_key(name))
    };

    // Every place each device is mounted, and what as. A btrfs mount carries
    // an anonymous device number rather than the partition's, which is why the
    // source is asked when the number names nothing.
    let mut mounts: HashMap<String, Vec<(PathBuf, String)>> = HashMap::new();
    let table = std::fs::read_to_string(root.join("proc/self/mountinfo")).unwrap_or_default();
    for mount in parse_mountinfo(&table) {
        if let Some(name) = named(&mount.number, &mount.source) {
            mounts.entry(name).or_default().push((mount.at, mount.kind));
        }
    }
    let swaps: HashSet<String> =
        parse_swaps(&std::fs::read_to_string(root.join("proc/swaps")).unwrap_or_default())
            .iter()
            .filter_map(|path| resolved(root, path))
            .collect();

    let mut parts = Vec::new();
    for (name, block) in &blocks {
        let udev = udev(root, &block.number);
        let label = udev
            .get("ID_FS_LABEL_ENC")
            .map(|label| unhexed(label))
            .or_else(|| udev.get("ID_FS_LABEL").cloned())
            .filter(|label| !label.trim().is_empty());
        let part_name = udev
            .get("ID_PART_ENTRY_NAME")
            .map(|name| unhexed(name))
            .filter(|name| !name.trim().is_empty())
            .or_else(|| block.part_name.clone());
        let mut kind = udev
            .get("ID_FS_TYPE")
            .cloned()
            .filter(|kind| !kind.is_empty());

        let standing = if let Some(places) = mounts.get(name) {
            let (at, mounted_as) = chosen_mount(places);
            kind = Some(mounted_as.clone());
            let role = role_of(places);
            Standing::Mounted {
                room: room(at),
                at: at.clone(),
                role,
            }
        } else if swaps.contains(name) {
            Standing::Memory
        } else {
            // Something built on it is already a row of its own, or nothing
            // is, because it is a device-mapper or RAID device with nothing
            // mounted from it.
            let virtual_device = name.starts_with("dm-") || name.starts_with("md");
            if block.held || virtual_device || block.size == 0 {
                continue;
            }
            if !block.partition {
                // A whole disk counts only when it carries a filesystem of its
                // own rather than a partition table: a USB stick formatted
                // without one. A disk with partitions is its partitions.
                let has_partitions = blocks
                    .iter()
                    .any(|(other, part)| part.partition && class.join(name).join(other).exists());
                if has_partitions || kind.is_none() {
                    continue;
                }
            }
            if kind.is_none() && block.size <= PLUMBING {
                continue;
            }
            Standing::Unused
        };

        parts.push(Part {
            device: name.clone(),
            label,
            part_name,
            size: block.size,
            kind,
            model: model(&class, &blocks, name),
            standing,
        });
    }
    parts.sort_by(|a, b| {
        a.rank()
            .cmp(&b.rank())
            .then_with(|| natural(&a.device, &b.device))
    });
    parts
}

/// The one place a filesystem mounted several times is shown at: `/` if it is
/// there, then `/home`, then whichever is nearest the top.
fn chosen_mount(places: &[(PathBuf, String)]) -> &(PathBuf, String) {
    places
        .iter()
        .min_by_key(|(at, _)| {
            let first = match at.to_str() {
                Some("/") => 0,
                Some("/home") => 1,
                _ => 2,
            };
            (first, at.components().count(), at.as_os_str().len())
        })
        .expect("a mounted device has somewhere it is mounted")
}

/// What a filesystem mounted at these places is for.
fn role_of(places: &[(PathBuf, String)]) -> Role {
    let at = |path: &str| places.iter().any(|(place, _)| place == Path::new(path));
    if at("/") {
        Role::System
    } else if at("/home") {
        Role::Home
    } else if at("/boot") || at("/efi") || at("/boot/efi") {
        Role::Startup
    } else {
        Role::Other
    }
}

/// The drive a device is on, as its maker names it: the disk a partition is
/// a part of, or — for an opened container — the disk under that.
fn model(class: &Path, blocks: &HashMap<String, Block>, name: &str) -> Option<String> {
    let mut disk = name.to_string();
    for _ in 0..8 {
        let Some(block) = blocks.get(&disk) else {
            break;
        };
        if block.partition {
            // A disk's own directory holds one for each of its partitions.
            match blocks
                .iter()
                .find(|(other, part)| !part.partition && class.join(other).join(&disk).exists())
            {
                Some((parent, _)) => disk = parent.clone(),
                None => break,
            }
        } else if let Some(under) = block.slaves.first() {
            disk = under.clone();
        } else {
            break;
        }
    }
    read_trimmed(&class.join(&disk).join("device/model")).filter(|model| !model.is_empty())
}

/// One mounted filesystem, as `/proc/self/mountinfo` gives it.
#[derive(Debug, PartialEq, Eq)]
struct Mounted {
    number: String,
    at: PathBuf,
    kind: String,
    source: String,
}

/// Parse the mount table. The optional fields between the options and the
/// `-` are skipped whatever their number, which is what the format asks.
fn parse_mountinfo(raw: &str) -> Vec<Mounted> {
    raw.lines()
        .filter_map(|line| {
            let mut fields = line.split(' ');
            let number = fields.nth(2)?.to_string();
            let at = crate::files::unescaped(fields.nth(1)?);
            let mut rest = fields.skip_while(|field| *field != "-");
            rest.next()?;
            let kind = rest.next()?.to_string();
            let source = crate::files::unescaped(rest.next()?);
            Some(Mounted {
                number,
                at: PathBuf::from(at),
                kind,
                source,
            })
        })
        .collect()
}

/// The swap devices in use. A swap *file* is on a filesystem that is already
/// a row, so only partitions are kept.
fn parse_swaps(raw: &str) -> Vec<String> {
    raw.lines()
        .skip(1)
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let path = crate::files::unescaped(fields.next()?);
            (fields.next()? == "partition").then_some(path)
        })
        .collect()
}

/// The kernel's name for a device node: `/dev/mapper/cryptroot` is `dm-0`.
fn resolved(root: &Path, source: &str) -> Option<String> {
    let relative = source.strip_prefix("/dev/")?;
    let path = root.join("dev").join(relative);
    let path = std::fs::canonicalize(&path).unwrap_or(path);
    Some(path.file_name()?.to_string_lossy().into_owned())
}

/// What udev wrote down about a device: its `E:` lines.
fn udev(root: &Path, number: &str) -> HashMap<String, String> {
    std::fs::read_to_string(root.join(format!("run/udev/data/b{number}")))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.strip_prefix("E:")?.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

/// udev writes a label's awkward bytes as `\x20`; this puts them back.
fn unhexed(value: &str) -> String {
    let mut bytes = Vec::with_capacity(value.len());
    let raw = value.as_bytes();
    let mut at = 0;
    while at < raw.len() {
        if raw[at] == b'\\' && raw.get(at + 1) == Some(&b'x') {
            if let Some(byte) = value
                .get(at + 2..at + 4)
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
            {
                bytes.push(byte);
                at += 4;
                continue;
            }
        }
        bytes.push(raw[at]);
        at += 1;
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn read_trimmed(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_string())
}

/// The names in a directory, or none if it is not there.
fn listed(dir: &Path) -> impl Iterator<Item = String> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
}

/// `sda2` before `sda10`: runs of digits compare as numbers.
fn natural(a: &str, b: &str) -> std::cmp::Ordering {
    fn pieces(text: &str) -> Vec<Result<u64, String>> {
        let mut out = Vec::new();
        let mut rest = text;
        while let Some(first) = rest.chars().next() {
            let digit = first.is_ascii_digit();
            let end = rest
                .find(|c: char| c.is_ascii_digit() != digit)
                .unwrap_or(rest.len());
            let (piece, tail) = rest.split_at(end);
            out.push(match digit {
                true => Ok(piece.parse().unwrap_or(u64::MAX)),
                false => Err(piece.to_string()),
            });
            rest = tail;
        }
        out
    }
    pieces(a).cmp(&pieces(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A machine made of files: the directories and the three tables this
    /// module reads, nothing else.
    struct Machine {
        root: PathBuf,
    }

    impl Machine {
        /// Named for the test, because the tests run side by side.
        fn new(test: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("lxb-storage-{test}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            Machine { root }
        }

        fn write(&self, path: &str, text: &str) {
            let path = self.root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }

        /// A disk, with the model its maker gave it.
        fn disk(&self, name: &str, number: &str, gib: u64, model: &str) {
            self.write(&format!("sys/class/block/{name}/dev"), number);
            self.write(
                &format!("sys/class/block/{name}/size"),
                &(gib * 1024 * 1024 * 2).to_string(),
            );
            self.write(&format!("sys/class/block/{name}/device/model"), model);
        }

        /// A partition of `disk`, sized in mebibytes.
        fn partition(&self, disk: &str, name: &str, number: &str, mib: u64) {
            for dir in [
                format!("sys/class/block/{name}"),
                format!("sys/class/block/{disk}/{name}"),
            ] {
                self.write(&format!("{dir}/dev"), number);
                self.write(&format!("{dir}/size"), &(mib * 1024 * 2).to_string());
                self.write(&format!("{dir}/partition"), "1");
            }
        }

        fn udev(&self, number: &str, lines: &[&str]) {
            let text: String = lines.iter().map(|line| format!("E:{line}\n")).collect();
            self.write(&format!("run/udev/data/b{number}"), &text);
        }

        fn mounts(&self, lines: &[&str]) {
            self.write("proc/self/mountinfo", &lines.join("\n"));
        }

        fn read(&self) -> Vec<Part> {
            read(&self.root, &|at: &Path| {
                Some(Room {
                    whole: 100 * 1024 * 1024 * 1024,
                    free: match at.to_str() {
                        Some("/") => 25 * 1024 * 1024 * 1024,
                        _ => 60 * 1024 * 1024 * 1024,
                    },
                })
            })
        }
    }

    impl Drop for Machine {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn devices(parts: &[Part]) -> Vec<&str> {
        parts.iter().map(|part| part.device.as_str()).collect()
    }

    /// The machine this page was written on, more or less: a system disk in
    /// three partitions and two disks of games, one mounted by hand and one by
    /// the desktop.
    fn workstation(test: &str) -> Machine {
        let machine = Machine::new(test);
        machine.disk("nvme0n1", "259:0", 954, "KINGSTON SKC3000S1024G   ");
        machine.partition("nvme0n1", "nvme0n1p1", "259:1", 512);
        machine.partition("nvme0n1", "nvme0n1p2", "259:2", 700 * 1024);
        machine.partition("nvme0n1", "nvme0n1p3", "259:3", 230 * 1024);
        machine.disk("sda", "8:0", 931, "WDC WD10EZEX-00R");
        machine.partition("sda", "sda1", "8:1", 931 * 1024);
        machine.disk("sdb", "8:16", 954, "Samsung SSD 870");
        machine.partition("sdb", "sdb10", "8:26", 954 * 1024);
        machine.disk("zram0", "253:0", 30, "");
        machine.udev(
            "8:1",
            &[
                "ID_FS_LABEL=Games_HDD",
                "ID_FS_LABEL_ENC=Games\\x20HDD",
                "ID_FS_TYPE=ext4",
            ],
        );
        machine.mounts(&[
            "26 32 0:24 / /proc rw - proc proc rw",
            "32 2 259:3 / / rw,relatime shared:1 - ext4 /dev/nvme0n1p3 rw",
            "128 32 259:2 / /home rw,relatime shared:220 - ext4 /dev/nvme0n1p2 rw",
            "237 32 259:1 / /boot rw,relatime shared:227 - vfat /dev/nvme0n1p1 rw",
            "272 32 8:26 / /mnt/Games\\040SSD rw - ext4 /dev/sdb10 rw",
            "294 29 8:1 / /run/media/kate/Games\\040HDD rw shared:1426 - ext4 /dev/sda1 rw",
        ]);
        machine.write(
            "proc/swaps",
            "Filename\tType\tSize\tUsed\tPriority\n/swapfile file 8388604 0 -1\n/dev/zram0 partition 31938556 7900940 100\n",
        );
        machine
    }

    #[test]
    fn every_partition_is_a_row_in_the_order_a_person_looks_for_them() {
        let parts = workstation("order").read();
        // The system, everybody's files, the two game disks in the order the
        // kernel numbers them, and what the machine starts from last of the
        // mounted ones. No whole disks, and no swap in memory.
        assert_eq!(
            devices(&parts),
            ["nvme0n1p3", "nvme0n1p2", "sda1", "sdb10", "nvme0n1p1"]
        );
        assert!(matches!(
            parts[0].standing,
            Standing::Mounted {
                role: Role::System,
                ..
            }
        ));
        assert!(matches!(
            parts[1].standing,
            Standing::Mounted {
                role: Role::Home,
                ..
            }
        ));
        assert!(matches!(
            parts[4].standing,
            Standing::Mounted {
                role: Role::Startup,
                ..
            }
        ));
    }

    /// A path is on the drive mounted at the longest part of it: a library on
    /// the games disk is the games disk's and not the system's under it, one
    /// in somebody's home is Home's, and nothing mounted anywhere near a path
    /// is no drive at all rather than the first one listed.
    #[test]
    fn a_path_is_on_the_drive_mounted_nearest_it() {
        let parts = workstation("holding").read();
        let on = |path: &str| holding(&parts, Path::new(path)).map(|part| part.device.as_str());
        assert_eq!(on("/mnt/Games SSD/SteamLibrary"), Some("sdb10"));
        assert_eq!(on("/home/kate/.local/share/Steam"), Some("nvme0n1p2"));
        assert_eq!(on("/opt/SteamLibrary"), Some("nvme0n1p3"));
        assert_eq!(
            on("/mnt/Games SSDs/SteamLibrary"),
            Some("nvme0n1p3"),
            "a prefix of a name is not the place"
        );
        assert_eq!(holding(&[], Path::new("/home")), None);
    }

    #[test]
    fn a_mounted_partition_carries_its_room_its_place_and_its_names() {
        let parts = workstation("room").read();
        let games = parts.iter().find(|part| part.device == "sda1").unwrap();
        assert_eq!(
            games.label.as_deref(),
            Some("Games HDD"),
            "the label with its space put back"
        );
        assert_eq!(games.kind.as_deref(), Some("ext4"));
        assert_eq!(games.model.as_deref(), Some("WDC WD10EZEX-00R"));
        let Standing::Mounted { at, room, .. } = &games.standing else {
            panic!("the games disk is mounted");
        };
        assert_eq!(at, Path::new("/run/media/kate/Games HDD"));
        assert_eq!(room.unwrap().used(), 0.4);

        let system = &parts[0];
        assert_eq!(
            system.model.as_deref(),
            Some("KINGSTON SKC3000S1024G"),
            "trimmed"
        );
        let Standing::Mounted { room, .. } = &system.standing else {
            panic!("the system is mounted");
        };
        assert_eq!(room.unwrap().used(), 0.75);
    }

    #[test]
    fn a_btrfs_root_mounted_five_times_is_one_row_at_the_top() {
        let machine = Machine::new("btrfs");
        machine.disk("nvme0n1", "259:0", 954, "Disk");
        machine.partition("nvme0n1", "nvme0n1p2", "259:2", 900 * 1024);
        // btrfs gives every mount the filesystem's anonymous number, which
        // names no block device; the source is what says which one it is.
        machine.mounts(&[
            "40 1 0:31 /@home /home rw - btrfs /dev/nvme0n1p2 rw,subvol=/@home",
            "38 1 0:31 /@ / rw - btrfs /dev/nvme0n1p2 rw,subvol=/@",
            "41 1 0:31 /@log /var/log rw - btrfs /dev/nvme0n1p2 rw,subvol=/@log",
        ]);
        let parts = machine.read();
        assert_eq!(devices(&parts), ["nvme0n1p2"]);
        assert_eq!(
            parts[0].standing,
            Standing::Mounted {
                at: PathBuf::from("/"),
                role: Role::System,
                room: Some(Room {
                    whole: 100 * 1024 * 1024 * 1024,
                    free: 25 * 1024 * 1024 * 1024,
                }),
            }
        );
    }

    #[test]
    fn an_encrypted_system_is_the_row_of_what_was_opened_on_the_disk_under_it() {
        let machine = Machine::new("encrypted");
        machine.disk("nvme0n1", "259:0", 954, "Disk");
        machine.partition("nvme0n1", "nvme0n1p2", "259:2", 900 * 1024);
        machine.write("sys/class/block/nvme0n1p2/holders/dm-0/dev", "254:0");
        machine.write("sys/class/block/dm-0/dev", "254:0");
        machine.write("sys/class/block/dm-0/size", "1887436800");
        machine.write("sys/class/block/dm-0/slaves/nvme0n1p2/dev", "259:2");
        machine.mounts(&["38 1 254:0 / / rw - ext4 /dev/mapper/root rw"]);
        let parts = machine.read();
        assert_eq!(
            devices(&parts),
            ["dm-0"],
            "the container is not a second row"
        );
        assert_eq!(
            parts[0].model.as_deref(),
            Some("Disk"),
            "the disk under the container"
        );
    }

    #[test]
    fn swap_and_idle_partitions_are_rows_and_plumbing_is_not() {
        let machine = Machine::new("swap");
        machine.disk("sda", "8:0", 954, "Disk");
        machine.partition("sda", "sda1", "8:1", 16);
        machine.partition("sda", "sda2", "8:2", 16 * 1024);
        machine.partition("sda", "sda3", "8:3", 400 * 1024);
        machine.partition("sda", "sda4", "8:4", 50);
        machine.udev(
            "8:3",
            &[
                "ID_FS_TYPE=ntfs",
                "ID_PART_ENTRY_NAME=Basic\\x20data\\x20partition",
            ],
        );
        machine.udev("8:4", &["ID_FS_TYPE=vfat", "ID_FS_LABEL=RECOVERY"]);
        machine.mounts(&[]);
        machine.write(
            "proc/swaps",
            "Filename Type Size Used Priority\n/dev/sda2 partition 16777212 0 -2\n",
        );
        let parts = machine.read();
        // sda1 is sixteen megabytes with nothing on it, which is Windows'
        // reserved partition; sda4 is as small but has a filesystem.
        assert_eq!(devices(&parts), ["sda2", "sda3", "sda4"]);
        assert_eq!(parts[0].standing, Standing::Memory);
        assert_eq!(parts[1].standing, Standing::Unused);
        assert_eq!(parts[1].part_name.as_deref(), Some("Basic data partition"));
        assert_eq!(parts[1].kind.as_deref(), Some("ntfs"));
        assert_eq!(parts[2].label.as_deref(), Some("RECOVERY"));
    }

    #[test]
    fn a_stick_with_no_partition_table_is_a_row_and_an_empty_card_slot_is_not() {
        let machine = Machine::new("stick");
        machine.disk("sdc", "8:32", 32, "Flash Drive");
        machine.udev("8:32", &["ID_FS_TYPE=exfat", "ID_FS_LABEL=STICK"]);
        machine.write("sys/class/block/sdd/dev", "8:48");
        machine.write("sys/class/block/sdd/size", "0");
        machine.udev("8:48", &["ID_FS_TYPE=vfat"]);
        machine.disk("sde", "8:64", 64, "Blank");
        machine.mounts(&[]);
        let parts = machine.read();
        assert_eq!(
            devices(&parts),
            ["sdc"],
            "a blank disk has no partitions to list"
        );
        assert_eq!(parts[0].model.as_deref(), Some("Flash Drive"));
    }

    #[test]
    fn a_machine_with_nothing_readable_has_no_rows_rather_than_a_failure() {
        assert!(Machine::new("empty").read().is_empty());
    }

    #[test]
    fn the_used_share_is_everything_that_is_not_free() {
        assert_eq!(Room { whole: 4, free: 1 }.used(), 0.75);
        assert_eq!(Room { whole: 0, free: 0 }.used(), 0.0);
        assert_eq!(
            Room { whole: 4, free: 9 }.used(),
            0.0,
            "never below nothing"
        );
    }

    #[test]
    fn device_names_sort_by_their_numbers() {
        let mut names = vec!["sda10", "sda2", "nvme1n1p1", "nvme0n1p12", "nvme0n1p3"];
        names.sort_by(|a, b| natural(a, b));
        assert_eq!(
            names,
            ["nvme0n1p3", "nvme0n1p12", "nvme1n1p1", "sda2", "sda10"]
        );
    }

    #[test]
    fn the_mount_table_is_read_past_its_optional_fields() {
        let mounts = parse_mountinfo(
            "36 35 98:0 /mnt1 /mnt/My\\040Films rw,noatime master:1 shared:2 - ext3 /dev/root rw\n",
        );
        assert_eq!(
            mounts,
            [Mounted {
                number: "98:0".into(),
                at: PathBuf::from("/mnt/My Films"),
                kind: "ext3".into(),
                source: "/dev/root".into(),
            }]
        );
    }
}
