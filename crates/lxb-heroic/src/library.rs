//! What the Epic account holds, read off the disk.
//!
//! legendary keeps one file per title the account owns under `metadata/`,
//! refreshed by `legendary list`. Everything a column needs is in those files
//! and in three beside them — `installed.json`, Heroic's
//! `third-party-installed.json`, and Heroic's playtime store — so after a
//! refresh the library is read with no network at all, and a refresh that
//! fails leaves the last good list exactly where it was.
//!
//! ## Heroic's rules, copied rather than improved on
//!
//! What counts as a game is decided by `LegendaryLibraryManager.loadFile` in
//! Heroic, and it is copied here rule for rule so that the shell's column and
//! Heroic's own library never disagree about what somebody owns:
//!
//! * Unreal Engine content is not a game — namespace `ue`, or a category of
//!   `assets`, `asset-format`, `plugins` or `projects`;
//! * nor is a mod (category `mods`);
//! * nor is a title whose every release is for Android or iOS;
//! * nor is DLC, which is anything with a `mainGameItem`.
//!
//! On the account this was written against that is 230 files and 208 games:
//! nine DLC and twelve phone titles fall out, and so does Unreal Tournament,
//! whose one release names no platform at all — Heroic's test reads an empty
//! list of platforms as every one of them being a phone, and hides it too.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::process::Stdio;

use serde::Deserialize;
use serde_json::Value;

use crate::heroic::Paths;
use crate::report::{Game, Installation, Installed, Reason, Store};

/// Every game the account holds, alphabetical by title.
pub fn read(paths: &Paths) -> Vec<Game> {
    let installed = installed(paths);
    let latest = latest_builds(paths);
    let elsewhere = third_party_installed(paths);
    let played = read_json(&paths.timestamps()).unwrap_or(Value::Null);

    let Ok(listing) = std::fs::read_dir(paths.metadata()) else {
        return Vec::new();
    };
    let mut games: Vec<Game> = listing
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|at| at.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|at| {
            let text = std::fs::read_to_string(&at).ok()?;
            match serde_json::from_str::<Record>(&text) {
                Ok(record) => Some(record),
                Err(err) => {
                    eprintln!("library: {} is not one I can read: {err}", at.display());
                    None
                }
            }
        })
        .filter_map(|record| game(record, &installed, &latest, &elsewhere, &played))
        .collect();
    games.sort_by(|a, b| {
        a.title
            .to_lowercase()
            .cmp(&b.title.to_lowercase())
            .then_with(|| a.app_name.cmp(&b.app_name))
    });
    games
}

/// Ask Epic for the list again, through Heroic's legendary.
///
/// What it prints is not read: it rewrites the files [`read`] reads, which is
/// the whole of what is wanted from it.
pub fn refresh(installation: &Installation, paths: &Paths) -> Result<(), Reason> {
    let out = crate::heroic::output(
        crate::heroic::legendary(installation, paths)
            .args(["list", "--json", "--third-party"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped()),
    )
    .map_err(|err| {
        eprintln!("library: flatpak could not be run: {err}");
        Reason::NoHeroic
    })?;
    let said = String::from_utf8_lossy(&out.stderr);
    for line in said.lines() {
        eprintln!("legendary: {line}");
    }
    if out.status.success() {
        Ok(())
    } else {
        Err(why_legendary_failed(&said))
    }
}

/// What a legendary that did not do its job was stopped by, from what it said.
///
/// Only three answers are worth telling apart, because they are three
/// different things for a person to do: sign in again, check the connection,
/// or nothing at all.
pub fn why_legendary_failed(said: &str) -> Reason {
    let said = said.to_lowercase();
    // The connection first: with no network legendary's login is what fails,
    // and it says so — "HTTP request for login failed: ConnectionError…",
    // then "Login failed!" — which read the other way round is an account
    // signed out, and a sign-in offered to somebody who is only offline.
    if said.contains("connectionerror")
        || said.contains("max retries exceeded")
        || said.contains("failed to establish a new connection")
        || said.contains("name resolution")
        || said.contains("timed out")
    {
        Reason::Offline
    } else if said.contains("login failed")
        || said.contains("no saved credentials")
        || said.contains("invalid_grant")
        || said.contains("invalidcredentials")
    {
        Reason::SignedOut
    } else {
        Reason::Legendary
    }
}

/// One file under `metadata/`, as legendary writes it.
#[derive(Debug, Deserialize)]
struct Record {
    app_name: String,
    #[serde(default)]
    metadata: Metadata,
    /// The game's achievements as Epic defines them, which legendary keeps
    /// beside the store's record. Only the count is read here.
    #[serde(default)]
    achievements: Option<Defined>,
}

#[derive(Debug, Deserialize)]
struct Defined {
    total_achievements: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Metadata {
    #[serde(default)]
    title: String,
    developer: Option<String>,
    namespace: Option<String>,
    #[serde(default)]
    categories: Vec<Category>,
    #[serde(default)]
    release_info: Vec<Release>,
    main_game_item: Option<Value>,
    #[serde(default)]
    custom_attributes: BTreeMap<String, Attribute>,
    #[serde(default)]
    key_images: Vec<Picture>,
}

#[derive(Debug, Deserialize)]
struct Category {
    #[serde(default)]
    path: String,
}

#[derive(Debug, Deserialize)]
struct Release {
    platform: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct Attribute {
    value: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Picture {
    #[serde(rename = "type")]
    kind: String,
    url: String,
}

/// One entry of legendary's `installed.json`.
#[derive(Debug, Deserialize)]
struct OnDisk {
    install_path: Option<String>,
    #[serde(default)]
    install_size: u64,
    version: Option<String>,
    platform: Option<String>,
}

/// The newest build Epic lists for each game, per platform, out of
/// legendary's `assets.json`: `{"Windows": [{"app_name", "build_version"}, …]}`.
fn latest_builds(paths: &Paths) -> BTreeMap<(String, String), String> {
    let Some(Value::Object(platforms)) = read_json(&paths.legendary().join("assets.json")) else {
        return BTreeMap::new();
    };
    let mut latest = BTreeMap::new();
    for (platform, assets) in platforms {
        let Value::Array(assets) = assets else {
            continue;
        };
        for asset in assets {
            if let (Some(app), Some(build)) =
                (asset["app_name"].as_str(), asset["build_version"].as_str())
            {
                latest.insert((platform.clone(), app.to_string()), build.to_string());
            }
        }
    }
    latest
}

fn installed(paths: &Paths) -> BTreeMap<String, OnDisk> {
    read_json(&paths.installed())
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default()
}

/// Heroic's own list of EA and Ubisoft titles it has "installed":
/// `[[app_name, platform], …]`.
fn third_party_installed(paths: &Paths) -> HashSet<String> {
    let Some(Value::Array(pairs)) = read_json(&paths.third_party_installed()) else {
        return HashSet::new();
    };
    pairs
        .iter()
        .filter_map(|pair| pair.get(0)?.as_str().map(str::to_string))
        .collect()
}

fn read_json(at: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(at).ok()?).ok()
}

fn game(
    record: Record,
    installed: &BTreeMap<String, OnDisk>,
    latest: &BTreeMap<(String, String), String>,
    elsewhere: &HashSet<String>,
    played: &Value,
) -> Option<Game> {
    let metadata = record.metadata;

    let unreal = metadata.namespace.as_deref() == Some("ue")
        || metadata.categories.iter().any(|category| {
            matches!(
                category.path.as_str(),
                "assets" | "asset-format" | "plugins" | "projects"
            )
        });
    let modification = metadata
        .categories
        .iter()
        .any(|category| category.path == "mods");
    // Heroic's own test, including what it does with a title that has no
    // releases listed at all: `every` over nothing is true, so that is left
    // out too.
    let only_for_phones = metadata.release_info.iter().all(|release| {
        release.platform.as_ref().is_some_and(|platforms| {
            platforms
                .iter()
                .all(|platform| platform == "Android" || platform == "iOS")
        })
    });
    let dlc = metadata
        .main_game_item
        .as_ref()
        .is_some_and(|main| !main.is_null());
    if unreal || modification || only_for_phones || dlc {
        return None;
    }

    let attribute = |key: &str| {
        metadata
            .custom_attributes
            .get(key)
            .and_then(|attribute| attribute.value.clone())
            .filter(|value| !value.is_empty())
    };
    let picture = |kinds: &[&str]| {
        kinds.iter().find_map(|kind| {
            metadata
                .key_images
                .iter()
                .find(|picture| picture.kind == *kind)
                .map(|picture| picture.url.clone())
        })
    };

    let store = attribute("ThirdPartyManagedApp")
        .or_else(|| attribute("ThirdPartyManagedProvider"))
        .map(|store| match store.to_lowercase().as_str() {
            "origin" | "the ea app" => Store::Ea,
            "ubisoftconnect" => Store::Ubisoft,
            _ => Store::Other,
        });

    let installed = match installed.get(&record.app_name) {
        Some(on_disk) => Some(Installed {
            path: on_disk.install_path.clone(),
            size: on_disk.install_size,
            version: on_disk.version.clone(),
            // Heroic's own comparison, and only where both halves are known:
            // a game Epic no longer lists is not one with an update.
            update: on_disk.version.as_ref().is_some_and(|version| {
                let platform = on_disk.platform.as_deref().unwrap_or("Windows");
                latest
                    .get(&(platform.to_string(), record.app_name.clone()))
                    .is_some_and(|build| build != version)
            }),
        }),
        None if elsewhere.contains(&record.app_name) => Some(Installed {
            path: None,
            size: 0,
            version: None,
            update: false,
        }),
        None => None,
    };

    let times = &played[&record.app_name];
    let fetched = |kind| {
        let root = crate::art::cache()?;
        crate::art::cached(&root, &record.app_name, kind)
            .map(|at| at.to_string_lossy().into_owned())
    };
    let cover_file = fetched(crate::art::Kind::Cover);
    let hero_file = fetched(crate::art::Kind::Hero);
    let logo_file = fetched(crate::art::Kind::Logo);
    let wide = picture(&["DieselGameBox", "OfferImageWide"]);
    Some(Game {
        title: if metadata.title.trim().is_empty() {
            record.app_name.clone()
        } else {
            metadata.title.trim().to_string()
        },
        developer: metadata
            .developer
            .clone()
            .filter(|name| !name.trim().is_empty()),
        // Heroic's `art_square`: the tall box, else the store front, else the
        // wide box rather than nothing.
        cover: picture(&[
            "DieselGameBoxTall",
            "OfferImageTall",
            "DieselStoreFrontTall",
        ])
        .or_else(|| wide.clone()),
        hero: wide,
        logo: picture(&["DieselGameBoxLogo"]),
        cover_file,
        hero_file,
        logo_file,
        installed,
        store,
        offline: attribute("CanRunOffline").as_deref() == Some("true"),
        cloud_saves: attribute("CloudSaveFolder").is_some(),
        played_minutes: times["totalPlayed"].as_u64().unwrap_or(0),
        last_played: times["lastPlayed"].as_str().map(str::to_string),
        achievements: record
            .achievements
            .map(|defined| defined.total_achievements.unwrap_or(0)),
        app_name: record.app_name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn title(app: &str, title: &str, extra: Value) -> Value {
        let mut metadata = json!({
            "title": title,
            "developer": "Somebody",
            "namespace": app,
            "categories": [{"path": "games"}, {"path": "applications"}],
            "releaseInfo": [{"appId": app, "platform": ["Windows"]}],
            "customAttributes": {"FolderName": {"type": "STRING", "value": app}},
            "keyImages": [
                {"type": "DieselGameBox", "url": format!("https://img/{app}/wide.jpg")},
                {"type": "DieselGameBoxTall", "url": format!("https://img/{app}/tall.jpg")},
                {"type": "DieselGameBoxLogo", "url": format!("https://img/{app}/logo.png")}
            ]
        });
        if let (Some(into), Value::Object(extra)) = (metadata.as_object_mut(), extra) {
            into.extend(extra);
        }
        json!({"app_name": app, "app_title": title, "metadata": metadata})
    }

    fn library(titles: &[Value]) -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::at(dir.path().to_path_buf());
        std::fs::create_dir_all(paths.metadata()).unwrap();
        for record in titles {
            let app = record["app_name"].as_str().unwrap();
            std::fs::write(
                paths.metadata().join(format!("{app}.json")),
                record.to_string(),
            )
            .unwrap();
        }
        (dir, paths)
    }

    /// How many achievements Epic defines is read from beside the store's
    /// record: a number where legendary wrote one, nought where it wrote
    /// none, and nothing where it has not said.
    #[test]
    fn a_games_achievements_are_counted_from_what_legendary_kept() {
        let mut some = title("Some", "Some", json!({}));
        some["achievements"] =
            json!({"total_achievements": 12, "achievements": [{"achievement": {"name": "a"}}]});
        let mut none = title("None", "None", json!({}));
        none["achievements"] = json!({"total_achievements": 0, "achievements": []});
        let mut null = title("Null", "Null", json!({}));
        null["achievements"] = Value::Null;
        let (_dir, paths) = library(&[some, none, null, title("Unsaid", "Unsaid", json!({}))]);
        let count = |app: &str| {
            read(&paths)
                .into_iter()
                .find(|game| game.app_name == app)
                .unwrap()
                .achievements
        };
        assert_eq!(count("Some"), Some(12));
        assert_eq!(count("None"), Some(0));
        assert_eq!(count("Null"), None);
        assert_eq!(count("Unsaid"), None);
    }

    #[test]
    fn heroics_rules_decide_what_is_a_game() {
        let (_dir, paths) = library(&[
            title("Game", "A Game", json!({})),
            title(
                "Addon",
                "A Game: Soundtrack",
                json!({"mainGameItem": {"id": "x"}}),
            ),
            title(
                "Engine",
                "Some Marketplace Pack",
                json!({"namespace": "ue"}),
            ),
            title(
                "Asset",
                "Some Asset",
                json!({"categories": [{"path": "assets"}]}),
            ),
            title("Mod", "Some Mod", json!({"categories": [{"path": "mods"}]})),
            title(
                "Phone",
                "A Phone Game",
                json!({"releaseInfo": [{"platform": ["Android", "iOS"]}]}),
            ),
            title("Nothing", "No Releases", json!({"releaseInfo": []})),
            title(
                "Both",
                "Phone And Windows",
                json!({"releaseInfo": [{"platform": ["Android"]}, {"platform": ["Windows"]}]}),
            ),
            title(
                "Unsaid",
                "No Platform Named",
                json!({"releaseInfo": [{"appId": "Unsaid"}]}),
            ),
            // Unreal Tournament's own shape: one release, an empty list.
            title(
                "Empty",
                "Unreal Something",
                json!({"releaseInfo": [{"appId": "Empty", "platform": []}]}),
            ),
        ]);
        let names: Vec<String> = read(&paths).into_iter().map(|game| game.app_name).collect();
        assert_eq!(names, ["Game", "Unsaid", "Both"]);
    }

    #[test]
    fn a_game_carries_its_pictures_its_store_and_whether_it_is_here() {
        let (_dir, paths) = library(&[
            title("Here", "Here", json!({})),
            title(
                "Ubi",
                "Watch Somebody",
                json!({"customAttributes": {"ThirdPartyManagedApp": {"value": "UbisoftConnect"}}}),
            ),
            title(
                "Ea",
                "Battle Something",
                json!({"customAttributes": {"ThirdPartyManagedProvider": {"value": "Origin"}}}),
            ),
            title(
                "Plain",
                "Plain",
                json!({
                    "keyImages": [{"type": "DieselGameBox", "url": "https://img/wide-only.jpg"}],
                    "customAttributes": {
                        "CanRunOffline": {"value": "true"},
                        "CloudSaveFolder": {"value": "{AppData}/Plain"}
                    }
                }),
            ),
        ]);
        std::fs::write(
            paths.installed(),
            json!({"Here": {"app_name": "Here", "install_path": "/games/Here",
                            "install_size": 377925421u64, "version": "1.4.1",
                            "executable": "Here.exe", "platform": "Windows"}})
            .to_string(),
        )
        .unwrap();
        std::fs::write(paths.third_party_installed(), r#"[["Ubi","Windows"]]"#).unwrap();
        std::fs::create_dir_all(paths.timestamps().parent().unwrap()).unwrap();
        std::fs::write(
            paths.timestamps(),
            json!({"Here": {"firstPlayed": "2026-09-24T21:29:47.407Z",
                            "lastPlayed": "2026-09-24T21:30:40.979Z", "totalPlayed": 42}})
            .to_string(),
        )
        .unwrap();
        // Epic lists a newer build than the one on the disk.
        std::fs::write(
            paths.legendary().join("assets.json"),
            json!({"Windows": [{"app_name": "Here", "build_version": "1.4.2"},
                               {"app_name": "Plain", "build_version": "1"}]})
            .to_string(),
        )
        .unwrap();

        let games = read(&paths);
        let by = |app: &str| {
            games
                .iter()
                .find(|game| game.app_name == app)
                .unwrap()
                .clone()
        };

        let here = by("Here");
        assert_eq!(here.cover.as_deref(), Some("https://img/Here/tall.jpg"));
        assert_eq!(here.hero.as_deref(), Some("https://img/Here/wide.jpg"));
        assert_eq!(here.logo.as_deref(), Some("https://img/Here/logo.png"));
        assert_eq!(
            here.installed,
            Some(Installed {
                path: Some("/games/Here".into()),
                size: 377925421,
                version: Some("1.4.1".into()),
                update: true,
            }),
            "1.4.1 on the disk, 1.4.2 on Epic"
        );
        assert_eq!(here.played_minutes, 42);
        assert_eq!(
            here.last_played.as_deref(),
            Some("2026-09-24T21:30:40.979Z")
        );
        assert_eq!(here.store, None);

        let ubi = by("Ubi");
        assert_eq!(ubi.store, Some(Store::Ubisoft));
        assert_eq!(
            ubi.installed,
            Some(Installed {
                path: None,
                size: 0,
                version: None,
                update: false,
            }),
            "Heroic marks a third-party title installed with nothing on the disk"
        );
        assert_eq!(by("Ea").store, Some(Store::Ea));
        assert_eq!(by("Ea").installed, None);

        let plain = by("Plain");
        assert_eq!(plain.cover.as_deref(), Some("https://img/wide-only.jpg"));
        assert!(plain.offline);
        assert!(plain.cloud_saves);
        assert!(!here.offline);
    }

    #[test]
    fn the_list_is_alphabetical_whatever_the_files_are_called() {
        let (_dir, paths) = library(&[
            title("z1", "abzu", json!({})),
            title("a1", "Zeta", json!({})),
            title("m1", "Absolute", json!({})),
        ]);
        let titles: Vec<String> = read(&paths).into_iter().map(|game| game.title).collect();
        assert_eq!(titles, ["Absolute", "abzu", "Zeta"]);
    }

    /// A file legendary was halfway through writing, or one from a version
    /// that shaped it differently, costs that title — never the library.
    #[test]
    fn an_unreadable_file_costs_one_title() {
        let (_dir, paths) = library(&[title("Good", "Good", json!({}))]);
        std::fs::write(paths.metadata().join("Broken.json"), "{\"app_name\": ").unwrap();
        std::fs::write(paths.metadata().join("notes.txt"), "not json").unwrap();
        assert_eq!(read(&paths).len(), 1);
    }

    #[test]
    fn no_library_on_the_disk_is_an_empty_one() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read(&Paths::at(dir.path().to_path_buf())).is_empty());
    }

    #[test]
    fn what_stopped_legendary_is_told_apart() {
        assert_eq!(
            why_legendary_failed("[cli] ERROR: Login failed, cannot continue!"),
            Reason::SignedOut
        );
        assert_eq!(
            why_legendary_failed(
                "requests.exceptions.ConnectionError: HTTPSConnectionPool(host=…): Max retries exceeded"
            ),
            Reason::Offline
        );
        assert_eq!(
            why_legendary_failed("Traceback: KeyError"),
            Reason::Legendary
        );
        // With no network it is the login that fails, and says so.
        assert_eq!(
            why_legendary_failed(
                "[Core] ERROR: HTTP request for login failed: ConnectionError(MaxRetryError(\"HTTPSConnectionPool(host='account-public-service-prod03.ol.epicgames.com', port=443): Max retries exceeded\")), please try again later.\n[cli] ERROR: Login failed! Unable to check for EULAs."
            ),
            Reason::Offline
        );
        assert_eq!(
            why_legendary_failed(
                "[Core] ERROR: Stored credentials are no longer valid! Please login again.\n[cli] ERROR: Login failed! Unable to check for EULAs."
            ),
            Reason::SignedOut
        );
    }
}
