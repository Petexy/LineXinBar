//! What Heroic can run a game with, and which one each game runs with.
//!
//! The list is Heroic's own — `getLinuxWineSet` in its `main.js`, read here
//! from the host rather than asked of a Heroic that may not be running: the
//! Protons in its `tools/proton`, those in every Steam library's
//! `compatibilitytools.d` (from Steam's `libraryfolders.vdf`, under the Steam
//! folder Heroic's settings name), Valve's own Protons where Heroic's
//! `showValveProton` asks for them, and the Wines in its `tools/wine` and in
//! Lutris's runners. Paths are written down exactly as Heroic would write
//! them, because it is Heroic, in its sandbox, that reads them back.
//!
//! What a game runs with is Heroic's `wineVersion`: the default in its
//! `config.json` (and the copy in `store/config.json` its window shows), and a
//! game's own in `GamesConfig/<app>.json`, which Heroic lays over the default
//! key by key. Clearing a game's own is taking the key out.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::heroic::Paths;
use crate::report::{Reason, Tool, ToolKind, ToolSet, Tools, PROTOCOL};

/// Everything Heroic would offer, Protons first as Heroic lists them, each
/// once however many ways it was reached.
pub fn list(paths: &Paths, home: &Path) -> Vec<Tool> {
    let mut tools: Vec<Tool> = Vec::new();
    let mut add = |tool: Tool| {
        if !tools.iter().any(|had| had.bin == tool.bin) {
            tools.push(tool);
        }
    };
    let mut proton_folders = vec![paths.root().join("tools/proton")];
    let steam = steam_folder(paths, home);
    for library in steam_libraries(&steam) {
        if show_valve_proton(paths) {
            proton_folders.push(library.join("steam/steamapps/common"));
            proton_folders.push(library.join("steamapps/common"));
        }
        proton_folders.push(library.join("root/compatibilitytools.d"));
        proton_folders.push(library.join("compatibilitytools.d"));
    }
    for folder in proton_folders {
        for (name, dir) in listed(&folder) {
            if name.starts_with("UMU-Latest") {
                continue;
            }
            let bin = dir.join("proton");
            if bin.is_file() {
                add(Tool {
                    name,
                    bin: bin.to_string_lossy().into_owned(),
                    kind: ToolKind::Proton,
                });
            }
        }
    }
    for folder in [
        paths.root().join("tools/wine"),
        home.join(".local/share/lutris/runners/wine"),
    ] {
        for (name, dir) in listed(&folder) {
            let bin = dir.join("bin/wine");
            if bin.is_file() {
                add(Tool {
                    name,
                    bin: bin.to_string_lossy().into_owned(),
                    kind: ToolKind::Wine,
                });
            }
        }
    }
    tools
}

/// The folders under `folder`, by name, in name order so the list is the same
/// list every time.
fn listed(folder: &Path) -> Vec<(String, PathBuf)> {
    let Ok(listing) = std::fs::read_dir(folder) else {
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

/// The Steam folder Heroic's settings name — `~/.steam/steam` unless changed.
fn steam_folder(paths: &Paths, home: &Path) -> PathBuf {
    let named = defaults(paths)["defaultSteamPath"]
        .as_str()
        .map(|at| at.replace('\'', ""))
        .filter(|at| !at.is_empty())
        .unwrap_or_else(|| "~/.steam/steam".to_string());
    match named.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => PathBuf::from(named),
    }
}

/// Whether Heroic lists Valve's own Protons as well.
fn show_valve_proton(paths: &Paths) -> bool {
    defaults(paths)["showValveProton"].as_bool() == Some(true)
}

/// The libraries Steam's `libraryfolders.vdf` lists, where they are there.
fn steam_libraries(steam: &Path) -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string(steam.join("steamapps/libraryfolders.vdf")) else {
        return Vec::new();
    };
    library_paths(&text)
        .into_iter()
        .map(PathBuf::from)
        .filter(|at| at.is_dir())
        .collect()
}

/// Every `"path" "…"` in a `libraryfolders.vdf`.
fn library_paths(vdf: &str) -> Vec<String> {
    vdf.lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("\"path\"")?.trim();
            let quoted = rest.strip_prefix('"')?.strip_suffix('"')?;
            Some(quoted.replace("\\\\", "\\"))
        })
        .collect()
}

/// Heroic's `defaultSettings`, or nothing where it has none.
fn defaults(paths: &Paths) -> Value {
    std::fs::read_to_string(paths.config())
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .map(|config| config["defaultSettings"].clone())
        .unwrap_or(Value::Null)
}

/// The name of what Heroic runs games with by default.
pub fn default(paths: &Paths) -> Option<String> {
    defaults(paths)["wineVersion"]["name"]
        .as_str()
        .map(str::to_string)
}

/// The name of what one game runs with, where it has its own.
pub fn chosen(paths: &Paths, app_name: &str) -> Option<String> {
    let text = std::fs::read_to_string(paths.game_config(app_name)).ok()?;
    let config: Value = serde_json::from_str(&text).ok()?;
    config[app_name]["wineVersion"]["name"]
        .as_str()
        .map(str::to_string)
}

/// Heroic's `wineVersion` for a tool.
fn setting(tool: &Tool) -> Value {
    json!({
        "bin": tool.bin,
        "name": tool.name,
        "type": match tool.kind {
            ToolKind::Proton => "proton",
            ToolKind::Wine => "wine",
        },
    })
}

/// `lxb-heroic tools`: the list, the default, and every game's own.
pub fn tools(paths: &Paths, home: &Path) -> Tools {
    let games = std::fs::read_dir(paths.root().join("GamesConfig"))
        .map(|listing| {
            listing
                .filter_map(Result::ok)
                .filter_map(|entry| {
                    let file = entry.file_name().into_string().ok()?;
                    let app_name = file.strip_suffix(".json")?;
                    crate::heroic::plain(app_name).then_some(())?;
                    Some((app_name.to_string(), chosen(paths, app_name)?))
                })
                .collect()
        })
        .unwrap_or_default();
    Tools {
        protocol: PROTOCOL,
        tools: list(paths, home),
        default: default(paths),
        games,
    }
}

/// `lxb-heroic use-tool`: run one game with `name`, or with Heroic's default
/// where there is no name; or, with no game, make `name` Heroic's default.
///
/// Not while Heroic is open: it holds both files in memory and writes them
/// back as it likes, so a change made behind its back is one its next launch
/// does not see and its next save undoes.
pub fn use_tool(paths: &Paths, home: &Path, game: Option<&str>, name: Option<&str>) -> ToolSet {
    let answer = |reason: Option<Reason>, note: String| ToolSet {
        protocol: PROTOCOL,
        game: game.map(str::to_string),
        tool: name.map(str::to_string),
        reason,
        note,
    };
    if crate::heroic::in_the_way() {
        return answer(Some(Reason::HeroicRunning), "Heroic is open".into());
    }
    let tool = match name {
        Some(name) => match list(paths, home).into_iter().find(|tool| tool.name == name) {
            Some(tool) => Some(tool),
            None => {
                return answer(
                    Some(Reason::Legendary),
                    format!("Heroic cannot run anything called {name}"),
                )
            }
        },
        None => None,
    };
    let written = match (game, &tool) {
        (Some(app_name), tool) => write_for_game(paths, app_name, tool.as_ref()),
        (None, Some(tool)) => crate::proton::make_default_wine(paths, home, setting(tool)),
        (None, None) => Err("a default has to be something".to_string()),
    };
    match written {
        Ok(()) => answer(None, "written".into()),
        Err(why) => answer(Some(Reason::Legendary), why),
    }
}

/// Write a game's own `wineVersion`, or take it out, as Heroic writes that
/// file: `{"<app>": {…}, "version": "v0", "explicit": false}`, two-space
/// JSON, everything else in it kept.
fn write_for_game(paths: &Paths, app_name: &str, tool: Option<&Tool>) -> Result<(), String> {
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
    let own = object
        .entry(app_name)
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| format!("{app_name}'s settings are not an object"))?;
    match tool {
        Some(tool) => {
            own.insert("wineVersion".to_string(), setting(tool));
        }
        None => {
            own.remove("wineVersion");
        }
    }
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|err| format!("{}: {err}", parent.display()))?;
    }
    crate::proton::write_json(&file, &config, b"  ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heroic() -> (tempfile::TempDir, Paths, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::at(dir.path().join("heroic"));
        let home = dir.path().join("home");
        for proton in ["Proton-CachyOS-latest", "UMU-Latest"] {
            let at = paths.root().join("tools/proton").join(proton);
            std::fs::create_dir_all(&at).unwrap();
            std::fs::write(at.join("proton"), "").unwrap();
        }
        let wine = paths.root().join("tools/wine/wine-ge-8/bin");
        std::fs::create_dir_all(&wine).unwrap();
        std::fs::write(wine.join("wine"), "").unwrap();
        let steam = home.join(".steam/steam");
        let tool = steam.join("compatibilitytools.d/GE-Proton10-1");
        std::fs::create_dir_all(&tool).unwrap();
        std::fs::write(tool.join("proton"), "").unwrap();
        std::fs::create_dir_all(steam.join("compatibilitytools.d/Not a tool")).unwrap();
        std::fs::create_dir_all(steam.join("steamapps")).unwrap();
        std::fs::write(
            steam.join("steamapps/libraryfolders.vdf"),
            format!(
                "\"libraryfolders\"\n{{\n\t\"0\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t}}\n}}\n",
                steam.display()
            ),
        )
        .unwrap();
        std::fs::create_dir_all(paths.root()).unwrap();
        std::fs::write(
            paths.config(),
            r#"{"defaultSettings": {"wineVersion": {"name": "Proton-CachyOS-latest"}}}"#,
        )
        .unwrap();
        (dir, paths, home)
    }

    /// What Heroic would list: its own Protons, Steam's tools, its Wines —
    /// never UMU's own entry, nor a folder with no Proton in it.
    #[test]
    fn heroics_tools_are_listed_as_heroic_lists_them() {
        let (_dir, paths, home) = heroic();
        let names: Vec<(String, ToolKind)> = list(&paths, &home)
            .into_iter()
            .map(|tool| (tool.name, tool.kind))
            .collect();
        assert_eq!(
            names,
            [
                ("Proton-CachyOS-latest".to_string(), ToolKind::Proton),
                ("GE-Proton10-1".to_string(), ToolKind::Proton),
                ("wine-ge-8".to_string(), ToolKind::Wine),
            ]
        );
        assert_eq!(default(&paths).as_deref(), Some("Proton-CachyOS-latest"));
    }

    /// A game's own choice goes into its own file and comes out again, and
    /// nothing else of that file is touched.
    #[test]
    fn a_games_own_tool_is_written_and_taken_back() {
        let (_dir, paths, home) = heroic();
        std::fs::create_dir_all(paths.game_config("Quail").parent().unwrap()).unwrap();
        std::fs::write(
            paths.game_config("Quail"),
            r#"{"Quail": {"savesPath": "/saves"}, "version": "v0", "explicit": false}"#,
        )
        .unwrap();
        let tool = list(&paths, &home)
            .into_iter()
            .find(|tool| tool.name == "GE-Proton10-1")
            .unwrap();
        write_for_game(&paths, "Quail", Some(&tool)).unwrap();
        assert_eq!(chosen(&paths, "Quail").as_deref(), Some("GE-Proton10-1"));
        assert_eq!(
            tools(&paths, &home).games.get("Quail").map(String::as_str),
            Some("GE-Proton10-1"),
            "every game's own, in one answer"
        );
        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(paths.game_config("Quail")).unwrap())
                .unwrap();
        assert_eq!(written["Quail"]["savesPath"], "/saves");
        assert_eq!(written["Quail"]["wineVersion"]["type"], "proton");
        assert!(written["Quail"]["wineVersion"]["bin"]
            .as_str()
            .unwrap()
            .ends_with("GE-Proton10-1/proton"));
        write_for_game(&paths, "Quail", None).unwrap();
        assert_eq!(chosen(&paths, "Quail"), None);
        // A game Heroic has never written a file for gets one.
        write_for_game(&paths, "Owl", Some(&tool)).unwrap();
        assert_eq!(chosen(&paths, "Owl").as_deref(), Some("GE-Proton10-1"));
    }

    #[test]
    fn steams_libraries_are_read_from_its_own_list() {
        let vdf = "\"libraryfolders\"\n{\n\t\"0\"\n\t{\n\t\t\"path\"\t\t\"/home/me/.local/share/Steam\"\n\t\t\"label\"\t\t\"\"\n\t}\n\t\"1\"\n\t{\n\t\t\"path\"\t\t\"/mnt/games\"\n\t}\n}\n";
        assert_eq!(
            library_paths(vdf),
            ["/home/me/.local/share/Steam", "/mnt/games"]
        );
    }
}
