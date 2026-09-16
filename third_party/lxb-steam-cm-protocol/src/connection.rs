use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use futures_util::{SinkExt, StreamExt, stream::SplitSink};
use prost::Message;
use tokio::{
    sync::{Mutex, Notify, RwLock, mpsc, oneshot},
    task::JoinHandle,
    time,
};
use tokio_tungstenite::tungstenite::Message as WebSocketMessage;

use crate::{
    emsg::EMsg,
    error::{Error, Result},
    message::{NO_JOB_ID, Packet, decode_frame, encode_message},
    protobuf::{CMsgClientHeartBeat, CMsgProtoBufHeader},
    transport::websocket::{SteamWebSocket, connect},
};

type PendingJobs = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Packet>>>>>;
type PendingStreams = Arc<Mutex<HashMap<u64, mpsc::UnboundedSender<Result<Packet>>>>>;
type IncomingEvents = mpsc::UnboundedReceiver<Result<Packet>>;
type WriteHalf = SplitSink<SteamWebSocket, WebSocketMessage>;

#[derive(Debug, Default, Clone)]
pub struct ConnectionState {
    pub steamid: Option<u64>,
    pub client_session_id: Option<i32>,
    pub heartbeat_seconds: Option<i32>,
    pub close_reason: Option<String>,
    pub license_list_received: bool,
    pub package_ids: Vec<u32>,
}

#[derive(Debug)]
pub struct Connection {
    sender: Arc<Mutex<WriteHalf>>,
    pending_jobs: PendingJobs,
    pending_streams: PendingStreams,
    incoming: IncomingEvents,
    next_job_id: AtomicU64,
    state: Arc<RwLock<ConnectionState>>,
    license_notify: Arc<Notify>,
    read_task: JoinHandle<()>,
    heartbeat_task: Option<JoinHandle<()>>,
}

impl Connection {
    pub async fn connect(url: &str) -> Result<Self> {
        let socket = connect(url).await?;
        let (writer, mut reader) = socket.split();
        let sender = Arc::new(Mutex::new(writer));
        let pending_jobs = Arc::new(Mutex::new(
            HashMap::<u64, oneshot::Sender<Result<Packet>>>::new(),
        ));
        let pending_streams = Arc::new(Mutex::new(HashMap::<
            u64,
            mpsc::UnboundedSender<Result<Packet>>,
        >::new()));
        let state = Arc::new(RwLock::new(ConnectionState::default()));
        let license_notify = Arc::new(Notify::new());
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        let pending_jobs_for_read = Arc::clone(&pending_jobs);
        let pending_streams_for_read = Arc::clone(&pending_streams);
        let state_for_read = Arc::clone(&state);

        let read_task = tokio::spawn(async move {
            while let Some(frame) = reader.next().await {
                let binary = match frame {
                    Ok(WebSocketMessage::Binary(payload)) => payload,
                    Ok(WebSocketMessage::Close(frame)) => {
                        // Carry the close frame's code and reason. Steam uses
                        // them to say why it hung up, and without them every
                        // server-side refusal looks like a dropped network.
                        let said = match &frame {
                            Some(frame) if frame.reason.is_empty() => {
                                format!("Steam CM closed the connection (code {})", frame.code)
                            }
                            Some(frame) => format!(
                                "Steam CM closed the connection (code {}: {})",
                                frame.code, frame.reason
                            ),
                            None => "Steam CM closed the connection with no reason".to_string(),
                        };
                        mark_closed(&state_for_read, said.clone()).await;
                        fail_pending_jobs(
                            &pending_jobs_for_read,
                            &pending_streams_for_read,
                            Error::Transport(said),
                        )
                        .await;
                        let _ = incoming_tx.send(Err(Error::Closed));
                        break;
                    }
                    Ok(WebSocketMessage::Ping(_))
                    | Ok(WebSocketMessage::Pong(_))
                    | Ok(WebSocketMessage::Text(_))
                    | Ok(WebSocketMessage::Frame(_)) => {
                        continue;
                    }
                    Err(error) => {
                        let message = error.to_string();
                        let wrapped = Error::from(error);
                        mark_closed(&state_for_read, message.clone()).await;
                        fail_pending_jobs(
                            &pending_jobs_for_read,
                            &pending_streams_for_read,
                            Error::Transport(message),
                        )
                        .await;
                        let _ = incoming_tx.send(Err(wrapped));
                        break;
                    }
                };

                match decode_frame(&binary) {
                    Ok(packets) => {
                        for packet in packets {
                            tracing::debug!(
                                emsg = packet.emsg,
                                jobid_target = ?packet.header.jobid_target,
                                "Steam CM sent a message"
                            );
                            // Server-initiated service notifications (`ServiceMethod` 146, e.g.
                            // `FriendMessagesClient.IncomingMessage#1`, and the rarer
                            // `ServiceMethodSendToClient` 152) are pushes, not responses to a
                            // pending request. Never route them to pending jobs even if jobid_target
                            // happens to match — doing so would consume the pending slot and silently
                            // drop the real response. (Responses are `ServiceMethodResponse` 147.)
                            let is_server_push = packet.emsg
                                == crate::emsg::EMsg::ServiceMethod.raw()
                                || packet.emsg
                                    == crate::emsg::EMsg::ServiceMethodSendToClient.raw();

                            if !is_server_push && let Some(job_id) = packet.jobid_target() {
                                let waiter = {
                                    let mut pending = pending_jobs_for_read.lock().await;
                                    pending.remove(&job_id)
                                };
                                if let Some(waiter) = waiter {
                                    let _ = waiter.send(Ok(packet));
                                    continue;
                                }

                                let stream = {
                                    let pending = pending_streams_for_read.lock().await;
                                    pending.get(&job_id).cloned()
                                };
                                if let Some(stream) = stream {
                                    if stream.send(Ok(packet)).is_err() {
                                        let mut pending = pending_streams_for_read.lock().await;
                                        pending.remove(&job_id);
                                    }
                                    continue;
                                }
                            }

                            let _ = incoming_tx.send(Ok(packet));
                        }
                    }
                    Err(error) => {
                        let message = error.to_string();
                        mark_closed(&state_for_read, message.clone()).await;
                        fail_pending_jobs(
                            &pending_jobs_for_read,
                            &pending_streams_for_read,
                            Error::Transport(message),
                        )
                        .await;
                        let _ = incoming_tx.send(Err(error));
                        break;
                    }
                }
            }

            mark_closed_if_unset(&state_for_read, "Steam CM read loop ended").await;
            fail_pending_jobs(
                &pending_jobs_for_read,
                &pending_streams_for_read,
                Error::Closed,
            )
            .await;
        });

        Ok(Self {
            sender,
            pending_jobs,
            pending_streams,
            incoming: incoming_rx,
            next_job_id: AtomicU64::new(1),
            state,
            license_notify,
            read_task,
            heartbeat_task: None,
        })
    }

    pub async fn send_message<M>(
        &self,
        emsg: EMsg,
        header: &CMsgProtoBufHeader,
        body: &M,
    ) -> Result<()>
    where
        M: Message,
    {
        let payload = encode_message(emsg, header, body)?;
        self.send_frame(payload).await
    }

    pub async fn request<M>(
        &self,
        emsg: EMsg,
        header: CMsgProtoBufHeader,
        body: &M,
    ) -> Result<Packet>
    where
        M: Message,
    {
        let rx = self.send_request(emsg, header, body).await?;
        rx.await
            .map_err(|_| self.closed_error())
            .and_then(|result| result)
    }

    /// Send a request and return the response receiver without awaiting it.
    /// The caller can release any held locks before awaiting the receiver.
    pub async fn send_request<M>(
        &self,
        emsg: EMsg,
        mut header: CMsgProtoBufHeader,
        body: &M,
    ) -> Result<oneshot::Receiver<Result<Packet>>>
    where
        M: Message,
    {
        let job_id = self.next_job_id.fetch_add(1, Ordering::Relaxed);
        header.jobid_source = Some(job_id);
        if header.jobid_target.is_none() {
            header.jobid_target = Some(NO_JOB_ID);
        }

        let (tx, rx) = oneshot::channel();
        self.pending_jobs.lock().await.insert(job_id, tx);

        if let Err(error) = self.send_message(emsg, &header, body).await {
            self.pending_jobs.lock().await.remove(&job_id);
            return Err(error);
        }

        Ok(rx)
    }

    /// Send a request whose response may arrive in multiple packets with the
    /// same jobid_target. The caller must call `end_stream` after it sees the
    /// protocol-level end marker.
    pub async fn send_request_stream<M>(
        &self,
        emsg: EMsg,
        mut header: CMsgProtoBufHeader,
        body: &M,
    ) -> Result<(u64, mpsc::UnboundedReceiver<Result<Packet>>)>
    where
        M: Message,
    {
        let job_id = self.next_job_id.fetch_add(1, Ordering::Relaxed);
        header.jobid_source = Some(job_id);
        if header.jobid_target.is_none() {
            header.jobid_target = Some(NO_JOB_ID);
        }

        let (tx, rx) = mpsc::unbounded_channel();
        self.pending_streams.lock().await.insert(job_id, tx);

        if let Err(error) = self.send_message(emsg, &header, body).await {
            self.pending_streams.lock().await.remove(&job_id);
            return Err(error);
        }

        Ok((job_id, rx))
    }

    pub async fn end_stream(&self, job_id: u64) {
        self.pending_streams.lock().await.remove(&job_id);
    }

    pub async fn next_event(&mut self) -> Option<Result<Packet>> {
        self.incoming.recv().await
    }

    pub async fn set_logged_on(
        &mut self,
        steamid: u64,
        client_session_id: i32,
        heartbeat_seconds: i32,
    ) -> Result<()> {
        {
            let mut state = self.state.write().await;
            state.steamid = Some(steamid);
            state.client_session_id = Some(client_session_id);
            state.heartbeat_seconds = Some(heartbeat_seconds);
        }

        self.start_heartbeat(Duration::from_secs(heartbeat_seconds as u64))
            .await
    }

    /// Extract the incoming event receiver so `run()` can select over it
    /// without holding `&mut self`. Replaces `self.incoming` with a dead channel.
    pub fn take_incoming(&mut self) -> IncomingEvents {
        let (_dead_tx, dead_rx) = mpsc::unbounded_channel();
        std::mem::replace(&mut self.incoming, dead_rx)
    }

    pub async fn state_snapshot(&self) -> ConnectionState {
        self.state.read().await.clone()
    }

    pub async fn set_package_ids(&self, package_ids: Vec<u32>) {
        {
            let mut state = self.state.write().await;
            state.license_list_received = true;
            state.package_ids = package_ids;
        }
        self.license_notify.notify_waiters();
    }

    pub fn license_notify(&self) -> Arc<Notify> {
        Arc::clone(&self.license_notify)
    }

    pub async fn is_closed(&self) -> bool {
        self.state.read().await.close_reason.is_some()
    }

    async fn send_frame(&self, payload: bytes::Bytes) -> Result<()> {
        if let Some(reason) = self.state.read().await.close_reason.clone() {
            return Err(Error::Transport(reason));
        }

        let mut sender = self.sender.lock().await;
        if let Err(error) = sender.send(WebSocketMessage::Binary(payload)).await {
            let message = error.to_string();
            {
                let mut state = self.state.write().await;
                state.close_reason = Some(message.clone());
            }
            return Err(Error::Transport(message));
        }
        Ok(())
    }

    async fn start_heartbeat(&mut self, interval: Duration) -> Result<()> {
        if let Some(task) = self.heartbeat_task.take() {
            task.abort();
        }

        let sender = Arc::clone(&self.sender);
        let state = Arc::clone(&self.state);
        self.heartbeat_task = Some(tokio::spawn(async move {
            let mut ticker = time::interval(interval);
            loop {
                ticker.tick().await;

                let state_snapshot = state.read().await.clone();
                let header = CMsgProtoBufHeader {
                    steamid: state_snapshot.steamid,
                    client_sessionid: state_snapshot.client_session_id,
                    ..Default::default()
                };
                let payload = match encode_message(
                    EMsg::ClientHeartBeat,
                    &header,
                    &CMsgClientHeartBeat {
                        send_reply: Some(false),
                    },
                ) {
                    Ok(payload) => payload,
                    Err(_) => break,
                };

                let mut writer = sender.lock().await;
                if writer
                    .send(WebSocketMessage::Binary(payload))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        }));

        Ok(())
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.read_task.abort();
        if let Some(task) = self.heartbeat_task.take() {
            task.abort();
        }
    }
}

impl Connection {
    fn closed_error(&self) -> Error {
        match self.state.try_read() {
            Ok(state) => state
                .close_reason
                .clone()
                .map(Error::Transport)
                .unwrap_or(Error::Closed),
            Err(_) => Error::Closed,
        }
    }
}

async fn mark_closed(state: &Arc<RwLock<ConnectionState>>, reason: impl Into<String>) {
    let mut state = state.write().await;
    state.close_reason = Some(reason.into());
}

async fn mark_closed_if_unset(state: &Arc<RwLock<ConnectionState>>, reason: impl Into<String>) {
    let mut state = state.write().await;
    if state.close_reason.is_none() {
        state.close_reason = Some(reason.into());
    }
}

async fn fail_pending_jobs(
    pending_jobs: &PendingJobs,
    pending_streams: &PendingStreams,
    error: Error,
) {
    let waiters = {
        let mut pending = pending_jobs.lock().await;
        pending
            .drain()
            .map(|(_, waiter)| waiter)
            .collect::<Vec<_>>()
    };
    let streams = {
        let mut pending = pending_streams.lock().await;
        pending
            .drain()
            .map(|(_, stream)| stream)
            .collect::<Vec<_>>()
    };

    let error_message = error.to_string();
    for waiter in waiters {
        let _ = waiter.send(Err(Error::Transport(error_message.clone())));
    }
    for stream in streams {
        let _ = stream.send(Err(Error::Transport(error_message.clone())));
    }
}
