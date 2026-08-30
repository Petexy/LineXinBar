//! Which console a folder is, what runs it, and what in it is a game.
//!
//! Somebody's ROM folder is sorted by hand — this integration says so before it
//! asks for one — so the folder names are whatever that person types, and this
//! table is the whole of what turns `roms/psp` into a column called PlayStation
//! Portable with a core behind it. Nothing here is discovered: a directory of
//! files says nothing about which machine they were written for, and the name
//! the user gave the folder is the only statement of intent there is.
//!
//! ## What a folder that is not in the table does
//!
//! It becomes a column under its own name, and every file in it that is not
//! obviously *not* a game is listed. That is deliberate and it is the reason
//! [`UNIVERSAL`] exists: this table will always be missing somebody's console,
//! and the failure it is worth designing for is a shelf that says "psx" instead
//! of "PlayStation" — not a shelf that is empty because a name was spelt in a
//! way nobody here thought of.
//!
//! ## Three lists per console, and why the cores are in order
//!
//! `cores` is a preference, best first, and every one of them is a real
//! libretro core name — the file is that name with `_libretro.so` after it.
//! More than one because which cores a machine has is not this integration's
//! decision: a distribution package installs a handful, the flatpak carries
//! several dozen, and somebody who has been using RetroArch for years has
//! downloaded whichever they prefer. So the answer is the first of these that
//! is actually on the disk, and where none is, naming the first is what lets
//! the shell say *which* core to go and get rather than only that there is not
//! one.

/// One console, as the folder that holds it.
pub struct Machine {
    /// Every folder name that means this console — lowercased, with spaces,
    /// dashes and underscores taken out, because `Mega Drive`, `mega-drive` and
    /// `megadrive` are one folder somebody spelt three ways. See [`normalise`].
    pub aliases: &'static [&'static str],
    /// What the column is called on the bar.
    pub title: &'static str,
    /// The mark this console's column and its games wear, as the shell looks
    /// it up.
    ///
    /// The whole name including the `lxb:` namespace, rather than the stem of
    /// the file: that prefix is what the shell's atlas files a mark of the
    /// *interface* under as against an icon out of an application's theme, and
    /// it is agreed between these two packages already — see
    /// `icons::from_packages`, which is what makes these names out of the
    /// drawings this package installs. Composing it on the far side would be
    /// the convention written down twice.
    ///
    /// A console whose drawing did not ship — a partial install, a
    /// distribution that split the files differently — falls back to
    /// RetroArch's own mark on the shell's side. Nothing here checks: this
    /// package does not know where its own data directory ended up.
    pub glyph: &'static str,
    /// What libretro's thumbnail server calls this console, best first.
    ///
    /// A separate name from [`Machine::title`] and never derived from it: the
    /// server's folders are named the way libretro's database names a system —
    /// `Sega - Master System - Mark III`, `The 3DO Company - 3DO` — and a
    /// spelling that is one character out is a console whose every game is
    /// drawn with no cover and nothing in the log to say why. Every one of
    /// these was read off the server's own listing.
    ///
    /// More than one where a folder somebody spelt one way holds two of
    /// libretro's shelves: `pce` is where the CD games are as well, `ws` holds
    /// the colour ones, and an arcade folder is MAME and FBNeo at once. They
    /// are searched in order and the first that has the game wins — see
    /// [`crate::art`].
    pub shelves: &'static [&'static str],
    /// The libretro cores that can run it, best first.
    pub cores: &'static [&'static str],
    /// The extensions its games come in, lowercased and without the dot.
    pub extensions: &'static [&'static str],
}

/// What every console takes on top of its own list.
///
/// RetroArch opens a zipped ROM for nearly every core there is, and somebody
/// who has compressed their library has not stopped owning it.
pub const UNIVERSAL: &[&str] = &["zip", "7z"];

/// Extensions that are never a game, whatever folder they turn up in.
///
/// The list an unknown console leans on: a folder this table has never heard of
/// is listed by taking everything out that is plainly not a ROM, rather than by
/// keeping only what is plainly one. Saves, states, patches, box art and
/// RetroArch's own files are what a ROM folder collects, and every one of them
/// sits beside the game it belongs to.
pub const NEVER: &[&str] = &[
    "srm", "sav", "state", "bak", "cfg", "opt", "cht", "rtc", "eep", "mcr", "mcd", "png", "jpg",
    "jpeg", "bmp", "gif", "webp", "txt", "nfo", "dat", "xml", "lpl", "db", "json", "md", "pdf",
    "ips", "bps", "ups", "xdelta", "sub", "log", "ini", "url", "desktop", "sh",
];

/// A save state, which is `.state`, `.state1`, `.state2` and so on for as long
/// as somebody keeps making them. Its own rule because the number is part of
/// the extension, and a list cannot hold all of them.
pub fn is_save_state(extension: &str) -> bool {
    extension
        .strip_prefix("state")
        .is_some_and(|rest| rest.chars().all(|c| c.is_ascii_digit()))
}

/// The consoles this helper knows by name.
///
/// Ordered by maker and generation rather than alphabetically, because that is
/// how the list reads when somebody is checking whether their own folder name
/// is in it. What order the *columns* come out in is not decided here — see
/// `scan`, which lists them the way the disk does.
pub const CONSOLES: &[Machine] = &[
    Machine {
        aliases: &["nes", "famicom", "fc", "nintendoentertainmentsystem"],
        title: "Nintendo Entertainment System",
        glyph: "lxb:console-nes",
        shelves: &["Nintendo - Nintendo Entertainment System"],
        cores: &["mesen", "nestopia", "fceumm", "quicknes"],
        extensions: &["nes", "unf", "unif"],
    },
    Machine {
        aliases: &["fds", "famicomdisksystem"],
        title: "Famicom Disk System",
        glyph: "lxb:console-fds",
        shelves: &["Nintendo - Family Computer Disk System"],
        cores: &["mesen", "nestopia", "fceumm"],
        extensions: &["fds"],
    },
    Machine {
        aliases: &[
            "snes",
            "sfc",
            "superfamicom",
            "supernintendo",
            "supernintendoentertainmentsystem",
        ],
        title: "Super Nintendo Entertainment System",
        glyph: "lxb:console-snes",
        shelves: &["Nintendo - Super Nintendo Entertainment System"],
        cores: &[
            "snes9x",
            "snes9x2010",
            "bsnes",
            "bsnes_mercury_balanced",
            "mesen-s",
        ],
        extensions: &["smc", "sfc", "swc", "fig", "bs", "st"],
    },
    Machine {
        aliases: &["n64", "nintendo64"],
        title: "Nintendo 64",
        glyph: "lxb:console-n64",
        shelves: &["Nintendo - Nintendo 64", "Nintendo - Nintendo 64DD"],
        cores: &["mupen64plus_next", "parallel_n64"],
        extensions: &["n64", "z64", "v64", "ndd"],
    },
    Machine {
        aliases: &["gb", "gameboy"],
        title: "Game Boy",
        glyph: "lxb:console-gb",
        shelves: &["Nintendo - Game Boy"],
        cores: &["gambatte", "sameboy", "mgba", "tgbdual"],
        extensions: &["gb"],
    },
    Machine {
        aliases: &["gbc", "gameboycolor"],
        title: "Game Boy Color",
        glyph: "lxb:console-gbc",
        shelves: &["Nintendo - Game Boy Color"],
        cores: &["gambatte", "sameboy", "mgba"],
        extensions: &["gbc", "gb"],
    },
    Machine {
        aliases: &["gba", "gameboyadvance"],
        title: "Game Boy Advance",
        glyph: "lxb:console-gba",
        shelves: &["Nintendo - Game Boy Advance"],
        cores: &["mgba", "vbam", "vba_next", "gpsp"],
        extensions: &["gba"],
    },
    Machine {
        aliases: &["nds", "ds", "nintendods"],
        title: "Nintendo DS",
        glyph: "lxb:console-nds",
        shelves: &["Nintendo - Nintendo DS", "Nintendo - Nintendo DSi"],
        cores: &["melonds", "melondsds", "desmume", "desmume2015"],
        extensions: &["nds", "dsi"],
    },
    Machine {
        aliases: &["3ds", "nintendo3ds"],
        title: "Nintendo 3DS",
        glyph: "lxb:console-3ds",
        shelves: &["Nintendo - Nintendo 3DS"],
        cores: &["citra", "citra2018", "citra_canary"],
        extensions: &["3ds", "3dsx", "cci", "cxi"],
    },
    Machine {
        aliases: &["gc", "gamecube", "nintendogamecube"],
        title: "Nintendo GameCube",
        glyph: "lxb:console-gc",
        shelves: &["Nintendo - GameCube"],
        cores: &["dolphin"],
        extensions: &["iso", "gcm", "gcz", "rvz", "ciso", "dol"],
    },
    Machine {
        aliases: &["wii", "nintendowii"],
        title: "Nintendo Wii",
        glyph: "lxb:console-wii",
        shelves: &["Nintendo - Wii"],
        cores: &["dolphin"],
        extensions: &["iso", "wbfs", "rvz", "wad", "ciso"],
    },
    Machine {
        aliases: &["vb", "virtualboy"],
        title: "Virtual Boy",
        glyph: "lxb:console-vb",
        shelves: &["Nintendo - Virtual Boy"],
        cores: &["mednafen_vb"],
        extensions: &["vb", "vboy"],
    },
    Machine {
        aliases: &["pokemini", "pokemonmini"],
        title: "Pokémon Mini",
        glyph: "lxb:console-pokemini",
        shelves: &["Nintendo - Pokemon Mini"],
        cores: &["pokemini"],
        extensions: &["min"],
    },
    Machine {
        aliases: &["psx", "ps1", "psone", "playstation"],
        title: "PlayStation",
        glyph: "lxb:console-psx",
        shelves: &["Sony - PlayStation"],
        cores: &[
            "swanstation",
            "duckstation",
            "pcsx_rearmed",
            "mednafen_psx_hw",
            "mednafen_psx",
        ],
        extensions: &["cue", "chd", "pbp", "m3u", "ecm", "iso", "img", "exe"],
    },
    Machine {
        aliases: &["ps2", "playstation2"],
        title: "PlayStation 2",
        glyph: "lxb:console-ps2",
        shelves: &["Sony - PlayStation 2"],
        cores: &["pcsx2"],
        extensions: &[
            "iso", "chd", "cso", "isz", "cue", "m3u", "mdf", "nrg", "bin",
        ],
    },
    Machine {
        aliases: &["psp", "playstationportable"],
        title: "PlayStation Portable",
        glyph: "lxb:console-psp",
        shelves: &["Sony - PlayStation Portable"],
        cores: &["ppsspp"],
        extensions: &["iso", "cso", "pbp", "chd", "elf", "prx"],
    },
    Machine {
        aliases: &[
            "genesis",
            "megadrive",
            "md",
            "smd",
            "segagenesis",
            "segamegadrive",
        ],
        title: "Sega Mega Drive",
        glyph: "lxb:console-megadrive",
        shelves: &["Sega - Mega Drive - Genesis"],
        cores: &["genesis_plus_gx", "picodrive", "blastem"],
        extensions: &["md", "smd", "gen", "bin", "sgd", "68k"],
    },
    Machine {
        aliases: &["sms", "mastersystem", "segamastersystem"],
        title: "Sega Master System",
        glyph: "lxb:console-sms",
        shelves: &["Sega - Master System - Mark III"],
        cores: &["genesis_plus_gx", "picodrive", "gearsystem", "smsplus"],
        extensions: &["sms"],
    },
    Machine {
        aliases: &["gg", "gamegear", "segagamegear"],
        title: "Sega Game Gear",
        glyph: "lxb:console-gg",
        shelves: &["Sega - Game Gear"],
        cores: &["genesis_plus_gx", "picodrive", "gearsystem"],
        extensions: &["gg"],
    },
    Machine {
        aliases: &["sg1000", "sg"],
        title: "SG-1000",
        glyph: "lxb:console-sg1000",
        shelves: &["Sega - SG-1000"],
        cores: &["genesis_plus_gx", "gearsystem"],
        extensions: &["sg"],
    },
    Machine {
        aliases: &["segacd", "megacd"],
        title: "Sega CD",
        glyph: "lxb:console-segacd",
        shelves: &["Sega - Mega-CD - Sega CD"],
        cores: &["genesis_plus_gx", "picodrive"],
        extensions: &["cue", "chd", "iso", "m3u"],
    },
    Machine {
        aliases: &["32x", "sega32x"],
        title: "Sega 32X",
        glyph: "lxb:console-32x",
        shelves: &["Sega - 32X"],
        cores: &["picodrive"],
        extensions: &["32x", "bin"],
    },
    Machine {
        aliases: &["saturn", "segasaturn"],
        title: "Sega Saturn",
        glyph: "lxb:console-saturn",
        shelves: &["Sega - Saturn"],
        cores: &["mednafen_saturn", "kronos", "yabasanshiro", "yabause"],
        extensions: &["cue", "chd", "m3u", "ccd", "iso"],
    },
    Machine {
        aliases: &["dreamcast", "dc", "segadreamcast"],
        title: "Sega Dreamcast",
        glyph: "lxb:console-dreamcast",
        shelves: &["Sega - Dreamcast"],
        cores: &["flycast"],
        extensions: &["cue", "chd", "gdi", "cdi", "m3u"],
    },
    Machine {
        aliases: &["atari2600", "2600", "vcs"],
        title: "Atari 2600",
        glyph: "lxb:console-atari2600",
        shelves: &["Atari - 2600"],
        cores: &["stella", "stella2014"],
        extensions: &["a26", "bin"],
    },
    Machine {
        aliases: &["atari5200", "5200"],
        title: "Atari 5200",
        glyph: "lxb:console-atari5200",
        shelves: &["Atari - 5200"],
        cores: &["atari800"],
        extensions: &["a52", "bin"],
    },
    Machine {
        aliases: &["atari7800", "7800"],
        title: "Atari 7800",
        glyph: "lxb:console-atari7800",
        shelves: &["Atari - 7800"],
        cores: &["prosystem"],
        extensions: &["a78", "bin"],
    },
    Machine {
        aliases: &["lynx", "atarilynx"],
        title: "Atari Lynx",
        glyph: "lxb:console-lynx",
        shelves: &["Atari - Lynx"],
        cores: &["handy", "mednafen_lynx"],
        extensions: &["lnx"],
    },
    Machine {
        aliases: &["jaguar", "atarijaguar"],
        title: "Atari Jaguar",
        glyph: "lxb:console-jaguar",
        shelves: &["Atari - Jaguar"],
        cores: &["virtualjaguar"],
        extensions: &["j64", "jag", "abs", "cof", "prg", "rom"],
    },
    Machine {
        aliases: &["pce", "pcengine", "tg16", "turbografx", "turbografx16"],
        title: "PC Engine",
        glyph: "lxb:console-pce",
        shelves: &[
            "NEC - PC Engine - TurboGrafx 16",
            "NEC - PC Engine CD - TurboGrafx-CD",
            "NEC - PC Engine SuperGrafx",
        ],
        cores: &["mednafen_pce", "mednafen_pce_fast", "mednafen_supergrafx"],
        extensions: &["pce", "sgx", "cue", "chd", "ccd"],
    },
    Machine {
        aliases: &["pcfx"],
        title: "PC-FX",
        glyph: "lxb:console-pcfx",
        shelves: &["NEC - PC-FX"],
        cores: &["mednafen_pcfx"],
        extensions: &["cue", "chd", "ccd"],
    },
    Machine {
        aliases: &["ngp", "ngpc", "neogeopocket", "neogeopocketcolor"],
        title: "Neo Geo Pocket",
        glyph: "lxb:console-ngp",
        shelves: &["SNK - Neo Geo Pocket", "SNK - Neo Geo Pocket Color"],
        cores: &["mednafen_ngp", "race"],
        extensions: &["ngp", "ngc"],
    },
    Machine {
        aliases: &["neogeo", "neogeoaes", "neogeomvs"],
        title: "Neo Geo",
        glyph: "lxb:console-neogeo",
        shelves: &["SNK - Neo Geo"],
        cores: &["fbneo", "fbalpha2012_neogeo", "mame2003_plus"],
        extensions: &["neo"],
    },
    Machine {
        aliases: &["arcade", "mame", "fbneo", "fba"],
        title: "Arcade",
        glyph: "lxb:console-arcade",
        shelves: &["MAME", "FBNeo - Arcade Games"],
        cores: &["fbneo", "mame2003_plus", "mame2010", "mame"],
        extensions: &["chd"],
    },
    Machine {
        aliases: &["wonderswan", "ws", "wsc", "wonderswancolor"],
        title: "WonderSwan",
        glyph: "lxb:console-wonderswan",
        shelves: &["Bandai - WonderSwan", "Bandai - WonderSwan Color"],
        cores: &["mednafen_wswan"],
        extensions: &["ws", "wsc", "pc2"],
    },
    Machine {
        aliases: &["3do", "panasonic3do"],
        title: "3DO",
        glyph: "lxb:console-3do",
        shelves: &["The 3DO Company - 3DO"],
        cores: &["opera"],
        extensions: &["cue", "chd", "iso", "m3u"],
    },
    Machine {
        aliases: &["colecovision", "coleco"],
        title: "ColecoVision",
        glyph: "lxb:console-colecovision",
        shelves: &["Coleco - ColecoVision"],
        cores: &["bluemsx", "gearcoleco"],
        extensions: &["col", "cv", "rom"],
    },
    Machine {
        aliases: &["intellivision", "intv"],
        title: "Intellivision",
        glyph: "lxb:console-intellivision",
        shelves: &["Mattel - Intellivision"],
        cores: &["freeintv"],
        extensions: &["int", "bin", "rom"],
    },
    Machine {
        aliases: &["vectrex"],
        title: "Vectrex",
        glyph: "lxb:console-vectrex",
        shelves: &["GCE - Vectrex"],
        cores: &["vecx"],
        extensions: &["vec", "bin"],
    },
    Machine {
        aliases: &["msx", "msx2"],
        title: "MSX",
        glyph: "lxb:console-msx",
        shelves: &["Microsoft - MSX", "Microsoft - MSX2"],
        cores: &["bluemsx", "fmsx"],
        extensions: &["rom", "mx1", "mx2", "dsk", "cas", "ri", "col"],
    },
    Machine {
        aliases: &["c64", "commodore64"],
        title: "Commodore 64",
        glyph: "lxb:console-c64",
        shelves: &["Commodore - 64"],
        cores: &["vice_x64", "vice_x64sc"],
        extensions: &["d64", "d81", "t64", "crt", "prg", "tap", "g64", "x64"],
    },
    Machine {
        aliases: &["amiga", "commodoreamiga"],
        title: "Amiga",
        glyph: "lxb:console-amiga",
        shelves: &["Commodore - Amiga"],
        cores: &["puae", "puae2021", "fsuae"],
        extensions: &["adf", "adz", "dms", "ipf", "hdf", "lha", "uae"],
    },
    Machine {
        aliases: &["zxspectrum", "spectrum", "zx"],
        title: "ZX Spectrum",
        glyph: "lxb:console-zxspectrum",
        shelves: &["Sinclair - ZX Spectrum"],
        cores: &["fuse"],
        extensions: &["tzx", "tap", "z80", "sna", "szx"],
    },
    Machine {
        aliases: &["dos", "msdos", "pcdos"],
        title: "DOS",
        glyph: "lxb:console-dos",
        shelves: &["DOS"],
        cores: &["dosbox_pure", "dosbox_core", "dosbox_svn"],
        extensions: &["exe", "com", "bat", "conf", "iso", "cue", "img"],
    },
    Machine {
        aliases: &["scummvm"],
        title: "ScummVM",
        glyph: "lxb:console-scummvm",
        shelves: &["ScummVM"],
        cores: &["scummvm"],
        extensions: &["scummvm", "svm"],
    },
];

/// The console a folder of this name is, if this table knows it.
pub fn machine(folder: &str) -> Option<&'static Machine> {
    let asked = normalise(folder);
    CONSOLES
        .iter()
        .find(|machine| machine.aliases.contains(&asked.as_str()))
}

/// A folder name reduced to the thing two spellings of it have in common:
/// lowercase, with everything that is not a letter or a digit taken out.
///
/// So `Mega Drive`, `mega-drive`, `MegaDrive` and `mega_drive` are one console,
/// and a folder called `Sega Mega Drive (32X)` is not this one — the whole name
/// has to match an alias, because a folder called `PSP Videos` is not a PSP
/// library and matching on a fragment is how it would become one.
pub fn normalise(folder: &str) -> String {
    folder
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Whether a file with this extension is a game of this console.
///
/// `None` for a folder the table does not know, where the question is asked the
/// other way round — see [`NEVER`].
pub fn plays(machine: Option<&Machine>, extension: &str) -> bool {
    let extension = extension.to_ascii_lowercase();
    if UNIVERSAL.contains(&extension.as_str()) {
        return true;
    }
    match machine {
        Some(machine) => machine.extensions.contains(&extension.as_str()),
        None => !NEVER.contains(&extension.as_str()) && !is_save_state(&extension),
    }
}

#[cfg(test)]
mod tests {
    /// The PlayStation 2 is played with pcsx2 and nothing else.
    ///
    /// It had Play! behind it for a while, put there because pcsx2 cannot start
    /// without a BIOS and Play! carries its own start-up code. Play! starts and
    /// then runs very little; compatibility is what a console is chosen for,
    /// and a missing BIOS is answered by asking for the BIOS — see the shell's
    /// `retroarch::bios_row`.
    #[test]
    fn the_playstation_2_is_played_with_pcsx2() {
        let ps2 = machine("ps2").expect("the console");
        assert_eq!(ps2.cores, ["pcsx2"]);
    }

    use super::*;

    /// The whole point of the table: the name somebody typed, however they
    /// typed it, reaching one console.
    #[test]
    fn a_folder_is_matched_however_it_is_spelt() {
        assert_eq!(
            machine("psp").map(|m| m.title),
            Some("PlayStation Portable")
        );
        assert_eq!(
            machine("PSP").map(|m| m.title),
            Some("PlayStation Portable")
        );
        assert_eq!(
            machine("Mega Drive").map(|m| m.title),
            machine("megadrive").map(|m| m.title)
        );
        assert_eq!(
            machine("mega_drive").map(|m| m.title),
            Some("Sega Mega Drive")
        );
    }

    /// And a name that only *contains* one is a different folder. `PSP Videos`
    /// is where somebody keeps films, and a shelf of them offered as games
    /// would be this integration inventing a library.
    #[test]
    fn a_name_that_merely_contains_a_console_is_not_that_console() {
        assert!(machine("PSP Videos").is_none());
        assert!(machine("old nes stuff").is_none());
    }

    /// A console nobody here has heard of still lists its games, and still
    /// leaves out what is plainly not one.
    #[test]
    fn an_unknown_folder_keeps_everything_that_could_be_a_game() {
        assert!(plays(None, "vpk"));
        assert!(plays(None, "ZIP"));
        assert!(!plays(None, "srm"));
        assert!(!plays(None, "state3"));
        assert!(!plays(None, "png"));
    }

    /// And a console that is in the table lists only what it can actually run,
    /// so a save beside a game does not become a second row.
    #[test]
    fn a_known_console_lists_only_what_it_plays() {
        let psp = machine("psp");
        assert!(plays(psp, "iso"));
        assert!(plays(psp, "cso"));
        assert!(!plays(psp, "srm"));
        assert!(!plays(psp, "nes"));
    }

    /// Every core named here is a file name this helper will go looking for, so
    /// a typo in one is a console that silently never finds its core. Nothing
    /// can check the spelling against libretro from here; what can be checked
    /// is that every console names at least one, and that none of them is
    /// written with the suffix the lookup adds.
    #[test]
    fn every_console_names_a_core_and_none_of_them_carries_the_suffix() {
        for machine in CONSOLES {
            assert!(
                !machine.cores.is_empty(),
                "{} has nothing to run it",
                machine.title
            );
            assert!(
                !machine.extensions.is_empty(),
                "{} would list nothing",
                machine.title
            );
            for core in machine.cores {
                assert!(
                    !core.contains("_libretro") && !core.ends_with(".so"),
                    "{core} is spelt as a file rather than as a core"
                );
            }
        }
    }

    /// Every console names a drawing, no two name the same one, and every name
    /// is in the shell's own namespace.
    ///
    /// The point of the set is that a column can be told from the one under it
    /// without reading either, and two consoles sharing a mark would quietly
    /// undo that for both of them. A name outside `lxb:` would be looked up in
    /// the atlas, not found, and fall back — so the column would wear
    /// RetroArch's own pad and nothing would say why.
    #[test]
    fn every_console_has_a_mark_of_its_own() {
        let mut seen: Vec<&str> = Vec::new();
        for machine in CONSOLES {
            let glyph = machine.glyph;
            assert!(
                glyph.starts_with("lxb:console-"),
                "{} names {glyph:?}, which the shell would not look up",
                machine.title
            );
            assert!(
                !seen.contains(&glyph),
                "{} and something above it both wear {glyph}",
                machine.title
            );
            seen.push(glyph);
        }
        assert_eq!(seen.len(), CONSOLES.len());
    }

    /// Two consoles claiming one folder name is a folder that opens the wrong
    /// column, and the table is long enough that it would not be noticed.
    #[test]
    fn no_two_consoles_answer_to_the_same_folder() {
        let mut seen: Vec<(&str, &str)> = Vec::new();
        for machine in CONSOLES {
            for alias in machine.aliases {
                assert_eq!(
                    *alias,
                    normalise(alias),
                    "{alias} is not written the way a folder name is reduced"
                );
                if let Some((_, other)) = seen.iter().find(|(had, _)| had == alias) {
                    panic!("{alias} is claimed by both {other} and {}", machine.title);
                }
                seen.push((alias, machine.title));
            }
        }
    }
}
