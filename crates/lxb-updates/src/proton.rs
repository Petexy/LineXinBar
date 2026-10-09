//! The Windows Apps package's Proton, as something this source can update.
//!
//! An optional package somebody may or may not have installed, found on
//! `PATH` and asked in JSON — the same contract the shell has with it, and
//! this crate depends on it no more than the shell does. What the two halves
//! share is a wire format declared on both sides, so the records below are
//! written out here rather than imported. See `crates/lxb-proton`.
//!
//! ## Why it is here at all
//!
//! Everything else this source updates is a release on GitHub, carried by a
//! package this system's repositories do not offer. A Proton is neither: it
//! is a tree under `$XDG_DATA_HOME/lxb/proton/` that the Windows Apps package
//! fetched for itself, and no package manager on earth will ever update it.
//! That makes it exactly what this source is for — and it is why the
//! LineXinBar row has to appear even on a machine whose LineXinBar came from
//! a distribution repository, where `releases::find` answers with nothing.
//!
//! ## What it never touches
//!
//! Steam's Protons and Steam's runtime. The helper decides that and this
//! crate does not second-guess it: `newer` answers about the managed build
//! and nothing else, and a machine with only Steam's Protons answers with
//! nothing at all.

use crate::Item;
use anyhow::Result;
use serde::Deserialize;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

/// The protocol this crate reads. One number, declared on both sides: a
/// helper that speaks another is one nothing is read from.
const PROTOCOL: u32 = 1;

/// The program, as it is called on `PATH`.
const HELPER: &str = "lxb-proton";

/// What the helper says is newer than the build it fetched.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Offer {
    pub protocol: u32,
    #[serde(default)]
    pub proton: Option<Download>,
    #[serde(default)]
    pub runtime: Option<Download>,
    #[serde(default)]
    pub trouble: Option<String>,
    #[serde(default)]
    pub note: String,
}

/// One thing to fetch.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Download {
    /// What it is called — a release tag, or a runtime's build.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub bytes: Option<u64>,
    /// Whether this machine already has this one.
    #[serde(default)]
    pub already: bool,
}

/// One Proton, as the helper lists it. Only two fields are read here; the
/// rest of the record is the shell's business.
#[derive(Debug, Clone, Deserialize)]
struct Tool {
    #[serde(default)]
    origin: String,
}

#[derive(Debug, Clone, Deserialize)]
struct Tools {
    protocol: u32,
    #[serde(default)]
    tools: Vec<Tool>,
}

/// Where this machine has the helper, or `None` where the package is not
/// installed.
///
/// `PATH` and nothing else. The shell looks beside itself as well, for a
/// development tree; this runs as an installed program or not at all.
pub fn helper() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(HELPER))
        .find(|at| at.is_file())
}

/// Ask the helper one question and read one JSON line back.
fn ask<T: for<'de> Deserialize<'de>>(verb: &str) -> Option<T> {
    let at = helper()?;
    let output = Command::new(at).arg(verb).output().ok()?;
    if !output.status.success() {
        return None;
    }
    // One record per line, and the last line is the answer: the helper
    // writes its own notes to stderr, but a future one writing progress
    // lines first should not make this unreadable.
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .rev()
        .find_map(|line| serde_json::from_str(line.trim()).ok())
}

/// Whether this machine has a Proton this source could update.
///
/// The cheap question, for discovery: directories, no network. Measured at
/// about a millisecond against a real Steam with two libraries, which is why
/// it is affordable on a page somebody is waiting for.
///
/// "Could update" means *ours*. Steam's Protons are the client's and are
/// never counted, so a machine with five of them and none of ours answers
/// no, and the LineXinBar row does not appear for a Proton nobody here owns.
pub fn installed() -> bool {
    let Some(tools) = ask::<Tools>("tools") else {
        return false;
    };
    tools.protocol == PROTOCOL && tools.tools.iter().any(|tool| tool.origin == "managed")
}

/// What is newer than the build this machine fetched. The network question.
///
/// `None` where there is no helper, where it speaks a protocol this does not
/// know, or where it could not answer — all three are "nothing to offer
/// here" rather than a failure of the whole check, because every other
/// source on that page is still worth showing.
pub fn newer() -> Option<Offer> {
    ask::<Offer>("newer").filter(|offer| offer.protocol == PROTOCOL)
}

/// What the review says about it, and whether there is anything to do.
///
/// Said as what a person gets rather than as what is downloaded: nobody
/// opened Settings to read a container runtime's build number. The sizes are
/// there because this is somebody's connection and somebody's disk.
pub fn items(offer: &Offer) -> Vec<Item> {
    let mut items = Vec::new();
    if let Some(proton) = &offer.proton {
        items.push(Item {
            name: "Windows games".into(),
            detail: match (proton.name.as_deref(), proton.bytes) {
                (Some(name), Some(bytes)) => {
                    format!(
                        "A newer version of what runs them · {name} · {}",
                        size(bytes)
                    )
                }
                (Some(name), None) => format!("A newer version of what runs them · {name}"),
                _ => "A newer version of what runs them".into(),
            },
        });
    }
    if let Some(runtime) = &offer.runtime {
        items.push(Item {
            name: "Windows games".into(),
            detail: match runtime.name.as_deref() {
                Some(name) => format!("A newer version of what they run inside · {name}"),
                None => "A newer version of what they run inside".into(),
            },
        });
    }
    items
}

/// A size somebody reads rather than counts.
fn size(bytes: u64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    let mib = bytes as f64 / MIB;
    match mib >= 1024.0 {
        true => format!("{:.1} GiB", mib / 1024.0),
        false => format!("{mib:.0} MiB"),
    }
}

/// Fetch it, saying how far it has got.
///
/// The helper does the whole of the work and has always done it: each
/// download checked against the digest published beside it, a runtime
/// checked again with Valve's own `pv-verify`, unpacked into a staging
/// directory and moved into place only once it is whole. Nothing of that is
/// repeated here — this runs it and turns its progress into the lines the
/// review shows.
pub fn fetch(say: impl Fn(&str)) -> Result<()> {
    let at =
        helper().ok_or_else(|| anyhow::anyhow!("The Windows Apps package is not installed"))?;
    say("Fetching what runs Windows games…");
    let output = Command::new(at)
        .arg("fetch")
        .env("LC_ALL", OsString::from("C"))
        .output()?;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if let Some(note) = stage_of(line) {
            say(&note);
        }
    }
    match output.status.success() {
        true => {
            say("Done. It applies the next time a Windows game is started.");
            Ok(())
        }
        false => {
            let said = String::from_utf8_lossy(&output.stderr);
            let why = said.lines().last().unwrap_or("The download did not finish");
            Err(anyhow::anyhow!("{why}"))
        }
    }
}

/// One progress line, as a sentence, or `None` for one nothing needs to say.
fn stage_of(line: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct Progress {
        #[serde(default)]
        stage: String,
        #[serde(default)]
        progress: Option<f32>,
    }
    let progress: Progress = serde_json::from_str(line.trim()).ok()?;
    // The helper's own words, which are the wire and not a guess — see
    // `report::Stage`. A word this does not know says nothing rather than
    // guessing: a newer helper may have stages this one never had.
    let what = match progress.stage.as_str() {
        "looking" => "Looking for the newest version",
        "proton" => "Downloading what runs Windows games",
        "runtime" => "Downloading what they run inside",
        "checking" => "Checking what was downloaded",
        "settling" => "Putting it in place",
        _ => return None,
    };
    Some(match progress.progress {
        Some(done) => format!("{what} · {}%", (done * 100.0).round() as i64),
        None => what.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer(proton: Option<(&str, u64)>, runtime: Option<&str>) -> Offer {
        Offer {
            protocol: PROTOCOL,
            proton: proton.map(|(name, bytes)| Download {
                name: Some(name.into()),
                bytes: Some(bytes),
                already: false,
            }),
            runtime: runtime.map(|name| Download {
                name: Some(name.into()),
                bytes: None,
                already: false,
            }),
            trouble: None,
            note: String::new(),
        }
    }

    /// Nothing newer is no rows, which is what makes the source say it has
    /// no updates rather than listing something nobody can act on.
    #[test]
    fn nothing_newer_is_nothing_to_review() {
        assert!(items(&offer(None, None)).is_empty());
    }

    /// What a person reads is what they get, and how much of their
    /// connection it costs. Neither row names a Proton, a container or a
    /// runtime: nobody opened Settings to read those.
    #[test]
    fn the_rows_say_what_somebody_gets_and_what_it_costs() {
        let rows = items(&offer(
            Some(("cachyos-11.0-20261005-slr", 346_993_320)),
            Some("4.0.20260805.254769"),
        ));
        assert_eq!(rows.len(), 2);
        assert!(rows[0].detail.contains("331 MiB"), "{}", rows[0].detail);
        for row in &rows {
            assert_eq!(row.name, "Windows games");
            let said = row.detail.to_lowercase();
            for never in ["proton", "runtime", "container", "digest", "prefix"] {
                assert!(!said.contains(never), "{} names {never}", row.detail);
            }
        }
    }

    /// Either half alone is one row.
    #[test]
    fn each_half_can_be_newer_on_its_own() {
        assert_eq!(items(&offer(Some(("tag", 1)), None)).len(), 1);
        assert_eq!(items(&offer(None, Some("4.0"))).len(), 1);
    }

    #[test]
    fn a_size_is_read_rather_than_counted() {
        assert_eq!(size(346_993_320), "331 MiB");
        assert_eq!(size(2 * 1024 * 1024 * 1024), "2.0 GiB");
        assert_eq!(size(0), "0 MiB");
    }

    /// The helper's progress becomes sentences, and the lines that are not
    /// progress at all say nothing.
    #[test]
    fn progress_becomes_something_a_person_can_read() {
        assert_eq!(
            stage_of(r#"{"stage":"proton","progress":0.5}"#).as_deref(),
            Some("Downloading what runs Windows games · 50%"),
        );
        assert_eq!(
            stage_of(r#"{"stage":"checking","progress":null}"#).as_deref(),
            Some("Checking what was downloaded"),
        );
        assert_eq!(
            stage_of(r#"{"stage":"done","progress":1.0}"#),
            None,
            "the end is said by the caller, which knows whether it worked"
        );
        assert_eq!(
            stage_of(r#"{"stage":"a-stage-from-a-later-version"}"#),
            None,
            "and a word this does not know says nothing rather than guessing"
        );
        assert_eq!(stage_of("not json at all"), None);
        assert_eq!(stage_of(""), None);
    }
}
