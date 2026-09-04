use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use crate::{
    connection::{Connection, ConnectionState},
    emsg::EMsg,
    error::{Error, Result},
    friends::LaunchEntry,
    kv::{self, KVValue},
    protobuf::{
        CMsgClientPicsAccessTokenRequest, CMsgClientPicsAccessTokenResponse,
        CMsgClientPicsProductInfoRequest, CMsgClientPicsProductInfoResponse, CMsgProtoBufHeader,
        c_msg_client_pics_product_info_request,
    },
};

const ACCESS_TOKEN_BATCH_SIZE: usize = 250;
const PRODUCT_INFO_BATCH_SIZE: usize = 100;
const STREAM_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppCatalogInfo {
    pub appid: u32,
    pub name: String,
    pub img_icon_url: Option<String>,
    /// Steam appinfo `common.type`, lowercased ("game", "application", "tool", "dlc", …).
    /// `None` when the app's product info was never resolved or carried no type.
    pub app_type: Option<String>,
    /// Appinfo `config.installdir` — the game's folder name under `steamapps/common/`.
    pub installdir: Option<String>,
    /// Appinfo `config/launch` entries — used by the direct (no-Steam) launch path.
    pub launch: Vec<LaunchEntry>,
    /// Appinfo `common/library_assets_full` — where the app's library pictures are published.
    pub library_art: LibraryArt,
}

/// Where Valve publishes one app's library pictures, as its own client asks for them.
///
/// Appinfo `common/library_assets_full`. Each picture is a path below the app's own asset
/// directory, in one of two forms: a bare file name, `library_600x900.jpg`, or a
/// content-addressed one, `28dbb24430c7fe4732fafd7ce3a3b701d1d805eb/library_600x900.jpg`.
/// Both are current — Valve moved store artwork to the hashed form and left what was
/// already published where it was — and the two are mixed within one app, so a game may
/// have a bare capsule and a hashed logo.
///
/// Carried because the path cannot be guessed. Artwork published only under the hashed form
/// is served under nothing else and the client caches it under nothing else, so an app whose
/// pictures were uploaded since the change has none at all for a caller that asks by name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LibraryArt {
    /// `library_capsule` — the portrait capsule, what a game is recognised by.
    pub capsule: Option<String>,
    /// `library_hero` — the wide picture from the app's own page.
    pub hero: Option<String>,
    /// `library_logo` — the app's title drawn as artwork on a transparent ground.
    pub logo: Option<String>,
}

/// Library entries we surface: real games plus owned software/tools. DLC, soundtracks (music),
/// videos, demos, configs, etc. are dropped — as are never-resolved empty rows. A named app whose
/// type we couldn't read is kept so a parse miss never silently loses a real game.
fn is_library_entry(app: &AppCatalogInfo) -> bool {
    match app.app_type.as_deref() {
        Some("game" | "application" | "tool") => true,
        Some(_) => false,
        None => !app.name.is_empty(),
    }
}

pub async fn load_owned_app_catalog(
    connection: &Connection,
    state: &ConnectionState,
    package_ids: Vec<u32>,
) -> Result<Vec<AppCatalogInfo>> {
    let package_ids = normalized_ids(package_ids);
    if package_ids.is_empty() {
        return Ok(vec![]);
    }

    tracing::info!(
        packages = package_ids.len(),
        "PICS library package resolution started"
    );

    let package_tokens = access_tokens_for_packages(connection, state, &package_ids).await?;
    let appids = resolve_package_appids(connection, state, &package_ids, &package_tokens).await?;
    if appids.is_empty() {
        return Err(Error::InvalidResponse(
            "PICS resolved a non-empty package list to no app ids",
        ));
    }

    tracing::info!(
        packages = package_ids.len(),
        appids = appids.len(),
        "PICS package resolution completed"
    );

    let app_tokens = access_tokens_for_apps(connection, state, &appids).await?;
    let apps = resolve_app_infos(connection, state, &appids, &app_tokens).await?;

    tracing::info!(
        appids = appids.len(),
        apps = apps.len(),
        "PICS app info resolution completed"
    );

    Ok(apps)
}

/// What Steam calls a handful of apps, by their ids alone.
///
/// `load_owned_app_catalog` above answers the same question about an account's
/// *licences*, which is the only shape the published crate offers — and it is
/// the wrong shape for the one caller this exists for. A friend's game is an
/// app id with no licence of this account's behind it: `ClientPersonaState`
/// carries `game_played_app_id` and very often no `game_name` at all, which is
/// why every other client that draws a friends list resolves the name itself
/// (see `third_party/Pluvia`, which keeps a PICS queue for exactly this).
///
/// Apps Steam declines to name are absent from the answer rather than present
/// and empty, so a caller can tell "not named" from "named nothing".
pub async fn app_names(
    connection: &Connection,
    state: &ConnectionState,
    appids: &[u32],
) -> Result<HashMap<u32, String>> {
    let appids = normalized_ids(appids.iter().copied());
    if appids.is_empty() {
        return Ok(HashMap::new());
    }
    let tokens = access_tokens_for_apps(connection, state, &appids).await?;
    let apps = resolve_app_infos(connection, state, &appids, &tokens).await?;
    Ok(apps
        .into_iter()
        .filter(|app| !app.name.is_empty())
        .map(|app| (app.appid, app.name))
        .collect())
}

/// The app information Steam holds for one app, exactly as it sent it.
///
/// `load_owned_app_catalog` reads the few keys a library listing needs out of
/// the same answer. This hands over the whole thing undecoded, for callers
/// after something else in it — the depot list, which is what a download is
/// planned from.
pub async fn raw_app_info(
    connection: &Connection,
    state: &ConnectionState,
    appids: &[u32],
) -> Result<HashMap<u32, Vec<u8>>> {
    let appids = normalized_ids(appids.iter().copied());
    if appids.is_empty() {
        return Ok(HashMap::new());
    }
    let tokens = access_tokens_for_apps(connection, state, &appids).await?;
    let mut buffers = HashMap::new();

    for chunk in appids.chunks(PRODUCT_INFO_BATCH_SIZE) {
        let apps = chunk
            .iter()
            .map(|appid| c_msg_client_pics_product_info_request::AppInfo {
                appid: Some(*appid),
                access_token: Some(tokens.get(appid).copied().unwrap_or_default()),
                only_public_obsolete: Some(false),
            })
            .collect();

        let responses = request_product_info_stream(
            connection,
            state,
            CMsgClientPicsProductInfoRequest {
                packages: vec![],
                apps,
                meta_data_only: Some(false),
                num_prev_failed: Some(0),
                obsolete_supports_package_tokens: Some(1),
                sequence_number: Some(1),
                single_response: Some(false),
            },
        )
        .await?;

        for response in responses {
            for app in response.apps {
                if let (Some(appid), Some(buffer)) = (app.appid, app.buffer) {
                    buffers.insert(appid, buffer);
                }
            }
        }
    }

    Ok(buffers)
}

/// The information Steam holds for one package, exactly as it sent it.
///
/// The same answer `load_owned_app_catalog` reads app ids out of, handed over
/// undecoded for callers after the rest: which depots a licence covers, which
/// is what says whether an account may fetch a given piece of a game.
pub async fn raw_package_info(
    connection: &Connection,
    state: &ConnectionState,
    package_ids: &[u32],
) -> Result<HashMap<u32, Vec<u8>>> {
    let package_ids = normalized_ids(package_ids.iter().copied());
    if package_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let tokens = access_tokens_for_packages(connection, state, &package_ids).await?;
    let mut buffers = HashMap::new();

    for chunk in package_ids.chunks(PRODUCT_INFO_BATCH_SIZE) {
        let packages = chunk
            .iter()
            .map(
                |package_id| c_msg_client_pics_product_info_request::PackageInfo {
                    packageid: Some(*package_id),
                    access_token: Some(tokens.get(package_id).copied().unwrap_or_default()),
                },
            )
            .collect();

        let responses = request_product_info_stream(
            connection,
            state,
            CMsgClientPicsProductInfoRequest {
                packages,
                apps: vec![],
                meta_data_only: Some(false),
                num_prev_failed: Some(0),
                obsolete_supports_package_tokens: Some(1),
                sequence_number: Some(1),
                single_response: Some(false),
            },
        )
        .await?;

        for response in responses {
            for package in response.packages {
                if let (Some(packageid), Some(buffer)) = (package.packageid, package.buffer) {
                    buffers.insert(packageid, buffer);
                }
            }
        }
    }

    Ok(buffers)
}

async fn access_tokens_for_packages(
    connection: &Connection,
    state: &ConnectionState,
    package_ids: &[u32],
) -> Result<HashMap<u32, u64>> {
    let mut tokens = HashMap::new();

    for chunk in package_ids.chunks(ACCESS_TOKEN_BATCH_SIZE) {
        let response = request_access_tokens(connection, state, vec![], chunk.to_vec()).await?;
        for token in response.package_access_tokens {
            if let (Some(packageid), Some(access_token)) = (token.packageid, token.access_token) {
                tokens.insert(packageid, access_token);
            }
        }
        if !response.package_denied_tokens.is_empty() {
            tracing::debug!(
                denied = response.package_denied_tokens.len(),
                "PICS package access tokens denied"
            );
        }
    }

    Ok(tokens)
}

async fn access_tokens_for_apps(
    connection: &Connection,
    state: &ConnectionState,
    appids: &[u32],
) -> Result<HashMap<u32, u64>> {
    let mut tokens = HashMap::new();

    for chunk in appids.chunks(ACCESS_TOKEN_BATCH_SIZE) {
        let response = request_access_tokens(connection, state, chunk.to_vec(), vec![]).await?;
        for token in response.app_access_tokens {
            if let (Some(appid), Some(access_token)) = (token.appid, token.access_token) {
                tokens.insert(appid, access_token);
            }
        }
        if !response.app_denied_tokens.is_empty() {
            tracing::debug!(
                denied = response.app_denied_tokens.len(),
                "PICS app access tokens denied"
            );
        }
    }

    Ok(tokens)
}

async fn request_access_tokens(
    connection: &Connection,
    state: &ConnectionState,
    appids: Vec<u32>,
    packageids: Vec<u32>,
) -> Result<CMsgClientPicsAccessTokenResponse> {
    let packet = connection
        .request(
            EMsg::ClientPICSAccessTokenRequest,
            session_header(state),
            &CMsgClientPicsAccessTokenRequest { packageids, appids },
        )
        .await?;
    packet.ensure_success()?;
    packet.decode_body()
}

async fn resolve_package_appids(
    connection: &Connection,
    state: &ConnectionState,
    package_ids: &[u32],
    package_tokens: &HashMap<u32, u64>,
) -> Result<Vec<u32>> {
    let mut appids = HashSet::new();
    let mut empty_parse_logs = 0usize;

    for chunk in package_ids.chunks(PRODUCT_INFO_BATCH_SIZE) {
        let packages = chunk
            .iter()
            .map(
                |package_id| c_msg_client_pics_product_info_request::PackageInfo {
                    packageid: Some(*package_id),
                    access_token: Some(package_tokens.get(package_id).copied().unwrap_or_default()),
                },
            )
            .collect();

        let responses = request_product_info_stream(
            connection,
            state,
            CMsgClientPicsProductInfoRequest {
                packages,
                apps: vec![],
                meta_data_only: Some(false),
                num_prev_failed: Some(0),
                obsolete_supports_package_tokens: Some(1),
                sequence_number: Some(1),
                single_response: Some(false),
            },
        )
        .await?;

        for response in responses {
            for package in response.packages {
                let packageid = package.packageid.unwrap_or_default();
                if let Some(buffer) = package.buffer {
                    match parse_package_appids(&buffer) {
                        Some(package_appids) if !package_appids.is_empty() => {
                            appids.extend(package_appids);
                        }
                        Some(_) if empty_parse_logs < 3 => {
                            empty_parse_logs += 1;
                            tracing::debug!(
                                packageid,
                                buffer_len = buffer.len(),
                                buffer_prefix = hex_prefix(&buffer, 96),
                                "PICS package buffer parsed but contained no appids"
                            );
                        }
                        None if empty_parse_logs < 3 => {
                            empty_parse_logs += 1;
                            tracing::warn!(
                                packageid,
                                buffer_len = buffer.len(),
                                buffer_prefix = hex_prefix(&buffer, 96),
                                "PICS package buffer binary KV parse failed"
                            );
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    Ok(normalized_ids(appids))
}

async fn resolve_app_infos(
    connection: &Connection,
    state: &ConnectionState,
    appids: &[u32],
    app_tokens: &HashMap<u32, u64>,
) -> Result<Vec<AppCatalogInfo>> {
    let mut parsed_count = 0usize;
    let mut named_count = 0usize;
    let mut empty_name_logs = 0usize;
    let mut apps_by_id: HashMap<u32, AppCatalogInfo> = appids
        .iter()
        .map(|appid| {
            (
                *appid,
                AppCatalogInfo {
                    appid: *appid,
                    name: String::new(),
                    img_icon_url: None,
                    app_type: None,
                    installdir: None,
                    launch: Vec::new(),
                    library_art: LibraryArt::default(),
                },
            )
        })
        .collect();

    for chunk in appids.chunks(PRODUCT_INFO_BATCH_SIZE) {
        let apps = chunk
            .iter()
            .map(|appid| c_msg_client_pics_product_info_request::AppInfo {
                appid: Some(*appid),
                access_token: Some(app_tokens.get(appid).copied().unwrap_or_default()),
                only_public_obsolete: Some(false),
            })
            .collect();

        let responses = request_product_info_stream(
            connection,
            state,
            CMsgClientPicsProductInfoRequest {
                packages: vec![],
                apps,
                meta_data_only: Some(false),
                num_prev_failed: Some(0),
                obsolete_supports_package_tokens: Some(1),
                sequence_number: Some(1),
                single_response: Some(false),
            },
        )
        .await?;

        for response in responses {
            for app in response.apps {
                let Some(appid) = app.appid else { continue };
                let Some(buffer) = app.buffer else { continue };
                match parse_app_info(appid, &buffer) {
                    Some(parsed) => {
                        parsed_count += 1;
                        if parsed.name.is_empty() {
                            if empty_name_logs < 3 {
                                empty_name_logs += 1;
                                tracing::debug!(
                                    appid,
                                    buffer_len = buffer.len(),
                                    buffer_prefix = hex_prefix(&buffer, 128),
                                    "PICS app-info parsed without a name"
                                );
                            }
                        } else {
                            named_count += 1;
                        }
                        apps_by_id.insert(appid, parsed);
                    }
                    None if empty_name_logs < 3 => {
                        empty_name_logs += 1;
                        tracing::debug!(
                            appid,
                            buffer_len = buffer.len(),
                            buffer_prefix = hex_prefix(&buffer, 128),
                            "PICS app-info parse failed"
                        );
                    }
                    None => {}
                }
            }
        }
    }

    let mut apps: Vec<AppCatalogInfo> = apps_by_id.into_values().collect();
    let before_filter = apps.len();
    apps.retain(is_library_entry);
    let dropped = before_filter - apps.len();
    apps.sort_by_key(|app| app.appid);
    tracing::info!(
        appids = appids.len(),
        parsed = parsed_count,
        named = named_count,
        "PICS app info parsing completed"
    );
    tracing::info!(
        kept = apps.len(),
        dropped,
        "PICS type filter (dropped dlc/music/video/empty rows)"
    );
    Ok(apps)
}

async fn request_product_info_stream(
    connection: &Connection,
    state: &ConnectionState,
    request: CMsgClientPicsProductInfoRequest,
) -> Result<Vec<CMsgClientPicsProductInfoResponse>> {
    let (job_id, mut rx) = connection
        .send_request_stream(
            EMsg::ClientPICSProductInfoRequest,
            session_header(state),
            &request,
        )
        .await?;
    let mut responses = Vec::new();

    loop {
        let packet = match tokio::time::timeout(STREAM_TIMEOUT, rx.recv()).await {
            Ok(Some(Ok(packet))) => packet,
            Ok(Some(Err(error))) => {
                connection.end_stream(job_id).await;
                return Err(error);
            }
            Ok(None) => {
                connection.end_stream(job_id).await;
                return Err(Error::Transport(
                    "PICS product-info stream closed".to_owned(),
                ));
            }
            Err(_) => {
                connection.end_stream(job_id).await;
                return Err(Error::Transport(
                    "PICS product-info stream timed out".to_owned(),
                ));
            }
        };

        if let Err(error) = packet.ensure_success() {
            connection.end_stream(job_id).await;
            return Err(error);
        }
        let response: CMsgClientPicsProductInfoResponse = match packet.decode_body() {
            Ok(response) => response,
            Err(error) => {
                connection.end_stream(job_id).await;
                return Err(error);
            }
        };
        let response_pending = response.response_pending.unwrap_or(false);
        tracing::debug!(
            response_bytes = packet.body.len(),
            packages = response.packages.len(),
            apps = response.apps.len(),
            response_pending,
            "PICS product-info stream chunk"
        );
        responses.push(response);

        if !response_pending {
            connection.end_stream(job_id).await;
            return Ok(responses);
        }
    }
}

fn session_header(state: &ConnectionState) -> CMsgProtoBufHeader {
    CMsgProtoBufHeader {
        steamid: state.steamid,
        client_sessionid: state.client_session_id,
        ..Default::default()
    }
}

fn normalized_ids<I>(ids: I) -> Vec<u32>
where
    I: IntoIterator<Item = u32>,
{
    let mut ids: Vec<u32> = ids.into_iter().filter(|id| *id != 0).collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

fn parse_package_appids(buffer: &[u8]) -> Option<Vec<u32>> {
    let root = kv::parse_binary_kv(buffer)?;

    let mut appids = Vec::new();
    collect_appids_nodes(&root, &mut appids);
    Some(normalized_ids(appids))
}

fn collect_appids_nodes(node: &KVValue, appids: &mut Vec<u32>) {
    let Some(children) = node.as_nested() else {
        return;
    };
    for (key, value) in children {
        if key.eq_ignore_ascii_case("appids") {
            collect_u32_values(value, appids);
        } else {
            collect_appids_nodes(value, appids);
        }
    }
}

fn collect_u32_values(node: &KVValue, values: &mut Vec<u32>) {
    if let Some(value) = node.as_u32() {
        values.push(value);
    }

    if let Some(children) = node.as_nested() {
        for (_, value) in children {
            collect_u32_values(value, values);
        }
    }
}

fn parse_app_info(appid: u32, buffer: &[u8]) -> Option<AppCatalogInfo> {
    parse_text_app_info(appid, buffer).or_else(|| parse_binary_app_info(appid, buffer))
}

fn hex_prefix(buffer: &[u8], limit: usize) -> String {
    let mut out = String::with_capacity(limit.min(buffer.len()) * 2);
    for byte in buffer.iter().take(limit) {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

fn parse_binary_app_info(appid: u32, buffer: &[u8]) -> Option<AppCatalogInfo> {
    let root = kv::parse_binary_kv(buffer)?;
    let common = root
        .get("appinfo")
        .and_then(|appinfo| appinfo.get("common"))
        .or_else(|| root.get("common"))?;
    let name = common
        .get("name")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let icon = common
        .get("clienticon")
        .or_else(|| common.get("icon"))
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let app_type = common
        .get("type")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase());

    let config = root
        .get("appinfo")
        .and_then(|appinfo| appinfo.get("config"))
        .or_else(|| root.get("config"));
    let installdir = config
        .and_then(|config| config.get("installdir"))
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let launch = config
        .and_then(|config| config.get("launch"))
        .map(launch_entries_from_kv)
        .unwrap_or_default();
    let library_art = common
        .get("library_assets_full")
        .map(library_art_from_kv)
        .unwrap_or_default();

    Some(AppCatalogInfo {
        appid,
        name: name.to_owned(),
        img_icon_url: icon,
        app_type,
        installdir,
        launch,
        library_art,
    })
}

/// Read `common/library_assets_full` from a binary-KV node.
///
/// Each picture is `{piece}/image/english`, with `image2x` standing in when there is no 1×
/// entry — some artwork is published only at the larger size, and a picture twice as big as
/// it needs to be is a picture rather than nothing.
///
/// English rather than the session's language, deliberately: it is the variant every app that
/// has the picture at all publishes, while the localized ones exist only where a publisher
/// made them. Falling back the other way round would give a caller a Chinese capsule for a
/// game whose English one is right there.
fn library_art_from_kv(assets: &KVValue) -> LibraryArt {
    let published = |piece: &str| -> Option<String> {
        let entry = assets.get(piece)?;
        ["image", "image2x"].into_iter().find_map(|size| {
            entry
                .get(size)?
                .get("english")
                .and_then(KVValue::as_str)
                .filter(|path| !path.is_empty())
                .map(str::to_owned)
        })
    };
    LibraryArt {
        capsule: published("library_capsule"),
        hero: published("library_hero"),
        logo: published("library_logo"),
    }
}

/// Collect `config/launch` entries from a binary-KV `launch` node (numbered children `0`, `1`, …).
fn launch_entries_from_kv(launch: &KVValue) -> Vec<LaunchEntry> {
    let Some(children) = launch.as_nested() else {
        return Vec::new();
    };
    children
        .iter()
        .filter_map(|(_, entry)| {
            let executable = entry
                .get("executable")
                .and_then(|value| value.as_str())
                .filter(|value| !value.is_empty())?
                .to_owned();
            let cfg = entry.get("config");
            let str_field = |node: Option<&KVValue>, key: &str| {
                node.and_then(|node| node.get(key))
                    .and_then(|value| value.as_str())
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
            };
            Some(LaunchEntry {
                executable,
                arguments: str_field(Some(entry), "arguments"),
                workingdir: str_field(Some(entry), "workingdir"),
                launch_type: str_field(Some(entry), "type"),
                oslist: str_field(cfg, "oslist"),
                osarch: str_field(cfg, "osarch"),
                betakey: str_field(cfg, "betakey"),
            })
        })
        .collect()
}

fn parse_text_app_info(appid: u32, buffer: &[u8]) -> Option<AppCatalogInfo> {
    let text = std::str::from_utf8(buffer)
        .ok()?
        .trim_matches(char::from(0));
    let root = parse_vdf(text)?;
    let common = root
        .get_node("appinfo")
        .and_then(|appinfo| appinfo.get_node("common"))
        .or_else(|| root.get_node("common"))?;
    let name = common.get_str("name").unwrap_or_default();
    let icon = common
        .get_str("clienticon")
        .or_else(|| common.get_str("icon"))
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let app_type = common
        .get_str("type")
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase());

    let config = root
        .get_node("appinfo")
        .and_then(|appinfo| appinfo.get_node("config"))
        .or_else(|| root.get_node("config"));
    let installdir = config
        .and_then(|config| config.get_str("installdir"))
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let launch = config
        .and_then(|config| config.get_node("launch"))
        .map(launch_entries_from_vdf)
        .unwrap_or_default();
    let library_art = common
        .get_node("library_assets_full")
        .map(library_art_from_vdf)
        .unwrap_or_default();

    Some(AppCatalogInfo {
        appid,
        name: name.to_owned(),
        img_icon_url: icon,
        app_type,
        installdir,
        launch,
        library_art,
    })
}

/// The same as [`library_art_from_kv`], from a text-VDF node.
fn library_art_from_vdf(assets: &VdfNode) -> LibraryArt {
    let published = |piece: &str| -> Option<String> {
        let entry = assets.get_node(piece)?;
        ["image", "image2x"].into_iter().find_map(|size| {
            entry
                .get_node(size)?
                .get_str("english")
                .filter(|path| !path.is_empty())
                .map(str::to_owned)
        })
    };
    LibraryArt {
        capsule: published("library_capsule"),
        hero: published("library_hero"),
        logo: published("library_logo"),
    }
}

/// Collect `config/launch` entries from a text-VDF `launch` node (numbered child nodes).
fn launch_entries_from_vdf(launch: &VdfNode) -> Vec<LaunchEntry> {
    launch
        .values
        .iter()
        .filter_map(|(_, value)| match value {
            VdfValue::Node(entry) => {
                let executable = entry
                    .get_str("executable")
                    .filter(|value| !value.is_empty())?
                    .to_owned();
                let cfg = entry.get_node("config");
                let str_field = |node: Option<&VdfNode>, key: &str| {
                    node.and_then(|node| node.get_str(key))
                        .filter(|value| !value.is_empty())
                        .map(str::to_owned)
                };
                Some(LaunchEntry {
                    executable,
                    arguments: str_field(Some(entry), "arguments"),
                    workingdir: str_field(Some(entry), "workingdir"),
                    launch_type: str_field(Some(entry), "type"),
                    oslist: str_field(cfg, "oslist"),
                    osarch: str_field(cfg, "osarch"),
                    betakey: str_field(cfg, "betakey"),
                })
            }
            VdfValue::Str(_) => None,
        })
        .collect()
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct VdfNode {
    values: Vec<(String, VdfValue)>,
}

impl VdfNode {
    fn get_node(&self, key: &str) -> Option<&VdfNode> {
        self.values.iter().find_map(|(k, value)| {
            if k.eq_ignore_ascii_case(key)
                && let VdfValue::Node(node) = value
            {
                return Some(node);
            }
            None
        })
    }

    fn get_str(&self, key: &str) -> Option<&str> {
        self.values.iter().find_map(|(k, value)| {
            if k.eq_ignore_ascii_case(key)
                && let VdfValue::Str(s) = value
            {
                return Some(s.as_str());
            }
            None
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum VdfValue {
    Node(VdfNode),
    Str(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum VdfToken {
    Str(String),
    Open,
    Close,
}

struct VdfLexer<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> VdfLexer<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input: input.as_bytes(),
            pos: 0,
        }
    }

    fn next_token(&mut self) -> Option<VdfToken> {
        self.skip_ws_and_comments();
        let byte = *self.input.get(self.pos)?;
        match byte {
            b'{' => {
                self.pos += 1;
                Some(VdfToken::Open)
            }
            b'}' => {
                self.pos += 1;
                Some(VdfToken::Close)
            }
            b'"' => self.read_quoted_string().map(VdfToken::Str),
            _ => self.read_bare_string().map(VdfToken::Str),
        }
    }

    fn skip_ws_and_comments(&mut self) {
        loop {
            while self
                .input
                .get(self.pos)
                .is_some_and(|byte| byte.is_ascii_whitespace())
            {
                self.pos += 1;
            }

            if self.input.get(self.pos..self.pos + 2) == Some(b"//") {
                self.pos += 2;
                while self.input.get(self.pos).is_some_and(|byte| *byte != b'\n') {
                    self.pos += 1;
                }
                continue;
            }

            break;
        }
    }

    fn read_quoted_string(&mut self) -> Option<String> {
        self.pos += 1;
        let mut out = Vec::new();
        while let Some(byte) = self.input.get(self.pos).copied() {
            self.pos += 1;
            match byte {
                b'"' => return Some(String::from_utf8_lossy(&out).into_owned()),
                b'\\' => {
                    let escaped = self.input.get(self.pos).copied()?;
                    self.pos += 1;
                    out.push(match escaped {
                        b'n' => b'\n',
                        b't' => b'\t',
                        b'r' => b'\r',
                        b'\\' => b'\\',
                        b'"' => b'"',
                        other => other,
                    });
                }
                other => out.push(other),
            }
        }
        None
    }

    fn read_bare_string(&mut self) -> Option<String> {
        let start = self.pos;
        while self
            .input
            .get(self.pos)
            .is_some_and(|byte| !byte.is_ascii_whitespace() && *byte != b'{' && *byte != b'}')
        {
            self.pos += 1;
        }
        (self.pos > start).then(|| String::from_utf8_lossy(&self.input[start..self.pos]).into())
    }
}

fn parse_vdf(text: &str) -> Option<VdfNode> {
    let mut lexer = VdfLexer::new(text);
    parse_vdf_node(&mut lexer, false)
}

fn parse_vdf_node(lexer: &mut VdfLexer<'_>, stop_on_close: bool) -> Option<VdfNode> {
    let mut node = VdfNode::default();

    loop {
        let Some(token) = lexer.next_token() else {
            return (!stop_on_close).then_some(node);
        };
        match token {
            VdfToken::Close if stop_on_close => return Some(node),
            VdfToken::Close => return None,
            VdfToken::Open => return None,
            VdfToken::Str(key) => match lexer.next_token()? {
                VdfToken::Open => {
                    let child = parse_vdf_node(lexer, true)?;
                    node.values.push((key, VdfValue::Node(child)));
                }
                VdfToken::Str(value) => node.values.push((key, VdfValue::Str(value))),
                VdfToken::Close => return None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog_app(appid: u32, name: &str, app_type: Option<&str>) -> AppCatalogInfo {
        AppCatalogInfo {
            appid,
            name: name.to_owned(),
            img_icon_url: None,
            app_type: app_type.map(str::to_owned),
            installdir: None,
            launch: Vec::new(),
            library_art: LibraryArt::default(),
        }
    }

    #[test]
    fn type_filter_keeps_games_software_tools_and_named_untyped() {
        // Kept: games, software/applications, tools.
        assert!(is_library_entry(&catalog_app(
            1,
            "Elden Ring",
            Some("game")
        )));
        assert!(is_library_entry(&catalog_app(
            2,
            "Wallpaper Engine",
            Some("application")
        )));
        assert!(is_library_entry(&catalog_app(3, "SteamVR", Some("tool"))));
        // Kept: a named app whose type we couldn't resolve (parse miss must not lose a real game).
        assert!(is_library_entry(&catalog_app(4, "Mystery Game", None)));

        // Dropped: DLC/soundtracks/videos/demos/configs.
        assert!(!is_library_entry(&catalog_app(
            5,
            "Shadow of the Erdtree",
            Some("dlc")
        )));
        assert!(!is_library_entry(&catalog_app(
            6,
            "Original Soundtrack",
            Some("music")
        )));
        assert!(!is_library_entry(&catalog_app(
            7,
            "Launch Trailer",
            Some("video")
        )));
        assert!(!is_library_entry(&catalog_app(8, "Playtest", Some("demo"))));
        // Dropped: never-resolved empty row (no type, no name).
        assert!(!is_library_entry(&catalog_app(9, "", None)));
    }

    #[test]
    fn parses_app_type_from_text_common_section() {
        let data = br#"
            "appinfo"
            {
                "common"
                {
                    "name" "Some DLC"
                    "type" "DLC"
                }
            }
        "#;
        let app = parse_app_info(123, data).expect("app info should parse");
        assert_eq!(app.app_type.as_deref(), Some("dlc"));
        assert!(!is_library_entry(&app));
    }

    #[test]
    fn parses_package_appids_from_binary_kv() {
        let data = [
            0x00, 0x00, // root
            0x00, b'a', b'p', b'p', b'i', b'd', b's', 0x00, 0x02, b'0', 0x00, 10, 0, 0, 0, 0x01,
            b'1', 0x00, b'2', b'0', 0x00, 0x08, 0x08,
        ];

        assert_eq!(parse_package_appids(&data).unwrap(), vec![10, 20]);
    }

    #[test]
    fn parses_text_app_common_section() {
        let data = br#"
            "appinfo"
            {
                "common"
                {
                    "name" "Half-Life"
                    "clienticon" "abc123"
                }
            }
        "#;

        let app = parse_app_info(70, data).expect("app info should parse");
        assert_eq!(app.appid, 70);
        assert_eq!(app.name, "Half-Life");
        assert_eq!(app.img_icon_url.as_deref(), Some("abc123"));
    }

    #[test]
    fn parses_utf8_text_app_names() {
        let data = b"\"appinfo\"\n{\n\"common\"\n{\n\"name\" \"ACE COMBAT\xe2\x84\xa27: SKIES UNKNOWN\"\n}\n}\n";

        let app = parse_app_info(502500, data).expect("app info should parse");
        assert_eq!(app.name, "ACE COMBAT\u{2122}7: SKIES UNKNOWN");
    }

    #[test]
    fn parses_installdir_and_launch_from_text_config() {
        let data = br#"
            "appinfo"
            {
                "common" { "name" "Portal" "type" "game" }
                "config"
                {
                    "installdir" "Portal"
                    "launch"
                    {
                        "0"
                        {
                            "executable" "hl2.exe"
                            "arguments" "-game portal"
                            "type" "default"
                            "config" { "oslist" "windows" "osarch" "64" }
                        }
                        "1"
                        {
                            "executable" "hl2_linux"
                            "config" { "oslist" "linux" }
                        }
                    }
                }
            }
        "#;

        let app = parse_app_info(400, data).expect("app info should parse");
        assert_eq!(app.installdir.as_deref(), Some("Portal"));
        assert_eq!(app.launch.len(), 2);
        assert_eq!(app.launch[0].executable, "hl2.exe");
        assert_eq!(app.launch[0].arguments.as_deref(), Some("-game portal"));
        assert_eq!(app.launch[0].launch_type.as_deref(), Some("default"));
        assert_eq!(app.launch[0].oslist.as_deref(), Some("windows"));
        assert_eq!(app.launch[0].osarch.as_deref(), Some("64"));
        assert_eq!(app.launch[1].executable, "hl2_linux");
        assert_eq!(app.launch[1].oslist.as_deref(), Some("linux"));
        // No common.type changes; still a launchable game.
        assert!(is_library_entry(&app));
    }

    #[test]
    fn launch_entries_from_kv_reads_numbered_children() {
        // Mirrors the binary-KV path: a `launch` node with numbered child entries.
        let entry0 = KVValue::Nested(vec![
            (
                "executable".to_owned(),
                KVValue::Str("bin/game.exe".to_owned()),
            ),
            ("workingdir".to_owned(), KVValue::Str("bin".to_owned())),
            (
                "config".to_owned(),
                KVValue::Nested(vec![
                    ("oslist".to_owned(), KVValue::Str("windows".to_owned())),
                    ("osarch".to_owned(), KVValue::Str("64".to_owned())),
                ]),
            ),
        ]);
        // An entry with no executable must be skipped.
        let entry1 = KVValue::Nested(vec![(
            "description".to_owned(),
            KVValue::Str("placeholder".to_owned()),
        )]);
        let launch = KVValue::Nested(vec![("0".to_owned(), entry0), ("1".to_owned(), entry1)]);

        let entries = launch_entries_from_kv(&launch);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].executable, "bin/game.exe");
        assert_eq!(entries[0].workingdir.as_deref(), Some("bin"));
        assert_eq!(entries[0].oslist.as_deref(), Some("windows"));
        assert_eq!(entries[0].osarch.as_deref(), Some("64"));
        assert_eq!(entries[0].arguments, None);
    }

    /// Both forms of published path, in the shape a real app has them: a hashed capsule and
    /// logo beside a bare hero, and localized variants that must not be picked over English.
    /// Taken from app 3288210, whose capsule is published under a hash and nowhere else.
    #[test]
    fn parses_where_library_pictures_are_published_from_text_common() {
        let data = br#"
            "appinfo"
            {
                "common"
                {
                    "name" "Super Meat Boy 3D"
                    "type" "game"
                    "library_assets_full"
                    {
                        "library_capsule"
                        {
                            "image"
                            {
                                "english" "28dbb244/library_600x900.jpg"
                                "schinese" "3b1cf993/library_capsule_schinese.jpg"
                            }
                            "image2x" { "english" "28dbb244/library_600x900_2x.jpg" }
                        }
                        "library_hero" { "image" { "english" "library_hero.jpg" } }
                        "library_logo" { "image" { "english" "032d5382/logo.png" } }
                        "library_header" { "image" { "english" "d9e88f30/library_header.jpg" } }
                    }
                }
            }
        "#;

        let app = parse_app_info(3288210, data).expect("app info should parse");
        assert_eq!(
            app.library_art.capsule.as_deref(),
            Some("28dbb244/library_600x900.jpg")
        );
        assert_eq!(app.library_art.hero.as_deref(), Some("library_hero.jpg"));
        assert_eq!(app.library_art.logo.as_deref(), Some("032d5382/logo.png"));
    }

    /// An app that publishes nothing under `library_assets_full` says so with every path
    /// absent, rather than with a guess the caller would then fetch a 404 for.
    #[test]
    fn an_app_with_no_published_pictures_has_no_paths() {
        let data = br#""appinfo" { "common" { "name" "Half-Life" "type" "game" } }"#;

        let app = parse_app_info(70, data).expect("app info should parse");
        assert_eq!(app.library_art, LibraryArt::default());
    }

    /// The binary-KV path reads the same block, and takes the 2× file for a picture published
    /// only at that size.
    #[test]
    fn library_art_from_kv_reads_the_published_paths() {
        let named = |key: &str, value: &str| (key.to_owned(), KVValue::Str(value.to_owned()));
        let assets = KVValue::Nested(vec![
            (
                "library_capsule".to_owned(),
                KVValue::Nested(vec![(
                    "image".to_owned(),
                    KVValue::Nested(vec![named("english", "28dbb244/library_600x900.jpg")]),
                )]),
            ),
            (
                // Only a 2× hero: bigger than it needs to be is a picture, and nothing is not.
                "library_hero".to_owned(),
                KVValue::Nested(vec![(
                    "image2x".to_owned(),
                    KVValue::Nested(vec![named("english", "67a1c596/library_hero_2x.jpg")]),
                )]),
            ),
            (
                // A logo in Chinese only, which is not this caller's picture.
                "library_logo".to_owned(),
                KVValue::Nested(vec![(
                    "image".to_owned(),
                    KVValue::Nested(vec![named("schinese", "a9884e0b/logo_schinese.png")]),
                )]),
            ),
        ]);

        let art = library_art_from_kv(&assets);
        assert_eq!(art.capsule.as_deref(), Some("28dbb244/library_600x900.jpg"));
        assert_eq!(art.hero.as_deref(), Some("67a1c596/library_hero_2x.jpg"));
        assert_eq!(art.logo, None);
    }
}
