//! The small, library-only Steam Connection Manager session.
//!
//! Authentication grants a refresh token over HTTPS. That token is not a Web
//! API key: it is sent here in `CMsgClientLogon`, after which Steam pushes the
//! account's package licenses and PICS resolves them into the game catalogue.
//!
//! Steam pushes the account's friends list on the same connection, unasked, and
//! goes on pushing persona state for the people in it — so the roster is kept
//! here rather than fetched. See [`crate::friends`], which owns everything about
//! what a row is; this file owns only *when* the two messages are read and what
//! is asked back. No chat, store, overlay or Valve client is started, and the
//! one thing ever written about the account is its own status — announced once
//! at logon, and again whenever somebody picks one off the friends panel. See
//! [`Chosen`], which is what carries a choice across a reconnect.

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use steam_cm_protocol::connection::Connection;
use steam_cm_protocol::emsg::EMsg;
use steam_cm_protocol::message::Packet;
use steam_cm_protocol::protobuf::{
    CMsgClientHello, CMsgClientLicenseList, CMsgClientLoggedOff, CMsgClientLogon,
    CMsgClientLogonResponse, CMsgProtoBufHeader,
};
use steam_cm_protocol::serverlist::ServerListCache;
use steam_cm_protocol::ProtocolGame;
use tokio::sync::mpsc;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio::time::timeout;

use crate::friends::{Roll, Roster};
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
/// And how long Steam is given to say what one game is called.
///
/// Short, because nothing waits on it and the row already says something true
/// meanwhile — and because this one is asked from the packet loop, which is
/// also the loop the licence list and the roster arrive on. A minute spent here
/// is a minute of a friends list not updating.
const NAMING_A_GAME: Duration = Duration::from_secs(15);

/// Where this session says the account stands, and where it has been asked to.
///
/// Shared with the worker rather than kept in the CM session, and that is the
/// whole reason it exists: a CM session lasts until the network hiccups, and
/// neither of these may. A reconnect reads both out of here, so a status is
/// still the status a minute after the wifi came back.
#[derive(Debug, Default, Clone, Copy)]
pub struct Stands {
    /// A status chosen at this shell that no client on this machine has worn
    /// yet.
    ///
    /// It is what this session announces while it lasts, and it is announced
    /// with `persona_set_by_user` **true**, because a person chose it. It is
    /// dropped the moment Valve's client's own record agrees with it — see
    /// [`crate::session::Owed`], which is where it survives a restart, and
    /// [`crate::client::recorded_status`], which is what agrees.
    pub owed: Option<crate::friends::Presence>,
    /// What this session last told Steam, so that a change on the other half
    /// can be noticed and matched rather than announced over and over.
    pub announced: Option<crate::friends::Presence>,
    /// What Valve's client recorded before an Offline handoff. Offline itself
    /// is not recorded, so a later different value is a newer choice made in
    /// the client's own UI and releases the otherwise unconfirmable debt.
    pub offline_record: Option<crate::friends::Presence>,
}

pub type Status = Arc<Mutex<Stands>>;

#[derive(Debug)]
pub enum Command {
    AchievementProgress {
        app_ids: Vec<u32>,
        request: u64,
    },
    Achievements {
        app_id: u32,
        request: u64,
    },
    /// Ask Steam for the account's catalogue again, now.
    Refresh,
    /// Something about a conversation: fetch its history, send a message, or
    /// say this account is typing.
    ///
    /// Carried out on a background task rather than in the packet loop — see
    /// [`Background::Chat`]. A history fetch is a round trip to Steam, and a
    /// loop that waited for one would be a loop not reading the message that
    /// arrived while it waited.
    Chat(crate::chat::Wanted),
    /// Tell Steam the account is in this state.
    Announce {
        presence: crate::friends::Presence,
        /// Whether a person chose it, as against this session matching a
        /// status the machine was already set to. It is Steam's
        /// `persona_set_by_user`, and it is the difference between a client
        /// reporting itself and somebody pressing a button.
        chosen: bool,
    },
}

#[derive(Debug)]
pub enum Event {
    AchievementProgress(crate::achievements::ProgressHeard),
    Achievements(crate::achievements::Heard),
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
    /// Who the account knows, and where each of them is, as it stands.
    ///
    /// Sent whole rather than as a change, because the whole of it is what is
    /// drawn and a shell holding a list it had to patch would be a second copy
    /// of the bookkeeping in [`Roll`]. Sent whenever anything in it moved,
    /// which after the first second of a session is one person at a time.
    Friends {
        generation: u64,
        roster: Roster,
    },
    /// One thing Steam said about a conversation.
    ///
    /// Already stamped with this session's generation and the account it is
    /// signed in as, because every one of these ends in something being drawn
    /// and none of them may be drawn on the wrong account's panel. See
    /// [`crate::chat::Heard`].
    Chat(crate::chat::Heard),
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

impl Cancel {
    /// A handle with nothing on the other end of it, for a test that has to
    /// build one of the worker's states rather than reach Steam to get one.
    #[cfg(test)]
    pub(crate) fn cancelling_nothing() -> Cancel {
        Cancel {
            cancel: watch::channel(false).0,
        }
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
pub fn start(
    stored: session::Stored,
    generation: u64,
    worker: Sender<WorkerMessage>,
    status: Status,
) -> Cancel {
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
                runtime.block_on(run(stored, generation, &thread_worker, cancelled, status))
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
    status: Status,
) -> Result<(), Failure> {
    tokio::select! {
        result = run_session(stored, generation, worker, status) => result,
        _ = cancelled.changed() => Ok(()),
    }
}

async fn run_session(
    stored: session::Stored,
    generation: u64,
    worker: &Sender<WorkerMessage>,
    status: Status,
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

    // Steam begins pushing the friends list and persona state the moment the
    // logon is answered, and it does not stop. Everything below that reads a
    // packet passes it through here first — the licence wait included, because
    // that wait is the first second of the session and the first second is when
    // the whole roster arrives.
    // Where this machine stands: what somebody chose here and no client has
    // worn yet, or failing that what Valve's client has the account set to. See
    // [`crate::status_on_this_machine`], and [`crate::friends`] for why the
    // second of those is read at all.
    let (wanted, by_the_user) = {
        let mut stands = status
            .lock()
            .expect("the account's status is never poisoned");
        let wanted = stands
            .owed
            .or_else(|| crate::status_on_this_machine(stored.steam_id))
            .unwrap_or(crate::friends::AS_QUIETLY_AS_IT_CAN);
        stands.announced = Some(wanted);
        (wanted, stands.owed.is_some())
    };
    let mut roll = Roll::about_and(stored.steam_id, wanted.is_away_from_it_all());
    // And first, the one thing this session says about itself. Without it every
    // friend reads Offline for ever — Steam does not send presence to a client
    // that has not said it is there — and the failure is silent: the rows
    // arrive complete and wrong. See [`crate::friends`], which is also where
    // the reason it announces *Invisible* is written down.
    //
    // Whatever this machine is set to: this is where a status set before the
    // network dropped comes back, and where a status the user never touched is
    // matched to the one Valve's client is showing them. See [`Stands`].
    //
    // Sent before the roster is asked for rather than after, so that the first
    // batch of persona state Steam sends is already a real one. A failure here
    // is logged and not retried: the session is otherwise sound, the library
    // still arrives, and the roster is a panel nobody has opened yet.
    {
        if let Err(error) = say_where_we_stand(&connection, wanted, by_the_user).await {
            tracing::warn!(
                %error,
                "Steam was not told this session is here; friends will all read offline"
            );
        }
    }

    // Steam may put the license list immediately before or after the logon
    // response. Keep the early one; otherwise wait for the push now.
    if package_ids.is_none() {
        match wait_for_licenses(&mut connection, &mut roll, generation, worker).await? {
            LicenseWait::Packages(packages) => package_ids = Some(packages),
            LicenseWait::Failed(reason) => library_failed(worker, generation, reason),
            LicenseWait::TimedOut => library_failed(
                worker,
                generation,
                "Steam did not send the account's license list in time.".to_string(),
            ),
        }
    }
    // From here on the packet receiver is held separately from the connection
    // handle. PICS/library requests use the handle on background tasks while
    // this loop goes on receiving presence and commands. The JoinSet owns
    // those tasks: dropping this CM generation aborts them before the
    // connection itself is dropped.
    let mut incoming = connection.take_incoming();
    let connection = Arc::new(connection);
    let mut background = JoinSet::new();
    // And that this session is the one carrying chat now. What was in flight
    // over the connection before this one never will be — its tasks went with
    // it — so the shell fails those rather than leaving them saying "sending"
    // for the rest of the session. See [`crate::chat::Conversations::heard`].
    let _ = worker.send(WorkerMessage::Cm(Event::Chat(crate::chat::Heard {
        generation,
        account: stored.steam_id,
        word: crate::chat::Word::Listening,
    })));
    let achievement_slots = Arc::new(tokio::sync::Semaphore::new(4));
    let mut latest_library = 0u64;
    let mut naming = false;
    if let Some(packages) = package_ids.as_deref() {
        start_library(&mut background, &connection, packages, &mut latest_library);
    }

    loop {
        start_naming(&mut background, &connection, &mut roll, &mut naming);
        tokio::select! {
            command = receive_commands.recv() => match command {
                Some(Command::Refresh) => {
                    if let Some(packages) = package_ids.as_deref() {
                        start_library(
                            &mut background,
                            &connection,
                            packages,
                            &mut latest_library,
                        );
                    }
                }
                Some(Command::AchievementProgress { app_ids, request }) => {
                    let connection = Arc::clone(&connection);
                    let account = stored.steam_id;
                    background.spawn(async move {
                        let state = connection.state_snapshot().await;
                        let result: Result<std::collections::BTreeMap<u32, crate::achievements::Progress>, String> = match timeout(Duration::from_secs(25),
                            steam_cm_protocol::achievements::get_progress(&connection, &state, app_ids)).await {
                            Ok(Ok(values)) => Ok(values.into_iter().filter_map(|p| {
                                let (app, unlocked, total) = (p.appid?, p.unlocked?, p.total?);
                                (unlocked <= total).then_some((app, crate::achievements::Progress {
                                    unlocked, total, fetched_at: p.cache_time.unwrap_or(0) as u64,
                                }))
                            }).collect()),
                            Ok(Err(error)) => Err(crate::achievements::failure(&error)),
                            Err(_) => Err("Steam took too long.".into()),
                        };
                        if let Ok(values) = &result {
                            let values = values.clone();
                            let _ = tokio::task::spawn_blocking(move || crate::achievements::save_progress(account, &values)).await;
                        }
                        Background::AchievementProgress(crate::achievements::ProgressHeard { generation, account, request, result })
                    });
                }
                Some(Command::Achievements { app_id, request }) => {
                    // Rapid navigation must not flood Steam while earlier pages finish.
                    let Ok(permit) = Arc::clone(&achievement_slots).try_acquire_owned() else {
                        let _ = worker.send(WorkerMessage::Cm(Event::Achievements(crate::achievements::Heard {
                            generation, account: stored.steam_id, app_id, request,
                            result: Err("Other achievement pages are still loading. Reopen this game to retry.".into()),
                        })));
                        continue;
                    };
                    let connection = Arc::clone(&connection);
                    let account = stored.steam_id;
                    background.spawn(async move {
                        let _permit = permit;
                        let state = connection.state_snapshot().await;
                        let result = match timeout(Duration::from_secs(25),
                            steam_cm_protocol::achievements::get_player_achievements(&connection, &state, app_id)).await {
                            Ok(result) => result.map_err(|e| crate::achievements::failure(&e)),
                            Err(_) => Err("Steam took too long. Reopen this game to retry.".into()),
                        };
                        let result = tokio::task::spawn_blocking(move || crate::achievements::finish(account, app_id, result))
                            .await.unwrap_or_else(|_| Err("Achievement worker stopped.".into()));
                        Background::Achievements(crate::achievements::Heard {
                            generation, account, app_id, request, result,
                        })
                    });
                }
                Some(Command::Chat(wanted)) => {
                    start_chat(
                        &mut background,
                        &connection,
                        &roll,
                        stored.steam_id,
                        generation,
                        wanted,
                        worker,
                    );
                }
                Some(Command::Announce { presence, chosen }) => {
                    if let Err(error) = say_where_we_stand(&connection, presence, chosen).await {
                        tracing::warn!(%error, "Steam would not take the status that was chosen");
                    }
                    // Offline is not a place to stand, it is leaving: the
                    // session stops taking part, and the roster it publishes
                    // empties. See [`crate::friends::Roll::went_offline`].
                    let crossed = roll.went_offline(presence.is_away_from_it_all());
                    // And coming back has to ask again. Steam sends the friends
                    // list once, on logon, so the push that fills a roster back
                    // in is one this session asks for rather than one it waits
                    // for — without this, choosing Online after Offline would
                    // leave the panel empty until the next reconnect.
                    if crossed && !roll.is_offline() {
                        let state = connection.state_snapshot().await;
                        let (header, body) = crate::friends::ask_about(&state, roll.everyone());
                        if let Err(error) = connection
                            .send_message(EMsg::ClientRequestFriendData, &header, &body)
                            .await
                        {
                            tracing::warn!(%error, "Steam could not be asked who the friends are");
                        }
                    }
                    // And say it on the panel now rather than a round trip from
                    // now. Steam echoes the account's own persona state back
                    // and that echo replaces this — see [`Roll::told_steam`],
                    // where the reason that is not two sources of truth is
                    // written down.
                    if roll.told_steam(presence) || crossed {
                        let _ = worker.send(WorkerMessage::Cm(Event::Friends {
                            generation,
                            roster: roll.roster(),
                        }));
                    }
                }
                None => return Ok(()),
            },
            packet = incoming.recv() => {
                let packet = packet
                    .ok_or_else(|| Failure::Temporary("Steam CM closed the connection".to_string()))?
                    .map_err(|error| Failure::Temporary(error.to_string()))?;
                if read_the_roster(&connection, &mut roll, &packet, generation, worker).await {
                    continue;
                }
                // A message, an echo of one of this account's own, or somebody
                // typing. Decoded and handed straight on: it is a decode and a
                // channel send, with nothing that waits on Steam, which is what
                // lets it live on this loop at all.
                if read_a_message(&packet, stored.steam_id, generation, &roll, worker) {
                    continue;
                }
                if packet.emsg == EMsg::ClientLicenseList.raw() {
                    match packages_from(&packet) {
                        Ok(packages) => {
                            package_ids = Some(packages);
                            start_library(
                                &mut background,
                                &connection,
                                package_ids.as_deref().unwrap_or_default(),
                                &mut latest_library,
                            );
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
            },
            finished = background.join_next(), if !background.is_empty() => {
                match finished {
                    Some(Ok(Background::AchievementProgress(heard))) => {
                        let _ = worker.send(WorkerMessage::Cm(Event::AchievementProgress(heard)));
                    }
                    Some(Ok(Background::Achievements(heard))) => {
                        let _ = worker.send(WorkerMessage::Cm(Event::Achievements(heard)));
                    }
                    Some(Ok(Background::Chat(Some(word)))) => {
                        let _ = worker.send(WorkerMessage::Cm(Event::Chat(crate::chat::Heard {
                            generation,
                            account: stored.steam_id,
                            word,
                        })));
                    }
                    // A typing notice, which nothing is said back about.
                    Some(Ok(Background::Chat(None))) => {}
                    Some(Ok(Background::Library { request, result }))
                        if request == latest_library =>
                    {
                        match result {
                            Ok(games) => {
                                let _ = worker.send(WorkerMessage::Cm(Event::Library {
                                    generation,
                                    games,
                                }));
                            }
                            Err(reason) => library_failed(worker, generation, reason),
                        }
                    }
                    Some(Ok(Background::Library { .. })) => {
                        tracing::debug!("discarded an older Steam library answer");
                    }
                    Some(Ok(Background::Names { asked, result })) => {
                        naming = false;
                        let moved = match result {
                            Ok(names) => {
                                tracing::info!(
                                    asked = asked.len(),
                                    named = names.len(),
                                    "asked Steam what the games friends are in are called"
                                );
                                roll.named(&asked, names)
                            }
                            Err(reason) => {
                                tracing::warn!(%reason, "Steam would not name a friend's game");
                                roll.named(&asked, Default::default())
                            }
                        };
                        if moved {
                            let _ = worker.send(WorkerMessage::Cm(Event::Friends {
                                generation,
                                roster: roll.roster(),
                            }));
                        }
                    }
                    Some(Err(error)) => {
                        tracing::warn!(%error, "a Steam CM background task stopped unexpectedly");
                    }
                    None => {}
                }
            }
        }
    }
}

enum Background {
    AchievementProgress(crate::achievements::ProgressHeard),
    Achievements(crate::achievements::Heard),
    /// One conversation request that has been answered, or failed. It carries
    /// nothing but the word to pass on: the correlation was decided when the
    /// task was started and travels inside it.
    ///
    /// `None` for a typing notice, which is the one request nothing is said
    /// back about — there is no line on the screen waiting to hear how it went,
    /// and a word built for it would have to name somebody, which it cannot: a
    /// typing notice going *out* is about this account and every other arm of
    /// [`crate::chat::Word`] is about a friend.
    Chat(Option<crate::chat::Word>),
    Library {
        request: u64,
        result: Result<Vec<ProtocolGame>, String>,
    },
    Names {
        asked: Vec<u32>,
        result: Result<std::collections::BTreeMap<u32, String>, String>,
    },
}

/// Start one conversation request, off the packet loop.
///
/// **Nothing here waits on Steam.** The whole of what runs on the caller's
/// thread is the friend check and a spawn; the round trip belongs to the task,
/// which shares the connection handle with the loop that goes on receiving
/// messages while it runs. A history fetch that blocked the loop would be a
/// panel that could not receive the reply it was waiting for.
///
/// The friend check is here rather than only at the panel because this is the
/// last place before the wire. A roster is a second or two behind Steam at the
/// best of times, and somebody who was unfriended while a message was being
/// typed must not have it sent — see [`crate::chat::Refused::NotAFriend`],
/// which is what the panel says about the same fact.
fn start_chat(
    tasks: &mut JoinSet<Background>,
    connection: &Arc<Connection>,
    roll: &Roll,
    account: u64,
    generation: u64,
    wanted: crate::chat::Wanted,
    worker: &Sender<WorkerMessage>,
) {
    use crate::chat::{Wanted, Word};
    let wanted = match may_be_asked(roll, wanted) {
        Ok(wanted) => wanted,
        Err(refused) => {
            if let Some(word) = refused {
                let _ = worker.send(WorkerMessage::Cm(Event::Chat(crate::chat::Heard {
                    generation,
                    account,
                    word,
                })));
            }
            return;
        }
    };
    let connection = Arc::clone(connection);
    tasks.spawn(async move {
        let state = connection.state_snapshot().await;
        Background::Chat(match wanted {
            Wanted::History { with, request } => Some({
                let said = match timeout(
                    A_CONVERSATION,
                    steam_cm_protocol::chat::get_recent_messages(
                        &connection,
                        &state,
                        with,
                        crate::chat::HISTORY_WANTED,
                    ),
                )
                .await
                {
                    Ok(Ok(messages)) => Ok(messages.into_iter().map(said_from).collect()),
                    Ok(Err(error)) => Err(what_steam_said(&error)),
                    Err(_) => Err(TOO_SLOW.to_string()),
                };
                Word::History {
                    with,
                    request,
                    said,
                }
            }),
            Wanted::Send {
                with,
                request,
                body,
            } => Some({
                let said = match timeout(
                    A_CONVERSATION,
                    steam_cm_protocol::chat::send_message(&connection, &state, with, body),
                )
                .await
                {
                    Ok(Ok(message)) => Ok(said_from(message)),
                    Ok(Err(error)) => Err(what_steam_said(&error)),
                    Err(_) => Err(TOO_SLOW.to_string()),
                };
                Word::Sent {
                    with,
                    request,
                    said,
                }
            }),
            Wanted::Typing { with } => {
                // Nothing is answered about a typing notice and nothing has to
                // be: it is a courtesy that expires by itself at the other end,
                // and a failure to send one is not a thing to put on a screen.
                if let Err(error) =
                    steam_cm_protocol::chat::send_typing(&connection, &state, with).await
                {
                    tracing::debug!(%error, "a typing notice did not go");
                }
                None
            }
        })
    });
}

/// Whether this may go out at all, and what to say back where it may not.
///
/// The friend check, cut out of [`start_chat`] so it can be exercised without a
/// socket — which is the whole of what it is: a roster is a second or two
/// behind Steam at the best of times, and somebody unfriended while a message
/// was being typed must not have it sent.
///
/// `Err(None)` is a refusal nothing is told about. A history nobody can fetch
/// and a typing notice nobody will read are not things to put a failure on the
/// screen for — the panel has already stopped offering to send — while a send
/// has a line on the screen waiting to hear how it went.
fn may_be_asked(
    roll: &Roll,
    wanted: crate::chat::Wanted,
) -> Result<crate::chat::Wanted, Option<crate::chat::Word>> {
    use crate::chat::{Wanted, Word};
    let with = match &wanted {
        Wanted::History { with, .. } | Wanted::Send { with, .. } | Wanted::Typing { with } => *with,
    };
    if roll.is_a_friend(with) {
        return Ok(wanted);
    }
    tracing::warn!(
        with = crate::chat::short(with),
        "a conversation with somebody who is not on the friends list was refused"
    );
    Err(match wanted {
        Wanted::Send { with, request, .. } => Some(Word::Sent {
            with,
            request,
            said: Err(crate::chat::Refused::NotAFriend.said().to_string()),
        }),
        Wanted::History { .. } | Wanted::Typing { .. } => None,
    })
}

/// Take one packet that might be a message, and say whether it was one.
///
/// A decode and a channel send, and deliberately nothing else: this runs on the
/// packet loop, so anything here that waited on Steam would stop the next
/// message arriving.
///
/// The typing notice this decodes is somebody *else* typing — the push carries
/// the friend either way — which is why a [`crate::chat::Word::Typing`] built
/// here names the friend and the one built for this session's own outgoing
/// notice names the account. Two different facts that happen to share a shape.
fn read_a_message(
    packet: &Packet,
    account: u64,
    generation: u64,
    roll: &crate::friends::Roll,
    worker: &Sender<WorkerMessage>,
) -> bool {
    use steam_cm_protocol::friends::FriendsEvent;

    // The two doors an invitation comes through, and the one everything else
    // does. Tried in this order because only one of them can answer about any
    // packet: the chat push is a unified notification and the invitation push
    // is an EMsg of its own.
    let Some(event) = steam_cm_protocol::chat::decode_incoming(packet)
        .or_else(|| steam_cm_protocol::chat::decode_invite_to_game(packet))
    else {
        return false;
    };
    let word = match event {
        FriendsEvent::IncomingMessage(message) => crate::chat::Word::Arrived {
            with: message.steamid,
            said: said_from(message),
        },
        FriendsEvent::TypingNotification { steamid } => crate::chat::Word::Typing { with: steamid },
        FriendsEvent::GameInvite(invite) => {
            // An invitation this account sent from another of its own sessions.
            // Steam echoes those exactly as it echoes a message, and an
            // invitation nobody here can accept is not a line in a
            // conversation.
            if invite.from_local {
                tracing::debug!(
                    with = crate::chat::short(invite.steamid),
                    "an invitation this account sent elsewhere was not filed"
                );
                return true;
            }
            // Which game, which the invitation does not say: whatever they are
            // playing at the moment they ask. Taken here, where the roster is,
            // and remembered with the invitation — see
            // [`crate::chat::Invite`].
            let (app_id, game) = roll.playing(invite.steamid);
            tracing::info!(
                with = crate::chat::short(invite.steamid),
                app_id,
                stamped = invite.timestamp != 0,
                "a friend asked this account to join them in a game"
            );
            crate::chat::Word::Invited {
                with: invite.steamid,
                invite: crate::chat::Invited {
                    at: invite.timestamp,
                    key: (invite.timestamp != 0)
                        .then(|| crate::chat::Key::new(invite.timestamp, invite.ordinal)),
                    connect: invite.connect_string,
                    app_id,
                    game,
                },
            }
        }
        // The decoders answer with one of those or with nothing.
        _ => return false,
    };
    let _ = worker.send(WorkerMessage::Cm(Event::Chat(crate::chat::Heard {
        generation,
        account,
        word,
    })));
    true
}

/// The protocol crate's message, as this crate's.
///
/// One place, because the three routes a message arrives by — the answer to a
/// send, the history, and the push — all come through here, and a key built
/// differently on any one of them would be the same message drawn twice.
fn said_from(message: steam_cm_protocol::chat::ChatMessage) -> crate::chat::Said {
    crate::chat::Said {
        key: crate::chat::Key::new(message.timestamp, message.ordinal),
        body: message.message,
        from_me: message.from_local,
    }
}

/// What to put on the screen about a conversation that did not work.
///
/// Never the protocol error verbatim: those carry endpoints and job numbers,
/// which are not something to write across a panel, and one of them is
/// `Transport` with the CM's own close reason in it. What the user needs is
/// what to do next, which is a different sentence for each of the answers Steam
/// actually gives.
///
/// **The two numbers are measured, not looked up.** Steam publishes nothing
/// about either and its answer is a bare `EResult`; both were established
/// against the live service on 2026-09-03, sending to a second account of the
/// user's own:
///
/// - **84 is too fast.** Two sends back to back are taken and the next is
///   refused *whatever its length* — a twenty-character message was refused
///   immediately after a four-thousand-character one had been accepted, and the
///   same lengths went through a minute later.
/// - **25 is too long.** Twenty thousand characters, on a connection that had
///   accepted five thousand.
///
/// Telling them apart matters because they ask opposite things of the user: one
/// message has to be shortened and the other has only to be sent again, and a
/// panel that said "Steam would not do that" to both would be no help with
/// either.
fn what_steam_said(error: &steam_cm_protocol::error::Error) -> String {
    use steam_cm_protocol::error::Error;
    tracing::warn!(%error, "Steam would not answer about a conversation");
    match error {
        Error::Closed | Error::Transport(_) => "Steam is not answering. Try again.".to_string(),
        Error::Refused { result: 84, .. } => {
            "Steam is rate-limiting messages. Try again in a moment.".to_string()
        }
        Error::Refused { result: 25, .. } => "That message is too long for Steam.".to_string(),
        _ => "Steam would not do that.".to_string(),
    }
}

/// How long Steam is given to answer about a conversation.
///
/// Twenty seconds: long enough for a history fetch on a slow connection, and
/// short enough that a send which is never going to be answered turns into a
/// message the user can try again rather than a spinner they have to guess
/// about. Nothing waits on this — it is a background task — so the number is
/// about how long a line on the screen says "sending".
const A_CONVERSATION: Duration = Duration::from_secs(20);

/// And what it says when it does not.
const TOO_SLOW: &str = "Steam did not answer in time. Try again.";

fn start_library(
    tasks: &mut JoinSet<Background>,
    connection: &Arc<Connection>,
    packages: &[u32],
    latest: &mut u64,
) {
    *latest = latest.wrapping_add(1);
    let request = *latest;
    let packages = packages.to_vec();
    let connection = Arc::clone(connection);
    tasks.spawn(async move {
        Background::Library {
            request,
            result: load_library(&connection, &packages).await,
        }
    });
}

fn start_naming(
    tasks: &mut JoinSet<Background>,
    connection: &Arc<Connection>,
    roll: &mut Roll,
    naming: &mut bool,
) {
    if *naming {
        return;
    }
    let asked = roll.games_to_name();
    if asked.is_empty() {
        return;
    }
    roll.began_naming(&asked);
    *naming = true;
    let connection = Arc::clone(connection);
    tasks.spawn(async move {
        let state = connection.state_snapshot().await;
        let result = match timeout(
            NAMING_A_GAME,
            steam_cm_protocol::pics::app_names(&connection, &state, &asked),
        )
        .await
        {
            Ok(Ok(names)) => Ok(names.into_iter().collect()),
            Ok(Err(error)) => Err(error.to_string()),
            Err(_) => Err("Steam took too long to name a friend's game".to_string()),
        };
        Background::Names { asked, result }
    });
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
                // Steam's "new chat" mode, and the **only** opt-in there is for
                // real-time friend messages: without it `SendMessage` and
                // `GetRecentMessages` still work and
                // `FriendMessagesClient.IncomingMessage#1` is never pushed at
                // this session at all, so a conversation would show what this
                // shell said and nothing that was said back. It costs nothing
                // else — the roster and the licence list arrive exactly as
                // before. See [`crate::chat`].
                chat_mode: Some(2),
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

/// Take one packet's worth of the roster, and say whether it was one.
///
/// `true` means the packet was about people rather than about the account's
/// licences or its session, and the caller has nothing left to do with it.
///
/// Two messages, and the order between them is the substance. A friends list is
/// a list of *numbers*: it is answered by asking Steam to send persona state
/// for every one of them and for the account itself, which is what turns a
/// number into a row. Persona state then goes on arriving unasked for the rest
/// of the session — somebody signing in, starting a game, or changing their
/// name — and each batch is folded into the roster the shell already has.
///
/// A request that could not be sent is logged and not retried here. The roster
/// is still whatever it was, the rows Steam has already described are still
/// drawn, and the next push repairs it; a connection that has actually gone
/// away is noticed by the read that follows this one, which is where a session
/// ends.
async fn read_the_roster(
    connection: &Connection,
    roll: &mut Roll,
    packet: &Packet,
    generation: u64,
    worker: &Sender<WorkerMessage>,
) -> bool {
    use steam_cm_protocol::friends::FriendsEvent;

    let Some(event) = steam_cm_protocol::friends::decode(packet) else {
        return false;
    };
    let moved = match event {
        FriendsEvent::FriendsList(listed) => {
            let people = roll.listed(&listed.friends, listed.incremental);
            tracing::info!(
                friends = people.len().saturating_sub(1),
                "Steam sent the friends list"
            );
            let state = connection.state_snapshot().await;
            let (header, body) = crate::friends::ask_about(&state, people);
            if let Err(error) = connection
                .send_message(EMsg::ClientRequestFriendData, &header, &body)
                .await
            {
                tracing::warn!(%error, "Steam could not be asked who the friends are");
            }
            // The ids alone are rows with no names on them yet, and they are
            // worth drawing: the panel gets its length now and fills in over
            // the second after.
            true
        }
        FriendsEvent::PersonaStates(personas) => roll.heard(&personas),
        // Everything else the protocol crate folds under this event is a
        // conversation — messages, typing, history — and this session opens
        // none. `chat_mode` is 0 in the logon above, so Steam sends none of it.
        _ => false,
    };
    if moved {
        let _ = worker.send(WorkerMessage::Cm(Event::Friends {
            generation,
            roster: roll.roster(),
        }));
    }
    true
}

/// Send one `ClientChangeStatus`, and say so in the log.
///
/// One function for the two places that announce — the session coming up, and
/// somebody choosing off the panel — so that the message is built in one place
/// and the log reads the same for both. See [`crate::friends::announce_ourselves`].
async fn say_where_we_stand(
    connection: &Connection,
    presence: crate::friends::Presence,
    chosen: bool,
) -> Result<(), steam_cm_protocol::error::Error> {
    let state = connection.state_snapshot().await;
    let (header, body) = crate::friends::announce_ourselves(&state, presence, chosen);
    connection
        .send_message(EMsg::ClientChangeStatus, &header, &body)
        .await?;
    tracing::info!(
        status = presence.said(),
        chosen,
        "told Steam where this session stands"
    );
    Ok(())
}

enum LicenseWait {
    Packages(Vec<u32>),
    Failed(String),
    TimedOut,
}

async fn wait_for_licenses(
    connection: &mut Connection,
    roll: &mut Roll,
    generation: u64,
    worker: &Sender<WorkerMessage>,
) -> Result<LicenseWait, Failure> {
    let waiting = async {
        loop {
            let packet = connection
                .next_event()
                .await
                .ok_or_else(|| Failure::Temporary("Steam CM closed".to_string()))?
                .map_err(|error| Failure::Temporary(error.to_string()))?;
            // The friends list arrives in this window more often than not, and
            // a wait that dropped it would leave the panel empty until somebody
            // in the list next changed their mind about something.
            if read_the_roster(&*connection, roll, &packet, generation, worker).await {
                continue;
            }
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

async fn load_library(
    connection: &Connection,
    package_ids: &[u32],
) -> Result<Vec<ProtocolGame>, String> {
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
        Ok(Ok(games)) => Ok(games),
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err("Steam's library request timed out.".to_string()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::{Wanted, Word};

    const A_FRIEND: u64 = 76_561_198_000_000_002;
    const A_STRANGER: u64 = 76_561_198_000_000_009;

    fn a_roll() -> Roll {
        let mut roll = Roll::about(76_561_198_000_000_001);
        roll.listed(
            &[steam_cm_protocol::friends::Friend {
                steamid: A_FRIEND,
                relationship: 3,
            }],
            false,
        );
        roll
    }

    /// A message to somebody who is still a friend goes out.
    #[test]
    fn a_message_to_a_friend_is_allowed() {
        let roll = a_roll();
        let wanted = Wanted::Send {
            with: A_FRIEND,
            request: 7,
            body: "hello".to_string(),
        };
        assert_eq!(may_be_asked(&roll, wanted.clone()), Ok(wanted));
    }

    /// And one to somebody who has left the list does not — and the line on the
    /// screen waiting for it is told why rather than left saying "sending".
    #[test]
    fn a_message_to_somebody_who_is_no_longer_a_friend_is_refused() {
        let roll = a_roll();
        let refused = may_be_asked(
            &roll,
            Wanted::Send {
                with: A_STRANGER,
                request: 7,
                body: "hello".to_string(),
            },
        );
        let Err(Some(Word::Sent {
            with,
            request,
            said,
        })) = refused
        else {
            panic!("a send to a stranger was allowed: {refused:?}");
        };
        assert_eq!(with, A_STRANGER);
        assert_eq!(request, 7);
        assert_eq!(
            said,
            Err(crate::chat::Refused::NotAFriend.said().to_string())
        );
    }

    /// The check follows the roster: somebody unfriended mid-session stops
    /// being writable the moment Steam's incremental list says so.
    #[test]
    fn unfriending_somebody_closes_the_conversation_to_writing() {
        let mut roll = a_roll();
        assert!(may_be_asked(&roll, Wanted::Typing { with: A_FRIEND }).is_ok());
        // Steam's own patch: one id, no longer a friend.
        roll.listed(
            &[steam_cm_protocol::friends::Friend {
                steamid: A_FRIEND,
                relationship: 0,
            }],
            true,
        );
        assert_eq!(
            may_be_asked(&roll, Wanted::Typing { with: A_FRIEND }),
            Err(None),
            "a typing notice to somebody who has left is a refusal nobody is told about"
        );
        assert_eq!(
            may_be_asked(
                &roll,
                Wanted::History {
                    with: A_FRIEND,
                    request: 1
                }
            ),
            Err(None)
        );
    }

    /// The shape of the session loop, which is what keeps a message arriving
    /// while a PICS lookup or a whole library is outstanding.
    ///
    /// Not the loop itself — that wants a socket and an account — but the two
    /// facts it is built on, asserted together because either alone is
    /// worthless: **the slow work is on a `JoinSet`**, so it is not on the
    /// thread that reads, and **the packet receiver is held apart from the
    /// connection**, so the reading can go on while the connection is shared
    /// with those tasks. Take either away and a friend's message waits behind a
    /// library.
    ///
    /// The regression this guards is a real one and it was in this file: the
    /// PICS naming lookup used to be awaited from the packet arm, and a Steam
    /// that took its full fifteen seconds to name a game was fifteen seconds of
    /// nothing arriving.
    #[tokio::test]
    async fn a_slow_background_job_does_not_stop_a_message_arriving() {
        let mut background: JoinSet<u32> = JoinSet::new();
        let (packets, mut incoming) = mpsc::unbounded_channel::<u32>();
        // A library load, or a PICS lookup: seconds of waiting on Steam.
        background.spawn(async {
            tokio::time::sleep(Duration::from_secs(30)).await;
            1
        });
        // A message arrives while it is outstanding.
        packets.send(42).expect("the loop is listening");
        let read = timeout(Duration::from_millis(250), async {
            loop {
                tokio::select! {
                    packet = incoming.recv() => return packet,
                    finished = background.join_next(), if !background.is_empty() => {
                        assert!(finished.is_none(), "the slow job cannot have finished");
                    }
                }
            }
        })
        .await;
        assert_eq!(
            read,
            Ok(Some(42)),
            "a message waited behind a background job"
        );
    }
}
