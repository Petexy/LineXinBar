//! A game disc in the drive, as a game RetroArch can open.
//!
//! ```text
//!   lxb-retroarch disc /dev/sr0
//!
//!   read the disc ─► which console, which game ─► its name, from RetroArch's
//!   own database ─► the core that plays it ─► somewhere RetroArch can open it
//!   ─► "ready" ─► its cover ─► "pictured" ─► … until the shell lets go
//! ```
//!
//! The one verb of this helper that does not end when it has answered. A
//! disc's game has to be somewhere RetroArch can open for as long as the disc
//! is in, and "somewhere" is this process's to keep:
//!
//! * **A DVD** — a PlayStation 2 disc — is a disc image already: the block
//!   device reads it the way a `.iso` is read, and the PlayStation 2 core opens
//!   the drive itself. So what is kept is a link to the drive under the game's
//!   own name, because RetroArch names save states, a game's own settings and
//!   the rest after the file it was given, and every disc in the drive would
//!   otherwise be `sr0`.
//! * **A CD** is not: its sectors are 2352 bytes and its music is not data at
//!   all, and the emulators want a `.cue` and a `.bin`. So those two are made
//!   up out of the drive with [`crate::fuse`], read a sector at a time as the
//!   game asks.
//!
//! Both live in `$XDG_CACHE_HOME/lxb/discs/<drive>/` — under the home
//! directory, which a sandboxed RetroArch can see at the same path, and never
//! anywhere shared.
//!
//! ## Talking to the shell
//!
//! One JSON [`Disc`] record per line on stdout, as every verb here writes.
//! Stdin carries the other direction: a line saying `again` asks for the core
//! to be looked up afresh — one has just been fetched for this console — a line
//! saying `pictures` that the machine is online again and a missing cover is
//! worth looking for, and the end of stdin is the shell letting go, whether because the disc came out
//! or because the shell itself has gone. So is a SIGTERM, SIGINT or SIGHUP —
//! a session ending signals every process in it — and either way what was put
//! in the cache is taken away before this exits.

use std::io::{self, BufRead, Write};
use std::os::unix::fs::{FileExt, FileTypeExt};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::art;
use crate::drive::{self, Sectors, Toc, Track, BATCH, RAW, USER};
use crate::find;
use crate::fuse::{self, Contents, Entry};
use crate::identify::{self, Identity};
use crate::rdb;
use crate::report::{Console, Disc, DiscStage, Rom, PROTOCOL};

/// Set by a signal asking this process to end, and read by [`run`]'s loop,
/// which then lets go the way it does when the shell does.
static ENDING: AtomicBool = AtomicBool::new(false);

extern "C" fn ending(_: libc::c_int) {
    ENDING.store(true, Ordering::SeqCst);
}

/// End on SIGTERM, SIGINT and SIGHUP by letting go rather than by dying.
///
/// A mount this process made outlives it otherwise: the next run clears it,
/// but until then a directory in somebody's cache answers "transport endpoint
/// is not connected" to anything that looks.
fn end_by_letting_go() {
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        // SAFETY: the handler only stores to an atomic, which is
        // async-signal-safe.
        unsafe {
            libc::signal(
                signal,
                ending as extern "C" fn(libc::c_int) as libc::sighandler_t,
            );
        }
    }
}

/// Where the games of the discs in this machine's drives are put.
pub fn discs_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|at| at.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|home| home.is_absolute())
                .map(|home| home.join(".cache"))
        })
        .map(|cache| cache.join("lxb").join("discs"))
}

/// Serve the disc in `device` until the shell lets go.
///
/// `device` may also be a file — a `.bin` of raw sectors, or an `.iso` — which
/// is read exactly as a disc would be. That is how the path from a disc to a
/// running game is tried on a machine with no disc of the right kind to hand.
pub fn run(out: &mut impl Write, device: &Path) -> ExitCode {
    let mut source = match open(device) {
        Ok(source) => source,
        Err(err) => {
            eprintln!("disc: {} could not be read: {err}", device.display());
            say(
                out,
                &record(DiscStage::Failed, device, "The disc could not be read"),
            );
            return ExitCode::FAILURE;
        }
    };
    let Some(identity) = identify::identify(source.as_mut()) else {
        eprintln!("disc: {} is not a game this can play", device.display());
        say(out, &record(DiscStage::NotAGame, device, "Not a game disc"));
        return ExitCode::SUCCESS;
    };
    let installation = find::installation();
    let database_name = installation
        .as_ref()
        .and_then(|installation| lookup(installation, &identity));
    eprintln!(
        "disc: {} is {} {} ({})",
        device.display(),
        identity.machine.title,
        identity.serial.as_deref().unwrap_or("with no number"),
        database_name.as_deref().unwrap_or("not in the database"),
    );

    let stem = file_stem(database_name.as_deref(), &identity);
    let Some(home) = discs_dir().map(|dir| dir.join(drive_name(device))) else {
        say(
            out,
            &record(
                DiscStage::Failed,
                device,
                "There is nowhere to put the disc",
            ),
        );
        return ExitCode::FAILURE;
    };
    end_by_letting_go();
    let toc = source.toc().clone();
    let served = if toc.dvd {
        link(&home, &stem, device).map(Served::Link)
    } else {
        mount(&home, &stem, source).map(|(content, server)| Served::Mount(content, server))
    };
    let served = match served {
        Ok(served) => served,
        Err(err) => {
            eprintln!("disc: the game could not be put where RetroArch can open it: {err}");
            say(
                out,
                &record(DiscStage::Failed, device, "The disc could not be opened"),
            );
            return ExitCode::FAILURE;
        }
    };

    // What to call it on the bar, and what libretro's pictures are filed under,
    // which are two different names: the bar says "Tekken 4" and the server
    // knows it as the dump's whole name.
    let pictured_as = database_name
        .clone()
        .or_else(|| identity.label.clone())
        .unwrap_or_default();
    let mut console = Console {
        key: identity.machine.aliases[0].to_string(),
        title: identity.machine.title.to_string(),
        glyph: Some(identity.machine.glyph.to_string()),
        core: None,
        wanted: identity
            .machine
            .cores
            .iter()
            .map(|core| core.to_string())
            .collect(),
        incomplete: false,
        needs: Vec::new(),
        roms: vec![Rom {
            title: shown_name(database_name.as_deref(), &identity),
            path: served.content().to_string_lossy().into_owned(),
            within: None,
            boxart: None,
            snap: None,
            // A PSP's disc is a UMD, which no PC drive reads; nothing on a
            // disc in the drive carries pictures of its own.
            icon: None,
            preview: None,
            music: None,
            backdrop: None,
            overlay: None,
        }],
    };
    let mut system = crate::resolve(std::slice::from_mut(&mut console));
    if let Some(cache) = art::cache() {
        let shelves = art::shelves_here(&cache, Some(identity.machine));
        let held = art::already(&cache, &shelves, &pictured_as);
        console.roms[0].boxart = held.boxart.map(|at| at.to_string_lossy().into_owned());
        console.roms[0].snap = held.snap.map(|at| at.to_string_lossy().into_owned());
    }
    let ready = |console: &Console, system: &Option<String>, stage| Disc {
        protocol: PROTOCOL,
        stage,
        device: device.to_string_lossy().into_owned(),
        console: Some(console.clone()),
        serial: identity.serial.clone(),
        system: system.clone(),
        note: String::new(),
    };

    // Everything after this is waited for together: the shell's next line, the
    // shell letting go, and the pictures. On threads of their own so that a
    // disc taken out while its cover is still coming down is let go of at
    // once rather than when the download gives up.
    let (tell, events) = std::sync::mpsc::channel();
    {
        let tell = tell.clone();
        std::thread::spawn(move || {
            for line in io::stdin().lock().lines() {
                let event = match line.as_deref().map(str::trim) {
                    Ok("again") => Event::Again,
                    Ok("pictures") => Event::Pictures,
                    Ok(_) => continue,
                    Err(_) => break,
                };
                if tell.send(event).is_err() {
                    return;
                }
            }
            let _ = tell.send(Event::LetGo);
        });
    }
    let look = |tell: &std::sync::mpsc::Sender<Event>| {
        let tell = tell.clone();
        let machine = identity.machine;
        let name = pictured_as.clone();
        std::thread::spawn(move || {
            let Some(cache) = art::cache() else {
                let _ = tell.send(Event::Looked(art::Looked::default()));
                return;
            };
            let agent = art::agent_with(FIRST_LOOK);
            let _ = tell.send(Event::Looked(art::look_for_one(
                &agent, &cache, machine, &name,
            )));
        });
    };

    // **The cover comes first.** What somebody sees when a disc goes in is the
    // game, and a game is its box: a row that arrived wearing its console's
    // mark and turned into a cover a moment later would be the bar changing
    // under them as they reached for it. So the game is not said until its
    // pictures have been looked for — which is seconds on any connection, and
    // the moment the server cannot be reached on none. Only then is it said
    // without a cover, and the cover is looked for again: when the shell says
    // the machine is back online (`pictures` on stdin), and on a timer of this
    // run's own that backs off, for a network that is up and still cannot
    // reach the server.
    let missing =
        |console: &Console| console.roms[0].boxart.is_none() || console.roms[0].snap.is_none();
    let mut said = false;
    let mut looking = false;
    let mut again_at: Option<(Instant, Duration)> = None;
    if missing(&console) && !pictured_as.is_empty() {
        look(&tell);
        looking = true;
    } else {
        say(out, &ready(&console, &system, DiscStage::Ready));
        said = true;
    }

    loop {
        let event = match events.recv_timeout(Duration::from_millis(250)) {
            Ok(event) => event,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if ENDING.load(Ordering::SeqCst) {
                    break;
                }
                if !looking && again_at.is_some_and(|(at, _)| Instant::now() >= at) {
                    look(&tell);
                    looking = true;
                }
                continue;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };
        match event {
            Event::Again => {
                console.core = None;
                console.incomplete = false;
                console.needs = Vec::new();
                system = crate::resolve(std::slice::from_mut(&mut console));
                // Before the first answer the core is simply part of it.
                if said {
                    say(out, &ready(&console, &system, DiscStage::Ready));
                }
            }
            Event::Pictures => {
                if !looking && missing(&console) && !pictured_as.is_empty() {
                    eprintln!("disc: back online; looking for the pictures again");
                    look(&tell);
                    looking = true;
                }
            }
            Event::Looked(looked) => {
                looking = false;
                let found = !looked.pictures.is_empty();
                if let Some(at) = looked.pictures.boxart {
                    console.roms[0].boxart = Some(at.to_string_lossy().into_owned());
                }
                if let Some(at) = looked.pictures.snap {
                    console.roms[0].snap = Some(at.to_string_lossy().into_owned());
                }
                again_at = match looked.unreached && missing(&console) {
                    true => {
                        let step =
                            again_at.map_or(FIRST_RETRY, |(_, step)| (step * 2).min(LAST_RETRY));
                        eprintln!(
                            "disc: the pictures could not be fetched; asking again in {}s",
                            step.as_secs()
                        );
                        Some((Instant::now() + step, step))
                    }
                    false => None,
                };
                if !said {
                    say(out, &ready(&console, &system, DiscStage::Ready));
                    said = true;
                } else if found {
                    say(out, &ready(&console, &system, DiscStage::Pictured));
                }
            }
            Event::LetGo => break,
        }
    }
    eprintln!("disc: let go of {}", device.display());
    served.take_away(&home);
    ExitCode::SUCCESS
}

/// What the loop at the end of [`run`] waits on.
enum Event {
    /// The shell asks for the core to be looked up again.
    Again,
    /// The shell says the machine is online again: look for the pictures, if
    /// they are still missing.
    Pictures,
    /// The pictures have been looked for.
    Looked(art::Looked),
    /// The shell has let go.
    LetGo,
}

/// How long the first look for a disc's pictures may take. The game is not on
/// the bar until it has answered, so this is what somebody waits at most after
/// putting a disc in on a connection that is up and useless.
const FIRST_LOOK: Duration = Duration::from_secs(20);

/// When a look that could not reach the server is tried again, doubling each
/// time up to the second — for a network that is up and still cannot get out,
/// which the shell's word that the machine is online again does not cover.
const FIRST_RETRY: Duration = Duration::from_secs(60);
const LAST_RETRY: Duration = Duration::from_secs(30 * 60);

/// The disc's name in RetroArch's database, looked up by its catalogue number.
fn lookup(installation: &crate::report::Installation, identity: &Identity) -> Option<String> {
    let serials = identity.serials();
    if serials.is_empty() {
        return None;
    }
    let file = format!("{}.rdb", identity.database);
    find::database_dirs(installation)
        .into_iter()
        .map(|dir| dir.join(&file))
        .find(|at| at.is_file())
        .and_then(|at| rdb::names_for(&at, &serials).into_iter().next())
}

/// What the row says: the game's name without the tags that tell one dump of
/// it from another — `Tekken 4 (Europe, Australia) (En,Fr,De,Es,It) (v2.00)`
/// is Tekken 4 to anybody holding the box — keeping the one tag that matters
/// on a shelf, which disc of a set it is.
///
/// Where the database has no name, the disc's own label, unless that is only
/// the catalogue number over again. Empty where there is nothing, and the
/// shell says "Game disc" in the person's own language.
fn shown_name(database: Option<&str>, identity: &Identity) -> String {
    if let Some(name) = database {
        let kept = untagged(name);
        if !kept.is_empty() {
            return kept;
        }
    }
    identity
        .label
        .as_deref()
        .filter(|label| !looks_like_a_number(label))
        .map(str::to_string)
        .unwrap_or_default()
}

/// A name with its bracketed tags taken off, but for `(Disc 2)`.
fn untagged(name: &str) -> String {
    let mut kept = String::new();
    let mut rest = name;
    while let Some(open) = rest.find(['(', '[']) {
        kept.push_str(&rest[..open]);
        let close = if rest[open..].starts_with('(') {
            ')'
        } else {
            ']'
        };
        let Some(length) = rest[open..].find(close) else {
            rest = &rest[open..];
            break;
        };
        let group = &rest[open..open + length + 1];
        if group[1..]
            .trim_start()
            .to_ascii_lowercase()
            .starts_with("disc ")
        {
            kept.push_str(group);
        }
        rest = &rest[open + length + 1..];
    }
    kept.push_str(rest);
    kept.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether a volume label is a catalogue number rather than a name —
/// `SLUS 00067`, which is how a good many PlayStation discs label themselves.
fn looks_like_a_number(label: &str) -> bool {
    let squeezed: String = label.chars().filter(char::is_ascii_alphanumeric).collect();
    squeezed.len() >= 7
        && squeezed[..4].chars().all(|c| c.is_ascii_alphabetic())
        && squeezed[4..].chars().all(|c| c.is_ascii_digit())
}

/// What the files RetroArch is given are called: the database's name for the
/// game, which is what RetroArch's own scanner would have called it, then the
/// catalogue number, then the label.
///
/// Never anything that could be a path.
fn file_stem(database: Option<&str>, identity: &Identity) -> String {
    let named = database
        .map(str::to_string)
        .or_else(|| identity.serial.clone())
        .or_else(|| identity.label.clone())
        .unwrap_or_else(|| identity.machine.title.to_string());
    let safe: String = named
        .chars()
        .map(|c| match c {
            '/' | '\\' | '\0' | '"' => '-',
            c => c,
        })
        .collect();
    let safe = safe.trim().trim_start_matches('.').to_string();
    if safe.is_empty() {
        "Game".to_string()
    } else {
        safe
    }
}

/// The drive's own name, `sr0`, which is the folder its disc is put in.
fn drive_name(device: &Path) -> String {
    device
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "disc".to_string())
}

/// A record with no game in it: the disc was not one, or could not be read.
fn record(stage: DiscStage, device: &Path, note: &str) -> Disc {
    Disc {
        protocol: PROTOCOL,
        stage,
        device: device.to_string_lossy().into_owned(),
        console: None,
        serial: None,
        system: None,
        note: note.to_string(),
    }
}

fn say(out: &mut impl Write, record: &Disc) {
    if let Ok(line) = serde_json::to_string(record) {
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    }
}

// --- where the game is put --------------------------------------------------

/// What this run put in the cache, to be taken away again.
enum Served {
    /// A link to the drive, for a DVD.
    Link(PathBuf),
    /// The mounted directory's cue sheet, and the thread answering for it.
    Mount(PathBuf, std::thread::JoinHandle<()>),
}

impl Served {
    /// The path RetroArch is given.
    fn content(&self) -> &Path {
        match self {
            Served::Link(at) | Served::Mount(at, _) => at,
        }
    }

    fn take_away(self, home: &Path) {
        match self {
            Served::Link(at) => {
                let _ = std::fs::remove_file(at);
            }
            Served::Mount(_, server) => {
                fuse::unmount(home);
                let _ = server.join();
            }
        }
        let _ = std::fs::remove_dir(home);
    }
}

/// Make `home` an empty directory, whatever an earlier run left in it.
///
/// Everything in it is this helper's — a link, or a mount that outlived the
/// run that made it — so it is all taken away.
fn fresh(home: &Path) -> io::Result<()> {
    fuse::clear(home);
    std::fs::create_dir_all(home)?;
    for entry in std::fs::read_dir(home)?.flatten() {
        let at = entry.path();
        let _ = match entry.file_type() {
            Ok(kind) if kind.is_dir() => std::fs::remove_dir_all(&at),
            _ => std::fs::remove_file(&at),
        };
    }
    Ok(())
}

/// A DVD: a link to the drive, under the game's name.
fn link(home: &Path, stem: &str, device: &Path) -> io::Result<PathBuf> {
    fresh(home)?;
    let target = std::fs::canonicalize(device).unwrap_or_else(|_| device.to_path_buf());
    let at = home.join(format!("{stem}.iso"));
    std::os::unix::fs::symlink(&target, &at)?;
    Ok(at)
}

/// A CD: its cue sheet and its raw sectors, mounted, with a thread to answer
/// for them.
fn mount(
    home: &Path,
    stem: &str,
    mut source: Box<dyn Sectors + Send>,
) -> io::Result<(PathBuf, std::thread::JoinHandle<()>)> {
    fresh(home)?;
    let modes = data_modes(source.as_mut());
    let bin = format!("{stem}.bin");
    let cue = cue_sheet(&bin, source.toc(), &modes);
    let mut image = CdImage::new(format!("{stem}.cue"), cue, bin, source);
    let mount = fuse::Mount::new(home)?;
    let server = std::thread::Builder::new()
        .name("disc".to_string())
        .spawn(move || {
            if let Err(err) = mount.serve(&mut image) {
                eprintln!("disc: serving stopped: {err}");
            }
        })?;
    Ok((home.join(format!("{stem}.cue")), server))
}

/// Each data track's mode, read off its first sector's header: `1` or `2`.
fn data_modes(source: &mut dyn Sectors) -> Vec<(u8, u8)> {
    let tracks: Vec<Track> = source.toc().tracks.clone();
    let mut modes = Vec::new();
    for track in tracks.iter().filter(|track| track.data) {
        let mut raw = vec![0u8; RAW];
        let mode = match source.raw(track.start, 1, &mut raw) {
            Ok(()) if matches!(raw[15], 1 | 2) => raw[15],
            // A drive that will not say is told Mode 1, which is what every
            // data CD but the PlayStation's and the CD-i's is.
            _ => 1,
        };
        modes.push((track.number, mode));
    }
    modes
}

/// The cue sheet for one image of the whole disc.
///
/// A track starts where the table of contents says its music or data does
/// (`INDEX 01`). An audio track straight after a data track has the two
/// seconds of silence every disc is mastered with between them marked as its
/// pregap (`INDEX 00`), which is where a dump made from the same disc puts it.
fn cue_sheet(bin: &str, toc: &Toc, modes: &[(u8, u8)]) -> String {
    let mut sheet = format!("FILE \"{bin}\" BINARY\n");
    let mut previous: Option<&Track> = None;
    for track in &toc.tracks {
        let kind = match track.data {
            true => {
                let mode = modes
                    .iter()
                    .find(|(number, _)| *number == track.number)
                    .map_or(1, |(_, mode)| *mode);
                format!("MODE{mode}/2352")
            }
            false => "AUDIO".to_string(),
        };
        sheet.push_str(&format!("  TRACK {:02} {kind}\n", track.number));
        let after_data =
            previous.is_some_and(|before| before.data && before.start + 150 < track.start);
        if !track.data && after_data {
            sheet.push_str(&format!("    INDEX 00 {}\n", msf(track.start - 150)));
        }
        sheet.push_str(&format!("    INDEX 01 {}\n", msf(track.start)));
        previous = Some(track);
    }
    sheet
}

/// A sector number as minutes, seconds and frames, 75 frames a second.
fn msf(lba: u32) -> String {
    format!("{:02}:{:02}:{:02}", lba / 4500, (lba / 75) % 60, lba % 75)
}

/// The two files a CD is served as.
struct CdImage {
    entries: Vec<Entry>,
    cue: Vec<u8>,
    source: Box<dyn Sectors + Send>,
    /// The spans of the disc that are music, where a sector that will not read
    /// is better played as a moment of silence than as a game stopping.
    music: Vec<(u32, u32)>,
    /// The last stretches read, by their first sector. A game reads a disc in
    /// runs, and a run asked for 4 KiB at a time is one READ CD per stretch
    /// rather than one per request.
    held: Vec<(u32, Vec<u8>)>,
}

/// How many stretches are kept: about two and a half megabytes.
const HELD: usize = 32;

impl CdImage {
    fn new(
        cue_name: String,
        cue: String,
        bin_name: String,
        source: Box<dyn Sectors + Send>,
    ) -> CdImage {
        let toc = source.toc();
        let music = toc
            .tracks
            .iter()
            .filter(|track| !track.data)
            .map(|track| (track.start.saturating_sub(150), toc.end_of(track)))
            .collect();
        let size = u64::from(toc.leadout) * RAW as u64;
        CdImage {
            entries: vec![
                Entry {
                    name: cue_name,
                    size: cue.len() as u64,
                },
                Entry {
                    name: bin_name,
                    size,
                },
            ],
            cue: cue.into_bytes(),
            source,
            music,
            held: Vec::new(),
        }
    }

    fn is_music(&self, lba: u32) -> bool {
        self.music
            .iter()
            .any(|(from, to)| (*from..*to).contains(&lba))
    }

    /// The stretch of sectors starting at `first`, read if it is not held.
    fn stretch(&mut self, first: u32) -> io::Result<&[u8]> {
        if let Some(at) = self.held.iter().position(|(start, _)| *start == first) {
            let found = self.held.remove(at);
            self.held.push(found);
            return Ok(&self
                .held
                .last()
                .map(|(_, bytes)| bytes)
                .expect("just pushed")[..]);
        }
        let leadout = self.source.toc().leadout;
        let count = BATCH.min(leadout.saturating_sub(first));
        let mut bytes = vec![0u8; count as usize * RAW];
        if self.source.raw(first, count, &mut bytes).is_err() {
            // Somewhere in the stretch is a sector that will not read. Each is
            // asked for alone, so one bad sector costs itself and not its
            // neighbours.
            for at in 0..count {
                let sector = &mut bytes[at as usize * RAW..(at as usize + 1) * RAW];
                let lba = first + at;
                let mut read = self.source.raw(lba, 1, sector);
                if read.is_err() {
                    read = self.source.raw(lba, 1, sector);
                }
                if let Err(err) = read {
                    if self.is_music(lba) {
                        sector.fill(0);
                    } else {
                        return Err(io::Error::other(format!(
                            "sector {lba} will not read: {err}"
                        )));
                    }
                }
            }
        }
        if self.held.len() >= HELD {
            self.held.remove(0);
        }
        self.held.push((first, bytes));
        Ok(&self
            .held
            .last()
            .map(|(_, bytes)| bytes)
            .expect("just pushed")[..])
    }
}

impl Contents for CdImage {
    fn entries(&self) -> &[Entry] {
        &self.entries
    }

    fn read(&mut self, file: usize, offset: u64, into: &mut [u8]) -> io::Result<usize> {
        if file == 0 {
            let from = (offset as usize).min(self.cue.len());
            let count = into.len().min(self.cue.len() - from);
            into[..count].copy_from_slice(&self.cue[from..from + count]);
            return Ok(count);
        }
        let size = self.entries[1].size;
        let mut filled = 0;
        while filled < into.len() && offset + (filled as u64) < size {
            let at = offset + filled as u64;
            let lba = (at / RAW as u64) as u32;
            let first = lba - lba % BATCH;
            let stretch = self.stretch(first)?;
            let within = (at - u64::from(first) * RAW as u64) as usize;
            let count = (into.len() - filled).min(stretch.len() - within);
            into[filled..filled + count].copy_from_slice(&stretch[within..within + count]);
            filled += count;
        }
        Ok(filled)
    }
}

// --- where the disc is read from ------------------------------------------------

/// The drive, or a file standing in for one.
fn open(device: &Path) -> io::Result<Box<dyn Sectors + Send>> {
    let kind = std::fs::metadata(device)?.file_type();
    if kind.is_block_device() {
        return Ok(Box::new(drive::Drive::open(device)?));
    }
    Ok(Box::new(Image::open(device)?))
}

/// An image of a disc on the disk: a `.iso` read as a DVD is, anything else as
/// raw CD sectors, one data track the length of the file.
struct Image {
    file: std::fs::File,
    toc: Toc,
}

impl Image {
    fn open(at: &Path) -> io::Result<Image> {
        let file = std::fs::File::open(at)?;
        let length = file.metadata()?.len();
        let dvd = at
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("iso"));
        let sector = if dvd { USER } else { RAW } as u64;
        Ok(Image {
            file,
            toc: Toc {
                tracks: vec![Track {
                    number: 1,
                    start: 0,
                    data: true,
                }],
                leadout: (length / sector) as u32,
                dvd,
            },
        })
    }
}

impl Sectors for Image {
    fn toc(&self) -> &Toc {
        &self.toc
    }

    fn raw(&mut self, lba: u32, count: u32, into: &mut [u8]) -> io::Result<()> {
        if self.toc.dvd {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "a DVD has no raw sectors",
            ));
        }
        let wanted = count as usize * RAW;
        self.file
            .read_exact_at(&mut into[..wanted], u64::from(lba) * RAW as u64)
    }

    fn user(&mut self, lba: u32) -> io::Result<Vec<u8>> {
        if self.toc.dvd {
            let mut sector = vec![0u8; USER];
            self.file
                .read_exact_at(&mut sector, u64::from(lba) * USER as u64)?;
            return Ok(sector);
        }
        let mut raw = vec![0u8; RAW];
        self.raw(lba, 1, &mut raw)?;
        drive::user_data(&raw)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "not a data sector"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consoles;

    fn identity(label: Option<&str>, serial: Option<&str>) -> Identity {
        Identity {
            machine: consoles::machine("ps2").expect("ps2"),
            database: "Sony - PlayStation 2",
            serial: serial.map(str::to_string),
            label: label.map(str::to_string),
        }
    }

    #[test]
    fn the_row_names_the_game_without_the_dump_tags() {
        assert_eq!(
            untagged("Tekken 4 (Europe, Australia) (En,Fr,De,Es,It) (v2.00)"),
            "Tekken 4"
        );
        assert_eq!(
            untagged("Final Fantasy VII (USA) (Disc 2)"),
            "Final Fantasy VII (Disc 2)"
        );
        assert_eq!(untagged("Ico [Demo] (Europe)"), "Ico");
    }

    /// A label that is only the catalogue number again is no name at all, and
    /// the shell says "Game disc" instead.
    #[test]
    fn a_label_that_is_a_number_is_not_a_name() {
        assert_eq!(shown_name(None, &identity(Some("SLUS 00067"), None)), "");
        assert_eq!(
            shown_name(None, &identity(Some("TEKKEN4"), None)),
            "TEKKEN4"
        );
        assert_eq!(
            shown_name(
                Some("Tekken 4 (Europe, Australia)"),
                &identity(Some("TEKKEN4"), None)
            ),
            "Tekken 4"
        );
    }

    #[test]
    fn the_file_is_named_after_the_database_and_nothing_can_make_it_a_path() {
        let found = identity(Some("TEKKEN4"), Some("SCES-50878"));
        assert_eq!(
            file_stem(
                Some("Tekken 4 (Europe, Australia) (En,Fr,De,Es,It) (v2.00)"),
                &found
            ),
            "Tekken 4 (Europe, Australia) (En,Fr,De,Es,It) (v2.00)"
        );
        assert_eq!(file_stem(None, &found), "SCES-50878");
        assert_eq!(
            file_stem(Some("../../etc/passwd"), &found),
            "-..-etc-passwd"
        );
    }

    /// A PlayStation disc with CD music after its data: the two seconds of
    /// silence between them are the music track's pregap.
    #[test]
    fn a_cue_sheet_marks_the_pregap_after_the_data() {
        let toc = Toc {
            tracks: vec![
                Track {
                    number: 1,
                    start: 0,
                    data: true,
                },
                Track {
                    number: 2,
                    start: 250_150,
                    data: false,
                },
                Track {
                    number: 3,
                    start: 260_000,
                    data: false,
                },
            ],
            leadout: 270_000,
            dvd: false,
        };
        let sheet = cue_sheet("Game.bin", &toc, &[(1, 2)]);
        assert_eq!(
            sheet,
            "FILE \"Game.bin\" BINARY\n\
             \x20 TRACK 01 MODE2/2352\n\
             \x20   INDEX 01 00:00:00\n\
             \x20 TRACK 02 AUDIO\n\
             \x20   INDEX 00 55:33:25\n\
             \x20   INDEX 01 55:35:25\n\
             \x20 TRACK 03 AUDIO\n\
             \x20   INDEX 01 57:46:50\n"
        );
    }

    /// A disc whose sectors are numbered, so that what comes out of the image
    /// can be checked against where it was asked from.
    struct Numbered {
        toc: Toc,
        bad: Option<u32>,
    }

    impl Sectors for Numbered {
        fn toc(&self) -> &Toc {
            &self.toc
        }

        fn raw(&mut self, lba: u32, count: u32, into: &mut [u8]) -> io::Result<()> {
            for at in 0..count {
                if Some(lba + at) == self.bad {
                    return Err(io::Error::other("scratched"));
                }
                into[at as usize * RAW..(at as usize + 1) * RAW].fill(((lba + at) % 250) as u8 + 1);
            }
            Ok(())
        }
    }

    fn numbered(bad: Option<u32>) -> CdImage {
        let toc = Toc {
            tracks: vec![
                Track {
                    number: 1,
                    start: 0,
                    data: true,
                },
                Track {
                    number: 2,
                    start: 400,
                    data: false,
                },
            ],
            leadout: 500,
            dvd: false,
        };
        let cue = cue_sheet("G.bin", &toc, &[(1, 1)]);
        CdImage::new(
            "G.cue".into(),
            cue,
            "G.bin".into(),
            Box::new(Numbered { toc, bad }),
        )
    }

    #[test]
    fn a_read_across_stretches_gets_every_sector_it_spans() {
        let mut image = numbered(None);
        assert_eq!(image.entries()[1].size, 500 * RAW as u64);
        // From the middle of sector 31 into sector 33, across a stretch.
        let mut into = vec![0u8; RAW * 2];
        let offset = 31 * RAW as u64 + 100;
        assert_eq!(image.read(1, offset, &mut into).expect("read"), RAW * 2);
        assert_eq!(into[0], 32);
        assert_eq!(into[RAW - 100], 33);
        assert_eq!(into[RAW * 2 - 1], 34);
    }

    #[test]
    fn a_read_stops_at_the_end_of_the_disc() {
        let mut image = numbered(None);
        let mut into = vec![0u8; RAW * 4];
        let offset = 499 * RAW as u64;
        assert_eq!(image.read(1, offset, &mut into).expect("read"), RAW);
    }

    /// A scratch in the music is a moment of silence; a scratch in the data is
    /// an error, because a game handed the wrong bytes does worse than stop.
    #[test]
    fn a_bad_sector_is_silence_in_music_and_an_error_in_data() {
        let mut music = numbered(Some(450));
        let mut into = vec![0u8; RAW];
        music.read(1, 450 * RAW as u64, &mut into).expect("read");
        assert!(into.iter().all(|&byte| byte == 0));
        let mut into = vec![0u8; RAW];
        music.read(1, 451 * RAW as u64, &mut into).expect("read");
        assert_eq!(into[0], (451 % 250) as u8 + 1);

        let mut data = numbered(Some(10));
        let mut into = vec![0u8; RAW];
        assert!(data.read(1, 10 * RAW as u64, &mut into).is_err());
    }

    #[test]
    fn the_cue_is_served_as_written() {
        let mut image = numbered(None);
        let mut into = vec![0u8; 4096];
        let count = image.read(0, 0, &mut into).expect("read");
        assert!(String::from_utf8_lossy(&into[..count]).starts_with("FILE \"G.bin\" BINARY"));
    }
}
