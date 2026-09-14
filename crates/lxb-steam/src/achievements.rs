//! Read-only Steam trophies. Progress is scoped to an account; artwork is public.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
pub use steam_cm_protocol::ProtocolAchievement as Achievement;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub achievements: Vec<Achievement>,
    pub percentages: BTreeMap<String, f64>,
    pub fetched_at: u64,
    #[serde(skip)]
    pub stale: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Heard {
    pub generation: u64,
    pub account: u64,
    pub app_id: u32,
    pub request: u64,
    pub result: Result<Snapshot, String>,
}

pub fn cache_root() -> Option<PathBuf> {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join(".cache"))
                .filter(|p| p.is_absolute())
        })
        .map(|p| p.join("linexinbar/steam-achievements"))
}

pub(crate) fn finish(
    account: u64,
    app_id: u32,
    result: Result<Vec<Achievement>, String>,
) -> Result<Snapshot, String> {
    let path = cache_root().map(|p| p.join(account.to_string()).join(format!("{app_id}.json")));
    let cached = || {
        path.as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice::<Snapshot>(&b).ok())
    };
    let mut snapshot = match result {
        Ok(achievements) => Snapshot {
            percentages: if achievements.is_empty() {
                BTreeMap::new()
            } else {
                percentages(app_id)
                    .or_else(|| cached().map(|s| s.percentages))
                    .unwrap_or_default()
            },
            achievements,
            fetched_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            stale: None,
        },
        Err(reason) => {
            let mut cached = cached().ok_or_else(|| reason.clone())?;
            cached.stale = Some(reason);
            return Ok(cached);
        }
    };
    sort(&mut snapshot);
    if let Some(path) = path {
        if let (Some(parent), Ok(bytes)) = (path.parent(), serde_json::to_vec(&snapshot)) {
            if std::fs::create_dir_all(parent).is_ok() {
                use std::os::unix::fs::PermissionsExt;
                use std::sync::atomic::{AtomicU64, Ordering};
                static NEXT: AtomicU64 = AtomicU64::new(0);
                let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
                let temporary = path.with_extension(format!(
                    "{}-{}.tmp",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                if std::fs::write(&temporary, bytes).is_ok() {
                    let _ = std::fs::rename(temporary, path);
                }
            }
        }
    }
    Ok(snapshot)
}

/// Steam's personal list: recent unlocks first, remaining achievements by
/// global completion (common first), followed by concealed achievements.
/// Verified against the installed Steam `steamui/chunk~2dcc5aaf7.js`, function Z,
/// on 2026-09-14. User-defined drag ordering in Steam is not part of the default.
/// Schema position makes equal values stable.
pub fn sort(snapshot: &mut Snapshot) {
    let percentages = &snapshot.percentages;
    snapshot.achievements.sort_by(|a, b| {
        b.achieved
            .cmp(&a.achieved)
            .then_with(|| {
                if a.achieved {
                    b.unlocktime.cmp(&a.unlocktime)
                } else {
                    let hidden = a.hidden.cmp(&b.hidden);
                    if !hidden.is_eq() {
                        return hidden;
                    }
                    let a = percentages.get(&a.apiname).copied().unwrap_or(-1.0);
                    let b = percentages.get(&b.apiname).copied().unwrap_or(-1.0);
                    b.total_cmp(&a)
                }
            })
            .then(a.schema_order.cmp(&b.schema_order))
    });
}

pub(crate) fn failure(error: &steam_cm_protocol::error::Error) -> String {
    use steam_cm_protocol::error::Error;
    match error {
        Error::Refused { result: 84, .. } => {
            "Steam is limiting achievement requests. Try again shortly.".into()
        }
        Error::Refused { result, .. } => {
            format!("Steam could not provide achievements for this game (code {result}).")
        }
        Error::MissingField(_) | Error::InvalidResponse(_) | Error::InvalidPacket(_) => {
            "Steam returned incomplete achievement data.".into()
        }
        _ => "Steam could not load achievements. Check the connection and try again.".into(),
    }
}

fn percentages(app_id: u32) -> Option<BTreeMap<String, f64>> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(10)))
        .build()
        .into();
    let bytes = agent.get(format!("https://api.steampowered.com/ISteamUserStats/GetGlobalAchievementPercentagesForApp/v2/?gameid={app_id}"))
        .call().ok()?.into_body().read_to_vec().ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    Some(
        value
            .get("achievementpercentages")?
            .get("achievements")?
            .as_array()?
            .iter()
            .filter_map(|v| {
                let percent = v.get("percent")?;
                let percent = percent
                    .as_f64()
                    .or_else(|| percent.as_str()?.parse().ok())?;
                (percent.is_finite() && (0.0..=100.0).contains(&percent))
                    .then_some((v.get("name")?.as_str()?.to_owned(), percent))
            })
            .collect(),
    )
}

/// Schema icons are filenames under this app's Steam Community image directory.
/// Use the current community_assets path: the legacy steamcommunity/public path
/// returns 404 for newer games such as ReStory even with the correct icon hash.
/// Never interpret a schema field as an arbitrary URL or filesystem path.
pub fn icon_url(app_id: u32, icon: &str) -> Option<String> {
    (!icon.is_empty()
        && icon.len() <= 160
        && !icon.contains("..")
        && icon
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.'))
    .then(|| format!("https://shared.steamstatic.com/community_assets/images/apps/{app_id}/{icon}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn achievement(id: &str, achieved: bool, time: u64, hidden: bool, order: u32) -> Achievement {
        Achievement {
            apiname: id.into(),
            achieved,
            unlocktime: time,
            name: None,
            description: None,
            icon: Some("a.jpg".into()),
            icon_gray: Some("b.jpg".into()),
            hidden,
            schema_order: order,
        }
    }
    #[test]
    fn personal_order_matches_steam_default_and_keeps_ties_stable() {
        let mut snapshot = Snapshot {
            achievements: vec![
                achievement("hidden", false, 0, true, 0),
                achievement("rare", false, 0, false, 1),
                achievement("old", true, 100, false, 2),
                achievement("common", false, 0, false, 3),
                achievement("new", true, 200, false, 4),
                achievement("tie", true, 200, false, 5),
                achievement("undated", true, 0, false, 6),
            ],
            percentages: [
                ("hidden".into(), 99.0),
                ("rare".into(), 1.0),
                ("common".into(), 80.0),
            ]
            .into(),
            fetched_at: 0,
            stale: None,
        };
        sort(&mut snapshot);
        assert_eq!(
            snapshot
                .achievements
                .iter()
                .map(|a| a.apiname.as_str())
                .collect::<Vec<_>>(),
            ["new", "tie", "old", "undated", "common", "rare", "hidden"]
        );
    }
    #[test]
    fn achievement_icons_use_the_current_steam_assets_path() {
        // Real ReStory schema filenames. Both are missing at the legacy path.
        for icon in [
            "4a104b6a4cef869d0909da98cae83fb9a9c17c09.jpg",
            "0489321b5e7c3f7d88b21fba084fcb4df0067b18.jpg",
        ] {
            assert_eq!(
                icon_url(3812600, icon).unwrap(),
                format!(
                    "https://shared.steamstatic.com/community_assets/images/apps/3812600/{icon}"
                )
            );
        }
    }

    #[test]
    fn schema_icon_cannot_escape_its_app_directory() {
        assert!(icon_url(400, "abc_123-gray.jpg")
            .unwrap()
            .contains("/400/abc_123-gray.jpg"));
        for invalid in ["", "../x", "/x", "https://example.org/x", "a?x", "a\\x"] {
            assert!(icon_url(400, invalid).is_none(), "{invalid}");
        }
    }
    #[test]
    fn cached_progress_never_crosses_accounts_or_apps() {
        let _guard = crate::one_at_a_time_with_the_environment();
        let old = std::env::var_os("XDG_CACHE_HOME");
        let scratch =
            std::env::temp_dir().join(format!("lxb-trophies-test-{}", std::process::id()));
        std::env::set_var("XDG_CACHE_HOME", &scratch);
        let path = cache_root().unwrap().join("1/400.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let snapshot = Snapshot {
            achievements: vec![achievement("one", true, 5, false, 0)],
            percentages: BTreeMap::new(),
            fetched_at: 2,
            stale: None,
        };
        std::fs::write(path, serde_json::to_vec(&snapshot).unwrap()).unwrap();
        assert!(finish(1, 400, Err("offline".into()))
            .unwrap()
            .stale
            .is_some());
        assert!(finish(2, 400, Err("offline".into())).is_err());
        assert!(finish(1, 401, Err("offline".into())).is_err());
        save_progress(
            1,
            &[
                (
                    400,
                    Progress {
                        unlocked: 0,
                        total: 1,
                        fetched_at: 1,
                    },
                ),
                (
                    620,
                    Progress {
                        unlocked: 7,
                        total: 51,
                        fetched_at: 3,
                    },
                ),
            ]
            .into(),
        );
        let (progress, pages) = saved(1);
        assert_eq!(progress[&400].unlocked, 1, "newer per-game snapshot wins");
        assert_eq!(
            progress[&620].unlocked, 7,
            "unopened games restore summary counts"
        );
        assert_eq!(pages.len(), 1);
        assert!(saved(2).0.is_empty());
        save_progress(
            1,
            &[(
                620,
                Progress {
                    unlocked: 8,
                    total: 51,
                    fetched_at: 4,
                },
            )]
            .into(),
        );
        assert_eq!(saved(1).0[&620].unlocked, 8);
        match old {
            Some(value) => std::env::set_var("XDG_CACHE_HOME", value),
            None => std::env::remove_var("XDG_CACHE_HOME"),
        }
        let _ = std::fs::remove_dir_all(scratch);
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct Progress {
    pub unlocked: u32,
    pub total: u32,
    pub fetched_at: u64,
}
#[derive(Debug, Clone)]
pub struct ProgressHeard {
    pub generation: u64,
    pub account: u64,
    pub request: u64,
    pub result: Result<BTreeMap<u32, Progress>, String>,
}

pub fn saved(account: u64) -> (BTreeMap<u32, Progress>, BTreeMap<u32, Snapshot>) {
    let Some(root) = cache_root().map(|r| r.join(account.to_string())) else {
        return Default::default();
    };
    let mut progress: BTreeMap<u32, Progress> = std::fs::read(root.join("progress.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let mut snapshots = BTreeMap::new();
    if let Ok(files) = std::fs::read_dir(root) {
        for file in files.flatten() {
            let path = file.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let Some(app) = path
                .file_stem()
                .and_then(|s| s.to_str())
                .and_then(|s| s.parse::<u32>().ok())
            else {
                continue;
            };
            if let Some(snapshot) = std::fs::read(path)
                .ok()
                .and_then(|b| serde_json::from_slice::<Snapshot>(&b).ok())
            {
                let summary = Progress {
                    total: snapshot.achievements.len() as u32,
                    unlocked: snapshot.achievements.iter().filter(|a| a.achieved).count() as u32,
                    fetched_at: snapshot.fetched_at,
                };
                if progress
                    .get(&app)
                    .is_none_or(|old| old.fetched_at < summary.fetched_at)
                {
                    progress.insert(app, summary);
                }
                snapshots.insert(app, snapshot);
            }
        }
    }
    (progress, snapshots)
}

pub(crate) fn save_progress(account: u64, fresh: &BTreeMap<u32, Progress>) {
    let Some(root) = cache_root().map(|r| r.join(account.to_string())) else {
        return;
    };
    let mut all: BTreeMap<u32, Progress> = std::fs::read(root.join("progress.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    all.extend(fresh.clone());
    if std::fs::create_dir_all(&root).is_ok() {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700));
        if let Ok(bytes) = serde_json::to_vec(&all) {
            let temp = root.join(format!("progress-{}.tmp", std::process::id()));
            if std::fs::write(&temp, bytes).is_ok() {
                let _ = std::fs::rename(temp, root.join("progress.json"));
            }
        }
    }
}
