//! Heroic's own default Proton, put where Heroic would put it.
//!
//! A fresh Heroic has no Proton at all — its setting reads "Default Wine - Not
//! Found" — and what it does about that at a game's first launch depends on
//! the machine. With nothing anywhere it quietly downloads its default and
//! carries on. With *something* it can see — a Proton in Steam's
//! `compatibilitytools.d`, which is most machines this shell runs on — it stops
//! and asks "Wine not found! … continue with …?" in a window of its own, which
//! behind a loading screen is a game that never starts.
//!
//! So setting Heroic up settles this first, the way the user chose on
//! 2026-09-24: **Heroic's own default**, not a Proton borrowed from Steam. That
//! is what `downloadDefaultWine` in Heroic fetches — the newest release of
//! CachyOS/proton-cachyos, the `x86_64.tar.xz` checked against the
//! `x86_64.sha512sum` beside it — and this does the same, into the same place,
//! under the same name: `tools/proton/Proton-CachyOS-latest`, the entry Heroic's
//! Wine Manager keeps pointing at whichever release is newest. Then it is made
//! Heroic's default, in both of the files Heroic reads it from, and marked
//! installed in the Wine Manager's own list so Heroic's window agrees.
//!
//! A Proton Heroic can already use is left exactly as it is: somebody who chose
//! one in Heroic chose it.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{json, Value};
use sha2::{Digest, Sha512};

use crate::heroic::{self, Paths};
use crate::report::Reason;

/// What Heroic calls its moving default, and the directory it lives in.
pub const NAME: &str = "Proton-CachyOS-latest";

/// The Wine Manager's name for where it comes from.
const KIND: &str = "Proton-CachyOS";

/// Heroic's own source for it.
const RELEASES: &str = "https://api.github.com/repos/CachyOS/proton-cachyos/releases?per_page=5";

/// One release, as much of it as this needs.
#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub tag: String,
    pub date: String,
    pub download: String,
    pub size: u64,
    pub checksum: String,
    pub notes: String,
}

/// What went wrong, as a word for the shell and a sentence for the log.
pub type Failure = (Reason, String);

/// Make sure Heroic has a Proton to start games with. Answers its name.
///
/// `progress` is told how far a download has got, 0 to 1, as it goes.
pub fn settle(
    paths: &Paths,
    home: &Path,
    progress: &mut dyn FnMut(f32),
) -> Result<String, Failure> {
    if let Some(name) = heroic::proton(paths) {
        eprintln!("proton: Heroic already starts games with {name}");
        return Ok(name);
    }
    let dir = paths.protons().join(NAME);
    let mut fetched = None;
    if !dir.join("proton").is_file() {
        if std::env::consts::ARCH != "x86_64" {
            return Err((
                Reason::Proton,
                format!(
                    "Heroic's default Proton is not built for {}",
                    std::env::consts::ARCH
                ),
            ));
        }
        // Before three hundred megabytes rather than after them: nothing
        // fetched could be made Heroic's default while it is open.
        if heroic::running() {
            return Err((Reason::HeroicRunning, "Heroic is open".to_string()));
        }
        let agent = agent();
        let release = latest(&agent)?;
        eprintln!("proton: fetching {} ({} bytes)", release.tag, release.size);
        let archive = download(&agent, &release, paths, progress)?;
        unpack(&archive, &dir)?;
        fetched = Some(release);
    } else {
        eprintln!("proton: {} is already on the disk", dir.display());
    }

    if heroic::running() {
        return Err((Reason::HeroicRunning, "Heroic is open".to_string()));
    }
    make_default(paths, home, &dir).map_err(|why| (Reason::Files, why))?;
    // The Wine Manager's list is Heroic's window's business, not whether a
    // game starts, so a failure here is for the log only.
    if let Err(why) = record(paths, fetched.as_ref(), &dir) {
        eprintln!("proton: the Wine Manager's list was not updated: {why}");
    }
    Ok(NAME.to_string())
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        // No timeout on the body: ureq's covers the whole of it, and would cut
        // a slow connection off halfway through three hundred megabytes. The
        // shell ends a download somebody gave up on by ending this process.
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .user_agent(concat!(
            "LXB/",
            env!("CARGO_PKG_VERSION"),
            " Heroic companion"
        ))
        .build()
        .into()
}

/// The newest release that has the build Heroic takes.
fn latest(agent: &ureq::Agent) -> Result<Release, Failure> {
    let offline = |err: ureq::Error| {
        (
            Reason::Offline,
            format!("GitHub could not be reached: {err}"),
        )
    };
    let mut response = agent
        .get(RELEASES)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(offline)?;
    let listing: Value = response
        .body_mut()
        .read_to_vec()
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .ok_or_else(|| {
            (
                Reason::Proton,
                "GitHub's release list was unreadable".to_string(),
            )
        })?;
    pick(&listing).ok_or_else(|| {
        (
            Reason::Proton,
            "no release carries an x86_64 build".to_string(),
        )
    })
}

/// Heroic's choice from a release list: the first release carrying an
/// `x86_64.tar.xz` — the plain build, not the `x86_64_v3` one — with its
/// checksum beside it.
pub fn pick(listing: &Value) -> Option<Release> {
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
        let archive = asset("x86_64.tar.xz")?;
        let checksum = asset("x86_64.sha512sum")?;
        Some(Release {
            tag: release["tag_name"].as_str()?.to_string(),
            date: release["published_at"]
                .as_str()
                .map(|at| at.split('T').next().unwrap_or(at).to_string())
                .unwrap_or_default(),
            download: archive["browser_download_url"].as_str()?.to_string(),
            size: archive["size"].as_u64().unwrap_or(0),
            checksum: checksum["browser_download_url"].as_str()?.to_string(),
            notes: release["html_url"].as_str().unwrap_or_default().to_string(),
        })
    })
}

/// Fetch the archive beside where it will be unpacked, checking it on the way.
fn download(
    agent: &ureq::Agent,
    release: &Release,
    paths: &Paths,
    progress: &mut dyn FnMut(f32),
) -> Result<PathBuf, Failure> {
    let lost = |why: String| (Reason::Proton, why);
    let protons = paths.protons();
    std::fs::create_dir_all(&protons)
        .map_err(|err| (Reason::Files, format!("{}: {err}", protons.display())))?;

    let published = agent
        .get(&release.checksum)
        .call()
        .map_err(|err| {
            (
                Reason::Offline,
                format!("the checksum could not be fetched: {err}"),
            )
        })?
        .body_mut()
        .read_to_string()
        .map_err(|err| lost(format!("the checksum was unreadable: {err}")))?;

    let name = release
        .download
        .rsplit('/')
        .next()
        .unwrap_or("proton.tar.xz");
    let part = protons.join(format!(".{name}.lxb-part"));
    let mut response = agent.get(&release.download).call().map_err(|err| {
        (
            Reason::Offline,
            format!("the download could not be started: {err}"),
        )
    })?;
    let mut reader = response
        .body_mut()
        .with_config()
        .limit(release.size.saturating_add(1 << 20).max(1 << 30))
        .reader();
    let mut file = std::fs::File::create(&part)
        .map_err(|err| (Reason::Files, format!("{}: {err}", part.display())))?;
    let mut hasher = Sha512::new();
    let mut buffer = vec![0u8; 1 << 16];
    let mut got: u64 = 0;
    let mut told = -1i32;
    let copied: Result<(), String> = loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => break Ok(()),
            Ok(read) => read,
            Err(err) => break Err(format!("the download stopped: {err}")),
        };
        hasher.update(&buffer[..read]);
        if let Err(err) = file.write_all(&buffer[..read]) {
            break Err(format!("{}: {err}", part.display()));
        }
        got += read as u64;
        if let Some(percent) = (got * 100).checked_div(release.size) {
            let percent = percent.min(100) as i32;
            if percent != told {
                told = percent;
                progress(percent as f32 / 100.0);
            }
        }
    };
    let flushed = file.sync_all();
    drop(file);
    if let Err(why) = copied {
        let _ = std::fs::remove_file(&part);
        return Err((Reason::Offline, why));
    }
    if let Err(err) = flushed {
        let _ = std::fs::remove_file(&part);
        return Err((Reason::Files, format!("{}: {err}", part.display())));
    }
    let digest: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if !published.to_lowercase().contains(&digest) {
        let _ = std::fs::remove_file(&part);
        return Err(lost(format!("{name} did not match its checksum")));
    }
    Ok(part)
}

/// Unpack beside the final place and move it in only once it is whole, so a
/// Proton that stopped halfway is never one Heroic finds and tries to use.
fn unpack(archive: &Path, dir: &Path) -> Result<(), Failure> {
    let broken = |why: String| {
        let _ = std::fs::remove_file(archive);
        (Reason::Proton, why)
    };
    let parent = dir
        .parent()
        .ok_or_else(|| broken("no folder to unpack into".to_string()))?;
    let staging = parent.join(format!(".{NAME}.lxb-unpacking"));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)
        .map_err(|err| broken(format!("{}: {err}", staging.display())))?;
    // `tar` with `xz` behind it is on every machine flatpak is, and it is
    // what Heroic's own extraction amounts to: one level of the archive's
    // paths stripped, so the Proton is the directory rather than inside it.
    let out = Command::new("tar")
        .arg("-xJf")
        .arg(archive)
        .arg("-C")
        .arg(&staging)
        .arg("--strip-components=1")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output()
        .map_err(|err| broken(format!("tar could not be run: {err}")))?;
    for line in String::from_utf8_lossy(&out.stderr).lines() {
        eprintln!("tar: {line}");
    }
    if !out.status.success() || !staging.join("proton").is_file() {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(broken(format!(
            "{} did not unpack into a Proton",
            archive.display()
        )));
    }
    if dir.exists() {
        // Only ever a remnant: a whole one would have been used, not fetched.
        std::fs::remove_dir_all(dir).map_err(|err| broken(format!("{}: {err}", dir.display())))?;
    }
    std::fs::rename(&staging, dir).map_err(|err| broken(format!("{}: {err}", dir.display())))?;
    let _ = std::fs::remove_file(archive);
    Ok(())
}

/// What Heroic keeps as a Proton setting.
fn setting(dir: &Path) -> Value {
    json!({"bin": dir.join("proton"), "name": NAME, "type": "proton"})
}

/// Make it Heroic's default, in both files Heroic reads it from.
///
/// `config.json` is what Heroic launches with. A Heroic that has never been
/// started has none, and gets one holding this and the prefix settings: Heroic
/// lays a file over its own defaults, but it copies `winePrefix` and
/// `defaultWinePrefixDir` across even when they are missing, which would put
/// nothing where its defaults had something. `store/config.json` is the copy
/// Heroic's window shows, and is only updated where Heroic has made one.
pub fn make_default(paths: &Paths, home: &Path, dir: &Path) -> Result<(), String> {
    make_default_wine(paths, home, setting(dir))
}

/// Make any of the things Heroic runs games with its default — a Proton of
/// Steam's, or one of its own Wines — in both files, as [`make_default`]
/// does for the one it fetched.
pub fn make_default_wine(paths: &Paths, home: &Path, wine: Value) -> Result<(), String> {
    let mut config = match std::fs::read_to_string(paths.config()) {
        Ok(text) => serde_json::from_str::<Value>(&text)
            .map_err(|err| format!("{} is not JSON: {err}", paths.config().display()))?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let prefixes = home.join("Games/Heroic/Prefixes");
            json!({
                "defaultSettings": {
                    "defaultInstallPath": home.join("Games/Heroic"),
                    "defaultWinePrefix": prefixes,
                    "defaultWinePrefixDir": prefixes,
                    "winePrefix": prefixes.join("shared"),
                },
                "version": "v0"
            })
        }
        Err(err) => return Err(format!("{}: {err}", paths.config().display())),
    };
    let settings = config
        .as_object_mut()
        .ok_or("Heroic's settings are not an object")?
        .entry("defaultSettings")
        .or_insert_with(|| json!({}));
    settings
        .as_object_mut()
        .ok_or("Heroic's default settings are not an object")?
        .insert("wineVersion".to_string(), wine.clone());
    write_json(&paths.config(), &config, b"  ")?;

    if let Ok(text) = std::fs::read_to_string(paths.store_config()) {
        let mut store: Value = serde_json::from_str(&text)
            .map_err(|err| format!("{} is not JSON: {err}", paths.store_config().display()))?;
        if let Some(settings) = store["settings"].as_object_mut() {
            settings.insert("wineVersion".to_string(), wine);
            write_json(&paths.store_config(), &store, b"\t")?;
        }
    }
    Ok(())
}

/// Mark it installed in the Wine Manager's list, the way Heroic's own install
/// leaves the entry: where it is, how big, installed, nothing to update.
pub fn record(paths: &Paths, fetched: Option<&Release>, dir: &Path) -> Result<(), String> {
    let at = paths.wine_releases();
    let mut store = match std::fs::read_to_string(&at) {
        Ok(text) => {
            serde_json::from_str::<Value>(&text).map_err(|err| format!("not JSON: {err}"))?
        }
        // Heroic writes this list the first time it looks for releases. A
        // Proton that was already on the disk, with nothing known about which
        // release it is, is not worth starting one for.
        Err(_) if fetched.is_none() => return Ok(()),
        Err(_) => json!({"wine-releases": []}),
    };
    let releases = store
        .as_object_mut()
        .ok_or("not an object")?
        .entry("wine-releases")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or("the releases are not a list")?;
    let index = match releases
        .iter()
        .position(|release| release["version"] == NAME && release["type"] == KIND)
    {
        Some(index) => index,
        None if fetched.is_some() => {
            // First, which is where Heroic keeps its "-latest" entries.
            releases.insert(0, json!({"version": NAME, "type": KIND}));
            0
        }
        None => return Ok(()),
    };
    let fields = releases[index]
        .as_object_mut()
        .ok_or("an entry is not an object")?;
    if let Some(release) = fetched {
        fields.insert("date".into(), json!(release.date));
        fields.insert("download".into(), json!(release.download));
        fields.insert("downsize".into(), json!(release.size));
        fields.insert("checksum".into(), json!(release.checksum));
        fields.insert("release_notes_link".into(), json!(release.notes));
    }
    fields.insert("disksize".into(), json!(size_of(dir)));
    fields.insert("installDir".into(), json!(dir));
    fields.insert("isInstalled".into(), json!(true));
    fields.insert("hasUpdate".into(), json!(false));
    write_json(&at, &store, b"\t")
}

/// How much a directory holds, in bytes.
fn size_of(dir: &Path) -> u64 {
    let Ok(listing) = std::fs::read_dir(dir) else {
        return 0;
    };
    listing
        .filter_map(Result::ok)
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => size_of(&entry.path()),
            Ok(kind) if kind.is_file() => entry.metadata().map(|meta| meta.len()).unwrap_or(0),
            _ => 0,
        })
        .sum()
}

/// Write JSON the way Heroic writes that file, beside it and then over it.
pub fn write_json(at: &Path, value: &Value, indent: &[u8]) -> Result<(), String> {
    let mut bytes = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(indent);
    let mut serializer = serde_json::Serializer::with_formatter(&mut bytes, formatter);
    serde::Serialize::serialize(value, &mut serializer).map_err(|err| err.to_string())?;
    if let Some(folder) = at.parent() {
        std::fs::create_dir_all(folder).map_err(|err| format!("{}: {err}", folder.display()))?;
    }
    let temp = at.with_extension(format!("lxb-{}", std::process::id()));
    std::fs::write(&temp, &bytes).map_err(|err| format!("{}: {err}", temp.display()))?;
    if let Ok(meta) = std::fs::metadata(at) {
        let _ = std::fs::set_permissions(&temp, meta.permissions());
    }
    std::fs::rename(&temp, at).map_err(|err| {
        let _ = std::fs::remove_file(&temp);
        format!("{}: {err}", at.display())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::at(dir.path().join("config/heroic"));
        std::fs::create_dir_all(paths.root()).unwrap();
        (dir, paths)
    }

    /// GitHub's release list, cut down to what the choice reads, with the
    /// two builds CachyOS publishes and one release missing its checksum.
    #[test]
    fn heroics_choice_is_the_newest_plain_build_with_a_checksum() {
        let url = |tag: &str, file: &str| {
            format!("https://github.com/CachyOS/proton-cachyos/releases/download/{tag}/{file}")
        };
        let listing = json!([
            {"tag_name": "draft", "draft": true, "assets": []},
            {"tag_name": "no-sum", "published_at": "2026-08-01T00:00:00Z", "assets": [
                {"browser_download_url": url("no-sum", "proton-x86_64.tar.xz"), "size": 1}
            ]},
            {"tag_name": "cachyos-11.0-20260703-slr", "published_at": "2026-07-22T12:00:00Z",
             "html_url": "https://github.com/CachyOS/proton-cachyos/releases/tag/cachyos-11.0-20260703-slr",
             "assets": [
                {"browser_download_url": url("t", "proton-cachyos-11.0-20260703-slr-x86_64_v3.tar.xz"), "size": 9},
                {"browser_download_url": url("t", "proton-cachyos-11.0-20260703-slr-x86_64.sha512sum"), "size": 1},
                {"browser_download_url": url("t", "proton-cachyos-11.0-20260703-slr-x86_64.tar.xz"), "size": 335059640}
            ]}
        ]);
        let release = pick(&listing).unwrap();
        assert_eq!(release.tag, "cachyos-11.0-20260703-slr");
        assert_eq!(release.date, "2026-07-22");
        assert!(
            release.download.ends_with("-x86_64.tar.xz"),
            "{}",
            release.download
        );
        assert!(!release.download.contains("_v3"));
        assert!(release.checksum.ends_with("-x86_64.sha512sum"));
        assert_eq!(release.size, 335059640);
        assert_eq!(pick(&json!([])), None);
    }

    /// A Heroic that has been started keeps every other setting it had; only
    /// the Proton changes, in both files, each in its own indentation.
    #[test]
    fn making_it_the_default_changes_that_and_nothing_else() {
        let (_scratch, paths) = scratch();
        std::fs::write(
            paths.config(),
            r#"{"defaultSettings": {"maxWorkers": 0, "winePrefix": "/p/shared",
                "wineVersion": {"bin": "", "name": "Default Wine - Not Found", "type": "wine"}},
               "version": "v0"}"#,
        )
        .unwrap();
        std::fs::create_dir_all(paths.store_config().parent().unwrap()).unwrap();
        std::fs::write(
            paths.store_config(),
            r#"{"userHome": "/home/somebody", "settings": {"language": "pl", "wineVersion": {}}}"#,
        )
        .unwrap();
        let dir = paths.protons().join(NAME);

        make_default(&paths, Path::new("/home/somebody"), &dir).unwrap();

        let config: Value =
            serde_json::from_str(&std::fs::read_to_string(paths.config()).unwrap()).unwrap();
        assert_eq!(config["version"], "v0");
        assert_eq!(config["defaultSettings"]["maxWorkers"], 0);
        assert_eq!(config["defaultSettings"]["winePrefix"], "/p/shared");
        assert_eq!(config["defaultSettings"]["wineVersion"], setting(&dir));
        assert!(std::fs::read_to_string(paths.config())
            .unwrap()
            .contains("\n  \"defaultSettings\""));

        let text = std::fs::read_to_string(paths.store_config()).unwrap();
        assert!(text.contains("\n\t\"settings\""), "{text}");
        let store: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(store["userHome"], "/home/somebody");
        assert_eq!(store["settings"]["language"], "pl");
        assert_eq!(store["settings"]["wineVersion"]["name"], NAME);
    }

    /// A Heroic nobody has started yet: the file it will lay over its own
    /// defaults has to carry the prefix settings, which Heroic would otherwise
    /// copy across as nothing.
    #[test]
    fn a_heroic_never_started_gets_the_prefixes_with_its_proton() {
        let (_scratch, paths) = scratch();
        let dir = paths.protons().join(NAME);
        make_default(&paths, Path::new("/home/somebody"), &dir).unwrap();
        let config: Value =
            serde_json::from_str(&std::fs::read_to_string(paths.config()).unwrap()).unwrap();
        let settings = &config["defaultSettings"];
        assert_eq!(
            settings["winePrefix"],
            "/home/somebody/Games/Heroic/Prefixes/shared"
        );
        assert_eq!(
            settings["defaultWinePrefix"],
            "/home/somebody/Games/Heroic/Prefixes"
        );
        assert_eq!(
            settings["defaultWinePrefixDir"],
            "/home/somebody/Games/Heroic/Prefixes"
        );
        assert_eq!(
            settings["defaultInstallPath"],
            "/home/somebody/Games/Heroic"
        );
        assert_eq!(settings["wineVersion"]["type"], "proton");
        assert_eq!(config["version"], "v0");
        assert!(
            !paths.store_config().exists(),
            "Heroic's window store is Heroic's to create"
        );
    }

    #[test]
    fn the_wine_manager_is_told_it_is_installed() {
        let (_scratch, paths) = scratch();
        let dir = paths.protons().join(NAME);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("proton"), "12345").unwrap();
        std::fs::create_dir_all(paths.wine_releases().parent().unwrap()).unwrap();
        std::fs::write(
            paths.wine_releases(),
            json!({"wine-releases": [
                {"version": "GE-Proton-latest", "type": "GE-Proton"},
                {"version": NAME, "type": KIND, "checksum": "c", "disksize": 0}
            ]})
            .to_string(),
        )
        .unwrap();

        record(&paths, None, &dir).unwrap();

        let store: Value =
            serde_json::from_str(&std::fs::read_to_string(paths.wine_releases()).unwrap()).unwrap();
        let entry = &store["wine-releases"][1];
        assert_eq!(entry["isInstalled"], true);
        assert_eq!(entry["hasUpdate"], false);
        assert_eq!(entry["disksize"], 5);
        assert_eq!(
            entry["checksum"], "c",
            "an adopted Proton keeps what Heroic knew of it"
        );
        assert_eq!(entry["installDir"], json!(dir));
        assert_eq!(store["wine-releases"][0]["version"], "GE-Proton-latest");
    }

    #[test]
    fn a_fetched_release_is_written_in_where_heroic_has_no_list_yet() {
        let (_scratch, paths) = scratch();
        let dir = paths.protons().join(NAME);
        std::fs::create_dir_all(&dir).unwrap();
        record(&paths, None, &dir).unwrap();
        assert!(
            !paths.wine_releases().exists(),
            "nothing known, nothing written"
        );

        let release = Release {
            tag: "t".into(),
            date: "2026-07-22".into(),
            download: "https://d/x86_64.tar.xz".into(),
            size: 7,
            checksum: "https://d/x86_64.sha512sum".into(),
            notes: "https://n".into(),
        };
        record(&paths, Some(&release), &dir).unwrap();
        let store: Value =
            serde_json::from_str(&std::fs::read_to_string(paths.wine_releases()).unwrap()).unwrap();
        let entry = &store["wine-releases"][0];
        assert_eq!(entry["version"], NAME);
        assert_eq!(entry["type"], KIND);
        assert_eq!(entry["download"], "https://d/x86_64.tar.xz");
        assert_eq!(entry["checksum"], "https://d/x86_64.sha512sum");
        assert_eq!(entry["isInstalled"], true);
    }
}
