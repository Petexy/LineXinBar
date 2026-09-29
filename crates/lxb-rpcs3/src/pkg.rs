//! A PlayStation Network package, read in place.
//!
//! A `.pkg` is how Sony delivered everything that was not on a disc: games,
//! updates, add-ons. It is a header, a few metadata packets, and a table of
//! files followed by their contents — the table and the contents encrypted
//! together in counter mode under a key every retail package shares (see
//! [`crate::crypt::PKG_KEY`]). The layout is RPCS3's `Crypto/unpkg.h`, and
//! psdevwiki's page on PKG files.
//!
//! **Nothing here installs anything.** Installing is RPCS3's own installer's
//! job — it knows which updates fit which game, and what a licence is for —
//! and this only reads what the shell shows before that: the game's name, its
//! icon, its backdrop, its film and its music, and where RPCS3 would put it.

use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::path::Path;

use crate::crypt;
use crate::zip::{Entry, EntryReader};

/// Where a package's bytes come from: a file on the disk, or an entry of a zip
/// that is read without being unpacked.
pub enum Source {
    File(File),
    Zip(Box<EntryReader>),
}

impl Source {
    pub fn file(path: &Path) -> io::Result<Self> {
        Ok(Source::File(File::open(path)?))
    }

    pub fn zipped(archive: &Path, entry: &Entry) -> io::Result<Self> {
        Ok(Source::Zip(Box::new(EntryReader::open(archive, entry)?)))
    }

    fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<()> {
        match self {
            Source::File(file) => file.read_exact_at(buffer, offset),
            Source::Zip(entry) => entry.read_at(offset, buffer),
        }
    }
}

/// What a package holds — see RPCS3's `pkg_content_type`. Only the kinds the
/// shell has anything to say about are named; everything else is `Other` and
/// is left to RPCS3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Content {
    /// Game data: an update, or an add-on for a game.
    GameData,
    /// A game.
    GameExec,
    /// A PlayStation classic, run by the PS3's own PS1 emulator.
    Ps1Classic,
    /// A licence and nothing else.
    License,
    Minis,
    NeoGeo,
    Other(u32),
}

impl Content {
    /// Whether a package of this kind is a game somebody plays, or an update
    /// or add-on to one — rather than a theme, an avatar, a licence on its own
    /// or a piece of the console's system, which RPCS3 would install and
    /// nothing on the bar could start.
    pub fn is_played(self) -> bool {
        matches!(
            self,
            Content::GameData
                | Content::GameExec
                | Content::Ps1Classic
                | Content::Minis
                | Content::NeoGeo
        )
    }

    fn from(value: u32) -> Self {
        match value {
            0x04 => Content::GameData,
            0x05 => Content::GameExec,
            0x06 => Content::Ps1Classic,
            0x0b => Content::License,
            0x0f => Content::Minis,
            0x10 => Content::NeoGeo,
            other => Content::Other(other),
        }
    }
}

/// One file in the package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub name: String,
    /// From the start of the package's encrypted data.
    offset: u64,
    pub size: u64,
    kind: u32,
}

impl Item {
    pub fn is_directory(&self) -> bool {
        self.kind & 0xff == 4
    }
}

/// An open package.
pub struct Package {
    source: Source,
    /// The content ID, `EP9000-NPEA00019_00-TEKKENRETAIL0000`, which is also
    /// what its licence file is named after.
    pub content_id: String,
    pub content: Content,
    /// Where RPCS3 installs it, under `dev_hdd0/game`: the title ID out of the
    /// content ID, or the directory an add-on's metadata names instead.
    pub install_dir: String,
    pub items: Vec<Item>,
    data_offset: u64,
    data_size: u64,
    nonce: [u8; 16],
    key: [u8; 16],
}

const MAGIC: [u8; 4] = [0x7f, b'P', b'K', b'G'];
const RETAIL: u16 = 0x8000;
const PS3: u16 = 0x0001;

impl Package {
    /// Read a package's header, metadata and file table.
    pub fn open(mut source: Source) -> io::Result<Self> {
        let mut header = [0u8; 0xc0];
        source.read_at(0, &mut header)?;
        if header[..4] != MAGIC {
            return Err(invalid("not a PS3 package"));
        }
        let release = be16(&header, 4);
        let platform = be16(&header, 6);
        if platform != PS3 {
            return Err(unsupported("a PSP or PS Vita package"));
        }
        if release != RETAIL {
            return Err(unsupported("a debug package"));
        }
        let meta_offset = u64::from(be32(&header, 8));
        let meta_count = be32(&header, 12);
        let file_count = be32(&header, 20);
        let data_offset = be64(&header, 32);
        let data_size = be64(&header, 40);
        let content_id = String::from_utf8_lossy(&header[0x30..0x60])
            .trim_end_matches('\0')
            .trim()
            .to_string();
        let nonce: [u8; 16] = header[0x70..0x80].try_into().expect("sixteen bytes");
        if file_count == 0 || file_count > 1_000_000 {
            return Err(invalid("a package with no sensible file table"));
        }

        // The metadata packets, which are not encrypted.
        let mut content = Content::Other(0);
        let mut install_dir = content_id.get(7..16).unwrap_or_default().to_string();
        let mut at = meta_offset;
        for _ in 0..meta_count.min(64) {
            let mut packet = [0u8; 8];
            source.read_at(at, &mut packet)?;
            let id = be32(&packet, 0);
            let size = be32(&packet, 4) as usize;
            if size > 4096 {
                break;
            }
            let mut value = vec![0u8; size];
            source.read_at(at + 8, &mut value)?;
            match id {
                0x2 if size == 4 => content = Content::from(be32(&value, 0)),
                // An add-on names the directory of the game it belongs to,
                // after eight bytes of something else.
                0xa if size > 8 => {
                    let name = value[8..].split(|b| *b == 0).next().unwrap_or_default();
                    let name = String::from_utf8_lossy(name).trim().to_string();
                    if !name.is_empty() {
                        install_dir = name;
                    }
                }
                _ => {}
            }
            at += 8 + size as u64;
        }
        if install_dir.is_empty() || install_dir.contains('/') || install_dir.starts_with('.') {
            return Err(invalid("a package that would install outside its folder"));
        }

        let mut package = Package {
            source,
            content_id,
            content,
            install_dir,
            items: Vec::new(),
            data_offset,
            data_size,
            nonce,
            key: crypt::PKG_KEY,
        };
        // RPCS3's order: the retail key, and the demonstration units' key where
        // the table makes no sense under it.
        for key in [crypt::PKG_KEY, crypt::PKG_KEY_IDU] {
            package.key = key;
            if let Some(items) = package.table(file_count)? {
                package.items = items;
                return Ok(package);
            }
        }
        Err(invalid("a package whose files cannot be read"))
    }

    /// The file table under the current key, or `None` where it makes no sense
    /// — which is the only way a wrong key shows.
    fn table(&mut self, count: u32) -> io::Result<Option<Vec<Item>>> {
        let mut table = vec![0u8; count as usize * 32];
        self.decrypt(0, &mut table)?;
        let mut items = Vec::with_capacity(count as usize);
        for entry in table.chunks_exact(32) {
            let name_offset = u64::from(be32(entry, 0));
            let name_size = be32(entry, 4) as usize;
            let offset = be64(entry, 8);
            let size = be64(entry, 16);
            let kind = be32(entry, 24);
            if name_size == 0
                || name_size > 256
                || name_offset + name_size as u64 > self.data_size
                || offset
                    .checked_add(size)
                    .is_none_or(|end| end > self.data_size)
            {
                return Ok(None);
            }
            items.push((name_offset, name_size, offset, size, kind));
        }
        // The names, in the order they sit in the package, so a zipped one is
        // read forwards.
        let mut order: Vec<usize> = (0..items.len()).collect();
        order.sort_by_key(|&index| items[index].0);
        let mut names = vec![String::new(); items.len()];
        for index in order {
            let (name_offset, name_size, ..) = items[index];
            let mut name = vec![0u8; name_size];
            self.decrypt(name_offset, &mut name)?;
            let name = name.split(|b| *b == 0).next().unwrap_or_default();
            let Ok(name) = std::str::from_utf8(name) else {
                return Ok(None);
            };
            if name.is_empty() || name.chars().any(char::is_control) {
                return Ok(None);
            }
            names[index] = name.trim_start_matches('/').to_string();
        }
        Ok(Some(
            items
                .into_iter()
                .zip(names)
                .map(|((_, _, offset, size, kind), name)| Item {
                    name,
                    offset,
                    size,
                    kind,
                })
                .collect(),
        ))
    }

    fn decrypt(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<()> {
        self.source.read_at(self.data_offset + offset, buffer)?;
        crypt::ctr(&self.key, &self.nonce, offset, buffer);
        Ok(())
    }

    /// The title ID, `NPEA00019`, out of the content ID.
    #[cfg(test)]
    pub fn title_id(&self) -> Option<&str> {
        self.content_id.get(7..16).filter(|id| id.len() == 9)
    }

    /// A file of the package by name, as it would be installed —
    /// `PARAM.SFO`, `ICON0.PNG`.
    pub fn item(&self, name: &str) -> Option<&Item> {
        self.items
            .iter()
            .find(|item| !item.is_directory() && item.name.eq_ignore_ascii_case(name))
    }

    /// An item's bytes, or `None` where it is bigger than `limit`.
    pub fn read(&mut self, item: &Item, limit: u64) -> io::Result<Option<Vec<u8>>> {
        if item.size > limit {
            return Ok(None);
        }
        let mut bytes = vec![0u8; item.size as usize];
        self.decrypt(item.offset, &mut bytes)?;
        Ok(Some(bytes))
    }

    /// The installed size of everything in it, which is what a bar measures an
    /// install against.
    pub fn unpacked_size(&self) -> u64 {
        self.items.iter().map(|item| item.size).sum()
    }
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_string())
}

fn unsupported(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, what.to_string())
}

fn be16(bytes: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([bytes[at], bytes[at + 1]])
}

fn be32(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(bytes[at..at + 4].try_into().expect("four bytes"))
}

fn be64(bytes: &[u8], at: usize) -> u64 {
    u64::from_be_bytes(bytes[at..at + 8].try_into().expect("eight bytes"))
}

/// A retail package written by hand, for the tests of this module and of the
/// scan: `files` are its items, `content` its content type.
#[cfg(test)]
pub fn build(content_id: &str, content: u32, files: &[(&str, &[u8])]) -> Vec<u8> {
    let nonce = [0x11u8; 16];
    // Metadata: the content type, and nothing that says patch.
    let mut meta = Vec::new();
    for (id, value) in [(2u32, content), (3u32, 0)] {
        meta.extend_from_slice(&id.to_be_bytes());
        meta.extend_from_slice(&4u32.to_be_bytes());
        meta.extend_from_slice(&value.to_be_bytes());
    }
    let meta_offset = 0xc0u32;
    let data_offset = (0xc0 + meta.len() as u64).next_multiple_of(16);

    // The data: the table, the names, then each file.
    let table_size = files.len() * 32;
    let mut names = Vec::new();
    let mut name_offsets = Vec::new();
    for (name, _) in files {
        name_offsets.push(table_size + names.len());
        names.extend_from_slice(name.as_bytes());
        names.resize(names.len().next_multiple_of(16), 0);
    }
    let mut body = Vec::new();
    let mut offsets = Vec::new();
    let body_start = table_size + names.len();
    for (_, data) in files {
        offsets.push(body_start + body.len());
        body.extend_from_slice(data);
        body.resize(body.len().next_multiple_of(16), 0);
    }
    let mut data = Vec::new();
    for (index, (name, bytes)) in files.iter().enumerate() {
        data.extend_from_slice(&(name_offsets[index] as u32).to_be_bytes());
        data.extend_from_slice(&(name.len() as u32).to_be_bytes());
        data.extend_from_slice(&(offsets[index] as u64).to_be_bytes());
        data.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
        data.extend_from_slice(&3u32.to_be_bytes());
        data.extend_from_slice(&0u32.to_be_bytes());
    }
    data.extend_from_slice(&names);
    data.extend_from_slice(&body);
    crypt::ctr(&crypt::PKG_KEY, &nonce, 0, &mut data);

    let mut out = vec![0u8; data_offset as usize];
    out[..4].copy_from_slice(&MAGIC);
    out[4..6].copy_from_slice(&RETAIL.to_be_bytes());
    out[6..8].copy_from_slice(&PS3.to_be_bytes());
    out[8..12].copy_from_slice(&meta_offset.to_be_bytes());
    out[12..16].copy_from_slice(&2u32.to_be_bytes());
    out[16..20].copy_from_slice(&(meta.len() as u32).to_be_bytes());
    out[20..24].copy_from_slice(&(files.len() as u32).to_be_bytes());
    out[32..40].copy_from_slice(&data_offset.to_be_bytes());
    out[40..48].copy_from_slice(&(data.len() as u64).to_be_bytes());
    out[0x30..0x30 + content_id.len()].copy_from_slice(content_id.as_bytes());
    out[0x70..0x80].copy_from_slice(&nonce);
    out[0xc0..0xc0 + meta.len()].copy_from_slice(&meta);
    let total = data_offset + data.len() as u64;
    out[24..32].copy_from_slice(&total.to_be_bytes());
    out.extend_from_slice(&data);
    out
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

    const ID: &str = "EP9000-NPEA00019_00-TEKKENRETAIL0000";

    #[test]
    fn reads_the_table_and_the_files() {
        let bytes = build(
            ID,
            0x05,
            &[
                ("PARAM.SFO", b"\0PSF sfo"),
                ("ICON0.PNG", b"\x89PNG icon"),
                ("USRDIR/EBOOT.BIN", b"SCE\0"),
            ],
        );
        let file = write(&bytes);
        let mut package = Package::open(Source::file(file.path()).unwrap()).expect("a package");
        assert_eq!(package.content_id, ID);
        assert_eq!(package.title_id(), Some("NPEA00019"));
        assert_eq!(package.install_dir, "NPEA00019");
        assert_eq!(package.content, Content::GameExec);
        assert!(package.content.is_played());
        assert_eq!(package.items.len(), 3);
        let icon = package.item("icon0.png").cloned().expect("the icon");
        assert_eq!(
            package.read(&icon, 1 << 20).unwrap().as_deref(),
            Some(&b"\x89PNG icon"[..])
        );
        assert_eq!(package.read(&icon, 4).unwrap(), None);
    }

    /// The Tekken 5 shape: the package inside a zip, beside its licence.
    #[test]
    fn reads_a_package_inside_a_zip() {
        let package = build(
            ID,
            0x05,
            &[("PARAM.SFO", b"\0PSF sfo"), ("PIC1.PNG", &[9u8; 5000])],
        );
        let zipped = crate::zip::build(&[("x.pkg", &package), ("licence.rap", &[1; 16])], true);
        let file = write(&zipped);
        let entries = crate::zip::entries(file.path()).expect("a zip");
        let mut package =
            Package::open(Source::zipped(file.path(), &entries[0]).unwrap()).expect("a package");
        let backdrop = package.item("PIC1.PNG").cloned().expect("the backdrop");
        assert_eq!(
            package.read(&backdrop, 1 << 20).unwrap(),
            Some(vec![9u8; 5000])
        );
    }

    #[test]
    fn a_zip_or_a_vita_package_is_not_a_ps3_package() {
        let file = write(b"PK\x03\x04 a zip");
        assert!(Package::open(Source::file(file.path()).unwrap()).is_err());
        let mut vita = build(ID, 0x15, &[("PARAM.SFO", b"\0PSF")]);
        vita[6..8].copy_from_slice(&2u16.to_be_bytes());
        let file = write(&vita);
        let refused = Package::open(Source::file(file.path()).unwrap())
            .err()
            .expect("refused");
        assert_eq!(refused.kind(), io::ErrorKind::Unsupported);
    }
}
