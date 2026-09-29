//! Getting a machine from nothing to a PlayStation 3 that boots games: RPCS3
//! where there is none, then the console's own system software where RPCS3 has
//! none. The user asked for both to be done for them, as RetroArch's cores
//! are.
//!
//! **RPCS3 goes into this user's own flatpaks**, for the reason the RetroArch
//! helper gives: a `--user` install needs nobody's password, so the shell can
//! offer it as a plain Yes. An RPCS3 already here — a distribution package, a
//! flatpak, an AppImage — is used as it is.
//!
//! **The system software comes from Sony**, from the update server a real PS3
//! asks, and is installed by RPCS3's own installer (`--headless --installfw`),
//! which checks the file's own digests before it writes anything. Sony serves
//! it over plain HTTP and nowhere else, which is why that check matters.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use crate::report::{Installation, Kind, Permission, Progress, Stage, Trouble, PROTOCOL};
use crate::rpcs3::{self, Console, FLATPAK_ID};

const FLATHUB: &str = "https://dl.flathub.org/repo/flathub.flatpakrepo";

/// Where Sony lists the current system software. The list names the file for
/// this region's servers; the file itself is the same everywhere.
const UPDATE_LIST: &str =
    "http://fus01.ps3.update.playstation.net/update/ps3/list/us/ps3-updatelist.txt";

/// Do whatever of the two is not done yet. Returns whether both are done.
pub fn run(out: &mut impl Write) -> bool {
    let installation = match rpcs3::installation() {
        Some(installation) => installation,
        None => {
            if !rpcs3::flatpak_available() {
                tell(
                    out,
                    Stage::Failed,
                    None,
                    Some(Trouble::NoFlatpak),
                    "no flatpak",
                );
                return false;
            }
            if !install_flatpak(out) {
                return false;
            }
            match rpcs3::installation() {
                Some(installation) => installation,
                None => {
                    tell(
                        out,
                        Stage::Failed,
                        None,
                        Some(Trouble::Refused),
                        "flatpak said it installed RPCS3, and there is none",
                    );
                    return false;
                }
            }
        }
    };
    let Some(config) = rpcs3::config_dir(&installation) else {
        tell(
            out,
            Stage::Failed,
            None,
            Some(Trouble::Other),
            "no home directory",
        );
        return false;
    };
    let console = Console::of(&config);
    if let Some(version) = console.firmware() {
        tell(
            out,
            Stage::Done,
            Some(1.0),
            None,
            &format!("system software {version}"),
        );
        return true;
    }
    firmware(out, &installation, &console)
}

/// RPCS3 from Flathub, into this user's own installation.
fn install_flatpak(out: &mut impl Write) -> bool {
    tell(
        out,
        Stage::Remote,
        None,
        None,
        "adding Flathub for this user",
    );
    let remote = Command::new("flatpak")
        .args([
            "remote-add",
            "--user",
            "--if-not-exists",
            "flathub",
            FLATHUB,
        ])
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output();
    match remote {
        Ok(result) if result.status.success() => {}
        Ok(result) => {
            let said = said(&result.stderr).unwrap_or_default();
            tell(out, Stage::Failed, None, Some(Trouble::Refused), &said);
            return false;
        }
        Err(err) => {
            tell(
                out,
                Stage::Failed,
                None,
                Some(Trouble::NoFlatpak),
                &err.to_string(),
            );
            return false;
        }
    }

    tell(out, Stage::Installing, None, None, "installing RPCS3");
    let child = Command::new("flatpak")
        .args([
            "install",
            "--user",
            "-y",
            "--noninteractive",
            "flathub",
            FLATPAK_ID,
        ])
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(err) => {
            tell(
                out,
                Stage::Failed,
                None,
                Some(Trouble::NoFlatpak),
                &err.to_string(),
            );
            return false;
        }
    };
    let complaint = child.stderr.take().map(|stderr| {
        std::thread::spawn(move || {
            let mut kept: Vec<String> = Vec::new();
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                eprintln!("flatpak: {line}");
                if !line.trim().is_empty() {
                    kept.push(line.trim().to_string());
                    if kept.len() > 8 {
                        kept.remove(0);
                    }
                }
            }
            kept
        })
    });
    if let Some(stdout) = child.stdout.take() {
        let mut job = (1u32, 1u32);
        let mut last = None;
        for chunk in chunks(stdout) {
            if let Some(said) = operation(&chunk) {
                job = said;
            }
            if let Some(percent) = percentage(&chunk) {
                let done = whole(job.0, job.1, percent);
                if last != Some(done) {
                    last = Some(done);
                    tell(
                        out,
                        Stage::Installing,
                        Some(f32::from(done) / 100.0),
                        None,
                        "installing RPCS3",
                    );
                }
            }
        }
    }
    let status = child.wait();
    let kept = complaint
        .and_then(|thread| thread.join().ok())
        .unwrap_or_default();
    if status.is_ok_and(|status| status.success()) {
        return true;
    }
    let said = kept.last().cloned().unwrap_or_default();
    let trouble = if said.contains("No space") {
        Trouble::NoSpace
    } else if said.contains("resolve") || said.contains("connect") || said.contains("Could not") {
        Trouble::Offline
    } else {
        Trouble::Refused
    };
    tell(out, Stage::Failed, None, Some(trouble), &said);
    false
}

/// The system software: Sony's list, the file it names, and RPCS3 installing
/// it.
fn firmware(out: &mut impl Write, installation: &Installation, console: &Console) -> bool {
    tell(
        out,
        Stage::Downloading,
        None,
        None,
        "asking Sony for the system software",
    );
    let url = match current_firmware() {
        Ok(url) => url,
        Err((trouble, note)) => {
            tell(out, Stage::Failed, None, Some(trouble), &note);
            return false;
        }
    };
    let Some(scratch) = rpcs3::scratch_dir(installation) else {
        tell(
            out,
            Stage::Failed,
            None,
            Some(Trouble::Other),
            "nowhere to download to",
        );
        return false;
    };
    let pup = scratch.join("PS3UPDAT.PUP");
    let downloaded = crate::net::download(&url, &pup, |fraction| {
        tell(
            out,
            Stage::Downloading,
            fraction,
            None,
            "downloading the system software",
        );
    });
    if let Err((trouble, note)) = downloaded {
        tell(out, Stage::Failed, None, Some(trouble), &note);
        return false;
    }
    if !is_pup(&pup) {
        let _ = std::fs::remove_file(&pup);
        tell(
            out,
            Stage::Failed,
            None,
            Some(Trouble::Other),
            "Sony's server sent something else",
        );
        return false;
    }

    tell(
        out,
        Stage::Firmware,
        None,
        None,
        "RPCS3 is installing the system software",
    );
    let result = rpcs3_run(installation, &["--headless", "--installfw"], &pup);
    let _ = std::fs::remove_file(&pup);
    if let Err(err) = result {
        tell(
            out,
            Stage::Failed,
            None,
            Some(Trouble::FirmwareRejected),
            &err,
        );
        return false;
    }
    // What RPCS3 did is read off the disk, never off how it ended: its
    // headless installer answers 0 whatever happened, and the flatpak build
    // of 2026-09 installs the whole of the system software and then aborts
    // while tearing itself down (`Verification failed` in `fixed_typemap.hpp`,
    // exit status 134) — which read as a failure the first time this ran, over
    // a console that was ready. See [`rpcs3_run`].
    match console.firmware() {
        Some(version) => {
            tell(
                out,
                Stage::Done,
                Some(1.0),
                None,
                &format!("system software {version}"),
            );
            true
        }
        None => {
            tell(
                out,
                Stage::Failed,
                None,
                Some(Trouble::FirmwareRejected),
                "RPCS3 did not install the system software",
            );
            false
        }
    }
}

/// The address of the current `PS3UPDAT.PUP`, out of Sony's list.
fn current_firmware() -> Result<String, (Trouble, String)> {
    let list = crate::net::agent()
        .get(UPDATE_LIST)
        .call()
        .map_err(|err| (crate::net::trouble(&err), err.to_string()))?
        .body_mut()
        .read_to_string()
        .map_err(|err| (Trouble::Offline, err.to_string()))?;
    full_image(&list).ok_or_else(|| {
        (
            Trouble::Other,
            "Sony's list names no system software".to_string(),
        )
    })
}

/// The whole image from a list — `PS3UPDAT.PUP`, not the `PS3PATCH.PUP` that
/// only updates a console already one version behind.
fn full_image(list: &str) -> Option<String> {
    list.lines().find_map(|line| {
        line.split(';').find_map(|field| {
            let url = field.strip_prefix("CDN=")?;
            url.ends_with("/PS3UPDAT.PUP").then(|| url.to_string())
        })
    })
}

/// Whether a file is a PUP at all: `SCEUF` and its version.
fn is_pup(at: &Path) -> bool {
    let mut head = [0u8; 8];
    std::fs::File::open(at)
        .and_then(|mut file| file.read_exact(&mut head))
        .is_ok()
        && head[..5] == *b"SCEUF"
}

/// Run RPCS3 with these arguments and a file after them, and wait for it.
///
/// `Err` only where RPCS3 could not be started at all: how it ended is logged
/// and otherwise not believed, for the reason [`firmware`] gives.
pub fn rpcs3_run(installation: &Installation, args: &[&str], file: &Path) -> Result<(), String> {
    let (program, rest) = installation
        .command
        .split_first()
        .ok_or_else(|| "no command".to_string())?;
    let result = Command::new(program)
        .args(rest)
        .args(args)
        .arg(file)
        // Nothing of RPCS3's is asked a question on a terminal, and its own
        // chatter goes to the session log.
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .env("QT_QPA_PLATFORM", "offscreen")
        .status()
        .map_err(|err| format!("RPCS3 could not be started: {err}"))?;
    if !result.success() {
        eprintln!("setup: RPCS3 ended with {result}; what it did is read off the disk");
    }
    Ok(())
}

/// Let a flatpak RPCS3 read a folder outside what it can already see.
///
/// The Flathub build reads the home folder, `/media` and `/run/media` on its
/// own, so a PS3 folder on a USB drive needs nothing; a folder anywhere else
/// is granted, read only — RPCS3 reads games and writes its saves under its own
/// directory.
pub fn permit(at: &Path) -> Permission {
    let answer = |permitted: bool, note: &str| Permission {
        protocol: PROTOCOL,
        permitted,
        note: note.to_string(),
    };
    let Some(installation) = rpcs3::installation() else {
        return answer(false, "there is no RPCS3");
    };
    let scope = match installation.kind {
        Kind::FlatpakUser => "--user",
        Kind::FlatpakSystem => return answer(true, "a system flatpak keeps what it was given"),
        Kind::System | Kind::AppImage => return answer(true, "not in a sandbox"),
    };
    let home = rpcs3::home();
    let seen = at.starts_with("/media")
        || at.starts_with("/run/media")
        || home.as_ref().is_some_and(|home| at.starts_with(home));
    if seen {
        return answer(true, "already readable");
    }
    let result = Command::new("flatpak")
        .args([
            "override",
            scope,
            &format!("--filesystem={}:ro", at.display()),
            FLATPAK_ID,
        ])
        .stdin(Stdio::null())
        .output();
    match result {
        Ok(result) if result.status.success() => answer(true, "granted"),
        Ok(result) => answer(false, &said(&result.stderr).unwrap_or_default()),
        Err(err) => answer(false, &err.to_string()),
    }
}

// flatpak's progress, as the RetroArch helper reads it — see
// `lxb-retroarch/src/install.rs`, where each of these is explained.

fn operation(line: &str) -> Option<(u32, u32)> {
    let rest = ["Installing", "Updating"]
        .iter()
        .find_map(|word| line.split_once(word).map(|(_, rest)| rest.trim_start()))?;
    let counted: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '/')
        .collect();
    let (at, of) = counted.split_once('/')?;
    let (at, of) = (at.parse::<u32>().ok()?, of.parse::<u32>().ok()?);
    (of > 0 && at >= 1 && at <= of).then_some((at, of))
}

fn whole(at: u32, of: u32, percent: u8) -> u8 {
    let of = of.max(1);
    let done = (at.saturating_sub(1) as f32 + f32::from(percent) / 100.0) / of as f32;
    (done * 100.0).round().clamp(0.0, 100.0) as u8
}

fn chunks(stream: impl Read) -> impl Iterator<Item = String> {
    let mut reader = BufReader::new(stream);
    std::iter::from_fn(move || {
        let mut raw = Vec::new();
        loop {
            let mut byte = [0u8; 1];
            match reader.read(&mut byte) {
                Ok(0) => break,
                Ok(_) => {
                    if byte[0] == b'\r' || byte[0] == b'\n' {
                        if raw.is_empty() {
                            continue;
                        }
                        break;
                    }
                    raw.push(byte[0]);
                }
                Err(_) => break,
            }
        }
        (!raw.is_empty()).then(|| String::from_utf8_lossy(&raw).into_owned())
    })
}

fn percentage(line: &str) -> Option<u8> {
    let chars: Vec<char> = line.chars().collect();
    let mut found = None;
    for (at, c) in chars.iter().enumerate() {
        if *c != '%' {
            continue;
        }
        let mut start = at;
        while start > 0 && chars[start - 1].is_ascii_digit() {
            start -= 1;
        }
        if start == at {
            continue;
        }
        let number: String = chars[start..at].iter().collect();
        if let Ok(percent) = number.parse::<u32>() {
            found = Some(percent.min(100) as u8);
        }
    }
    found
}

pub fn said(stderr: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(stderr);
    let line = text.lines().map(str::trim).rfind(|line| !line.is_empty())?;
    Some(line.to_string())
}

pub fn tell(
    out: &mut impl Write,
    stage: Stage,
    progress: Option<f32>,
    trouble: Option<Trouble>,
    note: &str,
) {
    if matches!(stage, Stage::Failed) {
        eprintln!("setup: failed: {note}");
    }
    let line = Progress {
        protocol: PROTOCOL,
        stage,
        progress,
        trouble,
        note: note.to_string(),
    };
    if let Ok(json) = serde_json::to_string(&line) {
        let _ = writeln!(out, "{json}");
        let _ = out.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sony's list as it stood on 2026-09-29.
    #[test]
    fn the_whole_image_is_taken_out_of_sonys_list() {
        let list = "# US\n\
            Dest=84;CompatibleSystemSoftwareVersion=4.9300-;\n\
            Dest=84;IncrementalUpdateVersion=00010b72-00010b72;ImageVersion=00010b94;SystemSoftwareVersion=4.9300;CDN=http://dus01.ps3.update.playstation.net/update/ps3/image/us/2026_0318_a2b60b6ac1d2e49e230144345616927c/PS3PATCH.PUP;CDN_Timeout=30;\n\
            Dest=84;ImageVersion=00010b94;SystemSoftwareVersion=4.9300;CDN=http://dus01.ps3.update.playstation.net/update/ps3/image/us/2026_0318_a2b60b6ac1d2e49e230144345616927c/PS3UPDAT.PUP;CDN_Timeout=30;\n";
        assert_eq!(
            full_image(list).as_deref(),
            Some("http://dus01.ps3.update.playstation.net/update/ps3/image/us/2026_0318_a2b60b6ac1d2e49e230144345616927c/PS3UPDAT.PUP")
        );
        assert_eq!(full_image("# US\n"), None);
    }

    #[test]
    fn flatpaks_jobs_are_one_bar() {
        assert_eq!(operation("Installing 2/4…  45%"), Some((2, 4)));
        assert_eq!(percentage("Installing… ████░░░░  45%"), Some(45));
        assert_eq!(whole(1, 4, 100), 25);
        let seen: Vec<String> = chunks("a\r 10%\r100%\n".as_bytes()).collect();
        assert_eq!(seen, vec!["a", " 10%", "100%"]);
    }
}
