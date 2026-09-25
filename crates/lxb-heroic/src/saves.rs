//! Keeping saves in Epic's cloud, the way Heroic does it.
//!
//! Heroic syncs a game's saves around every launch — down before it, up after
//! it, the shell's launches through its shortcut included — where the game's
//! settings say `autoSyncSaves` and name the folder its saves are in,
//! `savesPath`. The first falls back to Heroic's global setting and the second
//! does not, so the shell's one switch is:
//!
//! * Heroic's global `autoSyncSaves`, on or off — its own setting, which a
//!   game can still turn off for itself in Heroic's window;
//! * and, turning it on, each installed game's save folder found and written
//!   into its own settings, as Heroic's switch does when somebody turns it on
//!   for one game.
//!
//! The folder is legendary's to find: `sync-saves --accept-path`, inside the
//! Wine prefix the game runs in, with nothing uploaded or downloaded. It
//! refuses to keep a folder it could not resolve — before any game has made
//! that prefix, say — and such a game is found again the next time the
//! library is refreshed.

use std::process::Stdio;

use serde_json::{json, Value};

use crate::folder;
use crate::heroic::{self, Paths};
use crate::library;
use crate::report::{Installation, Reason, Saves, PROTOCOL};

/// Turn syncing on or off.
pub fn set(installation: &Installation, paths: &Paths, home: &std::path::Path, on: bool) -> Saves {
    let mut answer = Saves {
        protocol: PROTOCOL,
        on: heroic::cloud_saves(paths),
        found: 0,
        reason: None,
        note: String::new(),
    };
    // Heroic writes its settings back whole when it changes one.
    if heroic::in_the_way() {
        answer.reason = Some(Reason::HeroicRunning);
        answer.note = "Heroic is open".into();
        return answer;
    }
    if let Err(why) = folder::set(paths, home, "autoSyncSaves", json!(on)) {
        answer.reason = Some(Reason::Files);
        answer.note = why;
        return answer;
    }
    answer.on = heroic::cloud_saves(paths);
    if on {
        answer.found = find_folders(installation, paths);
    }
    answer.note = format!("saves sync {}", if on { "on" } else { "off" });
    answer
}

/// Find the save folder of every installed game that keeps saves with Epic
/// and has none written down. How many were found.
pub fn find_folders(installation: &Installation, paths: &Paths) -> u32 {
    let mut found = 0;
    for game in library::read(paths) {
        let wanted = game.cloud_saves && game.store.is_none() && game.installed.is_some();
        if wanted && saves_path(paths, &game.app_name).is_none() {
            found += u32::from(find_folder(installation, paths, &game.app_name));
        }
    }
    found
}

/// Find one game's save folder and write it into its settings. Whether it
/// was found.
pub fn find_folder(installation: &Installation, paths: &Paths, app_name: &str) -> bool {
    let at = match stored_path(paths, app_name) {
        Some(at) => Some(at),
        None => {
            let prefix = heroic::prefix_of(paths, app_name)
                .map(|at| at.to_string_lossy().into_owned())
                .unwrap_or_default();
            // Heroic's own environment for a Proton game: legendary reads the
            // prefix out of `STEAM_COMPAT_DATA_PATH`, and its `pfx` inside.
            let ran = heroic::output(
                heroic::legendary_with(
                    installation,
                    paths,
                    &[
                        ("STEAM_COMPAT_DATA_PATH", prefix.clone()),
                        ("WINEPREFIX", prefix),
                    ],
                )
                .args([
                    "sync-saves",
                    app_name,
                    "--skip-upload",
                    "--skip-download",
                    "--accept-path",
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::piped()),
            );
            if let Ok(out) = ran {
                for line in String::from_utf8_lossy(&out.stderr).lines() {
                    eprintln!("legendary: {line}");
                }
            }
            stored_path(paths, app_name)
        }
    };
    let Some(at) = at else {
        eprintln!("saves: no folder for {app_name} yet");
        return false;
    };
    match write_saves_path(paths, app_name, &at) {
        Ok(()) => true,
        Err(why) => {
            eprintln!("saves: {why}");
            false
        }
    }
}

/// The save folder legendary has written down for a game, where it has one.
fn stored_path(paths: &Paths, app_name: &str) -> Option<String> {
    let text = std::fs::read_to_string(paths.installed()).ok()?;
    let installed: Value = serde_json::from_str(&text).ok()?;
    installed[app_name]["save_path"]
        .as_str()
        .filter(|at| at.starts_with('/'))
        .map(str::to_string)
}

/// The save folder Heroic has for a game in its own settings.
fn saves_path(paths: &Paths, app_name: &str) -> Option<String> {
    let text = std::fs::read_to_string(paths.game_config(app_name)).ok()?;
    let config: Value = serde_json::from_str(&text).ok()?;
    config[app_name]["savesPath"]
        .as_str()
        .filter(|at| !at.is_empty())
        .map(str::to_string)
}

/// Write `savesPath` into a game's own settings, as Heroic writes that file:
/// `{"<app>": {…}, "version": "v0", "explicit": false}`, two-space JSON,
/// everything else in it kept.
fn write_saves_path(paths: &Paths, app_name: &str, at: &str) -> Result<(), String> {
    let file = paths.game_config(app_name);
    let mut config = match std::fs::read_to_string(&file) {
        Ok(text) => serde_json::from_str::<Value>(&text)
            .map_err(|err| format!("{} is not JSON: {err}", file.display()))?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(err) => return Err(format!("{}: {err}", file.display())),
    };
    let object = config
        .as_object_mut()
        .ok_or_else(|| format!("{} is not an object", file.display()))?;
    object.entry("version").or_insert_with(|| json!("v0"));
    object.entry("explicit").or_insert_with(|| json!(false));
    object
        .entry(app_name)
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| format!("{app_name}'s settings are not an object"))?
        .insert("savesPath".to_string(), json!(at));
    crate::proton::write_json(&file, &config, b"  ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Heroic's per-game file, as it writes it, with the folder added and
    /// nothing else in it disturbed.
    #[test]
    fn a_games_save_folder_goes_into_its_own_settings() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::at(dir.path().to_path_buf());
        assert_eq!(saves_path(&paths, "Quail"), None);
        write_saves_path(
            &paths,
            "Quail",
            "/pfx/drive_c/users/steamuser/Saved Games/20XX",
        )
        .unwrap();
        assert_eq!(
            saves_path(&paths, "Quail").as_deref(),
            Some("/pfx/drive_c/users/steamuser/Saved Games/20XX")
        );
        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(paths.game_config("Quail")).unwrap())
                .unwrap();
        assert_eq!(written["version"], "v0");
        assert_eq!(written["explicit"], false);

        // A file Heroic already wrote keeps what it had.
        std::fs::write(
            paths.game_config("Owl"),
            r#"{"Owl": {"autoSyncSaves": false, "winePrefix": "/own/prefix"}, "version": "v0", "explicit": true}"#,
        )
        .unwrap();
        write_saves_path(&paths, "Owl", "/own/prefix/pfx/saves").unwrap();
        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(paths.game_config("Owl")).unwrap())
                .unwrap();
        assert_eq!(
            written["Owl"]["autoSyncSaves"], false,
            "a game's own choice stands"
        );
        assert_eq!(written["explicit"], true);
        assert_eq!(
            heroic::prefix_of(&paths, "Owl"),
            Some(std::path::PathBuf::from("/own/prefix"))
        );
    }

    #[test]
    fn legendarys_own_folder_is_read_only_where_it_is_one() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::at(dir.path().to_path_buf());
        std::fs::create_dir_all(paths.legendary()).unwrap();
        std::fs::write(
            paths.installed(),
            r#"{"Quail": {"save_path": "/pfx/saves"}, "Owl": {"save_path": null}, "Wren": {"save_path": "{AppData}/x"}}"#,
        )
        .unwrap();
        assert_eq!(stored_path(&paths, "Quail").as_deref(), Some("/pfx/saves"));
        assert_eq!(stored_path(&paths, "Owl"), None);
        assert_eq!(stored_path(&paths, "Wren"), None);
    }
}
