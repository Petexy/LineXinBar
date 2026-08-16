use std::{collections::HashMap, time::Duration};

use crate::{
    connection::{Connection, ConnectionState},
    emsg::EMsg,
    error::{Error, Result},
    friends::ProtocolAchievement,
    kv::{self, KVValue},
    protobuf::{
        CMsgClientGamesPlayed, CMsgClientGetUserStats, CMsgClientGetUserStatsResponse,
        CMsgProtoBufHeader, c_msg_client_games_played::GamePlayed,
        c_msg_client_get_user_stats_response::AchievementBlocks,
    },
};

/// Steam EResult OK.
const ERESULT_OK: i32 = 1;
/// Delay between starting "games played" and retrying the stats request, so Steam
/// registers the app as currently playing before serving user-private stats.
const GAMES_PLAYED_RETRY_DELAY: Duration = Duration::from_millis(500);

pub async fn get_player_achievements(
    connection: &Connection,
    state: &ConnectionState,
    appid: u32,
) -> Result<Vec<ProtocolAchievement>> {
    let steamid = state
        .steamid
        .ok_or(Error::MissingField("steamid not set in connection state"))?;

    let request = CMsgClientGetUserStats {
        game_id: Some(appid as u64),
        steam_id_for_user: Some(steamid),
        crc_stats: Some(0), // 0 forces Steam to return the full schema + blocks
        schema_local_version: None,
    };

    let mut response = request_user_stats(connection, state, appid, &request).await?;

    // Steam only serves user-private stats when the app is "currently playing". If the first
    // request fails (typically eresult=2 Fail), mark the app as played, retry once, then stop.
    if response.eresult != Some(ERESULT_OK) {
        tracing::debug!(
            appid,
            eresult = ?response.eresult,
            "user stats request not OK, retrying with games-played"
        );

        connection
            .send_message(
                EMsg::ClientGamesPlayed,
                &session_header(state),
                &CMsgClientGamesPlayed {
                    games_played: vec![GamePlayed {
                        game_id: Some(appid as u64),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            )
            .await?;

        tokio::time::sleep(GAMES_PLAYED_RETRY_DELAY).await;

        let retry = request_user_stats(connection, state, appid, &request).await;

        // Always clear games-played so we don't leave the user shown as in-game.
        let _ = connection
            .send_message(
                EMsg::ClientGamesPlayed,
                &session_header(state),
                &CMsgClientGamesPlayed {
                    games_played: vec![],
                    ..Default::default()
                },
            )
            .await;

        response = retry?;
    }

    let schema_len = response.schema.as_ref().map(|s| s.len()).unwrap_or(0);
    tracing::info!(
        appid,
        eresult = ?response.eresult,
        schema_len,
        achievement_blocks = response.achievement_blocks.len(),
        "user stats response received"
    );

    let schema_bytes = response.schema.unwrap_or_default();
    if schema_bytes.is_empty() {
        return Ok(vec![]);
    }

    let achievements = build_achievements(&schema_bytes, &response.achievement_blocks);

    tracing::info!(
        appid,
        defs = achievements.len(),
        unlocked = achievements.iter().filter(|a| a.achieved).count(),
        "achievements built"
    );

    Ok(achievements)
}

async fn request_user_stats(
    connection: &Connection,
    state: &ConnectionState,
    appid: u32,
    request: &CMsgClientGetUserStats,
) -> Result<CMsgClientGetUserStatsResponse> {
    let header = CMsgProtoBufHeader {
        steamid: state.steamid,
        client_sessionid: state.client_session_id,
        routing_appid: Some(appid),
        ..Default::default()
    };
    let packet = connection
        .request(EMsg::ClientGetUserStats, header, request)
        .await?;
    packet.decode_body::<CMsgClientGetUserStatsResponse>()
}

fn session_header(state: &ConnectionState) -> CMsgProtoBufHeader {
    CMsgProtoBufHeader {
        steamid: state.steamid,
        client_sessionid: state.client_session_id,
        ..Default::default()
    }
}

/// Pure mapping from a binary-KV achievement schema plus the response's achievement blocks to the
/// surfaced achievement list. Kept free of `Connection` so it can be unit tested directly.
pub(crate) fn build_achievements(
    schema_bytes: &[u8],
    blocks: &[AchievementBlocks],
) -> Vec<ProtocolAchievement> {
    let defs = match parse_achievement_schema(schema_bytes) {
        Ok(d) => d,
        Err(error) => {
            tracing::warn!(
                schema_len = schema_bytes.len(),
                %error,
                "achievement schema parse failed"
            );
            return vec![];
        }
    };

    if defs.is_empty() {
        return vec![];
    }

    // Build unlock map keyed by (achievement-group stat_id, bit position). An achievement block's
    // `achievement_id` is the schema stat_id; `unlock_time[pos]` corresponds to schema bits/<pos>.
    // A bit is unlocked iff its unlock_time != 0 (value = Unix epoch unlock time).
    let mut unlocked: HashMap<(u32, u32), u64> = HashMap::new();
    for block in blocks {
        let stat_id = block.achievement_id.unwrap_or(0);
        for (pos, &t) in block.unlock_time.iter().enumerate() {
            if t != 0 {
                unlocked.insert((stat_id, pos as u32), t as u64);
            }
        }
    }

    defs.into_iter()
        .map(|def| {
            let unlock_time = unlocked.get(&(def.stat_id, def.bit)).copied().unwrap_or(0);
            ProtocolAchievement {
                apiname: def.internal_name,
                achieved: unlock_time > 0,
                unlocktime: unlock_time,
                name: def.display_name,
                description: def.description,
            }
        })
        .collect()
}

struct AchievementDef {
    stat_id: u32,
    bit: u32,
    internal_name: String,
    display_name: Option<String>,
    description: Option<String>,
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
/// Achievement stat groups have `type = 4` (stored as string).
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
            });
        }
    }

    defs
}

fn parse_achievement_schema(data: &[u8]) -> Result<Vec<AchievementDef>> {
    let root = kv::parse_binary_kv(data)
        .ok_or_else(|| Error::Transport("achievement schema binary KV parse failed".to_owned()))?;
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
    fn build_achievements_joins_schema_and_unlock_blocks() {
        let schema = synthetic_schema();
        let blocks = vec![AchievementBlocks {
            achievement_id: Some(0),
            unlock_time: vec![1_700_000_000, 0], // bit 0 unlocked, bit 1 locked
        }];

        let mut achievements = build_achievements(&schema, &blocks);
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
        let achievements = build_achievements(schema, &[]);
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
}
