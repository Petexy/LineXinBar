//! PlayStation 3 trophies, for the shell's Trophies column — read only, as
//! the Steam, Epic and RetroAchievements halves of that column are.
//!
//! ## Where they are
//!
//! Every PS3 game with trophies carries its **trophy set** in a `TROPHY.TRP`
//! under `TROPDIR/<set>` — the set is named by its NP communication ID,
//! `NPWR00482_00`. A TRP is a plain container: the trophies' definitions as
//! XML (`TROPCONF.SFM`), their names and descriptions in each language the
//! game has (`TROP_nn.SFM`, `nn` being the PS3's own language number, as in
//! `PARAM.SFO`'s `TITLE_nn`), and a picture per trophy.
//!
//! The first time RPCS3 starts a game, the game registers its set and RPCS3
//! unpacks it into `dev_hdd0/home/<user>/trophy/<set>/`, beside a
//! `TROPUSR.DAT` that records which trophies are unlocked and when. It does
//! not unpack every language: it keeps the names in *its own* system
//! language, renamed `TROPCONF.SFM` over the definitions (its
//! `sceNpTrophyRegisterContext`), and no `TROP.SFM` or `TROP_nn.SFM` at all.
//!
//! So a set is read out of the game itself wherever the game can be read —
//! its trophies, in the person's own language, and their pictures — and what
//! is unlocked, and when, from RPCS3's `TROPUSR.DAT` once the set is
//! registered. A set not registered yet is every trophy there is, none
//! unlocked, which is what a PS3 shows for a game never played too. A set
//! whose game has gone is read from RPCS3's copy alone, in RPCS3's language.
//!
//! Nothing is ever written to RPCS3's folders. A game's own pictures, read out
//! of a disc image, are kept in the shell's cache as [`crate::art`] keeps the
//! others.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::iso::Iso;
use crate::report::{Form, Game, PROTOCOL};
use crate::rpcs3::Console;
use crate::sfo::LANGUAGES;

/// One trophy set, as a line of `lxb-rpcs3 trophies` — or, with `done` set and
/// no set, the end of them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trophies {
    pub protocol: u32,
    /// `NPWR00482_00`.
    pub set: String,
    /// The games this set belongs to, by the ids `scan` gives them — two
    /// copies of one game share a set.
    pub games: Vec<String>,
    /// What the set calls itself, `TEKKEN 6 Trophy Set`.
    pub title: String,
    pub total: u32,
    pub unlocked: u32,
    /// Whether RPCS3 has started the game and registered the set. A set that
    /// has not been cannot have anything unlocked in it yet.
    pub registered: bool,
    pub list: Vec<Trophy>,
    pub done: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trophy {
    pub id: u32,
    pub name: String,
    pub detail: String,
    pub grade: Grade,
    /// Whether the game keeps it secret until it is earned. The shell shows a
    /// locked hidden trophy the way the PS3 did — as a question.
    pub hidden: bool,
    pub unlocked: bool,
    /// When, in seconds since 1970.
    pub unlocked_at: Option<u64>,
    /// Its picture on this disk.
    pub icon: Option<String>,
    /// The same picture grey, for while it is locked — see [`crate::grey`].
    /// Made only for a trophy that is locked.
    pub icon_locked: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Grade {
    Platinum,
    Gold,
    Silver,
    Bronze,
}

impl Grade {
    fn of(ttype: &str) -> Self {
        match ttype {
            "P" => Grade::Platinum,
            "G" => Grade::Gold,
            "S" => Grade::Silver,
            _ => Grade::Bronze,
        }
    }
}

/// Every set the games in `library` carry, in the person's `language` where
/// the game has it. `cache` is this helper's cache folder: the pictures of a
/// set read out of a disc image are kept under `trophies/<set>` in it —
/// beside the games' art rather than in it, because a scan sweeps that folder
/// of everything that is not a game.
pub fn all(
    games: &[Game],
    console: Option<&Console>,
    cache: Option<&Path>,
    language: Option<&str>,
) -> Vec<Trophies> {
    let mut by_set: BTreeMap<String, Vec<&Game>> = BTreeMap::new();
    for game in games {
        if let Some(set) = &game.trophies {
            by_set.entry(set.clone()).or_default().push(game);
        }
    }
    let mut out = Vec::new();
    for (set, games) in by_set {
        let registered = console
            .map(|console| console.trophies().join(&set))
            .filter(|dir| dir.join("TROPCONF.SFM").is_file());
        let own = games
            .iter()
            .find_map(|game| from_game(&set, game, console, cache, language));
        let found = match (own, registered) {
            (Some(own), Some(dir)) => Some(with_unlocks(own, &dir)),
            (None, Some(dir)) => from_registered(&set, &dir),
            (own, None) => own,
        };
        if let Some(mut trophies) = found {
            trophies.games = games.iter().map(|game| game.id.clone()).collect();
            if let Some(cache) = cache {
                grey_icons(&mut trophies, &cache.join("trophies").join(&set));
            }
            out.push(trophies);
        }
    }
    out
}

/// A grey copy of every locked trophy's picture, kept in `folder` beside the
/// set's others — made the first time it is needed, whichever cache the
/// picture itself came from.
fn grey_icons(trophies: &mut Trophies, folder: &Path) {
    let _ = std::fs::create_dir_all(folder);
    for trophy in &mut trophies.list {
        if trophy.unlocked {
            continue;
        }
        let Some(icon) = trophy.icon.as_deref() else {
            continue;
        };
        let name = format!("TROP{:03}.LOCKED.PNG", trophy.id);
        let kept = crate::art::kept(folder, &name).or_else(|| {
            let grey = crate::grey::grey_png(&std::fs::read(icon).ok()?)?;
            crate::art::keep(folder, &name, &grey)
        });
        trophy.icon_locked = kept.map(|at| at.to_string_lossy().into_owned());
    }
}

/// A set read out of its game, with what RPCS3 has recorded unlocked in it.
fn with_unlocks(mut trophies: Trophies, dir: &Path) -> Trophies {
    let unlocks = registered_unlocks(dir);
    for trophy in &mut trophies.list {
        let unlocked_at = unlocks.get(&trophy.id).copied();
        trophy.unlocked = unlocked_at.is_some();
        trophy.unlocked_at = unlocked_at.filter(|at| *at > 0);
        if trophy.icon.is_none() {
            trophy.icon = registered_icon(dir, trophy.id);
        }
    }
    trophies.unlocked = trophies
        .list
        .iter()
        .filter(|trophy| trophy.unlocked)
        .count() as u32;
    trophies.registered = true;
    trophies
}

/// A set RPCS3 has registered and whose game cannot be read: RPCS3's own
/// copy, whose `TROPCONF.SFM` is the names in RPCS3's language and the
/// definitions both.
fn from_registered(set: &str, dir: &Path) -> Option<Trophies> {
    let names = std::fs::read(dir.join("TROPCONF.SFM")).ok()?;
    let mut trophies = parse(set, &names, None, &registered_unlocks(dir))?;
    for trophy in &mut trophies.list {
        trophy.icon = registered_icon(dir, trophy.id);
    }
    trophies.registered = true;
    Some(trophies)
}

fn registered_unlocks(dir: &Path) -> BTreeMap<u32, u64> {
    std::fs::read(dir.join("TROPUSR.DAT"))
        .ok()
        .map(|bytes| unlocks(&bytes))
        .unwrap_or_default()
}

fn registered_icon(dir: &Path, id: u32) -> Option<String> {
    let icon = dir.join(format!("TROP{id:03}.PNG"));
    icon.is_file().then(|| icon.to_string_lossy().into_owned())
}

/// A set nobody has started yet, out of the game's own `TROPHY.TRP`.
fn from_game(
    set: &str,
    game: &Game,
    console: Option<&Console>,
    cache: Option<&Path>,
    language: Option<&str>,
) -> Option<Trophies> {
    let (trp, source) = match game.form {
        Form::Disc => {
            let iso = Iso::open(Path::new(&game.path)).ok()?;
            let bytes = iso.file(
                &format!("PS3_GAME/TROPDIR/{set}/TROPHY.TRP"),
                crate::art::LIMIT * 2,
                None,
            )?;
            (bytes, PathBuf::from(&game.path))
        }
        Form::Folder => {
            let base = Path::new(&game.path);
            let at = [base.join("PS3_GAME/TROPDIR"), base.join("TROPDIR")]
                .into_iter()
                .map(|dir| dir.join(set).join("TROPHY.TRP"))
                .find(|at| at.is_file())?;
            (std::fs::read(&at).ok()?, at)
        }
        Form::Installed => {
            let dir = game.id.strip_prefix("hdd0/")?;
            let at = console?
                .games()
                .join(dir)
                .join("TROPDIR")
                .join(set)
                .join("TROPHY.TRP");
            (std::fs::read(&at).ok()?, at)
        }
        Form::Package => return None,
    };
    let entries = trp_entries(&trp)?;
    let get = |name: &str| -> Option<Vec<u8>> {
        entries
            .iter()
            .find(|(entry, _)| entry.eq_ignore_ascii_case(name))
            .map(|(_, bytes)| bytes.to_vec())
    };
    let names = language_file(language, get)?;
    let config = get("TROPCONF.SFM");
    let mut trophies = parse(set, &names, config.as_deref(), &BTreeMap::new())?;

    // The pictures, kept once per set.
    if let Some(root) = cache {
        let folder = root.join("trophies").join(set);
        let stamp = crate::art::stamp(&source).unwrap_or_default();
        if !crate::art::fresh(&folder, &stamp) {
            crate::art::clear(&folder);
            for (name, bytes) in &entries {
                if name.to_uppercase().starts_with("TROP") && name.to_uppercase().ends_with(".PNG")
                {
                    crate::art::keep(&folder, &name.to_uppercase(), bytes);
                }
            }
            crate::art::seal(&folder, &stamp);
        }
        for trophy in &mut trophies.list {
            trophy.icon = crate::art::kept(&folder, &format!("TROP{:03}.PNG", trophy.id))
                .map(|at| at.to_string_lossy().into_owned());
        }
    }
    Some(trophies)
}

/// The names in the person's language where the set has them — `TROP_nn.SFM`
/// — and the set's own default, `TROP.SFM`, where it has not.
fn language_file(language: Option<&str>, get: impl Fn(&str) -> Option<Vec<u8>>) -> Option<Vec<u8>> {
    if let Some(language) = language {
        // The exact tag first (`pt-BR`), then any of the same language
        // (`pt-PT` for `pt`), the way the shell picks a game's title.
        let primary = language.split('-').next().unwrap_or(language);
        let numbers = LANGUAGES.iter().filter(|(_, tag)| *tag == language).chain(
            LANGUAGES
                .iter()
                .filter(|(_, tag)| *tag != language && tag.split('-').next() == Some(primary)),
        );
        for (number, _) in numbers {
            if let Some(bytes) = get(&format!("TROP_{number:02}.SFM")) {
                return Some(bytes);
            }
        }
    }
    get("TROP.SFM")
}

/// The set's definitions, names and unlocks together.
fn parse(
    set: &str,
    names: &[u8],
    config: Option<&[u8]>,
    unlocks: &BTreeMap<u32, u64>,
) -> Option<Trophies> {
    let names = std::str::from_utf8(names).ok()?;
    let document = roxmltree::Document::parse(strip_signature(names)).ok()?;
    let root = document.root_element();
    let title = root
        .children()
        .find(|node| node.has_tag_name("title-name"))
        .and_then(|node| node.text())
        .unwrap_or(set)
        .trim()
        .to_string();
    // What TROPCONF says of each trophy, where TROP.SFM leaves it out.
    let configured: BTreeMap<u32, (String, bool)> = config
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .and_then(|text| {
            let document = roxmltree::Document::parse(strip_signature(text)).ok()?;
            Some(
                document
                    .root_element()
                    .children()
                    .filter(|node| node.has_tag_name("trophy"))
                    .filter_map(|node| {
                        let id = node.attribute("id")?.parse().ok()?;
                        let grade = node.attribute("ttype").unwrap_or("B").to_string();
                        let hidden = node.attribute("hidden") == Some("yes");
                        Some((id, (grade, hidden)))
                    })
                    .collect(),
            )
        })
        .unwrap_or_default();
    let mut list = Vec::new();
    for node in root.children().filter(|node| node.has_tag_name("trophy")) {
        let Some(id) = node.attribute("id").and_then(|id| id.parse::<u32>().ok()) else {
            continue;
        };
        let text = |tag: &str| {
            node.children()
                .find(|child| child.has_tag_name(tag))
                .and_then(|child| child.text())
                .unwrap_or_default()
                .trim()
                .to_string()
        };
        let (grade, hidden) = configured.get(&id).cloned().unwrap_or_else(|| {
            (
                node.attribute("ttype").unwrap_or("B").to_string(),
                node.attribute("hidden") == Some("yes"),
            )
        });
        let unlocked_at = unlocks.get(&id).copied();
        list.push(Trophy {
            id,
            name: text("name"),
            detail: text("detail"),
            grade: Grade::of(&grade),
            hidden,
            unlocked: unlocked_at.is_some(),
            unlocked_at: unlocked_at.filter(|at| *at > 0),
            icon: None,
            icon_locked: None,
        });
    }
    list.sort_by_key(|trophy| trophy.id);
    Some(Trophies {
        protocol: PROTOCOL,
        set: set.to_string(),
        games: Vec::new(),
        title,
        total: list.len() as u32,
        unlocked: list.iter().filter(|trophy| trophy.unlocked).count() as u32,
        registered: false,
        list,
        done: false,
    })
}

/// Sony signs every SFM with a comment before the XML declaration would be,
/// which is not something every parser forgives. Taken off.
fn strip_signature(text: &str) -> &str {
    let text = text.trim_start_matches('\u{feff}').trim_start();
    if text.starts_with("<!--") {
        if let Some(end) = text.find("-->") {
            return text[end + 3..].trim_start();
        }
    }
    text
}

/// A TRP's files, by name. See RPCS3's `Loader/TRP.h`.
fn trp_entries(bytes: &[u8]) -> Option<Vec<(String, &[u8])>> {
    if bytes.get(..4)? != [0xdc, 0xa2, 0x4d, 0x00] {
        return None;
    }
    let count = u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?) as usize;
    let size = u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?) as usize;
    if size < 48 || count > 4096 {
        return None;
    }
    let mut out = Vec::new();
    for index in 0..count {
        let at = 0x40 + index * size;
        let entry = bytes.get(at..at + size)?;
        let name = entry[..32].split(|b| *b == 0).next().unwrap_or_default();
        let offset = u64::from_be_bytes(entry[32..40].try_into().ok()?) as usize;
        let length = u64::from_be_bytes(entry[40..48].try_into().ok()?) as usize;
        let Some(data) = bytes.get(offset..offset.checked_add(length)?) else {
            continue;
        };
        out.push((String::from_utf8_lossy(name).into_owned(), data));
    }
    Some(out)
}

/// The unlocked trophies in a `TROPUSR.DAT`, and when each was unlocked. See
/// RPCS3's `Loader/TROPUSR.h`: table 6 holds one entry per trophy, its state,
/// and the time as the PS3 keeps time — microseconds since the year 1. An
/// entry is a 16-byte header of its own and then the size the table gives
/// (0x60), so they are that plus 16 apart.
fn unlocks(bytes: &[u8]) -> BTreeMap<u32, u64> {
    let mut out = BTreeMap::new();
    let be32 = |at: usize| -> Option<u32> {
        Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
    };
    let be64 = |at: usize| -> Option<u64> {
        Some(u64::from_be_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
    };
    if be32(0) != Some(0x818f_54ad) {
        return out;
    }
    let tables = be32(8).unwrap_or(0).min(64) as usize;
    for table in 0..tables {
        let header = 0x30 + table * 32;
        let (Some(kind), Some(size), Some(count), Some(offset)) = (
            be32(header),
            be32(header + 4),
            be32(header + 12),
            be64(header + 16),
        ) else {
            break;
        };
        if kind != 6 || !(0x20..=0x1000).contains(&size) {
            continue;
        }
        let stride = size as usize + 16;
        for index in 0..count.min(4096) as usize {
            let entry = offset as usize + index * stride;
            let (Some(6), Some(id), Some(state), Some(when)) = (
                be32(entry),
                be32(entry + 16),
                be32(entry + 20),
                be64(entry + 40),
            ) else {
                break;
            };
            if state != 0 {
                out.insert(id, ps3_time_to_unix(when));
            }
        }
    }
    out
}

/// Microseconds since 0001-01-01 to seconds since 1970-01-01; zero for a
/// time before 1970, which is a trophy RPCS3 unlocked without saying when.
fn ps3_time_to_unix(ticks: u64) -> u64 {
    const EPOCH: u64 = 62_135_596_800;
    (ticks / 1_000_000).saturating_sub(EPOCH)
}

/// The sets in the order they are written, then the line that says there are
/// no more.
pub fn end() -> Trophies {
    Trophies {
        protocol: PROTOCOL,
        set: String::new(),
        games: Vec::new(),
        title: String::new(),
        total: 0,
        unlocked: 0,
        registered: false,
        list: Vec::new(),
        done: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAMES: &str = r#"<!--Sce-Np-Trophy-Signature: 4c39-->
<trophyconf version="1.0">
 <npcommid>NPWR00482_00</npcommid>
 <title-name>TEKKEN 6 Trophy Set</title-name>
 <trophy id="000" hidden="yes" ttype="P" pid="-1">
  <name>Tekken Fanatic</name>
  <detail>Complete all other objectives.</detail>
 </trophy>
 <trophy id="001" hidden="no" ttype="B" pid="000">
  <name>Friend or Foe?</name>
  <detail>Reunite with your ally.</detail>
 </trophy>
</trophyconf>"#;

    #[test]
    fn a_set_is_read_with_its_grades_and_secrets() {
        let mut unlocks = BTreeMap::new();
        unlocks.insert(1, 1_700_000_000);
        let set = parse("NPWR00482_00", NAMES.as_bytes(), None, &unlocks).expect("a set");
        assert_eq!(set.title, "TEKKEN 6 Trophy Set");
        assert_eq!(set.total, 2);
        assert_eq!(set.unlocked, 1);
        assert_eq!(set.list[0].grade, Grade::Platinum);
        assert!(set.list[0].hidden);
        assert!(!set.list[0].unlocked);
        assert_eq!(set.list[1].name, "Friend or Foe?");
        assert_eq!(set.list[1].unlocked_at, Some(1_700_000_000));
    }

    #[test]
    fn the_persons_language_is_taken_where_the_set_has_it() {
        let files = |name: &str| -> Option<Vec<u8>> {
            match name {
                "TROP.SFM" => Some(b"default".to_vec()),
                "TROP_16.SFM" => Some(b"polish".to_vec()),
                "TROP_07.SFM" => Some(b"portuguese".to_vec()),
                _ => None,
            }
        };
        assert_eq!(language_file(Some("pl"), files), Some(b"polish".to_vec()));
        assert_eq!(
            language_file(Some("pt-BR"), files),
            Some(b"portuguese".to_vec())
        );
        assert_eq!(language_file(Some("de"), files), Some(b"default".to_vec()));
        assert_eq!(language_file(None, files), Some(b"default".to_vec()));
    }

    /// TROPUSR.DAT as RPCS3 writes it: a header, a table header, and table 6
    /// with a state and a time per trophy, each entry 0x70 bytes from the
    /// last — as in the file RPCS3 wrote for Super Stardust HD.
    #[test]
    fn unlocks_are_read_with_their_times() {
        let mut bytes = vec![0u8; 0x30 + 32];
        bytes[0..4].copy_from_slice(&0x818f_54adu32.to_be_bytes());
        bytes[8..12].copy_from_slice(&1u32.to_be_bytes());
        let table = 0x30;
        let offset = bytes.len() as u64;
        bytes[table..table + 4].copy_from_slice(&6u32.to_be_bytes());
        bytes[table + 4..table + 8].copy_from_slice(&0x60u32.to_be_bytes());
        bytes[table + 12..table + 16].copy_from_slice(&3u32.to_be_bytes());
        bytes[table + 16..table + 24].copy_from_slice(&offset.to_be_bytes());
        // 2023-11-14 22:13:20 UTC, in the PS3's microseconds since the year 1.
        let when = (1_700_000_000u64 + 62_135_596_800) * 1_000_000;
        for (id, state) in [(0u32, 0u32), (1, 1), (2, 1)] {
            // The entry's own header, then its 0x60 bytes.
            let mut entry = vec![0u8; 0x70];
            entry[0..4].copy_from_slice(&6u32.to_be_bytes());
            entry[4..8].copy_from_slice(&0x60u32.to_be_bytes());
            entry[16..20].copy_from_slice(&id.to_be_bytes());
            entry[20..24].copy_from_slice(&state.to_be_bytes());
            if state == 1 {
                entry[40..48].copy_from_slice(&when.to_be_bytes());
            }
            bytes.extend_from_slice(&entry);
        }
        let read = unlocks(&bytes);
        assert_eq!(read.len(), 2);
        assert_eq!(read.get(&1), Some(&1_700_000_000));
        assert_eq!(read.get(&2), Some(&1_700_000_000));
    }

    /// A TRP holding `files`.
    fn trp(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut bytes = vec![0u8; 0x40 + files.len() * 64];
        bytes[0..4].copy_from_slice(&[0xdc, 0xa2, 0x4d, 0x00]);
        bytes[16..20].copy_from_slice(&(files.len() as u32).to_be_bytes());
        bytes[20..24].copy_from_slice(&64u32.to_be_bytes());
        for (index, (name, data)) in files.iter().enumerate() {
            let offset = bytes.len() as u64;
            bytes.extend_from_slice(data);
            let at = 0x40 + index * 64;
            bytes[at..at + name.len()].copy_from_slice(name.as_bytes());
            bytes[at + 32..at + 40].copy_from_slice(&offset.to_be_bytes());
            bytes[at + 40..at + 48].copy_from_slice(&(data.len() as u64).to_be_bytes());
        }
        bytes
    }

    /// A TROPUSR.DAT with `count` trophies, `unlocked` of them unlocked.
    fn tropusr(count: u32, unlocked: &[u32]) -> Vec<u8> {
        let mut bytes = vec![0u8; 0x30 + 32];
        bytes[0..4].copy_from_slice(&0x818f_54adu32.to_be_bytes());
        bytes[8..12].copy_from_slice(&1u32.to_be_bytes());
        let offset = bytes.len() as u64;
        bytes[0x30..0x34].copy_from_slice(&6u32.to_be_bytes());
        bytes[0x34..0x38].copy_from_slice(&0x60u32.to_be_bytes());
        bytes[0x3c..0x40].copy_from_slice(&count.to_be_bytes());
        bytes[0x40..0x48].copy_from_slice(&offset.to_be_bytes());
        let when = (1_700_000_000u64 + 62_135_596_800) * 1_000_000;
        for id in 0..count {
            let mut entry = vec![0u8; 0x70];
            entry[0..4].copy_from_slice(&6u32.to_be_bytes());
            entry[4..8].copy_from_slice(&0x60u32.to_be_bytes());
            entry[16..20].copy_from_slice(&id.to_be_bytes());
            if unlocked.contains(&id) {
                entry[20..24].copy_from_slice(&1u32.to_be_bytes());
                entry[40..48].copy_from_slice(&when.to_be_bytes());
            }
            bytes.extend_from_slice(&entry);
        }
        bytes
    }

    /// A set RPCS3 has registered is laid out the way RPCS3 lays it out — one
    /// `TROPCONF.SFM` in RPCS3's language, no `TROP.SFM` — and is still read:
    /// in the person's language out of the game, with RPCS3's unlocks; and
    /// out of RPCS3's copy alone where the game has gone.
    #[test]
    fn a_registered_set_keeps_its_trophies_and_gains_its_unlocks() {
        let dir = tempfile::tempdir().unwrap();
        let console = Console::of(dir.path());
        let polish = NAMES.replace("Friend or Foe?", "Przyjaciel czy wróg?");
        let game_dir = dir.path().join("games/TEKKEN 6");
        let tropdir = game_dir.join("PS3_GAME/TROPDIR/NPWR00482_00");
        std::fs::create_dir_all(&tropdir).unwrap();
        std::fs::write(
            tropdir.join("TROPHY.TRP"),
            trp(&[
                ("TROPCONF.SFM", NAMES.as_bytes()),
                ("TROP.SFM", NAMES.as_bytes()),
                ("TROP_16.SFM", polish.as_bytes()),
            ]),
        )
        .unwrap();
        let registered = console.trophies().join("NPWR00482_00");
        std::fs::create_dir_all(&registered).unwrap();
        std::fs::write(registered.join("TROPCONF.SFM"), NAMES).unwrap();
        std::fs::write(registered.join("TROPUSR.DAT"), tropusr(2, &[1])).unwrap();
        std::fs::write(registered.join("TROP001.PNG"), b"\x89PNG").unwrap();
        let game = Game {
            id: game_dir.to_string_lossy().into_owned(),
            serial: Some("BLUS30359".to_string()),
            title: "TEKKEN 6".to_string(),
            titles: BTreeMap::new(),
            form: Form::Folder,
            path: game_dir.to_string_lossy().into_owned(),
            boot: None,
            version: None,
            category: Some("DG".to_string()),
            icon: None,
            backdrop: None,
            overlay: None,
            preview: None,
            music: None,
            key: crate::report::Key::Unneeded,
            trophies: Some("NPWR00482_00".to_string()),
            size: 0,
            cache: 0,
            updates: Vec::new(),
            licence: None,
        };

        let sets = all(
            std::slice::from_ref(&game),
            Some(&console),
            None,
            Some("pl"),
        );
        assert_eq!(sets.len(), 1);
        let set = &sets[0];
        assert!(set.registered);
        assert_eq!((set.total, set.unlocked), (2, 1));
        assert_eq!(set.list[1].name, "Przyjaciel czy wróg?");
        assert!(set.list[1].unlocked);
        assert_eq!(set.list[1].unlocked_at, Some(1_700_000_000));
        assert!(set.list[1]
            .icon
            .as_deref()
            .is_some_and(|at| at.ends_with("TROP001.PNG")));

        std::fs::remove_dir_all(&game_dir).unwrap();
        let sets = all(&[game], Some(&console), None, Some("pl"));
        assert_eq!(sets.len(), 1, "a registered set outlives its game");
        assert_eq!(sets[0].list[1].name, "Friend or Foe?");
        assert_eq!(sets[0].unlocked, 1);
    }

    /// A locked trophy gets a grey copy of its picture in the cache, made once;
    /// an unlocked one is left in colour.
    #[test]
    fn a_locked_trophy_has_a_grey_picture() {
        let dir = tempfile::tempdir().unwrap();
        let picture = dir.path().join("TROP000.PNG");
        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, 1, 1);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&[200, 40, 40]).unwrap();
            writer.finish().unwrap();
        }
        std::fs::write(&picture, &png).unwrap();
        let mut set = parse(
            "NPWR00482_00",
            NAMES.as_bytes(),
            None,
            &BTreeMap::from([(1, 0)]),
        )
        .expect("a set");
        for trophy in &mut set.list {
            trophy.icon = Some(picture.to_string_lossy().into_owned());
        }
        let cache = dir.path().join("cache/trophies/NPWR00482_00");
        grey_icons(&mut set, &cache);
        let grey = set.list[0].icon_locked.clone().expect("locked, so grey");
        assert!(grey.ends_with("TROP000.LOCKED.PNG"));
        assert_ne!(std::fs::read(&grey).unwrap(), png);
        assert_eq!(set.list[1].icon_locked, None, "unlocked, so in colour");
        // The second time, the copy kept the first time.
        std::fs::remove_file(&picture).unwrap();
        grey_icons(&mut set, &cache);
        assert_eq!(set.list[0].icon_locked.as_deref(), Some(grey.as_str()));
    }

    #[test]
    fn a_trp_is_opened_as_its_files() {
        let mut bytes = vec![0u8; 0x40 + 2 * 64];
        bytes[0..4].copy_from_slice(&[0xdc, 0xa2, 0x4d, 0x00]);
        bytes[16..20].copy_from_slice(&2u32.to_be_bytes());
        bytes[20..24].copy_from_slice(&64u32.to_be_bytes());
        let files: [(&str, &[u8]); 2] =
            [("TROP.SFM", NAMES.as_bytes()), ("TROP000.PNG", b"\x89PNG")];
        for (index, (name, data)) in files.iter().enumerate() {
            let offset = bytes.len() as u64;
            bytes.extend_from_slice(data);
            let at = 0x40 + index * 64;
            bytes[at..at + name.len()].copy_from_slice(name.as_bytes());
            bytes[at + 32..at + 40].copy_from_slice(&offset.to_be_bytes());
            bytes[at + 40..at + 48].copy_from_slice(&(data.len() as u64).to_be_bytes());
        }
        let entries = trp_entries(&bytes).expect("a TRP");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].0, "TROP000.PNG");
        assert_eq!(entries[1].1, b"\x89PNG");
    }
}
