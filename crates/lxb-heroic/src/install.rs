//! Making Heroic ready to start a game: Heroic itself, then its Proton.
//!
//! **Heroic, into this user's own flatpak installation**, for the reasons the
//! RetroArch helper gives in its own `install.rs`: a `--user` install needs no
//! password and no polkit, which is what lets the shell offer it behind a plain
//! Yes. A Heroic that is already on the machine — the user's or the system's —
//! is used as it is and never installed a second time beside itself.
//!
//! **Then its Proton**, which is `proton.rs`: without one, Heroic's first
//! launch of a game stops to ask a question nobody can see.
//!
//! One [`Progress`] line per change on stdout, ending on `Done` or `Failed`.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use crate::heroic::{self, Paths, FLATPAK_ID};
use crate::proton;
use crate::report::{Progress, Reason, Stage, PROTOCOL};

/// Where Flathub is, for a user who has not got it as a remote of their own.
const FLATHUB: &str = "https://dl.flathub.org/repo/flathub.flatpakrepo";

/// Make Heroic ready. Whether it ended ready.
pub fn run(out: &mut impl Write, paths: &Paths, home: &Path) -> bool {
    match heroic::installation() {
        Some(found) => eprintln!("install: Heroic is already here ({:?})", found.kind),
        None => {
            if !heroic::flatpak_available() {
                return failed(
                    out,
                    Reason::NoFlatpak,
                    "flatpak is not on this machine".into(),
                );
            }
            tell(out, Stage::Remote, None, "Getting ready".into());
            if let Err(why) = remote() {
                return failed(out, Reason::FlatpakRefused, why);
            }
            tell(
                out,
                Stage::Installing,
                Some(0.0),
                "Downloading Heroic".into(),
            );
            if let Err(why) = install(out) {
                return failed(out, Reason::FlatpakRefused, why);
            }
            if heroic::installation().is_none() {
                return failed(
                    out,
                    Reason::FlatpakRefused,
                    "flatpak finished but Heroic is not there".into(),
                );
            }
        }
    }

    tell(out, Stage::Proton, None, "Getting Proton".into());
    let settled = {
        let mut told = |fraction: f32| {
            tell(
                out,
                Stage::Proton,
                Some(fraction),
                "Downloading Proton".into(),
            );
        };
        proton::settle(paths, home, &mut told)
    };
    match settled {
        Ok(name) => {
            tell(
                out,
                Stage::Done,
                Some(1.0),
                format!("Heroic is ready, with {name}"),
            );
            true
        }
        Err((reason, why)) => failed(out, reason, why),
    }
}

/// Add Flathub to this user's own remotes, which costs nothing when it is
/// there already.
fn remote() -> Result<(), String> {
    let out = Command::new("flatpak")
        .args([
            "remote-add",
            "--user",
            "--if-not-exists",
            "flathub",
            FLATHUB,
        ])
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output()
        .map_err(|err| format!("flatpak could not be run: {err}"))?;
    if out.status.success() {
        return Ok(());
    }
    Err(said(&out.stderr).unwrap_or_else(|| "Flathub could not be added".to_string()))
}

/// The install itself, with the percentage forwarded as it changes.
fn install(out: &mut impl Write) -> Result<(), String> {
    let mut child = Command::new("flatpak")
        .args(["install", "--user", "-y", "flathub", FLATPAK_ID])
        // One language, because the word in front of flatpak's job counter is
        // translated and `operation` reads it.
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("flatpak could not be run: {err}"))?;
    let complaint = child.stderr.take().map(|stderr| {
        std::thread::spawn(move || {
            let mut kept: Vec<String> = Vec::new();
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                eprintln!("flatpak: {line}");
                let line = line.trim().to_string();
                if !line.is_empty() {
                    kept.push(line);
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
                        format!("job {} of {}", job.0, job.1),
                    );
                }
            }
        }
    }
    let status = child
        .wait()
        .map_err(|err| format!("flatpak could not be waited for: {err}"))?;
    let kept = complaint
        .and_then(|thread| thread.join().ok())
        .unwrap_or_default();
    if status.success() {
        return Ok(());
    }
    Err(kept
        .last()
        .cloned()
        .unwrap_or_else(|| "Heroic could not be downloaded".to_string()))
}

/// Which of flatpak's jobs a line is about, and how many there are:
/// `Installing 2/4… ███░░░  45%`. Read in English — see [`install`].
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

/// How far along the whole install is, from which job it is on and how far
/// along that one is. Not the true fraction of the bytes — the jobs differ in
/// size — but it only ever goes forwards, which is what a bar has to do.
fn whole(at: u32, of: u32, percent: u8) -> u8 {
    let of = of.max(1);
    let done = (at.saturating_sub(1) as f32 + f32::from(percent) / 100.0) / of as f32;
    (done * 100.0).round().clamp(0.0, 100.0) as u8
}

/// flatpak's progress bar, split where it actually ends: it is redrawn with
/// carriage returns, so `lines()` would see nothing until the install is over.
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

/// The last percentage written in a line of flatpak's, if it wrote one.
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

fn said(stderr: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(stderr);
    let line = text.lines().map(str::trim).rfind(|line| !line.is_empty())?;
    Some(line.to_string())
}

fn failed(out: &mut impl Write, reason: Reason, why: String) -> bool {
    eprintln!("install: {why} ({reason:?})");
    let line = Progress {
        protocol: PROTOCOL,
        stage: Stage::Failed,
        progress: None,
        reason: Some(reason),
        note: why,
    };
    write(out, &line);
    false
}

fn tell(out: &mut impl Write, stage: Stage, progress: Option<f32>, note: String) {
    let line = Progress {
        protocol: PROTOCOL,
        stage,
        progress,
        reason: None,
        note,
    };
    write(out, &line);
}

fn write(out: &mut impl Write, line: &Progress) {
    if let Ok(json) = serde_json::to_string(line) {
        let _ = writeln!(out, "{json}");
        let _ = out.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// flatpak's own lines, as it draws them under `LC_ALL=C`: a bar that
    /// fills once per job has to become one bar that fills once.
    #[test]
    fn flatpaks_bar_becomes_one_that_only_goes_forwards() {
        assert_eq!(operation("Installing 2/4… ███░░░  45%"), Some((2, 4)));
        assert_eq!(operation("Updating 1/1… ██████ 100%"), Some((1, 1)));
        assert_eq!(operation("Installing 5/4…"), None);
        assert_eq!(percentage("Installing 2/4… ███░░░  45%"), Some(45));
        assert_eq!(percentage("no number here %"), None);
        assert_eq!(whole(1, 4, 100), 25);
        assert_eq!(whole(2, 4, 50), 38);
        assert_eq!(whole(4, 4, 100), 100);
        let redrawn: Vec<String> = chunks(
            &b"Installing 1/2\xe2\x80\xa6  10%\rInstalling 1/2\xe2\x80\xa6  90%\r\nDone\n"[..],
        )
        .collect();
        assert_eq!(redrawn.len(), 3, "{redrawn:?}");
    }
}
