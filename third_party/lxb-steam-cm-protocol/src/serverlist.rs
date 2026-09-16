use std::sync::Arc;

use tokio::sync::Mutex;

use crate::error::{Error, Result};

const CM_LIST_URL: &str = "https://api.steampowered.com/ISteamDirectory/GetCMListForConnect/v1/?cellid=0&cmtype=websockets&format=json";

#[derive(Debug, Clone, PartialEq)]
pub struct CmServer {
    pub endpoint: String,
    pub legacy_endpoint: Option<String>,
    pub data_center: Option<String>,
    pub realm: Option<String>,
    pub load: Option<u32>,
    pub weighted_load: Option<f64>,
}

impl CmServer {
    pub fn websocket_url(&self) -> String {
        format!("wss://{}/cmsocket/", self.endpoint)
    }
}

#[derive(Clone, Debug)]
pub struct ServerListCache {
    client: reqwest::Client,
    cache: Arc<Mutex<Vec<CmServer>>>,
}

impl Default for ServerListCache {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerListCache {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .user_agent("steam-cm-protocol/0.1")
                .build()
                .expect("reqwest client builder is valid"),
            cache: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub async fn list(&self, force_refresh: bool) -> Result<Vec<CmServer>> {
        let mut cache = self.cache.lock().await;
        if force_refresh || cache.is_empty() {
            *cache = fetch_cm_list(&self.client).await?;
        }

        Ok(cache.clone())
    }
}

pub async fn fetch_cm_list(client: &reqwest::Client) -> Result<Vec<CmServer>> {
    let response = client.get(CM_LIST_URL).send().await?.error_for_status()?;
    let directory: DirectoryResponse = response.json().await?;

    if !directory.response.success {
        return Err(Error::Protocol(
            "Steam CM directory returned failure".to_owned(),
        ));
    }

    let mut servers: Vec<CmServer> = directory
        .response
        .serverlist
        .into_iter()
        .map(|server| CmServer {
            endpoint: server.endpoint,
            legacy_endpoint: server.legacy_endpoint,
            data_center: server.data_center,
            realm: server.realm,
            load: server.load,
            weighted_load: server.weighted_load,
        })
        .collect();

    servers.sort_by(|left, right| {
        left.weighted_load
            .partial_cmp(&right.weighted_load)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    if servers.is_empty() {
        return Err(Error::InvalidResponse(
            "Steam CM directory returned no servers",
        ));
    }

    Ok(servers)
}

#[derive(Debug, serde::Deserialize)]
struct DirectoryResponse {
    response: DirectoryBody,
}

#[derive(Debug, serde::Deserialize)]
struct DirectoryBody {
    #[serde(default)]
    serverlist: Vec<DirectoryServer>,
    success: bool,
}

#[derive(Debug, serde::Deserialize)]
struct DirectoryServer {
    endpoint: String,
    #[serde(default)]
    legacy_endpoint: Option<String>,
    #[serde(default, rename = "dc")]
    data_center: Option<String>,
    #[serde(default)]
    realm: Option<String>,
    #[serde(default)]
    load: Option<u32>,
    #[serde(default, rename = "wtd_load")]
    weighted_load: Option<f64>,
}
