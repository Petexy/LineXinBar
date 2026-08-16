use std::collections::HashMap;
use std::sync::Arc;

use tokio::{
    sync::{Mutex, mpsc},
    time::{Duration, sleep, timeout},
};

use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::UnboundedSender;

use crate::{
    achievements,
    auth::{credentials, qr},
    chat,
    connection::Connection,
    emsg::EMsg,
    error::{Error, Result},
    friends::{self, PersonaState},
    library, pics,
    protobuf::{
        CAuthenticationDeviceDetails, CMsgClientHello, CMsgClientLicenseList, CMsgClientLogon,
        CMsgClientLogonResponse, CMsgProtoBufHeader, EAuthTokenPlatformType,
    },
    serverlist::ServerListCache,
    token::steamid_from_refresh_token,
};

/// Bound the authed `ClientGetLastPlayedTimes` call — short, since Steam normally replies in well
/// under a second; this only guards against a silent non-response.
const PLAYTIME_TIMEOUT: Duration = Duration::from_secs(10);
/// Bound the inline `GetUserStats` (achievements) call for the same reason.
const ACHIEVEMENTS_TIMEOUT: Duration = Duration::from_secs(10);
/// Overall bound on waiting for Steam's post-login `ClientLicenseList` push (mirrors the old
/// `GetOwnedGames` 30s bound) so a stalled/missed push fails the load instead of hanging forever.
const LICENSE_LIST_TIMEOUT: Duration = Duration::from_secs(30);
/// Bound the inline chat send/typing service calls (fast single round-trips).
const CHAT_SEND_TIMEOUT: Duration = Duration::from_secs(10);
/// Bound the chat history fetch (runs in a spawned task; emits empty on timeout so the UI clears).
const CHAT_HISTORY_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub enum RunCommand {
    SetPersonaState(PersonaState),
    RequestFriendData(Vec<u64>),
    GetLibrary,
    GetPlayerAchievements(u32),
    SendMessage { steamid: u64, message: String },
    SendTyping { steamid: u64 },
    GetRecentMessages { steamid: u64 },
}

const PROTOCOL_VERSION: u32 = 65580;
const CLIENT_LANGUAGE: &str = "english";
const CLIENT_OS_TYPE: u32 = 20;
const DEFAULT_DEVICE_NAME: &str = "Steamie";
const DEFAULT_WEBSITE_ID: &str = "Unknown";
const DEFAULT_GAMING_DEVICE_TYPE: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthMethod {
    Qr,
    Credentials { account: String, password: String },
    RefreshToken(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardKind {
    EmailCode,
    DeviceCode,
    DeviceConfirmation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedOn {
    pub steamid: u64,
    pub account_name: String,
    pub refresh_token: String,
}

#[derive(Debug)]
pub enum AuthEvent {
    QrChallenge(String),
    GuardRequired(GuardKind),
    Success(LoggedOn),
    Failure(Error),
}

#[derive(Debug)]
enum AuthCommand {
    GuardCode(String),
}

#[derive(Debug, Default)]
pub struct SteamClient {
    servers: ServerListCache,
    connection: Option<Arc<Mutex<Connection>>>,
    auth_commands: Option<mpsc::UnboundedSender<AuthCommand>>,
    account_name_hint: Option<String>,
}

impl SteamClient {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_account_name_hint(&mut self, account_name: impl Into<String>) {
        self.account_name_hint = Some(account_name.into());
    }

    pub async fn connect(&mut self) -> Result<()> {
        if let Some(connection) = &self.connection {
            if !connection.lock().await.is_closed().await {
                return Ok(());
            }
            self.connection = None;
        }

        let mut last_error = None;
        for force_refresh in [false, true] {
            let servers = self.servers.list(force_refresh).await?;
            for server in servers {
                match Connection::connect(&server.websocket_url()).await {
                    Ok(connection) => {
                        connection
                            .send_message(
                                EMsg::ClientHello,
                                &CMsgProtoBufHeader::default(),
                                &CMsgClientHello {
                                    protocol_version: Some(PROTOCOL_VERSION),
                                },
                            )
                            .await?;
                        self.connection = Some(Arc::new(Mutex::new(connection)));
                        return Ok(());
                    }
                    Err(error) => last_error = Some(error),
                }
            }
        }

        Err(last_error.unwrap_or_else(|| Error::Transport("no CM endpoints available".to_owned())))
    }

    pub async fn begin_auth(
        &mut self,
        method: AuthMethod,
    ) -> Result<mpsc::UnboundedReceiver<AuthEvent>> {
        self.connect().await?;

        let connection = self
            .connection
            .as_ref()
            .cloned()
            .ok_or_else(|| Error::Transport("connection missing after connect".to_owned()))?;
        let account_name_hint = self.account_name_hint.clone();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        self.auth_commands = Some(command_tx);

        tokio::spawn(async move {
            let result = match method {
                AuthMethod::Qr => run_qr_auth(connection, event_tx.clone()).await,
                AuthMethod::Credentials { account, password } => {
                    run_credentials_auth(
                        connection,
                        event_tx.clone(),
                        command_rx,
                        account,
                        password,
                    )
                    .await
                }
                AuthMethod::RefreshToken(refresh_token) => {
                    run_refresh_token_auth(connection, refresh_token, account_name_hint).await
                }
            };

            match result {
                Ok(logged_on) => {
                    let _ = event_tx.send(AuthEvent::Success(logged_on));
                }
                Err(error) => {
                    let _ = event_tx.send(AuthEvent::Failure(error));
                }
            }
        });

        Ok(event_rx)
    }

    pub fn submit_guard_code(&self, code: impl Into<String>) -> Result<()> {
        let sender = self
            .auth_commands
            .as_ref()
            .ok_or_else(|| Error::Authentication("no guard flow is active".to_owned()))?;
        sender
            .send(AuthCommand::GuardCode(code.into()))
            .map_err(|_| Error::Authentication("guard flow is no longer active".to_owned()))
    }

    pub async fn run(
        &mut self,
        mut commands: UnboundedReceiver<RunCommand>,
        events: UnboundedSender<crate::friends::FriendsEvent>,
    ) -> Result<()> {
        let connection = self
            .connection
            .as_ref()
            .cloned()
            .ok_or_else(|| Error::Transport("no active connection".to_owned()))?;

        // Pull the incoming receiver out so we can select over it without
        // holding &mut connection (which would conflict with send_message &self borrows).
        let mut incoming = connection.lock().await.take_incoming();

        // Set ourselves online so Steam starts pushing friend presence.
        {
            let conn = connection.lock().await;
            let state = conn.state_snapshot().await;
            let (header, body) = friends::build_change_status(&state, PersonaState::Online);
            conn.send_message(EMsg::ClientChangeStatus, &header, &body)
                .await?;
        }

        loop {
            tokio::select! {
                packet = incoming.recv() => {
                    match packet {
                        Some(Ok(pkt)) => {
                            if pkt.emsg == EMsg::ClientLicenseList.raw()
                                && let Some(package_ids) = decode_license_list_packages(&pkt)
                            {
                                let conn = connection.lock().await;
                                conn.set_package_ids(package_ids).await;
                            }

                            if let Some(event) = friends::decode(&pkt) {
                                // When the friends list arrives, immediately request persona data.
                                if let friends::FriendsEvent::FriendsList(ref friend_list) = event {
                                    let ids: Vec<u64> = friend_list.iter().map(|f| f.steamid).collect();
                                    if !ids.is_empty() {
                                        let conn = connection.lock().await;
                                        let state = conn.state_snapshot().await;
                                        let (header, body) = friends::build_request_friend_data(&state, ids);
                                        let _ = conn.send_message(EMsg::ClientRequestFriendData, &header, &body).await;
                                    }
                                }
                                let _ = events.send(event);
                            } else if let Some(event) = chat::decode_incoming(&pkt) {
                                // Unsolicited friend message / typing push (EMsg 146 ServiceMethod).
                                let _ = events.send(event);
                            }
                        }
                        Some(Err(e)) => return Err(e),
                        None => return Ok(()),
                    }
                }
                cmd = commands.recv() => {
                    match cmd {
                        Some(RunCommand::SetPersonaState(state)) => {
                            let conn = connection.lock().await;
                            let conn_state = conn.state_snapshot().await;
                            let (header, body) = friends::build_change_status(&conn_state, state);
                            conn.send_message(EMsg::ClientChangeStatus, &header, &body).await?;
                        }
                        Some(RunCommand::RequestFriendData(ids)) => {
                            let conn = connection.lock().await;
                            let conn_state = conn.state_snapshot().await;
                            let (header, body) = friends::build_request_friend_data(&conn_state, ids);
                            conn.send_message(EMsg::ClientRequestFriendData, &header, &body).await?;
                        }
                        Some(RunCommand::GetLibrary) => {
                            let connection_clone = Arc::clone(&connection);
                            let events_clone = events.clone();
                            tokio::spawn(async move {
                                match load_library(connection_clone, events_clone.clone()).await {
                                    Ok(games) => {
                                        let _ = events_clone
                                            .send(friends::FriendsEvent::OwnedGames(games));
                                    }
                                    Err(error) => {
                                        tracing::warn!("GetLibrary failed: {error}");
                                        let _ = events_clone
                                            .send(friends::FriendsEvent::OwnedGames(vec![]));
                                    }
                                }
                            });
                        }
                        Some(RunCommand::GetPlayerAchievements(appid)) => {
                            let conn = connection.lock().await;
                            let state = conn.state_snapshot().await;
                            // Runs inline in the command loop, so a hang would block every later
                            // command until disconnect. Bound it and always emit a result so the
                            // UI clears its loading state.
                            let achievements = match timeout(
                                ACHIEVEMENTS_TIMEOUT,
                                achievements::get_player_achievements(&conn, &state, appid),
                            )
                            .await
                            {
                                Ok(Ok(achievements)) => achievements,
                                Ok(Err(e)) => {
                                    tracing::warn!("GetPlayerAchievements({appid}) failed: {e}");
                                    vec![]
                                }
                                Err(_) => {
                                    tracing::warn!("GetPlayerAchievements({appid}) timed out");
                                    vec![]
                                }
                            };
                            let _ = events.send(friends::FriendsEvent::PlayerAchievements {
                                appid,
                                achievements,
                            });
                        }
                        Some(RunCommand::SendMessage { steamid, message }) => {
                            // Inline single round-trip (mirrors GetPlayerAchievements). Bounded so a
                            // silent non-response can't wedge later commands; failures are logged.
                            let conn = connection.lock().await;
                            let state = conn.state_snapshot().await;
                            match timeout(
                                CHAT_SEND_TIMEOUT,
                                chat::send_message(&conn, &state, steamid, message),
                            )
                            .await
                            {
                                Ok(Ok(sent)) => {
                                    let _ = events.send(friends::FriendsEvent::MessageSent(sent));
                                }
                                Ok(Err(e)) => tracing::warn!("SendMessage({steamid}) failed: {e}"),
                                Err(_) => tracing::warn!("SendMessage({steamid}) timed out"),
                            }
                        }
                        Some(RunCommand::SendTyping { steamid }) => {
                            // Fire-and-forget in a spawned task: typing pings are high-frequency and
                            // best-effort, so they must never block the receive path or later
                            // commands the way an inline round-trip would. Ordering is irrelevant.
                            let connection_clone = Arc::clone(&connection);
                            tokio::spawn(async move {
                                let conn = connection_clone.lock().await;
                                let state = conn.state_snapshot().await;
                                match timeout(
                                    CHAT_SEND_TIMEOUT,
                                    chat::send_typing(&conn, &state, steamid),
                                )
                                .await
                                {
                                    Ok(Ok(())) => {}
                                    Ok(Err(e)) => tracing::debug!("SendTyping({steamid}) failed: {e}"),
                                    Err(_) => tracing::debug!("SendTyping({steamid}) timed out"),
                                }
                            });
                        }
                        Some(RunCommand::GetRecentMessages { steamid }) => {
                            // Spawn (mirrors GetLibrary) so a slow history fetch can't block the
                            // select loop. Always emit a result so the UI clears its loading state.
                            let connection_clone = Arc::clone(&connection);
                            let events_clone = events.clone();
                            tokio::spawn(async move {
                                let messages = {
                                    let conn = connection_clone.lock().await;
                                    let state = conn.state_snapshot().await;
                                    match timeout(
                                        CHAT_HISTORY_TIMEOUT,
                                        chat::get_recent_messages(&conn, &state, steamid),
                                    )
                                    .await
                                    {
                                        Ok(Ok(messages)) => messages,
                                        Ok(Err(e)) => {
                                            tracing::warn!("GetRecentMessages({steamid}) failed: {e}");
                                            vec![]
                                        }
                                        Err(_) => {
                                            tracing::warn!("GetRecentMessages({steamid}) timed out");
                                            vec![]
                                        }
                                    }
                                };
                                let _ = events_clone.send(friends::FriendsEvent::RecentMessages {
                                    steamid,
                                    messages,
                                });
                            });
                        }
                        None => return Ok(()),
                    }
                }
            }
        }
    }
}

fn decode_license_list_packages(packet: &crate::message::Packet) -> Option<Vec<u32>> {
    let msg = match packet.decode_body::<CMsgClientLicenseList>() {
        Ok(msg) => msg,
        Err(error) => {
            tracing::warn!("ClientLicenseList decode failed: {error}");
            return None;
        }
    };

    let package_ids: Vec<u32> = msg
        .licenses
        .iter()
        .filter_map(|license| license.package_id)
        .filter(|package_id| *package_id != 0)
        .collect();

    tracing::info!(
        licenses = msg.licenses.len(),
        package_ids = package_ids.len(),
        "ClientLicenseList received"
    );

    Some(package_ids)
}

async fn load_library(
    connection: Arc<Mutex<Connection>>,
    events: UnboundedSender<crate::friends::FriendsEvent>,
) -> Result<Vec<friends::ProtocolGame>> {
    let package_ids = wait_for_package_ids(&connection).await?;

    let playtimes = {
        let conn = connection.lock().await;
        let state = conn.state_snapshot().await;
        // Bounded so a silently-ignored service method can't freeze the whole pipeline. On
        // timeout/error, load the library without playtime — names and icons still resolve.
        match timeout(
            PLAYTIME_TIMEOUT,
            library::get_last_played_times(&conn, &state),
        )
        .await
        {
            Ok(Ok(playtimes)) => {
                tracing::info!(
                    games = playtimes.len(),
                    "ClientGetLastPlayedTimes returned playtime data"
                );
                playtimes
            }
            Ok(Err(error)) => {
                tracing::warn!("ClientGetLastPlayedTimes failed: {error}");
                HashMap::new()
            }
            Err(_) => {
                tracing::warn!(
                    "ClientGetLastPlayedTimes timed out; loading library without playtime"
                );
                HashMap::new()
            }
        }
    };
    let recently_played = library::recently_played_games(&playtimes);
    let _ = events.send(friends::FriendsEvent::RecentlyPlayedGames(
        recently_played.clone(),
    ));

    let catalog = {
        let conn = connection.lock().await;
        let state = conn.state_snapshot().await;
        pics::load_owned_app_catalog(&conn, &state, package_ids).await?
    };

    let games = library::merge_catalog_and_playtimes(catalog, &playtimes);
    tracing::info!(
        games = games.len(),
        recently_played = recently_played.len(),
        "CM library pipeline completed"
    );

    Ok(games)
}

async fn wait_for_package_ids(connection: &Arc<Mutex<Connection>>) -> Result<Vec<u32>> {
    let notify = {
        let conn = connection.lock().await;
        conn.license_notify()
    };

    let wait = async {
        loop {
            // Register interest BEFORE checking state. `notify_waiters()` stores no permit, so a
            // notification that fires between the state read and the await would otherwise be
            // missed. Enabling the future first closes that race.
            let notified = notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            {
                let conn = connection.lock().await;
                let state = conn.state_snapshot().await;
                if let Some(reason) = state.close_reason {
                    return Err(Error::Transport(reason));
                }
                if state.license_list_received {
                    return Ok(state.package_ids);
                }
            }

            notified.await;
        }
    };

    match timeout(LICENSE_LIST_TIMEOUT, wait).await {
        Ok(result) => result,
        Err(_) => Err(Error::Transport(
            "timed out waiting for ClientLicenseList".to_owned(),
        )),
    }
}

async fn run_qr_auth(
    connection: Arc<Mutex<Connection>>,
    event_tx: mpsc::UnboundedSender<AuthEvent>,
) -> Result<LoggedOn> {
    let mut challenge = {
        let connection = connection.lock().await;
        qr::begin(
            &connection,
            DEFAULT_DEVICE_NAME,
            EAuthTokenPlatformType::KEAuthTokenPlatformTypeSteamClient as i32,
            build_device_details(),
            DEFAULT_WEBSITE_ID,
        )
        .await?
    };

    let _ = event_tx.send(AuthEvent::QrChallenge(challenge.challenge_url.clone()));

    loop {
        sleep(challenge.interval).await;
        let poll_result = {
            let connection = connection.lock().await;
            qr::poll(&connection, &mut challenge).await?
        };

        match poll_result {
            qr::PollState::Pending { challenge_changed } => {
                if challenge_changed {
                    let _ = event_tx.send(AuthEvent::QrChallenge(challenge.challenge_url.clone()));
                }
            }
            qr::PollState::Complete(completed) => {
                let mut connection = connection.lock().await;
                return log_on_with_token(
                    &mut connection,
                    &completed.refresh_token,
                    Some(completed.account_name),
                )
                .await;
            }
        }
    }
}

async fn run_credentials_auth(
    connection: Arc<Mutex<Connection>>,
    event_tx: mpsc::UnboundedSender<AuthEvent>,
    mut command_rx: mpsc::UnboundedReceiver<AuthCommand>,
    account: String,
    password: String,
) -> Result<LoggedOn> {
    let session = {
        let connection = connection.lock().await;
        credentials::begin(
            &connection,
            &account,
            &password,
            DEFAULT_DEVICE_NAME,
            build_device_details(),
            DEFAULT_WEBSITE_ID,
        )
        .await?
    };

    if let Some(kind) = session.preferred_guard_kind() {
        let _ = event_tx.send(AuthEvent::GuardRequired(kind.clone()));
        match kind {
            GuardKind::EmailCode | GuardKind::DeviceCode => {
                let AuthCommand::GuardCode(code) = command_rx
                    .recv()
                    .await
                    .ok_or_else(|| Error::Authentication("guard flow cancelled".to_owned()))?;
                let connection = connection.lock().await;
                credentials::submit_guard_code(&connection, &session, &code, kind).await?;
            }
            GuardKind::DeviceConfirmation => {}
        }
    }

    loop {
        sleep(session.interval).await;
        let poll_result = {
            let connection = connection.lock().await;
            credentials::poll(&connection, &session).await?
        };

        if let Some(completed) = poll_result {
            let mut connection = connection.lock().await;
            return log_on_with_token(
                &mut connection,
                &completed.refresh_token,
                Some(completed.account_name),
            )
            .await;
        }
    }
}

async fn run_refresh_token_auth(
    connection: Arc<Mutex<Connection>>,
    refresh_token: String,
    account_name_hint: Option<String>,
) -> Result<LoggedOn> {
    let mut connection = connection.lock().await;
    log_on_with_token(&mut connection, &refresh_token, account_name_hint).await
}

async fn log_on_with_token(
    connection: &mut Connection,
    refresh_token: &str,
    account_name: Option<String>,
) -> Result<LoggedOn> {
    let steamid = steamid_from_refresh_token(refresh_token).ok_or_else(|| {
        Error::Authentication("refresh token did not contain a valid steamid".to_owned())
    })?;
    let account_name = account_name.unwrap_or_default();

    let header = CMsgProtoBufHeader {
        steamid: Some(steamid),
        ..Default::default()
    };
    let body = CMsgClientLogon {
        protocol_version: Some(PROTOCOL_VERSION),
        client_language: Some(CLIENT_LANGUAGE.to_owned()),
        client_os_type: Some(CLIENT_OS_TYPE),
        client_supplied_steam_id: Some(steamid),
        machine_id: Some(machine_id()),
        account_name: if account_name.is_empty() {
            None
        } else {
            Some(account_name.clone())
        },
        should_remember_password: Some(true),
        supports_rate_limit_response: Some(true),
        access_token: Some(refresh_token.to_owned()),
        gaming_device_type: Some(DEFAULT_GAMING_DEVICE_TYPE),
        // Opt into Steam's "new chat" (unified ChatRoom/FriendMessages). This is the *only*
        // opt-in for real-time message pushes: without it Steam does not push
        // `FriendMessagesClient.IncomingMessage#1` to this session (matches node-steam-user, which
        // sets `chat_mode: 2`). Sending and history fetch work without it; live receive does not.
        chat_mode: Some(2),
        ..Default::default()
    };

    connection
        .send_message(EMsg::ClientLogon, &header, &body)
        .await?;

    loop {
        let packet = connection.next_event().await.ok_or(Error::Closed)??;
        if packet.emsg != EMsg::ClientLogOnResponse.raw() {
            continue;
        }

        let response = packet.decode_body::<CMsgClientLogonResponse>()?;
        if response.eresult.unwrap_or_default() != 1 {
            return Err(Error::Authentication(format!(
                "ClientLogOn failed with eresult {}",
                response.eresult.unwrap_or_default()
            )));
        }

        let client_session_id = packet.header.client_sessionid.ok_or(Error::MissingField(
            "ClientLogOnResponse proto header client_sessionid",
        ))?;
        let heartbeat_seconds = response
            .heartbeat_seconds
            .or(response.legacy_out_of_game_heartbeat_seconds)
            .ok_or(Error::MissingField(
                "CMsgClientLogonResponse.heartbeat_seconds",
            ))?;

        connection
            .set_logged_on(steamid, client_session_id, heartbeat_seconds)
            .await?;

        return Ok(LoggedOn {
            steamid,
            account_name,
            refresh_token: refresh_token.to_owned(),
        });
    }
}

fn build_device_details() -> CAuthenticationDeviceDetails {
    CAuthenticationDeviceDetails {
        device_friendly_name: Some(DEFAULT_DEVICE_NAME.to_owned()),
        platform_type: Some(EAuthTokenPlatformType::KEAuthTokenPlatformTypeSteamClient as i32),
        os_type: Some(CLIENT_OS_TYPE as i32),
        gaming_device_type: Some(DEFAULT_GAMING_DEVICE_TYPE),
        client_count: Some(1),
        machine_id: Some(machine_id()),
        app_type: None,
    }
}

fn machine_id() -> Vec<u8> {
    b"steamie".to_vec()
}
