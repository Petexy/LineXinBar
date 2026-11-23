//! Making Heroic ready to start a game: Heroic itself, then its Proton.
//!
//! Install Heroic per user with Flatpak or the official Linux archive.
//! Reuse an existing installation when the user only needs to set up Proton.
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
pub fn run(out: &mut impl Write, home: &Path, native: bool) -> bool {
    match heroic::installation() {
        Some(found) => eprintln!("install: Heroic is already here ({:?})", found.kind),
        None if native => {
            if let Err((reason, why)) = install_native(out) {
                return failed(out, reason, why);
            }
        }
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

    let Some(installation) = heroic::installation() else {
        return failed(out, Reason::NoHeroic, "Heroic could not be located after installation".into());
    };
    let paths = Paths::at(installation.config.into());
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
        proton::settle(&paths, home, &mut told)
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
/// Install Heroic's official Linux archive in this user's application data directory.
fn install_native(out: &mut impl Write) -> Result<(), (Reason, String)> {
    use sha2::{Digest, Sha256};

    const RELEASE: &str = "https://api.github.com/repos/Heroic-Games-Launcher/HeroicGamesLauncher/releases/latest";
    const ASSETS: &str = "https://github.com/Heroic-Games-Launcher/HeroicGamesLauncher/releases/download/";
    #[derive(serde::Deserialize)]
    struct Release {
        assets: Vec<Asset>,
    }
    #[derive(serde::Deserialize)]
    struct Asset {
        name: String,
        browser_download_url: String,
        size: u64,
        digest: Option<String>,
    }

    let files = |err: std::io::Error| (Reason::Files, err.to_string());
    if !heroic::native_available() {
        return Err((Reason::Files, "the native archive requires x86-64, tar, xz and curl".into()));
    }
    tell(out, Stage::Remote, None, "Looking up the official Heroic release".into());
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .user_agent(concat!("LineXinBar/", env!("CARGO_PKG_VERSION")))
        .timeout_connect(Some(std::time::Duration::from_secs(20)))
        .timeout_global(Some(std::time::Duration::from_secs(1800)))
        .build()
        .into();
    let offline = |err: ureq::Error| (Reason::Offline, err.to_string());
    let raw = agent.get(RELEASE).call().map_err(offline)?
        .body_mut().read_to_string().map_err(offline)?;
    let release: Release = serde_json::from_str(&raw).map_err(|err| (Reason::Files, err.to_string()))?;
    let asset = release.assets.into_iter().find(|asset| {
        asset.name.starts_with("Heroic-") && asset.name.ends_with("-linux-x64.tar.xz")
            && asset.browser_download_url.strip_prefix(ASSETS).is_some_and(|path| {
                path.split_once('/').is_some_and(|(_, name)| name == asset.name)
            })
    }).ok_or_else(|| (Reason::Files, "no official x86-64 Heroic archive in the release".into()))?;
    let digest = asset.digest.as_deref().and_then(|digest| digest.strip_prefix("sha256:"))
        .filter(|digest| digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| (Reason::Files, "the Heroic archive has no SHA-256 digest".into()))?;
    if asset.size == 0 {
        return Err((Reason::Files, "the Heroic archive is empty".into()));
    }
    let destination = heroic::native_directory()
        .ok_or_else(|| (Reason::Files, "no application data directory".into()))?;
    let parent = destination.parent().expect("the native Heroic directory has a parent");
    std::fs::create_dir_all(parent).map_err(files)?;
    let staging = parent.join(format!(".heroic-install-{}", std::process::id()));
    std::fs::create_dir(&staging).map_err(files)?;
    let result = (|| {
        let archive = staging.join("heroic.tar.xz");
        let mut file = std::fs::File::create(&archive).map_err(files)?;
        let mut response = agent.get(&asset.browser_download_url).call().map_err(offline)?;
        let mut body = response.body_mut().with_config().limit(asset.size.saturating_add(1)).reader();
        let mut checksum = Sha256::new();
        let mut received = 0u64;
        let mut percent = 0;
        let mut buffer = [0u8; 64 * 1024];
        tell(out, Stage::Installing, Some(0.0), "Downloading Heroic".into());
        loop {
            let read = body.read(&mut buffer).map_err(files)?;
            if read == 0 {
                break;
            }
            file.write_all(&buffer[..read]).map_err(files)?;
            checksum.update(&buffer[..read]);
            received += read as u64;
            let current = received.saturating_mul(100) / asset.size;
            if current != percent {
                percent = current;
                tell(out, Stage::Installing, Some(percent.min(100) as f32 / 100.0), "Downloading Heroic".into());
            }
        }
        file.sync_all().map_err(files)?;
        drop(file);
        if received != asset.size || !format!("{:x}", checksum.finalize()).eq_ignore_ascii_case(digest) {
            return Err((Reason::Files, "the Heroic archive does not match the official release".into()));
        }

        tell(out, Stage::Installing, None, "Unpacking Heroic".into());
        let unpacked = staging.join("unpacked");
        std::fs::create_dir(&unpacked).map_err(files)?;
        let result = Command::new("tar")
            .args(["--extract", "--xz", "--no-same-owner", "--no-same-permissions", "--file"])
            .arg(&archive).arg("--directory").arg(&unpacked)
            .stdin(Stdio::null()).output().map_err(files)?;
        if !result.status.success() {
            return Err((Reason::Files, said(&result.stderr).unwrap_or_else(|| "could not unpack Heroic".into())));
        }
        let valid = |path: &Path| path.join("heroic").is_file() && heroic::native_legendary(path).is_file();
        let root = if valid(&unpacked) {
            unpacked
        } else {
            std::fs::read_dir(&unpacked).map_err(files)?
                .filter_map(Result::ok)
                .find(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()) && valid(&entry.path()))
                .map(|entry| entry.path())
                .ok_or_else(|| (Reason::Files, "the archive contains no usable Heroic and Legendary binaries".into()))?
        };
        std::fs::rename(root, &destination).map_err(files)?;
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(staging);
    result
}

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

/// Heroic has gone; take what this integration kept for it.
///
/// The other end of [`run`], and the half of a removal that is nobody else's to
/// do. **Removing Heroic itself is the shell's job**, because only the shell
/// can ask for a password: it knows which flatpak installation holds this one
/// and hands it back to flatpak — this user's own with nobody's permission, a
/// system-wide one through polkit. What is left over is everything *outside*
/// that application, which `flatpak uninstall --delete-data` does not reach,
/// and that is this.
///
/// What goes, always:
///
/// * The filesystem permission the shell granted so Heroic could install games
///   where somebody asked for them — `flatpak override --user --reset`. It
///   outlives the application, and a reinstall would silently inherit it.
/// * The shell's own cache of the covers and backdrops fetched from Epic, which
///   lives under the *shell's* cache rather than Heroic's.
///
/// And with `data`, which is the shell's **Remove everything**: Heroic's own
/// configuration directory outside any sandbox, `~/.config/heroic`. The
/// flatpak's own went with it, because the shell passed `--delete-data`; this
/// is for a machine that has also had a Heroic installed some other way, and
/// the button that was pressed asked for everything.
///
/// **Games installed through Heroic are not touched.** They are kept where
/// Heroic was told to keep them — `~/Games/Heroic` unless somebody chose
/// somewhere else — which is outside all of this, and they are the user's.
///
/// Every failure is reported and none of them stops the rest: Heroic is already
/// gone by the time this runs.
pub fn forget(out: &mut impl Write, data: bool) -> bool {
    tell(out, Stage::Installing, None, "Clearing up".to_string());
    reset();
    let mut gone = Vec::new();
    if let Some(art) = crate::art::cache() {
        gone.push(art);
    }
    if data {
        if let Some(config) = heroic::native_config() {
            gone.push(config);
        }
    }
    for at in gone {
        match std::fs::remove_dir_all(&at) {
            Ok(()) => eprintln!("forget: removed {}", at.display()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => eprintln!("forget: {} could not be removed: {err}", at.display()),
        }
    }
    tell(
        out,
        Stage::Done,
        Some(1.0),
        "Heroic has been removed".to_string(),
    );
    true
}

/// Give back the filesystem permission the shell granted — see
/// `folder::permit`, which is the one that writes it.
///
/// Only this user's own overrides, because a `--user` override is the only one
/// anything here ever wrote: a system-wide installation's are per-installation
/// and need root, and are somebody else's to undo.
fn reset() {
    let out = Command::new("flatpak")
        .args(["override", "--user", "--reset", FLATPAK_ID])
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output();
    match out {
        Ok(out) if out.status.success() => eprintln!("forget: permissions reset"),
        Ok(out) => eprintln!(
            "forget: permissions were not reset: {}",
            said(&out.stderr).unwrap_or_else(|| "flatpak refused".to_string())
        ),
        Err(err) => eprintln!("forget: flatpak could not be run: {err}"),
    }
}

/// Heroic's own configuration directory on a machine where it was not a
/// flatpak, worked out here without an installation because by now there is
/// none.

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
