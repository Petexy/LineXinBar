//! The shell's PlayStation 3 integration, through RPCS3, as a program of its
//! own.
//!
//! The same split as `lxb-retroarch` and `lxb-heroic`, for the same reason:
//! install the package this comes in and the shell grows a PlayStation 3 row
//! and column; leave it out and the shell is exactly what it was. Nothing in
//! `lxb-desktop` depends on this crate at build time — what the two agree
//! about is one JSON record per line, [`report`], and the shell finds this
//! program beside itself or on `PATH`.
//!
//! RetroArch has no PlayStation 3 core, which is why this is a package of its
//! own rather than a console in RetroArch's column.
//!
//! ## Ten questions
//!
//! ```text
//! lxb-rpcs3 probe            is RPCS3 here, and has it the PS3's system software
//! lxb-rpcs3 setup            install whichever of the two is missing
//! lxb-rpcs3 scan [DIR]       the installed games, and the ones in the PS3 folder
//! lxb-rpcs3 key IMAGE        fetch an encrypted disc's key, where it has none
//! lxb-rpcs3 add PATH...      install packages (a game, then its updates)
//! lxb-rpcs3 permit DIR       let a flatpak RPCS3 read that folder
//! lxb-rpcs3 trophies [DIR]   every game's trophies, and which are unlocked
//! lxb-rpcs3 remove FOLDER    uninstall a game RPCS3 installed
//! lxb-rpcs3 clear-cache ID   clear what RPCS3 made for a game and can remake
//! lxb-rpcs3 mark             the PlayStation 3's mark, for the shell to draw
//! ```
//!
//! Nothing here starts a game: the shell does, with the command `probe`
//! hands it, so a game gets the loading screen and the guide's Close like
//! every row on the bar.

mod add;
mod art;
mod crypt;
mod grey;
mod iso;
mod keys;
mod net;
mod pkg;
mod remove;
mod report;
mod rpcs3;
mod scan;
mod setup;
mod sfo;
mod trophies;
mod zip;

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use report::{Probe, PROTOCOL};
use rpcs3::Console;

#[derive(Debug, Parser)]
#[command(name = "lxb-rpcs3", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    asked: Asked,
}

#[derive(Debug, Subcommand)]
enum Asked {
    /// Whether this machine has RPCS3, and whether that RPCS3 has the
    /// PlayStation 3's system software.
    Probe,
    /// Install RPCS3 into this user's own flatpaks where there is none, then
    /// the system software from Sony where RPCS3 has none. One line per
    /// change, ending on `done` or `failed`.
    Setup,
    /// The games RPCS3 has installed, and the ones in this folder.
    Scan {
        #[arg(value_name = "DIR")]
        folder: Option<PathBuf>,
    },
    /// Make sure RPCS3 has the key of this encrypted disc image, fetching it
    /// from Redump's published collection where it has not.
    Key {
        #[arg(value_name = "IMAGE")]
        image: PathBuf,
    },
    /// Install packages — each a `.pkg`, or a zip with one and its licence in
    /// it — through RPCS3's own installer, in the order given: a game, then
    /// its updates. One bar over all of them.
    Add {
        #[arg(value_name = "PATH", required = true)]
        paths: Vec<PathBuf>,
    },
    /// Let a flatpak RPCS3 read this folder.
    Permit {
        #[arg(value_name = "DIR")]
        folder: PathBuf,
    },
    /// Uninstall a game RPCS3 installed — its folder under `dev_hdd0/game`
    /// and what RPCS3 compiled for it — keeping its saves, trophies and
    /// licence.
    Remove {
        #[arg(value_name = "FOLDER")]
        folder: PathBuf,
    },
    /// Clear what RPCS3 compiled for the game with this serial, and the game's
    /// system cache. RPCS3 makes them again the next time the game starts.
    ClearCache {
        #[arg(value_name = "SERIAL")]
        serial: String,
    },
    /// The mark the shell draws the PlayStation 3 row, column and icon-less
    /// games with — this package's own, carried in this program so it is here
    /// wherever the program is, installed or not. The copy in
    /// `share/lxb/glyphs` is read first where there is one.
    Mark,
    /// The trophies of every game `scan` would list, and which of them RPCS3
    /// has recorded as unlocked. One line per trophy set, then one with
    /// `done` set.
    Trophies {
        #[arg(value_name = "DIR")]
        folder: Option<PathBuf>,
        /// The language to name them in, as the shell's catalogs are named —
        /// `pl`, `pt-BR` — where the game has it.
        #[arg(long)]
        language: Option<String>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let mut out = std::io::stdout().lock();
    // Before anything is asked of the machine: the shell asks for this while
    // it starts, and it needs nothing but this program.
    if let Asked::Mark = cli.asked {
        return say(
            &mut out,
            &report::Mark {
                protocol: PROTOCOL,
                name: "ps3".to_string(),
                drawing: include_str!("../glyphs/ps3.svg").to_string(),
            },
        );
    }
    let installation = rpcs3::installation();
    let console = installation
        .as_ref()
        .and_then(rpcs3::config_dir)
        .map(|config| Console::of(&config));

    match cli.asked {
        Asked::Probe => {
            if let Some(found) = &installation {
                eprintln!("rpcs3: {:?} {:?}", found.kind, found.command);
            }
            say(
                &mut out,
                &Probe {
                    protocol: PROTOCOL,
                    firmware: console.as_ref().and_then(Console::firmware),
                    config: console
                        .as_ref()
                        .map(|console| console.config.to_string_lossy().into_owned()),
                    rpcs3: installation,
                    flatpak: rpcs3::flatpak_available(),
                },
            )
        }
        Asked::Setup => ended(setup::run(&mut out)),
        Asked::Scan { folder } => {
            let art = art::art_root();
            let mut library = scan::library(folder.as_deref(), console.as_ref(), art.as_deref());
            if let Some(console) = &console {
                let cache = installation.as_ref().and_then(rpcs3::cache_dir);
                for game in &mut library.games {
                    if let Some(serial) = game.serial.as_deref() {
                        game.cache = remove::cache_size(serial, console, cache.as_deref());
                    }
                }
            }
            say(&mut out, &library)
        }
        Asked::Key { image } => {
            let Some(console) = console else {
                return say(
                    &mut out,
                    &report::KeyResult {
                        protocol: PROTOCOL,
                        key: report::Key::Missing,
                        trouble: Some(report::Trouble::NoRpcs3),
                        note: "no RPCS3".to_string(),
                    },
                );
            };
            let result = keys::fetch(&image, &console, art::cache_root().as_deref());
            eprintln!("key: {}", result.note);
            say(&mut out, &result)
        }
        Asked::Add { paths } => ended(add::run(&mut out, &paths)),
        Asked::Permit { folder } => say(&mut out, &setup::permit(&folder)),
        Asked::Remove { folder } => {
            let removal = match &console {
                Some(console) => remove::run(
                    &folder,
                    console,
                    installation.as_ref().and_then(rpcs3::cache_dir).as_deref(),
                ),
                None => report::Removal {
                    protocol: PROTOCOL,
                    removed: false,
                    trouble: Some(report::Trouble::NoRpcs3),
                    freed: 0,
                    note: "no RPCS3".to_string(),
                },
            };
            eprintln!("remove: {}", removal.note);
            say(&mut out, &removal)
        }
        Asked::Mark => unreachable!("answered before the machine was asked"),
        Asked::ClearCache { serial } => {
            let cleared = match &console {
                Some(console) => remove::clear_cache(
                    &serial,
                    console,
                    installation.as_ref().and_then(rpcs3::cache_dir).as_deref(),
                ),
                None => report::Removal {
                    protocol: PROTOCOL,
                    removed: false,
                    trouble: Some(report::Trouble::NoRpcs3),
                    freed: 0,
                    note: "no RPCS3".to_string(),
                },
            };
            eprintln!("clear-cache: {}", cleared.note);
            say(&mut out, &cleared)
        }
        Asked::Trophies { folder, language } => {
            let art = art::art_root();
            let library = scan::library(folder.as_deref(), console.as_ref(), art.as_deref());
            for set in trophies::all(
                &library.games,
                console.as_ref(),
                art::cache_root().as_deref(),
                language.as_deref(),
            ) {
                say(&mut out, &set);
            }
            say(&mut out, &trophies::end())
        }
    }
}

fn say(out: &mut impl Write, record: &impl serde::Serialize) -> ExitCode {
    match serde_json::to_string(record) {
        Ok(json) => {
            let _ = writeln!(out, "{json}");
            let _ = out.flush();
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("lxb-rpcs3: {err}");
            ExitCode::FAILURE
        }
    }
}

fn ended(done: bool) -> ExitCode {
    if done {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
