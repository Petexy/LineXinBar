//! One file out of a zip archive, and nothing else.
//!
//! A libretro core is published as `<name>_libretro.so.zip`: one entry, one
//! shared object, deflated, no encryption, no zip64, no comment. That is the
//! whole of the format this reads, and everything outside it is refused rather
//! than guessed at — a core is a shared object this shell is about to hand to
//! RetroArch to `dlopen`, so half-understanding the container it arrived in is
//! not a thing to do quietly.
//!
//! Written out rather than taken from a crate for the reason the rest of this
//! binary is small: what is needed is a central directory walk, a length, and
//! an `inflate` — and `flate2` is already in this tree, doing the inflating for
//! every PNG the shell draws.
//!
//! ## Where the sizes are read from
//!
//! The **central directory**, always, and never the local header. A zip written
//! by a streaming writer sets bit 3 of the flags and leaves the local header's
//! sizes and CRC as zeroes, putting the real ones in a descriptor *after* the
//! data — where a reader that has not already read the data cannot find them.
//! The central directory has them in both cases, which is what makes one code
//! path enough.

use std::io::Read;

/// The signature at the head of each of the three structures this reads.
const LOCAL: [u8; 4] = [b'P', b'K', 3, 4];
const CENTRAL: [u8; 4] = [b'P', b'K', 1, 2];
const END: [u8; 4] = [b'P', b'K', 5, 6];

/// How far back the end record may be: its own 22 bytes, plus the longest
/// comment the format can carry.
const END_SEARCH: usize = 22 + u16::MAX as usize;

/// The one entry of `raw` whose name ends in `suffix`, uncompressed.
///
/// `Err` with a sentence for anything this cannot read, including an archive
/// with no such entry in it.
pub fn one_file(raw: &[u8], suffix: &str) -> Result<Vec<u8>, String> {
    let (_, entry) = walk(raw)?
        .into_iter()
        .find(|(name, _)| name.ends_with(suffix))
        .ok_or_else(|| format!("the archive holds no {suffix}"))?;
    read(raw, &entry)
}

/// Every file in `raw`, as its name and its bytes.
///
/// For an archive that is a *directory* rather than a single file — the system
/// files a core needs are published that way, a few hundred entries under one
/// folder. Directory entries themselves are left out: what comes back is the
/// files, and the directories are whatever their names imply.
///
/// ## The names are checked before anything is returned
///
/// This unpacks something that came off the network into a directory the
/// emulator reads, so a name is not a name until it has been looked at. An
/// entry naming an absolute path, or reaching upwards out of the directory it
/// is being written into, fails the **whole archive** rather than being
/// skipped: an archive containing one is not an archive that was built the way
/// this expects, and unpacking the rest of it and hoping is not the answer.
pub fn every_file(raw: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut out = Vec::new();
    for (name, entry) in walk(raw)? {
        if !is_safe(&name) {
            return Err(format!(
                "the archive holds a name that reaches out of it: {name}"
            ));
        }
        // A directory, which zip writes as a zero-length entry whose name ends
        // in a slash. There is nothing to write for it.
        if name.ends_with('/') {
            continue;
        }
        let bytes = read(raw, &entry)?;
        out.push((name, bytes));
    }
    Ok(out)
}

/// Whether an entry's name is one this is willing to write.
///
/// Relative, staying put, and free of the two things a Windows path can carry
/// that a zip name should not — a drive letter and a backslash, either of
/// which means something different to the code that eventually opens it.
fn is_safe(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('/')
        && !name.contains('\\')
        && !name.contains(':')
        && !name.split('/').any(|part| part == ".." || part == ".")
}

/// One entry's bytes, checked against what the listing promised.
fn read(raw: &[u8], entry: &Entry) -> Result<Vec<u8>, String> {
    // Bit 0 is "encrypted", and the rest of the header is then not what it
    // appears to be. Bit 3 — sizes in a trailing descriptor — is deliberately
    // *not* refused: the values read are the central directory's, which are
    // filled in either way.
    if entry.flags & 1 != 0 {
        return Err("the archive is encrypted".to_string());
    }
    if entry.compressed == u32::MAX as u64
        || entry.size == u32::MAX as u64
        || entry.at == u32::MAX as u64
    {
        return Err("the archive is zip64, which this cannot read".to_string());
    }
    let data = compressed(raw, entry)?;
    let out = match entry.method {
        0 => data.to_vec(),
        8 => inflate(data, entry.size)?,
        other => {
            return Err(format!(
                "the archive is compressed a way this cannot read ({other})"
            ))
        }
    };
    if out.len() as u64 != entry.size {
        return Err(format!(
            "the file came out {} bytes and the archive says {}",
            out.len(),
            entry.size
        ));
    }
    let crc = crc32(&out);
    if crc != entry.crc {
        return Err(format!(
            "the file is damaged: {crc:08x} against the archive's {:08x}",
            entry.crc
        ));
    }
    Ok(out)
}

/// One entry of the central directory, as the things a caller needs.
struct Entry {
    flags: u16,
    method: u16,
    crc: u32,
    compressed: u64,
    size: u64,
    /// Where the local header is, which is where the data is a header away
    /// from.
    at: u64,
}

/// Every entry of the central directory, with its name.
fn walk(raw: &[u8]) -> Result<Vec<(String, Entry)>, String> {
    let end = end_record(raw)?;
    let entries = u16(raw, end + 10)? as usize;
    let mut at = u32(raw, end + 16)? as usize;

    let mut out = Vec::with_capacity(entries);
    for _ in 0..entries {
        if raw.get(at..at + 46).is_none() || raw[at..at + 4] != CENTRAL {
            return Err("the archive's listing is not where it says it is".to_string());
        }
        let flags = u16(raw, at + 8)?;
        let method = u16(raw, at + 10)?;
        let crc = u32(raw, at + 16)?;
        let compressed = u32(raw, at + 20)? as u64;
        let size = u32(raw, at + 24)? as u64;
        let name_len = u16(raw, at + 28)? as usize;
        let extra_len = u16(raw, at + 30)? as usize;
        let comment_len = u16(raw, at + 32)? as usize;
        let local = u32(raw, at + 42)? as u64;
        let name = raw
            .get(at + 46..at + 46 + name_len)
            .ok_or_else(|| "the archive ends in the middle of a name".to_string())?;

        out.push((
            String::from_utf8_lossy(name).into_owned(),
            Entry {
                flags,
                method,
                crc,
                compressed,
                size,
                at: local,
            },
        ));
        at += 46 + name_len + extra_len + comment_len;
    }
    Ok(out)
}

/// The entry's bytes as they are stored, found through its own local header.
///
/// The local header's name and extra fields are read for their *lengths* only.
/// They are allowed to differ from the central directory's — a writer may put
/// alignment padding in one and not the other — so where the data begins can
/// only be worked out here.
fn compressed<'a>(raw: &'a [u8], entry: &Entry) -> Result<&'a [u8], String> {
    let at = entry.at as usize;
    if raw.get(at..at + 30).is_none() || raw[at..at + 4] != LOCAL {
        return Err("the archive's file is not where its listing says".to_string());
    }
    let name_len = u16(raw, at + 26)? as usize;
    let extra_len = u16(raw, at + 28)? as usize;
    let from = at + 30 + name_len + extra_len;
    let to = from + entry.compressed as usize;
    raw.get(from..to)
        .ok_or_else(|| "the archive ends in the middle of the file".to_string())
}

/// Deflate, into a buffer the size the archive promised.
fn inflate(data: &[u8], size: u64) -> Result<Vec<u8>, String> {
    // Sized from the listing rather than grown: a core is several megabytes and
    // this is one allocation. Capped so that a damaged archive claiming four
    // gigabytes cannot be a way to exhaust this machine's memory.
    const CEILING: u64 = 256 * 1024 * 1024;
    if size > CEILING {
        return Err(format!(
            "the file inside is {size} bytes, which is too large"
        ));
    }
    let mut out = Vec::with_capacity(size as usize);
    flate2::read::DeflateDecoder::new(data)
        .take(size)
        .read_to_end(&mut out)
        .map_err(|err| format!("the archive could not be unpacked: {err}"))?;
    Ok(out)
}

/// Where the end-of-central-directory record starts.
///
/// Searched for backwards, because the record is at the end of the file and its
/// own length depends on a comment that comes after it — so it cannot be found
/// any other way. The first match from the end wins.
fn end_record(raw: &[u8]) -> Result<usize, String> {
    if raw.len() < 22 {
        return Err("not an archive at all".to_string());
    }
    let from = raw.len().saturating_sub(END_SEARCH);
    let at = (from..=raw.len() - 22)
        .rev()
        .find(|at| raw[*at..*at + 4] == END)
        .ok_or_else(|| "not an archive: it has no listing".to_string())?;
    // One disk. Anything else is a floppy set from 1993 and is not a core.
    if u16(raw, at + 4)? != 0 || u16(raw, at + 6)? != 0 {
        return Err("the archive is split across several files".to_string());
    }
    Ok(at)
}

fn u16(raw: &[u8], at: usize) -> Result<u16, String> {
    let bytes = raw
        .get(at..at + 2)
        .ok_or_else(|| "the archive is truncated".to_string())?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn u32(raw: &[u8], at: usize) -> Result<u32, String> {
    let bytes = raw
        .get(at..at + 4)
        .ok_or_else(|| "the archive is truncated".to_string())?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// The checksum a zip carries, which is the ordinary one.
///
/// `flate2`'s own, so that nothing here has a table of its own to be wrong.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = flate2::Crc::new();
    crc.update(data);
    crc.sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// An archive of one file, written the way the buildbot's are.
    fn zipped(name: &str, content: &[u8], method: u16) -> Vec<u8> {
        let body = match method {
            0 => content.to_vec(),
            _ => {
                let mut encoder =
                    flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
                encoder.write_all(content).expect("deflate");
                encoder.finish().expect("deflate")
            }
        };
        let crc = crc32(content);
        let name = name.as_bytes();
        let mut raw = Vec::new();

        raw.extend_from_slice(&LOCAL);
        raw.extend_from_slice(&20u16.to_le_bytes()); // version
        raw.extend_from_slice(&0u16.to_le_bytes()); // flags
        raw.extend_from_slice(&method.to_le_bytes());
        raw.extend_from_slice(&0u32.to_le_bytes()); // time and date
        raw.extend_from_slice(&crc.to_le_bytes());
        raw.extend_from_slice(&(body.len() as u32).to_le_bytes());
        raw.extend_from_slice(&(content.len() as u32).to_le_bytes());
        raw.extend_from_slice(&(name.len() as u16).to_le_bytes());
        raw.extend_from_slice(&0u16.to_le_bytes()); // extra
        raw.extend_from_slice(name);
        raw.extend_from_slice(&body);

        let central = raw.len() as u32;
        raw.extend_from_slice(&CENTRAL);
        raw.extend_from_slice(&20u16.to_le_bytes()); // made by
        raw.extend_from_slice(&20u16.to_le_bytes()); // version
        raw.extend_from_slice(&0u16.to_le_bytes()); // flags
        raw.extend_from_slice(&method.to_le_bytes());
        raw.extend_from_slice(&0u32.to_le_bytes()); // time and date
        raw.extend_from_slice(&crc.to_le_bytes());
        raw.extend_from_slice(&(body.len() as u32).to_le_bytes());
        raw.extend_from_slice(&(content.len() as u32).to_le_bytes());
        raw.extend_from_slice(&(name.len() as u16).to_le_bytes());
        raw.extend_from_slice(&0u16.to_le_bytes()); // extra
        raw.extend_from_slice(&0u16.to_le_bytes()); // comment
        raw.extend_from_slice(&0u16.to_le_bytes()); // disk
        raw.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
        raw.extend_from_slice(&0u32.to_le_bytes()); // external attributes
        raw.extend_from_slice(&0u32.to_le_bytes()); // where the local header is
        raw.extend_from_slice(name);
        let listing = raw.len() as u32 - central;

        raw.extend_from_slice(&END);
        raw.extend_from_slice(&0u16.to_le_bytes()); // this disk
        raw.extend_from_slice(&0u16.to_le_bytes()); // the disk the listing is on
        raw.extend_from_slice(&1u16.to_le_bytes()); // entries here
        raw.extend_from_slice(&1u16.to_le_bytes()); // entries in all
        raw.extend_from_slice(&listing.to_le_bytes());
        raw.extend_from_slice(&central.to_le_bytes());
        raw.extend_from_slice(&0u16.to_le_bytes()); // comment
        raw
    }

    /// The shape every core is published in.
    #[test]
    fn a_deflated_core_comes_back_whole() {
        // Long enough and repetitive enough that it is actually compressed,
        // which a short string would not be.
        let core: Vec<u8> = (0..40_000u32).map(|byte| (byte % 251) as u8).collect();
        let raw = zipped("ppsspp_libretro.so", &core, 8);
        assert!(raw.len() < core.len(), "the test archive is not compressed");
        assert_eq!(one_file(&raw, "_libretro.so").expect("the core"), core);
    }

    /// And the other one the format allows, which some writers use for small
    /// files.
    #[test]
    fn a_stored_file_comes_back_whole() {
        let raw = zipped("quicknes_libretro.so", b"not really a core", 0);
        assert_eq!(
            one_file(&raw, "_libretro.so").expect("the core"),
            b"not really a core"
        );
    }

    /// A file whose bytes have been changed under a listing that still
    /// describes the original is refused. It is the one failure that would
    /// otherwise be handed to `dlopen`.
    #[test]
    fn a_damaged_file_is_refused_rather_than_unpacked() {
        let raw = zipped("x_libretro.so", b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", 0);
        let mut damaged = raw.clone();
        // Into the stored data, which is straight after the 30-byte header and
        // the name.
        let at = 30 + "x_libretro.so".len();
        damaged[at] = b'b';
        let why = one_file(&damaged, "_libretro.so").expect_err("a damaged file");
        assert!(why.contains("damaged"), "{why}");
    }

    /// Everything else is a sentence rather than a panic or a wrong answer.
    #[test]
    fn what_is_not_a_core_archive_is_refused() {
        assert!(one_file(b"", "_libretro.so").is_err());
        assert!(one_file(b"<!doctype html>", "_libretro.so").is_err());
        let raw = zipped("readme.txt", b"nothing to see", 0);
        let why = one_file(&raw, "_libretro.so").expect_err("no core in it");
        assert!(why.contains("no _libretro.so"), "{why}");
    }
}
