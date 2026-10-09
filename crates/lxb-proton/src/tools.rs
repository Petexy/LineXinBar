//! What this machine can run a Windows game with, and which one it would
//! use.
//!
//! Three places a Proton comes from, and the difference between them is not
//! cosmetic — it is who owns the files:
//!
//! - **Valve's**, in a Steam library's `steamapps/common`. Read where it
//!   lies and never written to. Steam installed it and Steam updates it, and
//!   an update of ours that touched one would be writing into another
//!   program's installation.
//! - **Somebody's own**, in a library's `compatibilitytools.d`. Found so that
//!   a machine with one is not told it has no Proton, and otherwise left
//!   exactly alone.
//! - **The managed one**, which this helper fetched into its own store and is
//!   the only Proton it ever writes to.
//!
//! ## What a Proton says about itself
//!
//! Everything below is read out of the build's own `toolmanifest.vdf` rather
//! than assumed, which is the whole point of [`manifest`]. The key that
//! matters is `require_tool_appid`: the container runtime the build needs.
//! On this machine's Proton Experimental and on `proton-cachyos` alike it is
//! `4183110` today — and that is exactly why it is read. The generation
//! changes under us, and a hardcoded one would quietly run a new Proton in an
//! old container.

use std::path::{Path, PathBuf};

use crate::report::{Origin, Tool};
use crate::runtime;
use crate::steam::Steam;

/// Everything this machine could run a Windows game with, best first.
///
/// "Best" is [`newness`]: Valve's builds before anybody else's, and within
/// each the newer before the older. The shell draws the list in this order
/// and never sorts it — what counts as newer is a question about Valve's
/// naming, which belongs here and not on the other side of a pipe.
pub fn list(steam: &Steam, store: &Path) -> Vec<Tool> {
    let mut tools: Vec<Tool> = Vec::new();
    let mut add = |tool: Tool| {
        if !tools.iter().any(|had| had.bin == tool.bin) {
            tools.push(tool);
        }
    };

    for library in steam.libraries() {
        for (name, dir) in folders_in(&library.join("steamapps/common")) {
            if is_valves_proton(&name) {
                if let Some(tool) = read(&name, &dir, Origin::Valve, steam, store) {
                    add(tool);
                }
            }
        }
        // Both spellings: a library root holds `compatibilitytools.d`, and
        // the `root` symlink layout holds it one level in. Steam reads both,
        // so a build somebody put in either is one they expect to be offered.
        for at in [
            library.join("compatibilitytools.d"),
            library.join("root/compatibilitytools.d"),
        ] {
            for (name, dir) in folders_in(&at) {
                if let Some(tool) = read(&name, &dir, Origin::Custom, steam, store) {
                    add(tool);
                }
            }
        }
    }

    for (name, dir) in folders_in(&store.join("protons")) {
        if let Some(tool) = read(&name, &dir, Origin::Managed, steam, store) {
            add(tool);
        }
    }

    tools.sort_by_key(|tool| std::cmp::Reverse(newness(tool)));
    tools
}

/// Which of them a new prefix is made with, where the user has not chosen.
///
/// With Steam here, an installed Valve build — the plan's rule, and the one
/// somebody with Steam expects, since it is the same Proton their Steam games
/// run under. With no usable Valve build, the managed one, which is the whole
/// of what the standalone case has. A build somebody put in
/// `compatibilitytools.d` is never chosen for them: they chose it for Steam,
/// not for this.
pub fn chosen(tools: &[Tool]) -> Option<&Tool> {
    tools
        .iter()
        .find(|tool| tool.origin == Origin::Valve && usable(tool))
        .or_else(|| {
            tools
                .iter()
                .find(|tool| tool.origin == Origin::Managed && usable(tool))
        })
        .or_else(|| tools.iter().find(|tool| tool.origin == Origin::Valve))
        .or_else(|| tools.iter().find(|tool| tool.origin == Origin::Managed))
}

/// Whether a build could actually start a game today: it has the runtime it
/// asks for.
///
/// A build without one is still listed — it is a download away from working,
/// and a list that hid it would disagree with the disk — but it is not what
/// a prefix is made with by default.
fn usable(tool: &Tool) -> bool {
    tool.runtime
        .as_ref()
        .is_some_and(|runtime| runtime.entry_point.is_some())
}

/// Whether a directory in `steamapps/common` is one of Valve's Protons.
///
/// By name, because that is what Steam names them and there is nothing else
/// in the directory that says whose it is. `Proton - Experimental`,
/// `Proton 9.0`, `Proton Hotfix`, `Proton EasyAntiCheat Runtime` — the last
/// is not a Proton anybody plays with, and it has no `proton` script, so
/// [`read`] drops it without this needing to know.
fn is_valves_proton(name: &str) -> bool {
    name.starts_with("Proton")
}

/// One Proton, read out of its own files, or `None` for a directory that is
/// not one.
fn read(name: &str, dir: &Path, origin: Origin, steam: &Steam, store: &Path) -> Option<Tool> {
    let bin = dir.join("proton");
    if !bin.is_file() {
        return None;
    }
    let manifest = manifest(&dir.join("toolmanifest.vdf"));
    Some(Tool {
        name: name.to_string(),
        bin: bin.to_string_lossy().into_owned(),
        origin,
        version: version_of(dir),
        runtime: manifest
            .require_tool_appid
            .map(|app_id| runtime::resolve(app_id, steam, store)),
    })
}

/// A Proton's own `version` file: a build stamp and a name, of which the name
/// is the half worth carrying.
///
/// `1790839227 experimental-11.0-20261001-x86_64` — the number is the build
/// time and tells nobody anything; the rest is what the build calls itself
/// and is how two directories of the same name are told apart.
fn version_of(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("version")).ok()?;
    let line = text.lines().next()?.trim();
    let named = line.split_once(' ').map(|(_, name)| name).unwrap_or(line);
    Some(named.to_string()).filter(|named| !named.is_empty())
}

/// What a `toolmanifest.vdf` says, as much of it as anything here needs.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// The container runtime this build needs, where it names one. A build
    /// old enough to need none has no key.
    pub require_tool_appid: Option<u32>,
    /// How the build is invoked, `%verb%` and all — `/proton %verb%`, or the
    /// runtime's `/_v2-entry-point --verb=%verb% --`.
    ///
    /// Read rather than assumed because it is how the two layers are chained
    /// into one argv, and Valve has changed it before.
    pub commandline: Option<String>,
}

/// Read the handful of keys this needs out of a `toolmanifest.vdf`.
///
/// A hand-rolled read rather than a VDF parser, on the terms
/// [`crate::steam`] gives for `libraryfolders.vdf`: the file is Valve's, it
/// is a flat block of `"key" "value"` pairs, and a parser for the whole
/// format would be more code than the helper has use for. A line this does
/// not understand is a line it ignores.
pub fn manifest(at: &Path) -> Manifest {
    let Ok(text) = std::fs::read_to_string(at) else {
        return Manifest::default();
    };
    read_manifest(&text)
}

fn read_manifest(text: &str) -> Manifest {
    let mut manifest = Manifest::default();
    for line in text.lines() {
        let line = line.trim();
        // Valve's own generated manifests carry `//` comments.
        if line.starts_with("//") {
            continue;
        }
        let Some((key, rest)) = quoted(line) else {
            continue;
        };
        let Some((value, _)) = quoted(rest.trim()) else {
            continue;
        };
        match key {
            "require_tool_appid" => manifest.require_tool_appid = value.parse().ok(),
            "commandline" => manifest.commandline = Some(value.to_string()),
            _ => {}
        }
    }
    manifest
}

/// The first `"..."` of a line, and what follows it.
fn quoted(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some((&rest[..end], &rest[end + 1..]))
}

/// The folders directly under a directory, by name, in name order so the
/// list is the same list every time.
fn folders_in(at: &Path) -> Vec<(String, PathBuf)> {
    let Ok(listing) = std::fs::read_dir(at) else {
        return Vec::new();
    };
    let mut found: Vec<(String, PathBuf)> = listing
        .filter_map(Result::ok)
        .filter_map(|entry| Some((entry.file_name().into_string().ok()?, entry.path())))
        .filter(|(_, at)| at.is_dir())
        .collect();
    found.sort();
    found
}

/// How a list of Protons is ordered: later sorts first.
///
/// Valve's before anybody else's, because a Compatibility menu is a list of
/// what Steam would offer and Valve's are what it leads with. Within Valve's,
/// a numbered release before Experimental and Hotfix — deliberately, and for
/// the reason the Steam side already found: the last entry is what a game
/// nobody chose a Proton for gets, and that should be a release rather than
/// a moving target. Within the same kind, the higher number.
fn newness(tool: &Tool) -> (u8, u32, u32, String) {
    let whose = match tool.origin {
        Origin::Valve => 2,
        Origin::Managed => 1,
        Origin::Custom => 0,
    };
    let name = &tool.name;
    let moving = name.contains("Experimental") || name.contains("Hotfix");
    let kind = u8::from(!moving);
    let (major, minor) = version_numbers(name);
    (whose, u32::from(kind), major * 1000 + minor, name.clone())
}

/// The first `<major>.<minor>` in a name, as two numbers. `Proton 9.0` is
/// nine and nothing; `Proton 10.0` is ten, so it sorts above nine rather
/// than below it the way a string would.
fn version_numbers(name: &str) -> (u32, u32) {
    let digits: String = name
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let mut parts = digits.split('.');
    let major = parts.next().and_then(|n| n.parse().ok()).unwrap_or(0);
    let minor = parts.next().and_then(|n| n.parse().ok()).unwrap_or(0);
    (major, minor)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The manifest of this machine's own Proton Experimental, verbatim.
    const EXPERIMENTAL: &str = r#""manifest"
{
  "version" "2"
  "commandline" "/proton %verb%"
  "require_tool_appid" "4183110"
  "use_sessions" "1"
  "compatmanager_layer_name" "proton"
}
"#;

    /// And of the runtime it asks for, which carries no `require_tool_appid`
    /// of its own and a comment line Valve generates.
    const RUNTIME: &str = r#"// Generated file, do not edit
"manifest"
{
	"commandline" "/_v2-entry-point --verb=%verb% --"
	"filter_exclusive_priority" "4"
	"version" "2"
	"use_tool_subprocess_reaper" "1"
	"compatmanager_layer_name" "container-runtime"
}
"#;

    #[test]
    fn a_protons_manifest_names_the_runtime_it_needs() {
        let read = read_manifest(EXPERIMENTAL);
        assert_eq!(read.require_tool_appid, Some(4183110));
        assert_eq!(read.commandline.as_deref(), Some("/proton %verb%"));
    }

    /// The chain ends here: a runtime names no further tool, and the
    /// generated comment at the top is not mistaken for a key.
    #[test]
    fn a_runtimes_manifest_ends_the_chain() {
        let read = read_manifest(RUNTIME);
        assert_eq!(read.require_tool_appid, None);
        assert_eq!(
            read.commandline.as_deref(),
            Some("/_v2-entry-point --verb=%verb% --")
        );
    }

    #[test]
    fn a_manifest_that_is_not_there_says_nothing_rather_than_guessing() {
        assert_eq!(manifest(Path::new("/nowhere/at/all")), Manifest::default());
    }

    fn tool(name: &str, origin: Origin) -> Tool {
        Tool {
            name: name.to_string(),
            bin: format!("/t/{name}/proton"),
            origin,
            version: None,
            runtime: None,
        }
    }

    /// Valve's first, numbered releases above the moving ones, and ten above
    /// nine rather than below it.
    #[test]
    fn the_order_is_valves_newest_release_first() {
        let mut tools = [
            tool("Proton-CachyOS Latest", Origin::Custom),
            tool("Proton - Experimental", Origin::Valve),
            tool("Proton 9.0", Origin::Valve),
            tool("Proton 10.0", Origin::Valve),
            tool("proton-cachyos-11.0-20261005-slr-x86_64", Origin::Managed),
        ];
        tools.sort_by_key(|tool| std::cmp::Reverse(newness(tool)));
        let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Proton 10.0",
                "Proton 9.0",
                "Proton - Experimental",
                "proton-cachyos-11.0-20261005-slr-x86_64",
                "Proton-CachyOS Latest",
            ]
        );
    }

    /// A build that has its runtime is preferred to one that does not, even
    /// where the one without is Valve's and newer — a Compatibility choice
    /// nobody made should land on something that starts.
    #[test]
    fn the_default_is_a_build_that_can_actually_start_a_game() {
        let with_runtime = |mut tool: Tool| {
            tool.runtime = Some(crate::report::Runtime {
                app_id: 4183110,
                suite: Some("steamrt4".into()),
                entry_point: Some("/r/_v2-entry-point".into()),
                version: None,
                origin: Some(Origin::Managed),
            });
            tool
        };
        let tools = vec![
            tool("Proton - Experimental", Origin::Valve),
            with_runtime(tool("proton-cachyos", Origin::Managed)),
        ];
        assert_eq!(
            chosen(&tools).map(|t| t.name.as_str()),
            Some("proton-cachyos")
        );

        // And with nothing usable at all it still names one, so the shell can
        // say which build is a download away from working.
        let none = vec![tool("Proton - Experimental", Origin::Valve)];
        assert_eq!(
            chosen(&none).map(|t| t.name.as_str()),
            Some("Proton - Experimental")
        );
        assert_eq!(chosen(&[]), None);
    }

    /// Somebody's own build is never what a prefix is made with by default:
    /// they chose it for Steam, not for this.
    #[test]
    fn a_custom_build_is_offered_and_not_chosen() {
        let tools = vec![tool("GE-Proton10-1", Origin::Custom)];
        assert_eq!(chosen(&tools), None);
    }

    #[test]
    fn the_name_out_of_a_version_file_is_the_name_and_not_the_timestamp() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("version"),
            "1790839227 experimental-11.0-20261001-x86_64\n",
        )
        .unwrap();
        assert_eq!(
            version_of(dir.path()).as_deref(),
            Some("experimental-11.0-20261001-x86_64")
        );
    }

    /// A whole library read off the disk, Valve's and somebody else's, with
    /// a directory that is not a Proton sitting beside them.
    #[test]
    fn a_library_is_read_for_what_is_actually_in_it() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join(".local/share/Steam");
        let common = root.join("steamapps/common");
        std::fs::create_dir_all(&common).unwrap();

        let proton = common.join("Proton 10.0");
        std::fs::create_dir_all(&proton).unwrap();
        std::fs::write(proton.join("proton"), "#!/usr/bin/env python3\n").unwrap();
        std::fs::write(proton.join("toolmanifest.vdf"), EXPERIMENTAL).unwrap();
        std::fs::write(proton.join("version"), "1 10.0-ok\n").unwrap();

        // Valve's EasyAntiCheat runtime lives here too and is not a Proton:
        // it is named `Proton…` and carries no `proton` script.
        std::fs::create_dir_all(common.join("Proton EasyAntiCheat Runtime")).unwrap();
        // And a game, which is not named like one at all.
        std::fs::create_dir_all(common.join("Celeste")).unwrap();

        let custom = root.join("compatibilitytools.d/GE-Proton10-1");
        std::fs::create_dir_all(&custom).unwrap();
        std::fs::write(custom.join("proton"), "#!/usr/bin/env python3\n").unwrap();

        let steam = Steam {
            found: true,
            roots: vec![root],
        };
        let found = list(&steam, &home.path().join("store"));
        let names: Vec<&str> = found.iter().map(|tool| tool.name.as_str()).collect();
        assert_eq!(names, ["Proton 10.0", "GE-Proton10-1"]);
        assert_eq!(found[0].version.as_deref(), Some("10.0-ok"));
        assert_eq!(found[0].runtime.as_ref().unwrap().app_id, 4183110);
        assert_eq!(
            found[0].runtime.as_ref().unwrap().suite.as_deref(),
            Some("steamrt4")
        );
    }
}
