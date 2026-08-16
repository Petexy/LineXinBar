use std::collections::HashMap;

use crate::{
    connection::{Connection, ConnectionState},
    error::Result,
    friends::ProtocolGame,
    pics::AppCatalogInfo,
    protobuf::{CPlayerGetLastPlayedTimesRequest, CPlayerGetLastPlayedTimesResponse},
    service_method::{ServiceMethod, call_authed},
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlaytimeInfo {
    pub playtime_forever: i32,
    pub rtime_last_played: u32,
}

pub async fn get_last_played_times(
    connection: &Connection,
    state: &ConnectionState,
) -> Result<HashMap<u32, PlaytimeInfo>> {
    let method = ServiceMethod::new("Player.ClientGetLastPlayedTimes#1");
    let request = CPlayerGetLastPlayedTimesRequest {
        min_last_played: Some(0),
    };
    tracing::info!("requesting last-played-times (authed)");
    let response: CPlayerGetLastPlayedTimesResponse =
        call_authed(connection, state, &method, &request).await?;
    tracing::debug!(games = response.games.len(), "last-played-times response");

    let playtimes = response
        .games
        .into_iter()
        .filter_map(|game| {
            let appid = game.appid?;
            (appid > 0).then_some((
                appid as u32,
                PlaytimeInfo {
                    playtime_forever: game.playtime_forever.unwrap_or(0).max(0),
                    rtime_last_played: game.last_playtime.unwrap_or(0),
                },
            ))
        })
        .collect();

    Ok(playtimes)
}

pub fn recently_played_games(playtimes: &HashMap<u32, PlaytimeInfo>) -> Vec<ProtocolGame> {
    let mut games: Vec<ProtocolGame> = playtimes
        .iter()
        .filter(|(_, playtime)| playtime.rtime_last_played > 0)
        .map(|(appid, playtime)| ProtocolGame {
            appid: *appid,
            name: String::new(),
            playtime_forever: playtime.playtime_forever,
            rtime_last_played: playtime.rtime_last_played,
            img_icon_url: None,
            app_type: None,
            installdir: None,
            launch: Vec::new(),
        })
        .collect();
    games.sort_by(|a, b| {
        b.rtime_last_played
            .cmp(&a.rtime_last_played)
            .then_with(|| b.playtime_forever.cmp(&a.playtime_forever))
    });
    games
}

pub fn merge_catalog_and_playtimes(
    catalog: Vec<AppCatalogInfo>,
    playtimes: &HashMap<u32, PlaytimeInfo>,
) -> Vec<ProtocolGame> {
    let mut games: Vec<ProtocolGame> = catalog
        .into_iter()
        .map(|app| {
            let playtime = playtimes.get(&app.appid).copied().unwrap_or_default();
            ProtocolGame {
                appid: app.appid,
                name: app.name,
                playtime_forever: playtime.playtime_forever,
                rtime_last_played: playtime.rtime_last_played,
                img_icon_url: app.img_icon_url,
                app_type: app.app_type,
                installdir: app.installdir,
                launch: app.launch,
            }
        })
        .collect();

    games.sort_by(|a, b| {
        b.playtime_forever
            .cmp(&a.playtime_forever)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.appid.cmp(&b.appid))
    });
    games
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_defaults_never_played_games_to_zero_playtime() {
        let catalog = vec![
            AppCatalogInfo {
                appid: 20,
                name: "Played".to_owned(),
                img_icon_url: Some("icon".to_owned()),
                app_type: Some("game".to_owned()),
                installdir: None,
                launch: Vec::new(),
            },
            AppCatalogInfo {
                appid: 10,
                name: "Never Played".to_owned(),
                img_icon_url: None,
                app_type: Some("game".to_owned()),
                installdir: None,
                launch: Vec::new(),
            },
        ];
        let playtimes = HashMap::from([(
            20,
            PlaytimeInfo {
                playtime_forever: 120,
                rtime_last_played: 123,
            },
        )]);

        let games = merge_catalog_and_playtimes(catalog, &playtimes);

        assert_eq!(games.len(), 2);
        assert_eq!(games[0].appid, 20);
        assert_eq!(games[0].playtime_forever, 120);
        assert_eq!(games[1].appid, 10);
        assert_eq!(games[1].playtime_forever, 0);
    }
}
