//! Which console a disc is for, and which game it is.
//!
//! A ROM folder says which console its games are for by what somebody called
//! it. A disc says it itself, in the first sectors of its data track — every
//! console maker put something there for its own machine to check — and the
//! game's catalogue number is nearly always beside it:
//!
//! ```text
//!   PlayStation, PlayStation 2   ISO 9660, and SYSTEM.CNF naming the program
//!                                to boot: BOOT2 = cdrom0:\SCES_508.78;1
//!   Sega Saturn                  "SEGA SEGASATURN " at the head of sector 0,
//!                                the product number at 0x20
//!   Sega CD                      "SEGADISCSYSTEM" at the head of sector 0,
//!                                the product number at 0x183
//!   PC-FX                        "PC-FX:Hu_CD-ROM" at the head of sector 0
//!   PC Engine CD                 "PC Engine CD-ROM SYSTEM" at 0x20 of sector 1
//!   3DO                          the Opera file system's volume header
//!   Neo Geo CD                   ISO 9660 with IPL.TXT at the root
//! ```
//!
//! The same marks the emulators themselves look for, which is why they are
//! trusted: a disc that satisfies one is a disc that console's emulator will
//! take. The catalogue number is what RetroArch's own database files a game
//! under — see [`crate::rdb`] — and it is read here in the spelling that
//! database uses.
//!
//! Nothing a PC drive cannot read is here. A Dreamcast's GD-ROM, a GameCube's
//! or a Wii's disc are the wrong kind of disc for one, and asking about them
//! would be a list of consoles this can never answer for.

use crate::consoles::{self, Machine};
use crate::drive::{Sectors, USER};

/// What a disc turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub machine: &'static Machine,
    /// The name of RetroArch's database for this console, which is not always
    /// the console's first thumbnail shelf: a PC Engine's CD games are a
    /// database of their own.
    pub database: &'static str,
    /// The catalogue number, as the database spells it where the disc says it:
    /// `SCES-50878`, `MK-81020`. `None` for the consoles whose discs do not
    /// carry one.
    pub serial: Option<String>,
    /// What the disc calls itself — a volume's name or the title in a Sega
    /// header — for a game the database does not know.
    pub label: Option<String>,
}

impl Identity {
    /// Every spelling of the catalogue number worth looking up.
    ///
    /// libretro's databases are assembled from several sources and do not
    /// agree with the discs on the small things: Sega's own product numbers
    /// are printed `MK-81020` and filed as `81020`, and a second disc of a set
    /// carries `-1` on the end that the set's entry may not. So the number as
    /// read comes first, and the shorter forms after it.
    pub fn serials(&self) -> Vec<String> {
        let Some(serial) = self.serial.as_deref() else {
            return Vec::new();
        };
        let mut all = vec![serial.to_string()];
        if let Some(bare) = serial.strip_prefix("MK-") {
            all.push(bare.to_string());
        }
        if let Some((head, tail)) = serial.rsplit_once('-') {
            if !head.is_empty() && tail.len() <= 2 && tail.chars().all(|c| c.is_ascii_digit()) {
                all.push(head.to_string());
            }
        }
        all.dedup();
        all
    }
}

/// Which console the disc in `disc` is for, or `None` for a disc that is not a
/// game this integration can play — music, a film, somebody's photographs.
pub fn identify(disc: &mut dyn Sectors) -> Option<Identity> {
    let base = disc.toc().first_data()?.start;
    let head = disc.user(base).ok();
    if let Some(head) = head.as_deref() {
        if let Some(found) = from_header(head) {
            return Some(found);
        }
    }
    // A PC Engine's mark is one sector in, after the boot sector a PC-FX's is
    // on — so the two are asked in that order.
    if let Ok(second) = disc.user(base + 1) {
        if second.get(0x20..0x37) == Some(b"PC Engine CD-ROM SYSTEM".as_slice()) {
            return Some(Identity {
                machine: machine("pce"),
                database: "NEC - PC Engine CD - TurboGrafx-CD",
                serial: None,
                label: None,
            });
        }
    }
    from_iso(disc, base)
}

/// The consoles that say what they are at the head of the data track.
fn from_header(head: &[u8]) -> Option<Identity> {
    if head.starts_with(b"SEGA SEGASATURN ") {
        return Some(Identity {
            machine: machine("saturn"),
            database: "Sega - Saturn",
            serial: text(head.get(0x20..0x2a)?).filter(|serial| !serial.is_empty()),
            label: text(head.get(0x60..0xd0)?).filter(|title| !title.is_empty()),
        });
    }
    if [
        b"SEGADISCSYSTEM".as_slice(),
        b"SEGABOOTDISC",
        b"SEGADATADISC",
    ]
    .iter()
    .any(|mark| head.starts_with(mark))
    {
        // The product number after `GM `, up to the revision the header puts
        // after a space: `MK-4407 -00`.
        let serial = text(head.get(0x183..0x18e)?)
            .and_then(|serial| serial.split_whitespace().next().map(str::to_string))
            .filter(|serial| !serial.is_empty());
        // The overseas title, which is the one in English on a disc sold in
        // more than one place; the domestic one where there is none.
        let label = text(head.get(0x150..0x180)?)
            .filter(|title| !title.is_empty())
            .or_else(|| text(&head[0x120..0x150]).filter(|title| !title.is_empty()));
        return Some(Identity {
            machine: machine("segacd"),
            database: "Sega - Mega-CD - Sega CD",
            serial,
            label,
        });
    }
    if head.starts_with(b"PC-FX:Hu_CD-ROM") {
        return Some(Identity {
            machine: machine("pcfx"),
            database: "NEC - PC-FX",
            serial: None,
            label: None,
        });
    }
    // The Opera file system: record type 1, five sync bytes, version 1.
    if head.get(..7) == Some([1, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 1].as_slice()) {
        return Some(Identity {
            machine: machine("3do"),
            database: "The 3DO Company - 3DO",
            serial: None,
            label: text(head.get(0x28..0x48)?)
                // Nearly every disc is labelled just this, which names nothing.
                .filter(|label| !label.is_empty() && label != "CD-ROM"),
        });
    }
    None
}

/// The consoles whose discs are ISO 9660: the two PlayStations and the Neo
/// Geo CD.
fn from_iso(disc: &mut dyn Sectors, base: u32) -> Option<Identity> {
    let volume = disc.user(base + 16).ok()?;
    if volume.first() != Some(&1) || volume.get(1..6) != Some(b"CD001".as_slice()) {
        return None;
    }
    let system = text(volume.get(8..40)?).unwrap_or_default();
    let label = text(volume.get(40..72)?)
        .map(|label| label.replace('_', " "))
        .filter(|label| !label.trim().is_empty());
    let root = record_extent(volume.get(156..190)?)?;

    if let Some(config) = read_file(disc, root, "SYSTEM.CNF") {
        let config = String::from_utf8_lossy(&config);
        let (ps2, boot) = boot_line(&config)?;
        let (key, database) = match ps2 {
            true => ("ps2", "Sony - PlayStation 2"),
            false => ("psx", "Sony - PlayStation"),
        };
        return Some(Identity {
            machine: machine(key),
            database,
            serial: serial_of_boot(&boot),
            label,
        });
    }
    // The oldest PlayStation discs boot PSX.EXE and have no SYSTEM.CNF at all;
    // the volume says whose it is.
    if system.starts_with("PLAYSTATION") {
        return Some(Identity {
            machine: machine("psx"),
            database: "Sony - PlayStation",
            serial: None,
            label,
        });
    }
    if find(disc, root, "IPL.TXT").is_some() {
        return Some(Identity {
            machine: machine("neogeocd"),
            database: "SNK - Neo Geo CD",
            serial: None,
            label,
        });
    }
    None
}

/// A console from the table, by a name this file knows is in it.
fn machine(key: &str) -> &'static Machine {
    consoles::machine(key).unwrap_or_else(|| panic!("{key} is missing from the console table"))
}

/// Text in a fixed-width field, trimmed of the spaces and nulls it is padded
/// with and of anything that is not printable.
fn text(field: &[u8]) -> Option<String> {
    let said: String = field
        .iter()
        .map(|&byte| byte as char)
        .map(|c| if c.is_ascii_graphic() { c } else { ' ' })
        .collect();
    Some(said.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Where a directory record's file is, and how long it is.
#[derive(Debug, Clone, Copy)]
struct Extent {
    lba: u32,
    size: u32,
}

fn record_extent(record: &[u8]) -> Option<Extent> {
    Some(Extent {
        lba: u32::from_le_bytes(record.get(2..6)?.try_into().ok()?),
        size: u32::from_le_bytes(record.get(10..14)?.try_into().ok()?),
    })
}

/// A file in the root directory, by name, ignoring case and the `;1` version.
fn find(disc: &mut dyn Sectors, root: Extent, name: &str) -> Option<Extent> {
    // A root directory of a game disc is a sector or two; the bound is for a
    // corrupt record claiming to be a gigabyte.
    let sectors = root.size.div_ceil(USER as u32).min(64);
    for at in 0..sectors {
        let sector = disc.user(root.lba + at).ok()?;
        let mut offset = 0;
        while offset < sector.len() {
            let length = usize::from(sector[offset]);
            if length == 0 {
                // Records never cross a sector; the rest of this one is padding.
                break;
            }
            let record = sector.get(offset..offset + length)?;
            let name_length = usize::from(*record.get(32)?);
            let found = record.get(33..33 + name_length)?;
            let found = String::from_utf8_lossy(found);
            let found = found.split(';').next().unwrap_or_default();
            if found.eq_ignore_ascii_case(name) {
                return record_extent(record);
            }
            offset += length;
        }
    }
    None
}

/// A small file in the root directory, whole.
fn read_file(disc: &mut dyn Sectors, root: Extent, name: &str) -> Option<Vec<u8>> {
    let file = find(disc, root, name)?;
    // SYSTEM.CNF is a few lines; anything bigger is not the file this is for.
    let size = file.size.min(8 * USER as u32) as usize;
    let mut bytes = Vec::with_capacity(size);
    for at in 0..(size as u32).div_ceil(USER as u32) {
        bytes.extend(disc.user(file.lba + at).ok()?);
    }
    bytes.truncate(size);
    Some(bytes)
}

/// The boot line of a SYSTEM.CNF, and whether it is a PlayStation 2's.
///
/// `BOOT2` is the PlayStation 2's key and `BOOT` the first PlayStation's, and
/// nothing else in the file tells the two apart: a PlayStation 2 game on a CD
/// is still a PlayStation 2 game.
fn boot_line(config: &str) -> Option<(bool, String)> {
    config.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        match key.trim().to_ascii_uppercase().as_str() {
            "BOOT2" => Some((true, value.trim().to_string())),
            "BOOT" => Some((false, value.trim().to_string())),
            _ => None,
        }
    })
}

/// The catalogue number in a boot line — `cdrom0:\SCES_508.78;1` is SCES-50878.
///
/// `None` for a disc that boots a program by any other name, which the first
/// PlayStation discs did.
fn serial_of_boot(boot: &str) -> Option<String> {
    let file = boot
        .rsplit(['\\', '/', ':'])
        .next()?
        .split(';')
        .next()?
        .trim()
        .to_ascii_uppercase();
    let (prefix, number) = file.split_once(['_', '-'])?;
    let digits: String = number.chars().filter(char::is_ascii_digit).collect();
    let fits = prefix.len() == 4
        && prefix.chars().all(|c| c.is_ascii_alphabetic())
        && digits.len() == 5
        && number.chars().all(|c| c.is_ascii_digit() || c == '.');
    fits.then(|| format!("{prefix}-{digits}"))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::drive::{Toc, Track};
    use std::collections::BTreeMap;

    /// A disc as its data sectors, for everything in this module that reads one.
    pub struct Cooked {
        pub toc: Toc,
        pub sectors: BTreeMap<u32, Vec<u8>>,
    }

    impl Cooked {
        pub fn new(tracks: Vec<Track>, leadout: u32) -> Cooked {
            Cooked {
                toc: Toc {
                    tracks,
                    leadout,
                    dvd: false,
                },
                sectors: BTreeMap::new(),
            }
        }

        pub fn put(&mut self, lba: u32, bytes: &[u8]) {
            let mut sector = vec![0u8; USER];
            sector[..bytes.len()].copy_from_slice(bytes);
            self.sectors.insert(lba, sector);
        }

        pub fn put_at(&mut self, lba: u32, offset: usize, bytes: &[u8]) {
            let sector = self.sectors.entry(lba).or_insert_with(|| vec![0u8; USER]);
            sector[offset..offset + bytes.len()].copy_from_slice(bytes);
        }
    }

    impl Sectors for Cooked {
        fn toc(&self) -> &Toc {
            &self.toc
        }

        fn raw(&mut self, _: u32, _: u32, _: &mut [u8]) -> std::io::Result<()> {
            Err(std::io::Error::other("cooked"))
        }

        fn user(&mut self, lba: u32) -> std::io::Result<Vec<u8>> {
            Ok(self
                .sectors
                .get(&lba)
                .cloned()
                .unwrap_or_else(|| vec![0; USER]))
        }
    }

    fn data_track() -> Vec<Track> {
        vec![Track {
            number: 1,
            start: 0,
            data: true,
        }]
    }

    fn directory_record(name: &str, lba: u32, size: u32) -> Vec<u8> {
        let mut record = vec![0u8; 33 + name.len() + (name.len() + 1) % 2];
        record[0] = record.len() as u8;
        record[2..6].copy_from_slice(&lba.to_le_bytes());
        record[10..14].copy_from_slice(&size.to_le_bytes());
        record[32] = name.len() as u8;
        record[33..33 + name.len()].copy_from_slice(name.as_bytes());
        record
    }

    /// An ISO 9660 disc with one file at the root, laid out the way a mastering
    /// tool lays one out: the volume descriptor at 16, the root at 20, the file
    /// after it.
    pub fn iso(system: &str, volume: &str, file: (&str, &[u8])) -> Cooked {
        let mut disc = Cooked::new(data_track(), 1000);
        let mut descriptor = vec![0u8; USER];
        descriptor[0] = 1;
        descriptor[1..6].copy_from_slice(b"CD001");
        descriptor[8..40].fill(b' ');
        descriptor[8..8 + system.len()].copy_from_slice(system.as_bytes());
        descriptor[40..72].fill(b' ');
        descriptor[40..40 + volume.len()].copy_from_slice(volume.as_bytes());
        let root = directory_record("\0", 20, USER as u32);
        descriptor[156..156 + root.len()].copy_from_slice(&root);
        disc.put(16, &descriptor);

        let mut listing = directory_record("\0", 20, USER as u32);
        listing.extend(directory_record("\u{1}", 20, USER as u32));
        listing.extend(directory_record(
            &format!("{};1", file.0),
            21,
            file.1.len() as u32,
        ));
        disc.put(20, &listing);
        disc.put(21, file.1);
        disc
    }

    /// The disc this was written against: Tekken 4, PAL, on a PlayStation 2
    /// DVD. Its SYSTEM.CNF is what the PS2 core logged booting it.
    #[test]
    fn a_playstation_2_disc_names_its_game_in_system_cnf() {
        let mut disc = iso(
            "PLAYSTATION",
            "TEKKEN4",
            (
                "SYSTEM.CNF",
                b"BOOT2 = cdrom0:\\SCES_508.78;1\r\nVER = 1.00\r\nVMODE = PAL\r\n",
            ),
        );
        let found = identify(&mut disc).expect("a game");
        assert_eq!(found.machine.title, "PlayStation 2");
        assert_eq!(found.database, "Sony - PlayStation 2");
        assert_eq!(found.serial.as_deref(), Some("SCES-50878"));
        assert_eq!(found.label.as_deref(), Some("TEKKEN4"));
    }

    #[test]
    fn a_playstation_disc_boots_with_boot_rather_than_boot2() {
        let mut disc = iso(
            "PLAYSTATION",
            "SLUS_00067",
            ("SYSTEM.CNF", b"BOOT = cdrom:\\SLUS_000.67;1\nTCB = 4\n"),
        );
        let found = identify(&mut disc).expect("a game");
        assert_eq!(found.machine.title, "PlayStation");
        assert_eq!(found.serial.as_deref(), Some("SLUS-00067"));
        assert_eq!(found.label.as_deref(), Some("SLUS 00067"));
    }

    /// The first PlayStation discs boot PSX.EXE and have no SYSTEM.CNF: still a
    /// PlayStation disc, with no number to look up.
    #[test]
    fn an_early_playstation_disc_is_known_by_its_volume() {
        let mut disc = iso("PLAYSTATION", "RIDGE", ("PSX.EXE", b"PS-X EXE"));
        let found = identify(&mut disc).expect("a game");
        assert_eq!(found.machine.title, "PlayStation");
        assert!(found.serial.is_none());
    }

    #[test]
    fn a_boot_line_naming_no_catalogue_number_has_no_serial() {
        assert_eq!(serial_of_boot("cdrom:\\PSX.EXE;1"), None);
        assert_eq!(
            serial_of_boot("cdrom0:\\SLPM_654.84;1").as_deref(),
            Some("SLPM-65484")
        );
        assert_eq!(
            serial_of_boot("cdrom:SCUS_944.26;1").as_deref(),
            Some("SCUS-94426")
        );
    }

    #[test]
    fn a_neo_geo_cd_is_iso_9660_with_an_ipl_txt() {
        let mut disc = iso("", "METAL_SLUG", ("IPL.TXT", b"PRG,0,0\r\n"));
        let found = identify(&mut disc).expect("a game");
        assert_eq!(found.machine.title, "Neo Geo CD");
        assert_eq!(found.label.as_deref(), Some("METAL SLUG"));
    }

    /// A data disc somebody burned is ISO 9660 too, and is nobody's game.
    #[test]
    fn a_disc_of_somebodys_files_is_not_a_game() {
        let mut disc = iso("LINUX", "HOLIDAY", ("README.TXT", b"photos"));
        assert!(identify(&mut disc).is_none());
    }

    #[test]
    fn a_music_cd_is_not_a_game() {
        let mut disc = Cooked::new(
            vec![Track {
                number: 1,
                start: 0,
                data: false,
            }],
            100_000,
        );
        assert!(identify(&mut disc).is_none());
    }

    #[test]
    fn a_saturn_disc_says_so_in_sector_zero() {
        let mut disc = Cooked::new(data_track(), 1000);
        disc.put_at(0, 0, b"SEGA SEGASATURN SEGA ENTERPRISES");
        disc.put_at(0, 0x20, b"MK-81020  V1.000");
        disc.put_at(0, 0x60, b"NIGHTS INTO DREAMS...   ");
        let found = identify(&mut disc).expect("a game");
        assert_eq!(found.machine.title, "Sega Saturn");
        assert_eq!(found.serial.as_deref(), Some("MK-81020"));
        assert_eq!(found.label.as_deref(), Some("NIGHTS INTO DREAMS..."));
        assert_eq!(found.serials(), ["MK-81020", "81020"]);
    }

    #[test]
    fn a_sega_cd_disc_carries_its_number_after_gm() {
        let mut disc = Cooked::new(data_track(), 1000);
        disc.put_at(0, 0, b"SEGADISCSYSTEM  ");
        disc.put_at(0, 0x120, b"SONIC THE HEDGEHOG CD");
        disc.put_at(0, 0x150, b"SONIC CD");
        disc.put_at(0, 0x180, b"GM MK-4407 -00");
        let found = identify(&mut disc).expect("a game");
        assert_eq!(found.machine.title, "Sega CD");
        assert_eq!(found.serial.as_deref(), Some("MK-4407"));
        assert_eq!(found.label.as_deref(), Some("SONIC CD"));
    }

    /// A PC Engine CD opens with a music track warning that it is not a music
    /// CD, so its mark is on the second track.
    #[test]
    fn a_pc_engine_cd_is_found_on_its_data_track() {
        let mut disc = Cooked::new(
            vec![
                Track {
                    number: 1,
                    start: 0,
                    data: false,
                },
                Track {
                    number: 2,
                    start: 3000,
                    data: true,
                },
            ],
            100_000,
        );
        disc.put_at(3001, 0x20, b"PC Engine CD-ROM SYSTEM");
        let found = identify(&mut disc).expect("a game");
        assert_eq!(found.machine.title, "PC Engine");
        assert_eq!(found.database, "NEC - PC Engine CD - TurboGrafx-CD");
    }

    #[test]
    fn a_pc_fx_and_a_3do_are_known_by_their_first_sector() {
        let mut fx = Cooked::new(data_track(), 1000);
        fx.put_at(0, 0, b"PC-FX:Hu_CD-ROM ");
        assert_eq!(
            identify(&mut fx).map(|found| found.machine.title),
            Some("PC-FX")
        );

        let mut opera = Cooked::new(data_track(), 1000);
        opera.put_at(0, 0, &[1, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 1]);
        opera.put_at(0, 0x28, b"CD-ROM");
        let found = identify(&mut opera).expect("a game");
        assert_eq!(found.machine.title, "3DO");
        // Every disc is labelled that, so it is not a name.
        assert!(found.label.is_none());
    }

    #[test]
    fn a_second_disc_of_a_set_is_looked_up_with_and_without_its_number() {
        let found = Identity {
            machine: machine("psx"),
            database: "Sony - PlayStation",
            serial: Some("PCPX-96179-1".to_string()),
            label: None,
        };
        assert_eq!(found.serials(), ["PCPX-96179-1", "PCPX-96179"]);
    }
}
