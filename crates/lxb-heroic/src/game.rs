//! Putting one game on the disk and taking it off again, exactly as Heroic
//! does both.
//!
//! ## An Epic game
//!
//! Heroic's own `legendary install`, word for word — the platform, Heroic's
//! own install folder, no DLC, no selective download, `-y` — with Heroic's
//! own download settings, and its one retry: a `MemoryError` is asked again
//! with `--max-shared-memory 5000`. Its progress is read the way Heroic reads
//! it, from the lines legendary writes as it goes:
//!
//! ```text
//! [cli] INFO: Download size: 130.81 MiB (Compression savings: 62.2%)
//! [DLManager] INFO: = Progress: 74.73% (846/1132), Running for 00:00:01, ETA: 00:00:00
//! [DLManager] INFO:  - Downloaded: 68.69 MiB, Written: 240.00 MiB
//! ```
//!
//! The download size legendary states is what is *left*, which after a
//! stopped download is less than the game: the share is counted against the
//! whole game, as Heroic counts it, so a download picked up again starts its
//! bar where the last one left it rather than at nought.
//!
//! A download stopped halfway is kept where it is. legendary picks it up from
//! there the next time the same game is asked for, and says so itself.
//!
//! ## An EA or Ubisoft title
//!
//! What Heroic does on Linux, which is two small things: the other store's
//! installer is fetched into Heroic's own folder for them, and the game is
//! written into Heroic's `third-party-installed.json`. Nothing is run. The
//! first launch runs that installer inside the game's prefix, and the game
//! itself then comes down in that store's own window. See `EPIC-TO-DO.MD`.
//!
//! ## Never behind an open Heroic's back
//!
//! Heroic reads what is installed when it starts and keeps it. A game put on
//! the disk while it is open is one it goes on thinking is not there, and the
//! next press of Play hands the game to that Heroic — which answers with a
//! question in its own window, offering to install it. So both verbs refuse
//! while any Heroic is running, the same way signing in does.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;

use crate::heroic::{self, Paths};
use crate::library;
use crate::report::{Download, Installation, Reason, Removal, Size, Step, Store, PROTOCOL};

/// Where the two other stores' installers come from — Heroic's own addresses.
const UBISOFT_INSTALLER: (&str, &str) = (
    "UbisoftConnectInstaller.exe",
    "https://static3.cdn.ubi.com/orbit/launcher_installer/UbisoftConnectInstaller.exe",
);
const EA_INSTALLER: (&str, &str) = (
    "EAappInstaller.exe",
    "https://origin-a.akamaihd.net/EA-Desktop-Client-Download/installer-releases/EAappInstaller.exe",
);

/// How large an installer this will take. Both are a few megabytes; this is a
/// ceiling against a server that sends something else entirely.
const LARGEST_INSTALLER: u64 = 512 * 1024 * 1024;

/// Heroic's retry for a machine whose shared memory legendary could not get.
const SHARED_MEMORY: &str = "5000";

const MIB: f64 = 1024.0 * 1024.0;

/// What installing a game would take: its size from Epic, and where it would go.
pub fn size(installation: &Installation, paths: &Paths, home: &Path, app_name: &str) -> Size {
    let mut answer = Size {
        protocol: PROTOCOL,
        app_name: app_name.to_string(),
        download: None,
        disk: None,
        base: Some(
            heroic::install_base(paths, home)
                .to_string_lossy()
                .into_owned(),
        ),
        store: None,
        reason: None,
    };
    let Some(game) = owned(paths, app_name) else {
        answer.reason = Some(Reason::NotOwned);
        return answer;
    };
    answer.store = game.store;
    if game.store.is_some() {
        // The other store downloads it and says how large it is.
        return answer;
    }
    let out = heroic::output(
        heroic::legendary(installation, paths)
            .args(["info", app_name, "--json", "--platform", "Windows"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    );
    let out = match out {
        Ok(out) => out,
        Err(err) => {
            eprintln!("size: flatpak could not be run: {err}");
            answer.reason = Some(Reason::NoHeroic);
            return answer;
        }
    };
    let said = String::from_utf8_lossy(&out.stderr);
    match serde_json::from_slice::<Value>(&out.stdout) {
        Ok(info) if out.status.success() => {
            let manifest = &info["manifest"];
            answer.download = manifest["download_size"].as_u64().filter(|size| *size > 0);
            answer.disk = manifest["disk_size"].as_u64().filter(|size| *size > 0);
            if answer.download.is_none() {
                eprintln!("size: legendary gave no size for {app_name}");
                answer.reason = Some(library::why_legendary_failed(&said));
            }
        }
        _ => {
            for line in said.lines() {
                eprintln!("legendary: {line}");
            }
            answer.reason = Some(library::why_legendary_failed(&said));
        }
    }
    answer
}

/// Put one game on the disk. Whether it ended installed.
pub fn get(
    out: &mut impl Write,
    installation: &Installation,
    paths: &Paths,
    home: &Path,
    app_name: &str,
) -> bool {
    let mut told = Teller::new(out, app_name);
    if heroic::in_the_way() {
        return told.failed(Reason::HeroicRunning, "Heroic is open".into());
    }
    if heroic::account(paths).is_none() {
        return told.failed(Reason::SignedOut, "nobody is signed in".into());
    }
    let Some(game) = owned(paths, app_name) else {
        return told.failed(Reason::NotOwned, format!("the account has no {app_name}"));
    };
    match &game.installed {
        Some(installed) if installed.update && game.store.is_none() => {
            return update(&mut told, installation, paths);
        }
        Some(_) => {
            told.say(Step::Done, Some(1.0), "it is already installed".into());
            return true;
        }
        None => {}
    }
    match game.store {
        Some(store @ (Store::Ea | Store::Ubisoft)) => get_elsewhere(&mut told, paths, store),
        Some(Store::Other) => told.failed(
            Reason::Legendary,
            "a third-party store Heroic cannot install for".into(),
        ),
        None => get_from_epic(&mut told, installation, paths, home),
    }
}

/// An Epic game, through Heroic's legendary.
fn get_from_epic(
    told: &mut Teller<impl Write>,
    installation: &Installation,
    paths: &Paths,
    home: &Path,
) -> bool {
    let app_name = told.app_name.clone();
    told.say(Step::Preparing, None, "asking Epic about it".into());
    // The whole game's size, for the bar; a failure here is not the
    // download's, which says for itself why it could not start.
    let whole = size(installation, paths, home, &app_name).download;
    told.size = whole;

    let base = heroic::install_base(paths, home);
    let mut arguments: Vec<String> = [
        "install",
        app_name.as_str(),
        "--platform",
        "Windows",
        "--base-path",
    ]
    .iter()
    .map(|word| word.to_string())
    .collect();
    arguments.push(base.to_string_lossy().into_owned());
    arguments.extend(["--skip-dlcs", "-y"].iter().map(|word| word.to_string()));
    if let Some(workers) = heroic::max_workers(paths) {
        arguments.push("--max-workers".to_string());
        arguments.push(workers.to_string());
    }
    if heroic::no_https(paths) {
        arguments.push("--no-https".to_string());
    }
    arguments.push("--skip-sdl".to_string());

    let mut said = run_install(told, installation, paths, &arguments, whole);
    if said.contains("MemoryError:") && is_installed(paths, &app_name) == Some(false) {
        eprintln!("get: legendary ran out of shared memory; asking again as Heroic does");
        arguments.push("--max-shared-memory".to_string());
        arguments.push(SHARED_MEMORY.to_string());
        said = run_install(told, installation, paths, &arguments, whole);
    }
    if is_installed(paths, &app_name) == Some(false) && all_of_it_arrived(&said) {
        if let Some(path) = install_path_said(&said) {
            said.push_str(&finish_what_arrived(
                told,
                installation,
                paths,
                &app_name,
                &path,
            ));
        }
    }

    match is_installed(paths, &app_name) {
        Some(true) => {
            // Saves kept with Epic: the new game's save folder, found now so
            // its first launch already syncs. See `saves.rs`.
            if heroic::cloud_saves(paths) {
                crate::saves::find_folders(installation, paths);
            }
            told.say(Step::Done, Some(1.0), "installed".into());
            true
        }
        Some(false) | None => told.failed(why_it_did_not_install(&said), last_words(&said)),
    }
}

/// Check an installed game's every file against Epic's manifest and fetch
/// whatever is missing or wrong: legendary's own `repair`, which is Heroic's
/// Verify and Repair, with Heroic's download settings.
///
/// Two stretches, said as two steps: the check, which legendary counts on
/// its standard output, and then the download of what was wrong, which it
/// counts where it counts an install's. A copy with nothing wrong has nothing
/// to download and ends at the check.
pub fn repair(
    out: &mut impl Write,
    installation: &Installation,
    paths: &Paths,
    app_name: &str,
) -> bool {
    let mut told = Teller::new(out, app_name);
    if heroic::in_the_way() {
        return told.failed(Reason::HeroicRunning, "Heroic is open".into());
    }
    if heroic::account(paths).is_none() {
        return told.failed(Reason::SignedOut, "nobody is signed in".into());
    }
    let Some(game) = owned(paths, app_name) else {
        return told.failed(Reason::NotOwned, format!("the account has no {app_name}"));
    };
    if game.installed.is_none() || game.store.is_some() {
        return told.failed(
            Reason::Legendary,
            "not a game Epic installed here, so there are no files of Epic's to check".into(),
        );
    }
    told.say(Step::Checking, None, "checking its files".into());
    let mut arguments: Vec<String> = ["repair", app_name, "-y", "--skip-sdl"]
        .iter()
        .map(|word| word.to_string())
        .collect();
    if let Some(workers) = heroic::max_workers(paths) {
        arguments.push("--max-workers".to_string());
        arguments.push(workers.to_string());
    }
    if heroic::no_https(paths) {
        arguments.push("--no-https".to_string());
    }
    let (said, worked) = run_repair(&mut told, installation, paths, &arguments);
    if worked {
        told.say(Step::Done, Some(1.0), "every file checked".into());
        true
    } else {
        told.failed(why_it_did_not_install(&said), last_words(&said))
    }
}

/// What one of legendary's two streams said during a repair.
enum Said {
    /// How far the check has got, as a share of one.
    Checked(f32),
    /// A line of the download's log, or of anything else it wrote.
    Log(String),
    /// A line of what it printed rather than logged.
    Printed(String),
}

/// One `legendary repair`: its check read off standard output — where it is
/// written with carriage returns, one line rewritten in place — and its
/// download off the log, both into one channel so neither stream waits on
/// the other. Everything it said, and whether it ended well.
fn run_repair(
    told: &mut Teller<impl Write>,
    installation: &Installation,
    paths: &Paths,
    arguments: &[String],
) -> (String, bool) {
    let spawned = heroic::legendary(installation, paths)
        .args(arguments)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(err) => {
            eprintln!("repair: flatpak could not be run: {err}");
            return (String::new(), false);
        }
    };
    let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
    let mut said = String::new();
    let mut watch = Watch::new(None);
    // A check writes a line per file; the shell is told once a percent.
    let mut checked_said: Option<u32> = None;
    std::thread::scope(|scope| {
        let (tell, heard) = std::sync::mpsc::channel::<Said>();
        if let Some(stdout) = stdout {
            let tell = tell.clone();
            scope.spawn(move || {
                let mut bytes = Vec::new();
                for byte in BufReader::new(stdout).bytes().map_while(Result::ok) {
                    if byte != b'\r' && byte != b'\n' {
                        bytes.push(byte);
                        continue;
                    }
                    let line = String::from_utf8_lossy(&bytes).trim().to_string();
                    bytes.clear();
                    let said = match checked(&line) {
                        Some(share) => Said::Checked(share),
                        None if line.is_empty() => continue,
                        None => Said::Printed(line),
                    };
                    if tell.send(said).is_err() {
                        break;
                    }
                }
            });
        }
        if let Some(stderr) = stderr {
            let tell = tell.clone();
            scope.spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    if tell.send(Said::Log(line)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(tell);
        for said_now in heard {
            match said_now {
                Said::Checked(share) => {
                    let percent = (share * 100.0).floor() as u32;
                    if checked_said != Some(percent) {
                        checked_said = Some(percent);
                        told.say(Step::Checking, Some(share.min(0.99)), "checking".into());
                    }
                }
                Said::Log(line) => {
                    if !is_progress_detail(&line) {
                        eprintln!("legendary: {line}");
                    }
                    if watch.read(&line) {
                        told.downloaded = watch.downloaded();
                        if told.size.is_none() {
                            told.size = watch.whole();
                        }
                        told.say(Step::Downloading, watch.share(), "repairing".into());
                    }
                    said.push_str(&line);
                    said.push('\n');
                }
                Said::Printed(line) => {
                    eprintln!("legendary: {line}");
                    said.push_str(&line);
                    said.push('\n');
                }
            }
        }
    });
    let status = child.wait();
    eprintln!("repair: legendary ended {status:?}");
    let worked = status.is_ok_and(|status| status.success());
    (said, worked)
}

/// The share of a check done, out of legendary's
/// `Verification progress: 12/770 (1.6%) [3.2 MiB/s]`.
fn checked(line: &str) -> Option<f32> {
    let (_, rest) = line.split_once("Verification progress: ")?;
    let (counted, _) = rest.split_once(' ')?;
    let (done, total) = counted.split_once('/')?;
    let (done, total): (f32, f32) = (done.parse().ok()?, total.parse().ok()?);
    (total > 0.0).then(|| (done / total).clamp(0.0, 1.0))
}

/// Bring an installed game up to Epic's newest build: Heroic's own
/// `legendary update`, with its download settings. What legendary says it has
/// to download is the difference, not the game, so the bar is measured against
/// that — asking Epic for the whole game's size here would start it at 95%.
fn update(told: &mut Teller<impl Write>, installation: &Installation, paths: &Paths) -> bool {
    let app_name = told.app_name.clone();
    told.say(Step::Preparing, None, "asking Epic what has changed".into());
    let mut arguments: Vec<String> = ["update", app_name.as_str(), "-y", "--skip-sdl"]
        .iter()
        .map(|word| word.to_string())
        .collect();
    if let Some(workers) = heroic::max_workers(paths) {
        arguments.push("--max-workers".to_string());
        arguments.push(workers.to_string());
    }
    if heroic::no_https(paths) {
        arguments.push("--no-https".to_string());
    }
    let said = run_install(told, installation, paths, &arguments, None);
    let current = owned(paths, &app_name)
        .and_then(|game| game.installed)
        .is_some_and(|installed| !installed.update);
    if current {
        told.say(Step::Done, Some(1.0), "updated".into());
        true
    } else {
        told.failed(why_it_did_not_install(&said), last_words(&said))
    }
}

/// One `legendary install`, forwarding its progress. Everything it said,
/// both streams, for deciding afterwards what went wrong.
fn run_install(
    told: &mut Teller<impl Write>,
    installation: &Installation,
    paths: &Paths,
    arguments: &[String],
    whole: Option<u64>,
) -> String {
    let spawned = heroic::legendary(installation, paths)
        .args(arguments)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(err) => {
            eprintln!("get: flatpak could not be run: {err}");
            return String::new();
        }
    };
    // What legendary prints rather than logs — the requirements it checked,
    // and why it will not go on — comes out on stdout.
    let printed = child.stdout.take().map(|stdout| {
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = BufReader::new(stdout).read_to_string(&mut text);
            text
        })
    });
    let mut said = String::new();
    let mut watch = Watch::new(whole);
    if let Some(stderr) = child.stderr.take() {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if !is_progress_detail(&line) {
                eprintln!("legendary: {line}");
            }
            if watch.read(&line) {
                told.downloaded = watch.downloaded();
                if told.size.is_none() {
                    told.size = watch.whole();
                }
                told.say(Step::Downloading, watch.share(), "downloading".into());
            }
            said.push_str(&line);
            said.push('\n');
        }
    }
    let status = child.wait();
    let printed = printed
        .and_then(|thread| thread.join().ok())
        .unwrap_or_default();
    for line in printed.lines().filter(|line| !line.trim().is_empty()) {
        eprintln!("legendary: {line}");
    }
    eprintln!("get: legendary ended {status:?}");
    said.push_str(&printed);
    said
}

/// Whether legendary found nothing left to download for a game that is not
/// installed: a download stopped after its last file was written and before
/// legendary had written the game down. Measured on 2026-09-25 — stopped at
/// 5.3 s, all 770 of Cat Quest's files were on the disk and the game was not in
/// `installed.json`. Asked again, legendary skips every file on its resume data,
/// says "Download size is 0", and exits — crashing on the way, since it takes a
/// game with nothing to download to be one that is installed. So it never
/// installs, however often it is asked. Heroic does the same.
fn all_of_it_arrived(said: &str) -> bool {
    said.contains("Download size is 0")
}

/// The folder legendary said it was installing into.
fn install_path_said(said: &str) -> Option<String> {
    said.lines()
        .find_map(|line| line.split_once("Install path: ").map(|(_, at)| at.trim()))
        .filter(|at| at.starts_with('/'))
        .map(str::to_string)
}

/// Write a game whose every file arrived into legendary's list of what is
/// installed, the two ways legendary itself offers: `import` names the folder
/// as the game's, and `repair` checks every file of it against the manifest
/// and fetches whatever is wrong — which `import` asks for, since it cannot
/// know the copy is whole. Then the resume data goes: it describes a download
/// that has finished, and a later install after an uninstall would otherwise
/// skip every file on the strength of it and install nothing at all.
fn finish_what_arrived(
    told: &mut Teller<impl Write>,
    installation: &Installation,
    paths: &Paths,
    app_name: &str,
    path: &str,
) -> String {
    eprintln!("get: every file had arrived; writing the game down from {path}");
    told.say(
        Step::Preparing,
        None,
        "finishing a download that had arrived".into(),
    );
    let imported = heroic::output(
        heroic::legendary(installation, paths)
            .args([
                "import",
                app_name,
                path,
                "--skip-dlcs",
                "--platform",
                "Windows",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    );
    let mut said = match imported {
        Ok(out) => {
            let mut said = String::from_utf8_lossy(&out.stderr).into_owned();
            said.push_str(&String::from_utf8_lossy(&out.stdout));
            said
        }
        Err(err) => format!("flatpak could not be run: {err}\n"),
    };
    for line in said.lines().filter(|line| !line.trim().is_empty()) {
        eprintln!("legendary: {line}");
    }
    if is_installed(paths, app_name) != Some(true) {
        return said;
    }
    let repair: Vec<String> = ["repair", app_name, "-y", "--skip-sdl"]
        .iter()
        .map(|word| word.to_string())
        .collect();
    said.push_str(&run_install(told, installation, paths, &repair, None));
    let resume = paths
        .legendary()
        .join("tmp")
        .join(format!("{app_name}.resume"));
    if let Err(err) = std::fs::remove_file(&resume) {
        if err.kind() != std::io::ErrorKind::NotFound {
            eprintln!("get: {}: {err}", resume.display());
        }
    }
    said
}

/// The lines legendary writes every second of a download, which are the bar
/// and are not worth the log's space.
fn is_progress_detail(line: &str) -> bool {
    line.contains("= Progress:")
        || line.contains(" - Downloaded:")
        || line.contains(" - Cache usage:")
        || line.contains(" + Download\t")
        || line.contains(" + Disk\t")
}

/// Why an install did not happen, from what legendary said.
fn why_it_did_not_install(said: &str) -> Reason {
    if said.contains("Not enough available disk space") {
        Reason::NoSpace
    } else if said.contains("Failed to acquire installed data lock") {
        // Somebody else's legendary is installing something — Heroic's own.
        Reason::HeroicRunning
    } else {
        library::why_legendary_failed(said)
    }
}

/// The last thing legendary said, for the log.
fn last_words(said: &str) -> String {
    said.lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .unwrap_or("legendary said nothing")
        .to_string()
}

/// How far a download has got, from legendary's lines.
#[derive(Debug, Clone, PartialEq)]
struct Watch {
    /// The whole game's download, in bytes, where Epic said.
    whole: Option<u64>,
    /// What legendary said was left to download, in MiB.
    left: Option<f64>,
    /// legendary's own percentage of what was left.
    percent: Option<f64>,
    /// What has come down in this run, in MiB.
    downloaded: Option<f64>,
}

impl Watch {
    fn new(whole: Option<u64>) -> Watch {
        Watch {
            whole,
            left: None,
            percent: None,
            downloaded: None,
        }
    }

    /// Take in one line. Whether the bar moved.
    fn read(&mut self, line: &str) -> bool {
        if let Some(left) = number_after(line, "Download size: ") {
            self.left = Some(left);
            return false;
        }
        if let Some(percent) = number_after(line, "= Progress: ") {
            self.percent = Some(percent);
            return false;
        }
        if let Some(downloaded) = number_after(line, " - Downloaded: ") {
            self.downloaded = Some(downloaded);
            return true;
        }
        false
    }

    /// The whole game, in bytes: what Epic said, or else what legendary said
    /// was left, which is the whole of a download that is starting fresh.
    fn whole(&self) -> Option<u64> {
        self.whole
            .or_else(|| self.left.map(|left| (left * MIB).round() as u64))
    }

    /// Bytes down so far, counting what an earlier run left.
    fn downloaded(&self) -> Option<u64> {
        let downloaded = self.downloaded? * MIB;
        // legendary writes sizes to a hundredth of a MiB, so a fresh download
        // of the whole game still differs from Epic's byte count by a few
        // kilobytes. That is rounding, not an earlier run.
        let already = match (self.whole, self.left) {
            (Some(whole), Some(left)) => whole as f64 - left * MIB,
            _ => 0.0,
        };
        let already = if already < 0.01 * MIB { 0.0 } else { already };
        Some((downloaded + already).round() as u64)
    }

    /// The share of the whole game that is here. Never all of it while
    /// legendary is still going: the last of it is written after the last
    /// byte arrives, and `Done` is only said once the game is on Heroic's list.
    fn share(&self) -> Option<f32> {
        let share = match (self.whole(), self.downloaded()) {
            (Some(whole), Some(downloaded)) if whole > 0 => downloaded as f64 / whole as f64,
            _ => self.percent? / 100.0,
        };
        Some(share.clamp(0.0, 0.99) as f32)
    }
}

/// The number written right after `marker` in a line — `12.34` out of
/// `… = Progress: 12.34% (…)`.
fn number_after(line: &str, marker: &str) -> Option<f64> {
    let (_, rest) = line.split_once(marker)?;
    let number: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    number.parse().ok()
}

/// An EA or Ubisoft title, as Heroic does it on Linux: fetch that store's
/// installer where Heroic keeps it, and mark the game installed.
fn get_elsewhere(told: &mut Teller<impl Write>, paths: &Paths, store: Store) -> bool {
    let (file, address) = match store {
        Store::Ubisoft => UBISOFT_INSTALLER,
        _ => EA_INSTALLER,
    };
    let folder = paths.redist();
    let installer = folder.join(file);
    if installer.is_file() {
        eprintln!("get: {} is already here", installer.display());
    } else {
        told.say(Step::Downloading, Some(0.0), format!("fetching {file}"));
        if let Err((reason, why)) = fetch_installer(told, &folder, file, address) {
            return told.failed(reason, why);
        }
    }
    if heroic::in_the_way() {
        return told.failed(Reason::HeroicRunning, "Heroic is open".into());
    }
    match mark_elsewhere(paths, &told.app_name) {
        Ok(()) => {
            told.say(
                Step::Done,
                Some(1.0),
                "marked installed, as Heroic does".into(),
            );
            true
        }
        Err(why) => told.failed(Reason::Files, why),
    }
}

fn fetch_installer(
    told: &mut Teller<impl Write>,
    folder: &Path,
    file: &str,
    address: &str,
) -> Result<(), (Reason, String)> {
    std::fs::create_dir_all(folder)
        .map_err(|err| (Reason::Files, format!("{}: {err}", folder.display())))?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .user_agent(concat!(
            "LXB/",
            env!("CARGO_PKG_VERSION"),
            " Heroic companion"
        ))
        .build()
        .into();
    let mut response = agent.get(address).call().map_err(|err| {
        (
            Reason::Offline,
            format!("{file} could not be fetched: {err}"),
        )
    })?;
    let length = response
        .headers()
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    told.size = length;
    let mut reader = response
        .body_mut()
        .with_config()
        .limit(LARGEST_INSTALLER)
        .reader();
    let part = folder.join(format!(".{file}.lxb-part"));
    let mut written = std::fs::File::create(&part)
        .map_err(|err| (Reason::Files, format!("{}: {err}", part.display())))?;
    let mut buffer = vec![0u8; 1 << 16];
    let mut got: u64 = 0;
    let mut last = -1i64;
    let copied: Result<(), (Reason, String)> = loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => break Ok(()),
            Ok(read) => read,
            Err(err) => break Err((Reason::Offline, format!("{file} stopped: {err}"))),
        };
        if let Err(err) = written.write_all(&buffer[..read]) {
            break Err((Reason::Files, format!("{}: {err}", part.display())));
        }
        got += read as u64;
        if let Some(length) = length.filter(|length| *length > 0) {
            let percent = (got * 100 / length).min(99) as i64;
            if percent != last {
                last = percent;
                told.downloaded = Some(got);
                told.say(
                    Step::Downloading,
                    Some(percent as f32 / 100.0),
                    format!("fetching {file}"),
                );
            }
        }
    };
    let flushed = written.sync_all();
    drop(written);
    let kept = copied.and_then(|()| {
        flushed.map_err(|err| (Reason::Files, format!("{}: {err}", part.display())))
    });
    let kept = kept.and_then(|()| {
        std::fs::rename(&part, folder.join(file))
            .map_err(|err| (Reason::Files, format!("{file}: {err}")))
    });
    if kept.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    kept
}

/// Add a game to Heroic's list of the titles another store installs, as
/// Heroic writes it: `[["app", "Windows"], …]`, compact. Once, however many
/// times it is asked.
fn mark_elsewhere(paths: &Paths, app_name: &str) -> Result<(), String> {
    let mut pairs = elsewhere(paths)?;
    if !pairs
        .iter()
        .any(|pair| pair.first().and_then(Value::as_str) == Some(app_name))
    {
        pairs.push(vec![Value::from(app_name), Value::from("Windows")]);
    }
    write_elsewhere(paths, &pairs)
}

/// Take a game off that list. Every entry naming it, and nothing else —
/// Heroic's own removal takes off the *last* entry when the game is not
/// there, which this does not copy.
fn unmark_elsewhere(paths: &Paths, app_name: &str) -> Result<bool, String> {
    let mut pairs = elsewhere(paths)?;
    let before = pairs.len();
    pairs.retain(|pair| pair.first().and_then(Value::as_str) != Some(app_name));
    if pairs.len() == before {
        return Ok(false);
    }
    write_elsewhere(paths, &pairs).map(|()| true)
}

fn elsewhere(paths: &Paths) -> Result<Vec<Vec<Value>>, String> {
    let at = paths.third_party_installed();
    match std::fs::read_to_string(&at) {
        Ok(text) => serde_json::from_str(&text).map_err(|err| format!("{}: {err}", at.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(err) => Err(format!("{}: {err}", at.display())),
    }
}

fn write_elsewhere(paths: &Paths, pairs: &[Vec<Value>]) -> Result<(), String> {
    let at = paths.third_party_installed();
    let text = serde_json::to_string(pairs).map_err(|err| err.to_string())?;
    if let Some(folder) = at.parent() {
        std::fs::create_dir_all(folder).map_err(|err| format!("{}: {err}", folder.display()))?;
    }
    let temp = at.with_extension(format!("lxb-{}", std::process::id()));
    std::fs::write(&temp, text).map_err(|err| format!("{}: {err}", temp.display()))?;
    std::fs::rename(&temp, &at).map_err(|err| {
        let _ = std::fs::remove_file(&temp);
        format!("{}: {err}", at.display())
    })
}

/// Take one game off the disk, or off Heroic's list of what another store
/// installs.
pub fn remove(installation: &Installation, paths: &Paths, app_name: &str) -> Removal {
    let mut answer = Removal {
        protocol: PROTOCOL,
        app_name: app_name.to_string(),
        removed: false,
        reason: None,
        note: String::new(),
    };
    if heroic::in_the_way() {
        answer.reason = Some(Reason::HeroicRunning);
        answer.note = "Heroic is open".into();
        return answer;
    }
    match unmark_elsewhere(paths, app_name) {
        Ok(true) => {
            answer.removed = true;
            answer.note = "taken off Heroic's list of other stores' titles".into();
            return answer;
        }
        Ok(false) => {}
        Err(why) => {
            answer.reason = Some(Reason::Files);
            answer.note = why;
            return answer;
        }
    }
    match is_installed(paths, app_name) {
        Some(false) => {
            answer.removed = true;
            answer.note = "it was not installed".into();
            return answer;
        }
        None => {
            answer.reason = Some(Reason::Files);
            answer.note = "Heroic's list of installed games is unreadable".into();
            return answer;
        }
        Some(true) => {}
    }
    let out = heroic::output(
        heroic::legendary(installation, paths)
            .args(["uninstall", app_name, "-y"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    );
    let said = match out {
        Ok(out) => {
            let mut said = String::from_utf8_lossy(&out.stderr).into_owned();
            said.push_str(&String::from_utf8_lossy(&out.stdout));
            said
        }
        Err(err) => {
            answer.reason = Some(Reason::NoHeroic);
            answer.note = format!("flatpak could not be run: {err}");
            return answer;
        }
    };
    for line in said.lines().filter(|line| !line.trim().is_empty()) {
        eprintln!("legendary: {line}");
    }
    answer.removed = is_installed(paths, app_name) == Some(false);
    if answer.removed {
        answer.note = "uninstalled".into();
        if said.contains("Removing game failed") {
            // Off the list, with files left behind that legendary could not
            // delete. Said in the log, which is where a leftover folder is
            // looked for.
            answer.note = last_words(&said);
        }
    } else {
        answer.reason = Some(if said.contains("Failed to acquire installed data lock") {
            Reason::HeroicRunning
        } else {
            Reason::Legendary
        });
        answer.note = last_words(&said);
    }
    answer
}

/// Whether legendary's `installed.json` lists a game. `None` where the file is
/// there and cannot be read, which is not the same as the game being absent.
fn is_installed(paths: &Paths, app_name: &str) -> Option<bool> {
    let text = match std::fs::read_to_string(paths.installed()) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Some(false),
        Err(_) => return None,
    };
    let listed: BTreeMap<String, Value> = serde_json::from_str(&text).ok()?;
    Some(listed.contains_key(app_name))
}

/// The game, where the account holds it — by Heroic's own rules for what one is.
fn owned(paths: &Paths, app_name: &str) -> Option<crate::report::Game> {
    library::read(paths)
        .into_iter()
        .find(|game| game.app_name == app_name)
}

/// Writes one [`Download`] line per change, carrying the numbers so far.
struct Teller<W: Write> {
    out: W,
    app_name: String,
    downloaded: Option<u64>,
    size: Option<u64>,
}

impl<W: Write> Teller<W> {
    fn new(out: W, app_name: &str) -> Teller<W> {
        Teller {
            out,
            app_name: app_name.to_string(),
            downloaded: None,
            size: None,
        }
    }

    fn say(&mut self, step: Step, progress: Option<f32>, note: String) {
        let line = Download {
            protocol: PROTOCOL,
            app_name: self.app_name.clone(),
            step,
            progress,
            downloaded: self.downloaded,
            size: self.size,
            reason: None,
            note,
        };
        self.write(&line);
    }

    fn failed(&mut self, reason: Reason, why: String) -> bool {
        eprintln!("get: {why} ({reason:?})");
        let line = Download {
            protocol: PROTOCOL,
            app_name: self.app_name.clone(),
            step: Step::Failed,
            progress: None,
            downloaded: self.downloaded,
            size: self.size,
            reason: Some(reason),
            note: why,
        };
        self.write(&line);
        false
    }

    fn write(&mut self, line: &Download) {
        if let Ok(json) = serde_json::to_string(line) {
            let _ = writeln!(self.out, "{json}");
            let _ = self.out.flush();
        }
    }
}

/// Where the installer for an EA or Ubisoft title would be — for a test.
#[cfg(test)]
fn installer_for(paths: &Paths, store: Store) -> std::path::PathBuf {
    let (file, _) = match store {
        Store::Ubisoft => UBISOFT_INSTALLER,
        _ => EA_INSTALLER,
    };
    paths.redist().join(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A repair's check is counted from legendary's own line, which it
    /// rewrites in place — and nothing else is taken for one.
    #[test]
    fn a_repairs_check_is_counted_from_legendarys_line() {
        assert_eq!(
            checked("Verification progress: 385/770 (50.0%) [412.3 MiB/s]"),
            Some(0.5)
        );
        assert_eq!(
            checked("Verification progress: 770/770 (100.0%) [0.0 MiB/s]"),
            Some(1.0)
        );
        assert_eq!(checked("Verification progress: 0/0 (0.0%)"), None);
        assert_eq!(
            checked("[cli] INFO: Verifying \"Cat Quest\" version \"1.4.1\""),
            None
        );
    }

    /// legendary's own lines, from the install of Cat Quest measured on
    /// 2026-09-24, in the order it wrote them.
    const CAT_QUEST: &str = "\
[cli] INFO: Install size: 360.42 MiB
[cli] INFO: Download size: 130.81 MiB (Compression savings: 62.2%)
[DLManager] INFO: = Progress: 0.00% (0/1132), Running for 00:00:00, ETA: 00:00:00
[DLManager] INFO:  - Downloaded: 0.00 MiB, Written: 0.00 MiB
[DLManager] INFO: = Progress: 74.73% (846/1132), Running for 00:00:01, ETA: 00:00:00
[DLManager] INFO:  - Downloaded: 68.69 MiB, Written: 240.00 MiB
[DLManager] INFO:  + Download\t- 68.67 MiB/s (raw) / 251.91 MiB/s (decompressed)
[DLManager] INFO: = Progress: 100.00% (1132/1132), Running for 00:00:02, ETA: 00:00:00
[DLManager] INFO:  - Downloaded: 130.81 MiB, Written: 360.42 MiB";

    fn shares(whole: Option<u64>, lines: &str) -> Vec<(Option<f32>, Option<u64>)> {
        let mut watch = Watch::new(whole);
        let mut seen = Vec::new();
        for line in lines.lines() {
            if watch.read(line) {
                seen.push((watch.share(), watch.downloaded()));
            }
        }
        seen
    }

    #[test]
    fn legendarys_lines_become_a_bar_that_stops_short_of_done() {
        let seen = shares(Some(137_167_567), CAT_QUEST);
        assert_eq!(seen.len(), 3, "{seen:?}");
        assert_eq!(seen[0], (Some(0.0), Some(0)));
        let middle = seen[1].0.unwrap();
        assert!((0.52..0.53).contains(&middle), "{middle}");
        // All of it down is still not done: that is Heroic's list's to say.
        assert_eq!(seen[2].0, Some(0.99));
        assert_eq!(seen[2].1, Some(137_164_227));
        assert!(
            CAT_QUEST
                .lines()
                .filter(|line| is_progress_detail(line))
                .count()
                == 7
        );
        assert!(!is_progress_detail(
            "[cli] INFO: Download size: 130.81 MiB (Compression savings: 62.2%)"
        ));
    }

    /// A download stopped halfway and asked for again: legendary says only
    /// what is left, and the bar starts where the last one got to.
    #[test]
    fn a_download_picked_up_again_starts_its_bar_where_it_was() {
        let resumed = "\
[cli] INFO: Download size: 65.00 MiB (Compression savings: 60.0%)
[DLManager] INFO: = Progress: 0.00% (0/560), Running for 00:00:00, ETA: 00:00:00
[DLManager] INFO:  - Downloaded: 0.00 MiB, Written: 0.00 MiB";
        let seen = shares(Some(130 * 1024 * 1024), resumed);
        assert_eq!(seen[0].0, Some(0.5));
        // Without Epic's size, what is left is taken for the whole.
        let seen = shares(None, resumed);
        assert_eq!(seen[0].0, Some(0.0));
        // And without either, legendary's own percentage.
        let mut watch = Watch::new(None);
        watch.read(
            "[DLManager] INFO: = Progress: 42.00% (1/2), Running for 00:00:01, ETA: 00:00:01",
        );
        assert_eq!(watch.share(), Some(0.42));
    }

    /// What legendary said, word for word, when it was asked again for a game
    /// whose download had been stopped after its last file was written.
    #[test]
    fn a_download_that_had_all_arrived_is_recognised_and_found() {
        let said = "\
[Core] INFO: Install path: /home/petexy/Games/Heroic/CatQuest
[DLM] INFO: Found previously interrupted download. Download will be resumed if possible.
[DLM] INFO: Skipping 770 files based on resume data.
[cli] INFO: Download size is 0, the game is either already up to date or has not changed. Exiting...
";
        assert!(all_of_it_arrived(said));
        assert_eq!(
            install_path_said(said).as_deref(),
            Some("/home/petexy/Games/Heroic/CatQuest")
        );
        assert!(!all_of_it_arrived(CAT_QUEST));
        assert_eq!(install_path_said("Install path: relative"), None);
    }

    #[test]
    fn why_legendary_would_not_install_is_one_word() {
        assert_eq!(
            why_it_did_not_install(
                " ! Failure: Not enough available disk space! 0.10 GiB < 0.35 GiB\n"
            ),
            Reason::NoSpace
        );
        assert_eq!(
            why_it_did_not_install("[cli] CRITICAL: Failed to acquire installed data lock, only one instance of Legendary may install/import/move applications at a time.\n"),
            Reason::HeroicRunning
        );
        assert_eq!(
            why_it_did_not_install("requests.exceptions.ConnectionError: Max retries exceeded\n"),
            Reason::Offline
        );
        assert_eq!(
            why_it_did_not_install("something else\n"),
            Reason::Legendary
        );
        assert_eq!(last_words("one\ntwo\n\n"), "two");
    }

    /// Heroic's list of other stores' titles, written the way Heroic writes it.
    #[test]
    fn other_stores_titles_are_marked_once_and_unmarked_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::at(dir.path().to_path_buf());
        mark_elsewhere(&paths, "Wren").unwrap();
        mark_elsewhere(&paths, "Wren").unwrap();
        mark_elsewhere(&paths, "Owl").unwrap();
        let text = std::fs::read_to_string(paths.third_party_installed()).unwrap();
        assert_eq!(text, r#"[["Wren","Windows"],["Owl","Windows"]]"#);
        assert_eq!(unmark_elsewhere(&paths, "Nobody"), Ok(false));
        let text = std::fs::read_to_string(paths.third_party_installed()).unwrap();
        assert_eq!(text, r#"[["Wren","Windows"],["Owl","Windows"]]"#);
        assert_eq!(unmark_elsewhere(&paths, "Wren"), Ok(true));
        let text = std::fs::read_to_string(paths.third_party_installed()).unwrap();
        assert_eq!(text, r#"[["Owl","Windows"]]"#);
        assert!(installer_for(&paths, Store::Ubisoft)
            .ends_with("tools/redist/legendary/UbisoftConnectInstaller.exe"));
    }

    #[test]
    fn installed_is_read_off_legendarys_own_list() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::at(dir.path().to_path_buf());
        assert_eq!(is_installed(&paths, "Quail"), Some(false));
        std::fs::create_dir_all(paths.legendary()).unwrap();
        std::fs::write(paths.installed(), r#"{"Quail": {"title": "20XX"}}"#).unwrap();
        assert_eq!(is_installed(&paths, "Quail"), Some(true));
        assert_eq!(is_installed(&paths, "Wren"), Some(false));
        std::fs::write(paths.installed(), "{").unwrap();
        assert_eq!(is_installed(&paths, "Quail"), None);
    }
}
