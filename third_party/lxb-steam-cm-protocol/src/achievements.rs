use crate::{
    connection::{Connection, ConnectionState},
    error::{Error, Result},
    friends::ProtocolAchievement,
    kv::{self, KVValue},
};
use std::collections::HashMap;

/// Reads the account's stats without changing games-played or presence.
pub async fn get_player_achievements(
    connection: &Connection,
    state: &ConnectionState,
    appid: u32,
) -> Result<Vec<ProtocolAchievement>> {
    use crate::protobuf::{CPlayerGetUserStatsRequest, CPlayerGetUserStatsResponse};
    use crate::service_method::{ServiceMethod, call_authed};
    let request = CPlayerGetUserStatsRequest {
        steamid: Some(state.steamid.ok_or(Error::MissingField("steamid"))?),
        appid: Some(appid),
        ..Default::default()
    };
    let response: Result<CPlayerGetUserStatsResponse> = call_authed(
        connection,
        state,
        &ServiceMethod::new("Player.GetUserStats#1"),
        &request,
    )
    .await;
    let response = match response {
        Ok(response) => response,
        Err(error @ Error::Refused { result: 2, .. }) => {
            // Fail is ambiguous. The same progress service Steam's library
            // uses can positively identify a title with no achievements.
            // A missing row or a refusal must never be mistaken for zero.
            if let Ok(progress) = get_progress(connection, state, vec![appid]).await {
                if progress
                    .iter()
                    .any(|p| p.appid == Some(appid) && p.total == Some(0))
                {
                    return Ok(Vec::new());
                }
            }
            return Err(error);
        }
        Err(error) => return Err(error),
    };
    let schema = response
        .schema
        .as_deref()
        .filter(|bytes| !bytes.is_empty())
        .ok_or(Error::MissingField("achievement schema"))?;
    build_player_achievements(schema, &response.stats)
}

fn build_player_achievements(
    schema: &[u8],
    values: &[crate::protobuf::c_player_get_user_stats_response::Stats],
) -> Result<Vec<ProtocolAchievement>> {
    let defs = parse_achievement_schema(schema)?;
    let stats: HashMap<_, _> = values
        .iter()
        .filter_map(|stat| stat.stat_id.map(|id| (id, stat)))
        .collect();
    Ok(defs
        .into_iter()
        .map(|def| {
            let stat = stats.get(&def.stat_id);
            let achieved = stat
                .and_then(|s| s.stat_value)
                .is_some_and(|bits| def.bit < 32 && bits & (1u32 << def.bit) != 0);
            let time = stat
                .and_then(|s| {
                    s.unlock_times
                        .iter()
                        .find(|t| t.achievement_bit == Some(def.bit))
                })
                .and_then(|t| t.unlock_time)
                .unwrap_or(0) as u64;
            achievement(def, achieved, time)
        })
        .collect())
}

fn achievement(def: AchievementDef, achieved: bool, unlocktime: u64) -> ProtocolAchievement {
    ProtocolAchievement {
        apiname: def.internal_name,
        achieved,
        unlocktime,
        name: def.display_name,
        description: def.description,
        icon: def.icon,
        icon_gray: def.icon_gray,
        hidden: def.hidden,
        schema_order: def.schema_order,
    }
}

struct AchievementDef {
    stat_id: u32,
    bit: u32,
    internal_name: String,
    display_name: Option<String>,
    description: Option<String>,
    icon: Option<String>,
    icon_gray: Option<String>,
    hidden: bool,
    schema_order: u32,
}

/// The display "name" and "desc" fields are language-keyed nested blocks:
///   display { name { english "First Blood" french "Premier Sang" } }
/// Fall back to plain-string form in case the game uses a simpler schema.
fn get_localized_string<'a>(node: &'a KVValue, key: &str) -> Option<&'a str> {
    let field = node.get(key)?;
    // Plain string (uncommon but guard against it)
    if let Some(s) = field.as_str() {
        return if s.is_empty() { None } else { Some(s) };
    }
    // Language-keyed nested node
    if field.as_nested().is_some() {
        // Prefer "english", then fall back to the first non-empty string child
        if let Some(eng) = field.get("english").and_then(|v| v.as_str())
            && !eng.is_empty()
        {
            return Some(eng);
        }
        if let Some(children) = field.as_nested() {
            for (_, v) in children {
                if let Some(s) = v.as_str()
                    && !s.is_empty()
                {
                    return Some(s);
                }
            }
        }
    }
    None
}

/// Walk the parsed KV tree and extract achievement definitions.
/// Achievement groups carry `bits`; real schemas use several different `type` representations.
fn extract_achievements(root: &KVValue) -> Vec<AchievementDef> {
    let mut defs = Vec::new();

    // Try root → "stats" first (standard layout)
    let stats_node = if let Some(s) = root.get("stats") {
        s
    } else if let Some(nested) = root.as_nested() {
        // Some schemas wrap the root in an extra level; look one level down.
        let mut found = None;
        for (_, v) in nested {
            if let Some(s) = v.get("stats") {
                found = Some(s);
                break;
            }
        }
        match found {
            Some(s) => s,
            None => return defs,
        }
    } else {
        return defs;
    };

    let stat_entries = match stats_node.as_nested() {
        Some(e) => e,
        None => return defs,
    };

    for (stat_key, stat_value) in stat_entries {
        let stat_id: u32 = match stat_key.parse() {
            Ok(id) => id,
            Err(_) => continue,
        };

        // Achievement stats are exactly those carrying a `bits` block (each bit = one
        // achievement). The schema's `type` field is an unreliable discriminator — real schemas
        // store it as a word ("INT", "FLOAT", …), not the numeric "4" — so the presence of `bits`
        // is the gate.
        let bits_node = match stat_value.get("bits").and_then(|b| b.as_nested()) {
            Some(b) => b,
            None => continue,
        };

        for (bit_key, bit_value) in bits_node {
            let bit: u32 = match bit_key.parse() {
                Ok(b) => b,
                Err(_) => continue,
            };

            let internal_name = match bit_value.get("name").and_then(|n| n.as_str()) {
                Some(n) if !n.is_empty() => n.to_owned(),
                _ => continue,
            };

            let display = bit_value.get("display");
            let display_name = display
                .and_then(|d| get_localized_string(d, "name"))
                .map(|s| s.to_owned());
            let description = display
                .and_then(|d| get_localized_string(d, "desc"))
                .map(|s| s.to_owned());

            defs.push(AchievementDef {
                stat_id,
                bit,
                internal_name,
                display_name,
                description,
                icon: display
                    .and_then(|d| d.get("icon"))
                    .and_then(KVValue::as_str)
                    .map(str::to_owned),
                icon_gray: display
                    .and_then(|d| d.get("icon_gray"))
                    .and_then(KVValue::as_str)
                    .map(str::to_owned),
                hidden: display
                    .and_then(|d| d.get("hidden"))
                    .and_then(KVValue::as_u32)
                    .unwrap_or(0)
                    != 0,
                schema_order: defs.len() as u32,
            });
        }
    }

    defs
}

fn parse_achievement_schema(data: &[u8]) -> Result<Vec<AchievementDef>> {
    let root = kv::parse_binary_kv(data)
        .ok_or_else(|| Error::Transport("achievement schema binary KV parse failed".to_owned()))?;
    let has_stats = root.get("stats").and_then(KVValue::as_nested).is_some()
        || root.as_nested().is_some_and(|children| {
            children
                .iter()
                .any(|(_, v)| v.get("stats").and_then(KVValue::as_nested).is_some())
        });
    if !has_stats {
        return Err(Error::InvalidResponse(
            "achievement schema has no stats block",
        ));
    }
    Ok(extract_achievements(&root))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal binary-KV achievement schema blob matching the layout `kv.rs` parses:
    /// type bytes 0x00 = nested, 0x01 = string; each child block terminated by 0x08.
    ///
    /// root {
    ///   stats {
    ///     "0" {
    ///       type "4"
    ///       bits {
    ///         "0" { name "ACH_FIRST" display { name { english "First!" } } }
    ///         "1" { name "ACH_SECOND" }
    ///       }
    ///     }
    ///   }
    /// }
    fn synthetic_schema() -> Vec<u8> {
        let mut data = Vec::new();
        // root (nested, empty key)
        data.push(0x00);
        data.push(0x00);
        // stats (nested)
        data.push(0x00);
        data.extend_from_slice(b"stats\0");
        {
            // stats/"0" (nested)
            data.push(0x00);
            data.extend_from_slice(b"0\0");
            {
                // type = "4" (string)
                data.push(0x01);
                data.extend_from_slice(b"type\0");
                data.extend_from_slice(b"4\0");
                // bits (nested)
                data.push(0x00);
                data.extend_from_slice(b"bits\0");
                {
                    // bits/"0" (nested)
                    data.push(0x00);
                    data.extend_from_slice(b"0\0");
                    {
                        // name = "ACH_FIRST"
                        data.push(0x01);
                        data.extend_from_slice(b"name\0");
                        data.extend_from_slice(b"ACH_FIRST\0");
                        // display (nested)
                        data.push(0x00);
                        data.extend_from_slice(b"display\0");
                        {
                            // display/name (nested)
                            data.push(0x00);
                            data.extend_from_slice(b"name\0");
                            {
                                // display/name/english = "First!"
                                data.push(0x01);
                                data.extend_from_slice(b"english\0");
                                data.extend_from_slice(b"First!\0");
                            }
                            data.push(0x08); // end display/name
                        }
                        data.push(0x08); // end display
                    }
                    data.push(0x08); // end bits/"0"
                    // bits/"1" (nested)
                    data.push(0x00);
                    data.extend_from_slice(b"1\0");
                    {
                        // name = "ACH_SECOND"
                        data.push(0x01);
                        data.extend_from_slice(b"name\0");
                        data.extend_from_slice(b"ACH_SECOND\0");
                    }
                    data.push(0x08); // end bits/"1"
                }
                data.push(0x08); // end bits
            }
            data.push(0x08); // end stats/"0"
        }
        data.push(0x08); // end stats
        data.push(0x08); // end root
        data
    }

    #[test]
    fn player_stats_join_schema_and_sparse_unlock_times() {
        let schema = synthetic_schema();
        let blocks = vec![crate::protobuf::c_player_get_user_stats_response::Stats {
            stat_id: Some(0),
            stat_value: Some(1),
            unlock_times: vec![
                crate::protobuf::c_player_get_user_stats_response::UnlockTime {
                    achievement_bit: Some(0),
                    unlock_time: Some(1_700_000_000),
                },
            ],
        }];

        let mut achievements = build_player_achievements(&schema, &blocks).unwrap();
        achievements.sort_by(|a, b| a.apiname.cmp(&b.apiname));

        assert_eq!(achievements.len(), 2);

        let first = &achievements[0];
        assert_eq!(first.apiname, "ACH_FIRST");
        assert!(first.achieved);
        assert_eq!(first.unlocktime, 1_700_000_000);
        assert_eq!(first.name.as_deref(), Some("First!"));

        let second = &achievements[1];
        assert_eq!(second.apiname, "ACH_SECOND");
        assert!(!second.achieved);
        assert_eq!(second.unlocktime, 0);
    }

    /// Regression test against a real schema captured live from appid 410110
    /// ("12 is Better Than 6"). Guards the parser against the real Steam binary-KV layout —
    /// notably that a stat's `type` is a word ("INT"/"FLOAT"/…), so achievement stats must be
    /// recognised by the presence of a `bits` block, not by `type == 4`.
    #[test]
    fn parses_real_captured_schema() {
        let schema = include_bytes!("../tests/fixtures/userstats_schema_410110.bin");
        // No unlock blocks supplied, so every achievement parses as locked — but all 46
        // definitions must still be extracted with api names (and mostly display names).
        let achievements = build_player_achievements(schema, &[]).unwrap();
        assert_eq!(
            achievements.len(),
            46,
            "expected 46 achievement definitions"
        );
        assert!(
            achievements
                .iter()
                .all(|a| !a.achieved && a.unlocktime == 0)
        );
        assert!(achievements.iter().all(|a| !a.apiname.is_empty()));
        let named = achievements.iter().filter(|a| a.name.is_some()).count();
        assert!(
            named >= 40,
            "expected most achievements to have display names, got {named}"
        );
    }
    #[test]
    fn unlock_state_does_not_depend_on_a_timestamp() {
        use crate::protobuf::c_player_get_user_stats_response::{Stats, UnlockTime};
        let stats = [Stats {
            stat_id: Some(0),
            stat_value: Some(2),
            unlock_times: vec![UnlockTime {
                achievement_bit: Some(0),
                unlock_time: Some(100),
            }],
        }];
        let result = build_player_achievements(&synthetic_schema(), &stats).unwrap();
        assert!(
            !result[0].achieved,
            "a stale timestamp is not an unlock bit"
        );
        assert!(result[1].achieved, "an unlock can have no recorded date");
        assert_eq!(result[1].unlocktime, 0);
    }

    #[test]
    fn malformed_schema_is_an_error_not_an_empty_list() {
        assert!(build_player_achievements(&[255, 3, 5], &[]).is_err());
        assert!(build_player_achievements(&[0, 0, 8], &[]).is_err());
    }

    #[test]
    fn real_schema_carries_both_steam_icons_and_source_order() {
        let result = build_player_achievements(
            include_bytes!("../tests/fixtures/userstats_schema_410110.bin"),
            &[],
        )
        .unwrap();
        assert!(
            result
                .iter()
                .filter(|a| a.icon.is_some() && a.icon_gray.is_some())
                .count()
                >= 40
        );
        for (order, achievement) in result.iter().enumerate() {
            assert_eq!(achievement.schema_order as usize, order);
        }
    }
}

/// Counts for a batch of games, without fetching every game's achievement schema.
pub async fn get_progress(
    connection: &Connection,
    state: &ConnectionState,
    appids: Vec<u32>,
) -> Result<Vec<crate::protobuf::c_player_get_achievements_progress_response::Progress>> {
    use crate::protobuf::{
        CPlayerGetAchievementsProgressRequest, CPlayerGetAchievementsProgressResponse,
    };
    use crate::service_method::{ServiceMethod, call_authed};
    let response: CPlayerGetAchievementsProgressResponse = call_authed(
        connection,
        state,
        &ServiceMethod::new("Player.GetAchievementsProgress#1"),
        &CPlayerGetAchievementsProgressRequest {
            steamid: state.steamid,
            language: Some("english".into()),
            appids,
            include_unvetted_apps: Some(true),
        },
    )
    .await?;
    Ok(response.achievement_progress)
}
