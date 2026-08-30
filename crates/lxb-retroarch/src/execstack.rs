//! Cores that ask for a stack they can execute, and the one byte that fixes
//! them.
//!
//! A shared object carries a `PT_GNU_STACK` program header whose flags say what
//! kind of stack it wants. Almost every object asks for a readable, writable
//! one. A few ask for a readable, writable, *executable* one — nearly always by
//! accident, because one assembly file in the build was missing the
//! `.note.GNU-stack` marker that says it needs nothing special, and the linker
//! takes silence to mean the worst.
//!
//! That used to cost nothing: the loader quietly made the whole stack
//! executable and the program ran. It does not any more. A current glibc
//! refuses, and `dlopen` fails with
//!
//! ```text
//! cannot enable executable stack as shared object requires: Invalid argument
//! ```
//!
//! ## Why this is the integration's problem
//!
//! Because of where the failure lands. libretro's build server ships melonDS
//! built this way, so the core downloads whole, passes every check this helper
//! makes — it is a valid ELF of the right size, and `ldd` finds every library
//! it names — and then fails in the dynamic linker at the moment somebody
//! presses a game. What they see is a console that scans, shelves its games,
//! offers to play one, and does nothing when asked. RetroArch on its own fails
//! identically, so there is nowhere for them to go and find out why.
//!
//! It also takes the core's settings page with it, and by the same door: the
//! options probe reaches a core through `dlopen`, so a core that will not open
//! declares nothing, and the shell reports an emulator with no settings rather
//! than an emulator it cannot load.
//!
//! One bit fixes both, and it is a bit this shell is entitled to change: the
//! cores it repairs are the ones in the user's own core directory, which is to
//! say the ones this helper downloaded. Clearing it cannot make a working core
//! stop working — an object that genuinely executed its stack would already be
//! failing to load at all, which is the state this is getting it out of.
//!
//! Nothing here parses ELF beyond the few fields it must. It reads the header,
//! walks the program headers, and writes four bytes back over one of them. A
//! file that is not an ELF, or is of a shape this does not recognise, is left
//! exactly as it was.

use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

/// `PT_GNU_STACK` — the program header whose flags describe the stack.
const PT_GNU_STACK: u32 = 0x6474_e551;

/// `PF_X`, the execute bit in a program header's flags.
const PF_X: u32 = 1;

/// Where `p_flags` sits inside one program header entry.
///
/// The two classes disagree, and getting this wrong would write four bytes over
/// something else in somebody's core — so they are written out separately
/// rather than worked out. In a 64-bit entry `p_flags` follows `p_type`
/// immediately; in a 32-bit one it is last of the first seven words, after
/// `p_type`, `p_offset`, `p_vaddr`, `p_paddr`, `p_filesz` and `p_memsz`.
const FLAGS_IN_ENTRY_64: u64 = 4;
const FLAGS_IN_ENTRY_32: u64 = 24;

/// What one look at a file found.
#[derive(Debug, PartialEq, Eq)]
pub enum Asked {
    /// It wants an executable stack, and the `p_flags` word saying so is this
    /// many bytes into the file.
    Yes(u64),
    /// It does not — or it is not a file this can read, which comes to the same
    /// thing here: nothing to do.
    No,
}

/// Whether this object asks for an executable stack, and where it says so.
///
/// `Err` only for a file that could not be read. Everything else — a file that
/// is not an ELF, an ELF of a class or byte order this does not handle, an ELF
/// with no `PT_GNU_STACK` at all — is [`Asked::No`], because the question being
/// answered is "is there one byte here worth changing", and for all of those
/// the answer is no.
pub fn asked(at: &Path) -> Result<Asked, String> {
    let mut file = std::fs::File::open(at).map_err(|err| err.to_string())?;

    // The ELF header, up to and including the program header table's shape.
    let mut header = [0u8; 64];
    if file.read_exact(&mut header).is_err() {
        // Shorter than a header, so not an ELF.
        return Ok(Asked::No);
    }
    if &header[..4] != b"\x7fELF" {
        return Ok(Asked::No);
    }
    // Little-endian only. Every machine a libretro core is built for is one,
    // and a big-endian branch here would be a byte-swap nothing could test.
    if header[5] != 1 {
        return Ok(Asked::No);
    }

    // `e_phoff`, `e_phentsize` and `e_phnum` — where the program headers are,
    // how big each is, and how many there are. Their offsets differ by class.
    let (table_at, entry_size, count, flags_in_entry) = match header[4] {
        2 => (
            u64::from_le_bytes(header[0x20..0x28].try_into().expect("eight bytes")),
            u16::from_le_bytes(header[0x36..0x38].try_into().expect("two bytes")),
            u16::from_le_bytes(header[0x38..0x3a].try_into().expect("two bytes")),
            FLAGS_IN_ENTRY_64,
        ),
        1 => (
            u64::from(u32::from_le_bytes(
                header[0x1c..0x20].try_into().expect("four bytes"),
            )),
            u16::from_le_bytes(header[0x2a..0x2c].try_into().expect("two bytes")),
            u16::from_le_bytes(header[0x2c..0x2e].try_into().expect("two bytes")),
            FLAGS_IN_ENTRY_32,
        ),
        _ => return Ok(Asked::No),
    };
    // An entry too small to hold the field about to be read out of it is a
    // table this does not understand, and reading it anyway would be reading
    // whatever follows.
    if entry_size == 0 || u64::from(entry_size) < flags_in_entry + 4 {
        return Ok(Asked::No);
    }

    for which in 0..u64::from(count) {
        let entry_at = table_at + which * u64::from(entry_size);
        if file.seek(SeekFrom::Start(entry_at)).is_err() {
            return Ok(Asked::No);
        }
        let mut kind = [0u8; 4];
        if file.read_exact(&mut kind).is_err() {
            return Ok(Asked::No);
        }
        if u32::from_le_bytes(kind) != PT_GNU_STACK {
            continue;
        }
        let flags_at = entry_at + flags_in_entry;
        if file.seek(SeekFrom::Start(flags_at)).is_err() {
            return Ok(Asked::No);
        }
        let mut flags = [0u8; 4];
        if file.read_exact(&mut flags).is_err() {
            return Ok(Asked::No);
        }
        return Ok(if u32::from_le_bytes(flags) & PF_X == 0 {
            Asked::No
        } else {
            Asked::Yes(flags_at)
        });
    }
    Ok(Asked::No)
}

/// Take the execute bit off this object's stack, if it has one.
///
/// `Ok(true)` where a byte was changed, `Ok(false)` where there was nothing to
/// change — which is the ordinary case and not a failure. `Err` for a file that
/// asks for an executable stack and could not be written, which is worth
/// saying: it is a core that will not load and now cannot be made to.
///
/// Only the four `p_flags` bytes are rewritten. Nothing is moved, nothing is
/// re-linked, and a file this declines to understand is not opened for writing
/// at all.
pub fn clear(at: &Path) -> Result<bool, String> {
    let Asked::Yes(flags_at) = asked(at)? else {
        return Ok(false);
    };
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(at)
        .map_err(|err| err.to_string())?;
    file.seek(SeekFrom::Start(flags_at))
        .map_err(|err| err.to_string())?;
    let mut flags = [0u8; 4];
    file.read_exact(&mut flags).map_err(|err| err.to_string())?;
    let cleared = u32::from_le_bytes(flags) & !PF_X;
    file.seek(SeekFrom::Start(flags_at))
        .map_err(|err| err.to_string())?;
    file.write_all(&cleared.to_le_bytes())
        .map_err(|err| err.to_string())?;
    file.flush().map_err(|err| err.to_string())?;
    Ok(true)
}

/// Repair every core in one directory, and say which ones needed it.
///
/// For the directory this helper downloads into and no other. A core in
/// `/usr/lib/libretro` belongs to the distribution's package manager, which
/// would put it back on the next update anyway, and writing there needs a root
/// this integration deliberately never asks for.
pub fn repair(cores: &Path) -> Vec<String> {
    let mut fixed = Vec::new();
    let Ok(entries) = std::fs::read_dir(cores) else {
        return fixed;
    };
    for entry in entries.flatten() {
        let file = entry.file_name();
        let Some(name) = file
            .to_str()
            .and_then(|file| file.strip_suffix(crate::find::CORE_SUFFIX))
        else {
            continue;
        };
        match clear(&entry.path()) {
            Ok(true) => {
                eprintln!("cores: {name} asked for an executable stack; taken off");
                fixed.push(name.to_string());
            }
            Ok(false) => {}
            Err(why) => eprintln!("cores: {name} asks for an executable stack and will not load, and could not be repaired: {why}"),
        }
    }
    fixed.sort();
    fixed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A little-endian ELF with one `PT_GNU_STACK` program header carrying
    /// `flags`, of the given class.
    ///
    /// Built by hand rather than checked in as a fixture: what is being tested
    /// is that two structure layouts are read at the right offsets, and a
    /// binary blob in the repository would prove that for whichever one it
    /// happened to be.
    fn elf(class: u8, flags: u32) -> Vec<u8> {
        let (header_size, entry_size, flags_in_entry) = match class {
            2 => (64usize, 56usize, 4usize),
            _ => (52usize, 32usize, 24usize),
        };
        let table_at = header_size;
        let mut bytes = vec![0u8; header_size + entry_size * 2];
        bytes[..4].copy_from_slice(b"\x7fELF");
        bytes[4] = class;
        // Little-endian.
        bytes[5] = 1;
        if class == 2 {
            bytes[0x20..0x28].copy_from_slice(&(table_at as u64).to_le_bytes());
            bytes[0x36..0x38].copy_from_slice(&(entry_size as u16).to_le_bytes());
            bytes[0x38..0x3a].copy_from_slice(&2u16.to_le_bytes());
        } else {
            bytes[0x1c..0x20].copy_from_slice(&(table_at as u32).to_le_bytes());
            bytes[0x2a..0x2c].copy_from_slice(&(entry_size as u16).to_le_bytes());
            bytes[0x2c..0x2e].copy_from_slice(&2u16.to_le_bytes());
        }
        // A first program header that is something else entirely, so that the
        // walk has to actually look at `p_type` rather than take the first one.
        bytes[table_at..table_at + 4].copy_from_slice(&1u32.to_le_bytes());
        let second = table_at + entry_size;
        bytes[second..second + 4].copy_from_slice(&PT_GNU_STACK.to_le_bytes());
        bytes[second + flags_in_entry..second + flags_in_entry + 4]
            .copy_from_slice(&flags.to_le_bytes());
        bytes
    }

    fn written(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lxb-execstack-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let at = dir.join(name);
        std::fs::write(&at, bytes).unwrap();
        at
    }

    /// The bit is found in a 64-bit object, and taking it off leaves the rest
    /// of the file exactly as it was.
    ///
    /// The second half is the whole risk of this module: it writes into
    /// somebody's core, and a write at the wrong offset would corrupt an
    /// emulator rather than repair one.
    #[test]
    fn the_execute_bit_comes_off_a_64_bit_core_and_nothing_else_moves() {
        // RWE, which is what libretro's melonDS ships as.
        let before = elf(2, 7);
        let at = written("sixty-four.so", &before);

        assert_eq!(asked(&at).unwrap(), Asked::Yes(64 + 56 + 4));
        assert!(clear(&at).unwrap(), "it had the bit and it came off");

        let after = std::fs::read(&at).unwrap();
        assert_eq!(after.len(), before.len(), "the file did not change size");
        let flags = 64 + 56 + 4;
        assert_eq!(
            u32::from_le_bytes(after[flags..flags + 4].try_into().unwrap()),
            6,
            "RWE became RW"
        );
        assert_eq!(
            after[..flags],
            before[..flags],
            "everything before the flags is untouched"
        );
        assert_eq!(
            after[flags + 4..],
            before[flags + 4..],
            "and everything after them"
        );

        // And it is now a file with nothing to do, which is what makes a second
        // run of the repair cost nothing.
        assert_eq!(asked(&at).unwrap(), Asked::No);
        assert!(!clear(&at).unwrap());
        let _ = std::fs::remove_file(&at);
    }

    /// The 32-bit layout puts `p_flags` somewhere else entirely, and is read
    /// there.
    #[test]
    fn a_32_bit_object_has_its_flags_in_the_other_place() {
        let at = written("thirty-two.so", &elf(1, 7));
        assert_eq!(asked(&at).unwrap(), Asked::Yes(52 + 32 + 24));
        assert!(clear(&at).unwrap());
        let after = std::fs::read(&at).unwrap();
        let flags = 52 + 32 + 24;
        assert_eq!(
            u32::from_le_bytes(after[flags..flags + 4].try_into().unwrap()),
            6
        );
        let _ = std::fs::remove_file(&at);
    }

    /// A core that asks for an ordinary stack is not touched, and neither is a
    /// file that is not an ELF at all.
    ///
    /// The second one matters more than it looks: this walks a directory and
    /// opens what it finds, and a directory somebody has put a text file in
    /// must not become a directory with a corrupted text file in it.
    #[test]
    fn nothing_is_written_to_a_core_that_never_asked() {
        let ordinary = written("ordinary.so", &elf(2, 6));
        assert_eq!(asked(&ordinary).unwrap(), Asked::No);
        assert!(!clear(&ordinary).unwrap());

        let not_elf = written("notes.txt", b"this is not a core");
        assert_eq!(asked(&not_elf).unwrap(), Asked::No);
        assert!(!clear(&not_elf).unwrap());
        assert_eq!(
            std::fs::read(&not_elf).unwrap(),
            b"this is not a core",
            "a file that is not an ELF is left alone"
        );

        // Nor is a file too short to hold a header, which is what a download
        // that was interrupted looks like.
        let stub = written("half.so", b"\x7fELF");
        assert_eq!(asked(&stub).unwrap(), Asked::No);
        assert!(!clear(&stub).unwrap());

        let _ = std::fs::remove_file(&ordinary);
        let _ = std::fs::remove_file(&not_elf);
        let _ = std::fs::remove_file(&stub);
    }

    /// The repair walks a directory, fixes the cores that need it, names them,
    /// and leaves everything else alone — including files that are not cores.
    #[test]
    fn repairing_a_directory_names_only_what_it_changed() {
        let dir = std::env::temp_dir().join(format!("lxb-execstack-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("melonds_libretro.so"), elf(2, 7)).unwrap();
        std::fs::write(dir.join("mesen_libretro.so"), elf(2, 6)).unwrap();
        // Not a core, and so not even looked at.
        std::fs::write(dir.join("readme.txt"), elf(2, 7)).unwrap();

        assert_eq!(repair(&dir), ["melonds"]);
        // And a second pass has nothing left to do, so a scan that runs every
        // time the shell starts is not rewriting cores every time.
        assert!(repair(&dir).is_empty());

        assert_eq!(
            asked(&dir.join("readme.txt")).unwrap(),
            Asked::Yes(64 + 56 + 4),
            "a file that is not a core was not repaired"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
