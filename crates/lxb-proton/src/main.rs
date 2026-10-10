//! The shell's Windows Apps integration, through Proton, as a program of its
//! own.
//!
//! The same split as `lxb-heroic` and `lxb-rpcs3`, and for the same reason:
//! install the package this comes in and the shell grows a Windows Apps row
//! and column; leave it out and the shell is exactly what it was. Nothing in
//! `lxb-desktop` depends on this crate at build time — what the two agree
//! about is one JSON record per line, [`report`], and the shell finds this
//! program on `PATH`.
//!
//! ## The questions
//!
//! ```text
//! lxb-proton mark              the drawing this package carries
//! lxb-proton icon EXE --to P   the drawing one Windows program carries
//! lxb-proton disc DIR --to P   what the disc mounted there runs, and its
//!                              picture [--label LABEL]
//! lxb-proton probe             what a Windows game would run with here
//! lxb-proton tools             everything it could run with, and which one
//! lxb-proton offer             what a machine with none would have to fetch
//! lxb-proton fetch             fetch it, checking everything on the way
//! lxb-proton folder DIR        keep new games there from now on
//! lxb-proton games             the Windows games on this machine
//! lxb-proton start ID          how to run one [--in EXE] [--compatibility N]
//!                              [--desktop WxH]
//! lxb-proton use ID            run it under this Proton from now on [--proton N]
//! lxb-proton newer             what is newer than the build we fetched
//! lxb-proton end ID            leave nothing of its Windows running
//! lxb-proton remove ID         take one off the disk, prefix and all
//! lxb-proton new EXE           a prefix of its own, and how to run that
//!                              program where it is [--desktop WxH]
//!                              [--disc-label L --disc-uuid U] [-- ARGS]
//! lxb-proton import DIR        a game somebody already has, copied into one
//! lxb-proton found ID          what in that prefix could be the game
//! lxb-proton settled ID        what the run in there left: it, or what it
//!                              installed
//! lxb-proton target ID AT      that is the one; play it from now on
//! ```
//!
//! ## Whose Proton is whose
//!
//! With Steam here, games run under the Protons Steam installed — read where
//! they lie and never written to. With no Steam, this fetches one of its own
//! into `$XDG_DATA_HOME/lxb/proton` and that is the only Proton it ever
//! writes to. Which build, and why it is not Valve's Experimental, is
//! `PROTON-INTEGRATION-TO-DO.MD`: Experimental can be downloaded without a
//! Steam account, but the container runtime it asks for cannot, and the only
//! road to a Valve build is a 32-bit binary this would then have to ship on
//! every distribution.
//!
//! Nothing here starts a game: that is the shell's machinery, with what
//! `probe` and `tools` hand it. Nothing is remembered between runs except
//! the two things that are facts about the disk — which Protons are on it,
//! and which folders prefixes have been made in.

mod disc;
mod fetch;
mod icon;
mod install;
mod launch;
mod paths;
mod pe;
mod prefix;
mod report;
mod runtime;
mod steam;
mod tools;

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use paths::Paths;
use report::{
    Candidate, Disc, DiscReading, Download, Ended, Found, GameRow, GameState, Games, Made, Offer,
    Probe, Progress, Removal, Stage, Start, Tools, Trouble, PROTOCOL,
};
use steam::Steam;

/// The file name this package's drawing has under `share/lxb/glyphs`,
/// without `.svg`. The shell draws it as `lxb:<name>`.
const MARK: &str = "windows";

#[derive(Debug, Parser)]
#[command(name = "lxb-proton", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    asked: Asked,
}

#[derive(Debug, Subcommand)]
enum Asked {
    /// The drawing this package carries for the row, the column and every
    /// game without a picture of its own.
    ///
    /// Answered before anything on this machine is looked at, so the shell
    /// can get the mark out of a helper it has only just found.
    Mark,
    /// The drawing one Windows program wears, taken out of the program and
    /// written where this says.
    ///
    /// For a row of the shell's file explorer. A `.exe` sitting in
    /// somebody's Downloads is not a game: it has no folder of its own for a
    /// picture to live in, it may be on a disk nobody can write to, and it
    /// is asked about because somebody is looking at that folder right now.
    /// So the shell names a scratch file of its own, reads the PNG out of it
    /// and drops it — exactly the bargain it makes with
    /// `ffmpegthumbnailer` for a frame of a film.
    ///
    /// A game's own picture is not this: that one is written beside the game
    /// and kept, because it is drawn on every listing of the column for as
    /// long as the game is installed. See `icon::beside`.
    ///
    /// Answered before this machine is looked at, as `mark` is. It reads the
    /// one file it was handed and writes the one it was told to, and a home
    /// directory is no part of either.
    Icon {
        #[arg(value_name = "EXE")]
        exe: std::path::PathBuf,
        /// Write the PNG here. There is no default: a picture this program
        /// put somewhere of its own choosing would be a file nobody came
        /// back for.
        #[arg(long = "to", value_name = "PNG")]
        to: std::path::PathBuf,
    },
    /// Whether the disc mounted at DIR carries a Windows program, which one a
    /// press runs, what the disc is called and what it wears — written to
    /// the PNG named, as `icon` writes one.
    ///
    /// Read the way Windows reads a disc that has just gone in: its
    /// `autorun.inf`, and without one the program at its top that is plainly
    /// the way in. See `disc`'s own head.
    ///
    /// Answered before this machine is looked at, as `icon` is: it reads the
    /// disc it was pointed at and writes the file it was told to.
    Disc {
        #[arg(value_name = "DIR")]
        dir: std::path::PathBuf,
        /// What the disc's own file system is called, which names the row
        /// where `autorun.inf` does not. Read as a value even where it starts
        /// with a dash, as every name and label here is: a disc can be called
        /// `-= GAME =-`.
        #[arg(long, value_name = "LABEL", allow_hyphen_values = true)]
        label: Option<String>,
        /// Write the disc's picture here. Nothing is written for a disc that
        /// carries none.
        #[arg(long = "to", value_name = "PNG")]
        to: std::path::PathBuf,
    },
    /// What a Windows game would be run with on this machine, whether Steam
    /// is installed, and where the games are kept.
    Probe,
    /// Everything this machine could run a Windows game with — Valve's own
    /// Protons, anybody's in Steam's compatibility folder, and the one this
    /// program fetched — and which of them a new game would use.
    Tools,
    /// What a machine with no usable Proton would have to fetch, named and
    /// measured. Nothing is downloaded; this is the question the panel asks
    /// before offering.
    Offer,
    /// What is newer than the Proton and runtime this program fetched, for
    /// Settings > Updates. Nothing is downloaded.
    ///
    /// A different question from `offer`, which asks what a machine with
    /// nothing would need. This one asks only about the build under this
    /// program's own store: Steam's Protons are Steam's to update, and a
    /// machine with no managed build has nothing here to update at all.
    Newer,
    /// Fetch what `offer` named, checking every download against the digest
    /// published beside it. One line per change, ending on `done` or
    /// `failed`; end the process to stop it, and asking again starts the
    /// unfinished half over.
    Fetch,
    /// Keep new games in this folder from now on. The games already
    /// installed stay where they are and are still found.
    Folder {
        #[arg(value_name = "DIR")]
        dir: std::path::PathBuf,
    },
    /// The Windows games on this machine, finished and unfinished alike.
    Games,
    /// How to run one of them: the command, the environment and the folder
    /// to run it in. Nothing is started — the shell starts it, so the game
    /// gets the loading screen and the display it was pressed on.
    Start {
        #[arg(value_name = "ID")]
        id: String,
        /// Run this executable inside the game's prefix instead of the game
        /// itself, building nothing and upgrading nothing — what the shell's
        /// **Run something inside a game** asks for.
        #[arg(long = "in", value_name = "EXE")]
        inside: Option<std::path::PathBuf>,
        /// Run it under this Proton rather than the one the game is set to,
        /// by name, as `tools` lists them.
        #[arg(long, value_name = "NAME")]
        compatibility: Option<String>,
        /// How many pixels across and down the display this is being started
        /// on has, as `1280x800`.
        ///
        /// Only a setup — `--in` — uses it, and then it is the Windows
        /// desktop that setup opens on, so its window lands in the middle of
        /// the screen rather than in the corner. The game itself is run
        /// against the display itself whatever is passed here.
        #[arg(long, value_name = "WxH", value_parser = a_display)]
        desktop: Option<launch::Desktop>,
    },
    /// Make a prefix of its own for this executable and say how to run it
    /// where it already is. For an installer, and for a program that needs
    /// none — nothing is copied either way; see `install`'s head.
    New {
        #[arg(value_name = "EXE")]
        exe: std::path::PathBuf,
        /// What to call it. The executable's own name where this is not
        /// given.
        #[arg(long, value_name = "NAME", allow_hyphen_values = true)]
        name: Option<String>,
        /// The display this is being started on, as `1280x800` — the Windows
        /// desktop the setup opens on. See `start`.
        #[arg(long, value_name = "WxH", value_parser = a_display)]
        desktop: Option<launch::Desktop>,
        /// The label of the disc this program is on, where it is on one.
        ///
        /// With `--disc-uuid`, what makes this the disc's own Windows rather
        /// than a program somebody pressed: it is kept on the game, a disc
        /// that already has a Windows runs in it again rather than in a new
        /// one, and the prefix is built before the program is first run so
        /// the program finds its disc where Windows would put it. See
        /// `report::Disc`.
        #[arg(long = "disc-label", value_name = "LABEL", allow_hyphen_values = true)]
        disc_label: Option<String>,
        /// When the disc's file system was made — UDisks' `IdUUID` for it.
        #[arg(long = "disc-uuid", value_name = "UUID", allow_hyphen_values = true)]
        disc_uuid: Option<String>,
        /// What to put after the program — what a disc's `autorun.inf`
        /// puts after it.
        #[arg(last = true, value_name = "ARGS")]
        arguments: Vec<String>,
    },
    /// Make a prefix of its own and copy a game somebody already has into
    /// it — the whole folder, because an executable without its data files
    /// is a game that starts and stops.
    Import {
        #[arg(value_name = "DIR")]
        dir: std::path::PathBuf,
        #[arg(long, value_name = "NAME", allow_hyphen_values = true)]
        name: Option<String>,
    },
    /// What inside a game's prefix could be the game, likeliest first, for
    /// the shell to confirm one with the person.
    Found {
        #[arg(value_name = "ID")]
        id: String,
    },
    /// What the run in a prefix left behind: whether the program it plays
    /// is still the one that was pressed, or something that run installed.
    ///
    /// Asked once each time something in that prefix exits. A program that
    /// put another program in there was a setup, so the installed one
    /// becomes what the game plays; a program that put nothing there was the
    /// program itself, and nothing changes.
    ///
    /// Answers with the one row, as `target` does. A game whose program is
    /// already inside its own prefix — a folder somebody imported, an
    /// installation that has settled — answers with no row at all, which is
    /// not a failure.
    Settled {
        #[arg(value_name = "ID")]
        id: String,
    },
    /// That one is the game: play it from now on.
    Target {
        #[arg(value_name = "ID")]
        id: String,
        /// Where it is inside the game's own folder, as `found` lists it.
        #[arg(value_name = "AT")]
        at: String,
        /// And call the game this.
        #[arg(long, value_name = "NAME", allow_hyphen_values = true)]
        name: Option<String>,
    },
    /// Run one game under this Proton from now on, by name, as `tools`
    /// lists them — what the shell's **Compatibility** answers with.
    ///
    /// Remembered on the game rather than applied once, and remembered even
    /// where that Proton is Steam's and Steam later goes: a machine that
    /// loses Steam falls back to what it has and finds the choice again when
    /// Steam comes back.
    Use {
        #[arg(value_name = "ID")]
        id: String,
        /// The Proton to run it under. Leave it out to go back to whatever
        /// this machine would choose today.
        #[arg(long, value_name = "NAME")]
        proton: Option<String>,
    },
    /// Leave nothing of this game's Windows running: its `wineserver`, the
    /// services under it, and the container around them.
    ///
    /// **What keeps a Windows game from outliving the screen it was on.**
    /// Wine is not one process, and a `wineserver` goes on holding a prefix
    /// open for as long as any client is alive — so a game that leaves one
    /// behind leaves a whole Wine running with nothing on screen to say so.
    /// Asked politely first, then not; see the helper's `prefix::end`.
    ///
    /// Safe to ask about a game that is already over, which is the ordinary
    /// case: it finds nothing and says so at once.
    End {
        #[arg(value_name = "ID")]
        id: String,
    },
    /// Take a game off the disk: its whole folder, the Windows installation
    /// inside it, and the saves kept beside it. Refused while something is
    /// still running in that prefix.
    Remove {
        #[arg(value_name = "ID")]
        id: String,
    },
}

/// A display, as the shell writes one on the command line: `1280x800`.
///
/// Refused rather than rounded where it is not two numbers: a size this
/// helper made up would be a Windows desktop somebody's installer opens in
/// the middle of and nobody can see the edges of.
fn a_display(written: &str) -> Result<launch::Desktop, String> {
    let bad = || format!("{written} is not a size like 1280x800");
    let (across, down) = written.split_once(['x', 'X']).ok_or_else(bad)?;
    let across: u32 = across.trim().parse().map_err(|_| bad())?;
    let down: u32 = down.trim().parse().map_err(|_| bad())?;
    if across == 0 || down == 0 {
        return Err(bad());
    }
    Ok(launch::Desktop {
        width: across,
        height: down,
    })
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let mut out = std::io::stdout().lock();

    // Before the machine is looked at: a shell that has just found this
    // program asks for the drawing, and a home directory is no part of that.
    if matches!(cli.asked, Asked::Mark) {
        return say(
            &mut out,
            &report::Mark {
                protocol: PROTOCOL,
                name: MARK.to_string(),
                drawing: include_str!("../glyphs/windows.svg").to_string(),
            },
        );
    }

    // And before it as well, for the same reason: this reads the one file it
    // was handed and writes the one it was told to. The shell asks about a
    // `.exe` on whatever disk somebody is looking at, which may be before
    // anything at all has been installed.
    if let Asked::Icon { exe, to } = &cli.asked {
        let written = icon::written_to(exe, to);
        let note = match written {
            true => format!("{} carries a drawing of its own", exe.display()),
            // Not a failure. Most of the four endings Windows runs carry no
            // resources at all — a `.bat` is a text file — and plenty of
            // real programs carry no icon either.
            false => format!("{} carries no drawing this can read", exe.display()),
        };
        eprintln!("icon: {note}");
        return say(
            &mut out,
            &report::Icon {
                protocol: PROTOCOL,
                png: written.then(|| to.to_string_lossy().into_owned()),
                note,
            },
        );
    }

    // And a disc, for the same reason as `icon`: the disc it was pointed at
    // and the one picture it was told to write are the whole of what this
    // touches.
    if let Asked::Disc { dir, label, to } = &cli.asked {
        let read = disc::read(dir, label.as_deref());
        let png = read
            .as_ref()
            .and_then(|read| read.png.as_ref())
            .filter(|png| std::fs::write(to, png).is_ok())
            .map(|_| to.to_string_lossy().into_owned());
        let note = match &read {
            Some(read) => format!(
                "{} runs {}{}",
                dir.display(),
                read.program.display(),
                if png.is_some() {
                    ", and wears a picture"
                } else {
                    ""
                }
            ),
            None => format!("{} carries nothing for Windows to run", dir.display()),
        };
        eprintln!("disc: {note}");
        return say(
            &mut out,
            &DiscReading {
                protocol: PROTOCOL,
                windows: read.is_some(),
                name: read.as_ref().map(|read| read.name.clone()),
                program: read
                    .as_ref()
                    .map(|read| read.program.to_string_lossy().into_owned()),
                arguments: read
                    .as_ref()
                    .map(|read| read.arguments.clone())
                    .unwrap_or_default(),
                png,
                note,
            },
        );
    }

    let Some(mut paths) = Paths::of_this_user() else {
        eprintln!("lxb-proton: there is no home directory to keep games in");
        return ExitCode::FAILURE;
    };
    let steam = Steam::of_this_user();

    match cli.asked {
        Asked::Mark | Asked::Icon { .. } | Asked::Disc { .. } => {
            unreachable!("answered above, before the machine is read")
        }
        Asked::Probe => {
            let found = tools::list(&steam, paths.store());
            let chosen = tools::chosen(&found).cloned();
            eprintln!(
                "probe: steam {}, {} proton(s), using {}, new games in {}",
                if steam.found { "here" } else { "not here" },
                found.len(),
                chosen.as_ref().map_or("none", |tool| tool.name.as_str()),
                paths.new_prefixes().display()
            );
            say(
                &mut out,
                &Probe {
                    protocol: PROTOCOL,
                    proton: chosen,
                    steam: steam.found,
                    store: paths.store().to_string_lossy().into_owned(),
                    prefix_roots: paths
                        .roots()
                        .iter()
                        .map(|at| at.to_string_lossy().into_owned())
                        .collect(),
                },
            )
        }
        Asked::Tools => {
            let found = tools::list(&steam, paths.store());
            for tool in &found {
                eprintln!(
                    "tools: {} ({:?}) runtime {}",
                    tool.name,
                    tool.origin,
                    tool.runtime.as_ref().map_or("none named".to_string(), |r| {
                        match (&r.suite, &r.entry_point) {
                            (Some(suite), Some(_)) => format!("{suite}, here"),
                            (Some(suite), None) => format!("{suite}, missing"),
                            (None, _) => format!("{} — unknown generation", r.app_id),
                        }
                    })
                );
            }
            let chosen = tools::chosen(&found).map(|tool| tool.name.clone());
            say(
                &mut out,
                &Tools {
                    protocol: PROTOCOL,
                    tools: found,
                    chosen,
                    steam: steam.found,
                },
            )
        }
        Asked::Offer => {
            let offer = offer(&steam, &paths);
            eprintln!("offer: {} ({:?})", offer.note, offer.trouble);
            let worked = offer.trouble.is_none();
            say(&mut out, &offer);
            ended(worked)
        }
        Asked::Newer => {
            let offer = newer(&steam, &paths);
            eprintln!("newer: {} ({:?})", offer.note, offer.trouble);
            let worked = offer.trouble.is_none();
            say(&mut out, &offer);
            ended(worked)
        }
        Asked::Fetch => ended(run_fetch(&mut out, &steam, &paths)),
        Asked::Games => {
            let games = prefix::all(paths.roots());
            eprintln!("games: {} found", games.len());
            say(
                &mut out,
                &Games {
                    protocol: PROTOCOL,
                    games: games.iter().map(row).collect(),
                },
            )
        }
        Asked::Start {
            id,
            inside,
            compatibility,
            desktop,
        } => {
            let start = start(
                &steam,
                &paths,
                &id,
                inside.as_deref(),
                compatibility.as_deref(),
                desktop,
            );
            eprintln!("start: {} ({:?})", start.note, start.trouble);
            let worked = start.trouble.is_none();
            say(&mut out, &start);
            ended(worked)
        }
        Asked::End { id } => {
            let gone = end_everything(&paths, &id);
            eprintln!("end: {} ({:?})", gone.note, gone.trouble);
            let worked = gone.trouble.is_none();
            say(&mut out, &gone);
            ended(worked)
        }
        Asked::New {
            exe,
            name,
            desktop,
            disc_label,
            disc_uuid,
            arguments,
        } => {
            // Either half says it is a disc. A disc pressed without a label,
            // or whose file system carries no date, is still a disc.
            let disc = (disc_label.is_some() || disc_uuid.is_some()).then(|| Disc {
                label: disc_label.unwrap_or_default(),
                uuid: disc_uuid.unwrap_or_default(),
            });
            let made = make_new(
                &steam,
                &paths,
                &exe,
                name.as_deref(),
                desktop,
                disc.as_ref(),
                &arguments,
            );
            eprintln!("new: {} ({:?})", made.note, made.trouble);
            let worked = made.trouble.is_none();
            say(&mut out, &made);
            ended(worked)
        }
        Asked::Settled { id } => match settle(&paths, &id) {
            Some(game) => say(
                &mut out,
                &Games {
                    protocol: PROTOCOL,
                    games: vec![game],
                },
            ),
            // Not a failure, and not silence either: the shell asks this of
            // anything that ends in a prefix, and a game nothing was brought
            // into has nothing to settle. An empty list says so in the shape
            // the shell already reads, where saying nothing at all is read as
            // a helper that could not answer.
            None => {
                eprintln!("settled: {id} plays something in its own prefix already");
                say(
                    &mut out,
                    &Games {
                        protocol: PROTOCOL,
                        games: Vec::new(),
                    },
                )
            }
        },
        Asked::Import { dir, name } => ended(run_import(&mut out, &paths, &dir, name.as_deref())),
        Asked::Found { id } => {
            let games = prefix::all(paths.roots());
            let Some(game) = games.iter().find(|game| game.id == id) else {
                eprintln!("found: there is no game called {id}");
                return ExitCode::FAILURE;
            };
            let candidates = install::candidates(&game.at);
            eprintln!("found: {} executable(s) in {id}", candidates.len());
            say(
                &mut out,
                &Found {
                    protocol: PROTOCOL,
                    id,
                    candidates: candidates
                        .into_iter()
                        .map(|found| Candidate {
                            at: found.at,
                            name: found.name,
                            bytes: found.bytes,
                        })
                        .collect(),
                },
            )
        }
        Asked::Target { id, at, name } => {
            let games = prefix::all(paths.roots());
            let Some(mut game) = games.into_iter().find(|game| game.id == id) else {
                eprintln!("target: there is no game called {id}");
                return ExitCode::FAILURE;
            };
            // Only ever something inside this game's own folder: the path
            // comes back from the shell, and a path that climbed out of the
            // prefix would make a shortcut to anywhere on the disk.
            if !inside_the_game(&game.at, &at) {
                eprintln!("target: {at} is not inside {id}");
                return ExitCode::FAILURE;
            }
            // What a disc's `autorun.inf` put after its program belonged to
            // that program, and is no part of whatever it installed.
            if game.runs_in_place() {
                game.kept.arguments.clear();
            }
            game.kept.play = Some(at);
            if let Some(name) = name {
                game.kept.name = name;
            }
            if let Err(err) = game.save() {
                eprintln!("target: {err}");
                return ExitCode::FAILURE;
            }
            eprintln!("target: {} plays {:?}", game.id, game.kept.play);
            say(
                &mut out,
                &Games {
                    protocol: PROTOCOL,
                    games: vec![row(&game)],
                },
            )
        }
        Asked::Use { id, proton } => {
            let games = prefix::all(paths.roots());
            let Some(mut game) = games.into_iter().find(|game| game.id == id) else {
                eprintln!("use: there is no game called {id}");
                return ExitCode::FAILURE;
            };
            // A name that is not on this machine is refused here rather than
            // written down and tripped over at the next launch. `start`
            // forgives one that disappears later, which is a different
            // thing: this is somebody choosing, and a choice that cannot be
            // honoured is not a choice.
            if let Some(name) = &proton {
                let found = tools::list(&steam, paths.store());
                if !found.iter().any(|tool| &tool.name == name) {
                    eprintln!("use: there is no Proton called {name} on this machine");
                    return ExitCode::FAILURE;
                }
            }
            game.kept.proton = proton;
            if let Err(err) = game.save() {
                eprintln!("use: {err}");
                return ExitCode::FAILURE;
            }
            eprintln!("use: {} runs under {:?}", game.id, game.kept.proton);
            say(
                &mut out,
                &Games {
                    protocol: PROTOCOL,
                    games: vec![row(&game)],
                },
            )
        }
        Asked::Remove { id } => {
            let removal = remove(&paths, &id);
            eprintln!("remove: {} ({:?})", removal.note, removal.trouble);
            let removed = removal.removed;
            say(&mut out, &removal);
            ended(removed)
        }
        Asked::Folder { dir } => {
            if !dir.is_absolute() {
                eprintln!("folder: {} is not an absolute path", dir.display());
                return ExitCode::FAILURE;
            }
            if let Err(err) = std::fs::create_dir_all(&dir) {
                eprintln!("folder: {}: {err}", dir.display());
                return ExitCode::FAILURE;
            }
            if let Err(err) = paths.use_root(&dir) {
                eprintln!("folder: {err}");
                return ExitCode::FAILURE;
            }
            eprintln!("folder: new games go in {}", dir.display());
            say(
                &mut out,
                &Probe {
                    protocol: PROTOCOL,
                    proton: tools::chosen(&tools::list(&steam, paths.store())).cloned(),
                    steam: steam.found,
                    store: paths.store().to_string_lossy().into_owned(),
                    prefix_roots: paths
                        .roots()
                        .iter()
                        .map(|at| at.to_string_lossy().into_owned())
                        .collect(),
                },
            )
        }
    }
}

/// What this machine is missing, if anything.
///
/// Both halves are asked separately, because they can be missing separately:
/// a machine with Steam usually has the runtime already and needs only a
/// Proton, and a machine with a Proton whose generation has moved on needs
/// only a runtime.
fn offer(steam: &Steam, paths: &Paths) -> Offer {
    let found = tools::list(steam, paths.store());
    let chosen = tools::chosen(&found);

    // Something already here that can start a game: nothing to fetch.
    if chosen.is_some_and(|tool| {
        tool.runtime
            .as_ref()
            .is_some_and(|runtime| runtime.entry_point.is_some())
    }) {
        return Offer {
            protocol: PROTOCOL,
            proton: None,
            runtime: None,
            trouble: None,
            note: "this machine can already run a Windows game".to_string(),
        };
    }

    let managed = found
        .iter()
        .find(|tool| tool.origin == report::Origin::Managed);
    let release = match fetch::newest_proton() {
        Ok(release) => release,
        Err((trouble, note)) => {
            return Offer {
                protocol: PROTOCOL,
                proton: None,
                runtime: None,
                trouble: Some(trouble),
                note,
            }
        }
    };
    let have_this_one = managed.is_some_and(|tool| tool.name == release.tag);

    // Which runtime, asked of whichever Proton will actually be used: the
    // managed one if it is already here, otherwise the one about to be
    // fetched. Both ask for the same generation today, and that is exactly
    // the assumption this avoids making.
    let needed = managed
        .and_then(|tool| tool.runtime.as_ref())
        .map(|runtime| (runtime.app_id, runtime.entry_point.is_some()));
    let runtime_offer = match needed {
        Some((_, true)) => None,
        Some((app_id, false)) => match runtime::suite(app_id) {
            Some(suite) => match fetch::published_runtime(suite) {
                Ok(version) => Some(Download {
                    name: Some(version),
                    bytes: None,
                    already: false,
                }),
                Err((trouble, note)) => {
                    return Offer {
                        protocol: PROTOCOL,
                        proton: None,
                        runtime: None,
                        trouble: Some(trouble),
                        note,
                    }
                }
            },
            None => {
                return Offer {
                    protocol: PROTOCOL,
                    proton: None,
                    runtime: None,
                    trouble: Some(Trouble::UnknownRuntime),
                    note: format!("this build has never heard of runtime {app_id}"),
                }
            }
        },
        // Nothing managed here yet, so the runtime is whatever the Proton
        // about to arrive asks for — which cannot be known until it has. It
        // is offered unmeasured rather than guessed at.
        None => Some(Download {
            name: None,
            bytes: None,
            already: false,
        }),
    };

    Offer {
        protocol: PROTOCOL,
        proton: Some(Download {
            name: Some(release.tag),
            bytes: Some(release.size).filter(|size| *size > 0),
            already: have_this_one,
        }),
        runtime: runtime_offer,
        trouble: None,
        note: "a Proton and its runtime are needed".to_string(),
    }
}

/// What is newer than the build this program fetched.
///
/// Only ever about `$XDG_DATA_HOME/lxb/proton/`. Two things follow from
/// that and both are deliberate:
///
/// - **A machine with no managed build has nothing to update here**, even
///   where it has five of Steam's Protons. They are the client's, it keeps
///   them current, and this program never writes into them.
/// - **The runtime is only ours when it resolved to ours.** A managed
///   Proton on a machine with Steam runs under Steam's runtime — see
///   `runtime::find`, which prefers it so a machine does not keep two copies
///   of the same hundred and sixty megabytes — and that one is not ours to
///   replace.
fn newer(steam: &Steam, paths: &Paths) -> Offer {
    let found = tools::list(steam, paths.store());
    decide_newer(&found, fetch::newest_proton, |suite| {
        fetch::published_runtime(suite)
    })
}

/// The whole of what `newer` decides, with the two questions it asks of the
/// network handed in.
///
/// Split out so the decision can be tested without one: what is installed
/// and what is published are the only inputs, and every branch here is
/// about a machine somebody really has — ours and nothing else, ours beside
/// Steam's, ours already current. `releases::plan` in `lxb-updates` is split
/// the same way and for the same reason.
fn decide_newer(
    found: &[report::Tool],
    newest: impl Fn() -> Result<fetch::Release, fetch::Failure>,
    published: impl Fn(&runtime::Suite) -> Result<String, fetch::Failure>,
) -> Offer {
    let nothing = |note: &str| Offer {
        protocol: PROTOCOL,
        proton: None,
        runtime: None,
        trouble: None,
        note: note.to_string(),
    };
    let failed = |(trouble, note): (Trouble, String)| Offer {
        protocol: PROTOCOL,
        proton: None,
        runtime: None,
        trouble: Some(trouble),
        note,
    };

    let Some(managed) = found
        .iter()
        .find(|tool| tool.origin == report::Origin::Managed)
    else {
        return nothing("there is no Proton of ours on this machine");
    };

    let release = match newest() {
        Ok(release) => release,
        Err(failure) => return failed(failure),
    };
    let proton = (release.tag != managed.name).then(|| Download {
        name: Some(release.tag.clone()),
        bytes: Some(release.size).filter(|size| *size > 0),
        already: false,
    });

    // The runtime, asked of the Proton that would be in force. Where it
    // resolved to Steam's copy there is nothing here to do, whatever is
    // published.
    let ours = managed
        .runtime
        .as_ref()
        .filter(|runtime| runtime.origin != Some(report::Origin::Valve));
    let runtime = match ours {
        None => None,
        Some(runtime) => {
            let Some(suite) = runtime::suite(runtime.app_id) else {
                return failed((
                    Trouble::UnknownRuntime,
                    format!("this build has never heard of runtime {}", runtime.app_id),
                ));
            };
            let published = match published(suite) {
                Ok(published) => published,
                Err(failure) => return failed(failure),
            };
            // A runtime that is not on the disk at all is one to fetch, and
            // one at another version is one to replace. Equal is neither.
            (runtime.version.as_deref() != Some(published.as_str())).then_some(Download {
                name: Some(published),
                bytes: None,
                already: false,
            })
        }
    };

    let note = match (&proton, &runtime) {
        (None, None) => "the Proton and runtime of ours are the newest published".to_string(),
        _ => format!("{} is the newest published", release.tag),
    };
    Offer {
        protocol: PROTOCOL,
        proton,
        runtime,
        trouble: None,
        note,
    }
}

/// Fetch whatever is missing, saying how far it has got.
fn run_fetch(out: &mut impl Write, steam: &Steam, paths: &Paths) -> bool {
    let step = |out: &mut dyn Write, stage: Stage, progress: Option<f32>, note: &str| {
        let _ = say(
            out,
            &Progress {
                protocol: PROTOCOL,
                stage,
                progress,
                trouble: None,
                note: note.to_string(),
            },
        );
    };
    let failed = |out: &mut dyn Write, (trouble, note): fetch::Failure| {
        eprintln!("fetch: {note} ({trouble:?})");
        let _ = say(
            out,
            &Progress {
                protocol: PROTOCOL,
                stage: Stage::Failed,
                progress: None,
                trouble: Some(trouble),
                note,
            },
        );
        false
    };

    // Never under a running game. This replaces the tree a launch reads —
    // Proton holds `dist.lock` in it while it sets a prefix up, and the
    // files under it are read for as long as a game is playing — so a fetch
    // that landed now would be pulling the floor out from under somebody.
    //
    // Conservative on purpose: it refuses while *any* game of ours is
    // running, including one playing under Steam's Proton, which this fetch
    // would not touch. The alternative is working out which Proton each
    // running game is under, which is a guess about another program's
    // process tree.
    //
    // What it costs is a refusal somebody may not be able to see the reason
    // for. Until this question was *answered* — see `prefix::users`, which
    // had been matching nothing at all — the game it names was always one on
    // the screen; now it can be a Windows a session that crashed left behind.
    // That is the honest answer and it is not a dead end: a game's Uninstall
    // empties its Windows on the way in, and so does the end of any launch of
    // it. Sweeping every game from here instead would be this update deciding
    // to end somebody's game, which is not an update's business.
    if let Some(busy) = prefix::all(paths.roots())
        .iter()
        .find(|game| prefix::in_use(game))
    {
        eprintln!("fetch: {} is still running", busy.id);
        let _ = say(
            out,
            &Progress {
                protocol: PROTOCOL,
                stage: Stage::Failed,
                progress: None,
                trouble: Some(Trouble::InUse),
                note: format!("{} is still running", busy.id),
            },
        );
        return false;
    }

    step(out, Stage::Looking, None, "looking for the newest build");
    let release = match fetch::newest_proton() {
        Ok(release) => release,
        Err(why) => return failed(out, why),
    };

    // The Proton, unless this machine already has exactly this release.
    let into = paths.protons().join(&release.tag);
    if !into.join("proton").is_file() {
        let digest = match fetch::proton_digest(&release) {
            Ok(digest) => digest,
            Err(why) => return failed(out, why),
        };
        step(out, Stage::Proton, Some(0.0), &release.tag);
        let archive = match fetch::download(
            &release.download,
            &paths.downloads(),
            &digest,
            fetch::Digest512Or256::Sha512Anywhere,
            release.size,
            &mut |done| step(out, Stage::Proton, Some(done), &release.tag),
        ) {
            Ok(archive) => archive,
            Err(why) => return failed(out, why),
        };
        step(out, Stage::Settling, None, &release.tag);
        if let Err(why) = fetch::unpack(&archive, &into, 1, "proton") {
            return failed(out, why);
        }
    } else {
        eprintln!("fetch: {} is already on the disk", into.display());
    }

    // And the runtime it asks for — read out of the build that just landed,
    // not assumed.
    let manifest = tools::manifest(&into.join("toolmanifest.vdf"));
    let Some(app_id) = manifest.require_tool_appid else {
        // A Proton old enough to need no container is complete as it stands.
        step(out, Stage::Done, Some(1.0), "ready");
        return true;
    };
    let Some(suite) = runtime::suite(app_id) else {
        return failed(
            out,
            (
                Trouble::UnknownRuntime,
                format!("this build has never heard of runtime {app_id}"),
            ),
        );
    };
    if runtime::resolve(app_id, steam, paths.store())
        .entry_point
        .is_some()
    {
        eprintln!("fetch: {} is already on the disk", suite.name);
        step(out, Stage::Done, Some(1.0), "ready");
        return true;
    }

    let digest = match fetch::runtime_digest(suite) {
        Ok(digest) => digest,
        Err(why) => return failed(out, why),
    };
    step(out, Stage::Runtime, Some(0.0), suite.name);
    let archive = match fetch::download(
        &suite.archive_url(),
        &paths.downloads(),
        &digest,
        fetch::Digest512Or256::Sha256Named,
        0,
        &mut |done| step(out, Stage::Runtime, Some(done), suite.name),
    ) {
        Ok(archive) => archive,
        Err(why) => return failed(out, why),
    };
    let into = paths.runtimes().join(suite.name);
    step(out, Stage::Settling, None, suite.name);
    // Valve's runtime archives wrap everything in a directory of their own,
    // the same as the Proton's, so one element comes off.
    if let Err(why) = fetch::unpack(&archive, &into, 1, "_v2-entry-point") {
        return failed(out, why);
    }
    step(out, Stage::Checking, None, suite.name);
    if let Err(why) = fetch::verify_runtime(&into) {
        // A tree that does not match its own manifest is never left where a
        // launch could find it.
        let _ = std::fs::remove_dir_all(&into);
        return failed(out, why);
    }

    step(out, Stage::Done, Some(1.0), "ready");
    true
}

/// Whether a path the shell handed back really is inside this game.
///
/// Written against the string rather than the disk because the file may not
/// exist yet, and the question is about the shape of the path: no absolute
/// path, and nothing that climbs out with `..`.
fn inside_the_game(_at: &Path, relative: &str) -> bool {
    let path = Path::new(relative);
    path.is_relative()
        && !path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
}

/// Leave nothing of one game's Windows running.
///
/// A game this helper has never heard of is not an error worth a panel: the
/// shell asks this of a launch that has just ended, and a game removed in the
/// meantime is a game with nothing left to end. Said in the note, and the
/// counts are zero.
fn end_everything(paths: &Paths, id: &str) -> Ended {
    let games = prefix::all(paths.roots());
    let Some(game) = games.into_iter().find(|game| game.id == id) else {
        return Ended {
            protocol: PROTOCOL,
            id: id.to_string(),
            found: 0,
            killed: 0,
            left: 0,
            trouble: None,
            note: format!("there is no game called {id}"),
        };
    };
    let gone = prefix::end(&game);
    let note = match (gone.found, gone.killed, gone.left) {
        (0, _, _) => format!("nothing of {id} was running"),
        (found, _, left) if left > 0 => {
            format!("asked {found} of {id}'s processes to end; {left} would not go")
        }
        (found, 0, _) => format!("{found} of {id}'s processes ended when asked"),
        (found, killed, _) => {
            format!("asked {found} of {id}'s processes to end, then killed {killed}")
        }
    };
    Ended {
        protocol: PROTOCOL,
        id: game.id,
        found: gone.found as u32,
        killed: gone.killed as u32,
        left: gone.left as u32,
        trouble: None,
        note,
    }
}

/// A prefix of its own for an executable, and how to run it where it is.
///
/// **Nothing here decides whether this is an installer**; see `install`'s own
/// head for why that question cannot be answered before the thing has run,
/// and [`settle`] for the answer that is read off the prefix afterwards.
/// What this does is the same for both: a prefix, the executable named as
/// what the game plays *at the path it is already at*, and a launch that runs
/// it there with its own folder shared into the container.
///
/// `disc` is the disc the executable is on, where the shell pressed a disc's
/// row rather than a file — see [`make_for_a_disc`], which is the one place
/// the two differ.
fn make_new(
    steam: &Steam,
    paths: &Paths,
    exe: &Path,
    name: Option<&str>,
    desktop: Option<launch::Desktop>,
    disc: Option<&Disc>,
    arguments: &[String],
) -> Made {
    if !exe.is_file() {
        return refused(Trouble::Files, format!("{} is not there", exe.display()));
    }
    // What to call it, best source first. The filename is last on purpose:
    // an installer is named for a download — `npp.8.9.8.Installer.x64` is a
    // real one — and nothing in this shell renames a game afterwards, so a
    // name taken from the file would stand on the bar for ever. The same
    // executable says `Notepad++` inside itself.
    let called = name
        .map(str::to_string)
        .or_else(|| pe::product_name(exe))
        .or_else(|| {
            exe.file_stem()
                .and_then(|stem| stem.to_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "Windows game".to_string());

    let found = tools::list(steam, paths.store());
    let Some(tool) = tools::chosen(&found) else {
        return refused(
            Trouble::Other,
            "there is no Proton on this machine to make a prefix with".to_string(),
        );
    };

    if let Some(disc) = disc {
        return make_for_a_disc(paths, &found, exe, &called, desktop, disc, arguments);
    }

    // **The same program pressed twice is one program.** A press in Files on
    // something that already has a prefix of its own starts what is here
    // rather than standing a second row beside the first.
    //
    // Only while that game still plays the file itself, which is exactly the
    // program that needs no installer. A *setup* pressed again is a setup
    // somebody is running again — to repair an installation, or to install it
    // somewhere else — and that is a new prefix, which is what it has always
    // been. The two are told apart by the same fact everything else here is:
    // whether anything ever appeared in the prefix.
    if let Some(already) = prefix::all(paths.roots()).into_iter().find(|game| {
        game.runs_in_place()
            && game
                .kept
                .play
                .as_deref()
                .is_some_and(|play| Path::new(play) == exe)
    }) {
        eprintln!(
            "new: {} already plays {}, so it is started rather than made again",
            already.id,
            exe.display()
        );
        // A disc's own Windows, met by somebody pressing the disc's program
        // in Files: run the way the disc's row runs it, on a Windows desktop,
        // and without what `autorun.inf` puts after it, which nobody pressing
        // the file asked for. See [`make_for_a_disc`].
        if already.kept.disc.is_some() {
            let tool = proton_for(&found, already.kept.proton.as_deref()).unwrap_or(tool);
            let made = launch::of(
                &already,
                tool,
                exe,
                &[],
                launch::Verb::WaitForExitAndRun,
                paths.store(),
                desktop,
            );
            return launched(&already, tool, &called, exe, made);
        }
        // Started the way the row in the column starts it, down to having no
        // Windows desktop: it is a program this machine knows, and the
        // setting-up run it had on the first press is over.
        let start = start(steam, paths, &already.id, None, None, None);
        let trouble = start.trouble;
        return Made {
            protocol: PROTOCOL,
            game: Some(row(&already)),
            start: trouble.is_none().then_some(start),
            trouble,
            note: format!("{} is what that program already is", already.id),
        };
    }

    let mut game = match prefix::make(paths.new_prefixes(), &called, Some(exe)) {
        Ok(game) => game,
        Err(err) => return refused(Trouble::Files, err.to_string()),
    };

    // Named as what it plays before it has been run once, so the row is one
    // somebody can press from the frame it first appears on. Absolute,
    // because this is a program on their own disk that nothing here has
    // copied — see `prefix::Kept::play`.
    game.kept.play = Some(exe.to_string_lossy().into_owned());
    game.kept.arguments = arguments.to_vec();
    if let Err(err) = game.save() {
        let _ = std::fs::remove_dir_all(&game.at);
        return refused(Trouble::Files, err.to_string());
    }

    // The folder the program lives in is shared into the container by
    // `launch::of`, which has always done that for an executable that is not
    // inside the game — so it runs with everything it came with beside it.
    let made = launch::of(
        &game,
        tool,
        exe,
        arguments,
        launch::Verb::WaitForExitAndRun,
        paths.store(),
        desktop,
    );
    launched(&game, tool, &called, exe, made)
}

/// The program on a disc, run in that disc's own Windows.
///
/// Three things are different from a file pressed in Files, and all three are
/// about the disc being the same disc every time it goes in, wherever it is
/// mounted:
///
/// 1. **A disc that already has a Windows runs in it again** rather than in a
///    new one — matched by [`Disc`], never by the path, because the path is
///    made out of the label and another disc with that label shares it. That
///    is how a game played straight off its disc keeps its saves, which is
///    the user's own reason for keeping the Windows at all. What it plays is
///    pointed at where the disc is *now*.
/// 2. **That Windows is built before the program first runs in it.** Wine
///    gives a disc in the drive a letter of its own, a CD drive like the one
///    the disc was made for, but only once its own services are up — which in
///    a Windows being built for the first time is after the program has
///    already started, so the program finds itself on `Z:\run\media\…` and
///    not on `D:\`. Measured on 2026-10-10 against Proton 11.0: the first run
///    in a new prefix started in `Z:`, every run after it in `D:`. A setup
///    that asks for its second disc looks for it on the drive it was started
///    from, and only a drive letter follows the disc being changed.
/// 3. **It opens on a Windows desktop every time**, not only the first: until
///    something is installed, what a disc runs is a setup or a menu, which is
///    one fixed-size window — the shape this session draws into a corner
///    without a desktop. See `launch`'s head.
fn make_for_a_disc(
    paths: &Paths,
    found: &[report::Tool],
    exe: &Path,
    called: &str,
    desktop: Option<launch::Desktop>,
    disc: &Disc,
    arguments: &[String],
) -> Made {
    let games = prefix::all(paths.roots());
    let mut game = match disc::its_own_windows(&games, disc) {
        Some(already) => {
            eprintln!(
                "new: {} is that disc's Windows already, so it runs there again",
                already.id
            );
            already.clone()
        }
        None => match prefix::make(paths.new_prefixes(), called, Some(exe)) {
            Ok(game) => game,
            Err(err) => return refused(Trouble::Files, err.to_string()),
        },
    };
    let fresh = game.kept.disc.is_none();
    game.kept.play = Some(exe.to_string_lossy().into_owned());
    game.kept.arguments = arguments.to_vec();
    game.kept.disc = Some(disc.clone());
    if let Err(err) = game.save() {
        if fresh {
            let _ = std::fs::remove_dir_all(&game.at);
        }
        return refused(Trouble::Files, err.to_string());
    }
    // The Proton somebody chose for it, where this is a Windows they have
    // had long enough to choose one for, and the machine's otherwise.
    let Some(tool) = proton_for(found, game.kept.proton.as_deref()) else {
        return refused(
            Trouble::Other,
            "there is no Proton on this machine to make a prefix with".to_string(),
        );
    };
    if !game.prefix_built() {
        build_before_the_first_run(&game, tool, paths.store());
    }
    let made = launch::of(
        &game,
        tool,
        exe,
        arguments,
        launch::Verb::WaitForExitAndRun,
        paths.store(),
        desktop,
    );
    launched(&game, tool, called, exe, made)
}

/// What `new` answers once it has a game and a launch for it.
fn launched(
    game: &prefix::Game,
    tool: &report::Tool,
    called: &str,
    exe: &Path,
    made: Result<launch::Launch, launch::Cannot>,
) -> Made {
    match made {
        Ok(launch) => Made {
            protocol: PROTOCOL,
            game: Some(row(game)),
            start: Some(Start {
                protocol: PROTOCOL,
                argv: launch.argv,
                environment: launch.environment.into_iter().collect(),
                directory: launch.directory.to_string_lossy().into_owned(),
                proton: Some(tool.name.clone()),
                trouble: None,
                note: format!("{called} starting under {}", tool.name),
            }),
            trouble: None,
            note: format!("{} plays {}", game.id, exe.display()),
        },
        Err(launch::Cannot::NoRuntime) => Made {
            protocol: PROTOCOL,
            game: Some(row(game)),
            start: None,
            trouble: Some(Trouble::UnknownRuntime),
            note: format!("{} has no runtime on this machine", tool.name),
        },
        Err(launch::Cannot::NoTarget) => refused(Trouble::Other, "nothing to run".to_string()),
    }
}

/// `new`'s answer where nothing was made.
fn refused(trouble: Trouble, note: String) -> Made {
    Made {
        protocol: PROTOCOL,
        game: None,
        start: None,
        trouble: Some(trouble),
        note,
    }
}

/// How long building a Windows may take before it is given up on. Measured
/// at four to nine seconds on this machine's disks; the first run of a
/// runtime this helper has never used unpacks a container as well, which is
/// most of half a minute. Three minutes is a Windows that is not coming.
const LONGEST_BUILD: std::time::Duration = std::time::Duration::from_secs(180);

/// Build a game's Windows and wait for it, before anything is run in it.
///
/// **The one Wine this helper ever runs itself**, and not a game: Proton's
/// `wineboot` in the game's own prefix, which builds the Windows and starts
/// its services and ends — `waitforexitandrun` waits for the whole of it,
/// services included — so nothing of it is left for the shell to end. Its
/// output goes nowhere; Proton's own chatter is not this helper's log. See
/// [`make_for_a_disc`] for why a disc needs it.
///
/// Answers whether the Windows is built. One that is not is no reason to
/// refuse the press: the program then starts where it would have started
/// without this, which is on `Z:` rather than on the disc's own letter.
fn build_before_the_first_run(game: &prefix::Game, tool: &report::Tool, store: &Path) -> bool {
    let Ok(launch) = launch::of(
        game,
        tool,
        Path::new("wineboot"),
        &[],
        launch::Verb::WaitForExitAndRun,
        store,
        None,
    ) else {
        return false;
    };
    let Some((program, arguments)) = launch.argv.split_first() else {
        return false;
    };
    let began = std::time::Instant::now();
    let child = std::process::Command::new(program)
        .args(arguments)
        .envs(&launch.environment)
        .current_dir(&game.at)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(err) => {
            eprintln!("new: {}'s Windows could not be built ({err})", game.id);
            return false;
        }
    };
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if began.elapsed() < LONGEST_BUILD => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            _ => {
                eprintln!("new: {}'s Windows took too long to build", game.id);
                let _ = child.kill();
                let _ = child.wait();
                // The supervisor cannot pass a `SIGKILL` on, so whatever it
                // had started is ended by the prefix it is running in — see
                // `prefix::end`.
                let _ = prefix::end(game);
                break;
            }
        }
    }
    let built = game.prefix_built();
    eprintln!(
        "new: {}'s Windows {} in {:.1} s",
        game.id,
        if built { "was built" } else { "was not built" },
        began.elapsed().as_secs_f32()
    );
    built
}

/// What a run in a prefix left behind, read once that run is over.
///
/// **The one place an installer is told from a program that needs none**, and
/// it is told after the fact: nothing a press runs is ever copied into a
/// prefix, so anything in one is something the run put there. Something there
/// and what ran was a setup, so what it installed is what the game plays from
/// now on; nothing there and what ran was the program, which goes on being
/// played where it lives.
///
/// Asked of a game that plays something outside its own folder, every time
/// something in it exits — see [`prefix::Game::runs_in_place`] for why it is
/// not asked once and then forgotten. `None` for every other game, which is
/// every game whose program is in its own prefix already.
///
/// Answers with the one row, in `games`' own shape, so the shell has nothing
/// extra to learn.
fn settle(paths: &Paths, id: &str) -> Option<GameRow> {
    let games = prefix::all(paths.roots());
    let mut game = games.into_iter().find(|game| game.id == id)?;
    if !game.runs_in_place() {
        return None;
    }
    let Some(installed) = install::settled(&game.at) else {
        eprintln!("settled: {id} put nothing in its prefix, so it is the program itself");
        return Some(row(&game));
    };
    eprintln!(
        "settled: {id} installed {}, so what ran was a setup",
        installed.at
    );
    game.kept.play = Some(installed.at);
    // Whatever a disc's `autorun.inf` put after its program was for that
    // program — a menu's `/autorun`, a setup's language — and a game started
    // with its installer's switches is a game told something it never asked.
    game.kept.arguments.clear();
    // And the drawing on the row is read again, out of what was installed:
    // it was taken from the setup, and the two are not always the same
    // picture. See `icon::beside`, which never opens a program twice.
    icon::forget(&game.at);
    if let Err(err) = game.save() {
        eprintln!("settled: {id} could not be written ({err})");
    }
    Some(row(&game))
}

/// A prefix of its own, with a game somebody already has copied into it.
fn run_import(out: &mut impl Write, paths: &Paths, from: &Path, name: Option<&str>) -> bool {
    let failed = |out: &mut dyn Write, trouble: Trouble, note: String| {
        eprintln!("import: {note} ({trouble:?})");
        let _ = say(
            out,
            &Progress {
                protocol: PROTOCOL,
                stage: Stage::Failed,
                progress: None,
                trouble: Some(trouble),
                note,
            },
        );
        false
    };
    if !from.is_dir() {
        return failed(
            out,
            Trouble::Files,
            format!("{} is not a folder", from.display()),
        );
    }
    // The folder's name is last here for the reason an installer's filename
    // is: `npp.8.9.8.portable.x64` is what the download was called, not what
    // the program is. Asked of the likeliest program in the folder before
    // anything is copied, so the game is named right from the first frame it
    // is drawn on.
    let called = name
        .map(str::to_string)
        .or_else(|| {
            let best = install::best_in(from)?;
            pe::product_name(&from.join(&best.at))
        })
        .or_else(|| {
            from.file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "Windows game".to_string());

    let game = match prefix::make(paths.new_prefixes(), &called, Some(from)) {
        Ok(game) => game,
        Err(err) => return failed(out, Trouble::Files, err.to_string()),
    };
    let _ = say(
        out,
        &Progress {
            protocol: PROTOCOL,
            stage: Stage::Settling,
            progress: Some(0.0),
            trouble: None,
            note: called.clone(),
        },
    );
    if let Err(err) = install::import(&game.at, from, &called, &mut |done| {
        let _ = say(
            out,
            &Progress {
                protocol: PROTOCOL,
                stage: Stage::Settling,
                progress: Some(done),
                trouble: None,
                note: called.clone(),
            },
        );
    }) {
        // A copy that did not finish leaves a prefix with half a game in
        // it, which is worse than nothing: it would be listed, and pressing
        // it would start something broken.
        let _ = std::fs::remove_dir_all(&game.at);
        return failed(out, Trouble::Files, err.to_string());
    }
    // A folder somebody already has is a game that is already installed, so
    // unlike an installer there is nothing left to run before the likeliest
    // executable can be named — and a game that lands with nothing to play
    // is a row that cannot be pressed. The ranking is the same one the
    // shell's **Choose what to play** offers, so a wrong guess is one menu
    // press from being corrected rather than a dead end.
    let mut game = game;
    if let Some(best) = install::candidates(&game.at).into_iter().next() {
        game.kept.play = Some(best.at.clone());
        match game.save() {
            Ok(()) => eprintln!("import: {} plays {}", game.id, best.at),
            Err(err) => eprintln!("import: {} could not be written ({err})", game.id),
        }
    } else {
        eprintln!("import: nothing in {} looks like a program", game.id);
    }
    let _ = say(
        out,
        &Progress {
            protocol: PROTOCOL,
            stage: Stage::Done,
            progress: Some(1.0),
            trouble: None,
            note: game.id.clone(),
        },
    );
    true
}

/// One game, as the shell draws it.
///
/// Where the game has something to play, this is also where its drawing is
/// taken out of that program and written beside it — once, and then read
/// back off the disk on every listing afterwards. See [`icon::beside`].
fn row(game: &prefix::Game) -> GameRow {
    let cover = game
        .target()
        .filter(|at| at.is_file())
        .and_then(|exe| icon::beside(&game.at, &exe))
        .map(|at| at.to_string_lossy().into_owned());
    GameRow {
        cover,
        id: game.id.clone(),
        name: game.kept.name.clone(),
        state: match game.state() {
            prefix::State::Ready => GameState::Ready,
            prefix::State::Unfinished => GameState::Unfinished,
        },
        proton: game.kept.proton.clone(),
        at: game.at.to_string_lossy().into_owned(),
        play: game.kept.play.clone(),
        prefix_built: game.prefix_built(),
        runs_in_place: game.runs_in_place(),
        disc: game.kept.disc.clone(),
    }
}

/// How to run one game, or why it cannot be run.
fn start(
    steam: &Steam,
    paths: &Paths,
    id: &str,
    inside: Option<&std::path::Path>,
    compatibility: Option<&str>,
    desktop: Option<launch::Desktop>,
) -> Start {
    let refuse = |trouble: Trouble, note: String| Start {
        protocol: PROTOCOL,
        argv: Vec::new(),
        environment: Vec::new(),
        directory: String::new(),
        proton: None,
        trouble: Some(trouble),
        note,
    };

    let games = prefix::all(paths.roots());
    let Some(game) = games.into_iter().find(|game| game.id == id) else {
        return refuse(Trouble::Other, format!("there is no game called {id}"));
    };

    // The user's choice if they made one, then this game's own, then
    // whatever the machine would choose. A named Proton that is no longer
    // on the disk falls through to the machine's choice rather than
    // refusing: somebody who uninstalled a Proton should find their game
    // still starts.
    let found = tools::list(steam, paths.store());
    let wanted = compatibility.or(game.kept.proton.as_deref());
    let tool = proton_for(&found, wanted);
    let Some(tool) = tool else {
        return refuse(
            Trouble::Other,
            "there is no Proton on this machine to run it with".to_string(),
        );
    };
    if let Some(name) = wanted {
        if name != tool.name {
            eprintln!("start: {name} is no longer here; using {}", tool.name);
        }
    }

    let made = match inside {
        Some(exe) => {
            // Said here rather than left to fail inside Wine: a path that
            // is not there produces a perfectly well-formed launch, and
            // the shell would answer it with a loading screen that waits
            // out its patience over a program that was never started.
            // `make_new` has asked this of its own executable from the
            // first day.
            if !exe.is_file() {
                return refuse(Trouble::Files, format!("{} is not there", exe.display()));
            }
            // **Which verb depends on whether there is a prefix yet.**
            // `runinprefix` creates nothing and upgrades nothing, which is
            // exactly right for running a mod's setup inside a game that
            // has been played — and exactly wrong for one that has not,
            // because then there is no prefix and bare Wine would make one
            // with none of Proton's own DLLs in it. A game imported as a
            // folder has no prefix until the first time it is played, and
            // an install that was interrupted may have none either; both
            // are games somebody reaches for this on.
            let verb = match game.prefix_built() {
                true => launch::Verb::RunInPrefix,
                false => launch::Verb::WaitForExitAndRun,
            };
            // On a Windows desktop, because everything that reaches here is a
            // setup: a mod's installer, a patcher, a game's own configuration
            // tool. All of them open one fixed-size window, which is the one
            // kind of window this session draws into a corner without one.
            launch::of(&game, tool, exe, &[], verb, paths.store(), desktop)
        }
        // And the game itself against the display itself. See `launch`.
        None => {
            // A program run where the person keeps it is the one thing a
            // game points at that this shell does not own, so it is the one
            // that can go: they moved the folder, or deleted the download it
            // was in. Said here for the reason `--in` says it above — a path
            // that is not there makes a perfectly well-formed launch, and
            // the shell would answer it with a loading screen that waits its
            // patience out over a program nothing ever started.
            if let Some(at) = game.target().filter(|_| game.runs_in_place()) {
                if !at.is_file() {
                    return refuse(Trouble::Gone, format!("{} is not there", at.display()));
                }
            }
            launch::play(&game, tool, paths.store())
        }
    };
    match made {
        Ok(launch) => Start {
            protocol: PROTOCOL,
            argv: launch.argv,
            environment: launch.environment.into_iter().collect(),
            directory: launch.directory.to_string_lossy().into_owned(),
            proton: Some(tool.name.clone()),
            trouble: None,
            note: format!("{} under {}", game.kept.name, tool.name),
        },
        Err(launch::Cannot::NoRuntime) => refuse(
            Trouble::UnknownRuntime,
            format!("{} has no runtime on this machine", tool.name),
        ),
        Err(launch::Cannot::NoTarget) => refuse(
            Trouble::Other,
            format!("nothing has been named to run in {id}"),
        ),
    }
}

/// Which Proton to run a game under: the one named, and whatever this
/// machine would choose where that one is not here.
///
/// The falling through is the point, and it is what makes a machine that
/// loses Steam go on working. A game set to one of Steam's Protons keeps
/// that choice written down — see `Kept::proton`, which is never cleared —
/// and runs under the managed build until Steam comes back, at which point
/// it is that game's choice again without anybody having to make it twice.
fn proton_for<'a>(found: &'a [report::Tool], wanted: Option<&str>) -> Option<&'a report::Tool> {
    wanted
        .and_then(|name| found.iter().find(|tool| tool.name == name))
        .or_else(|| tools::chosen(found))
}

/// Take a game off the disk, prefix and all.
///
/// Three things have to be true before anything is deleted, and the order
/// matters: the directory has to be one of ours, nothing may be running in
/// it, and it has to be where we think it is rather than a link pointing
/// somewhere else.
fn remove(paths: &Paths, id: &str) -> Removal {
    let refuse = |trouble: Trouble, note: String| Removal {
        protocol: PROTOCOL,
        id: id.to_string(),
        removed: false,
        trouble: Some(trouble),
        note,
    };

    let games = prefix::all(paths.roots());
    let Some(game) = games.into_iter().find(|game| game.id == id) else {
        return refuse(Trouble::Other, format!("there is no game called {id}"));
    };

    // Asked of the live processes, not of `pfx.lock`: Proton releases that
    // lock before the game ever starts.
    if prefix::in_use(&game) {
        return refuse(
            Trouble::InUse,
            format!("something is still running in {id}"),
        );
    }

    // Only ever a directory one of our own roots holds directly, resolved
    // first so a link cannot aim the delete somewhere else. `remove_dir_all`
    // does not follow links out of a tree, but the directory handed to it
    // is the one thing that could, and that is what this settles.
    let Ok(settled) = std::fs::canonicalize(&game.at) else {
        return refuse(
            Trouble::Files,
            format!("{} is not there", game.at.display()),
        );
    };
    let ours = paths.roots().iter().any(|root| {
        std::fs::canonicalize(root).is_ok_and(|root| settled.parent() == Some(root.as_path()))
    });
    if !ours {
        return refuse(
            Trouble::Files,
            format!(
                "{} is not in a folder this keeps games in",
                settled.display()
            ),
        );
    }

    match std::fs::remove_dir_all(&settled) {
        Ok(()) => Removal {
            protocol: PROTOCOL,
            id: id.to_string(),
            removed: true,
            trouble: None,
            note: format!("{} is gone", settled.display()),
        },
        Err(err) => refuse(Trouble::Files, format!("{}: {err}", settled.display())),
    }
}

fn ended(succeeded: bool) -> ExitCode {
    if succeeded {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Write one record as one line. A closed pipe means the shell has gone.
fn say(out: &mut (impl Write + ?Sized), record: &impl serde::Serialize) -> ExitCode {
    let Ok(json) = serde_json::to_string(record) else {
        return ExitCode::FAILURE;
    };
    match writeln!(out, "{json}").and_then(|()| out.flush()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{Origin, Runtime};

    /// **A disc's label is a value, whatever it starts with**, and so is a
    /// name — a disc called `-= GAME =-` was refused by both verbs that hear
    /// it, with nothing on stdout, which the shell can only read as a helper
    /// that could not answer.
    #[test]
    fn a_label_that_starts_with_a_dash_is_a_label() {
        let read = Cli::try_parse_from([
            "lxb-proton",
            "disc",
            "/run/media/someone/GAME",
            "--label",
            "-= GAME =-",
            "--to",
            "/tmp/picture.png",
        ])
        .expect("the label is a value");
        assert!(
            matches!(read.asked, Asked::Disc { label: Some(ref label), .. } if label == "-= GAME =-")
        );

        let made = Cli::try_parse_from([
            "lxb-proton",
            "new",
            "/run/media/someone/GAME/setup.exe",
            "--name",
            "-= GAME =-",
            "--disc-label",
            "-= GAME =-",
            "--disc-uuid",
            "",
            "--",
            "-autorun",
        ])
        .expect("and so are these");
        match made.asked {
            Asked::New {
                name,
                disc_label,
                disc_uuid,
                arguments,
                ..
            } => {
                assert_eq!(name.as_deref(), Some("-= GAME =-"));
                assert_eq!(disc_label.as_deref(), Some("-= GAME =-"));
                assert_eq!(disc_uuid.as_deref(), Some(""));
                assert_eq!(arguments, ["-autorun"]);
            }
            other => panic!("{other:?}"),
        }
    }

    fn published_release() -> fetch::Release {
        fetch::Release {
            tag: "cachyos-11.0-20261005-slr".into(),
            download: "https://example.invalid/proton.tar.xz".into(),
            size: 346_993_320,
            checksum: String::new(),
        }
    }

    fn a_tool(name: &str, origin: Origin, runtime: Option<Runtime>) -> report::Tool {
        report::Tool {
            name: name.into(),
            bin: format!("/somewhere/{name}/proton"),
            origin,
            version: None,
            runtime,
        }
    }

    fn a_runtime(origin: Origin, version: Option<&str>) -> Runtime {
        Runtime {
            app_id: 4_183_110,
            suite: Some("steamrt4".into()),
            entry_point: Some("/somewhere/SteamLinuxRuntime_4/_v2-entry-point".into()),
            version: version.map(str::to_string),
            origin: Some(origin),
        }
    }

    fn decide(found: &[report::Tool], published: &str) -> Offer {
        let published = published.to_string();
        decide_newer(
            found,
            || Ok(published_release()),
            move |_| Ok(published.clone()),
        )
    }

    /// A game set to one of Steam's Protons goes on working when Steam is
    /// taken off the machine, and remembers the choice for Steam's return.
    ///
    /// Three machines, one game, and the choice never rewritten: the point
    /// is that the falling back happens at the launch and not in the file.
    #[test]
    fn a_game_set_to_steams_proton_survives_steam_being_taken_off() {
        let ours = a_tool("cachyos-11.0-slr", Origin::Managed, None);
        let steams = a_tool("Proton - Experimental", Origin::Valve, None);
        let set_to = Some("Proton - Experimental");

        let with_steam = [steams.clone(), ours.clone()];
        assert_eq!(
            proton_for(&with_steam, set_to).map(|tool| tool.name.as_str()),
            Some("Proton - Experimental"),
        );

        let without_steam = [ours.clone()];
        assert_eq!(
            proton_for(&without_steam, set_to).map(|tool| tool.name.as_str()),
            Some("cachyos-11.0-slr"),
            "the game still starts, under what is left"
        );

        // And Steam comes back. Nothing was rewritten in between, so the
        // choice is simply in force again.
        assert_eq!(
            proton_for(&with_steam, set_to).map(|tool| tool.name.as_str()),
            Some("Proton - Experimental"),
        );
    }

    /// A machine with nothing at all cannot start a game, and says so
    /// rather than picking something that is not there.
    #[test]
    fn a_machine_with_no_proton_at_all_runs_nothing() {
        assert!(proton_for(&[], Some("Proton - Experimental")).is_none());
        assert!(proton_for(&[], None).is_none());
    }

    /// A machine with five of Steam's Protons and none of ours has nothing
    /// here to update. They are the client's, it keeps them current, and
    /// this program never writes into them.
    #[test]
    fn a_machine_with_only_steams_protons_has_nothing_of_ours_to_update() {
        let found = [
            a_tool(
                "Proton 11.0",
                Origin::Valve,
                Some(a_runtime(Origin::Valve, None)),
            ),
            a_tool(
                "GE-Proton9",
                Origin::Custom,
                Some(a_runtime(Origin::Valve, None)),
            ),
        ];
        let offer = decide(&found, "4.0.20260805.254769");
        assert_eq!(offer.proton, None);
        assert_eq!(offer.runtime, None);
        assert_eq!(offer.trouble, None);
    }

    /// Ours, out of date, with its runtime also ours: both are offered.
    #[test]
    fn an_old_build_of_ours_is_offered_both_halves() {
        let found = [a_tool(
            "cachyos-9.0-20250101-slr",
            Origin::Managed,
            Some(a_runtime(Origin::Managed, Some("4.0.20250101.1"))),
        )];
        let offer = decide(&found, "4.0.20260805.254769");
        assert_eq!(
            offer.proton.as_ref().and_then(|d| d.name.as_deref()),
            Some("cachyos-11.0-20261005-slr"),
        );
        assert_eq!(
            offer.proton.as_ref().and_then(|d| d.bytes),
            Some(346_993_320),
            "and how big it is, so somebody can decide whether to spend it"
        );
        assert_eq!(
            offer.runtime.as_ref().and_then(|d| d.name.as_deref()),
            Some("4.0.20260805.254769"),
        );
    }

    /// **The one that must never be got wrong.** A managed Proton on a
    /// machine with Steam runs under *Steam's* runtime — `runtime::find`
    /// prefers it, so a machine does not keep two copies of the same
    /// hundred and sixty megabytes — and that copy is not ours to replace,
    /// however old it looks beside what is published.
    #[test]
    fn steams_runtime_is_never_offered_however_old_it_is() {
        let found = [a_tool(
            "cachyos-9.0-20250101-slr",
            Origin::Managed,
            Some(a_runtime(Origin::Valve, Some("4.0.20200101.1"))),
        )];
        let offer = decide(&found, "4.0.20260805.254769");
        assert!(
            offer.proton.is_some(),
            "ours is still ours and is still out of date"
        );
        assert_eq!(
            offer.runtime, None,
            "but Steam's runtime is Steam's, whatever its version says"
        );
    }

    /// Ours and current: nothing is offered, and it says so rather than
    /// saying nothing.
    #[test]
    fn a_build_of_ours_that_is_current_offers_nothing() {
        let found = [a_tool(
            "cachyos-11.0-20261005-slr",
            Origin::Managed,
            Some(a_runtime(Origin::Managed, Some("4.0.20260805.254769"))),
        )];
        let offer = decide(&found, "4.0.20260805.254769");
        assert_eq!(offer.proton, None);
        assert_eq!(offer.runtime, None);
        assert_eq!(offer.trouble, None);
        assert!(offer.note.contains("newest published"));
    }

    /// A runtime of ours that is not on the disk at all is one to fetch,
    /// which is the same answer as one at the wrong version.
    #[test]
    fn a_runtime_of_ours_that_is_missing_is_offered() {
        let found = [a_tool(
            "cachyos-11.0-20261005-slr",
            Origin::Managed,
            Some(a_runtime(Origin::Managed, None)),
        )];
        let offer = decide(&found, "4.0.20260805.254769");
        assert_eq!(offer.proton, None, "the Proton itself is current");
        assert!(
            offer.runtime.is_some(),
            "but it has no runtime to run under"
        );
    }

    /// A generation this build has never heard of is said out loud rather
    /// than quietly treated as nothing to do.
    #[test]
    fn a_runtime_generation_nobody_here_knows_is_trouble_not_silence() {
        let mut runtime = a_runtime(Origin::Managed, Some("1"));
        runtime.app_id = 999_999_999;
        let found = [a_tool(
            "cachyos-11.0-20261005-slr",
            Origin::Managed,
            Some(runtime),
        )];
        let offer = decide(&found, "4.0.20260805.254769");
        assert_eq!(offer.trouble, Some(Trouble::UnknownRuntime));
    }

    /// The network failing is trouble to show, not an update to miss.
    #[test]
    fn a_listing_that_cannot_be_reached_is_trouble() {
        let found = [a_tool(
            "cachyos-9.0-20250101-slr",
            Origin::Managed,
            Some(a_runtime(Origin::Managed, Some("1"))),
        )];
        let offer = decide_newer(
            &found,
            || Err((Trouble::Offline, "no route to the listing".into())),
            |_| Ok("4.0.20260805.254769".to_string()),
        );
        assert_eq!(offer.trouble, Some(Trouble::Offline));
        assert_eq!(offer.proton, None);
    }
}
