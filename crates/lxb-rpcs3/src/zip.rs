//! Reading a zip without unpacking it.
//!
//! A PS3 game downloaded as a package nearly always arrives as a zip holding
//! the `.pkg` and the `.rap` licence that unlocks it — Tekken 5: Dark
//! Resurrection, the first one this was written for, is exactly that. The
//! shell lists such a zip as the game it holds, with its pictures, so its
//! contents have to be read in place: a package is hundreds of megabytes, and
//! unpacking one to look at its icon would be absurd.
//!
//! What is here is the part of the format those zips use: the central
//! directory (zip64 included, because a package can be past four gigabytes),
//! entries stored or deflated, and a reader over one entry that can be asked
//! for any offset — forwards by reading on, backwards by starting the entry
//! again. Everything else a zip can be — encrypted, split, compressed with
//! anything but deflate — is refused by name rather than half read.

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use flate2::read::DeflateDecoder;

/// One file in the archive, as its central directory describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    method: u16,
    flags: u16,
    pub compressed: u64,
    pub size: u64,
    pub crc: u32,
    local: u64,
}

impl Entry {
    /// The last part of its name, `foo.pkg` for `dir/foo.pkg`.
    pub fn file_name(&self) -> &str {
        self.name.rsplit('/').next().unwrap_or(&self.name)
    }

    pub fn is_directory(&self) -> bool {
        self.name.ends_with('/')
    }

    /// Whether this reader can unpack it at all.
    pub fn readable(&self) -> bool {
        self.flags & 0x0001 == 0 && matches!(self.method, 0 | 8)
    }
}

/// The central directory of the zip at `path`.
pub fn entries(path: &Path) -> io::Result<Vec<Entry>> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    // The end record is the last thing in the file, after a comment of up to
    // 65535 bytes.
    let tail_length = length.min(65_557 + 20);
    let mut tail = vec![0u8; tail_length as usize];
    file.seek(SeekFrom::Start(length - tail_length))?;
    file.read_exact(&mut tail)?;
    let end = (0..tail.len().saturating_sub(21))
        .rev()
        .find(|&at| tail[at..at + 4] == [0x50, 0x4b, 0x05, 0x06])
        .ok_or_else(|| invalid("no end of central directory"))?;
    let mut count = u64::from(le16(&tail, end + 10));
    let mut directory_size = u64::from(le32(&tail, end + 12));
    let mut directory_at = u64::from(le32(&tail, end + 16));

    // zip64: the locator sits just before the end record and points at a
    // record with the real numbers.
    if (count == 0xffff || directory_size == 0xffff_ffff || directory_at == 0xffff_ffff)
        && end >= 20
        && tail[end - 20..end - 16] == [0x50, 0x4b, 0x06, 0x07]
    {
        let record_at = le64(&tail, end - 20 + 8);
        let mut record = [0u8; 56];
        file.seek(SeekFrom::Start(record_at))?;
        file.read_exact(&mut record)?;
        if record[..4] != [0x50, 0x4b, 0x06, 0x06] {
            return Err(invalid("a zip64 locator pointing at nothing"));
        }
        count = le64(&record, 32);
        directory_size = le64(&record, 40);
        directory_at = le64(&record, 48);
    }
    if directory_size > 256 * 1024 * 1024 || directory_at + directory_size > length {
        return Err(invalid("a central directory past the end of the file"));
    }
    let mut directory = vec![0u8; directory_size as usize];
    file.seek(SeekFrom::Start(directory_at))?;
    file.read_exact(&mut directory)?;

    let mut out = Vec::new();
    let mut at = 0usize;
    for _ in 0..count {
        if directory.get(at..at + 4) != Some(&[0x50, 0x4b, 0x01, 0x02][..]) {
            return Err(invalid("a damaged central directory"));
        }
        let header = directory
            .get(at..at + 46)
            .ok_or_else(|| invalid("a damaged central directory"))?;
        let flags = le16(header, 8);
        let method = le16(header, 10);
        let crc = le32(header, 16);
        let mut compressed = u64::from(le32(header, 20));
        let mut size = u64::from(le32(header, 24));
        let name_length = le16(header, 28) as usize;
        let extra_length = le16(header, 30) as usize;
        let comment_length = le16(header, 32) as usize;
        let mut local = u64::from(le32(header, 42));
        let name = directory
            .get(at + 46..at + 46 + name_length)
            .ok_or_else(|| invalid("a damaged central directory"))?;
        // Bit 11 says UTF-8; without it the name is CP437, which for the
        // names anybody gives a game is ASCII either way.
        let name = String::from_utf8_lossy(name).into_owned();
        let extra = directory
            .get(at + 46 + name_length..at + 46 + name_length + extra_length)
            .ok_or_else(|| invalid("a damaged central directory"))?;
        zip64_sizes(extra, &mut size, &mut compressed, &mut local);
        out.push(Entry {
            name,
            method,
            flags,
            compressed,
            size,
            crc,
            local,
        });
        at += 46 + name_length + extra_length + comment_length;
    }
    Ok(out)
}

/// The zip64 extra field's values, which replace whichever of the three
/// 32-bit fields were written as `0xffffffff` — and only those, in this order.
fn zip64_sizes(extra: &[u8], size: &mut u64, compressed: &mut u64, local: &mut u64) {
    let mut at = 0;
    while at + 4 <= extra.len() {
        let id = le16(extra, at);
        let length = le16(extra, at + 2) as usize;
        let Some(field) = extra.get(at + 4..at + 4 + length) else {
            return;
        };
        if id == 0x0001 {
            let mut read = 0;
            for value in [size, compressed, local] {
                if *value == 0xffff_ffff && read + 8 <= field.len() {
                    *value = le64(field, read);
                    read += 8;
                }
            }
            return;
        }
        at += 4 + length;
    }
}

/// One entry of a zip, readable at any offset.
///
/// Forwards is cheap: the entry is inflated up to the offset asked for.
/// Backwards starts the entry again from its beginning, which a caller that
/// reads in order — as a package's own layout lets one — never pays for.
pub struct EntryReader {
    archive: PathBuf,
    entry: Entry,
    stream: Box<dyn Read + Send>,
    position: u64,
}

impl EntryReader {
    pub fn open(archive: &Path, entry: &Entry) -> io::Result<Self> {
        if !entry.readable() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "the zip is encrypted or compressed in a way this cannot read",
            ));
        }
        Ok(EntryReader {
            archive: archive.to_path_buf(),
            entry: entry.clone(),
            stream: start(archive, entry)?,
            position: 0,
        })
    }

    /// Fill `buffer` from `offset` on.
    pub fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<()> {
        if offset < self.position {
            self.stream = start(&self.archive, &self.entry)?;
            self.position = 0;
        }
        let skip = offset - self.position;
        if skip > 0 {
            let skipped = io::copy(&mut (&mut self.stream).take(skip), &mut io::sink())?;
            self.position += skipped;
            if skipped < skip {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
        }
        self.stream.read_exact(buffer)?;
        self.position += buffer.len() as u64;
        Ok(())
    }

    /// The rest of the entry, in order, as a plain reader.
    pub fn stream(&mut self) -> &mut (dyn Read + Send) {
        &mut *self.stream
    }
}

/// The entry's bytes from the start, inflated where they are deflated.
fn start(archive: &Path, entry: &Entry) -> io::Result<Box<dyn Read + Send>> {
    let mut file = File::open(archive)?;
    let mut local = [0u8; 30];
    file.seek(SeekFrom::Start(entry.local))?;
    file.read_exact(&mut local)?;
    if local[..4] != [0x50, 0x4b, 0x03, 0x04] {
        return Err(invalid(
            "a file header that is not where the directory says",
        ));
    }
    let skip = 30 + u64::from(le16(&local, 26)) + u64::from(le16(&local, 28));
    file.seek(SeekFrom::Start(entry.local + skip))?;
    let raw = BufReader::with_capacity(1 << 20, file).take(entry.compressed);
    Ok(match entry.method {
        0 => Box::new(raw),
        _ => Box::new(DeflateDecoder::new(raw)),
    })
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_string())
}

fn le16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes"))
}

fn le64(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("eight bytes"))
}

/// A zip written by hand, for the tests of this module and the ones that read
/// a package out of one. `deflate` compresses every entry.
#[cfg(test)]
pub fn build(files: &[(&str, &[u8])], deflate: bool) -> Vec<u8> {
    use flate2::write::DeflateEncoder;
    use flate2::Compression;
    use std::io::Write;

    let mut out = Vec::new();
    let mut directory = Vec::new();
    for (name, data) in files {
        let mut crc = flate2::Crc::new();
        crc.update(data);
        let body = if deflate {
            let mut encoder = DeflateEncoder::new(Vec::new(), Compression::best());
            encoder.write_all(data).unwrap();
            encoder.finish().unwrap()
        } else {
            data.to_vec()
        };
        let method: u16 = if deflate { 8 } else { 0 };
        let local = out.len() as u32;
        out.extend_from_slice(&[0x50, 0x4b, 0x03, 0x04, 20, 0, 0, 0]);
        out.extend_from_slice(&method.to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&crc.sum().to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&body);

        directory.extend_from_slice(&[0x50, 0x4b, 0x01, 0x02, 20, 0, 20, 0, 0, 0]);
        directory.extend_from_slice(&method.to_le_bytes());
        directory.extend_from_slice(&[0; 4]);
        directory.extend_from_slice(&crc.sum().to_le_bytes());
        directory.extend_from_slice(&(body.len() as u32).to_le_bytes());
        directory.extend_from_slice(&(data.len() as u32).to_le_bytes());
        directory.extend_from_slice(&(name.len() as u16).to_le_bytes());
        directory.extend_from_slice(&[0; 12]);
        directory.extend_from_slice(&local.to_le_bytes());
        directory.extend_from_slice(name.as_bytes());
    }
    let directory_at = out.len() as u32;
    out.extend_from_slice(&directory);
    out.extend_from_slice(&[0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0]);
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(directory.len() as u32).to_le_bytes());
    out.extend_from_slice(&directory_at.to_le_bytes());
    out.extend_from_slice(&[0, 0]);
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

    #[test]
    fn lists_what_is_inside() {
        let file = write(&build(
            &[
                ("game.pkg", b"package"),
                ("EP9000-NPEA00019_00-TEKKENRETAIL0000.rap", &[7; 16]),
            ],
            true,
        ));
        let listed = entries(file.path()).expect("a zip");
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].name, "game.pkg");
        assert_eq!(listed[0].size, 7);
        assert_eq!(
            listed[1].file_name(),
            "EP9000-NPEA00019_00-TEKKENRETAIL0000.rap"
        );
        assert!(listed.iter().all(Entry::readable));
    }

    #[test]
    fn reads_forwards_and_backwards() {
        let data: Vec<u8> = (0..200_000u32).map(|i| (i % 253) as u8).collect();
        for deflate in [false, true] {
            let file = write(&build(&[("big.pkg", &data)], deflate));
            let listed = entries(file.path()).expect("a zip");
            let mut reader = EntryReader::open(file.path(), &listed[0]).expect("readable");
            let mut buffer = [0u8; 10];
            for offset in [5u64, 150_000, 100, 199_990] {
                reader.read_at(offset, &mut buffer).expect("read");
                assert_eq!(
                    buffer,
                    data[offset as usize..offset as usize + 10],
                    "at {offset}"
                );
            }
            assert!(reader.read_at(199_995, &mut buffer).is_err());
        }
    }

    #[test]
    fn a_file_that_is_not_a_zip_is_refused() {
        let file = write(b"\x7fPKG not a zip at all");
        assert!(entries(file.path()).is_err());
    }
}
