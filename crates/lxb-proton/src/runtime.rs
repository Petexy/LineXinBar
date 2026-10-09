//! The container runtime a Proton asks for: which generation, where it is,
//! and where one can be had.
//!
//! Every modern Proton runs inside one of Valve's Steam Linux Runtimes, and
//! which one is not ours to choose — the build's own `toolmanifest.vdf` names
//! it by `require_tool_appid`, and this module is the two answers that number
//! leads to.
//!
//! ## Why it is a table and not a guess
//!
//! On 2026-10-06 both Valve's Proton Experimental and `proton-cachyos` ask
//! for `4183110`, Steam Linux Runtime 4.0. A build that hardcoded that would
//! work today and would quietly run tomorrow's Proton inside yesterday's
//! container — which is the failure that leaves no message anywhere. So the
//! id is read from the build, and [`SUITES`] turns it into Valve's own name
//! for the generation.
//!
//! An id this build has never heard of is reported as exactly that, with no
//! suite and nothing to fetch, rather than being silently mapped to the
//! newest one we know. A new generation is a thing to notice.
//!
//! ## Where one is had without Steam
//!
//! Valve publishes the container runtimes in the open, which is the finding
//! the standalone half of this integration rests on: the same tarball Steam
//! installs, at a plain URL, with Valve's own `SHA256SUMS` beside it and no
//! account anywhere. Measured on 2026-10-06 — `diff -rq` across all 243
//! directories of the published `SteamLinuxRuntime_4` against the copy Steam
//! had installed printed nothing.
//!
//! This is *not* true of Steam's own depots: the runtime cannot be fetched
//! anonymously through them at all (`No subscription`), while Proton
//! Experimental can. The two halves come from two different places, and that
//! asymmetry is why the managed build is `proton-cachyos` — see
//! `PROTON-INTEGRATION-TO-DO.MD`.

use std::path::{Path, PathBuf};

use crate::report::{Origin, Runtime};
use crate::steam::Steam;

/// One generation of the runtime: the app id a Proton names, Valve's name
/// for the suite, the directory Steam unpacks it into, and the archive
/// published for it.
pub struct Suite {
    pub app_id: u32,
    /// Valve's name for the generation, which is also the path element under
    /// `repo.steampowered.com`.
    pub name: &'static str,
    /// What Steam calls the directory in `steamapps/common`.
    pub installdir: &'static str,
    /// The archive published under
    /// `<name>/images/latest-container-runtime-depot/`.
    pub archive: &'static str,
}

/// The generations a Proton may ask for, newest last.
///
/// Scout (`1070560`) is deliberately absent: it is the old
/// `LD_LIBRARY_PATH` runtime rather than a container, no Proton this shell
/// would run asks for it, and its tree does not carry the
/// `latest-container-runtime-depot` the other three do — checked, and it
/// answers with a directory index rather than an archive.
pub const SUITES: &[Suite] = &[
    Suite {
        app_id: 1391110,
        name: "steamrt2",
        installdir: "SteamLinuxRuntime_soldier",
        archive: "SteamLinuxRuntime_soldier.tar.xz",
    },
    Suite {
        app_id: 1628350,
        name: "steamrt3",
        installdir: "SteamLinuxRuntime_sniper",
        archive: "SteamLinuxRuntime_sniper.tar.xz",
    },
    Suite {
        app_id: 4183110,
        name: "steamrt4",
        installdir: "SteamLinuxRuntime_4",
        archive: "SteamLinuxRuntime_4.tar.xz",
    },
];

/// Where Valve publishes them.
const PUBLISHED: &str = "https://repo.steampowered.com";

/// The generation an app id names, or `None` for one this build has never
/// heard of.
pub fn suite(app_id: u32) -> Option<&'static Suite> {
    SUITES.iter().find(|suite| suite.app_id == app_id)
}

impl Suite {
    /// The directory holding the published archive, its digests and its
    /// version — the channel Steam itself installs from, rather than the
    /// newest snapshot, so what this fetches is what a Steam machine would
    /// have.
    pub fn published(&self) -> String {
        format!(
            "{PUBLISHED}/{}/images/latest-container-runtime-depot",
            self.name
        )
    }

    /// The archive itself.
    pub fn archive_url(&self) -> String {
        format!("{}/{}", self.published(), self.archive)
    }

    /// Valve's digests for everything in that directory.
    pub fn sums_url(&self) -> String {
        format!("{}/SHA256SUMS", self.published())
    }

    /// The one line saying which build the directory currently holds —
    /// `4.0.20260805.254769`. What "is there a newer one" costs.
    pub fn version_url(&self) -> String {
        format!(
            "{PUBLISHED}/{}/images/latest-container-runtime-depot.txt",
            self.name
        )
    }
}

/// Everything known about the runtime an app id names: which generation it
/// is, and where this machine has it, if it has it.
pub fn resolve(app_id: u32, steam: &Steam, store: &Path) -> Runtime {
    let suite = suite(app_id);
    let found = suite.and_then(|suite| find(suite, steam, store));
    Runtime {
        app_id,
        suite: suite.map(|suite| suite.name.to_string()),
        entry_point: found
            .as_ref()
            .map(|(dir, _)| dir.join("_v2-entry-point").to_string_lossy().into_owned()),
        version: found.as_ref().and_then(|(dir, _)| version_of(dir)),
        origin: found.as_ref().map(|(_, origin)| *origin),
    }
}

/// Where this machine has that generation: Steam's copy first, then ours.
///
/// Steam's first deliberately. On a machine with Steam the client keeps its
/// runtime up to date and ours would be a second copy of the same hundred
/// and sixty megabytes; the only reason to fetch one at all is a machine
/// where nothing else will.
fn find(suite: &Suite, steam: &Steam, store: &Path) -> Option<(PathBuf, Origin)> {
    for library in steam.libraries() {
        let at = library.join("steamapps/common").join(suite.installdir);
        if is_a_runtime(&at) {
            return Some((at, Origin::Valve));
        }
    }
    let ours = store.join("runtimes").join(suite.name);
    is_a_runtime(&ours).then_some((ours, Origin::Managed))
}

/// Whether a directory is a usable runtime, which is one question: is the
/// entry point there.
///
/// Not "does the directory exist" — an interrupted download leaves one of
/// those, and a launch that found it would fail inside the container with
/// nothing on the screen to say why.
fn is_a_runtime(at: &Path) -> bool {
    at.join("_v2-entry-point").is_file()
}

/// Which build of a runtime is on the disk.
///
/// Two files, because an unpacked runtime and the directory it was published
/// from do not say it the same way. The tree itself carries `VERSIONS.txt`, a
/// table of every part with its own version, whose `depot` row is the number
/// for the whole — `depot\t4.0.20260805.254769`. The published directory
/// carries a one-line `VERSION.txt` instead. Reading only the second gave
/// `null` for every runtime actually installed on this machine.
fn version_of(dir: &Path) -> Option<String> {
    let table = std::fs::read_to_string(dir.join("VERSIONS.txt")).unwrap_or_default();
    if let Some(depot) = depot_row(&table) {
        return Some(depot);
    }
    let text = std::fs::read_to_string(dir.join("VERSION.txt")).ok()?;
    let line = text.lines().next()?.trim().to_string();
    Some(line).filter(|line| !line.is_empty())
}

/// The overall version out of a runtime's `VERSIONS.txt`: the second column
/// of the row named `depot`.
///
/// Tab-separated, with a `#Name` header and trailing comment columns. Only
/// the first two columns are read, and a table without a `depot` row is no
/// answer rather than the first number that happens to be in it.
fn depot_row(table: &str) -> Option<String> {
    table.lines().find_map(|line| {
        if line.starts_with('#') {
            return None;
        }
        let mut columns = line.split('\t').filter(|column| !column.is_empty());
        (columns.next()?.trim() == "depot")
            .then(|| columns.next().map(|version| version.trim().to_string()))
            .flatten()
            .filter(|version| !version.is_empty())
    })
}

/// The digest `SHA256SUMS` gives for one file in it.
///
/// Valve writes the usual `<hex>  <name>` pairs, with the two spaces
/// `sha256sum` writes and a leading `*` for a binary file on some of its
/// trees. Both are accepted; anything else is no answer rather than a wrong
/// one.
pub fn digest_for(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hex, rest) = line.split_once(char::is_whitespace)?;
        let named = rest.trim_start().trim_start_matches('*').trim();
        (named == name && hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()))
            .then(|| hex.to_lowercase())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three live generations, and the URLs they build. These strings
    /// were each fetched on 2026-10-06; a typo here is a download that 404s
    /// on somebody else's machine and nowhere else.
    #[test]
    fn each_generation_builds_the_url_it_was_measured_at() {
        let four = suite(4183110).expect("steamrt4");
        assert_eq!(four.name, "steamrt4");
        assert_eq!(
            four.archive_url(),
            "https://repo.steampowered.com/steamrt4/images/\
             latest-container-runtime-depot/SteamLinuxRuntime_4.tar.xz"
        );
        assert_eq!(
            four.sums_url(),
            "https://repo.steampowered.com/steamrt4/images/\
             latest-container-runtime-depot/SHA256SUMS"
        );
        assert_eq!(
            four.version_url(),
            "https://repo.steampowered.com/steamrt4/images/\
             latest-container-runtime-depot.txt"
        );
        assert_eq!(
            suite(1628350).unwrap().archive,
            "SteamLinuxRuntime_sniper.tar.xz"
        );
        assert_eq!(
            suite(1391110).unwrap().installdir,
            "SteamLinuxRuntime_soldier"
        );
    }

    /// A generation this build has never heard of is said to be exactly
    /// that. It is not quietly rounded to the newest one we know, because
    /// running a new Proton in an old container fails inside the container
    /// where nothing can see it.
    #[test]
    fn an_unknown_generation_is_reported_rather_than_guessed() {
        let steam = Steam {
            found: false,
            roots: Vec::new(),
        };
        let resolved = resolve(9999999, &steam, Path::new("/store"));
        assert_eq!(resolved.app_id, 9999999);
        assert_eq!(resolved.suite, None);
        assert_eq!(resolved.entry_point, None);
        assert!(
            suite(1070560).is_none(),
            "scout is deliberately not a suite"
        );
    }

    /// Steam's copy wins over ours: on a machine with Steam the client keeps
    /// it current, and a second copy would be the same hundred and sixty
    /// megabytes twice.
    #[test]
    fn steams_own_runtime_is_preferred_to_the_one_we_fetched() {
        let home = tempfile::tempdir().unwrap();
        let library = home.path().join("Steam");
        let theirs = library.join("steamapps/common/SteamLinuxRuntime_4");
        std::fs::create_dir_all(&theirs).unwrap();
        std::fs::write(theirs.join("_v2-entry-point"), "#!/bin/sh\n").unwrap();
        // The real file, as Steam's own SteamLinuxRuntime_4 carries it.
        std::fs::write(
            theirs.join("VERSIONS.txt"),
            "#Name\tVersion\t\tRuntime\tRuntime_Version\tComment\n\
             depot\t4.0.20260805.254769\t\t\t# Overall version number\n\
             pressure-vessel\t0.20260805.0\t\t\t\n",
        )
        .unwrap();

        let store = home.path().join("store");
        let ours = store.join("runtimes/steamrt4");
        std::fs::create_dir_all(&ours).unwrap();
        std::fs::write(ours.join("_v2-entry-point"), "#!/bin/sh\n").unwrap();

        std::fs::create_dir_all(library.join("steamapps")).unwrap();
        let steam = Steam {
            found: true,
            roots: vec![library],
        };
        let resolved = resolve(4183110, &steam, &store);
        assert_eq!(resolved.origin, Some(Origin::Valve));
        assert_eq!(resolved.version.as_deref(), Some("4.0.20260805.254769"));
        assert!(resolved
            .entry_point
            .unwrap()
            .ends_with("Steam/steamapps/common/SteamLinuxRuntime_4/_v2-entry-point"));
    }

    /// With no Steam, ours is found — and a directory left by a download
    /// that stopped halfway is not a runtime, because the entry point is
    /// what a launch actually runs.
    #[test]
    fn a_half_fetched_runtime_is_not_a_runtime() {
        let home = tempfile::tempdir().unwrap();
        let store = home.path().join("store");
        let ours = store.join("runtimes/steamrt4");
        std::fs::create_dir_all(ours.join("pressure-vessel")).unwrap();
        let steam = Steam {
            found: false,
            roots: Vec::new(),
        };
        assert_eq!(resolve(4183110, &steam, &store).entry_point, None);

        std::fs::write(ours.join("_v2-entry-point"), "#!/bin/sh\n").unwrap();
        let resolved = resolve(4183110, &steam, &store);
        assert_eq!(resolved.origin, Some(Origin::Managed));
        assert!(resolved.entry_point.is_some());
    }

    /// An unpacked runtime says its version in a table and a published one
    /// says it in a line, and both are read. Reading only the second gave
    /// `null` for every runtime on the machine this was written on.
    #[test]
    fn a_runtime_version_is_read_from_either_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(version_of(dir.path()), None);

        std::fs::write(dir.path().join("VERSION.txt"), "4.0.20260928.262390\n").unwrap();
        assert_eq!(
            version_of(dir.path()).as_deref(),
            Some("4.0.20260928.262390")
        );

        std::fs::write(
            dir.path().join("VERSIONS.txt"),
            "#Name\tVersion\t\tRuntime\tRuntime_Version\tComment\n\
             depot\t4.0.20260805.254769\t\t\t# Overall version number\n\
             pressure-vessel\t0.20260805.0\t\t\t\n",
        )
        .unwrap();
        assert_eq!(
            version_of(dir.path()).as_deref(),
            Some("4.0.20260805.254769"),
            "the tree's own table wins over the published line"
        );

        // A table with no `depot` row is no answer rather than the first
        // number in it.
        assert_eq!(depot_row("#Name\tVersion\nscripts\t0.1\n"), None);
    }

    /// Valve's `SHA256SUMS`, in the shape it is actually written in.
    #[test]
    fn a_digest_is_read_out_of_valves_own_sums() {
        let sums = "\
3226d82300000000000000000000000000000000000000000000000000009349  SteamLinuxRuntime_4.tar.xz
aaaa000000000000000000000000000000000000000000000000000000000000 *SteamLinuxRuntime_4-arm64.tar.xz
not-a-digest  SteamLinuxRuntime_4.tar.xz
";
        assert_eq!(
            digest_for(sums, "SteamLinuxRuntime_4.tar.xz").as_deref(),
            Some("3226d82300000000000000000000000000000000000000000000000000009349")
        );
        assert!(digest_for(sums, "SteamLinuxRuntime_4-arm64.tar.xz").is_some());
        assert_eq!(digest_for(sums, "nothing.tar.xz"), None);
    }
}
