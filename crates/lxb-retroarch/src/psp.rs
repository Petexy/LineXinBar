//! A PlayStation Portable game's own pictures, film and music.
//!
//! Every PSP game carries the things the handheld's own menu dressed it in,
//! beside `PARAM.SFO` under `PSP_GAME/` on the disc:
//!
//! ```text
//!   ICON0.PNG   144 × 80   the icon the game is listed with
//!   ICON1.PMF              a short film that plays in place of the icon
//!                          while the game is highlighted
//!   SND0.AT3               the music that plays behind it
//!   PIC1.PNG    480 × 272  the backdrop the screen turns to
//!   PIC0.PNG    310 × 180  a picture that stands over the backdrop
//! ```
//!
//! The user asked for exactly those, used the way the PSP used them and the
//! way the PlayStation 3 column uses its own — rather than a cover and a
//! screenshot fetched from libretro, which stay for a game these cannot be
//! read out of. See [`crate::art`] for those.
//!
//! ## Where they are
//!
//! * **An `.iso`** is the UMD as it was: ISO 9660 on 2048-byte sectors, and
//!   the files are under `PSP_GAME/`.
//! * **A `.cso`** is the same image in compressed blocks — `CISO`, a header,
//!   an index of where each block starts, and each block raw deflate or, where
//!   deflating it saved nothing, as it was. Version 2 (maxcso's) may also keep a
//!   block in LZ4, which nothing in this tree decodes; a game with one of those
//!   in the way is a game without its own pictures, and keeps libretro's.
//! * **A `.pbp`** — a game bought from the PlayStation Store, or homebrew — is
//!   a header naming where eight files start, the five here among them.
//!
//! A `.chd` is not read. Its hunks are LZMA, Zstandard or FLAC as often as
//! deflate, and none of those is in this tree; such a game keeps libretro's
//! pictures, as every game did before this.
//!
//! ## What is on this disk afterwards
//!
//! ```text
//! $XDG_CACHE_HOME/lxb/retroarch-art/.own/<game>/
//!     ICON0.PNG  ICON1.PMF  SND0.AT3  PIC1.PNG  PIC0.PNG   whichever it has
//!     source                                               what they came from
//! ```
//!
//! Copied out once, under the name the game gives each, beside a stamp of the
//! file they came from; a scan that finds the stamp unchanged reads nothing
//! again. Under the art cache so that taking RetroArch away takes them too —
//! see `install::remove` — and in a dotted folder so no libretro system name
//! can ever be it. A game no longer in the folder has its copies taken away
//! after the next scan that could read the folder.

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use crate::consoles::Machine;

/// The icon, the card a game is drawn on.
const ICON: &str = "ICON0.PNG";
/// The film that plays in place of the icon.
const FILM: &str = "ICON1.PMF";
/// The picture over the backdrop.
const OVERLAY: &str = "PIC0.PNG";
/// The backdrop.
const BACKDROP: &str = "PIC1.PNG";
/// The music.
const MUSIC: &str = "SND0.AT3";

/// The most any one of them may be. The film is the largest and a real one is
/// a few hundred kilobytes; anything past this is not an icon.
const LIMIT: u64 = 32 * 1024 * 1024;

/// A UMD's sector.
const SECTOR: u64 = 2048;

/// What one game carries of its own, as files this helper kept.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Own {
    pub icon: Option<PathBuf>,
    pub preview: Option<PathBuf>,
    pub music: Option<PathBuf>,
    pub backdrop: Option<PathBuf>,
    pub overlay: Option<PathBuf>,
}

/// Whether this console's games carry their own: the PlayStation Portable's.
pub fn carries_its_own(machine: Option<&Machine>) -> bool {
    machine.is_some_and(|machine| machine.aliases.contains(&"psp"))
}

/// Where the copies are kept: `.own` under the art cache.
pub fn root() -> Option<PathBuf> {
    crate::art::cache().map(|cache| cache.join(".own"))
}

/// The folder one game's copies live in, named after the game's path.
pub fn folder(root: &Path, rom: &Path) -> PathBuf {
    root.join(format!("{:016x}", fnv(rom.as_os_str().as_encoded_bytes())))
}

/// This game's own pictures, film and music — read out of it the first time,
/// and from the copies every time after.
pub fn of(root: &Path, rom: &Path) -> Own {
    if !readable(rom) {
        return Own::default();
    }
    let folder = folder(root, rom);
    let Some(stamp) = stamp(rom) else {
        return Own::default();
    };
    if !fresh(&folder, &stamp) {
        match read_out(rom) {
            Ok(found) => {
                clear(&folder);
                let names: Vec<&str> = found.iter().map(|(name, _)| *name).collect();
                for (name, bytes) in &found {
                    keep(&folder, name, bytes);
                }
                eprintln!("psp: {} carries {names:?}", rom.display());
                // Sealed even with nothing found, so a game with no pictures
                // of its own is not read again every time the shell starts.
                seal(&folder, &stamp);
            }
            // Not sealed: a drive that failed a read this morning is asked
            // again next time rather than remembered as a game with nothing.
            Err(err) => {
                eprintln!("psp: {} could not be read: {err}", rom.display());
                return Own::default();
            }
        }
    }
    Own {
        icon: kept(&folder, ICON),
        preview: kept(&folder, FILM),
        music: kept(&folder, MUSIC),
        backdrop: kept(&folder, BACKDROP),
        overlay: kept(&folder, OVERLAY),
    }
}

/// Whether this is a kind of file the pictures can be read out of.
fn readable(rom: &Path) -> bool {
    rom.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ["iso", "cso", "pbp"]
                .iter()
                .any(|kind| extension.eq_ignore_ascii_case(kind))
        })
}

/// Every one of the five this game has, by its name. What the file *is* is
/// read off its first bytes rather than its extension, so a compressed image
/// somebody named `.iso` is still read.
fn read_out(rom: &Path) -> io::Result<Vec<(&'static str, Vec<u8>)>> {
    let file = File::open(rom)?;
    let mut magic = [0u8; 4];
    if file.read_exact_at(&mut magic, 0).is_err() {
        return Ok(Vec::new());
    }
    let found = match &magic {
        b"\0PBP" => from_pbp(&file)?,
        b"CISO" => match Cso::open(file)? {
            Some(mut image) => from_iso(&mut image)?,
            None => Vec::new(),
        },
        _ => from_iso(&mut Plain(file))?,
    };
    Ok(found
        .into_iter()
        .filter(|(name, bytes)| is_what_it_says(name, bytes))
        .collect())
}

/// Whether a file starts the way one of its kind does. A game with a damaged
/// or encrypted copy of one of these is better without it than wearing noise.
fn is_what_it_says(name: &str, bytes: &[u8]) -> bool {
    match name {
        ICON | OVERLAY | BACKDROP => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        FILM => bytes.starts_with(b"PSMF"),
        MUSIC => bytes.starts_with(b"RIFF"),
        _ => false,
    }
}

// --- a PBP ------------------------------------------------------------------

/// The five out of a PBP's header: `\0PBP`, a version, then where each of
/// eight files begins — `PARAM.SFO`, `ICON0.PNG`, `ICON1.PMF`, `PIC0.PNG`,
/// `PIC1.PNG`, `SND0.AT3`, `DATA.PSP`, `DATA.PSAR` — each ending where the next
/// begins. An empty one is one the game has not got.
fn from_pbp(file: &File) -> io::Result<Vec<(&'static str, Vec<u8>)>> {
    let mut head = [0u8; 40];
    file.read_exact_at(&mut head, 0)?;
    let length = file.metadata()?.len();
    let starts: Vec<u64> = head[8..40]
        .chunks_exact(4)
        .map(|at| u64::from(u32::from_le_bytes([at[0], at[1], at[2], at[3]])))
        .collect();
    let mut found = Vec::new();
    for (index, name) in [
        (1, ICON),
        (2, FILM),
        (3, OVERLAY),
        (4, BACKDROP),
        (5, MUSIC),
    ] {
        let (from, to) = (starts[index], starts[index + 1]);
        if to <= from || to > length || to - from > LIMIT {
            continue;
        }
        let mut bytes = vec![0u8; (to - from) as usize];
        file.read_exact_at(&mut bytes, from)?;
        found.push((name, bytes));
    }
    Ok(found)
}

// --- an image of a UMD ------------------------------------------------------

/// A UMD's bytes, however they are kept.
trait Image {
    /// `into.len()` bytes of the disc, from `offset`.
    fn read_at(&mut self, offset: u64, into: &mut [u8]) -> io::Result<()>;
}

/// An `.iso`: the disc, byte for byte.
struct Plain(File);

impl Image for Plain {
    fn read_at(&mut self, offset: u64, into: &mut [u8]) -> io::Result<()> {
        self.0.read_exact_at(into, offset)
    }
}

/// A `.cso`: the disc in blocks, each deflated or kept as it was.
struct Cso {
    file: File,
    /// How long the disc is, unpacked.
    total: u64,
    /// How long a block is, unpacked.
    block: u64,
    /// How far each place in the index is shifted to give a byte offset.
    align: u32,
    /// maxcso's second version, where the index's high bit means LZ4.
    second: bool,
    /// The block read last, which is nearly always the one asked for next.
    last: Option<(u64, Vec<u8>)>,
}

impl Cso {
    /// Read the header, or `None` for one that is not a CSO this can read.
    fn open(file: File) -> io::Result<Option<Cso>> {
        let mut head = [0u8; 24];
        file.read_exact_at(&mut head, 0)?;
        let total = u64::from_le_bytes(head[8..16].try_into().unwrap_or_default());
        let block = u64::from(u32::from_le_bytes(
            head[16..20].try_into().unwrap_or_default(),
        ));
        let version = head[20];
        let align = u32::from(head[21]);
        // A block is a power of two no smaller than a sector; anything else is
        // a header this does not understand, not a disc to guess at.
        if !block.is_power_of_two() || !(SECTOR..=1 << 24).contains(&block) || version > 2 {
            return Ok(None);
        }
        Ok(Some(Cso {
            file,
            total,
            block,
            align: align.min(31),
            second: version == 2,
            last: None,
        }))
    }

    /// Where block `index` is stored, and whether its index entry's high bit
    /// was set.
    fn entry(&self, index: u64) -> io::Result<(u64, bool)> {
        let mut raw = [0u8; 4];
        self.file.read_exact_at(&mut raw, 24 + index * 4)?;
        let raw = u32::from_le_bytes(raw);
        Ok((
            u64::from(raw & 0x7fff_ffff) << self.align,
            raw & 0x8000_0000 != 0,
        ))
    }

    /// One block, unpacked.
    fn block(&mut self, index: u64) -> io::Result<Vec<u8>> {
        if let Some((held, bytes)) = &self.last {
            if *held == index {
                return Ok(bytes.clone());
            }
        }
        if index * self.block >= self.total {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "past the end of the disc",
            ));
        }
        let (from, high) = self.entry(index)?;
        let (to, _) = self.entry(index + 1)?;
        let stored = to.saturating_sub(from);
        if stored > self.block * 2 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "a block larger than it can be",
            ));
        }
        let mut raw = vec![0u8; stored as usize];
        self.file.read_exact_at(&mut raw, from)?;
        // Version 1 says "kept as it was" with the high bit. Version 2 says it
        // by the block taking as much room as it would unpacked, and uses the
        // high bit for LZ4.
        let plain = match self.second {
            false => high,
            true => stored >= self.block,
        };
        let mut bytes = if plain {
            raw.truncate(self.block as usize);
            raw
        } else if self.second && high {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "a block kept in LZ4",
            ));
        } else {
            let mut out = Vec::with_capacity(self.block as usize);
            flate2::read::DeflateDecoder::new(raw.as_slice())
                .take(self.block)
                .read_to_end(&mut out)?;
            out
        };
        bytes.resize(self.block as usize, 0);
        self.last = Some((index, bytes.clone()));
        Ok(bytes)
    }
}

impl Image for Cso {
    fn read_at(&mut self, offset: u64, into: &mut [u8]) -> io::Result<()> {
        let mut done = 0usize;
        while done < into.len() {
            let at = offset + done as u64;
            let block = self.block(at / self.block)?;
            let within = (at % self.block) as usize;
            let take = (block.len() - within).min(into.len() - done);
            into[done..done + take].copy_from_slice(&block[within..within + take]);
            done += take;
        }
        Ok(())
    }
}

/// One directory record: where its file is, how long, and whether it is a
/// directory.
#[derive(Debug, Clone, Copy)]
struct Record {
    lba: u64,
    size: u64,
    directory: bool,
}

/// The five out of `PSP_GAME/` on an ISO 9660 image. An image that is not ISO
/// 9660, or has no `PSP_GAME`, has none of them — which is an answer, not a
/// failure: a UMD of a film is one of those.
fn from_iso(image: &mut dyn Image) -> io::Result<Vec<(&'static str, Vec<u8>)>> {
    let mut volume = vec![0u8; SECTOR as usize];
    image.read_at(16 * SECTOR, &mut volume)?;
    if volume[0] != 1 || &volume[1..6] != b"CD001" {
        return Ok(Vec::new());
    }
    let Some(root) = record(&volume[156..190]) else {
        return Ok(Vec::new());
    };
    let Some(game) = lookup(image, root, "PSP_GAME")?.filter(|found| found.directory) else {
        return Ok(Vec::new());
    };
    let mut found = Vec::new();
    for name in [ICON, FILM, OVERLAY, BACKDROP, MUSIC] {
        let Some(file) = lookup(image, game, name)? else {
            continue;
        };
        if file.directory || file.size == 0 || file.size > LIMIT {
            continue;
        }
        let mut bytes = vec![0u8; file.size as usize];
        image.read_at(file.lba * SECTOR, &mut bytes)?;
        found.push((name, bytes));
    }
    Ok(found)
}

/// A directory record's extent and kind, or `None` for one too short to be
/// one.
fn record(bytes: &[u8]) -> Option<Record> {
    Some(Record {
        lba: u64::from(u32::from_le_bytes(bytes.get(2..6)?.try_into().ok()?)),
        size: u64::from(u32::from_le_bytes(bytes.get(10..14)?.try_into().ok()?)),
        directory: bytes.get(25)? & 0x02 != 0,
    })
}

/// A name in a directory, ignoring case and the `;1` a file's name ends in.
fn lookup(image: &mut dyn Image, directory: Record, name: &str) -> io::Result<Option<Record>> {
    // A directory on a game's disc is a sector or two; the bound is for a
    // damaged record claiming to be a gigabyte.
    let sectors = directory.size.div_ceil(SECTOR).min(64);
    let mut sector = vec![0u8; SECTOR as usize];
    for at in 0..sectors {
        image.read_at((directory.lba + at) * SECTOR, &mut sector)?;
        let mut offset = 0;
        while offset < sector.len() {
            let length = usize::from(sector[offset]);
            // Records never cross a sector; the rest of this one is padding.
            if length == 0 {
                break;
            }
            let Some(entry) = sector.get(offset..offset + length) else {
                break;
            };
            let named = usize::from(entry.get(32).copied().unwrap_or(0));
            if let Some(found) = entry.get(33..33 + named) {
                let found = String::from_utf8_lossy(found);
                let found = found.split(';').next().unwrap_or_default();
                let found = found.strip_suffix('.').unwrap_or(found);
                if found.eq_ignore_ascii_case(name) {
                    return Ok(record(entry));
                }
            }
            offset += length;
        }
    }
    Ok(None)
}

// --- the copies -------------------------------------------------------------

/// What the copies were made from: the file's path, size and when it last
/// changed. A scan that finds the same stamp reads nothing.
fn stamp(source: &Path) -> Option<String> {
    let meta = std::fs::metadata(source).ok()?;
    let modified = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some(format!(
        "{} {} {}\n",
        source.display(),
        meta.len(),
        modified
    ))
}

/// Whether `folder` already holds the copies made from a source with this
/// stamp.
fn fresh(folder: &Path, stamp: &str) -> bool {
    std::fs::read_to_string(folder.join("source")).is_ok_and(|kept| kept == stamp)
}

/// Mark `folder` as holding the copies made from this stamp. Written last, so
/// a scan stopped halfway is done again rather than trusted.
fn seal(folder: &Path, stamp: &str) {
    if let Err(err) = std::fs::write(folder.join("source"), stamp) {
        eprintln!("psp: {} could not be sealed: {err}", folder.display());
    }
}

/// Start `folder` over, for a source that has changed.
fn clear(folder: &Path) {
    let _ = std::fs::remove_dir_all(folder);
    if let Err(err) = std::fs::create_dir_all(folder) {
        eprintln!("psp: {} could not be made: {err}", folder.display());
    }
}

/// Keep one file. Written beside and renamed over, so the shell never opens a
/// half-written picture.
fn keep(folder: &Path, name: &str, bytes: &[u8]) {
    let at = folder.join(name);
    let partial = folder.join(format!(".{name}.partial"));
    if let Err(err) = std::fs::write(&partial, bytes).and_then(|()| std::fs::rename(&partial, &at))
    {
        eprintln!("psp: {} could not be written: {err}", at.display());
    }
}

/// A kept file, where the folder has it.
fn kept(folder: &Path, name: &str) -> Option<PathBuf> {
    let at = folder.join(name);
    at.is_file().then_some(at)
}

/// Take away every game's copies but these.
///
/// Only called after a scan that could read the folder it was asked to, so a
/// drive that is not plugged in this morning does not cost its games their
/// pictures.
pub fn sweep(root: &Path, keep: &HashSet<PathBuf>) {
    let Ok(listing) = std::fs::read_dir(root) else {
        return;
    };
    for entry in listing.filter_map(Result::ok) {
        let path = entry.path();
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) && !keep.contains(&path) {
            match std::fs::remove_dir_all(&path) {
                Ok(()) => eprintln!("psp: removed {}", path.display()),
                Err(err) => eprintln!("psp: {} could not be removed: {err}", path.display()),
            }
        }
    }
}

/// FNV-1a, 64-bit: a name for a folder that is the same every run, which the
/// standard library's hasher does not promise.
fn fnv(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
pub mod fixture {
    //! Images of a PSP game, built the way a mastering tool lays one out, for
    //! this module's tests and the scan's.

    use super::SECTOR;

    pub const PNG: &[u8] = b"\x89PNG\r\n\x1a\nicon";
    pub const BACKDROP: &[u8] = b"\x89PNG\r\n\x1a\nbackdrop";
    pub const FILM: &[u8] = b"PSMF0014film";
    pub const MUSIC: &[u8] = b"RIFF....WAVEmusic";

    fn directory_record(name: &str, lba: u32, size: u32, directory: bool) -> Vec<u8> {
        let mut record = vec![0u8; 33];
        record[2..6].copy_from_slice(&lba.to_le_bytes());
        record[10..14].copy_from_slice(&size.to_le_bytes());
        record[25] = if directory { 2 } else { 0 };
        record[32] = name.len() as u8;
        record.extend_from_slice(name.as_bytes());
        if record.len() % 2 == 1 {
            record.push(0);
        }
        record[0] = record.len() as u8;
        record
    }

    /// An ISO 9660 image holding `PSP_GAME/` and these files in it. A file's
    /// name may carry the `;1` a real one does.
    pub fn iso(files: &[(&str, &[u8])]) -> Vec<u8> {
        let sector = SECTOR as usize;
        // 16 system sectors, the volume, the terminator, the root, PSP_GAME,
        // then a sector or more per file.
        let mut image = vec![0u8; 20 * sector];
        let mut next = 20u32;
        let mut listing = Vec::new();
        for (name, bytes) in files {
            listing.extend(directory_record(name, next, bytes.len() as u32, false));
            let at = next as usize * sector;
            let sectors = bytes.len().div_ceil(sector).max(1);
            image.resize(at + sectors * sector, 0);
            image[at..at + bytes.len()].copy_from_slice(bytes);
            next += sectors as u32;
        }
        let game_at = 19 * sector;
        image[game_at..game_at + listing.len()].copy_from_slice(&listing);
        let root = directory_record("PSP_GAME", 19, SECTOR as u32, true);
        let root_at = 18 * sector;
        image[root_at..root_at + root.len()].copy_from_slice(&root);
        let volume = 16 * sector;
        image[volume] = 1;
        image[volume + 1..volume + 6].copy_from_slice(b"CD001");
        let own = directory_record("\0", 18, SECTOR as u32, true);
        image[volume + 156..volume + 156 + own.len()].copy_from_slice(&own);
        image[17 * sector] = 255;
        image[17 * sector + 1..17 * sector + 6].copy_from_slice(b"CD001");
        image
    }

    /// The same disc as a version 1 CSO: every other block deflated, the rest
    /// kept as they were, the way a real one mixes the two.
    pub fn cso(iso: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let block = SECTOR as usize;
        let blocks = iso.len().div_ceil(block);
        let mut head = Vec::new();
        head.extend_from_slice(b"CISO");
        head.extend_from_slice(&24u32.to_le_bytes());
        head.extend_from_slice(&(iso.len() as u64).to_le_bytes());
        head.extend_from_slice(&(block as u32).to_le_bytes());
        head.push(1);
        head.push(0);
        head.extend_from_slice(&[0, 0]);
        let mut data = Vec::new();
        let mut index = Vec::new();
        let start = 24 + (blocks + 1) * 4;
        for (at, chunk) in iso.chunks(block).enumerate() {
            let offset = (start + data.len()) as u32;
            if at % 2 == 0 {
                index.push(offset | 0x8000_0000);
                data.extend_from_slice(chunk);
            } else {
                index.push(offset);
                let mut deflate =
                    flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
                deflate.write_all(chunk).unwrap();
                data.extend(deflate.finish().unwrap());
            }
        }
        index.push((start + data.len()) as u32);
        let mut out = head;
        for entry in index {
            out.extend_from_slice(&entry.to_le_bytes());
        }
        out.extend(data);
        out
    }

    /// A PBP holding these five, in its order, with `None` for one it has
    /// not got.
    pub fn pbp(icon: Option<&[u8]>, film: Option<&[u8]>, backdrop: Option<&[u8]>) -> Vec<u8> {
        let sfo: &[u8] = b"\0PSF";
        let parts: [&[u8]; 8] = [
            sfo,
            icon.unwrap_or_default(),
            film.unwrap_or_default(),
            b"",
            backdrop.unwrap_or_default(),
            b"",
            b"DATA.PSP",
            b"DATA.PSAR",
        ];
        let mut out = b"\0PBP".to_vec();
        out.extend_from_slice(&0x0001_0000u32.to_le_bytes());
        let mut at = 40u32;
        for part in parts {
            out.extend_from_slice(&at.to_le_bytes());
            at += part.len() as u32;
        }
        for part in parts {
            out.extend_from_slice(part);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let at = std::env::temp_dir().join(format!("lxb-psp-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&at);
        std::fs::create_dir_all(&at).unwrap();
        at
    }

    fn the_four() -> Vec<(&'static str, &'static [u8])> {
        vec![
            ("ICON0.PNG;1", fixture::PNG),
            ("ICON1.PMF;1", fixture::FILM),
            ("PIC1.PNG;1", fixture::BACKDROP),
            ("SND0.AT3;1", fixture::MUSIC),
        ]
    }

    /// The case this module exists for: a UMD image, and what the handheld's
    /// menu dressed the game in, read out of `PSP_GAME/`.
    #[test]
    fn a_umd_image_gives_up_its_icon_film_music_and_backdrop() {
        let dir = scratch("iso");
        let rom = dir.join("Tekken 6.iso");
        std::fs::write(&rom, fixture::iso(&the_four())).unwrap();
        let own = of(&dir.join("own"), &rom);
        assert_eq!(std::fs::read(own.icon.unwrap()).unwrap(), fixture::PNG);
        assert_eq!(std::fs::read(own.preview.unwrap()).unwrap(), fixture::FILM);
        assert_eq!(std::fs::read(own.music.unwrap()).unwrap(), fixture::MUSIC);
        assert_eq!(
            std::fs::read(own.backdrop.unwrap()).unwrap(),
            fixture::BACKDROP
        );
        assert_eq!(own.overlay, None, "this game has no PIC0");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A compressed image is the same disc, and gives up the same files —
    /// through blocks both deflated and kept as they were.
    #[test]
    fn a_compressed_image_is_read_through_its_blocks() {
        let dir = scratch("cso");
        let iso = fixture::iso(&the_four());
        let rom = dir.join("Tekken 6.cso");
        std::fs::write(&rom, fixture::cso(&iso)).unwrap();
        let own = of(&dir.join("own"), &rom);
        assert_eq!(std::fs::read(own.icon.unwrap()).unwrap(), fixture::PNG);
        assert_eq!(std::fs::read(own.music.unwrap()).unwrap(), fixture::MUSIC);
        // And a CSO somebody named `.iso` is still read as one.
        let renamed = dir.join("Renamed.iso");
        std::fs::write(&renamed, fixture::cso(&iso)).unwrap();
        assert!(of(&dir.join("own"), &renamed).preview.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A game from the Store is a PBP, whose header says where each file is.
    #[test]
    fn a_store_game_gives_up_what_its_header_names() {
        let dir = scratch("pbp");
        let rom = dir.join("EBOOT.PBP");
        std::fs::write(
            &rom,
            fixture::pbp(Some(fixture::PNG), None, Some(fixture::BACKDROP)),
        )
        .unwrap();
        let own = of(&dir.join("own"), &rom);
        assert_eq!(std::fs::read(own.icon.unwrap()).unwrap(), fixture::PNG);
        assert_eq!(
            std::fs::read(own.backdrop.unwrap()).unwrap(),
            fixture::BACKDROP
        );
        assert_eq!((own.preview, own.music), (None, None));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file where a picture should be that is not one is left out, rather
    /// than drawn as noise on a card.
    #[test]
    fn a_picture_that_is_not_one_is_left_out() {
        let dir = scratch("noise");
        let rom = dir.join("Odd.iso");
        std::fs::write(
            &rom,
            fixture::iso(&[
                ("ICON0.PNG;1", b"encrypted"),
                ("PIC1.PNG;1", fixture::BACKDROP),
            ]),
        )
        .unwrap();
        let own = of(&dir.join("own"), &rom);
        assert_eq!(own.icon, None);
        assert!(own.backdrop.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Read once. The copies are what every later scan answers with, and a
    /// game changed on the disk is read again.
    #[test]
    fn a_game_is_read_once_until_it_changes() {
        let dir = scratch("once");
        let root = dir.join("own");
        let rom = dir.join("Tekken 6.iso");
        std::fs::write(&rom, fixture::iso(&the_four())).unwrap();
        let first = of(&root, &rom);
        // The copy is what is answered with: taking the icon out of the copies
        // and asking again gives no icon, because nothing was read.
        std::fs::remove_file(first.icon.as_ref().unwrap()).unwrap();
        assert_eq!(of(&root, &rom).icon, None);
        // A different game in the same file is read again.
        std::fs::write(
            &rom,
            fixture::iso(&[("ICON0.PNG;1", fixture::PNG), ("PIC0.PNG;1", fixture::PNG)]),
        )
        .unwrap();
        let again = of(&root, &rom);
        assert!(again.icon.is_some() && again.overlay.is_some());
        assert_eq!(again.preview, None, "the old film went with the old game");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A disc that is not a PSP game's, and a kind of file nothing is read
    /// out of, carry nothing.
    #[test]
    fn what_is_not_a_psp_game_carries_nothing() {
        let dir = scratch("nothing");
        let root = dir.join("own");
        let data = dir.join("Data.iso");
        std::fs::write(&data, vec![0u8; 40 * SECTOR as usize]).unwrap();
        assert_eq!(of(&root, &data), Own::default());
        let chd = dir.join("Tekken 6.chd");
        std::fs::write(&chd, fixture::iso(&the_four())).unwrap();
        assert_eq!(of(&root, &chd), Own::default());
        assert!(!folder(&root, &chd).exists(), "nothing was even tried");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A game taken off the drive takes its copies with it; one still there
    /// keeps them.
    #[test]
    fn a_game_that_has_gone_takes_its_copies_with_it() {
        let dir = scratch("sweep");
        let root = dir.join("own");
        let (kept_rom, gone_rom) = (dir.join("a.iso"), dir.join("b.iso"));
        for rom in [&kept_rom, &gone_rom] {
            std::fs::write(rom, fixture::iso(&the_four())).unwrap();
            of(&root, rom);
        }
        sweep(&root, &HashSet::from([folder(&root, &kept_rom)]));
        assert!(folder(&root, &kept_rom).exists());
        assert!(!folder(&root, &gone_rom).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_the_psp_carries_its_own() {
        assert!(carries_its_own(crate::consoles::machine("psp")));
        assert!(carries_its_own(crate::consoles::machine(
            "PlayStation Portable"
        )));
        assert!(!carries_its_own(crate::consoles::machine("psx")));
        assert!(!carries_its_own(None));
    }
}
