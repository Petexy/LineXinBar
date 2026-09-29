//! A PlayStation 3 disc image, read as far as this helper needs to: the files
//! under `PS3_GAME`, and whether the image is still encrypted.
//!
//! ## The filesystem
//!
//! A PS3 Blu-ray carries ISO 9660 (and UDF beside it). ISO 9660 is the one
//! read here because it is the simpler of the two and every PS3 disc has it:
//! the primary volume descriptor at sector 16, a root directory record in it,
//! and directories that are lists of records, none of which crosses a sector.
//! Only small files are ever read — the pictures, the film, the music and
//! `PARAM.SFO` — so the multi-extent records a four-gigabyte file is split
//! into are walked past and never followed.
//!
//! ## The three kinds of image
//!
//! Sector 0 of a Redump dump is the disc's own **region table**: the ranges of
//! sectors that are plain, with everything between them encrypted under the
//! disc's key. Sony put the filesystem and the pictures in the plain ranges
//! and the game in the encrypted ones.
//!
//! * **Encrypted** — as it came off the disc. RPCS3 can boot it only with the
//!   disc's key beside it (see [`crate::keys`]).
//! * **Decrypted** — a Redump dump somebody ran through PS3Dec or the like.
//!   It keeps the region table, but every sector is plain. **Giving RPCS3 a
//!   key for one of these breaks it**: RPCS3 applies a key it finds by name
//!   without checking it, and decrypting plain sectors makes noise of them.
//!   Both of the images this was first tried on were this kind.
//! * **Plain** — a table that marks nothing encrypted, or a 3k3y image, which
//!   RPCS3 reads on its own.
//!
//! What tells the first two apart is the first sector of a file whose first
//! bytes are known — `LIC.DAT` starts `PS3LICDA`, `EBOOT.BIN` starts `SCE` —
//! lying in a range the table calls encrypted: if it reads as its magic
//! without a key, the image has been decrypted. That is the test RPCS3 itself
//! uses to prove a key, run the other way round.

use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::path::Path;

use crate::crypt;

pub const SECTOR: u64 = 2048;

/// Where a file's bytes are on the image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Extent {
    pub lba: u32,
    pub size: u64,
}

/// A range of sectors, first to last inclusive, and whether the disc encrypted
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub first: u32,
    pub last: u32,
    pub encrypted: bool,
}

/// Which of the three kinds of image this is — see the module note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encryption {
    Plain,
    Decrypted,
    /// Still encrypted. `probe` is a sector that decrypts to `magic` under the
    /// right key, which is how a candidate key is proven.
    Encrypted {
        probe: Probe,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Probe {
    pub lba: u32,
    pub head: [u8; 16],
    pub magic: &'static [u8],
}

/// The files RPCS3 tests a key against, in its order, and what each begins
/// with. From `iso_file_decryption::retrieve_key` in `Loader/ISO.cpp`.
const KNOWN: &[(&str, &[u8])] = &[
    ("PS3_GAME/LICDIR/LIC.DAT", b"PS3LICDA"),
    ("PS3_GAME/USRDIR/EBOOT.BIN", b"SCE\0"),
];

/// An open disc image.
pub struct Iso {
    file: File,
    root: Extent,
    regions: Vec<Region>,
    serial: Option<String>,
}

impl Iso {
    /// Open one, or say why it is not a PS3 disc image.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let mut head = vec![0u8; 2 * SECTOR as usize];
        file.read_exact_at(&mut head, 0)?;
        let mut volume = vec![0u8; SECTOR as usize];
        file.read_exact_at(&mut volume, 16 * SECTOR)?;
        if volume[0] != 1 || &volume[1..6] != b"CD001" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "no ISO 9660 volume descriptor",
            ));
        }
        let root = record(&volume[156..190])
            .map(|(extent, _, _)| extent)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no root directory"))?;
        let serial = (&head[0x800..0x80c] == b"PlayStation3")
            .then(|| {
                let raw = String::from_utf8_lossy(&head[0x810..0x820])
                    .trim()
                    .to_string();
                (!raw.is_empty()).then(|| raw.replace('-', ""))
            })
            .flatten();
        Ok(Iso {
            file,
            root,
            regions: regions(&head),
            serial,
        })
    }

    /// The serial the disc's own sector 1 carries, `BLUS30359`, where it has
    /// one — a Redump dump always does, a 3k3y image sometimes does not.
    pub fn serial(&self) -> Option<&str> {
        self.serial.as_deref()
    }

    /// Where a file is, by its path from the root: `PS3_GAME/ICON0.PNG`.
    /// Matched without regard to case or ISO 9660's `;1` version suffix.
    pub fn find(&self, path: &str) -> Option<Extent> {
        let mut here = self.root;
        let mut parts = path.split('/').filter(|part| !part.is_empty()).peekable();
        while let Some(part) = parts.next() {
            let last = parts.peek().is_none();
            let (extent, directory) = self.lookup(here, part)?;
            if last {
                return (!directory).then_some(extent);
            }
            if !directory {
                return None;
            }
            here = extent;
        }
        None
    }

    /// Whether the image has a directory at this path — a `PS3_GAME` is what
    /// makes an ISO a PS3 game rather than a film or a PS2 disc.
    pub fn has_directory(&self, path: &str) -> bool {
        let mut here = self.root;
        for part in path.split('/').filter(|part| !part.is_empty()) {
            match self.lookup(here, part) {
                Some((extent, true)) => here = extent,
                _ => return false,
            }
        }
        true
    }

    /// What a directory holds: each name, as the image spells it without its
    /// `;1`, and whether it is a directory itself. Empty for a path that is
    /// not a directory.
    pub fn list(&self, path: &str) -> Vec<(String, bool)> {
        let mut here = self.root;
        for part in path.split('/').filter(|part| !part.is_empty()) {
            match self.lookup(here, part) {
                Some((extent, true)) => here = extent,
                _ => return Vec::new(),
            }
        }
        self.records(here)
            .into_iter()
            .filter(|(_, _, name)| !name.is_empty() && name != "\0" && name != "\u{1}")
            .map(|(_, directory, name)| {
                let name = name.split(';').next().unwrap_or(&name);
                (
                    name.strip_suffix('.').unwrap_or(name).to_string(),
                    directory,
                )
            })
            .collect()
    }

    fn lookup(&self, directory: Extent, name: &str) -> Option<(Extent, bool)> {
        self.records(directory)
            .into_iter()
            .find(|(_, _, entry)| same_name(entry, name))
            .map(|(extent, directory, _)| (extent, directory))
    }

    /// Every record of a directory, `.` and `..` included.
    fn records(&self, directory: Extent) -> Vec<(Extent, bool, String)> {
        // A directory of a PS3 disc is a few sectors at most; a record that
        // claims more than this is a damaged image, not a big directory.
        if directory.size > 4 * 1024 * 1024 {
            return Vec::new();
        }
        let mut listing = vec![0u8; directory.size as usize];
        if self
            .file
            .read_exact_at(&mut listing, u64::from(directory.lba) * SECTOR)
            .is_err()
        {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut at = 0usize;
        while at < listing.len() {
            let length = listing[at] as usize;
            if length == 0 {
                // The rest of this sector is padding: records never cross one.
                at = (at / SECTOR as usize + 1) * SECTOR as usize;
                continue;
            }
            let Some(bytes) = listing.get(at..at + length) else {
                break;
            };
            out.extend(record(bytes));
            at += length;
        }
        out
    }

    /// A file's bytes, up to `limit` of them, decrypted where the disc
    /// encrypted them and a key is given.
    ///
    /// Without a key, a file lying in an encrypted range of a still-encrypted
    /// image is refused rather than returned as noise.
    pub fn read(&self, extent: Extent, limit: u64, key: Option<&[u8; 16]>) -> io::Result<Vec<u8>> {
        let size = extent.size.min(limit);
        let mut bytes = vec![0u8; size as usize];
        self.file
            .read_exact_at(&mut bytes, u64::from(extent.lba) * SECTOR)?;
        let encrypted = matches!(self.encryption(), Encryption::Encrypted { .. });
        if !encrypted {
            return Ok(bytes);
        }
        for (index, sector) in bytes.chunks_mut(SECTOR as usize).enumerate() {
            let lba = extent.lba + index as u32;
            if !self.is_encrypted(lba) {
                continue;
            }
            let Some(key) = key else {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "the file is on the encrypted part of the disc",
                ));
            };
            // A short last sector is read whole and cut back, because CBC
            // decrypts in whole blocks.
            let mut whole = vec![0u8; SECTOR as usize];
            self.file
                .read_exact_at(&mut whole, u64::from(lba) * SECTOR)?;
            crypt::sector(key, lba, &mut whole);
            let length = sector.len();
            sector.copy_from_slice(&whole[..length]);
        }
        Ok(bytes)
    }

    /// A file's bytes, where it is there and not too big to want.
    pub fn file(&self, path: &str, limit: u64, key: Option<&[u8; 16]>) -> Option<Vec<u8>> {
        let extent = self.find(path)?;
        if extent.size > limit {
            return None;
        }
        self.read(extent, limit, key).ok()
    }

    #[cfg(test)]
    pub fn regions(&self) -> &[Region] {
        &self.regions
    }

    fn is_encrypted(&self, lba: u32) -> bool {
        self.regions
            .iter()
            .any(|region| region.encrypted && (region.first..=region.last).contains(&lba))
    }

    /// Which kind of image this is. See the module note.
    pub fn encryption(&self) -> Encryption {
        if !self.regions.iter().any(|region| region.encrypted) {
            return Encryption::Plain;
        }
        for (path, magic) in KNOWN {
            let Some(extent) = self.find(path) else {
                continue;
            };
            if extent.size < 16 || !self.is_encrypted(extent.lba) {
                continue;
            }
            let mut head = [0u8; 16];
            if self
                .file
                .read_exact_at(&mut head, u64::from(extent.lba) * SECTOR)
                .is_err()
            {
                continue;
            }
            if head.starts_with(magic) {
                return Encryption::Decrypted;
            }
            return Encryption::Encrypted {
                probe: Probe {
                    lba: extent.lba,
                    head,
                    magic,
                },
            };
        }
        // Neither file lies where the table says the disc is encrypted, so
        // neither can say whether it still is — and neither could RPCS3 prove a
        // key against it. Called plain, which is what RPCS3 reads it as
        // without a key.
        Encryption::Plain
    }
}

impl Probe {
    /// Whether `key` is this disc's.
    pub fn fits(&self, key: &[u8; 16]) -> bool {
        crypt::first_block(key, self.lba, &self.head).starts_with(self.magic)
    }
}

/// The region table in sector 0, as RPCS3's `iso_file_decryption::init`
/// reads it: a count of plain ranges, then the boundaries from byte 12 on,
/// alternating plain and encrypted and starting plain.
///
/// A sector 0 that is not a region table — every image that is not a Redump
/// dump — is one plain range covering everything.
fn regions(head: &[u8]) -> Vec<Region> {
    let count = u32::from_be_bytes(head[0..4].try_into().expect("four bytes")) as usize;
    let whole = vec![Region {
        first: 0,
        last: u32::MAX,
        encrypted: false,
    }];
    if count == 0 || count > 64 {
        return whole;
    }
    let mut out: Vec<Region> = Vec::new();
    for index in 0..count * 2 - 1 {
        let at = 12 + index * 4;
        let Some(bytes) = head.get(at..at + 4) else {
            return whole;
        };
        let bound = u32::from_be_bytes(bytes.try_into().expect("four bytes"));
        let encrypted = index % 2 == 1;
        let first = out
            .last()
            .map_or(0, |previous| previous.last.wrapping_add(1));
        // An encrypted range ends one before the plain range after it starts;
        // a plain range's bound is its own last sector.
        let last = if encrypted {
            bound.wrapping_sub(1)
        } else {
            bound
        };
        if last < first {
            return whole;
        }
        out.push(Region {
            first,
            last,
            encrypted,
        });
    }
    out
}

/// One directory record: where its file is, whether it is a directory, and its
/// name.
fn record(bytes: &[u8]) -> Option<(Extent, bool, String)> {
    if bytes.len() < 34 {
        return None;
    }
    let lba = u32::from_le_bytes(bytes[2..6].try_into().ok()?);
    let size = u32::from_le_bytes(bytes[10..14].try_into().ok()?);
    let flags = bytes[25];
    let name_length = bytes[32] as usize;
    let name = bytes.get(33..33 + name_length)?;
    Some((
        Extent {
            lba,
            size: u64::from(size),
        },
        flags & 0x02 != 0,
        String::from_utf8_lossy(name).into_owned(),
    ))
}

/// Whether a record's name is the one asked for: ISO 9660 upper-cases names
/// and adds `;1`, and a name with no extension may end in a bare `.`.
fn same_name(entry: &str, wanted: &str) -> bool {
    let entry = entry.split(';').next().unwrap_or(entry);
    let entry = entry.strip_suffix('.').unwrap_or(entry);
    entry.eq_ignore_ascii_case(wanted)
}

/// A tiny PS3 disc image, for the tests of this module and of the scan.
///
/// `files` are placed one after another from sector 32 in the order given, and
/// `encrypted` lists the ranges sector 0's table should call encrypted.
#[cfg(test)]
pub mod fixture {
    use super::SECTOR;

    pub struct Built {
        pub bytes: Vec<u8>,
        /// Where each file landed, in the order given.
        pub lbas: Vec<u32>,
    }

    pub fn build(serial: &str, files: &[(&str, &[u8])], encrypted: &[(u32, u32)]) -> Built {
        let sector = SECTOR as usize;
        // Directories: the root, and every directory a file is under.
        let mut directories: Vec<String> = vec![String::new()];
        for (path, _) in files {
            let mut parent = String::new();
            for part in path.split('/').collect::<Vec<_>>().split_last().unwrap().1 {
                parent = if parent.is_empty() {
                    part.to_string()
                } else {
                    format!("{parent}/{part}")
                };
                if !directories.contains(&parent) {
                    directories.push(parent.clone());
                }
            }
        }
        let first_directory = 20u32;
        let first_file = 32u32;
        let mut lbas = Vec::new();
        let mut next = first_file;
        for (_, data) in files {
            lbas.push(next);
            next += (data.len().div_ceil(sector)).max(1) as u32;
        }
        let total = next as usize + 1;
        let mut bytes = vec![0u8; total * sector];

        // Sector 0: the region table. Plain ranges are what is between the
        // encrypted ones.
        let mut plain = Vec::new();
        let mut start = 0u32;
        for (first, last) in encrypted {
            plain.push((start, first - 1));
            start = last + 1;
        }
        plain.push((start, total as u32 - 1));
        bytes[0..4].copy_from_slice(&(plain.len() as u32).to_be_bytes());
        for (index, (first, last)) in plain.iter().enumerate() {
            let at = 8 + index * 8;
            bytes[at..at + 4].copy_from_slice(&first.to_be_bytes());
            bytes[at + 4..at + 8].copy_from_slice(&last.to_be_bytes());
        }
        bytes[0x800..0x80c].copy_from_slice(b"PlayStation3");
        let serial = format!("{serial:<16}");
        bytes[0x810..0x820].copy_from_slice(&serial.as_bytes()[..16]);

        // Sector 16: the primary volume descriptor.
        let pvd = 16 * sector;
        bytes[pvd] = 1;
        bytes[pvd + 1..pvd + 6].copy_from_slice(b"CD001");
        let root = entry(first_directory, sector as u32, true, "\0");
        bytes[pvd + 156..pvd + 156 + root.len()].copy_from_slice(&root);

        // One sector per directory.
        for (index, directory) in directories.iter().enumerate() {
            let mut listing = Vec::new();
            for (child_index, child) in directories.iter().enumerate() {
                let parent = child.rsplit_once('/').map_or("", |(parent, _)| parent);
                if !child.is_empty() && parent == directory {
                    let name = child.rsplit('/').next().unwrap();
                    listing.extend(entry(
                        first_directory + child_index as u32,
                        sector as u32,
                        true,
                        name,
                    ));
                }
            }
            for ((path, data), lba) in files.iter().zip(&lbas) {
                let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
                if parent == directory {
                    listing.extend(entry(*lba, data.len() as u32, false, &format!("{name};1")));
                }
            }
            let at = (first_directory as usize + index) * sector;
            bytes[at..at + listing.len()].copy_from_slice(&listing);
        }
        for ((_, data), lba) in files.iter().zip(&lbas) {
            let at = *lba as usize * sector;
            bytes[at..at + data.len()].copy_from_slice(data);
        }
        Built { bytes, lbas }
    }

    fn entry(lba: u32, size: u32, directory: bool, name: &str) -> Vec<u8> {
        let mut record = vec![0u8; 33];
        record[2..6].copy_from_slice(&lba.to_le_bytes());
        record[6..10].copy_from_slice(&lba.to_be_bytes());
        record[10..14].copy_from_slice(&size.to_le_bytes());
        record[14..18].copy_from_slice(&size.to_be_bytes());
        record[25] = if directory { 2 } else { 0 };
        record[32] = name.len() as u8;
        record.extend_from_slice(name.as_bytes());
        if record.len() % 2 == 1 {
            record.push(0);
        }
        record[0] = record.len() as u8;
        record
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write(bytes: &[u8]) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().expect("a scratch file");
        file.write_all(bytes).expect("written");
        file
    }

    fn lic() -> Vec<u8> {
        let mut data = b"PS3LICDA".to_vec();
        data.resize(4096, 0x5a);
        data
    }

    #[test]
    fn finds_files_by_path_whatever_their_case() {
        let built = fixture::build(
            "BLUS-30359",
            &[
                ("PS3_GAME/PARAM.SFO", b"\0PSF...."),
                ("PS3_GAME/ICON0.PNG", b"\x89PNG icon"),
            ],
            &[],
        );
        let file = write(&built.bytes);
        let iso = Iso::open(file.path()).expect("an image");
        assert_eq!(iso.serial(), Some("BLUS30359"));
        assert!(iso.has_directory("PS3_GAME"));
        assert!(!iso.has_directory("PS3_GAME/USRDIR"));
        assert_eq!(
            iso.file("ps3_game/icon0.png", 1 << 20, None).as_deref(),
            Some(&b"\x89PNG icon"[..])
        );
        assert_eq!(iso.file("PS3_GAME/ICON1.PAM", 1 << 20, None), None);
        // A directory is not a file.
        assert_eq!(iso.find("PS3_GAME"), None);
        assert_eq!(
            iso.list("PS3_GAME"),
            vec![
                ("PARAM.SFO".to_string(), false),
                ("ICON0.PNG".to_string(), false)
            ]
        );
        assert_eq!(iso.list(""), vec![("PS3_GAME".to_string(), true)]);
    }

    #[test]
    fn a_table_with_nothing_encrypted_is_plain() {
        let built = fixture::build("BLES-00001", &[("PS3_GAME/LICDIR/LIC.DAT", &lic())], &[]);
        let file = write(&built.bytes);
        let iso = Iso::open(file.path()).expect("an image");
        assert_eq!(iso.encryption(), Encryption::Plain);
    }

    /// The case both of the user's own dumps turned out to be: Redump's table
    /// still there, and the "encrypted" range already plain.
    #[test]
    fn a_decrypted_dump_is_told_from_an_encrypted_one() {
        let built = fixture::build(
            "BLUS-30464",
            &[("PS3_GAME/LICDIR/LIC.DAT", &lic())],
            &[(32, 33)],
        );
        let file = write(&built.bytes);
        let iso = Iso::open(file.path()).expect("an image");
        assert_eq!(
            iso.regions(),
            &[
                Region {
                    first: 0,
                    last: 31,
                    encrypted: false
                },
                Region {
                    first: 32,
                    last: 33,
                    encrypted: true
                },
                Region {
                    first: 34,
                    last: 34,
                    encrypted: false
                },
            ]
        );
        assert_eq!(iso.encryption(), Encryption::Decrypted);
    }

    #[test]
    fn an_encrypted_dump_is_proven_only_by_its_own_key() {
        let key = [0x3cu8; 16];
        let mut built = fixture::build(
            "BLUS-30359",
            &[("PS3_GAME/LICDIR/LIC.DAT", &lic())],
            &[(32, 33)],
        );
        let lba = built.lbas[0];
        for sector in 32..=33u32 {
            let at = sector as usize * SECTOR as usize;
            crypt::encrypt_sector(&key, sector, &mut built.bytes[at..at + SECTOR as usize]);
        }
        let file = write(&built.bytes);
        let iso = Iso::open(file.path()).expect("an image");
        let Encryption::Encrypted { probe } = iso.encryption() else {
            panic!("an encrypted image");
        };
        assert_eq!(probe.lba, lba);
        assert!(probe.fits(&key));
        assert!(!probe.fits(&[0x3du8; 16]));
        // And the file reads back whole through the key, and not without it.
        let extent = iso.find("PS3_GAME/LICDIR/LIC.DAT").expect("the file");
        assert_eq!(iso.read(extent, 1 << 20, Some(&key)).expect("read"), lic());
        assert!(iso.read(extent, 1 << 20, None).is_err());
    }

    #[test]
    fn a_film_is_not_a_game() {
        let file = write(&vec![0u8; 40 * SECTOR as usize]);
        assert!(Iso::open(file.path()).is_err());
    }
}
