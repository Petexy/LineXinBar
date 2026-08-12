//! The small, library-only Steam Connection Manager session.
//!
//! Authentication grants a refresh token over HTTPS. That token is not a Web
//! API key: it is sent here in `CMsgClientLogon`, after which Steam pushes the
//! account's package licenses and PICS resolves them into the game catalogue.
//! No persona state, friends, chat, store, overlay or Valve client is started.

use std::sync::mpsc::Sender;
use std::time::Duration;

use steam_cm_protocol::connection::Connection;
use steam_cm_protocol::emsg::EMsg;
use steam_cm_protocol::protobuf::{
    CMsgClientHello, CMsgClientLicenseList, CMsgClientLoggedOff, CMsgClientLogon,
    CMsgClientLogonResponse, CMsgProtoBufHeader,
};
use steam_cm_protocol::serverlist::ServerListCache;
use steam_cm_protocol::ProtocolGame;
use tokio::sync::mpsc;
use tokio::sync::watch;
use tokio::time::timeout;

use crate::session;
use crate::WorkerMessage;

const PROTOCOL_VERSION: u32 = 65_580;
/// Steam's `EOSType::LinuxUnknown`. The protobuf field is unsigned for legacy
/// reasons, so the negative enum value is carried as its two's-complement bits.
const LINUX_OS_TYPE: u32 = (-203_i32) as u32;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const LOGON_TIMEOUT: Duration = Duration::from_secs(30);
const LICENSE_TIMEOUT: Duration = Duration::from_secs(30);
const LIBRARY_TIMEOUT: Duration = Duration::from_secs(3 * 60);

#[derive(Debug)]
pub enum Command {
    /// Ask Steam for the account's catalogue again, now.
    Refresh,
}

#[derive(Debug)]
pub enum Event {
    Ready {
        generation: u64,
        commands: mpsc::UnboundedSender<Command>,
    },
    Library {
        generation: u64,
        games: Vec<ProtocolGame>,
    },
    LibraryFailed {
        generation: u64,
        reason: String,
    },
    /// Steam's answer about one app. `Err` covers both a refusal and a
    /// failure to ask; either way the game does not start.
    /// Everything Steam had to say about fetching one app.
    Ended {
        generation: u64,
        failure: Failure,
    },
}

#[derive(Debug)]
pub enum Failure {
    Rejected(i32),
    Temporary(String),
}

/// The lifetime of one CM attempt. Dropping the shell state that owns this
/// handle also drops the network future, its connection, heartbeat and any
/// PICS request currently in flight.
#[derive(Debug)]
pub struct Cancel {
    cancel: watch::Sender<bool>,
}

impl Drop for Cancel {
    fn drop(&mut self) {
        let _ = self.cancel.send(true);
    }
}

impl Failure {
    /// Results that mean the credential itself cannot establish another CM
    /// session. Unknown results are deliberately kept and retried: a protocol
    /// change must never erase an account token merely because we do not yet
    /// recognise its number.
    pub fn permanently_rejected(&self) -> bool {
        matches!(
            self,
            Failure::Rejected(
                5 | 15 | 26 | 27 | 56 | 61 | 63 | 65 | 66 | 69 | 71 | 74 | 77 | 85 | 126
            )
        )
    }

    pub fn said(&self) -> String {
        match self {
            Failure::Rejected(result) => {
                format!("Steam did not accept this sign-in (result {result}).")
            }
            Failure::Temporary(_) => {
                "Steam could not connect to the account. It will try again.".to_string()
            }
        }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Rejected(result) => write!(f, "Steam CM logon result {result}"),
            Failure::Temporary(reason) => f.write_str(reason),
        }
    }
}

/// Start one continuously-driven CM session on its own runtime thread.
pub fn start(stored: session::Stored, generation: u64, worker: Sender<WorkerMessage>) -> Cancel {
    let (cancel, cancelled) = watch::channel(false);
    let thread_worker = worker.clone();
    let spawn = std::thread::Builder::new()
        .name("lxb-steam-cm".to_string())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    ended(
                        &thread_worker,
                        generation,
                        Failure::Temporary(format!("could not start the Steam runtime: {error}")),
                    );
                    return;
                }
            };

            if let Err(failure) =
                runtime.block_on(run(stored, generation, &thread_worker, cancelled))
            {
                ended(&thread_worker, generation, failure);
            }
        });

    if let Err(error) = spawn {
        ended(
            &worker,
            generation,
            Failure::Temporary(format!("could not start the Steam worker: {error}")),
        );
    }
    Cancel { cancel }
}

fn ended(worker: &Sender<WorkerMessage>, generation: u64, failure: Failure) {
    let _ = worker.send(WorkerMessage::Cm(Event::Ended {
        generation,
        failure,
    }));
}

async fn run(
    stored: session::Stored,
    generation: u64,
    worker: &Sender<WorkerMessage>,
    mut cancelled: watch::Receiver<bool>,
) -> Result<(), Failure> {
    tokio::select! {
        result = run_session(stored, generation, worker) => result,
        _ = cancelled.changed() => Ok(()),
    }
}

async fn run_session(
    stored: session::Stored,
    generation: u64,
    worker: &Sender<WorkerMessage>,
) -> Result<(), Failure> {
    let (mut connection, LoggedOn { mut package_ids }) = connect_and_log_on(&stored).await?;
    let (commands, mut receive_commands) = mpsc::unbounded_channel();
    if worker
        .send(WorkerMessage::Cm(Event::Ready {
            generation,
            commands,
        }))
        .is_err()
    {
        return Ok(());
    }

    // Steam may put the license list immediately before or after the logon
    // response. Keep the early one; otherwise wait for the push now.
    if package_ids.is_none() {
        match wait_for_licenses(&mut connection).await? {
            LicenseWait::Packages(packages) => package_ids = Some(packages),
            LicenseWait::Failed(reason) => library_failed(worker, generation, reason),
            LicenseWait::TimedOut => library_failed(
                worker,
                generation,
                "Steam did not send the account's license list in time.".to_string(),
            ),
        }
    }
    if let Some(packages) = package_ids.as_deref() {
        refresh_library(&connection, packages, generation, worker).await;
    }
    // Filled in the first time a download is planned, and emptied whenever
    // Steam says the account's licences have changed.

    loop {
        tokio::select! {
            command = receive_commands.recv() => match command {
                Some(Command::Refresh) => {
                    if let Some(packages) = package_ids.as_deref() {
                        refresh_library(&connection, packages, generation, worker).await;
                    }
                }
                None => return Ok(()),
            },
            packet = connection.next_event() => {
                let packet = packet
                    .ok_or_else(|| Failure::Temporary("Steam CM closed the connection".to_string()))?
                    .map_err(|error| Failure::Temporary(error.to_string()))?;
                if packet.emsg == EMsg::ClientLicenseList.raw() {
                    match packages_from(&packet) {
                        Ok(packages) => {
                            package_ids = Some(packages);
                            refresh_library(
                                &connection,
                                package_ids.as_deref().unwrap_or_default(),
                                generation,
                                worker,
                            ).await;
                        }
                        Err(reason) => library_failed(worker, generation, reason),
                    }
                } else if packet.emsg == 757 { // EMsg::ClientLoggedOff
                    let logged_off = packet
                        .decode_body::<CMsgClientLoggedOff>()
                        .map_err(|error| Failure::Temporary(error.to_string()))?;
                    let result = logged_off.eresult.unwrap_or_default();
                    // Steam ends a session rather than answering when it
                    // objects to something that was asked. The result is the
                    // only thing it says about why, so say it out loud.
                    tracing::warn!(result, "Steam logged this session off");
                    return Err(Failure::Rejected(result));
                }
            }
        }
    }
}

/// What a logon settles besides the fact of being signed in: the packages the
/// account holds, when Steam volunteers them early, and the part of the network
/// Steam thinks this machine is nearest to — which is what content servers are
/// chosen by.
struct LoggedOn {
    package_ids: Option<Vec<u32>>,
}

async fn connect_and_log_on(stored: &session::Stored) -> Result<(Connection, LoggedOn), Failure> {
    let servers = ServerListCache::new();
    let mut last_error = "Steam returned no CM endpoints".to_string();

    for force_refresh in [false, true] {
        let listed = timeout(CONNECT_TIMEOUT, servers.list(force_refresh))
            .await
            .map_err(|_| Failure::Temporary("Steam CM discovery timed out".to_string()))?
            .map_err(|error| Failure::Temporary(error.to_string()))?;

        for server in listed {
            match timeout(
                CONNECT_TIMEOUT,
                Connection::connect(&server.websocket_url()),
            )
            .await
            {
                Ok(Ok(mut connection)) => match log_on(&mut connection, stored).await {
                    Ok(logged_on) => return Ok((connection, logged_on)),
                    // Steam uses this result to move a client off one CM.
                    // Trying the same first endpoint again forever defeats
                    // the instruction; continue through the discovered list.
                    Err(Failure::Rejected(48)) => {
                        last_error = format!("Steam CM {} asked for another CM", server.endpoint)
                    }
                    Err(Failure::Rejected(result)) => return Err(Failure::Rejected(result)),
                    Err(Failure::Temporary(error)) => last_error = error,
                },
                Ok(Err(error)) => last_error = error.to_string(),
                Err(_) => last_error = format!("Steam CM {} timed out", server.endpoint),
            }
        }
    }

    Err(Failure::Temporary(last_error))
}

async fn log_on(
    connection: &mut Connection,
    stored: &session::Stored,
) -> Result<LoggedOn, Failure> {
    connection
        .send_message(
            EMsg::ClientHello,
            &CMsgProtoBufHeader::default(),
            &CMsgClientHello {
                protocol_version: Some(PROTOCOL_VERSION),
            },
        )
        .await
        .map_err(|error| Failure::Temporary(error.to_string()))?;

    connection
        .send_message(
            EMsg::ClientLogon,
            &CMsgProtoBufHeader {
                steamid: Some(stored.steam_id),
                ..Default::default()
            },
            &CMsgClientLogon {
                protocol_version: Some(PROTOCOL_VERSION),
                client_language: Some("english".to_string()),
                client_os_type: Some(LINUX_OS_TYPE),
                client_supplied_steam_id: Some(stored.steam_id),
                machine_id: Some(stored.machine_id.as_bytes().to_vec()),
                machine_name_userchosen: Some("LineXinBar".to_string()),
                account_name: Some(stored.account.clone()),
                should_remember_password: Some(true),
                supports_rate_limit_response: Some(true),
                access_token: Some(stored.refresh_token.clone()),
                gaming_device_type: Some(1),
                chat_mode: Some(0),
                qos_level: Some(2),
                ..Default::default()
            },
        )
        .await
        .map_err(|error| Failure::Temporary(error.to_string()))?;

    let handshake = async {
        let mut packages = None;
        loop {
            let packet = connection
                .next_event()
                .await
                .ok_or_else(|| Failure::Temporary("Steam CM closed during logon".to_string()))?
                .map_err(|error| Failure::Temporary(error.to_string()))?;

            if packet.emsg == EMsg::ClientLicenseList.raw() {
                match packages_from(&packet) {
                    Ok(list) => packages = Some(list),
                    Err(reason) => tracing::warn!(%reason, "Steam sent an unusable license list"),
                }
                continue;
            }
            if packet.emsg != EMsg::ClientLogOnResponse.raw() {
                continue;
            }

            let response = packet
                .decode_body::<CMsgClientLogonResponse>()
                .map_err(|error| Failure::Temporary(error.to_string()))?;
            let result = response.eresult.unwrap_or_default();
            if result != 1 {
                return Err(Failure::Rejected(result));
            }
            let session_id = packet.header.client_sessionid.ok_or_else(|| {
                Failure::Temporary("Steam's logon response had no session id".to_string())
            })?;
            let heartbeat = response
                .heartbeat_seconds
                .or(response.legacy_out_of_game_heartbeat_seconds)
                .filter(|seconds| *seconds > 0)
                .ok_or_else(|| {
                    Failure::Temporary("Steam's logon response had no heartbeat".to_string())
                })?;
            connection
                .set_logged_on(stored.steam_id, session_id, heartbeat)
                .await
                .map_err(|error| Failure::Temporary(error.to_string()))?;
            return Ok(LoggedOn {
                package_ids: packages,
            });
        }
    };

    timeout(LOGON_TIMEOUT, handshake)
        .await
        .map_err(|_| Failure::Temporary("Steam CM logon timed out".to_string()))?
}

enum LicenseWait {
    Packages(Vec<u32>),
    Failed(String),
    TimedOut,
}

async fn wait_for_licenses(connection: &mut Connection) -> Result<LicenseWait, Failure> {
    let waiting = async {
        loop {
            let packet = connection
                .next_event()
                .await
                .ok_or_else(|| Failure::Temporary("Steam CM closed".to_string()))?
                .map_err(|error| Failure::Temporary(error.to_string()))?;
            if packet.emsg == EMsg::ClientLicenseList.raw() {
                return Ok(match packages_from(&packet) {
                    Ok(packages) => LicenseWait::Packages(packages),
                    Err(reason) => LicenseWait::Failed(reason),
                });
            }
            if packet.emsg == 757 {
                let logged_off = packet
                    .decode_body::<CMsgClientLoggedOff>()
                    .map_err(|error| Failure::Temporary(error.to_string()))?;
                return Err(Failure::Rejected(logged_off.eresult.unwrap_or_default()));
            }
        }
    };

    match timeout(LICENSE_TIMEOUT, waiting).await {
        Ok(result) => result,
        Err(_) => Ok(LicenseWait::TimedOut),
    }
}

fn packages_from(packet: &steam_cm_protocol::message::Packet) -> Result<Vec<u32>, String> {
    let licenses = packet
        .decode_body::<CMsgClientLicenseList>()
        .map_err(|error| error.to_string())?;
    let result = licenses.eresult.unwrap_or_default();
    if result != 1 {
        return Err(format!("Steam license-list result {result}"));
    }

    let mut packages: Vec<u32> = licenses
        .licenses
        .into_iter()
        // Bit 3 is `Expired`; it is a historical entitlement, not an owned
        // package the account can fetch today.
        .filter(|license| license.flags.unwrap_or_default() & 8 == 0)
        .filter_map(|license| license.package_id)
        .filter(|package| *package != 0)
        .collect();
    packages.sort_unstable();
    packages.dedup();
    Ok(packages)
}

async fn refresh_library(
    connection: &Connection,
    package_ids: &[u32],
    generation: u64,
    worker: &Sender<WorkerMessage>,
) {
    let loading = async {
        let state = connection.state_snapshot().await;
        let playtimes = match timeout(
            Duration::from_secs(12),
            steam_cm_protocol::library::get_last_played_times(connection, &state),
        )
        .await
        {
            Ok(Ok(playtimes)) => playtimes,
            Ok(Err(error)) => {
                tracing::warn!(%error, "Steam playtime could not be read; keeping the catalogue");
                Default::default()
            }
            Err(_) => {
                tracing::warn!("Steam playtime timed out; keeping the catalogue");
                Default::default()
            }
        };
        let catalogue = steam_cm_protocol::pics::load_owned_app_catalog(
            connection,
            &state,
            package_ids.to_vec(),
        )
        .await?;
        Ok::<_, steam_cm_protocol::Error>(steam_cm_protocol::library::merge_catalog_and_playtimes(
            catalogue, &playtimes,
        ))
    };

    match timeout(LIBRARY_TIMEOUT, loading).await {
        Ok(Ok(games)) => {
            let _ = worker.send(WorkerMessage::Cm(Event::Library { generation, games }));
        }
        Ok(Err(error)) => library_failed(worker, generation, error.to_string()),
        Err(_) => library_failed(
            worker,
            generation,
            "Steam's library request timed out.".to_string(),
        ),
    }
}

/// Ask Steam for the ownership ticket that lets one app start.
///
/// A result other than success means the account does not own the app, or
/// that Steam declined to say — and both are the same thing to the caller,
/// because in neither case has ownership been established. There is no
/// fallback path and no cached second answer: an unanswered question is a
/// game that does not start.
fn library_failed(worker: &Sender<WorkerMessage>, generation: u64, reason: String) {
    let _ = worker.send(WorkerMessage::Cm(Event::LibraryFailed {
        generation,
        reason,
    }));
}
