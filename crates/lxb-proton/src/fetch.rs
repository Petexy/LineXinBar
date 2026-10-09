//! Getting a Proton and a runtime onto a machine that has neither.
//!
//! The same shape as [`lxb-heroic`'s own Proton download], and deliberately:
//! fetch beside where it will live, check it against the digest published
//! with it, unpack into a staging directory, and move the whole thing into
//! place only once it is complete. A download that stopped halfway must never
//! be a Proton the next launch finds and tries to use — and on a machine with
//! no Steam, that launch is the only one there is.
//!
//! [`lxb-heroic`'s own Proton download]: ../../lxb-heroic/src/proton.rs
//!
//! Two sources, because the two halves are published in two different places
//! — see `crate::runtime` for why:
//!
//! - **The Proton** is `proton-cachyos`, from its GitHub releases: the plain
//!   `x86_64.tar.xz`, not the `x86_64_v3` one, with the `sha512sum` beside
//!   it. The same release and the same check `lxb-heroic` makes, so a machine
//!   running both integrations is running one build it has twice rather than
//!   two builds that disagree.
//! - **The runtime** is Valve's own, from `repo.steampowered.com`, against
//!   the `SHA256SUMS` beside it and then against the runtime's own `mtree`
//!   through the `pv-verify` it ships with. Valve already wrote the staged
//!   verification this needs; inventing a second one would only be a second
//!   thing to get wrong.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use sha2::{Digest, Sha256, Sha512};

use crate::report::Trouble;
use crate::runtime::{self, Suite};

/// Where the managed Proton comes from. The same repository `lxb-heroic`
/// takes Heroic's default from.
const RELEASES: &str = "https://api.github.com/repos/CachyOS/proton-cachyos/releases?per_page=5";

/// What went wrong, as a word for the shell and a sentence for the log.
pub type Failure = (Trouble, String);

/// One release of the managed Proton, as much of it as this needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    pub download: String,
    pub size: u64,
    pub checksum: String,
}

/// The build this machine's processor can run, as the release names it.
///
/// `x86_64` rather than `x86_64_v3`: the v3 build wants instructions a 2013
/// processor does not have, and a shell that is run on handhelds and on
/// whatever somebody has in a cupboard does not get to assume. The same
/// choice `lxb-heroic` makes, for the same reason.
fn suffix_for_this_machine() -> Option<&'static str> {
    match std::env::consts::ARCH {
        "x86_64" => Some("x86_64"),
        "aarch64" => Some("arm64"),
        _ => None,
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        // No timeout on the body: ureq's would cover the whole of it and
        // would cut a slow connection off part-way through three hundred
        // megabytes. A download somebody gave up on is ended by the shell
        // ending this process.
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .user_agent(concat!("LXB/", env!("CARGO_PKG_VERSION"), " Windows Apps"))
        .build()
        .into()
}

fn offline(what: &str) -> impl Fn(ureq::Error) -> Failure + '_ {
    move |err| (Trouble::Offline, format!("{what}: {err}"))
}

/// The newest release carrying a build for this machine, with its checksum.
pub fn newest_proton() -> Result<Release, Failure> {
    let suffix = suffix_for_this_machine().ok_or_else(|| {
        (
            Trouble::WrongMachine,
            format!("there is no managed Proton for {}", std::env::consts::ARCH),
        )
    })?;
    let agent = agent();
    let mut response = agent
        .get(RELEASES)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(offline("GitHub could not be reached"))?;
    let listing: serde_json::Value = response
        .body_mut()
        .read_to_vec()
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .ok_or((
            Trouble::Other,
            "GitHub's release list was unreadable".to_string(),
        ))?;
    pick(&listing, suffix).ok_or((
        Trouble::WrongMachine,
        format!("no release carries a {suffix} build"),
    ))
}

/// The newest release in a listing that carries both the archive for this
/// machine and the checksum beside it.
///
/// A release missing either is passed over rather than half-used: an archive
/// with no published digest is one this would have to trust, and nothing
/// here trusts a download.
pub fn pick(listing: &serde_json::Value, suffix: &str) -> Option<Release> {
    let archive_ending = format!("-{suffix}.tar.xz");
    let sum_ending = format!("-{suffix}.sha512sum");
    listing.as_array()?.iter().find_map(|release| {
        if release["draft"].as_bool() == Some(true) {
            return None;
        }
        let asset = |ending: &str| {
            release["assets"].as_array()?.iter().find(|asset| {
                asset["browser_download_url"]
                    .as_str()
                    .is_some_and(|url| url.ends_with(ending))
            })
        };
        let archive = asset(&archive_ending)?;
        let checksum = asset(&sum_ending)?;
        Some(Release {
            tag: release["tag_name"].as_str()?.to_string(),
            download: archive["browser_download_url"].as_str()?.to_string(),
            size: archive["size"].as_u64().unwrap_or(0),
            checksum: checksum["browser_download_url"].as_str()?.to_string(),
        })
    })
}

/// Which build of a runtime generation is published today, out of the one
/// line Valve keeps beside it.
pub fn published_runtime(suite: &Suite) -> Result<String, Failure> {
    let agent = agent();
    let text = agent
        .get(&suite.version_url())
        .call()
        .map_err(offline("the runtime version could not be read"))?
        .body_mut()
        .read_to_string()
        .map_err(|err| (Trouble::Offline, format!("the runtime version: {err}")))?;
    let line = text.lines().next().unwrap_or_default().trim().to_string();
    if line.is_empty() {
        return Err((Trouble::Other, "the runtime version was empty".to_string()));
    }
    Ok(line)
}

/// Which digest to check a download against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Digest512Or256 {
    /// GitHub's `sha512sum` file beside the archive, as `lxb-heroic` reads
    /// it: the whole file is searched for the digest rather than parsed,
    /// because the format differs between projects and a hex string either
    /// is in there or is not.
    Sha512Anywhere,
    /// Valve's `SHA256SUMS`, where the digest belongs to a named file.
    Sha256Named,
}

/// Fetch one archive and check it, leaving it beside where it will be
/// unpacked.
///
/// `progress` is told how far it has got, 0 to 1, as it goes.
#[allow(clippy::too_many_arguments)]
pub fn download(
    url: &str,
    into: &Path,
    expect: &str,
    how: Digest512Or256,
    size: u64,
    progress: &mut dyn FnMut(f32),
) -> Result<PathBuf, Failure> {
    std::fs::create_dir_all(into)
        .map_err(|err| (Trouble::Files, format!("{}: {err}", into.display())))?;
    let name = url.rsplit('/').next().unwrap_or("download");
    let part = into.join(format!(".{name}.lxb-part"));

    let agent = agent();
    let mut response = agent
        .get(url)
        .call()
        .map_err(offline("the download could not be started"))?;
    let mut reader = response
        .body_mut()
        .with_config()
        // A limit is required and the server's own size may be unknown, so
        // this is a ceiling rather than an expectation: two gigabytes is
        // well above any Proton or runtime and well below anything that
        // would fill a disk unnoticed.
        .limit(size.saturating_add(1 << 20).max(2 << 30))
        .reader();

    let mut file = std::fs::File::create(&part)
        .map_err(|err| (Trouble::Files, format!("{}: {err}", part.display())))?;
    let mut sha512 = Sha512::new();
    let mut sha256 = Sha256::new();
    let mut buffer = vec![0u8; 1 << 16];
    let mut got: u64 = 0;
    let mut told = -1i32;
    let copied: Result<(), Failure> = loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => break Ok(()),
            Ok(read) => read,
            Err(err) => break Err((Trouble::Offline, format!("the download stopped: {err}"))),
        };
        match how {
            Digest512Or256::Sha512Anywhere => sha512.update(&buffer[..read]),
            Digest512Or256::Sha256Named => sha256.update(&buffer[..read]),
        }
        if let Err(err) = file.write_all(&buffer[..read]) {
            break Err((trouble_for(&err), format!("{}: {err}", part.display())));
        }
        got += read as u64;
        if let Some(percent) = (got * 100).checked_div(size) {
            let percent = percent.min(100) as i32;
            if percent != told {
                told = percent;
                progress(percent as f32 / 100.0);
            }
        }
    };
    let flushed = file.sync_all();
    drop(file);
    let fail = |why: Failure| {
        let _ = std::fs::remove_file(&part);
        Err(why)
    };
    if let Err(why) = copied {
        return fail(why);
    }
    if let Err(err) = flushed {
        return fail((trouble_for(&err), format!("{}: {err}", part.display())));
    }

    let got = match how {
        Digest512Or256::Sha512Anywhere => hex(&sha512.finalize()),
        Digest512Or256::Sha256Named => hex(&sha256.finalize()),
    };
    let matched = match how {
        // The published file is `<digest>  <name>` or a bare digest
        // depending on the project, so the digest is looked for rather than
        // parsed out.
        Digest512Or256::Sha512Anywhere => expect.to_lowercase().contains(&got),
        Digest512Or256::Sha256Named => expect.eq_ignore_ascii_case(&got),
    };
    if !matched {
        return fail((
            Trouble::Damaged,
            format!("{name} did not match its published digest"),
        ));
    }
    Ok(part)
}

/// Whether a write failed because the disk is full, which is the one kind of
/// file trouble worth its own word on the screen: it is the only one the
/// person can do something about.
fn trouble_for(err: &std::io::Error) -> Trouble {
    // 28 is ENOSPC, spelt as the number because reading it from libc would
    // be a dependency for one constant.
    match err.raw_os_error() {
        Some(28) => Trouble::NoSpace,
        _ => Trouble::Files,
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Unpack an archive beside its final place and move it in whole.
///
/// `strip` is how many leading path elements the archive wraps everything
/// in: one for `proton-cachyos`, which packs a directory, and none for
/// Valve's runtime, which packs its contents. `proof` is the file whose
/// presence means the unpacked tree is actually the thing — the `proton`
/// script, or the runtime's entry point — so a truncated archive that `tar`
/// accepted is still refused.
pub fn unpack(archive: &Path, into: &Path, strip: u32, proof: &str) -> Result<(), Failure> {
    let parent = into
        .parent()
        .ok_or((Trouble::Files, "no folder to unpack into".to_string()))?;
    let name = into
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unpacking".to_string());
    let staging = parent.join(format!(".{name}.lxb-unpacking"));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)
        .map_err(|err| (Trouble::Files, format!("{}: {err}", staging.display())))?;

    let give_up = |why: Failure| {
        let _ = std::fs::remove_dir_all(&staging);
        let _ = std::fs::remove_file(archive);
        Err(why)
    };

    // `tar` with `xz` behind it is on every machine this shell runs on, and
    // doing it in-process would be two more dependencies for one call.
    let mut command = Command::new("tar");
    command.arg("-xJf").arg(archive).arg("-C").arg(&staging);
    if strip > 0 {
        command.arg(format!("--strip-components={strip}"));
    }
    let out = command
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output()
        .map_err(|err| (Trouble::Other, format!("tar could not be run: {err}")))?;
    for line in String::from_utf8_lossy(&out.stderr).lines() {
        eprintln!("tar: {line}");
    }
    if !out.status.success() || !staging.join(proof).exists() {
        return give_up((
            Trouble::Damaged,
            format!("{} did not unpack into a {proof}", archive.display()),
        ));
    }

    if into.exists() {
        // Two ways there is something here. A half-unpacked remnant of a
        // fetch that was interrupted, which is rubbish; or a whole working
        // tree this fetch is deliberately replacing, which is an update.
        //
        // Replacing one out from under a running game would be the end of
        // that game, so the fetch refuses to start while any game of ours is
        // running — see `run_fetch`, where that is the first thing it does.
        // The container copies pressure-vessel keeps are somewhere else
        // entirely, under the store's `containers/`, and are not touched.
        std::fs::remove_dir_all(into)
            .map_err(|err| (Trouble::Files, format!("{}: {err}", into.display())))?;
    }
    std::fs::rename(&staging, into)
        .map_err(|err| (Trouble::Files, format!("{}: {err}", into.display())))?;
    let _ = std::fs::remove_file(archive);
    Ok(())
}

/// Check an unpacked runtime against the manifest it ships with, using the
/// verifier it ships with.
///
/// Valve puts an `mtree.txt.gz` at the root of every container runtime and a
/// `pv-verify` beside it that reads it. That is the staged verification the
/// plan asks for, already written by the people who built the tree, so this
/// runs theirs rather than inventing a second answer to the same question.
///
/// A runtime with no verifier in it is accepted: older generations did not
/// ship one, and refusing a tree whose digest already matched Valve's own
/// `SHA256SUMS` would be refusing it for having fewer checks, not more.
pub fn verify_runtime(at: &Path) -> Result<(), Failure> {
    let verifier = at.join("pressure-vessel/bin/pv-verify");
    let manifest = at.join("mtree.txt.gz");
    if !verifier.is_file() || !manifest.is_file() {
        eprintln!(
            "runtime: {} ships no pv-verify; the published digest is the check",
            at.display()
        );
        return Ok(());
    }
    let out = Command::new(&verifier)
        .arg("--quiet")
        .current_dir(at)
        .stdin(Stdio::null())
        .output()
        .map_err(|err| (Trouble::Other, format!("pv-verify could not be run: {err}")))?;
    for line in String::from_utf8_lossy(&out.stderr).lines() {
        eprintln!("pv-verify: {line}");
    }
    if !out.status.success() {
        return Err((
            Trouble::Damaged,
            format!("{} did not match its own manifest", at.display()),
        ));
    }
    Ok(())
}

/// Valve's digest for the archive of one generation.
pub fn runtime_digest(suite: &Suite) -> Result<String, Failure> {
    let agent = agent();
    let sums = agent
        .get(&suite.sums_url())
        .call()
        .map_err(offline("the runtime digests could not be fetched"))?
        .body_mut()
        .read_to_string()
        .map_err(|err| (Trouble::Offline, format!("the runtime digests: {err}")))?;
    runtime::digest_for(&sums, suite.archive).ok_or((
        Trouble::Other,
        format!("{} is not in Valve's SHA256SUMS", suite.archive),
    ))
}

/// The checksum file published beside a Proton release.
pub fn proton_digest(release: &Release) -> Result<String, Failure> {
    let agent = agent();
    agent
        .get(&release.checksum)
        .call()
        .map_err(offline("the checksum could not be fetched"))?
        .body_mut()
        .read_to_string()
        .map_err(|err| (Trouble::Offline, format!("the checksum: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GitHub's release list, cut to what the choice reads, in the shape
    /// `proton-cachyos` actually publishes: three architectures per release,
    /// one of them the `_v3` build this must not take.
    fn listing() -> serde_json::Value {
        let url = |tag: &str, file: &str| {
            format!("https://github.com/CachyOS/proton-cachyos/releases/download/{tag}/{file}")
        };
        serde_json::json!([
            {"tag_name": "draft", "draft": true, "assets": []},
            {"tag_name": "no-sum", "assets": [
                {"browser_download_url": url("no-sum", "p-x86_64.tar.xz"), "size": 1}
            ]},
            {"tag_name": "cachyos-11.0-20261005-slr", "assets": [
                {"browser_download_url":
                    url("t", "proton-cachyos-11.0-20261005-slr-x86_64_v3.tar.xz"), "size": 9},
                {"browser_download_url":
                    url("t", "proton-cachyos-11.0-20261005-slr-x86_64_v3.sha512sum"), "size": 1},
                {"browser_download_url":
                    url("t", "proton-cachyos-11.0-20261005-slr-arm64.tar.xz"), "size": 7},
                {"browser_download_url":
                    url("t", "proton-cachyos-11.0-20261005-slr-arm64.sha512sum"), "size": 1},
                {"browser_download_url":
                    url("t", "proton-cachyos-11.0-20261005-slr-x86_64.sha512sum"), "size": 1},
                {"browser_download_url":
                    url("t", "proton-cachyos-11.0-20261005-slr-x86_64.tar.xz"), "size": 335059640}
            ]}
        ])
    }

    /// The plain build and not the `_v3` one, and not a release that
    /// publishes an archive without a digest to check it against.
    #[test]
    fn the_choice_is_the_newest_plain_build_with_a_checksum() {
        let release = pick(&listing(), "x86_64").expect("a release");
        assert_eq!(release.tag, "cachyos-11.0-20261005-slr");
        assert!(
            release.download.ends_with("-x86_64.tar.xz"),
            "{}",
            release.download
        );
        assert!(!release.download.contains("_v3"), "{}", release.download);
        assert!(release.checksum.ends_with("-x86_64.sha512sum"));
        assert_eq!(release.size, 335059640);
    }

    /// `-x86_64.tar.xz` is a suffix of `-x86_64_v3.tar.xz` nowhere, but
    /// `arm64` must not pick up the x86 build either. One assertion per
    /// machine this ships for.
    #[test]
    fn each_machine_gets_its_own_build() {
        assert!(pick(&listing(), "arm64")
            .unwrap()
            .download
            .ends_with("-arm64.tar.xz"));
        assert_eq!(pick(&listing(), "riscv64"), None);
        assert_eq!(pick(&serde_json::json!([]), "x86_64"), None);
    }

    /// A tree that `tar` unpacked but that is not the thing is refused, and
    /// nothing is left behind where a launch could find it.
    #[test]
    fn an_archive_without_the_thing_in_it_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let payload = dir.path().join("payload");
        std::fs::create_dir_all(payload.join("inner")).unwrap();
        std::fs::write(payload.join("inner/README"), "not a proton").unwrap();
        let archive = dir.path().join("p.tar.xz");
        let made = Command::new("tar")
            .arg("-cJf")
            .arg(&archive)
            .arg("-C")
            .arg(&payload)
            .arg("inner")
            .status();
        if !made.is_ok_and(|status| status.success()) {
            eprintln!("no tar with xz here; skipping");
            return;
        }

        let into = dir.path().join("protons/one");
        let failed = unpack(&archive, &into, 1, "proton");
        assert_eq!(failed.unwrap_err().0, Trouble::Damaged);
        assert!(!into.exists(), "nothing a launch could find is left");
        assert!(!archive.exists(), "the bad archive is not kept");
        assert!(
            !dir.path().join("protons/.one.lxb-unpacking").exists(),
            "and neither is the staging directory"
        );
    }

    /// The same archive with the thing in it lands whole, under the name it
    /// was asked for, with the wrapping directory stripped.
    #[test]
    fn a_good_archive_lands_whole() {
        let dir = tempfile::tempdir().unwrap();
        let payload = dir.path().join("payload");
        std::fs::create_dir_all(payload.join("inner/files")).unwrap();
        std::fs::write(payload.join("inner/proton"), "#!/usr/bin/env python3\n").unwrap();
        let archive = dir.path().join("p.tar.xz");
        let made = Command::new("tar")
            .arg("-cJf")
            .arg(&archive)
            .arg("-C")
            .arg(&payload)
            .arg("inner")
            .status();
        if !made.is_ok_and(|status| status.success()) {
            eprintln!("no tar with xz here; skipping");
            return;
        }

        let into = dir.path().join("protons/Proton-CachyOS");
        unpack(&archive, &into, 1, "proton").expect("it unpacks");
        assert!(into.join("proton").is_file());
        assert!(into.join("files").is_dir());
        assert!(!archive.exists(), "the archive is tidied away");
    }

    /// A runtime with no verifier in it is accepted — older generations ship
    /// none, and the published digest has already answered for the bytes.
    #[test]
    fn a_runtime_without_pv_verify_is_still_accepted() {
        let dir = tempfile::tempdir().unwrap();
        assert!(verify_runtime(dir.path()).is_ok());
    }
}
