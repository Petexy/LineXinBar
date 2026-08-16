//! 1-on-1 friend chat over the modern unified `FriendMessages.*` service.
//!
//! Mirrors the `library.rs` / `service_method.rs` pattern: outbound requests go through
//! `call_authed` (`ServiceMethodCallFromClient`, EMsg 151 → `ServiceMethodResponse` 147), and
//! incoming messages arrive unsolicited as a `ServiceMethod` (EMsg 146) server push, identified by
//! `target_job_name`. No per-conversation subscribe is needed, but the logon MUST set
//! `chat_mode = 2` (see `client.rs`) or Steam never pushes friend messages to the session.

use crate::{
    connection::{Connection, ConnectionState},
    emsg::EMsg,
    error::Result,
    friends::FriendsEvent,
    message::Packet,
    protobuf::{
        CFriendMessagesGetRecentMessagesRequest, CFriendMessagesGetRecentMessagesResponse,
        CFriendMessagesIncomingMessageNotification, CFriendMessagesSendMessageRequest,
        CFriendMessagesSendMessageResponse,
    },
    service_method::{ServiceMethod, call_authed},
};

/// `EChatEntryType` values we use. The enum isn't present in any compiled proto, so the raw
/// ints are hardcoded (values per SteamKit `enums.steamd`).
pub const CHAT_ENTRY_TEXT: i32 = 1;
pub const CHAT_ENTRY_TYPING: i32 = 2;

/// Job name carried by the incoming-message server push (EMsg 146 `ServiceMethod`).
const INCOMING_MESSAGE_JOB: &str = "FriendMessagesClient.IncomingMessage#1";

/// A single 1-on-1 chat message, normalised for the UI/cache layers.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChatMessage {
    /// The conversation partner (the friend), regardless of who authored the message.
    pub steamid: u64,
    pub message: String,
    /// Steam `rtime32` server timestamp (Unix seconds).
    pub timestamp: u32,
    /// Disambiguates multiple messages sharing the same second.
    pub ordinal: u32,
    /// True when the local user authored this message.
    pub from_local: bool,
}

/// Send a text message to `steamid`, returning the confirmed message stamped with Steam's
/// authoritative `server_timestamp` + `ordinal` (the same `(timestamp, ordinal)` key the message
/// later carries in history and cross-session echoes — so callers can dedupe without heuristics).
pub async fn send_message(
    connection: &Connection,
    state: &ConnectionState,
    steamid: u64,
    message: String,
) -> Result<ChatMessage> {
    let method = ServiceMethod::new("FriendMessages.SendMessage#1");
    let request = CFriendMessagesSendMessageRequest {
        steamid: Some(steamid),
        chat_entry_type: Some(CHAT_ENTRY_TEXT),
        message: Some(message.clone()),
        // Send the raw text verbatim; brackets render literally in the terminal.
        contains_bbcode: Some(false),
        ..Default::default()
    };
    let response: CFriendMessagesSendMessageResponse =
        call_authed(connection, state, &method, &request).await?;
    Ok(sent_message(steamid, message, response))
}

/// Send a typing indicator to `steamid` (best-effort).
pub async fn send_typing(
    connection: &Connection,
    state: &ConnectionState,
    steamid: u64,
) -> Result<()> {
    let method = ServiceMethod::new("FriendMessages.SendMessage#1");
    let request = CFriendMessagesSendMessageRequest {
        steamid: Some(steamid),
        chat_entry_type: Some(CHAT_ENTRY_TYPING),
        message: Some(String::new()),
        ..Default::default()
    };
    let _response: CFriendMessagesSendMessageResponse =
        call_authed(connection, state, &method, &request).await?;
    Ok(())
}

/// Fetch recent message history for the conversation with `steamid`, oldest first.
pub async fn get_recent_messages(
    connection: &Connection,
    state: &ConnectionState,
    steamid: u64,
) -> Result<Vec<ChatMessage>> {
    let method = ServiceMethod::new("FriendMessages.GetRecentMessages#1");
    let request = CFriendMessagesGetRecentMessagesRequest {
        steamid1: state.steamid,
        steamid2: Some(steamid),
        count: Some(50),
        ..Default::default()
    };
    let response: CFriendMessagesGetRecentMessagesResponse =
        call_authed(connection, state, &method, &request).await?;
    Ok(recent_to_messages(response, steamid, state.steamid))
}

/// Decode an incoming friend-message server push. Steam delivers server-initiated unified
/// notifications as EMsg `ServiceMethod` (146) — NOT `ServiceMethodSendToClient` (152) — and
/// demultiplexes them purely by `target_job_name` (verified against node-steam-user and SteamKit).
/// Accept both EMsgs and gate on the job name; return `None` for any other push (reactions,
/// session notices, group-chat client pushes) so the run loop ignores them.
pub fn decode_incoming(packet: &Packet) -> Option<FriendsEvent> {
    let is_service_push = packet.emsg == EMsg::ServiceMethod.raw()
        || packet.emsg == EMsg::ServiceMethodSendToClient.raw();
    if !is_service_push || packet.target_job_name() != Some(INCOMING_MESSAGE_JOB) {
        return None;
    }
    let notification = packet
        .decode_body::<CFriendMessagesIncomingMessageNotification>()
        .ok()?;
    // `steamid_friend` is always the partner, even for cross-session echoes of our own sends.
    let partner = notification.steamid_friend?;
    match notification.chat_entry_type.unwrap_or(0) {
        CHAT_ENTRY_TEXT => Some(FriendsEvent::IncomingMessage(ChatMessage {
            steamid: partner,
            message: notification.message.unwrap_or_default(),
            timestamp: notification.rtime32_server_timestamp.unwrap_or(0),
            ordinal: notification.ordinal.unwrap_or(0),
            // Direction comes from `local_echo`, never from comparing steamids.
            from_local: notification.local_echo.unwrap_or(false),
        })),
        CHAT_ENTRY_TYPING => Some(FriendsEvent::TypingNotification { steamid: partner }),
        _ => None,
    }
}

/// Build the confirmed `ChatMessage` for one of our own sends from the SendMessage response.
fn sent_message(
    steamid: u64,
    original: String,
    response: CFriendMessagesSendMessageResponse,
) -> ChatMessage {
    ChatMessage {
        steamid,
        // Steam may normalise the message (e.g. bbcode); prefer its version when present.
        message: response
            .modified_message
            .filter(|m| !m.is_empty())
            .unwrap_or(original),
        timestamp: response.server_timestamp.unwrap_or(0),
        ordinal: response.ordinal.unwrap_or(0),
        from_local: true,
    }
}

/// The low 32 bits of a SteamID64 are the account id used in `GetRecentMessages` rows.
fn account_id_of(steamid: Option<u64>) -> Option<u32> {
    steamid.map(|s| (s & 0xFFFF_FFFF) as u32)
}

fn recent_to_messages(
    response: CFriendMessagesGetRecentMessagesResponse,
    partner: u64,
    self_steamid: Option<u64>,
) -> Vec<ChatMessage> {
    let self_account = account_id_of(self_steamid);
    let mut messages: Vec<ChatMessage> = response
        .messages
        .into_iter()
        .map(|m| ChatMessage {
            steamid: partner,
            message: m.message.unwrap_or_default(),
            timestamp: m.timestamp.unwrap_or(0),
            ordinal: m.ordinal.unwrap_or(0),
            from_local: m.accountid.is_some() && m.accountid == self_account,
        })
        .collect();
    // Steam does not guarantee ordering; sort oldest-first for display.
    messages.sort_by(|a, b| {
        a.timestamp
            .cmp(&b.timestamp)
            .then(a.ordinal.cmp(&b.ordinal))
    });
    messages
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{decode_frame, encode_message};
    use crate::protobuf::{
        CMsgProtoBufHeader, c_friend_messages_get_recent_messages_response::FriendMessage,
    };
    use prost::Message;

    const SELF_STEAMID: u64 = 76561198000000001;
    const PARTNER_STEAMID: u64 = 76561198000000002;

    fn incoming_packet(
        emsg: EMsg,
        job: &str,
        notification: &CFriendMessagesIncomingMessageNotification,
    ) -> Packet {
        let header = CMsgProtoBufHeader {
            target_job_name: Some(job.to_owned()),
            ..Default::default()
        };
        let encoded = encode_message(emsg, &header, notification).unwrap();
        decode_frame(&encoded)
            .unwrap()
            .into_iter()
            .next()
            .expect("one packet")
    }

    #[test]
    fn send_request_roundtrips() {
        let request = CFriendMessagesSendMessageRequest {
            steamid: Some(PARTNER_STEAMID),
            chat_entry_type: Some(CHAT_ENTRY_TEXT),
            message: Some("hi there".to_owned()),
            contains_bbcode: Some(false),
            ..Default::default()
        };
        let bytes = request.encode_to_vec();
        let back = CFriendMessagesSendMessageRequest::decode(bytes.as_slice()).unwrap();
        assert_eq!(back.steamid, Some(PARTNER_STEAMID));
        assert_eq!(back.message.as_deref(), Some("hi there"));
        assert_eq!(back.chat_entry_type, Some(CHAT_ENTRY_TEXT));
    }

    #[test]
    fn sent_message_uses_server_stamp_and_modified_text() {
        let response = CFriendMessagesSendMessageResponse {
            modified_message: Some("hi &lt;there&gt;".to_owned()),
            server_timestamp: Some(1717),
            ordinal: Some(3),
            ..Default::default()
        };
        let sent = sent_message(PARTNER_STEAMID, "hi <there>".to_owned(), response);
        assert_eq!(sent.steamid, PARTNER_STEAMID);
        assert_eq!(sent.message, "hi &lt;there&gt;");
        assert_eq!(sent.timestamp, 1717);
        assert_eq!(sent.ordinal, 3);
        assert!(sent.from_local);
    }

    #[test]
    fn sent_message_falls_back_to_original_when_unmodified() {
        let response = CFriendMessagesSendMessageResponse {
            server_timestamp: Some(42),
            ordinal: Some(0),
            ..Default::default()
        };
        let sent = sent_message(PARTNER_STEAMID, "plain".to_owned(), response);
        assert_eq!(sent.message, "plain");
        assert_eq!(sent.timestamp, 42);
        assert!(sent.from_local);
    }

    #[test]
    fn decodes_incoming_text_message() {
        let notification = CFriendMessagesIncomingMessageNotification {
            steamid_friend: Some(PARTNER_STEAMID),
            chat_entry_type: Some(CHAT_ENTRY_TEXT),
            message: Some("hello".to_owned()),
            rtime32_server_timestamp: Some(1000),
            ordinal: Some(2),
            local_echo: Some(false),
            ..Default::default()
        };
        // Real-world pushes arrive as EMsg ServiceMethod (146), keyed by target_job_name.
        let packet = incoming_packet(EMsg::ServiceMethod, INCOMING_MESSAGE_JOB, &notification);
        match decode_incoming(&packet) {
            Some(FriendsEvent::IncomingMessage(m)) => {
                assert_eq!(m.steamid, PARTNER_STEAMID);
                assert_eq!(m.message, "hello");
                assert_eq!(m.timestamp, 1000);
                assert_eq!(m.ordinal, 2);
                assert!(!m.from_local);
            }
            other => panic!("expected IncomingMessage, got {other:?}"),
        }
    }

    #[test]
    fn decodes_incoming_text_message_via_send_to_client() {
        // Defensive: also accept the rarer ServiceMethodSendToClient (152) EMsg.
        let notification = CFriendMessagesIncomingMessageNotification {
            steamid_friend: Some(PARTNER_STEAMID),
            chat_entry_type: Some(CHAT_ENTRY_TEXT),
            message: Some("hello".to_owned()),
            rtime32_server_timestamp: Some(1000),
            ordinal: Some(2),
            local_echo: Some(false),
            ..Default::default()
        };
        let packet = incoming_packet(
            EMsg::ServiceMethodSendToClient,
            INCOMING_MESSAGE_JOB,
            &notification,
        );
        assert!(matches!(
            decode_incoming(&packet),
            Some(FriendsEvent::IncomingMessage(_))
        ));
    }

    #[test]
    fn decodes_incoming_typing() {
        let notification = CFriendMessagesIncomingMessageNotification {
            steamid_friend: Some(PARTNER_STEAMID),
            chat_entry_type: Some(CHAT_ENTRY_TYPING),
            ..Default::default()
        };
        let packet = incoming_packet(EMsg::ServiceMethod, INCOMING_MESSAGE_JOB, &notification);
        match decode_incoming(&packet) {
            Some(FriendsEvent::TypingNotification { steamid }) => {
                assert_eq!(steamid, PARTNER_STEAMID)
            }
            other => panic!("expected TypingNotification, got {other:?}"),
        }
    }

    #[test]
    fn ignores_unrelated_push_job() {
        let notification = CFriendMessagesIncomingMessageNotification {
            steamid_friend: Some(PARTNER_STEAMID),
            chat_entry_type: Some(CHAT_ENTRY_TEXT),
            message: Some("from a group".to_owned()),
            ..Default::default()
        };
        let packet = incoming_packet(
            EMsg::ServiceMethod,
            "ChatRoomClient.NotifyIncomingChatMessage#1",
            &notification,
        );
        assert!(decode_incoming(&packet).is_none());
    }

    #[test]
    fn recent_messages_marks_local_author_and_sorts_oldest_first() {
        let self_account = (SELF_STEAMID & 0xFFFF_FFFF) as u32;
        let response = CFriendMessagesGetRecentMessagesResponse {
            messages: vec![
                FriendMessage {
                    accountid: Some(self_account),
                    timestamp: Some(200),
                    message: Some("my reply".to_owned()),
                    ordinal: Some(0),
                    ..Default::default()
                },
                FriendMessage {
                    accountid: Some(0xDEAD_BEEF),
                    timestamp: Some(100),
                    message: Some("their hello".to_owned()),
                    ordinal: Some(0),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let messages = recent_to_messages(response, PARTNER_STEAMID, Some(SELF_STEAMID));
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].message, "their hello");
        assert!(!messages[0].from_local);
        assert_eq!(messages[0].steamid, PARTNER_STEAMID);
        assert_eq!(messages[1].message, "my reply");
        assert!(messages[1].from_local);
    }
}
