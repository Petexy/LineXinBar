//! Where Heroic installs games: its own `defaultInstallPath`, changed from
//! the shell's Settings > Games > Epic Games.
//!
//! **Heroic's setting, not a copy of it.** Written into both of the files
//! Heroic keeps its settings in — `config.json`, which its main process
//! reads, and `store/config.json`, which its window reads — so Heroic's own
//! window shows the folder the shell chose, and the next install from either
//! goes there.
//!
//! **And the sandbox told.** Heroic's flatpak can write under
//! `~/Games/Heroic`, `/mnt`, `/media` and `/run/media` and nowhere else, so a
//! folder outside them is granted with a per-user override — which applies to
//! a system-wide Heroic as much as to the user's own, and needs no password.
//! A folder legendary cannot write to would be an install that fails at its
//! first byte.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

use crate::heroic::{self, Paths, FLATPAK_ID};
use crate::proton::write_json;
use crate::report::{Folder, Reason, PROTOCOL};

/// The places Heroic's flatpak may write to without being told, as its own
/// manifest grants them (`flatpak info --show-permissions`, 2.22.3).
fn reachable(home: &Path) -> [PathBuf; 4] {
    [
        home.join("Games/Heroic"),
        PathBuf::from("/mnt"),
        PathBuf::from("/media"),
        PathBuf::from("/run/media"),
    ]
}

/// Whether Heroic's sandbox already reaches `dir`.
pub fn already_reached(dir: &Path, home: &Path) -> bool {
    reachable(home).iter().any(|place| dir.starts_with(place))
}

/// Make `dir` where Heroic installs games.
pub fn choose(paths: &Paths, home: &Path, dir: &Path) -> Folder {
    let mut answer = Folder {
        protocol: PROTOCOL,
        base: heroic::install_base(paths, home)
            .to_string_lossy()
            .into_owned(),
        reason: None,
        note: String::new(),
    };
    let refused = |answer: &mut Folder, reason: Reason, note: String| {
        answer.reason = Some(reason);
        answer.note = note;
    };
    if !dir.is_absolute() || !dir.is_dir() {
        refused(
            &mut answer,
            Reason::Files,
            format!("{} is not a folder", dir.display()),
        );
        return answer;
    }
    // Heroic keeps its settings in memory and writes them back whole when it
    // changes one, which would put the old folder back.
    if heroic::in_the_way() {
        refused(&mut answer, Reason::HeroicRunning, "Heroic is open".into());
        return answer;
    }
    if !already_reached(dir, home) {
        if let Err(why) = permit(dir) {
            refused(&mut answer, Reason::FlatpakRefused, why);
            return answer;
        }
    }
    if let Err(why) = set(paths, home, "defaultInstallPath", json!(dir)) {
        refused(&mut answer, Reason::Files, why);
        return answer;
    }
    answer.base = heroic::install_base(paths, home)
        .to_string_lossy()
        .into_owned();
    answer.note = format!("games now go to {}", answer.base);
    answer
}

/// Let Heroic's sandbox write to `dir`, for this user.
fn permit(dir: &Path) -> Result<(), String> {
    let out = Command::new("flatpak")
        .args([
            "override",
            "--user",
            &format!("--filesystem={}", dir.display()),
            FLATPAK_ID,
        ])
        .stdin(Stdio::null())
        .output()
        .map_err(|err| format!("flatpak could not be run: {err}"))?;
    if out.status.success() {
        eprintln!("folder: Heroic may now write to {}", dir.display());
        return Ok(());
    }
    Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
}

/// Set one of Heroic's global settings, in both of the files it keeps them in.
pub fn set(paths: &Paths, home: &Path, key: &str, value: Value) -> Result<(), String> {
    let mut config = match std::fs::read_to_string(paths.config()) {
        Ok(text) => serde_json::from_str::<Value>(&text)
            .map_err(|err| format!("{} is not JSON: {err}", paths.config().display()))?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            // A Heroic that has never been started: the defaults it would
            // write itself, so the folder is not the only thing it has.
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
    config
        .as_object_mut()
        .ok_or("Heroic's settings are not an object")?
        .entry("defaultSettings")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("Heroic's default settings are not an object")?
        .insert(key.to_string(), value.clone());
    write_json(&paths.config(), &config, b"  ")?;

    if let Ok(text) = std::fs::read_to_string(paths.store_config()) {
        let mut store: Value = serde_json::from_str(&text)
            .map_err(|err| format!("{} is not JSON: {err}", paths.store_config().display()))?;
        if let Some(settings) = store["settings"].as_object_mut() {
            settings.insert(key.to_string(), value);
            write_json(&paths.store_config(), &store, b"\t")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sandbox_already_reaches_heroics_own_folder_and_the_drives() {
        let home = Path::new("/home/somebody");
        assert!(already_reached(
            Path::new("/home/somebody/Games/Heroic/More"),
            home
        ));
        assert!(already_reached(
            Path::new("/run/media/somebody/Games"),
            home
        ));
        assert!(already_reached(Path::new("/mnt/games"), home));
        assert!(!already_reached(Path::new("/home/somebody/Spiele"), home));
        assert!(!already_reached(Path::new("/srv/games"), home));
    }

    /// Both of Heroic's files, and nothing else in them.
    #[test]
    fn the_folder_is_written_where_heroic_keeps_it() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::at(dir.path().to_path_buf());
        std::fs::create_dir_all(dir.path().join("store")).unwrap();
        std::fs::write(
            paths.config(),
            r#"{"defaultSettings": {"defaultInstallPath": "/old", "maxWorkers": 0}, "version": "v0"}"#,
        )
        .unwrap();
        std::fs::write(
            paths.store_config(),
            r#"{"settings": {"defaultInstallPath": "/old", "language": "pl"}}"#,
        )
        .unwrap();
        set(
            &paths,
            Path::new("/home/somebody"),
            "defaultInstallPath",
            json!("/mnt/games"),
        )
        .unwrap();
        let config: Value =
            serde_json::from_str(&std::fs::read_to_string(paths.config()).unwrap()).unwrap();
        assert_eq!(
            config["defaultSettings"]["defaultInstallPath"],
            "/mnt/games"
        );
        assert_eq!(config["defaultSettings"]["maxWorkers"], 0);
        assert_eq!(config["version"], "v0");
        let store: Value =
            serde_json::from_str(&std::fs::read_to_string(paths.store_config()).unwrap()).unwrap();
        assert_eq!(store["settings"]["defaultInstallPath"], "/mnt/games");
        assert_eq!(store["settings"]["language"], "pl");
        assert_eq!(
            heroic::install_base(&paths, Path::new("/home/somebody")),
            PathBuf::from("/mnt/games")
        );
    }
}
