//! PlayStation 3, as the shell holds it: one row, one column, and the games
//! RPCS3 plays.
//!
//! Everything that touches the machine — whether RPCS3 and the console's system
//! software are here, installing them, reading somebody's PS3 folder, installing
//! a package, fetching a disc's key — is a separate program, `lxb-rpcs3`, which
//! ships in a package of its own, exactly as `lxb-retroarch` and `lxb-heroic`
//! do. **This module is the half that belongs to the shell**, and where that
//! program is not on the machine every method here answers as though the
//! integration did not exist. RetroArch has no PlayStation 3 core, which is why
//! this is a column of its own rather than a console in RetroArch's.
//!
//! ## The journey, as the row
//!
//! ```text
//!   press PlayStation 3 ─► no RPCS3, or no system software ─► "Set it up?" ─┐
//!                       ├─ no games anywhere yet ─► "where are they?" ◄──────┘
//!                       └─ the PlayStation 3 column
//! ```
//!
//! Where the games are is the user's PS3 folder: the one they chose under
//! Settings > Games > PlayStation 3, or — until they choose one — the `ps3`
//! folder inside RetroArch's ROM folder, where RetroArch's own folder has one.
//! See [`folder`].
//!
//! ## A game, as its row
//!
//! A disc image, a game in a folder and a game RPCS3 has installed are played
//! by the command on their row. A package — a `.pkg`, or the zip it was
//! downloaded in — is installed on its first press and becomes the installed
//! game; a disc still encrypted has its key fetched on its press, and then
//! plays. Every picture a row wears is the game's own, read out of it by the
//! helper: its icon is the card, its backdrop stands behind the display, and
//! the picture PIC0 stands on the backdrop while it starts.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Mutex, OnceLock};

use serde::Deserialize;

use crate::apps::{self, Entry};
use crate::icons;

/// What the helper is called, and what the shell looks for.
pub const HELPER: &str = "lxb-rpcs3";

/// Every name RPCS3's windows and desktop entries go by: the flatpak's
/// application id, and the window class its own entry declares.
pub const WINDOW_NAMES: &[&str] = &["net.rpcs3.RPCS3", "rpcs3"];

/// The protocol revision this shell was written against. See
/// `crates/lxb-rpcs3/src/report.rs`, which declares the same number.
pub const PROTOCOL: u32 = 1;

/// What the row and the column are called. A name rather than a sentence, so
/// it is not translated — the same as Steam's, RetroArch's and Epic's.
pub const TITLE: &str = "PlayStation 3";

/// The mark the row, the column and every game without an icon wear. It
/// arrives with the package, under `share/lxb/glyphs`.
const MARK: &str = "lxb:ps3";

/// The name of the controller file the shell writes for RPCS3 before every
/// game — `input_configs/global/lxb.yml` under its configuration — and names
/// on its command line with `--input-config`. See [`input_config`].
pub const INPUT_CONFIG: &str = "lxb";

/// The shape of a PlayStation 3 game's icon, `ICON0.PNG`: 320 by 176, the
/// size the console's own menu draws every game at. The column's cards are
/// this shape, so every icon fills its card.
pub const ICON_ASPECT: f32 = 320.0 / 176.0;

/// The mark, or the shell's own pad where the package brought no drawing.
pub fn mark() -> &'static str {
    if icons::shaped(MARK) {
        MARK
    } else {
        icons::CATEGORY_GAMES
    }
}

// --- where the helper is ----------------------------------------------------

static FOUND: Mutex<Option<PathBuf>> = Mutex::new(None);

/// The shell's hidden `--rpcs3-helper`, where it named one.
static NAMED: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Whether this machine has the package — which decides whether there is a
/// PlayStation 3 row, column and Settings page at all.
pub fn offered() -> bool {
    FOUND.lock().is_ok_and(|found| found.is_some())
}

/// Look for the helper: `named` first, then beside the shell's own
/// executable, then `PATH` — [`crate::heroic::look_for_helper`]'s order, for
/// its reason: a local build must not pick up an older installed helper.
pub fn look_for_helper(named: Option<PathBuf>) -> Option<PathBuf> {
    let _ = NAMED.set(named);
    let found = find_helper(true);
    match &found {
        Some(at) => {
            tracing::info!(at = %at.display(), "the PlayStation 3 integration is installed")
        }
        None => tracing::debug!("no PlayStation 3 integration on this machine"),
    }
    if let Ok(mut held) = FOUND.lock() {
        *held = found.clone();
    }
    found
}

/// Ask the helper for the PlayStation 3's mark, for a machine whose
/// `share/lxb/glyphs` does not have it — the helper run from where it was
/// built, which is how the shell found it on the machine this was written on.
/// Once, while the shell starts; see [`icons::bring_mark`].
pub fn bring_the_mark(helper: &Path) {
    #[derive(Deserialize)]
    struct Mark {
        protocol: u32,
        name: String,
        drawing: String,
    }
    let said = Command::new(helper)
        .arg("mark")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    let mark = said
        .ok()
        .filter(|said| said.status.success())
        .and_then(|said| serde_json::from_slice::<Mark>(&said.stdout).ok());
    match mark {
        Some(mark) if mark.protocol == PROTOCOL && format!("lxb:{}", mark.name) == MARK => {
            icons::bring_mark(MARK, mark.drawing);
        }
        _ => tracing::debug!(helper = %helper.display(), "the PlayStation 3 helper gave no mark"),
    }
}

/// Look for the package again, and answer where it has come or gone since —
/// `None` where nothing has changed. The same five-second watch the Epic
/// Games package has: a package installed or removed while the session runs
/// puts the row on the bar or takes it off.
pub fn look_again() -> Option<Option<PathBuf>> {
    let now = find_helper(false);
    let mut held = FOUND.lock().ok()?;
    let changed = held.is_some() != now.is_some();
    *held = now.clone();
    changed.then_some(now)
}

fn find_helper(say: bool) -> Option<PathBuf> {
    let named = NAMED.get().cloned().flatten();
    if let Some(named) = named {
        return if executable(&named) {
            Some(named)
        } else {
            if say {
                tracing::warn!(at = %named.display(), "there is no executable helper there");
            }
            None
        };
    }
    std::env::current_exe()
        .ok()
        .as_deref()
        .and_then(Path::parent)
        .map(|dir| dir.join(HELPER))
        .filter(|at| executable(at))
        .or_else(|| {
            let path = std::env::var_os("PATH")?;
            std::env::split_paths(&path)
                .map(|dir| dir.join(HELPER))
                .find(|at| executable(at))
        })
}

fn executable(at: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(at).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

// --- where the games are ------------------------------------------------------

/// What a PS3 folder inside RetroArch's ROM folder may be called: RetroArch's
/// own short name for the console first, as RetroArch's column names its
/// folders.
const FOLDER_NAMES: &[&str] = &[
    "ps3",
    "playstation3",
    "playstation 3",
    "sony - playstation 3",
];

/// The folder somebody's PlayStation 3 games are in: the one they chose, or
/// until they have chosen one, the PS3 folder inside RetroArch's ROM folder.
///
/// `None` where neither is known, which is when the row asks.
pub fn folder() -> Option<PathBuf> {
    crate::settings::ps3_folder().or_else(|| {
        let roms = crate::settings::roms_folder()?;
        inside(&roms)
    })
}

/// A PS3 folder in `roms`, by any of the names one is given.
pub fn inside(roms: &Path) -> Option<PathBuf> {
    let listing = std::fs::read_dir(roms).ok()?;
    let mut found: Vec<(usize, PathBuf)> = listing
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            let rank = FOLDER_NAMES.iter().position(|known| *known == name)?;
            Some((rank, entry.path()))
        })
        .collect();
    found.sort();
    found.into_iter().next().map(|(_, path)| path)
}

/// Whether a folder of RetroArch's is the PS3 one — which RetroArch's column
/// leaves out wherever this integration is here to play it.
pub fn is_ps3_folder(name: &str) -> bool {
    FOLDER_NAMES.contains(&name.to_lowercase().as_str())
}

/// The row that asks where the games are, at the head of the column while
/// the answer is not known. Pressing it walks the disk for a folder.
pub fn folder_row() -> Entry {
    let comment = match folder() {
        Some(at) => crate::screenshot::abbreviated(&at),
        None => crate::i18n::text("ps3-choose-the-folder-your-games-are-in").to_string(),
    };
    Entry::Folder(apps::Folder {
        title_message: Some("shell-games-folder"),
        comment_message: None,
        identity: None,
        title: crate::i18n::text("shell-games-folder").to_string(),
        comment: Some(comment),
        icon: Some(icons::FILE_FOLDER.to_string()),
        entries: Vec::new(),
        place: Some(crate::files::Place::Volumes(crate::files::Shows::Folders(
            crate::settings::Picking::Ps3Folder,
        ))),
        chosen: false,
        over_the_list: true,
        person: None,
        portrait: None,
        used: None,
    })
}

/// Whether a file is a PlayStation 3 package this integration can install: a
/// `.pkg`, or a zip with one in it — which is how a downloaded game nearly
/// always arrives. Asked of a file pressed in Files, so it reads nothing but
/// the zip's own listing: the end of the file and the directory it points at.
pub fn holds_a_package(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if name.ends_with(".pkg") {
        return true;
    }
    name.ends_with(".zip")
        && zip_names(path).is_some_and(|names| {
            names
                .iter()
                .any(|entry| entry.to_lowercase().ends_with(".pkg") && !entry.ends_with('/'))
        })
}

/// The names in a zip's central directory, or `None` for what is not a zip.
/// Zip64 archives are read too, because a package can be past four gigabytes.
fn zip_names(path: &Path) -> Option<Vec<String>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    let tail_length = length.min(65_557 + 20);
    let mut tail = vec![0u8; tail_length as usize];
    file.seek(SeekFrom::Start(length - tail_length)).ok()?;
    file.read_exact(&mut tail).ok()?;
    let end = (0..tail.len().saturating_sub(21))
        .rev()
        .find(|&at| tail[at..at + 4] == [0x50, 0x4b, 0x05, 0x06])?;
    let le16 = |bytes: &[u8], at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
    let le32 = |bytes: &[u8], at: usize| {
        u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap_or_default())
    };
    let le64 = |bytes: &[u8], at: usize| {
        u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap_or_default())
    };
    let mut count = u64::from(le16(&tail, end + 10));
    let mut size = u64::from(le32(&tail, end + 12));
    let mut at = u64::from(le32(&tail, end + 16));
    if (count == 0xffff || size == 0xffff_ffff || at == 0xffff_ffff)
        && end >= 20
        && tail[end - 20..end - 16] == [0x50, 0x4b, 0x06, 0x07]
    {
        let mut record = [0u8; 56];
        file.seek(SeekFrom::Start(le64(&tail, end - 12))).ok()?;
        file.read_exact(&mut record).ok()?;
        count = le64(&record, 32);
        size = le64(&record, 40);
        at = le64(&record, 48);
    }
    if size > 64 * 1024 * 1024 || at + size > length {
        return None;
    }
    let mut directory = vec![0u8; size as usize];
    file.seek(SeekFrom::Start(at)).ok()?;
    file.read_exact(&mut directory).ok()?;
    let mut names = Vec::new();
    let mut here = 0usize;
    for _ in 0..count.min(100_000) {
        let header = directory.get(here..here + 46)?;
        if header[..4] != [0x50, 0x4b, 0x01, 0x02] {
            return None;
        }
        let name_length = le16(header, 28) as usize;
        let extra = le16(header, 30) as usize;
        let comment = le16(header, 32) as usize;
        let name = directory.get(here + 46..here + 46 + name_length)?;
        names.push(String::from_utf8_lossy(name).into_owned());
        here += 46 + name_length + extra + comment;
    }
    Some(names)
}

// --- what the helper says -----------------------------------------------------
//
// Written out here rather than shared with the helper's crate, on the terms
// the RetroArch integration gives: the two are installed apart, and what they
// share is a wire format declared on both sides.

#[derive(Debug, Clone, Deserialize)]
struct Probe {
    protocol: u32,
    rpcs3: Option<Installation>,
    flatpak: bool,
    firmware: Option<String>,
    config: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct Installation {
    /// RPCS3 with its own window.
    command: Vec<String>,
    /// RPCS3 for a game — see the helper's `report.rs` for why the two
    /// differ. A helper that does not say is answered with `command`.
    #[serde(default)]
    play: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct Progress {
    protocol: u32,
    stage: Stage,
    progress: Option<f32>,
    trouble: Option<Trouble>,
    note: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Stage {
    Remote,
    Installing,
    Downloading,
    Firmware,
    Unpacking,
    Adding,
    Done,
    Failed,
}

/// What went wrong, as the helper names it; the shell says it in the
/// person's language — see [`trouble_sentence`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Trouble {
    NoFlatpak,
    NoRpcs3,
    Offline,
    NoSpace,
    Refused,
    FirmwareRejected,
    NotAPackage,
    PackageRejected,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize)]
struct Library {
    protocol: u32,
    folder: Option<String>,
    games: Vec<Game>,
    unreadable: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct Game {
    id: String,
    serial: Option<String>,
    title: String,
    #[serde(default)]
    titles: BTreeMap<String, String>,
    form: Form,
    path: String,
    boot: Option<String>,
    icon: Option<String>,
    backdrop: Option<String>,
    overlay: Option<String>,
    preview: Option<String>,
    music: Option<String>,
    key: Key,
    trophies: Option<String>,
    /// Bytes on the disk.
    #[serde(default)]
    size: u64,
    /// Bytes RPCS3 keeps for it that it can make again.
    #[serde(default)]
    cache: u64,
    /// For a package: the updates to the same game beside it, installed after
    /// it by the same press.
    #[serde(default)]
    updates: Vec<String>,
}

/// How a game is kept — see [`apps::Ps3Game::form`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Form {
    Disc,
    Folder,
    Package,
    Installed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Key {
    Unneeded,
    Present,
    Missing,
}

#[derive(Debug, Clone, Deserialize)]
struct KeyResult {
    protocol: u32,
    key: Key,
    trouble: Option<Trouble>,
    note: String,
}

/// One game's trophy set, as a line of `lxb-rpcs3 trophies` — or, with
/// `done` set, the end of them.
#[derive(Debug, Clone, Deserialize)]
struct TrophySet {
    protocol: u32,
    set: String,
    /// The ids of the games that carry it — two copies of one game share it.
    games: Vec<String>,
    total: u32,
    unlocked: u32,
    list: Vec<TrophyLine>,
    done: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct TrophyLine {
    id: u32,
    name: String,
    detail: String,
    grade: Grade,
    hidden: bool,
    unlocked: bool,
    unlocked_at: Option<u64>,
    icon: Option<String>,
    /// The same picture grey, which is what a locked trophy wears — as
    /// Steam's and RetroAchievements' do.
    #[serde(default)]
    icon_locked: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Grade {
    Platinum,
    Gold,
    Silver,
    Bronze,
}

impl Grade {
    fn word(self) -> &'static str {
        crate::i18n::text(match self {
            Grade::Platinum => "ps3-trophy-platinum",
            Grade::Gold => "ps3-trophy-gold",
            Grade::Silver => "ps3-trophy-silver",
            Grade::Bronze => "ps3-trophy-bronze",
        })
    }
}

/// What `lxb-rpcs3 remove` answers.
#[derive(Debug, Clone, Deserialize)]
struct Removal {
    protocol: u32,
    removed: bool,
    trouble: Option<Trouble>,
    freed: u64,
    note: String,
}

#[derive(Debug, Clone, Deserialize)]
struct Permission {
    protocol: u32,
    permitted: bool,
    note: String,
}

// --- the worker -----------------------------------------------------------------

enum Ask {
    Probe,
    Setup,
    Scan(Option<PathBuf>),
    Key(PathBuf),
    /// Install a package, and after it the updates to the same game that were
    /// downloaded beside it — see [`Game::updates`].
    Add(PathBuf, Vec<PathBuf>),
    Permit(PathBuf),
    /// Every game's trophies, named in this language where the game has it.
    Trophies(Option<PathBuf>, String),
    /// Uninstall the game RPCS3 installed in this folder.
    Remove(PathBuf),
    /// Clear what RPCS3 keeps for the game with this serial and can make again.
    ClearCache(String),
}

enum Heard {
    Probed(Probe),
    SettingUp(Progress),
    Scanned(Library),
    Keyed(PathBuf, KeyResult),
    Adding(PathBuf, Progress),
    Permitted(Permission),
    Trophied(TrophySet),
    Removed(PathBuf, Removal),
    Cleared(String, Removal),
    Broken(String),
}

fn answer(helper: &Path, ask: &Ask, back: &Sender<Heard>) {
    let mut command = Command::new(helper);
    match ask {
        Ask::Probe => command.arg("probe"),
        Ask::Setup => command.arg("setup"),
        Ask::Scan(folder) => command.arg("scan").args(folder),
        Ask::Key(image) => command.arg("key").arg(image),
        Ask::Add(path, updates) => command.arg("add").arg(path).args(updates),
        Ask::Permit(folder) => command.arg("permit").arg(folder),
        Ask::Remove(folder) => command.arg("remove").arg(folder),
        Ask::ClearCache(serial) => command.arg("clear-cache").arg(serial),
        Ask::Trophies(folder, language) => command
            .arg("trophies")
            .args(folder)
            .arg("--language")
            .arg(language),
    };
    let mut child = match command.stdin(Stdio::null()).stdout(Stdio::piped()).spawn() {
        Ok(child) => child,
        Err(err) => {
            tracing::warn!(%err, "the PlayStation 3 helper could not be run");
            let _ = back.send(Heard::Broken(err.to_string()));
            return;
        }
    };
    let mut said = false;
    if let Some(out) = child.stdout.take() {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let heard = match ask {
                Ask::Probe => serde_json::from_str(line).map(Heard::Probed),
                Ask::Setup => serde_json::from_str(line).map(Heard::SettingUp),
                Ask::Scan(_) => serde_json::from_str(line).map(Heard::Scanned),
                Ask::Key(image) => {
                    serde_json::from_str(line).map(|said| Heard::Keyed(image.clone(), said))
                }
                Ask::Add(path, _) => {
                    serde_json::from_str(line).map(|said| Heard::Adding(path.clone(), said))
                }
                Ask::Permit(_) => serde_json::from_str(line).map(Heard::Permitted),
                Ask::Trophies(..) => serde_json::from_str(line).map(Heard::Trophied),
                Ask::Remove(folder) => {
                    serde_json::from_str(line).map(|said| Heard::Removed(folder.clone(), said))
                }
                Ask::ClearCache(serial) => {
                    serde_json::from_str(line).map(|said| Heard::Cleared(serial.clone(), said))
                }
            };
            match heard {
                Ok(heard) => {
                    said = true;
                    if back.send(heard).is_err() {
                        break;
                    }
                }
                Err(err) => {
                    tracing::warn!(%err, line, "the PlayStation 3 helper said something unreadable")
                }
            }
        }
    }
    let status = child.wait();
    if !said {
        let why = format!("{HELPER} answered nothing ({status:?})");
        tracing::warn!(why, "the PlayStation 3 helper");
        let _ = back.send(Heard::Broken(why));
    }
}

// --- the shell's own state ------------------------------------------------------

/// The PlayStation 3 as one field of the shell.
pub struct Ps3 {
    /// `None` on a machine without the package: every method then answers as
    /// though this file did not exist.
    inner: Option<Inner>,
}

struct Inner {
    ask: Sender<Ask>,
    heard: Receiver<Heard>,
    found: Found,
    /// The setup on screen, where one is happening.
    setting_up: Option<Working>,
    /// A package being installed, and which.
    adding: Option<(PathBuf, Working)>,
    /// A disc whose key is being fetched.
    keying: Option<PathBuf>,
    /// An installed game being uninstalled, by its folder.
    removing: Option<PathBuf>,
    /// A game whose cache is being cleared, by its serial.
    clearing: Option<String>,
    games: Vec<Game>,
    /// The folder the last scan was of, and whether a scan is in flight.
    scanned: Option<Option<PathBuf>>,
    reading: bool,
    /// The folder the last scan was *asked* about — what stops a helper that
    /// cannot answer being asked again every frame.
    asked: Option<Option<PathBuf>>,
    unreadable: Option<String>,
    broken: Option<String>,
    /// Every game's trophy set, by the set's name, as last read — and the
    /// sets a read under way has said so far, which replace them when it ends.
    trophies: BTreeMap<String, TrophySet>,
    trophies_coming: Option<BTreeMap<String, TrophySet>>,
}

enum Found {
    Asking,
    Absent {
        flatpak: bool,
    },
    Here {
        command: Vec<String>,
        play: Vec<String>,
        firmware: Option<String>,
        /// RPCS3's configuration folder, where its controller file goes.
        config: Option<PathBuf>,
    },
}

/// Something counting up on a panel: setting up, or a package installing.
#[derive(Debug, Clone)]
pub struct Working {
    pub stage: Stage,
    pub progress: Option<f32>,
    /// Set on the line that ends it, and taken by the shell once.
    ended: Option<Result<(), Trouble>>,
}

impl Working {
    fn new() -> Self {
        Working {
            stage: Stage::Remote,
            progress: None,
            ended: None,
        }
    }

    /// What is happening, as the panel says it.
    pub fn sentence(&self) -> String {
        let key = match self.stage {
            Stage::Remote | Stage::Installing => "ps3-getting-rpcs3",
            Stage::Downloading | Stage::Firmware => "ps3-getting-the-system-software",
            Stage::Unpacking | Stage::Adding => "ps3-installing-the-game",
            Stage::Done => "ps3-ready",
            Stage::Failed => "ps3-not-done",
        };
        let said = crate::i18n::text(key);
        match self.progress {
            Some(done) if !matches!(self.stage, Stage::Done | Stage::Failed) => {
                format!("{said} · {}%", (done * 100.0).round() as u32)
            }
            _ => said.to_string(),
        }
    }
}

/// What one turn of the loop changed.
#[derive(Debug, Default)]
pub struct Change {
    /// The row's line, the column, or both.
    pub rows: bool,
    /// A panel counting something up wants redrawing.
    pub panel: bool,
    /// The setup has just ended, and how.
    pub set_up: Option<Result<(), Trouble>>,
    /// A package has just finished installing, and how: its path, and the
    /// serial of the game it installed.
    pub added: Option<(PathBuf, Result<(), Trouble>)>,
    /// A disc's key has been asked for and the helper has answered: the image,
    /// and whether the disc can now be played.
    pub keyed: Option<(PathBuf, Result<(), Trouble>)>,
    /// An installed game has been uninstalled, or has not: its folder, and how.
    pub removed: Option<(PathBuf, Result<(), Trouble>)>,
    /// A game's cache has been cleared, or has not: its serial, and how.
    pub cleared: Option<(String, Result<(), Trouble>)>,
    /// A folder has just been read.
    pub scanned: bool,
    /// The trophies have just been read again.
    pub trophies: bool,
}

/// What pressing the row means, asked at the moment it is pressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Press {
    /// The helper has not answered yet, or something is being set up.
    Waiting,
    /// RPCS3 or the system software is missing: offer to get them.
    SetUp,
    /// It cannot be set up here, and why.
    Cannot(String),
    /// Nobody has said where the games are, and there are none installed.
    Folder,
    Enter,
}

impl Ps3 {
    /// A session without the integration.
    pub fn absent() -> Ps3 {
        Ps3 { inner: None }
    }

    /// Start the worker, and ask it what is on this machine.
    pub fn start(helper: PathBuf) -> Ps3 {
        let (ask, asked) = std::sync::mpsc::channel::<Ask>();
        let (back, heard) = std::sync::mpsc::channel::<Heard>();
        let started = std::thread::Builder::new()
            .name("lxb-rpcs3".to_string())
            .spawn(move || {
                for question in asked {
                    answer(&helper, &question, &back);
                }
            });
        if let Err(err) = started {
            tracing::error!(?err, "no thread for the PlayStation 3 integration");
            return Ps3::absent();
        }
        let mut ps3 = Ps3 {
            inner: Some(Inner {
                ask,
                heard,
                found: Found::Asking,
                setting_up: None,
                adding: None,
                keying: None,
                removing: None,
                clearing: None,
                games: Vec::new(),
                scanned: None,
                reading: false,
                asked: None,
                unreadable: None,
                broken: None,
                trophies: BTreeMap::new(),
                trophies_coming: None,
            }),
        };
        ps3.send(Ask::Probe);
        ps3
    }

    fn send(&mut self, ask: Ask) {
        let Some(inner) = self.inner.as_mut() else {
            return;
        };
        if inner.ask.send(ask).is_err() {
            inner.broken = Some("the worker has gone".to_string());
        }
    }

    /// Ask again what is on the machine.
    pub fn reprobe(&mut self) {
        if let Some(inner) = self.inner.as_mut() {
            inner.broken = None;
        }
        self.send(Ask::Probe);
    }

    /// Read the games again: the installed ones, and the folder's.
    ///
    /// Called when the folder changes, when something has been installed, and
    /// once RPCS3 has been found — never on a timer.
    pub fn rescan(&mut self) {
        let folder = folder();
        let Some(inner) = self.inner.as_mut() else {
            return;
        };
        if !matches!(inner.found, Found::Here { .. }) {
            return;
        }
        inner.reading = true;
        inner.asked = Some(folder.clone());
        self.send(Ask::Scan(folder));
    }

    /// Read the games again if the folder is not the one last read — a
    /// setting changed under the session, or RetroArch's folder was chosen.
    pub fn rescan_if_moved(&mut self) {
        let now = folder();
        let due = self.inner.as_ref().is_some_and(|inner| {
            matches!(inner.found, Found::Here { .. }) && inner.asked.as_ref() != Some(&now)
        });
        if due {
            self.rescan();
        }
    }

    /// Read every game's trophies again — after the games have been read, and
    /// after a game has ended, which is when something may have been
    /// unlocked.
    pub fn read_trophies(&mut self) {
        let folder = folder();
        let Some(inner) = self.inner.as_mut() else {
            return;
        };
        if !matches!(inner.found, Found::Here { .. }) || inner.trophies_coming.is_some() {
            return;
        }
        inner.trophies_coming = Some(BTreeMap::new());
        let language = crate::i18n::spoken().key().to_string();
        self.send(Ask::Trophies(folder, language));
    }

    /// Let a sandboxed RPCS3 read the folder just chosen.
    pub fn permit(&mut self, at: &Path) {
        self.send(Ask::Permit(at.to_path_buf()));
    }

    /// Get RPCS3 and the system software, whichever is missing.
    pub fn set_up(&mut self) {
        let Some(inner) = self.inner.as_mut() else {
            return;
        };
        if inner.setting_up.is_some() {
            return;
        }
        inner.setting_up = Some(Working::new());
        self.send(Ask::Setup);
    }

    pub fn setting_up(&self) -> Option<&Working> {
        self.inner.as_ref()?.setting_up.as_ref()
    }

    /// Install a package — a `.pkg`, or a zip with one in it — and the
    /// updates to the same game the scan found beside it, under one bar.
    pub fn add(&mut self, path: &Path) {
        let Some(inner) = self.inner.as_mut() else {
            return;
        };
        if inner.adding.is_some() {
            return;
        }
        let updates: Vec<PathBuf> = inner
            .games
            .iter()
            .find(|game| Path::new(&game.path) == path)
            .map(|game| game.updates.iter().map(PathBuf::from).collect())
            .unwrap_or_default();
        let mut working = Working::new();
        working.stage = Stage::Adding;
        inner.adding = Some((path.to_path_buf(), working));
        self.send(Ask::Add(path.to_path_buf(), updates));
    }

    /// The package being installed, and how far along it is.
    pub fn adding(&self) -> Option<(&Path, &Working)> {
        let (path, working) = self.inner.as_ref()?.adding.as_ref()?;
        Some((path.as_path(), working))
    }

    /// Fetch an encrypted disc's key.
    pub fn fetch_key(&mut self, image: &Path) {
        let Some(inner) = self.inner.as_mut() else {
            return;
        };
        if inner.keying.is_some() {
            return;
        }
        inner.keying = Some(image.to_path_buf());
        self.send(Ask::Key(image.to_path_buf()));
    }

    /// Uninstall the game RPCS3 installed in `folder` — see `lxb-rpcs3
    /// remove` for what goes and what stays. One at a time.
    pub fn remove(&mut self, folder: &Path) {
        let Some(inner) = self.inner.as_mut() else {
            return;
        };
        if inner.removing.is_some() {
            return;
        }
        inner.removing = Some(folder.to_path_buf());
        self.send(Ask::Remove(folder.to_path_buf()));
    }

    /// Clear what RPCS3 keeps for the game with `serial` and can make again —
    /// see `lxb-rpcs3 clear-cache`. One at a time.
    pub fn clear_cache(&mut self, serial: &str) {
        let Some(inner) = self.inner.as_mut() else {
            return;
        };
        if inner.clearing.is_some() {
            return;
        }
        inner.clearing = Some(serial.to_string());
        self.send(Ask::ClearCache(serial.to_string()));
    }

    /// Whether RPCS3's disk is being worked on: a package installing, a game
    /// uninstalling or a cache clearing — none of which is a moment to start
    /// another of them.
    pub fn busy(&self) -> bool {
        self.inner.as_ref().is_some_and(|inner| {
            inner.adding.is_some() || inner.removing.is_some() || inner.clearing.is_some()
        })
    }

    /// RPCS3's configuration folder, where the shell leaves it a controller
    /// file before each game.
    pub fn config_dir(&self) -> Option<&Path> {
        match &self.inner.as_ref()?.found {
            Found::Here { config, .. } => config.as_deref(),
            _ => None,
        }
    }

    /// What starts RPCS3 on its own, with no game: its own window.
    pub fn open_command(&self) -> Option<Vec<String>> {
        match &self.inner.as_ref()?.found {
            Found::Here { command, .. } => Some(command.clone()),
            _ => None,
        }
    }

    /// Everything the helper has said since the last frame.
    pub fn poll(&mut self) -> Change {
        let mut change = Change::default();
        let Some(inner) = self.inner.as_mut() else {
            return change;
        };
        let mut rescan = false;
        while let Ok(heard) = inner.heard.try_recv() {
            match heard {
                Heard::Probed(probe) => {
                    if probe.protocol != PROTOCOL {
                        tracing::warn!(
                            said = probe.protocol,
                            ours = PROTOCOL,
                            "the PlayStation 3 helper speaks another protocol"
                        );
                        inner.broken = Some("protocol".to_string());
                        change.rows = true;
                        continue;
                    }
                    let was_here = matches!(inner.found, Found::Here { .. });
                    inner.found = match probe.rpcs3 {
                        Some(installation) => Found::Here {
                            play: match installation.play.is_empty() {
                                true => installation.command.clone(),
                                false => installation.play,
                            },
                            command: installation.command,
                            firmware: probe.firmware,
                            config: probe.config.clone().map(PathBuf::from),
                        },
                        None => Found::Absent {
                            flatpak: probe.flatpak,
                        },
                    };
                    tracing::info!(config = ?probe.config, "the PlayStation 3 helper has probed");
                    crate::ps3_settings::known_config(match &inner.found {
                        Found::Here { config, .. } => config.clone(),
                        _ => None,
                    });
                    if !was_here && matches!(inner.found, Found::Here { .. }) {
                        rescan = true;
                    }
                    change.rows = true;
                }
                Heard::SettingUp(progress) if progress.protocol != PROTOCOL => {
                    tracing::warn!(said = progress.protocol, "a setup line of another protocol");
                    inner.broken = Some("protocol".to_string());
                    change.rows = true;
                }
                Heard::SettingUp(progress) => {
                    let working = inner.setting_up.get_or_insert_with(Working::new);
                    working.stage = progress.stage;
                    working.progress = progress.progress;
                    match progress.stage {
                        Stage::Done => working.ended = Some(Ok(())),
                        Stage::Failed => {
                            tracing::warn!(note = progress.note, "the PlayStation 3 setup failed");
                            working.ended = Some(Err(progress.trouble.unwrap_or(Trouble::Other)));
                        }
                        _ => {}
                    }
                    change.panel = true;
                    change.rows = true;
                }
                Heard::Adding(_, progress) if progress.protocol != PROTOCOL => {
                    tracing::warn!(
                        said = progress.protocol,
                        "an install line of another protocol"
                    );
                    inner.broken = Some("protocol".to_string());
                    change.rows = true;
                }
                Heard::Adding(path, progress) => {
                    if let Some((_, working)) = inner.adding.as_mut() {
                        working.stage = progress.stage;
                        working.progress = progress.progress;
                        match progress.stage {
                            Stage::Done => working.ended = Some(Ok(())),
                            Stage::Failed => {
                                tracing::warn!(path = %path.display(), note = progress.note, "the package was not installed");
                                working.ended =
                                    Some(Err(progress.trouble.unwrap_or(Trouble::Other)));
                            }
                            _ => {}
                        }
                    }
                    change.panel = true;
                    change.rows = true;
                }
                Heard::Keyed(image, said) => {
                    tracing::info!(image = %image.display(), note = said.note, "a disc's key");
                    inner.keying = None;
                    let result = match said.key {
                        Key::Present | Key::Unneeded if said.protocol == PROTOCOL => Ok(()),
                        _ => Err(said.trouble.unwrap_or(Trouble::Other)),
                    };
                    if result.is_ok() {
                        if let Some(game) = inner
                            .games
                            .iter_mut()
                            .find(|game| Path::new(&game.path) == image)
                        {
                            game.key = Key::Present;
                        }
                    }
                    change.keyed = Some((image, result));
                    change.rows = true;
                }
                Heard::Scanned(library) => {
                    inner.reading = false;
                    if library.protocol != PROTOCOL {
                        inner.broken = Some("protocol".to_string());
                        change.rows = true;
                        continue;
                    }
                    let folder = library.folder.map(PathBuf::from);
                    // An answer about a folder nobody is asking about any more
                    // is somebody else's games.
                    if inner.asked.as_ref().is_some_and(|asked| *asked != folder) {
                        continue;
                    }
                    inner.scanned = Some(folder);
                    inner.unreadable = library.unreadable;
                    inner.games = library.games;
                    change.scanned = true;
                    change.rows = true;
                }
                Heard::Trophied(set) if set.protocol != PROTOCOL => {
                    tracing::warn!(said = set.protocol, "a trophy line of another protocol");
                    inner.trophies_coming = None;
                }
                Heard::Trophied(set) if set.done => {
                    if let Some(read) = inner.trophies_coming.take() {
                        inner.trophies = read;
                        change.trophies = true;
                    }
                }
                Heard::Trophied(set) => {
                    if let Some(read) = inner.trophies_coming.as_mut() {
                        read.insert(set.set.clone(), set);
                    }
                }
                Heard::Removed(folder, said) => {
                    tracing::info!(
                        folder = %folder.display(),
                        removed = said.removed,
                        freed = said.freed,
                        note = said.note,
                        "a PlayStation 3 game uninstalled"
                    );
                    inner.removing = None;
                    let result = if said.removed && said.protocol == PROTOCOL {
                        // Off the column at once rather than at the scan that
                        // follows: the press was the last thing it answered.
                        inner.games.retain(|game| Path::new(&game.path) != folder);
                        Ok(())
                    } else {
                        Err(said.trouble.unwrap_or(Trouble::Other))
                    };
                    change.removed = Some((folder, result));
                    change.rows = true;
                    rescan = true;
                }
                Heard::Cleared(serial, said) => {
                    tracing::info!(
                        serial,
                        cleared = said.removed,
                        freed = said.freed,
                        note = said.note,
                        "a PlayStation 3 game's cache cleared"
                    );
                    inner.clearing = None;
                    let result = if said.removed && said.protocol == PROTOCOL {
                        for game in &mut inner.games {
                            if game.serial.as_deref() == Some(serial.as_str()) {
                                game.cache = 0;
                            }
                        }
                        Ok(())
                    } else {
                        Err(said.trouble.unwrap_or(Trouble::Other))
                    };
                    change.cleared = Some((serial, result));
                    change.rows = true;
                }
                Heard::Permitted(permission) => {
                    tracing::info!(
                        permitted = permission.permitted,
                        note = permission.note,
                        protocol = permission.protocol,
                        "the PS3 folder's permission"
                    );
                }
                Heard::Broken(why) => {
                    inner.reading = false;
                    inner.keying = None;
                    if let Some(folder) = inner.removing.take() {
                        change.removed = Some((folder, Err(Trouble::Other)));
                    }
                    if let Some(serial) = inner.clearing.take() {
                        change.cleared = Some((serial, Err(Trouble::Other)));
                    }
                    inner.trophies_coming = None;
                    inner.broken = Some(why);
                    change.rows = true;
                }
            }
        }
        // Ends, taken once.
        if let Some(working) = inner.setting_up.as_mut() {
            if let Some(ended) = working.ended.take() {
                change.set_up = Some(ended);
                inner.setting_up = None;
                // Whatever came of it, the machine is asked again what it has.
                let _ = inner.ask.send(Ask::Probe);
            }
        }
        if let Some((path, working)) = inner.adding.as_mut() {
            if let Some(ended) = working.ended.take() {
                change.added = Some((path.clone(), ended));
                inner.adding = None;
                rescan = true;
            }
        }
        if rescan {
            self.rescan();
        }
        // The games have been read: their trophies are read next, which is
        // the one question that walks every game's trophy set.
        if change.scanned {
            self.read_trophies();
        }
        change
    }

    /// What pressing the row does.
    pub fn press(&self) -> Press {
        let Some(inner) = self.inner.as_ref() else {
            return Press::Waiting;
        };
        if inner.broken.is_some() {
            return Press::Cannot(
                crate::i18n::text("shell-not-working-on-this-machine").to_string(),
            );
        }
        if inner.setting_up.is_some() {
            return Press::Waiting;
        }
        match &inner.found {
            Found::Asking => Press::Waiting,
            Found::Absent { flatpak: true } => Press::SetUp,
            Found::Absent { flatpak: false } => Press::Cannot(
                crate::i18n::text("shell-it-cannot-be-downloaded-on-this-machine").to_string(),
            ),
            Found::Here { firmware: None, .. } => Press::SetUp,
            Found::Here { .. } if folder().is_none() && inner.games.is_empty() => Press::Folder,
            Found::Here { .. } => Press::Enter,
        }
    }

    /// The line under the row. `None` on a machine without the integration,
    /// which takes the row off the bar.
    pub fn note(&self) -> Option<String> {
        let inner = self.inner.as_ref()?;
        if let Some(working) = &inner.setting_up {
            return Some(working.sentence());
        }
        if inner.broken.is_some() {
            return Some(crate::i18n::text("shell-not-working-on-this-machine").to_string());
        }
        if let Some((path, working)) = &inner.adding {
            let name = inner
                .games
                .iter()
                .find(|game| Path::new(&game.path) == path)
                .map(title_of)
                .unwrap_or_default();
            return Some(format!("{name} · {}", working.sentence()));
        }
        Some(match &inner.found {
            Found::Asking => crate::i18n::text("ps3-looking").to_string(),
            Found::Absent { flatpak: true } => crate::i18n::text("ps3-press-to-set-up").to_string(),
            Found::Absent { flatpak: false } => {
                crate::i18n::text("shell-it-cannot-be-downloaded-on-this-machine").to_string()
            }
            Found::Here { firmware: None, .. } => {
                crate::i18n::text("ps3-press-to-set-up").to_string()
            }
            Found::Here { .. } if inner.reading && inner.games.is_empty() => {
                crate::i18n::text("ps3-looking").to_string()
            }
            Found::Here { .. } if inner.unreadable.is_some() && inner.games.is_empty() => {
                crate::i18n::text("ps3-the-folder-cannot-be-read").to_string()
            }
            Found::Here { .. } if folder().is_none() && inner.games.is_empty() => {
                crate::i18n::text("ps3-choose-the-folder-your-games-are-in").to_string()
            }
            Found::Here { .. } => {
                let count = inner.games.len();
                crate::message!("count-games", "count" => count)
            }
        })
    }

    /// The row's line as a bar, while something counts up.
    pub fn arriving(&self) -> Option<f32> {
        let inner = self.inner.as_ref()?;
        match (&inner.setting_up, &inner.adding) {
            (Some(working), _) => working.progress,
            (None, Some((_, working))) => working.progress,
            _ => None,
        }
    }

    /// The column: the question of where the games are while it is open, and
    /// the games by name. Empty — which takes the column off the bar — until
    /// RPCS3 and its system software are here, and while there is nothing to
    /// show and nothing to ask.
    pub fn rows(&self) -> Vec<Entry> {
        let Some(inner) = self.inner.as_ref() else {
            return Vec::new();
        };
        let Found::Here {
            play,
            firmware: Some(_),
            ..
        } = &inner.found
        else {
            return Vec::new();
        };
        let mut rows = Vec::new();
        // The question stands at the head for as long as it is open, as
        // RetroArch's does — and while the folder is being read, because
        // taking the column away under the press that answered it would carry
        // the cursor off to the next column.
        let from_folder = inner.games.iter().any(|game| game.form != Form::Installed);
        if folder().is_none() || inner.unreadable.is_some() || (inner.reading && !from_folder) {
            rows.push(folder_row());
        }
        let adding = inner.adding.as_ref();
        rows.extend(inner.games.iter().map(|game| game_row(play, game, adding)));
        rows
    }

    /// Every game's trophies, as rows of the Trophies column: the game, and
    /// in it each trophy — unlocked first — the way Steam's and Epic's are.
    /// Read only: nothing here unlocks, starts or writes anything.
    pub fn trophy_rows(&self) -> Vec<Entry> {
        use crate::trophies::{Key, Platform, Row};
        let Some(inner) = self.inner.as_ref() else {
            return Vec::new();
        };
        let mut rows = Vec::new();
        for (set_name, set) in &inner.trophies {
            if set.list.is_empty() {
                continue;
            }
            let game = inner.games.iter().find(|game| {
                set.games.contains(&game.id) || game.trophies.as_deref() == Some(set_name)
            });
            // Steam's three sections, in Steam's order: what has been earned,
            // what can be seen and has not, and the secrets — each counted
            // under its own heading, a hidden trophy never among the locked.
            let section_of = |trophy: &TrophyLine| match (trophy.unlocked, trophy.hidden) {
                (true, _) => 0,
                (false, false) => 1,
                (false, true) => 2,
            };
            let mut counts = [0usize; 3];
            for trophy in &set.list {
                counts[section_of(trophy)] += 1;
            }
            let mut list: Vec<&TrophyLine> = set.list.iter().collect();
            list.sort_by_key(|trophy| (section_of(trophy), trophy.id));
            let trophies = list
                .into_iter()
                .map(|trophy| {
                    let section = section_of(trophy);
                    let secret = section == 2;
                    let state = match (trophy.unlocked, trophy.unlocked_at) {
                        (true, Some(at)) => crate::trophies::unlocked_at(at)
                            .unwrap_or_else(|| crate::i18n::text("shell-unlocked").to_string()),
                        (true, None) => crate::i18n::text("shell-unlocked").to_string(),
                        (false, _) => crate::i18n::text("shell-locked").to_string(),
                    };
                    let grade = trophy.grade.word();
                    let name = if trophy.name.is_empty() {
                        crate::i18n::text("shell-hidden-achievement").to_string()
                    } else {
                        trophy.name.clone()
                    };
                    Entry::Trophy(Row {
                        key: Key::Ps3Trophy(set_name.clone(), trophy.id),
                        facts: apps::Facts {
                            // A secret is kept on the row and told on the
                            // press, as Steam's are.
                            title: if secret {
                                crate::i18n::text("shell-hidden-achievement").to_string()
                            } else {
                                name.clone()
                            },
                            comment: if secret {
                                crate::i18n::text("shell-press-to-reveal-details").to_string()
                            } else {
                                crate::message!(
                                    "ps3-trophy-summary",
                                    "description" => trophy.detail.as_str(),
                                    "grade" => grade,
                                    "state" => state.clone()
                                )
                            },
                            icon: icons::CATEGORY_TROPHIES.into(),
                            about: apps::About::Listed(vec![
                                (crate::i18n::text("shell-achievement").to_string(), name),
                                (
                                    crate::i18n::text("shell-description").to_string(),
                                    trophy.detail.clone(),
                                ),
                                (
                                    crate::i18n::text("ps3-trophy-grade").to_string(),
                                    grade.to_string(),
                                ),
                                (crate::i18n::text("shell-status").to_string(), state),
                            ]),
                        },
                        // Grey until it is earned, as every other trophy in the
                        // column is — a secret one too, as Steam's is.
                        picture: if trophy.unlocked {
                            trophy.icon.as_ref()
                        } else {
                            trophy.icon_locked.as_ref().or(trophy.icon.as_ref())
                        }
                        .map(PathBuf::from),
                        entries: None,
                        section: Some(match section {
                            0 => {
                                crate::message!("achievements-unlocked-count", "count" => counts[0])
                            }
                            1 => crate::message!("achievements-locked-count", "count" => counts[1]),
                            _ => crate::message!("achievements-hidden-count", "count" => counts[2]),
                        }),
                        shape: None,
                        installed: None,
                        platform: None,
                    })
                })
                .collect();
            rows.push(Entry::Trophy(Row {
                key: Key::Ps3Game(set_name.clone()),
                facts: apps::Facts {
                    title: game.map_or_else(|| set_name.clone(), title_of),
                    comment: format!(
                        "{TITLE} · {}",
                        crate::message!(
                            "achievements-unlocked-of",
                            "unlocked" => set.unlocked,
                            "total" => set.total
                        )
                    ),
                    icon: icons::CATEGORY_TROPHIES.into(),
                    about: apps::About::Listed(Vec::new()),
                },
                picture: game.and_then(|game| game.icon.as_ref()).map(PathBuf::from),
                entries: Some(trophies),
                section: None,
                // The icon's own shape, which every PS3 game's is.
                shape: Some(ICON_ASPECT),
                installed: Some(game.is_some_and(|game| game.form != Form::Package)),
                platform: Some(Platform {
                    name: TITLE.to_string(),
                    mark: mark().to_string(),
                }),
            }));
        }
        rows
    }
}

/// RPCS3's controller file for one game: a player per pad, in the order the
/// shell hands them over, each read through SDL under SDL's own name for it.
///
/// **Which device is which** is the whole difficulty, as it is for RetroArch
/// (see [`crate::pads`]): every pad the guard holds is on the machine twice,
/// the silent original and the copy, under one name. RPCS3 names SDL pads
/// `<name> <n>`, counting same-named pads in the order SDL opened them, so the
/// launch lists the copies in `SDL_JOYSTICK_DEVICE` — SDL opens those first,
/// before its own search finds the originals — and the copy of the first pad
/// of a name is then that name's `1`. `players` are those `<name> <n>`s.
///
/// The device and the buttons are written — `buttons` is which of the pad's
/// buttons each PS3 button is on, the same for every player (see
/// [`crate::ps3_settings::buttons`]) — and RPCS3 fills everything else in
/// from its own SDL defaults. The PS button is Select and Start together,
/// because the guide button never reaches an application — it is the way back
/// to the shell.
pub fn input_config(players: &[String], buttons: &[(&str, String)]) -> String {
    let mut out = String::new();
    for index in 0..7 {
        out.push_str(&format!("Player {} Input:\n", index + 1));
        match players.get(index) {
            Some(device) => {
                out.push_str("  Handler: SDL\n");
                out.push_str(&format!("  Device: {}\n", yaml_string(device)));
                out.push_str("  Config:\n");
                for (button, on) in buttons {
                    out.push_str(&format!("    {button}: {}\n", yaml_string(on)));
                }
                out.push_str("    PS Button: \"Back&Start\"\n");
            }
            None => {
                out.push_str("  Handler: \"Null\"\n");
                out.push_str("  Device: \"Null\"\n");
            }
        }
    }
    out
}

/// Write [`input_config`] where RPCS3 reads it. Whether it was written.
pub fn write_input_config(config: &Path, players: &[String]) -> bool {
    let folder = config.join("input_configs/global");
    let at = folder.join(format!("{INPUT_CONFIG}.yml"));
    let written = std::fs::create_dir_all(&folder)
        .and_then(|()| std::fs::write(&at, input_config(players, &crate::ps3_settings::buttons())));
    match written {
        Ok(()) => true,
        Err(err) => {
            tracing::warn!(at = %at.display(), %err, "RPCS3's controllers could not be written");
            false
        }
    }
}

/// RPCS3's own questions that a game started from the bar must not stop for:
/// keys of its window's settings (`GuiConfigs/CurrentSettings.ini`, section
/// `[main_window]`), each turned off.
///
/// - `confirmationBoxExitGame` is "Exit Game?", which RPCS3 asks, over a game
///   it has just taken out of full screen, whenever anything closes the game's
///   window, the guide's Close included. That Close is the person's answer
///   already.
/// - `infoBoxEnabledWelcome` is RPCS3's first-start welcome. A game started
///   without RPCS3's window still puts it up beside the game, and closing it
///   quits the game.
const QUIET: [&str; 2] = ["confirmationBoxExitGame", "infoBoxEnabledWelcome"];

/// The section of RPCS3's window settings the [`QUIET`] keys are in.
const QUIET_SECTION: &str = "[main_window]";

/// `ini` with every [`QUIET`] key set to `false`, and every other line as it
/// was — the file is RPCS3's, and holds its window's own state.
pub fn quiet_settings(ini: &str) -> String {
    let mut lines: Vec<String> = ini.lines().map(str::to_string).collect();
    let start = lines.iter().position(|line| line.trim() == QUIET_SECTION);
    let Some(start) = start else {
        let mut out = ini.to_string();
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(QUIET_SECTION);
        out.push('\n');
        for key in QUIET {
            out.push_str(&format!("{key}=false\n"));
        }
        return out;
    };
    let end = lines[start + 1..]
        .iter()
        .position(|line| line.trim_start().starts_with('['))
        .map_or(lines.len(), |at| start + 1 + at);
    let mut missing = Vec::new();
    for key in QUIET {
        let found = lines[start + 1..end].iter().position(|line| {
            line.split_once('=')
                .is_some_and(|(name, _)| name.trim() == key)
        });
        match found {
            Some(at) => lines[start + 1 + at] = format!("{key}=false"),
            None => missing.push(format!("{key}=false")),
        }
    }
    // After the section's last setting, not after the blank line that parts
    // it from the next section.
    let mut at = end;
    while at > start + 1 && lines[at - 1].trim().is_empty() {
        at -= 1;
    }
    lines.splice(at..at, missing);
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Turn RPCS3's [`QUIET`] questions off in `config`, before a game. Written
/// only where something changes. Whether they are off.
pub fn write_quiet_settings(config: &Path) -> bool {
    let folder = config.join("GuiConfigs");
    let at = folder.join("CurrentSettings.ini");
    let before = match std::fs::read_to_string(&at) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => {
            tracing::warn!(at = %at.display(), %err, "RPCS3's window settings could not be read");
            return false;
        }
    };
    let after = quiet_settings(&before);
    if after == before {
        return true;
    }
    let written = std::fs::create_dir_all(&folder).and_then(|()| std::fs::write(&at, after));
    match written {
        Ok(()) => true,
        Err(err) => {
            tracing::warn!(at = %at.display(), %err, "RPCS3's questions could not be turned off");
            false
        }
    }
}

/// A YAML double-quoted string.
fn yaml_string(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The players' names for RPCS3, from each pad's SDL name in player order:
/// the second pad of a name is `<name> 2`, and so on.
pub fn player_names(sdl_names: &[String]) -> Vec<String> {
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    sdl_names
        .iter()
        .map(|name| {
            let count = seen.entry(name.as_str()).or_insert(0);
            *count += 1;
            format!("{name} {count}")
        })
        .collect()
}

/// A game's name in the language the shell speaks, where it has one: the
/// exact tag first, then any of the same language, then its own default.
fn title_of(game: &Game) -> String {
    let tag = crate::i18n::spoken().key();
    let primary = tag.split('-').next().unwrap_or(tag);
    game.titles
        .get(tag)
        .or_else(|| {
            game.titles
                .iter()
                .find(|(key, _)| key.split('-').next() == Some(primary))
                .map(|(_, title)| title)
        })
        .cloned()
        .unwrap_or_else(|| game.title.clone())
}

fn game_row(command: &[String], game: &Game, adding: Option<&(PathBuf, Working)>) -> Entry {
    let path = PathBuf::from(&game.path);
    let being_added = adding
        .filter(|(at, _)| *at == path)
        .map(|(_, working)| working);
    let note = match (game.form, being_added) {
        (_, Some(working)) => working.sentence(),
        (Form::Package, None) => crate::i18n::text("ps3-press-to-install").to_string(),
        (_, None) if game.key == Key::Missing => {
            crate::i18n::text("ps3-press-to-get-it-ready").to_string()
        }
        (Form::Installed, None) => crate::i18n::text("ps3-installed").to_string(),
        (Form::Disc | Form::Folder, None) => crate::i18n::text("ps3-ready-to-play").to_string(),
    };
    let start = game.boot.as_ref().map(|boot| {
        let mut argv = command.to_vec();
        argv.extend([
            "--no-gui".to_string(),
            "--fullscreen".to_string(),
            "--input-config".to_string(),
            INPUT_CONFIG.to_string(),
            boot.clone(),
        ]);
        argv
    });
    Entry::Ps3Game(apps::Ps3Game {
        id: game.id.clone(),
        name: title_of(game),
        note,
        progress: being_added
            .and_then(|working| working.progress)
            .map(|share| apps::Arriving {
                share,
                stuck: false,
            }),
        form: game.form,
        path,
        serial: game.serial.clone(),
        start,
        needs_key: game.key == Key::Missing,
        cover: game.icon.as_ref().map(PathBuf::from),
        hero: game.backdrop.as_ref().map(PathBuf::from),
        logo: game.overlay.as_ref().map(PathBuf::from),
        preview: game.preview.as_ref().map(PathBuf::from),
        music: game.music.as_ref().map(PathBuf::from),
        size: game.size,
        cache: game.cache,
    })
}

/// What went wrong, as a sentence for a panel — plain words, and where there
/// is something to do, the thing to do. The helper's own words are the log's.
pub fn trouble_sentence(trouble: Trouble) -> &'static str {
    crate::i18n::text(match trouble {
        Trouble::NoFlatpak => "shell-it-cannot-be-downloaded-on-this-machine",
        Trouble::NoRpcs3 => "ps3-trouble-no-rpcs3",
        Trouble::Offline => "ps3-trouble-offline",
        Trouble::NoSpace => "ps3-trouble-no-space",
        Trouble::Refused => "ps3-trouble-refused",
        Trouble::FirmwareRejected => "ps3-trouble-firmware",
        Trouble::NotAPackage => "ps3-trouble-not-a-package",
        Trouble::PackageRejected => "ps3-trouble-package",
        Trouble::Other => "ps3-trouble-other",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(title: &str, titles: &[(&str, &str)]) -> Game {
        Game {
            id: "/games/x.iso".to_string(),
            serial: Some("BLUS30359".to_string()),
            title: title.to_string(),
            titles: titles
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            form: Form::Disc,
            path: "/games/x.iso".to_string(),
            boot: Some("/games/x.iso".to_string()),
            icon: None,
            backdrop: None,
            overlay: None,
            preview: None,
            music: None,
            key: Key::Unneeded,
            trophies: None,
            size: 0,
            cache: 0,
            updates: Vec::new(),
        }
    }

    /// A zip written by hand: a stored entry per name, and the directory.
    fn zip_of(names: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut directory = Vec::new();
        for name in names {
            let local = out.len() as u32;
            out.extend_from_slice(&[0x50, 0x4b, 0x03, 0x04]);
            out.extend_from_slice(&[0u8; 22]);
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            directory.extend_from_slice(&[0x50, 0x4b, 0x01, 0x02]);
            directory.extend_from_slice(&[0u8; 24]);
            directory.extend_from_slice(&(name.len() as u16).to_le_bytes());
            directory.extend_from_slice(&[0u8; 12]);
            directory.extend_from_slice(&local.to_le_bytes());
            directory.extend_from_slice(name.as_bytes());
        }
        let at = out.len() as u32;
        out.extend_from_slice(&directory);
        out.extend_from_slice(&[0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0]);
        out.extend_from_slice(&(names.len() as u16).to_le_bytes());
        out.extend_from_slice(&(names.len() as u16).to_le_bytes());
        out.extend_from_slice(&(directory.len() as u32).to_le_bytes());
        out.extend_from_slice(&at.to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out
    }

    /// The Tekken 5 shape, and the zip of somebody's holiday photos.
    #[test]
    fn a_zip_is_a_package_only_with_a_package_in_it() {
        let dir = std::env::temp_dir().join(format!("lxb-ps3-zip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let game = dir.join("Tekken 5.zip");
        std::fs::write(
            &game,
            zip_of(&["x.pkg", "EP9000-NPEA00019_00-TEKKENRETAIL0000.rap"]),
        )
        .unwrap();
        let photos = dir.join("Holiday.zip");
        std::fs::write(&photos, zip_of(&["beach.jpg", "folder.pkg/"])).unwrap();
        assert!(holds_a_package(&game));
        assert!(!holds_a_package(&photos));
        assert!(holds_a_package(Path::new("/nowhere/game.PKG")));
        assert!(!holds_a_package(Path::new("/nowhere/game.iso")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_ps3_folder_is_found_by_any_of_its_names() {
        let dir = std::env::temp_dir().join(format!("lxb-ps3-folder-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("psp")).unwrap();
        assert_eq!(inside(&dir), None);
        std::fs::create_dir_all(dir.join("PlayStation 3")).unwrap();
        assert_eq!(inside(&dir), Some(dir.join("PlayStation 3")));
        std::fs::create_dir_all(dir.join("ps3")).unwrap();
        assert_eq!(inside(&dir), Some(dir.join("ps3")));
        assert!(is_ps3_folder("PS3"));
        assert!(!is_ps3_folder("ps2"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A disc starts in RPCS3 with no window of its own, full screen.
    #[test]
    fn a_disc_is_started_with_rpcs3s_own_command() {
        let command = vec![
            "flatpak".to_string(),
            "run".to_string(),
            "--user".to_string(),
            "net.rpcs3.RPCS3".to_string(),
        ];
        let Entry::Ps3Game(row) = game_row(&command, &game("TEKKEN 6", &[]), None) else {
            panic!("a game row");
        };
        let start = row.start.expect("a disc starts");
        assert_eq!(start[..4], command[..]);
        assert_eq!(
            start[4..],
            [
                "--no-gui",
                "--fullscreen",
                "--input-config",
                INPUT_CONFIG,
                "/games/x.iso"
            ]
        );
        assert!(!row.needs_key);
    }

    /// Two pads of one model and one of another, as RPCS3 names them.
    #[test]
    fn players_are_named_as_rpcs3_counts_them() {
        let names = player_names(&[
            "Xbox 360 Controller".to_string(),
            "8BitDo Ultimate 2 Wireless Controller".to_string(),
            "Xbox 360 Controller".to_string(),
        ]);
        assert_eq!(
            names,
            [
                "Xbox 360 Controller 1",
                "8BitDo Ultimate 2 Wireless Controller 1",
                "Xbox 360 Controller 2"
            ]
        );
        let file = input_config(&names[..1], &[("Cross", "East".to_string())]);
        assert!(file
            .starts_with("Player 1 Input:\n  Handler: SDL\n  Device: \"Xbox 360 Controller 1\"\n"));
        assert!(file.contains("  Config:\n    Cross: \"East\"\n    PS Button: \"Back&Start\"\n"));
        assert!(file.contains("Player 2 Input:\n  Handler: \"Null\"\n"));
        assert!(file.contains("Player 7 Input:"));
        assert_eq!(yaml_string(r#"a "b" \c"#), r#""a \"b\" \\c""#);
    }

    /// RPCS3's questions go off, and nothing else in its settings moves.
    #[test]
    fn rpcs3_is_told_not_to_ask() {
        // RPCS3's own file, as a first start leaves it: one of the two keys
        // already there, the other missing, a section after it.
        let before = "[GameList]\nsortCol=1\n\n[main_window]\nconfirmationBoxExitGame=true\n\
                      geometry=@ByteArray(\\x1)\n\n[Meta]\ncurrentStylesheet=Darker\n";
        let after = quiet_settings(before);
        assert_eq!(
            after,
            "[GameList]\nsortCol=1\n\n[main_window]\nconfirmationBoxExitGame=false\n\
             geometry=@ByteArray(\\x1)\ninfoBoxEnabledWelcome=false\n\n[Meta]\n\
             currentStylesheet=Darker\n"
        );
        // Asked again, nothing changes, so nothing is written.
        assert_eq!(quiet_settings(&after), after);
        // No section, and no file at all.
        assert_eq!(
            quiet_settings("[Meta]\na=1"),
            "[Meta]\na=1\n\n[main_window]\nconfirmationBoxExitGame=false\n\
             infoBoxEnabledWelcome=false\n"
        );
        assert_eq!(
            quiet_settings(""),
            "[main_window]\nconfirmationBoxExitGame=false\ninfoBoxEnabledWelcome=false\n"
        );
        // The last section, with no newline at the end.
        assert_eq!(
            quiet_settings("[main_window]\ninfoBoxEnabledWelcome=true"),
            "[main_window]\ninfoBoxEnabledWelcome=false\nconfirmationBoxExitGame=false\n"
        );

        let dir = std::env::temp_dir().join(format!("lxb-rpcs3-quiet-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(write_quiet_settings(&dir));
        let written = std::fs::read_to_string(dir.join("GuiConfigs/CurrentSettings.ini")).unwrap();
        assert_eq!(written, quiet_settings(""));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_package_is_not_started_but_installed() {
        let mut package = game("TEKKEN 5 DR", &[]);
        package.form = Form::Package;
        package.boot = None;
        let Entry::Ps3Game(row) = game_row(&[], &package, None) else {
            panic!("a game row");
        };
        assert_eq!(row.start, None);
        assert_eq!(row.form, Form::Package);
    }
}
