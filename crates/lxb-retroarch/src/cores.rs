//! Fetching a core, from the place RetroArch fetches its own.
//!
//! A libretro core is a shared object that is not part of RetroArch and is not
//! in anybody's distribution: the Flathub build ships **none at all**, and
//! without one a ROM is a file the emulator will open and refuse. The way a
//! person gets one is RetroArch's own Online Updater, which downloads it from
//! libretro's build server — and that is exactly what this does, from the same
//! server, to the same directory, so a core fetched here is one RetroArch's own
//! interface then lists as installed.
//!
//! ```text
//! https://buildbot.libretro.com/nightly/linux/<arch>/latest/
//!     .index-extended            date, hash and filename, one core per line
//!     ppsspp_libretro.so.zip     the core itself
//! ```
//!
//! ## Why the index is read first
//!
//! A console's cores are a list, best first — see [`crate::consoles`] — and
//! whether any given one exists for this machine's processor is a question only
//! the server can answer. Asking for the first and taking a 404 as "no core for
//! this console" would be wrong twice over: the second name on the list is
//! usually there, and a network that is simply down would read as a console
//! nobody has written an emulator for. So the listing is fetched once, the
//! first name on it wins, and a name that is on nobody's list is said out loud.
//!
//! ## What is not here
//!
//! Nothing decides *that* a core should be fetched. This runs when it is asked
//! to, with the names it is given; which console needs what, and whether the
//! user said yes, are the shell's — see `lxb-desktop`'s `retroarch.rs`.

use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use crate::assets;
use crate::find::{self, Cores};
use crate::report::{Fetch, Getting, PROTOCOL};
use crate::zip;

/// Where libretro publishes what it builds every night.
const BUILDBOT: &str = "https://buildbot.libretro.com/nightly/linux";

/// The listing of everything published for one architecture: `date hash name`,
/// one per line.
const INDEX: &str = ".index-extended";

/// What a core's file is called, after its name.
const SUFFIX: &str = "_libretro.so";

/// How long the whole of one request may take. Generous: a core is a few
/// megabytes and the line at the other end may be a telephone.
const PATIENCE: Duration = Duration::from_secs(600);

/// Fetch a core for each of `wanted`, and say what is happening as it happens.
///
/// Each item of `wanted` is one console's cores, best first, separated by
/// commas — the whole list rather than one name, because which of them exists
/// for this machine is not the shell's business to know. The first that the
/// build server has is the one fetched.
///
/// Returns whether every one of them ended installed. Every line written is one
/// JSON object and the last is always `Done` or `Failed`, so that a shell
/// watching this stream can tell it is over without watching the process too.
pub fn fetch(out: &mut impl Write, wanted: &[String]) -> bool {
    let of = wanted.len() as u32;
    let Some(installation) = find::installation() else {
        tell(
            out,
            Getting::Failed,
            "",
            None,
            0,
            of,
            "RetroArch is not here",
        );
        return false;
    };
    let cores = Cores::of(&installation);
    let Some(into) = cores.own().map(Path::to_path_buf) else {
        tell(
            out,
            Getting::Failed,
            "",
            None,
            0,
            of,
            "There is nowhere to put it",
        );
        return false;
    };
    let Some(arch) = arch() else {
        tell(
            out,
            Getting::Failed,
            "",
            None,
            0,
            of,
            &format!(
                "Nothing is made for this machine ({})",
                std::env::consts::ARCH
            ),
        );
        return false;
    };

    // Where the files a core reads beside itself go. Worked out even on a
    // machine where no core needs any, because which cores those are is not
    // known until the loop below has picked one.
    let system = find::config_dir(&installation).map(|config| assets::system_dir(&config));
    // And libretro's descriptions of them, for the one question that decides
    // whether a core already on the disk counts as an answer: whether it can
    // boot. See `firmware`, and the sort in `main` that puts the same question
    // to the same list.
    // What this machine has already tried and could not open, so a press cannot
    // spend a download on it twice. The scan drops these from the row's list as
    // well; this is the same rule said where the fetching happens, for a request
    // that came from anywhere else.
    let refused = find::refused();
    tell(out, Getting::Looking, "", None, 0, of, "Getting ready");
    let published = match index(arch) {
        Ok(published) => published,
        Err(why) => {
            tell(out, Getting::Failed, "", None, 0, of, &why);
            return false;
        }
    };

    let mut failed: Vec<String> = Vec::new();
    for (index, request) in wanted.iter().enumerate() {
        let at = index as u32 + 1;
        let names: Vec<&str> = request
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .filter(|name| !refused.iter().any(|had| had == name))
            .collect();

        // Somebody may have installed it themselves between the scan that found
        // it missing and this press. Nothing more to fetch — except the folder
        // that core reads beside itself, which RetroArch's own updater keeps on
        // a different page and which a core installed by hand very often has
        // not got. That is the whole of what this press is for, in the case
        // that brought it here.
        //
        // Whether it can *boot* is deliberately not asked. It was, once, and a
        // PlayStation 2 with pcsx2 installed and no BIOS beside it then failed
        // this test on every press and downloaded pcsx2 again to no purpose. A
        // missing BIOS is answered by asking for the BIOS, not by fetching
        // something else — see the shell's `retroarch::bios_row`.
        if let Some(had) = names.iter().find(|name| cores.find(name).is_some()) {
            eprintln!("cores: {had} is already installed");
            if let Err(why) = complete(out, had, system.as_deref(), at, of) {
                failed.push((*had).to_string());
                eprintln!("cores: {had} is still missing its system files: {why}");
            }
            continue;
        }

        // The first of the console's own list that this server actually
        // publishes, keeping the table's order.
        let Some(name) = names
            .iter()
            .find(|name| published.iter().any(|had| had == *name))
        else {
            failed.push(names.first().unwrap_or(&"a core").to_string());
            eprintln!("cores: nothing published for {request}");
            continue;
        };

        tell(out, Getting::Downloading, name, None, at, of, name);
        match one(out, arch, name, &into, at, of) {
            Ok(()) => {
                // Whole is not the same as loadable. libretro builds its cores
                // against a general-purpose Linux and a flatpak RetroArch runs
                // against a runtime that is not one, so a core can arrive
                // perfectly and then fail in the dynamic linker — and a core
                // left on the disk that cannot be opened is worse than none at
                // all: the scan finds it, the row says ready, and the press
                // does nothing. Take it back off.
                let file = into.join(format!("{name}{}", find::CORE_SUFFIX));
                // Before it is judged: libretro ships some cores built asking
                // for a stack they can execute, which no current glibc will
                // grant. Such a core is whole, names every library it needs,
                // and still cannot be opened — so repairing it here is what
                // keeps a perfectly good emulator from being thrown away and
                // refused for good three lines further down.
                match crate::execstack::clear(&file) {
                    Ok(true) => {
                        eprintln!("cores: {name} asked for an executable stack; taken off")
                    }
                    Ok(false) => {}
                    Err(why) => eprintln!("cores: {name} could not be repaired: {why}"),
                }
                if let Err(why) = find::loads(&installation, &file) {
                    let _ = std::fs::remove_file(&file);
                    // And do not offer it again on this machine, or the next
                    // press downloads the same core to throw it away again.
                    find::refuse(name);
                    failed.push((*name).to_string());
                    eprintln!("cores: {name} installed and will not load here: {why} missing");
                    continue;
                }
                eprintln!("cores: {name} installed");
                // A core is not ready because it is on the disk. The ones that
                // read a folder beside themselves are only half here until it
                // is too, and a half-here emulator draws a game with no text
                // in it rather than failing in any way somebody could read.
                if let Err(why) = complete(out, name, system.as_deref(), at, of) {
                    failed.push((*name).to_string());
                    eprintln!("cores: {name} is missing its system files: {why}");
                }
            }
            Err(why) => {
                failed.push((*name).to_string());
                eprintln!("cores: {name} could not be installed: {why}");
            }
        }
    }

    if failed.is_empty() {
        tell(
            out,
            Getting::Done,
            "",
            Some(1.0),
            of,
            of,
            "Everything is ready",
        );
        return true;
    }
    // Counted rather than named: what failed is a file whose name means
    // nothing to the person reading the panel, and the log has it.
    let note = match failed.len() {
        1 => "One thing could not be downloaded".to_string(),
        many => format!("{many} things could not be downloaded"),
    };
    tell(out, Getting::Failed, "", None, of, of, &note);
    false
}

/// Fetch one core and put it where RetroArch will find it.
fn one(
    out: &mut impl Write,
    arch: &str,
    name: &str,
    into: &Path,
    at: u32,
    of: u32,
) -> Result<(), String> {
    let url = format!("{BUILDBOT}/{arch}/latest/{name}{SUFFIX}.zip");
    let archive = download(out, &url, name, at, of)?;
    let core = zip::one_file(&archive, SUFFIX)?;
    // Handed to `dlopen` the moment somebody presses a game, so it is worth the
    // four bytes: a server that answered a request with something that is not a
    // shared object must not have that written to this disk under a core's
    // name, where every later run would find it and believe it.
    if !core.starts_with(b"\x7fELF") {
        return Err("what came down is not a shared object".to_string());
    }

    std::fs::create_dir_all(into)
        .map_err(|err| format!("{} could not be made: {err}", into.display()))?;
    // Beside the core rather than in a temporary directory, and renamed into
    // place: the rename is what makes a half-downloaded core impossible to
    // find, and a rename is only atomic within one filesystem.
    let done = into.join(format!("{name}{SUFFIX}"));
    let part = into.join(format!("{name}{SUFFIX}.part"));
    std::fs::write(&part, &core).map_err(|err| format!("it could not be written: {err}"))?;
    std::fs::rename(&part, &done).map_err(|err| {
        let _ = std::fs::remove_file(&part);
        format!("it could not be put in place: {err}")
    })?;
    Ok(())
}

/// Fetch the folder a core reads beside itself, where it needs one and has
/// not got it.
///
/// `Ok` for a core that needs nothing and for one already complete, because
/// both mean the same thing here: there is nothing left to do. See
/// [`crate::assets`], which is also where the reason this is a short list
/// rather than a rule is written down.
fn complete(
    out: &mut impl Write,
    core: &str,
    system: Option<&Path>,
    at: u32,
    of: u32,
) -> Result<(), String> {
    let Some(system) = system else {
        // No configuration directory means nowhere for RetroArch to read them
        // from either, so there is nothing this could usefully do.
        return Ok(());
    };
    let Some(bundle) = assets::wanted(system, core) else {
        return Ok(());
    };

    eprintln!("cores: {core} also needs {}", bundle.archive);
    tell(
        out,
        Getting::Downloading,
        core,
        None,
        at,
        of,
        &format!("{core} — system files"),
    );
    let archive = download(out, &assets::url(bundle), core, at, of)?;
    let files = zip::every_file(&archive)?;
    let written = assets::unpack(bundle, files, system)?;
    eprintln!(
        "cores: {core} got {written} system files under {}",
        system.display()
    );
    Ok(())
}

/// One file off the build server, with how far along it is written out as it
/// arrives.
fn download(
    out: &mut impl Write,
    url: &str,
    name: &str,
    at: u32,
    of: u32,
) -> Result<Vec<u8>, String> {
    let response = agent()
        .get(url)
        .call()
        .map_err(|err| format!("the build server could not be reached: {err}"))?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(format!("the build server answered {status}"));
    }

    let body = response.into_body();
    let total = body.content_length();
    let mut reader = body.into_reader();
    // A core is a few megabytes; the largest published is about ten. The cap is
    // for a server that has gone wrong, not for the cores.
    const CEILING: u64 = 256 * 1024 * 1024;
    let mut raw: Vec<u8> = Vec::with_capacity(total.unwrap_or(4 << 20).min(CEILING) as usize);
    let mut chunk = [0u8; 64 * 1024];
    let mut said = 0u8;
    loop {
        let read = reader
            .read(&mut chunk)
            .map_err(|err| format!("the download stopped: {err}"))?;
        if read == 0 {
            break;
        }
        raw.extend_from_slice(&chunk[..read]);
        if raw.len() as u64 > CEILING {
            return Err("what is coming down is far too large to be a core".to_string());
        }
        // One line per whole percent. A core arriving in sixty-kilobyte reads
        // would otherwise be two hundred lines a second down a pipe, to move a
        // bar by nothing.
        if let Some(total) = total.filter(|total| *total > 0) {
            let done = ((raw.len() as f64 / total as f64) * 100.0).min(100.0) as u8;
            if done != said {
                said = done;
                tell(
                    out,
                    Getting::Downloading,
                    name,
                    Some(done as f32 / 100.0),
                    at,
                    of,
                    name,
                );
            }
        }
    }
    if raw.is_empty() {
        return Err("the build server sent nothing".to_string());
    }
    Ok(raw)
}

/// The names of every core published for this architecture.
fn index(arch: &str) -> Result<Vec<String>, String> {
    let url = format!("{BUILDBOT}/{arch}/latest/{INDEX}");
    let mut response = agent().get(&url).call().map_err(|err| {
        eprintln!("cores: the listing could not be fetched: {err}");
        "Nothing could be downloaded".to_string()
    })?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        eprintln!("cores: the listing answered {status}");
        return Err("Nothing could be downloaded".to_string());
    }
    let listing = response.body_mut().read_to_string().map_err(|err| {
        eprintln!("cores: the listing could not be read: {err}");
        "Nothing could be downloaded".to_string()
    })?;
    let names = names(&listing);
    if names.is_empty() {
        eprintln!("cores: the listing came back empty");
        return Err("Nothing could be downloaded".to_string());
    }
    Ok(names)
}

/// The core names out of the build server's listing.
///
/// Its lines are `date hash filename`, and the filename is the only part of
/// interest — the hash is the build's, not the file's, and there is nothing to
/// check it against.
fn names(listing: &str) -> Vec<String> {
    listing
        .lines()
        .filter_map(|line| line.split_whitespace().next_back())
        .filter_map(|file| file.strip_suffix(".zip"))
        .filter_map(|file| file.strip_suffix(SUFFIX))
        .map(str::to_string)
        .collect()
}

/// What the build server calls this machine's processor.
///
/// `None` where it publishes nothing for it at all, which is every architecture
/// but these four — and is a sentence on a panel rather than a download that
/// answers 404 four times.
fn arch() -> Option<&'static str> {
    match std::env::consts::ARCH {
        "x86_64" => Some("x86_64"),
        "x86" => Some("x86"),
        "aarch64" => Some("aarch64"),
        "arm" => Some("armhf"),
        _ => None,
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        // A core that is not published answers 404, and that is an answer this
        // acts on rather than an error to be raised.
        .http_status_as_error(false)
        .timeout_global(Some(PATIENCE))
        .user_agent(concat!("LineXinBar/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

#[allow(clippy::too_many_arguments)]
fn tell(
    out: &mut impl Write,
    stage: Getting,
    core: &str,
    progress: Option<f32>,
    at: u32,
    of: u32,
    note: &str,
) {
    let line = Fetch {
        protocol: PROTOCOL,
        stage,
        core: core.to_string(),
        progress,
        at,
        of,
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

    /// The listing as the build server actually writes it.
    #[test]
    fn the_core_names_come_out_of_the_listing() {
        let listing = "2026-08-20 edf888ae mednafen_supergrafx_libretro.so.zip\n\
                       2026-08-21 af498a60 ppsspp_libretro.so.zip\n\
                       \n\
                       2026-08-20 b947e992 quicknes_libretro.so.zip\n";
        assert_eq!(
            names(listing),
            vec!["mednafen_supergrafx", "ppsspp", "quicknes"]
        );
    }

    /// And anything else in it is skipped rather than becoming a core name
    /// nothing will ever answer to.
    #[test]
    fn what_is_not_a_core_is_not_a_name() {
        let listing = "2026-08-20 aaaa RetroArch.7z\n2026-08-20 bbbb info.zip\n";
        assert!(names(listing).is_empty());
    }

    /// This machine is one the build server publishes for, which is worth
    /// asserting because the alternative is a panel that says so on a machine
    /// where everything else works.
    #[test]
    fn a_core_can_be_fetched_for_the_architecture_this_was_built_for() {
        // Only the four it publishes; a build for anything else is expected to
        // answer `None` and say so.
        let known = ["x86_64", "x86", "aarch64", "arm"];
        assert_eq!(
            arch().is_some(),
            known.contains(&std::env::consts::ARCH),
            "{} is not answered for",
            std::env::consts::ARCH
        );
    }
}
