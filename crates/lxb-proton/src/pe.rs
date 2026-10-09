//! What a Windows program says about itself, read out of the executable:
//! the name it calls itself, and the drawing it wears.
//!
//! Two questions, asked of one file: what should this game be called on the
//! bar, and what should its row look like? Everything else about a `.exe` is
//! Wine's business.
//!
//! Without the first, a game is named after the file somebody downloaded,
//! and those are named for a download rather than for a person:
//! `npp.8.9.8.Installer.x64` is a real one, and it would have stood in the
//! Windows Apps column for ever, because nothing in this shell renames a
//! game afterwards. The same executable carries `Notepad++` inside it, which
//! is what its own author called it — and carries its own icon beside it,
//! which is what its author drew.
//!
//! ## What is read
//!
//! The `VS_VERSIONINFO` resource — the thing Windows shows on a file's
//! Details tab — and two of its fields: `ProductName`, then
//! `FileDescription` where there is no product name. And the icon group,
//! which [`crate::icon`] turns into a picture. Nothing else in the file is
//! looked at, and nothing is ever executed.
//!
//! ## This file is not ours
//!
//! It is an arbitrary executable somebody picked off their disk, so it is
//! read the way anything untrusted is read: every offset is checked against
//! the length before it is used, every walk is depth- and count-capped,
//! nothing is seeked to that was not found inside a structure already
//! bounds-checked, and the whole of it returns `None` rather than failing.
//! There is no panic path — no slicing by range, no unwrap on arithmetic —
//! and the only thing that escapes is a short string with its control
//! characters taken out.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// How much of a resource section is worth reading.
///
/// Version information is a few hundred bytes and would fit in a page. The
/// icons are what makes this large: a program's drawing may be a quarter of
/// a megabyte on its own, it sits wherever the linker put it rather than at
/// the front, and a section is read once and then not again — see
/// [`crate::icon::beside`], which writes its answer out and never asks
/// twice. Sixteen is past every executable this was measured against and
/// still a bound a file cannot talk its way out of.
const MOST_OF_A_RESOURCE_SECTION: u32 = 16 * 1024 * 1024;

/// How deep the resource tree is walked. Three is the whole of it — type,
/// name, language — and anything deeper is a file lying about its shape.
const DEEPEST: u8 = 3;

/// How many entries are read at one level of the tree. A real executable
/// has a handful; the cap is for one claiming to have thousands.
const MOST_ENTRIES: u16 = 512;

/// The resource type that holds version information.
const RT_VERSION: u32 = 16;

/// One image of one icon, at one size and one depth.
pub const RT_ICON: u32 = 3;

/// The directory that says which [`RT_ICON`] images belong to one icon, and
/// what size each of them is. This is what a program's "icon" actually is:
/// a program carries several drawings of itself and Windows picks by the
/// room it has.
pub const RT_GROUP_ICON: u32 = 14;

/// The longest name worth putting on a row. Longer than any real product
/// name and short enough that a file full of text cannot hand the shell a
/// megabyte to draw.
const LONGEST_NAME: usize = 80;

/// What this executable calls itself, or `None` where it says nothing a
/// person would want to read.
///
/// `ProductName` first — "Notepad++" — then `FileDescription`, which is
/// what a program with no product name usually fills in instead.
pub fn product_name(at: &Path) -> Option<String> {
    let block = Reader::of(at)?.resource(RT_VERSION, None)?.to_vec();
    let fields = strings_in(&block);
    for wanted in ["ProductName", "FileDescription"] {
        if let Some(found) = fields
            .iter()
            .find(|(key, _)| key == wanted)
            .and_then(|(_, value)| worth_showing(value))
        {
            return Some(found);
        }
    }
    None
}

/// A name with nothing in it a row cannot draw, or `None` where what was
/// read is not a name at all.
fn worth_showing(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .map(|c| match c.is_control() {
            true => ' ',
            false => c,
        })
        .collect();
    let mut name = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.chars().count() > LONGEST_NAME {
        name = name.chars().take(LONGEST_NAME).collect::<String>();
        name = name.trim_end().to_string();
    }
    // A name of nothing but punctuation is not a name. The caller falls
    // back to the filename, which at least came from somebody's disk.
    let has_a_letter = name.chars().any(char::is_alphanumeric);
    (!name.is_empty() && has_a_letter).then_some(name)
}

// --- finding the block ------------------------------------------------------

fn read_at(file: &mut File, offset: u64, length: usize) -> Option<Vec<u8>> {
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut buffer = vec![0u8; length];
    file.read_exact(&mut buffer).ok()?;
    Some(buffer)
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    let pair = bytes.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes([pair[0], pair[1]]))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    let four = bytes.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([four[0], four[1], four[2], four[3]]))
}

/// One section's place in the file, for turning an address the headers use
/// into an offset something can be read from.
struct Section {
    virtual_address: u32,
    virtual_size: u32,
    raw_offset: u32,
    raw_size: u32,
}

/// Where a resource address lands in the file.
fn offset_of(sections: &[Section], address: u32) -> Option<(u64, u32)> {
    sections.iter().find_map(|section| {
        let past = address.checked_sub(section.virtual_address)?;
        // Inside what the section says it holds *and* inside what the file
        // actually carries for it. A section can declare more than it
        // stores, and the difference is zeroes that are not in the file.
        if past >= section.virtual_size || past >= section.raw_size {
            return None;
        }
        let at = u64::from(section.raw_offset).checked_add(u64::from(past))?;
        Some((at, section.raw_size - past))
    })
}

/// An executable's resources, read once.
///
/// Once, and not once per question: an icon is two lookups — the group that
/// says which drawing to take, and then that drawing — and a program's own
/// name is a third. Each of them is a walk of a tree that is already in
/// hand, so the file is opened and read here and nothing touches the disk
/// again.
pub struct Reader {
    bytes: Vec<u8>,
    /// The address the first byte of `bytes` has in the headers' terms.
    base: u32,
}

impl Reader {
    /// Read one executable's resource section, or `None` for a file that
    /// is not a Windows executable, or has no resources, or is damaged.
    pub fn of(at: &Path) -> Option<Reader> {
        let mut file = File::open(at).ok()?;
        resource_section(&mut file)
    }

    /// One resource's bytes: the first of `kind`, or the one of `kind`
    /// carrying `name` where a name is asked for — which is how the image
    /// an icon group points at is found.
    pub fn resource(&self, kind: u32, name: Option<u32>) -> Option<&[u8]> {
        let (address, length) = walk(self, &Wanted { kind, name }, 0, 0)?;
        let past = address.checked_sub(self.base)? as usize;
        let end = past.checked_add(length as usize)?.min(self.bytes.len());
        self.bytes.get(past..end)
    }
}

/// Which resource a walk is looking for.
struct Wanted {
    /// Matched at the top of the tree, where the types are.
    kind: u32,
    /// Matched one level down, where the names are. `None` takes the first
    /// of its type, which is what a file with two version resources
    /// deserves.
    name: Option<u32>,
}

/// Read the whole resource section, with the address its first byte has.
fn resource_section(file: &mut File) -> Option<Reader> {
    let front = read_at(file, 0, 0x40)?;
    if front.get(..2)? != b"MZ" {
        return None;
    }
    let pe_at = u64::from(u32_at(&front, 0x3c)?);
    // The COFF header is twenty bytes after the signature, and the optional
    // header follows it; its size is in the COFF header.
    let coff = read_at(file, pe_at, 24)?;
    if coff.get(..4)? != b"PE\0\0" {
        return None;
    }
    let sections_count = u16_at(&coff, 6)?;
    let optional_size = u16_at(&coff, 20)?;
    if sections_count == 0 || optional_size < 2 {
        return None;
    }
    let optional_at = pe_at.checked_add(24)?;
    let optional = read_at(file, optional_at, usize::from(optional_size))?;

    // The data directory sits at a different place in the two shapes, and
    // the magic is what says which: 0x10b is PE32, 0x20b is PE32+.
    let directories_at = match u16_at(&optional, 0)? {
        0x10b => 96usize,
        0x20b => 112usize,
        _ => return None,
    };
    // Entry two of the data directory is the resource table.
    let resource_address = u32_at(&optional, directories_at.checked_add(8 * 2)?)?;
    if resource_address == 0 {
        return None;
    }

    let table_at = optional_at.checked_add(u64::from(optional_size))?;
    let table = read_at(file, table_at, usize::from(sections_count) * 40)?;
    let mut sections = Vec::new();
    for index in 0..usize::from(sections_count) {
        let row = index.checked_mul(40)?;
        sections.push(Section {
            virtual_size: u32_at(&table, row.checked_add(8)?)?,
            virtual_address: u32_at(&table, row.checked_add(12)?)?,
            raw_size: u32_at(&table, row.checked_add(16)?)?,
            raw_offset: u32_at(&table, row.checked_add(20)?)?,
        });
    }

    let (at, available) = offset_of(&sections, resource_address)?;
    let length = available.min(MOST_OF_A_RESOURCE_SECTION) as usize;
    Some(Reader {
        bytes: read_at(file, at, length)?,
        base: resource_address,
    })
}

/// Walk the resource tree to what is wanted, and answer with its address
/// and length.
fn walk(resources: &Reader, wanted: &Wanted, directory: usize, depth: u8) -> Option<(u32, u32)> {
    if depth > DEEPEST {
        return None;
    }
    let bytes = &resources.bytes;
    let named = u16_at(bytes, directory.checked_add(12)?)?;
    let numbered = u16_at(bytes, directory.checked_add(14)?)?;
    let entries = named.checked_add(numbered)?.min(MOST_ENTRIES);
    for index in 0..entries {
        let at = directory
            .checked_add(16)?
            .checked_add(usize::from(index).checked_mul(8)?)?;
        let name = u32_at(bytes, at)?;
        let offset = u32_at(bytes, at.checked_add(4)?)?;
        // The top of the tree is the types, and only the one wanted is
        // worth walking into. One level down are the names, which matter
        // only for an icon: a group names the image it wants, and taking
        // the first image in the file instead would draw whichever
        // drawing the linker happened to put first. Below that are the
        // languages, where the first will do.
        let numbered_name = name & 0x8000_0000 == 0;
        if depth == 0 && (!numbered_name || name != wanted.kind) {
            continue;
        }
        if depth == 1 {
            if let Some(asked) = wanted.name {
                if !numbered_name || name != asked {
                    continue;
                }
            }
        }
        match offset & 0x8000_0000 == 0 {
            // A leaf: the entry points at a description of the bytes.
            true => {
                let leaf = offset as usize;
                let address = u32_at(bytes, leaf)?;
                let length = u32_at(bytes, leaf.checked_add(4)?)?;
                if length == 0 {
                    continue;
                }
                return Some((address, length));
            }
            // Another directory, at an offset from the start of the tree.
            false => {
                let next = (offset & 0x7fff_ffff) as usize;
                if next >= bytes.len() {
                    continue;
                }
                if let Some(found) = walk(resources, wanted, next, depth.checked_add(1)?) {
                    return Some(found);
                }
            }
        }
    }
    None
}

// --- reading the block ------------------------------------------------------

/// Every key and value in a `VS_VERSIONINFO` block.
///
/// Walked structurally rather than scanned for: the same words appear in an
/// installer's own string tables — a scan of `npp.8.9.8.Installer.x64.exe`
/// for `ProductName` finds one belonging to the installer's script long
/// before the one in its version resource.
fn strings_in(block: &[u8]) -> Vec<(String, String)> {
    let mut found = Vec::new();
    // The outermost node is VS_VERSION_INFO; its children are
    // StringFileInfo and VarFileInfo, and a StringFileInfo's children are
    // string tables whose children are the pairs. Three levels of the same
    // shape, so one walk does all of it.
    gather(block, 0, block.len(), 0, &mut found);
    found
}

fn gather(block: &[u8], at: usize, end: usize, depth: u8, out: &mut Vec<(String, String)>) {
    if depth > 4 || out.len() > 64 {
        return;
    }
    let mut cursor = at;
    while cursor + 6 <= end {
        let Some(length) = u16_at(block, cursor) else {
            return;
        };
        let Some(value_length) = u16_at(block, cursor + 2) else {
            return;
        };
        let Some(kind) = u16_at(block, cursor + 4) else {
            return;
        };
        if length < 6 {
            return;
        }
        let node_end = match cursor.checked_add(usize::from(length)) {
            Some(node_end) if node_end <= end => node_end,
            // A node claiming to run past its parent is a node to stop at.
            _ => return,
        };
        let Some((key, after_key)) = wide_string(block, cursor + 6, node_end) else {
            return;
        };
        let body = round_up(after_key);
        // `wValueLength` counts *characters* for a text node and *bytes*
        // for a binary one. Getting that wrong is not a wrong length, it is
        // the wrong place: the outermost node is binary — its value is the
        // fixed version numbers — and its children come after that value.
        // Walking from the value instead reads the version numbers as
        // though they were nodes and finds nothing at all.
        let value_bytes = match kind {
            1 => usize::from(value_length).saturating_mul(2),
            _ => usize::from(value_length),
        };
        // Type 1 with a length is text: a key and its value, which is what
        // is being looked for.
        if kind == 1 && value_length > 0 {
            let value_end = body
                .checked_add(value_bytes)
                .unwrap_or(node_end)
                .min(node_end);
            if let Some((value, _)) = wide_string(block, body, value_end) {
                out.push((key, value));
            }
        }
        // And every node may carry children after its value, which is where
        // StringFileInfo, its tables and their pairs all live.
        let children = round_up(body.saturating_add(value_bytes));
        if children < node_end {
            gather(block, children, node_end, depth + 1, out);
        }
        // Nodes are four-byte aligned, and a node that did not advance the
        // cursor would be a loop.
        let next = round_up(node_end);
        if next <= cursor {
            return;
        }
        cursor = next;
    }
}

fn round_up(at: usize) -> usize {
    at.checked_add(3).map_or(at, |padded| padded & !3)
}

/// One NUL-terminated UTF-16 string, and where it ended.
fn wide_string(block: &[u8], at: usize, end: usize) -> Option<(String, usize)> {
    let mut units = Vec::new();
    let mut cursor = at;
    while cursor + 2 <= end {
        let unit = u16_at(block, cursor)?;
        cursor += 2;
        if unit == 0 {
            break;
        }
        if units.len() > 512 {
            break;
        }
        units.push(unit);
    }
    Some((String::from_utf16_lossy(&units), cursor))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `VS_VERSIONINFO` node: length, value length, type, key, then a
    /// value or children. Built here rather than copied out of a real file,
    /// so the test says what shape it is testing.
    fn node(key: &str, kind: u16, value: Option<&str>, children: Vec<u8>) -> Vec<u8> {
        let mut body = Vec::new();
        let value_length = match value {
            // A text value's length is counted in characters, including its
            // terminator — the trap this whole walk turned on.
            Some(text) => {
                for unit in text.encode_utf16().chain(std::iter::once(0)) {
                    body.extend_from_slice(&unit.to_le_bytes());
                }
                (text.encode_utf16().count() + 1) as u16
            }
            None => 0,
        };
        while body.len() % 4 != 0 {
            body.push(0);
        }
        body.extend_from_slice(&children);

        let mut head = Vec::new();
        head.extend_from_slice(&0u16.to_le_bytes());
        head.extend_from_slice(&value_length.to_le_bytes());
        head.extend_from_slice(&kind.to_le_bytes());
        for unit in key.encode_utf16().chain(std::iter::once(0)) {
            head.extend_from_slice(&unit.to_le_bytes());
        }
        while head.len() % 4 != 0 {
            head.push(0);
        }

        let mut whole = head;
        whole.extend_from_slice(&body);
        let length = whole.len() as u16;
        whole[0..2].copy_from_slice(&length.to_le_bytes());
        while whole.len() % 4 != 0 {
            whole.push(0);
        }
        whole
    }

    /// A whole version block of the shape a real executable has: a binary
    /// outermost node carrying the fixed version numbers, with the strings
    /// two levels under it and *after* that value.
    fn a_version_block(pairs: &[(&str, &str)]) -> Vec<u8> {
        let mut strings = Vec::new();
        for (key, value) in pairs {
            strings.extend_from_slice(&node(key, 1, Some(value), Vec::new()));
        }
        let table = node("040904b0", 1, None, strings);
        let info = node("StringFileInfo", 1, None, table);
        let mut outer = node("VS_VERSION_INFO", 0, None, Vec::new());
        outer[2..4].copy_from_slice(&52u16.to_le_bytes());
        outer.extend_from_slice(&[0u8; 52]);
        outer.extend_from_slice(&info);
        let length = outer.len() as u16;
        outer[0..2].copy_from_slice(&length.to_le_bytes());
        outer
    }

    fn pick(fields: &[(String, String)]) -> Option<String> {
        for wanted in ["ProductName", "FileDescription"] {
            if let Some(found) = fields
                .iter()
                .find(|(key, _)| key == wanted)
                .and_then(|(_, value)| worth_showing(value))
            {
                return Some(found);
            }
        }
        None
    }

    /// The fields are read from where they really live: under
    /// StringFileInfo, under a language table, past the fixed version
    /// numbers the outermost node carries.
    ///
    /// Walking from that value instead of past it finds nothing at all,
    /// which is what this did on the first real executable it was given.
    #[test]
    fn the_strings_are_found_under_the_version_numbers_not_inside_them() {
        let block = a_version_block(&[
            ("CompanyName", "Don HO"),
            ("ProductName", "Notepad++"),
            ("FileDescription", "a free source code editor"),
        ]);
        let found = strings_in(&block);
        assert_eq!(
            found
                .iter()
                .find(|(key, _)| key == "ProductName")
                .map(|(_, value)| value.as_str()),
            Some("Notepad++"),
        );
        assert_eq!(found.len(), 3, "and every pair, not only the first");
    }

    /// `ProductName` is what a program calls itself; `FileDescription` is
    /// what one with no product name fills in instead.
    #[test]
    fn a_product_name_is_preferred_and_a_description_is_the_fallback() {
        let both = a_version_block(&[
            ("FileDescription", "a free source code editor"),
            ("ProductName", "Notepad++"),
        ]);
        assert_eq!(pick(&strings_in(&both)).as_deref(), Some("Notepad++"));

        let only_description = a_version_block(&[("FileDescription", "Notepad++")]);
        assert_eq!(
            pick(&strings_in(&only_description)).as_deref(),
            Some("Notepad++"),
        );
    }

    /// Nothing is trusted about these bytes: they are an executable
    /// somebody picked off their own disk. Every one of these is a file
    /// that must come back as nothing rather than as a panic.
    #[test]
    fn a_file_that_is_not_what_it_says_comes_back_as_nothing() {
        assert!(strings_in(&[]).is_empty());
        assert!(strings_in(&[0, 0]).is_empty());
        // A node claiming to be longer than the block holding it.
        assert!(strings_in(&[0xff, 0xff, 0, 0, 1, 0, b'A', 0, 0, 0]).is_empty());
        // A node of length zero, which would otherwise never advance.
        assert!(strings_in(&[0, 0, 0, 0, 0, 0, 0, 0]).is_empty());
        // And every truncation of a real one.
        let whole = a_version_block(&[("ProductName", "Notepad++")]);
        for cut in 0..whole.len() {
            let _ = strings_in(&whole[..cut]);
        }
    }

    /// Every single-byte corruption of a real block, and a sweep of
    /// scrambled ones. None of them may panic, hang or return a name they
    /// invented — this reads a file somebody picked off their own disk.
    ///
    /// Deterministic on purpose: a failure here has to be reproducible, so
    /// the scrambling is a fixed sequence rather than a random one.
    #[test]
    fn no_corruption_of_a_block_can_make_this_fall_over() {
        let whole = a_version_block(&[("ProductName", "Notepad++")]);

        // Every byte, set to each of the values most likely to break a
        // length or a type: zero, one, and the two all-ones widths.
        for at in 0..whole.len() {
            for byte in [0x00u8, 0x01, 0x7f, 0xff] {
                let mut broken = whole.clone();
                broken[at] = byte;
                let _ = strings_in(&broken);
            }
        }

        // And whole blocks of nonsense, from a fixed sequence.
        let mut seed = 0x1234_5678u32;
        let mut next = move || {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            (seed >> 16) as u8
        };
        for length in [0usize, 1, 7, 8, 64, 512] {
            for _ in 0..32 {
                let noise: Vec<u8> = (0..length).map(|_| next()).collect();
                let _ = strings_in(&noise);
            }
        }
    }

    /// What reaches a row has had its control characters taken out, is
    /// capped, and is nothing at all when it holds no letters or digits.
    #[test]
    fn a_name_is_cleaned_before_anybody_reads_it() {
        assert_eq!(worth_showing("Notepad++").as_deref(), Some("Notepad++"));
        assert_eq!(
            worth_showing("  Notepad\t\u{1}++  \n").as_deref(),
            Some("Notepad ++"),
            "a control character becomes a space and the run is collapsed"
        );
        assert_eq!(worth_showing("").as_deref(), None);
        assert_eq!(worth_showing("   ").as_deref(), None);
        assert_eq!(
            worth_showing("---").as_deref(),
            None,
            "a name of nothing but punctuation is not a name; the caller \
             falls back to the filename"
        );
        let long = "A".repeat(500);
        assert_eq!(
            worth_showing(&long).map(|name| name.chars().count()),
            Some(LONGEST_NAME),
        );
    }

    /// A file that is not an executable at all is simply not one, and
    /// neither is a file that is not there.
    #[test]
    fn something_that_is_not_an_executable_says_nothing() {
        let home = tempfile::tempdir().unwrap();
        let at = home.path().join("notes.txt");
        std::fs::write(&at, b"this is not a program").unwrap();
        assert_eq!(product_name(&at), None);
        assert_eq!(product_name(&home.path().join("nothing-here.exe")), None);
    }

    /// Read a real executable, when one is named. Nothing in this repository
    /// ships a `.exe`, and nothing here may depend on a file on one
    /// developer's disk — so this is opt-in and skips in silence:
    ///
    /// ```text
    /// LXB_PROTON_TEST_EXE=/path/to/a.exe cargo test -p lxb-proton -- --nocapture
    /// ```
    #[test]
    fn a_real_executable_says_what_it_is_called() {
        let Some(at) = std::env::var_os("LXB_PROTON_TEST_EXE") else {
            return;
        };
        let at = std::path::PathBuf::from(at);
        if let Some(block) =
            Reader::of(&at).and_then(|read| read.resource(RT_VERSION, None).map(<[u8]>::to_vec))
        {
            for (key, value) in strings_in(&block) {
                println!("   {key} = {value:?}");
            }
        }
        println!("{} -> {:?}", at.display(), product_name(&at));
    }
}
