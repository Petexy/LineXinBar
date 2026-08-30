//! What a core needs beside itself that nobody is allowed to fetch for it.
//!
//! A core downloading cleanly is not the same as a game being playable. Several
//! emulators cannot start at all without a copy of the original machine's boot
//! ROM — a PlayStation 2 has no software of its own until its BIOS is there,
//! and neither has an Amiga without a Kickstart. Those files are the console
//! maker's, they are not redistributable, and the only lawful copy is one
//! dumped from hardware somebody owns. So this integration will never fetch
//! them, RetroArch's own updater will not either, and the whole of what can be
//! done is to *say so* — before a press, rather than by starting an emulator
//! that exits half a second later with nothing on the screen.
//!
//! That last is what this module is for. Without it the shell offers to
//! download a core, downloads it, marks the row ready, and then the game does
//! not start, and there is nothing anywhere the user can read to find out why.
//!
//! ## Why nothing here is a list of BIOS files
//!
//! It is tempting to write a table: PlayStation 2 wants `SCPH*.bin`, Saturn
//! wants `sega_101.bin`, and so on. Every such table is wrong within a year and
//! wrong immediately for the cores whose author changed their mind. libretro
//! already publishes the answer, per core, in the `<core>_libretro.info` files
//! that ship beside the cores themselves:
//!
//! ```text
//! firmware_count = 2
//! firmware0_desc = "'pcsx2/bios' folder"
//! firmware0_path = "pcsx2/bios"
//! firmware0_opt = "false"
//! ```
//!
//! `_path` is relative to RetroArch's system folder and `_opt` says whether the
//! core can manage without. Reading that is the difference between this helper
//! knowing what a core wants and this helper *guessing*, and the distinction
//! matters more here than almost anywhere: a wrong file name in a panel is the
//! shell telling somebody to go and find something that would not have helped.
//!
//! The declarations are also better curated than a first look suggests. The
//! Famicom Disk System's `disksys.rom` is marked optional under `mesen`,
//! because a Mesen with no disk drive still plays cartridges; the PC Engine's
//! system cards are optional under `mednafen_pce` for the same reason. Only
//! what genuinely stops a core dead is `_opt = "false"`.

use std::path::Path;

use crate::report::Need;

/// What a core reads beside itself, and whether this machine has each of them.
///
/// Everything it declares, required or optional, present or not — see
/// [`Need::here`]. A core that declares nothing and a machine whose RetroArch
/// shipped without libretro's descriptions both answer with nothing, which is
/// the same thing they have always answered.
///
/// **Optional firmware is in here too, and that is deliberate.** It was left
/// out once, on the reasoning that a core which starts without a file is not
/// one anybody has to be told about — and that is still true of *warning*
/// somebody. It is not true of offering. melonDS declares eight files and calls
/// every one of them optional, because its own high-level BIOS plays most
/// Nintendo DS games; somebody who owns a Nintendo DS and dumped its firmware
/// wants those files used, and leaving them out meant the Nintendo DS page had
/// nowhere to put them. Which of these actually stops a game is not decided
/// here and is not decided in advance at all — see the shell's
/// `say_needs_bios`, which is raised by a game that has already failed to
/// start.
pub fn declared(info: &Path, system: &Path, core: &str) -> Vec<Need> {
    let Ok(text) = std::fs::read_to_string(info.join(format!("{core}_libretro.info"))) else {
        return Vec::new();
    };
    let count: usize = match value(&text, "firmware_count").and_then(|n| n.parse().ok()) {
        Some(count) => count,
        None => return Vec::new(),
    };

    (0..count)
        .filter_map(|which| {
            let path = value(&text, &format!("firmware{which}_path"))?;
            Some(Need {
                note: value(&text, &format!("firmware{which}_desc"))
                    .unwrap_or_else(|| path.clone()),
                here: here(&system.join(&path)),
                path,
            })
        })
        .collect()
}

/// Whether what a declaration asks for is actually there.
///
/// A firmware entry is usually one file, and then this is only whether it
/// exists. It is sometimes a *folder* — `pcsx2/bios` is the case this was
/// written for — and a folder that exists and is empty is the one shape that
/// looks satisfied and is not: the core makes it on its first run and then
/// stops, which is exactly the failure this whole module is about.
///
/// Nor is a folder with *something* in it satisfied. Somebody who points the
/// shell's picker at the wrong folder of theirs gets that folder's contents
/// copied into the console's firmware directory, and a check that asked only
/// whether anything was in there would then call the machine set up and leave
/// the emulator starting to a black screen with nowhere on the bar saying why.
/// So the question is whether anything in it could be a boot ROM at all — see
/// [`could_be_firmware`].
fn here(at: &Path) -> bool {
    match std::fs::metadata(at) {
        Ok(what) if what.is_dir() => std::fs::read_dir(at)
            .map(|entries| {
                entries
                    .flatten()
                    .any(|entry| could_be_firmware(&entry.path()))
            })
            .unwrap_or(false),
        Ok(_) => true,
        Err(_) => false,
    }
}

/// The largest file worth treating as a firmware dump.
///
/// A guard rather than a limit anybody is expected to meet: a PlayStation 2
/// BIOS is about four megabytes and a Saturn's is half of one. What it stops is
/// a folder of somebody's disc images reading as a folder of boot ROMs.
pub const BIGGEST: u64 = 64 * 1024 * 1024;

/// Whether a file could be a console's boot ROM.
///
/// Asked only where a declaration names a *folder* rather than a file, which in
/// the whole of libretro's collection is one core: pcsx2 reads every BIOS it
/// finds in `pcsx2/bios` and lets the user pick a region in its own menu, so
/// there is no name to match on and no other way to tell a dump from whatever
/// else happens to be in the folder somebody pointed at. Everywhere else the
/// declaration names the file, and a name is a better test than any of this.
///
/// It is deliberately a test for what a dump *is not*. A boot ROM is an opaque
/// blob and there is nothing positive to look for that would not be a table of
/// consoles, which this module exists to avoid — see the note at the top. So
/// what is refused is the things a blob demonstrably is not: an archive, a
/// document, a picture, a recording, a program, and text of any kind. A folder
/// of holiday photographs then reads as empty, which is the answer, and a
/// four-byte `.mec` file pcsx2 wrote beside its own BIOS still reads as a file,
/// which is also the answer — it costs nothing to copy and this is not the
/// place to decide what an emulator wants beside its firmware.
///
/// The shell has its own copy of this, because it is the half that does the
/// copying and the two packages deliberately do not depend on each other — see
/// `lxb-desktop/src/retroarch.rs`. They have to agree: a file this refuses and
/// that one copies is a folder that fills up and never counts as filled.
pub fn could_be_firmware(at: &Path) -> bool {
    let Ok(about) = std::fs::metadata(at) else {
        return false;
    };
    if !about.is_file() || about.len() == 0 || about.len() > BIGGEST {
        return false;
    }
    let mut head = [0u8; 512];
    let Ok(read) = std::fs::File::open(at).and_then(|mut file| {
        use std::io::Read;
        file.read(&mut head)
    }) else {
        return false;
    };
    let head = &head[..read];
    !something_else(head) && !plain_text(head)
}

/// Whether what a file begins with says it is one of the kinds of thing a boot
/// ROM is not.
///
/// Signatures rather than file names, because a name is the one thing about a
/// file anybody can change and somebody's `bios.bin` is very often the zip they
/// downloaded it in. Nothing here is a guess about emulation: it is the list of
/// formats a person's folder actually holds.
fn something_else(head: &[u8]) -> bool {
    const SIGNATURES: &[&[u8]] = &[
        b"PK\x03\x04", // a zip, and every format built on one
        b"PK\x05\x06",
        b"PK\x07\x08",
        b"\x1f\x8b",           // gzip
        b"BZh",                // bzip2
        b"\xfd7zXZ\x00",       // xz
        b"7z\xbc\xaf\x27\x1c", // 7-zip
        b"Rar!",               // rar
        b"\x28\xb5\x2f\xfd",   // zstandard
        b"%PDF",               // a document
        b"\xd0\xcf\x11\xe0",   // an older one
        b"\x89PNG\r\n\x1a\n",  // a picture
        b"\xff\xd8\xff",
        b"GIF8",
        b"BM",
        b"RIFF", // a recording, a film, or a webp
        b"OggS",
        b"fLaC",
        b"ID3",
        b"\x1a\x45\xdf\xa3", // matroska
        b"\x7fELF",          // a program
        b"\xca\xfe\xba\xbe", // a java class
        b"SQLite format 3\x00",
    ];
    if SIGNATURES.iter().any(|magic| head.starts_with(magic)) {
        return true;
    }
    // The one that is not at the beginning: an mp4, a mov and a heic all carry
    // their kind four bytes in.
    head.len() >= 8 && &head[4..8] == b"ftyp"
}

/// Whether a file is text.
///
/// A boot ROM is machine code and tables, and both put a zero byte in the first
/// few hundred of them almost immediately; nothing written for a person to read
/// has one at all. So the test is that first, and then that every byte is one
/// somebody could have typed — which is what keeps a `.cue`, a `.m3u`, an
/// `.xml` and a folder of notes out of a console's firmware directory.
fn plain_text(head: &[u8]) -> bool {
    !head.contains(&0)
        && head
            .iter()
            .all(|byte| matches!(byte, 0x20..=0x7e | b'\t' | b'\n' | b'\r'))
}

/// One `key = "value"` line out of a core's description.
///
/// Matched on the whole key rather than a prefix, or `firmware1_path` would be
/// answered by `firmware11_path` on any core with more than ten of them — and
/// the Amiga's has twelve.
fn value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (had, value) = line.split_once('=')?;
        (had.trim() == key).then(|| value.trim().trim_matches('"').to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A core's description, written the way libretro writes one.
    fn describe(at: &Path, core: &str, body: &str) {
        std::fs::write(at.join(format!("{core}_libretro.info")), body).expect("a description");
    }

    /// A folder the core made on its first run and then found empty is missing.
    ///
    /// This is the PlayStation 2, and it is why this module exists: `pcsx2`
    /// creates `system/pcsx2/bios` before it looks in it, so a check that asked
    /// only whether the path was there would call an unplayable machine ready.
    #[test]
    fn an_empty_folder_is_a_thing_that_is_not_there() {
        let at = tmp("empty-folder");
        let info = at.join("info");
        let system = at.join("system");
        std::fs::create_dir_all(system.join("pcsx2/bios")).expect("the folder");
        describe(
            &info,
            "pcsx2",
            "firmware_count = 1\n\
             firmware0_desc = \"'pcsx2/bios' folder\"\n\
             firmware0_path = \"pcsx2/bios\"\n\
             firmware0_opt = \"false\"\n",
        );

        let wanted = declared(&info, &system, "pcsx2");
        assert_eq!(wanted.len(), 1);
        assert_eq!(wanted[0].path, "pcsx2/bios");
        assert!(!wanted[0].here, "an empty bios folder is not a bios");

        std::fs::write(system.join("pcsx2/bios/scph.bin"), [0u8; 1024]).expect("a bios");
        let wanted = declared(&info, &system, "pcsx2");
        assert_eq!(
            wanted.len(),
            1,
            "still declared, because the row that offers to choose one has to be \
             there after somebody has chosen one"
        );
        assert!(wanted[0].here, "and one with something in it is a bios");
    }

    /// A folder somebody filled with the wrong thing is still empty.
    ///
    /// The bug this was written for. The picker copies what it finds into the
    /// console's firmware folder, so pointing it at a folder of documents
    /// leaves `pcsx2/bios` full of documents — and the check that said "there
    /// is something in there" then called an unplayable machine ready, took
    /// the warning off every PlayStation 2 game, and started an emulator that
    /// showed a black screen and said nothing.
    #[test]
    fn a_folder_of_the_wrong_thing_is_not_a_bios() {
        let at = tmp("wrong-thing");
        let info = at.join("info");
        let system = at.join("system");
        let bios = system.join("pcsx2/bios");
        std::fs::create_dir_all(&bios).expect("the folder");
        describe(
            &info,
            "pcsx2",
            "firmware_count = 1\n\
             firmware0_desc = \"'pcsx2/bios' folder\"\n\
             firmware0_path = \"pcsx2/bios\"\n\
             firmware0_opt = \"false\"\n",
        );

        std::fs::write(bios.join("notes.txt"), b"where did I put it").expect("a note");
        std::fs::write(bios.join("holiday.png"), b"\x89PNG\r\n\x1a\n....").expect("a picture");
        std::fs::write(bios.join("dumps.zip"), b"PK\x03\x04....").expect("an archive");
        assert!(
            !declared(&info, &system, "pcsx2")[0].here,
            "none of that is a boot ROM"
        );

        std::fs::write(bios.join("scph39001.bin"), [0u8; 4096]).expect("a bios");
        assert!(
            declared(&info, &system, "pcsx2")[0].here,
            "and this could be one"
        );
    }

    /// What the folder test refuses, and what it lets through.
    ///
    /// Split out from the declaration above because it is the one piece of
    /// judgement in this module and the place a mistake in it would be found:
    /// a rule that refused a real dump would make a console impossible to set
    /// up, and both halves have to be held down.
    #[test]
    fn a_dump_is_what_is_left_when_everything_else_is_refused() {
        let at = tmp("what-a-dump-is");
        let mine = |name: &str, body: &[u8]| {
            let file = at.join(name);
            std::fs::write(&file, body).expect("a file");
            could_be_firmware(&file)
        };

        assert!(mine(
            "scph39001.bin",
            &[0x00, 0x78, 0x1a, 0x40, 0x00, 0x00, 0x59, 0x00]
        ));
        assert!(
            mine("blank.rom", &[0xff; 64]),
            "an unwritten ROM is still one"
        );
        assert!(
            mine("bios.mec", &[0x03, 0x06, 0x02, 0x00]),
            "four bytes pcsx2 wrote"
        );

        assert!(!mine("empty.bin", b""), "nothing is not a dump");
        assert!(!mine("games.m3u", b"disc1.chd\ndisc2.chd\n"), "text");
        assert!(
            !mine("readme", b"Put your BIOS here."),
            "text with no extension"
        );
        assert!(
            !mine("bios.zip", b"PK\x03\x04\x00\x00\x00\x00"),
            "an archive"
        );
        assert!(
            !mine("cover.png", b"\x89PNG\r\n\x1a\n\x00\x00"),
            "a picture"
        );
        assert!(!mine("manual.pdf", b"%PDF-1.7\n\x00"), "a document");
        assert!(!mine("clip.mp4", b"\x00\x00\x00 ftypisom"), "a film");
        assert!(!mine("core.so", b"\x7fELF\x02\x01\x01\x00"), "a program");
        assert!(!could_be_firmware(&at), "and a folder is not a file");
    }

    /// What a core says it can manage without is still somewhere to put a file.
    ///
    /// Mesen plays cartridges with no disk drive attached, so `disksys.rom` is
    /// declared optional — and somebody who has one wants it used. Left out
    /// once, and the cost was melonDS: it calls all eight of its files
    /// optional, because its own high-level BIOS plays most Nintendo DS games,
    /// so the Nintendo DS page had nowhere at all to put a real dump.
    ///
    /// Nothing here refuses to start a game over one. What a missing file
    /// stops is decided by the game failing to start, not in advance.
    #[test]
    fn what_a_core_can_manage_without_is_still_declared() {
        let at = tmp("optional");
        let info = at.join("info");
        let system = at.join("system");
        std::fs::create_dir_all(&system).expect("the folder");
        describe(
            &info,
            "mesen",
            "firmware_count = 1\n\
             firmware0_desc = \"disksys.rom (FDS BIOS)\"\n\
             firmware0_path = \"disksys.rom\"\n\
             firmware0_opt = \"true\"\n",
        );

        let wanted = declared(&info, &system, "mesen");
        assert_eq!(wanted.len(), 1, "there is a row to offer it on");
        assert_eq!(wanted[0].path, "disksys.rom");
        assert!(!wanted[0].here, "and this machine has not got it");
    }

    /// Every entry is its own, and a two-digit one is not the first.
    ///
    /// The Amiga's description has twelve, and each is read by its whole key —
    /// so `firmware1_path` names kick1 wherever in the file it was written, and
    /// `firmware11_path` names kick11. The lines are deliberately written back
    /// to front here: a lookup that found a key by scanning for something
    /// shorter would answer with whichever the file reached first, and in
    /// libretro's own order that happens to be the right one.
    #[test]
    fn a_tenth_entry_does_not_answer_for_the_first() {
        let at = tmp("two-digits");
        let info = at.join("info");
        let system = at.join("system");
        std::fs::create_dir_all(&system).expect("the folder");
        let mut body = String::from("firmware_count = 12\n");
        // The two-digit entries written first, which is what makes this a test
        // rather than a coincidence: a lookup that matched on a prefix would
        // answer `firmware1_path` with whichever of them the file reached
        // first, and in libretro's own order that happens to be the right one.
        for which in (0..12).rev() {
            body.push_str(&format!("firmware{which}_desc = \"rom {which}\"\n"));
            body.push_str(&format!("firmware{which}_path = \"kick{which}.rom\"\n"));
            body.push_str(&format!("firmware{which}_opt = \"true\"\n"));
        }
        describe(&info, "puae", &body);

        let wanted = declared(&info, &system, "puae");
        assert_eq!(wanted.len(), 12, "every Kickstart it named");
        assert_eq!(wanted[1].path, "kick1.rom", "not the eleventh entry's");
        assert_eq!(wanted[11].path, "kick11.rom");
    }

    /// A RetroArch that shipped without libretro's descriptions says nothing.
    ///
    /// Fail open, and deliberately: the alternative is a shell that refuses to
    /// start anything on a machine where it simply cannot see the answer.
    #[test]
    fn no_description_is_no_complaint() {
        let at = tmp("undescribed");
        let info = at.join("info");
        std::fs::create_dir_all(&info).expect("the folder");
        assert!(declared(&info, &at, "pcsx2").is_empty());
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let at = std::env::temp_dir().join(format!("lxb-firmware-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&at);
        std::fs::create_dir_all(at.join("info")).expect("a scratch directory");
        at
    }
}
