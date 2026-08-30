//! The files a core needs beside it, which are not the core.
//!
//! Some emulators are two things: a shared object, and a folder of data it
//! cannot run without. PPSSPP is the clearest case — the core is 21 megabytes
//! of emulator and *none* of the PSP's own fonts, so a core installed by itself
//! starts, runs, and draws every menu as blank boxes with `Core system files
//! are missing` along the bottom. The emulator is not broken and the ROM is not
//! broken; the half of the emulator that draws letters was never fetched.
//!
//! RetroArch's own updater has a *System Files* page for this, separate from
//! the core downloader and reached from a different menu. A shell that fetches
//! cores and stops there has therefore only done half the job, which is why
//! this is here.
//!
//! ```text
//! https://buildbot.libretro.com/assets/system/
//!     PPSSPP.zip      the folder, as one archive, named for the folder
//! ```
//!
//! ## Why this is a list and not a rule
//!
//! Every core declares what it cannot run without, in the `.info` file
//! RetroArch ships beside it — `firmware0_path = "PPSSPP/ppge_atlas.zim"` — so
//! it is tempting to take the folder out of that path and ask the server for
//! it. That does not work, and it is worth writing down why, because it looks
//! like it should:
//!
//! * The archives are named for the *emulator*, not the folder. Dolphin's
//!   folder is `dolphin-emu` and its archive is `Dolphin.zip`; the PlayStation
//!   2 core's folder is `pcsx2` and its archive is `LRPS2.zip`; blueMSX's
//!   files land in `Databases` and `Machines` and its archive is `blueMSX.zip`.
//! * Most declared firmware is not published at all, and must not be. A
//!   Dreamcast core declares `dc/dc_boot.bin` and a PlayStation core declares
//!   `scph5501.bin`; those are the console's own ROM, they belong to whoever
//!   made the machine, and nobody publishes them. Asking the server for them
//!   would be asking for something that should not be there.
//!
//! So the list below is the intersection somebody has to check by hand: cores
//! whose *required* files libretro publishes in full. Each entry was verified
//! by fetching the archive and looking for the file the core says it needs.

use std::path::{Path, PathBuf};

/// Where libretro publishes the folders its cores need.
const ASSETS: &str = "https://buildbot.libretro.com/assets/system";

/// A folder of system files, and the core that cannot run without it.
pub struct Bundle {
    /// The core, by the name its file has.
    pub core: &'static str,
    /// What the archive is called on the server, without the `.zip`.
    pub archive: &'static str,
    /// One file out of it that the core's own `.info` declares it cannot run
    /// without, as a path under the system directory.
    ///
    /// This is what says whether the folder is there, and it is written *last*
    /// when unpacking — so a folder half-written by a run that died is a folder
    /// this still calls missing, rather than one that looks complete and is
    /// not.
    pub proves: &'static str,
}

/// The cores this knows how to complete.
///
/// Short on purpose. A core belongs here when libretro publishes an archive
/// containing every file that core declares as *required* — not when it merely
/// publishes something. A core whose remaining requirement is a console's own
/// ROM is left out: fetching the rest would turn "this cannot run" into "this
/// cannot run, and now it has downloaded 4 megabytes as well".
pub const BUNDLES: &[Bundle] = &[
    Bundle {
        core: "ppsspp",
        archive: "PPSSPP",
        proves: "PPSSPP/ppge_atlas.zim",
    },
    Bundle {
        core: "dolphin",
        archive: "Dolphin",
        proves: "dolphin-emu/Sys/codehandler.bin",
    },
    Bundle {
        // The PlayStation 2 core declares two things it cannot run without: the
        // console's own BIOS, which is Sony's and is nobody's to publish, and
        // `pcsx2/resources/GameIndex.yaml`, which is the emulator's own database
        // of per-game fixes and is in here. Without the second it starts and
        // then plays a great many games badly, which is a worse failure than
        // not starting because nothing says why.
        //
        // The archive also holds an empty `pcsx2/bios/` — which is exactly the
        // shape [`crate::firmware::here`] was written to see through, so the
        // BIOS goes on being reported as missing after this lands.
        core: "pcsx2",
        archive: "LRPS2",
        proves: "pcsx2/resources/GameIndex.yaml",
    },
    Bundle {
        core: "bluemsx",
        archive: "blueMSX",
        proves: "Machines/Shared Roms/MSX.rom",
    },
];

/// The bundle this core needs, if it needs one at all.
pub fn of(core: &str) -> Option<&'static Bundle> {
    BUNDLES.iter().find(|bundle| bundle.core == core)
}

/// Whether this integration fetches this declared file itself.
///
/// libretro's firmware list is not only the console's own boot ROM. pcsx2
/// declares two things it cannot run without, and one of them —
/// `pcsx2/resources/GameIndex.yaml`, its per-game fixes — is published in
/// `LRPS2.zip` and comes down with the core. Asking somebody to go and find
/// that in a folder of their own would be asking for a file they have never
/// had, beside a question about a BIOS that they may well have.
pub fn fetched(core: &str, path: &str) -> bool {
    of(core).is_some_and(|bundle| bundle.proves == path)
}

/// The bundle this core needs and has not got.
///
/// `None` both for a core that needs nothing and for one whose files are
/// already there, because the two have the same answer: there is nothing to
/// fetch.
pub fn wanted(system: &Path, core: &str) -> Option<&'static Bundle> {
    let bundle = of(core)?;
    if system.join(bundle.proves).exists() {
        return None;
    }
    Some(bundle)
}

/// Where this RetroArch keeps the files its cores read.
///
/// RetroArch's own setting where it has one, because somebody may have moved
/// it and the core will look wherever they said. Its default is a directory of
/// that name beside the configuration, which is also the answer when the
/// setting is absent, unreadable, or something this cannot make sense of.
pub fn system_dir(config: &Path) -> PathBuf {
    let beside = config.join("system");
    let Ok(text) = std::fs::read_to_string(config.join("retroarch.cfg")) else {
        return beside;
    };
    let Some(said) = text.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key.trim() == "system_directory").then(|| value.trim().trim_matches('"').to_string())
    }) else {
        return beside;
    };
    // `default` is RetroArch's own word for "beside the configuration", and an
    // empty value means the same thing.
    if said.is_empty() || said == "default" {
        return beside;
    }
    if let Some(rest) = said.strip_prefix("~/") {
        let Some(home) = std::env::var_os("HOME") else {
            return beside;
        };
        return PathBuf::from(home).join(rest);
    }
    let at = PathBuf::from(&said);
    if at.is_absolute() {
        at
    } else {
        beside
    }
}

/// Where that archive is on the build server.
pub fn url(bundle: &Bundle) -> String {
    // The names carry spaces and brackets on that server. None of the three
    // here do, but the encoding is not conditional on that being true forever.
    let name: String = bundle
        .archive
        .chars()
        .map(|c| match c {
            ' ' => "%20".to_string(),
            '(' => "%28".to_string(),
            ')' => "%29".to_string(),
            other => other.to_string(),
        })
        .collect();
    format!("{ASSETS}/{name}.zip")
}

/// Write an unpacked archive into the system directory.
///
/// The file the core is known to want goes **last**, so that a run interrupted
/// part of the way through leaves a folder that still reads as incomplete and
/// is fetched again, rather than one that passes the check and does not work.
pub fn unpack(
    bundle: &Bundle,
    files: Vec<(String, Vec<u8>)>,
    into: &Path,
) -> Result<usize, String> {
    let mut last = None;
    let mut written = 0;
    for (name, bytes) in files {
        if name == bundle.proves {
            last = Some(bytes);
            continue;
        }
        write_one(into, &name, &bytes)?;
        written += 1;
    }
    let Some(bytes) = last else {
        return Err(format!(
            "the archive does not hold {}, which is the file this was for",
            bundle.proves
        ));
    };
    write_one(into, bundle.proves, &bytes)?;
    Ok(written + 1)
}

fn write_one(into: &Path, name: &str, bytes: &[u8]) -> Result<(), String> {
    let at = into.join(name);
    if let Some(parent) = at.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("{} could not be made: {err}", parent.display()))?;
    }
    std::fs::write(&at, bytes).map_err(|err| format!("{name} could not be written: {err}"))
}

#[cfg(test)]
mod tests {
    /// What this integration fetches is not something to ask a person for.
    ///
    /// pcsx2 declares two files it cannot run without and they are entirely
    /// different kinds of thing: `pcsx2/resources/GameIndex.yaml` comes down
    /// with the core, and the console's BIOS is Sony's and comes from a machine
    /// somebody owns. Offering both on the same row would ask them to go and
    /// find a file they have never had.
    #[test]
    fn the_file_that_comes_with_the_core_is_not_asked_for() {
        assert!(fetched("pcsx2", "pcsx2/resources/GameIndex.yaml"));
        assert!(
            !fetched("pcsx2", "pcsx2/bios"),
            "the BIOS is nobody's to publish and is exactly what is asked for"
        );
        assert!(!fetched("mesen", "disksys.rom"), "no bundle at all");
    }

    /// The PlayStation 2 core's own database is fetched, and its BIOS is not.
    ///
    /// pcsx2 declares two things it cannot run without. One of them —
    /// `pcsx2/resources/GameIndex.yaml`, its per-game fixes — is published in
    /// `LRPS2.zip` and is fetched like any other bundle. The other is the
    /// console's own BIOS, which is Sony's and is in nobody's archive: this
    /// list must never grow an entry that pretends otherwise.
    #[test]
    fn the_playstation_2_core_fetches_its_database_and_not_its_bios() {
        let bundle = of("pcsx2").expect("the bundle");
        assert_eq!(bundle.archive, "LRPS2");
        assert_eq!(bundle.proves, "pcsx2/resources/GameIndex.yaml");
        assert!(
            !BUNDLES.iter().any(|bundle| bundle.proves.contains("bios")),
            "nothing here may claim to fetch a console's own boot ROM"
        );
    }

    use super::*;

    /// Every entry names a core that some console actually offers. An entry for
    /// a core nothing can ask for is an entry that will never run.
    #[test]
    fn every_bundle_belongs_to_a_core_this_helper_can_fetch() {
        for bundle in BUNDLES {
            let known = crate::consoles::CONSOLES
                .iter()
                .any(|machine| machine.cores.contains(&bundle.core));
            assert!(known, "{} is on no console's list", bundle.core);
        }
    }

    /// The file that proves a bundle is there has to be *inside* it — a check
    /// against a path in some other folder would pass forever or fail forever.
    #[test]
    fn what_proves_a_bundle_is_a_path_below_the_system_directory() {
        for bundle in BUNDLES {
            assert!(!bundle.proves.is_empty());
            assert!(
                !bundle.proves.starts_with('/') && !bundle.proves.contains(".."),
                "{} does not stay put",
                bundle.proves
            );
            assert!(
                bundle.proves.contains('/'),
                "{} is not in a folder of its own",
                bundle.proves
            );
        }
    }

    #[test]
    fn a_core_that_needs_nothing_is_asked_for_nothing() {
        assert!(of("snes9x").is_none());
        assert!(of("ppsspp").is_some());
    }

    /// The one that carries a space in the name it is published under.
    #[test]
    fn an_archives_name_is_spelt_the_way_a_url_spells_it() {
        let spaced = Bundle {
            core: "x",
            archive: "MAME 2003-Plus",
            proves: "a/b",
        };
        assert_eq!(
            url(&spaced),
            "https://buildbot.libretro.com/assets/system/MAME%202003-Plus.zip"
        );
        assert_eq!(
            url(&BUNDLES[0]),
            "https://buildbot.libretro.com/assets/system/PPSSPP.zip"
        );
    }

    #[test]
    fn a_folder_already_there_is_not_fetched_again() {
        let dir = std::env::temp_dir().join(format!("lxb-assets-{}", std::process::id()));
        let system = dir.join("system");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(wanted(&system, "ppsspp").is_some(), "nothing is there yet");

        let at = system.join("PPSSPP/ppge_atlas.zim");
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        std::fs::write(&at, b"not really an atlas").unwrap();
        assert!(wanted(&system, "ppsspp").is_none(), "it is there now");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The file the core looks for is written after everything else, so a run
    /// that stopped half way is a folder that gets fetched again.
    #[test]
    fn the_file_that_proves_it_is_written_last() {
        let dir = std::env::temp_dir().join(format!("lxb-unpack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let bundle = &BUNDLES[0];

        // An archive that has everything except the file it was for.
        let short = vec![("PPSSPP/flash0/font/a.pgf".to_string(), b"font".to_vec())];
        assert!(unpack(bundle, short, &dir).is_err());
        assert!(wanted(&dir, "ppsspp").is_some(), "still incomplete");
        assert!(
            dir.join("PPSSPP/flash0/font/a.pgf").exists(),
            "the rest is there"
        );

        let whole = vec![
            ("PPSSPP/flash0/font/a.pgf".to_string(), b"font".to_vec()),
            (bundle.proves.to_string(), b"atlas".to_vec()),
        ];
        assert_eq!(unpack(bundle, whole, &dir).unwrap(), 2);
        assert!(wanted(&dir, "ppsspp").is_none(), "complete now");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The setting is read where somebody has moved the folder, and the
    /// default is a folder of that name beside the configuration.
    #[test]
    fn the_system_folder_is_the_one_retroarch_was_told_about() {
        let dir = std::env::temp_dir().join(format!("lxb-sysdir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(system_dir(&dir), dir.join("system"), "no configuration");

        std::fs::write(
            dir.join("retroarch.cfg"),
            "system_directory = \"default\"\n",
        )
        .unwrap();
        assert_eq!(system_dir(&dir), dir.join("system"), "the word default");

        std::fs::write(
            dir.join("retroarch.cfg"),
            "system_directory = \"/srv/bios\"\n",
        )
        .unwrap();
        assert_eq!(system_dir(&dir), PathBuf::from("/srv/bios"));

        // A relative path is not one this can resolve, so it is not guessed at.
        std::fs::write(dir.join("retroarch.cfg"), "system_directory = \"bios\"\n").unwrap();
        assert_eq!(system_dir(&dir), dir.join("system"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
