//! The shell's RetroArch integration, as a program of its own.
//!
//! This binary is the whole of the optional half of that integration: install
//! the package it comes in and the shell grows a RetroArch row, a column of
//! consoles and a page under Settings; leave it out and the shell is exactly
//! what it was. Nothing in `lxb-desktop` depends on this crate at build time —
//! what the two agree about is one JSON record per line, [`report`], and the
//! shell finds this program the way anything finds a program, on `PATH`.
//!
//! It exists as a separate program rather than as another module of the shell
//! for the reason the split is worth anything at all: a machine that will never
//! emulate a console should not carry the table of forty consoles, the flatpak
//! plumbing, or a walk over somebody's ROM folder, and a distribution should be
//! able to ship the shell without shipping an opinion about emulators.
//!
//! ## Seven questions and no state
//!
//! ```text
//! lxb-retroarch probe          is there a RetroArch, and could there be one
//! lxb-retroarch install        put one in this user's own flatpak installation
//! lxb-retroarch scan DIR       what consoles are in this folder, and what runs them
//! lxb-retroarch permit DIR     let a sandboxed RetroArch read that folder
//! lxb-retroarch cores A,B ...  fetch a core per console, from libretro's server
//! lxb-retroarch options        what every installed core can be set to
//! lxb-retroarch art DIR        fetch the covers and screenshots of those games
//! ```
//!
//! Nothing here remembers anything between runs, and nothing here starts a
//! game. Three of the seven reach the network, and only when asked: `install`
//! fetches RetroArch from Flathub, `cores` fetches a core from libretro's build
//! server — the same one RetroArch's own Online Updater uses — and `art`
//! fetches pictures from libretro's thumbnail server.
//!
//! Where the ROM folder is, is the shell's setting; launching is the
//! shell's own machinery, because a game started from this bar has to be
//! watched, drawn a loading screen, pinned to the display it was started on and
//! closed by the guide exactly like every other application — and none of that
//! is reachable from a process the shell forked and forgot. What this answers
//! is the command line to use, which is the only part of a launch that is
//! RetroArch's business.

mod achievements;
mod art;
mod assets;
mod consoles;
mod cores;
mod execstack;
mod find;
mod firmware;
mod install;
mod options;
mod report;
mod scan;
mod zip;

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

use clap::{Parser, Subcommand};

use report::{CoreOptions, Probe, PROTOCOL};

#[derive(Debug, Parser)]
#[command(name = "lxb-retroarch", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    asked: Asked,
}

/// How long a core gets to be taken all the way before it is given up on.
///
/// Generous against what it costs when it works — pcsx2 answers in a hundredth
/// of a second and dolphin in a tenth, because neither is being asked to *run*
/// anything. What this is really for is the core that opens a window, waits on
/// a device or sits in a loop, where the alternative to giving up is a settings
/// screen that never arrives.
const PATIENCE: Duration = Duration::from_secs(20);

/// The most lines of a core's own chatter to look through for an answer.
const CHATTER: usize = 400;

/// Where a core that is being asked a question may write.
///
/// Deliberately not RetroArch's save folder. An emulator taken as far as
/// `retro_load_game` lays things down on its way past — dolphin writes a whole
/// `User` tree — and none of it may land among somebody's real saves.
fn scratch() -> PathBuf {
    let at = std::env::temp_dir().join("lxb-retroarch-asking");
    let _ = std::fs::create_dir_all(&at);
    at
}

/// Write one record out the instant the core declared it.
///
/// A plain function rather than a closure because it is handed to a C callback
/// — see [`options::deeper`] — and flushed on the spot because the core is
/// entitled to die in the next instruction, which for dolphin it does.
fn announced(record: &CoreOptions) {
    if let Ok(line) = serde_json::to_string(record) {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    }
    eprintln!(
        "options: {} declares {} settings in {} groups, asked all the way",
        record.core,
        record.options.len(),
        record.categories.len()
    );
}

/// Ask one core again, all the way, in a process of its own.
///
/// A separate process because `retro_init` and `retro_load_game` are calls into
/// an emulator, and what a crash in one must cost is this core's settings page
/// rather than the answers every core before it gave. dolphin crashes *every
/// time* — a moment after handing over its ninety-nine settings — and this is
/// what turns that from a lost answer into a complete one.
///
/// The answer is looked for rather than read: a core opening writes what it
/// likes to the stdout it inherits, so this takes the newest line that parses
/// as a record for the core that was asked.
fn deeper(core: &str) -> Option<CoreOptions> {
    let me = std::env::current_exe().ok()?;
    let mut child = Command::new(me)
        .arg("options")
        .arg("--deep")
        .arg(core)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .ok()?;
    let out = child.stdout.take()?;

    // Read on a thread of its own, so that a core which never returns is a
    // question this gives up on rather than one it waits out.
    let (say, heard) = std::sync::mpsc::channel();
    let wanted = core.to_string();
    std::thread::spawn(move || {
        for line in BufReader::new(out)
            .lines()
            .map_while(Result::ok)
            .take(CHATTER)
        {
            if let Ok(record) = serde_json::from_str::<CoreOptions>(line.trim()) {
                if record.core == wanted && say.send(record).is_err() {
                    return;
                }
            }
        }
    });

    // Every record it manages to send, until it stops or runs out of time: a
    // core is entitled to declare twice, and the last table is the whole one.
    let until = std::time::Instant::now() + PATIENCE;
    let mut best = None;
    while let Some(left) = until.checked_duration_since(std::time::Instant::now()) {
        match heard.recv_timeout(left) {
            Ok(record) => best = Some(record),
            Err(_) => break,
        }
    }
    // Whether it answered or ran out of time, it is finished with. Not waited
    // for: a core is entitled to sit in an atexit handler of its own, and the
    // answer is already in hand.
    let _ = child.kill();
    let _ = child.wait();
    best
}

#[derive(Debug, Subcommand)]
enum Asked {
    /// Private JSON stdin protocol for RetroAchievements account and browsing.
    Achievements,
    /// Whether this machine has RetroArch, and what would start it.
    Probe,
    /// Install RetroArch into this user's own flatpak installation.
    ///
    /// Deliberately `--user`: it needs no password, no polkit agent and no
    /// distribution package, which is what makes the offer one the shell can
    /// put behind a plain Yes.
    Install,
    /// Take it off again, with everything it and this integration kept.
    ///
    /// The application, its whole `~/.var/app` tree — configuration, the cores
    /// the shell downloaded, the assets, the saves, any BIOS put there — the
    /// filesystem permission granted for the games folder, and the shell's own
    /// cache of the cover art it fetched. Somebody's games and the folder they
    /// are in are not touched.
    ///
    /// Only for the `--user` flatpak this shell installs. A system-wide one or
    /// a distribution package is somebody else's decision to undo, and this
    /// says so rather than asking for a password.
    Uninstall,
    /// List the consoles in a ROM folder, and the games in each.
    Scan {
        /// The folder holding one subfolder per console.
        roms: PathBuf,
    },
    /// Fetch a core for each console named, from libretro's own build server.
    ///
    /// One argument per console, holding that console's cores best first and
    /// separated by commas: `lxb-retroarch cores ppsspp mesen,nestopia`. The
    /// first of each list that the server actually publishes is the one
    /// fetched, which is why the whole list is given rather than one name — see
    /// `cores.rs`.
    ///
    /// Into this user's own core directory, which is where RetroArch's Online
    /// Updater puts them: a core fetched here is one RetroArch itself lists as
    /// installed, and needs no more authority than that updater does.
    Cores {
        /// One console's cores, best first, comma-separated. Repeat for more.
        #[arg(required = true)]
        wanted: Vec<String>,
    },
    /// Let RetroArch read this folder, where it is a flatpak and the folder is
    /// outside what its sandbox already reaches.
    ///
    /// Run when the user chooses a folder rather than every time it is read: it
    /// changes one application's permissions, which is a thing to do once and
    /// on purpose. A distribution package needs none of it and this says so
    /// without doing anything.
    Permit {
        /// The folder the games are in.
        roms: PathBuf,
    },
    /// Ask every installed core what it can be set to.
    ///
    /// One JSON record per core, written the moment that core has answered.
    /// A core is loaded into this process to be asked — see `options.rs` —
    /// which is why the records are streamed rather than collected: a core
    /// that takes this process down with it must not take the cores that
    /// already answered with it.
    Options {
        /// Ask only these cores, by libretro name. Every installed core when
        /// nothing is named.
        cores: Vec<String>,
        /// Take the cores named all the way: `retro_init`, and then
        /// `retro_load_game` with no game, for one that will not answer when it
        /// is merely asked.
        ///
        /// What this binary passes to itself for the deeper ask — see
        /// [`options::deeper`] and [`deeper`]. Given by hand it does exactly
        /// the same thing, which is how a core that answers neither way gets
        /// looked at.
        #[arg(long)]
        deep: bool,
    },
    /// Fetch the cover and the screenshot of every game in a ROM folder, from
    /// libretro's thumbnail server.
    ///
    /// The names on that server are the names libretro's database gives a dump
    /// — `Tekken 6 (USA) (En,Fr,De,Es,It,Ru)` — and somebody who dumped their
    /// own disc called the file something else, so nothing here asks for a
    /// picture by the file's name. Each shelf is listed once and the games are
    /// matched against it; see `art.rs`.
    ///
    /// Into this shell's own cache, never into RetroArch's thumbnail folder:
    /// what somebody sees in the emulator's own interface is the emulator's
    /// business.
    Art {
        /// The folder holding one subfolder per console.
        roms: PathBuf,
        /// Only these games, by the paths a scan gave them. The whole folder
        /// when none is named.
        #[arg(long = "only", value_name = "PATH")]
        only: Vec<String>,
        /// Ask the server which games exist again, rather than believing the
        /// listing already on this disk. What the row that fetches the pictures
        /// a second time is for.
        #[arg(long)]
        again: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let mut out = std::io::stdout().lock();

    match cli.asked {
        Asked::Achievements => {
            achievements::run();
            ExitCode::SUCCESS
        }
        Asked::Probe => {
            let installation = find::installation();
            if let Some(installation) = &installation {
                // Not part of the record: which cores are here is per console,
                // and asking it as a list is only worth doing where somebody is
                // reading a log to find out why nothing will start.
                let cores = find::Cores::of(installation).installed();
                eprintln!(
                    "retroarch: {:?} at {:?}, {} core(s)",
                    installation.kind,
                    installation.command,
                    cores.len()
                );
            }
            let config = installation
                .as_ref()
                .and_then(find::config_dir)
                .map(|at| at.to_string_lossy().into_owned());
            say(
                &mut out,
                &Probe {
                    protocol: PROTOCOL,
                    retroarch: installation,
                    config,
                    flatpak: find::flatpak_available(),
                    // This one can. The field exists for the shell talking to
                    // one that cannot, which is any helper written before the
                    // verb was.
                    fetches_cores: true,
                },
            )
        }
        Asked::Install => {
            if install::run(&mut out) {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Asked::Uninstall => {
            if install::remove(&mut out) {
                achievements::forget();
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Asked::Cores { wanted } => {
            if cores::fetch(&mut out, &wanted) {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Asked::Options { cores, deep } => {
            let Some(installation) = find::installation() else {
                eprintln!("options: RetroArch is not here");
                return ExitCode::FAILURE;
            };
            let here = find::Cores::of(&installation);
            // The same repair the scan does, for the same reason and one door
            // further along: this probe reaches a core through `dlopen`, so a
            // core asking for an executable stack answers nothing here either
            // and its settings page goes missing along with its games.
            if let Some(own) = here.own() {
                execstack::repair(own);
            }
            let asked = if cores.is_empty() {
                here.installed()
            } else {
                cores
            };
            // Where a core may read what it keeps beside itself, and where it
            // may write. Both only matter to the deeper ask, and both are set
            // here because this is the run that may become one.
            options::folders(
                find::config_dir(&installation)
                    .map(|at| assets::system_dir(&at))
                    .as_deref(),
                &scratch(),
            );

            // The deeper ask, in the process that was started to be it. One
            // core, taken as far as it needs, and every table it declares
            // written out the moment it arrives — see [`options::deeper`].
            if deep {
                let mut said = 0;
                for core in asked {
                    let Some(at) = here.on_this_disk(&core) else {
                        continue;
                    };
                    options::deeper(&core, &at, announced);
                    said += 1;
                }
                return if said > 0 {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                };
            }

            let mut said = 0;
            for core in asked {
                let Some(at) = here.on_this_disk(&core) else {
                    eprintln!("options: {core} is not on this disk");
                    continue;
                };
                match options::of(&core, &at) {
                    Ok(mut record) => {
                        // A core that declares nothing when it is merely asked
                        // may declare everything a call or two further in:
                        // pcsx2 hands over sixty-six settings during
                        // `retro_init` and dolphin ninety-nine during
                        // `retro_load_game`. Asked again in a process of its
                        // own, because those are calls into an emulator rather
                        // than questions put to a shared object.
                        if record.options.is_empty() {
                            eprintln!("options: {core} declared nothing; asking it all the way");
                            if let Some(deeper) = deeper(&core) {
                                record = deeper;
                            }
                        }
                        eprintln!(
                            "options: {core} declares {} settings in {} groups",
                            record.options.len(),
                            record.categories.len()
                        );
                        // A closed pipe means the shell has gone, which is
                        // the one failure worth stopping for.
                        if say(&mut out, &record) == ExitCode::FAILURE {
                            return ExitCode::FAILURE;
                        }
                        said += 1;
                    }
                    // Not a failure of the run. A core that will not answer is
                    // a core with no settings page, and the rest still have
                    // one.
                    Err(why) => eprintln!("options: {core} said nothing: {why}"),
                }
            }
            if said > 0 {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Asked::Permit { roms } => {
            let permission = install::permit(&roms, find::installation().as_ref());
            eprintln!("permit: {}", permission.note);
            say(&mut out, &permission)
        }
        Asked::Scan { roms } => {
            let mut library = scan::library(&roms);
            // One listing of the core directories for the whole library rather
            // than one per console: it is a `readdir` of two or three places,
            // and a machine with forty consoles in its folder would otherwise
            // do it forty times over.
            if let Some(installation) = find::installation() {
                let cores = find::Cores::of(&installation);
                // Before anything is looked up: a core built asking for a stack
                // it can execute is one no current glibc will load, and the
                // repair is one bit in a header. Done here because this is the
                // run that happens whenever the shell starts and whenever the
                // folder changes, so a core that arrived by RetroArch's own
                // updater is fixed as surely as one this helper fetched.
                if let Some(own) = cores.own() {
                    execstack::repair(own);
                }
                // Where the files a core reads beside itself would be, read
                // once for the whole library for the reason the core listing
                // is: it is one look at one configuration file.
                let system = find::config_dir(&installation).map(|at| assets::system_dir(&at));
                // And libretro's own descriptions of those cores, which is
                // where what a core cannot boot without is written down. Read
                // once for the library, like the two above it.
                let info = find::info_dir(&installation);
                // Cores this machine has already tried and could not open. Read
                // once, like everything else out here.
                let refused = find::refused();
                for console in &mut library.consoles {
                    // The table's own order, less whatever this machine has
                    // already downloaded and found it could not open.
                    //
                    // Deliberately *not* reordered by what can boot without a
                    // BIOS. It was, once, and it put Play! in front of pcsx2 on
                    // a machine with no PlayStation 2 BIOS — a core that starts
                    // and plays almost nothing in front of the one people
                    // actually use. What a console is best played with is a
                    // question about emulators, and the answer to a missing
                    // BIOS is to ask for the BIOS.
                    find::drop_refused(&mut console.wanted, &refused);
                    let wanted: Vec<&str> = console.wanted.iter().map(String::as_str).collect();
                    console.core = cores.first_of(&wanted);
                    // A core on the disk is not the same as a core that can
                    // play something. The ones that read a folder beside
                    // themselves are only half installed until it is there.
                    console.incomplete = match (&console.core, &system) {
                        (Some(core), Some(system)) => assets::wanted(system, &core.name).is_some(),
                        _ => false,
                    };
                    // And what it cannot boot without — every required file it
                    // declares, whether or not this machine has it, because the
                    // shell keeps a row offering to go and find one and that row
                    // has to be there after somebody has. Reported rather than
                    // acted on: these are the console maker's files and nobody
                    // may fetch them.
                    console.needs = match (&console.core, &system, &info) {
                        (Some(core), Some(system), Some(info)) => {
                            firmware::declared(info, system, &core.name)
                                .into_iter()
                                // Less whatever this integration fetches for
                                // itself, which is not a thing to ask anybody
                                // for — see `assets::fetched`.
                                .filter(|need| !assets::fetched(&core.name, &need.path))
                                .collect()
                        }
                        _ => Vec::new(),
                    };
                }
                library.system = system.map(|at| at.to_string_lossy().into_owned());
            }
            // And the pictures already on this disk, which is a look at one
            // listing per console and two `stat`s per game. Nothing is fetched
            // here: a scan runs every time the shell starts and whenever a
            // folder changes, and a scan that reached the network would be the
            // bar waiting on somebody's line to draw a row.
            pictures(&mut library);
            say(&mut out, &library)
        }
        Asked::Art { roms, only, again } => {
            let mut library = scan::library(&roms);
            pictures(&mut library);
            if art::run(&mut out, &library.consoles, &only, again) {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

/// Say which of a library's games already have their pictures on this disk.
///
/// One read of one listing per console, because that is what the matching needs
/// and a game's name is matched against the whole shelf — see [`art`]. A
/// machine with no cache yet answers `None` for everything, which is the same
/// answer it would give for a collection nobody has fetched pictures for.
fn pictures(library: &mut report::Library) {
    let Some(cache) = art::cache() else {
        return;
    };
    for console in &mut library.consoles {
        // Off the disk only — the reading half of `art` takes no agent, so
        // there is nothing here that could reach the server.
        let shelves = art::shelves_here(&cache, consoles::machine(&console.key));
        if shelves.is_empty() {
            continue;
        }
        for rom in &mut console.roms {
            let held = art::already(&cache, &shelves, &rom.title);
            rom.boxart = held.boxart.map(|at| at.to_string_lossy().into_owned());
            rom.snap = held.snap.map(|at| at.to_string_lossy().into_owned());
        }
    }
}

/// Write one record, and answer with whether it could be written.
///
/// A shell that asked a question and got nothing is a shell with a row it
/// cannot draw, so the failure is loud on stderr as well as being an exit
/// status: what puts it there is a closed pipe, which means the shell has gone.
fn say(out: &mut impl Write, record: &impl serde::Serialize) -> ExitCode {
    let line = match serde_json::to_string(record) {
        Ok(line) => line,
        Err(err) => {
            eprintln!("could not write the answer: {err}");
            return ExitCode::FAILURE;
        }
    };
    match writeln!(out, "{line}").and_then(|()| out.flush()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("could not write the answer: {err}");
            ExitCode::FAILURE
        }
    }
}
