//! The shell's Epic Games integration, through Heroic Games Launcher, as a
//! program of its own.
//!
//! The same split as `lxb-retroarch`, and for the same reason: install the
//! package this comes in and the shell grows an Epic Games row and column;
//! leave it out and the shell is exactly what it was. Nothing in `lxb-desktop`
//! depends on this crate at build time — what the two agree about is one JSON
//! record per line, [`report`], and the shell finds this program on `PATH`.
//!
//! ## Twelve questions
//!
//! ```text
//! lxb-heroic probe             is Heroic here, whose account, which Proton
//! lxb-heroic install           put Heroic in this user's flatpaks, and its Proton
//! lxb-heroic sign-in           a code for the phone, and Heroic signed in with it
//! lxb-heroic sign-out          Heroic signed out
//! lxb-heroic library           the account's games [--refresh: ask Epic first]
//! lxb-heroic art               their covers [--heroes] [--only APP …]
//! lxb-heroic size APP          what installing one would take
//! lxb-heroic get APP           install it, as Heroic does
//! lxb-heroic remove APP        uninstall it, as Heroic does
//! lxb-heroic folder DIR        install games there from now on
//! lxb-heroic saves on|off      keep saves in Epic's cloud, or not
//! lxb-heroic achievements      what has been unlocked [--only APP …]
//! ```
//!
//! Everything here reads and writes **Heroic's own** files — its legendary's
//! account and library, its settings — so what the shell shows and what
//! Heroic's own window shows are one account and one library, never a copy.
//! Nothing here starts a game: that is the shell's machinery, with the command
//! `probe` hands it. Nothing is remembered between runs except what Heroic
//! itself keeps.

mod art;
mod epic;
mod folder;
mod game;
mod heroic;
mod install;
mod library;
mod proton;
mod report;
mod saves;
mod secret;
mod signin;
mod tools;
mod trophies;

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use heroic::Paths;
use report::{Account, Download, Library, Probe, Reason, Removal, SignIn, Step, PROTOCOL};

#[derive(Debug, Parser)]
#[command(name = "lxb-heroic", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    asked: Asked,
}

#[derive(Debug, Subcommand)]
enum Asked {
    /// Whether this machine has Heroic, whose Epic account it holds, and
    /// whether it has a Proton to start games with.
    Probe,
    /// Install Heroic into this user's own flatpak installation where there is
    /// none, then make sure it has Heroic's own default Proton.
    ///
    /// Deliberately `--user`: it needs no password and no polkit agent.
    Install,
    /// Sign Heroic in to Epic with a code approved on a phone.
    ///
    /// Writes the code as it is issued, and again whenever a fresh one
    /// replaces it; ends on `signed-in` or `failed`. End the process to
    /// cancel.
    SignIn,
    /// Sign Heroic out of Epic.
    SignOut,
    /// The Epic account's games, by Heroic's own rules for what is one.
    Library {
        /// Ask Epic for the list again first, rather than reading what the
        /// last refresh left on the disk.
        #[arg(long)]
        refresh: bool,
    },
    /// Fetch the covers the library's games do not have yet, into the
    /// shell's cache. One line per game that gained a picture, then `done`.
    Art {
        /// The backdrops as well as the covers.
        #[arg(long)]
        heroes: bool,
        /// Only these games, by app name, in this order.
        #[arg(long = "only", value_name = "APP")]
        only: Vec<String>,
    },
    /// How large a game is to download and on the disk, and where it would go.
    Size {
        #[arg(value_name = "APP")]
        app_name: String,
    },
    /// Install a game, the way Heroic installs it. One line per change,
    /// ending on `done` or `failed`; end the process to stop it, and asking
    /// again carries on from where it stopped.
    Get {
        #[arg(value_name = "APP")]
        app_name: String,
        /// Check every file of an installed game against Epic's manifest and
        /// fetch whatever is missing or wrong, rather than install it —
        /// Heroic's Verify and Repair.
        #[arg(long)]
        repair: bool,
    },
    /// Uninstall a game, the way Heroic uninstalls it.
    Remove {
        #[arg(value_name = "APP")]
        app_name: String,
    },
    /// Install games into this folder from now on: Heroic's own setting, and
    /// its sandbox told it may write there.
    Folder {
        #[arg(value_name = "DIR")]
        dir: PathBuf,
    },
    /// Keep saves in Epic's cloud — Heroic's own switch — and find the save
    /// folder of every installed game that has one.
    Saves {
        #[arg(value_parser = ["on", "off"])]
        state: String,
    },
    /// The achievements of the games on this disk and the ones played, and
    /// which of them this account has unlocked. One line per game, then
    /// `done`.
    Achievements {
        /// Only these games, by app name.
        #[arg(long = "only", value_name = "APP")]
        only: Vec<String>,
    },
    /// What Heroic can run games with, which it runs them with by default,
    /// and which games have one of their own.
    Tools,
    /// Run a game with one of those, or with Heroic's default where none is
    /// named; or, with no game, make one of them Heroic's default.
    UseTool {
        #[arg(long = "game", value_name = "APP")]
        game: Option<String>,
        #[arg(value_name = "NAME")]
        name: Option<String>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let mut out = std::io::stdout().lock();
    let Some(paths) = Paths::of_this_user() else {
        eprintln!("lxb-heroic: there is no home directory to find Heroic in");
        return ExitCode::FAILURE;
    };

    match cli.asked {
        Asked::Probe => {
            let installation = heroic::installation();
            if let Some(found) = &installation {
                eprintln!("heroic: {:?} {:?}", found.kind, found.version);
            }
            say(
                &mut out,
                &Probe {
                    protocol: PROTOCOL,
                    account: heroic::account(&paths),
                    proton: heroic::proton(&paths),
                    running: heroic::running(),
                    flatpak: heroic::flatpak_available(),
                    cloud_saves: heroic::cloud_saves(&paths),
                    base: home().map(|home| {
                        heroic::install_base(&paths, &home)
                            .to_string_lossy()
                            .into_owned()
                    }),
                    heroic: installation,
                },
            )
        }
        Asked::Install => {
            let Some(home) = home() else {
                return ExitCode::FAILURE;
            };
            ended(install::run(&mut out, &paths, &home))
        }
        Asked::SignIn => {
            let Some(installation) = heroic::installation() else {
                say(
                    &mut out,
                    &SignIn::Failed {
                        protocol: PROTOCOL,
                        reason: Reason::NoHeroic,
                        url: None,
                        note: "Heroic is not installed".to_string(),
                    },
                );
                return ExitCode::FAILURE;
            };
            ended(signin::run(&mut out, &installation, &paths))
        }
        Asked::SignOut => {
            let result = match heroic::installation() {
                Some(installation) => signin::sign_out(&installation, &paths),
                None => Err(Reason::NoHeroic),
            };
            if let Err(reason) = result {
                eprintln!("sign-out: {reason:?}");
            }
            let account = Account {
                protocol: PROTOCOL,
                name: heroic::account(&paths),
                reason: result.err(),
            };
            let succeeded = account.name.is_none();
            say(&mut out, &account);
            ended(succeeded)
        }
        Asked::Art { heroes, only } => ended(art::fetch(&mut out, &paths, &only, heroes)),
        Asked::Achievements { only } => {
            if only.iter().any(|app| !heroic::plain(app)) {
                return ExitCode::FAILURE;
            }
            let Some(installation) = heroic::installation() else {
                return ExitCode::FAILURE;
            };
            ended(trophies::fetch(&mut out, &installation, &paths, &only))
        }
        Asked::Tools => {
            let Some(home) = home() else {
                return ExitCode::FAILURE;
            };
            say(&mut out, &tools::tools(&paths, &home))
        }
        Asked::UseTool { game, name } => {
            let Some(home) = home() else {
                return ExitCode::FAILURE;
            };
            if game.as_deref().is_some_and(|app| !heroic::plain(app)) {
                return ExitCode::FAILURE;
            }
            let answer = tools::use_tool(&paths, &home, game.as_deref(), name.as_deref());
            eprintln!("use-tool: {} ({:?})", answer.note, answer.reason);
            let worked = answer.reason.is_none();
            say(&mut out, &answer);
            ended(worked)
        }
        Asked::Saves { state } => {
            let (Some(installation), Some(home)) = (heroic::installation(), home()) else {
                return ExitCode::FAILURE;
            };
            let answer = saves::set(&installation, &paths, &home, state == "on");
            eprintln!(
                "saves: {} ({:?}), {} found",
                answer.note, answer.reason, answer.found
            );
            let worked = answer.reason.is_none();
            say(&mut out, &answer);
            ended(worked)
        }
        Asked::Folder { dir } => {
            let Some(home) = home() else {
                return ExitCode::FAILURE;
            };
            let answer = folder::choose(&paths, &home, &dir);
            eprintln!("folder: {} ({:?})", answer.note, answer.reason);
            let worked = answer.reason.is_none();
            say(&mut out, &answer);
            ended(worked)
        }
        Asked::Size { app_name } | Asked::Get { app_name, .. } | Asked::Remove { app_name }
            if !heroic::plain(&app_name) =>
        {
            eprintln!("lxb-heroic: {app_name:?} is not one of Epic's names");
            ExitCode::FAILURE
        }
        Asked::Size { app_name } => {
            let (Some(installation), Some(home)) = (heroic::installation(), home()) else {
                return ExitCode::FAILURE;
            };
            say(
                &mut out,
                &game::size(&installation, &paths, &home, &app_name),
            )
        }
        Asked::Get { app_name, repair } => {
            let (Some(installation), Some(home)) = (heroic::installation(), home()) else {
                say(
                    &mut out,
                    &Download {
                        protocol: PROTOCOL,
                        app_name,
                        step: Step::Failed,
                        progress: None,
                        downloaded: None,
                        size: None,
                        reason: Some(Reason::NoHeroic),
                        note: "Heroic is not installed".to_string(),
                    },
                );
                return ExitCode::FAILURE;
            };
            match repair {
                true => ended(game::repair(&mut out, &installation, &paths, &app_name)),
                false => ended(game::get(&mut out, &installation, &paths, &home, &app_name)),
            }
        }
        Asked::Remove { app_name } => {
            let removal = match heroic::installation() {
                Some(installation) => game::remove(&installation, &paths, &app_name),
                None => Removal {
                    protocol: PROTOCOL,
                    app_name,
                    removed: false,
                    reason: Some(Reason::NoHeroic),
                    note: "Heroic is not installed".to_string(),
                },
            };
            eprintln!("remove: {} ({:?})", removal.note, removal.reason);
            let removed = removal.removed;
            say(&mut out, &removal);
            ended(removed)
        }
        Asked::Library { refresh } => {
            let installation = heroic::installation();
            let mut reason = None;
            let mut refreshed = false;
            if refresh {
                match (&installation, heroic::account(&paths)) {
                    (None, _) => reason = Some(Reason::NoHeroic),
                    (Some(_), None) => reason = Some(Reason::SignedOut),
                    (Some(installation), Some(_)) => match library::refresh(installation, &paths) {
                        Ok(()) => {
                            refreshed = true;
                            // And any game whose save folder could not be
                            // found before — installed before any game had
                            // made the Wine prefix it is found in.
                            if heroic::cloud_saves(&paths) {
                                saves::find_folders(installation, &paths);
                            }
                        }
                        Err(why) => reason = Some(why),
                    },
                }
            }
            // A signed-out account has no library, whatever legendary left
            // on the disk.
            let account = heroic::account(&paths);
            let games = if account.is_some() {
                library::read(&paths)
            } else {
                Vec::new()
            };
            eprintln!(
                "library: {} games{}",
                games.len(),
                if refreshed { ", fresh from Epic" } else { "" }
            );
            say(
                &mut out,
                &Library {
                    protocol: PROTOCOL,
                    account,
                    refreshed,
                    reason,
                    games,
                },
            )
        }
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
}

fn ended(succeeded: bool) -> ExitCode {
    if succeeded {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Write one record as one line. A closed pipe means the shell has gone.
fn say(out: &mut impl Write, record: &impl serde::Serialize) -> ExitCode {
    let Ok(json) = serde_json::to_string(record) else {
        return ExitCode::FAILURE;
    };
    match writeln!(out, "{json}").and_then(|()| out.flush()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}
