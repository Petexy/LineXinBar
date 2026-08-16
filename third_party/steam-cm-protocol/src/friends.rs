use crate::{
    emsg::EMsg,
    message::Packet,
    protobuf::{
        CMsgClientChangeStatus, CMsgClientFriendsList, CMsgClientPersonaState,
        CMsgClientRequestFriendData, CMsgProtoBufHeader,
    },
};

// EClientPersonaStateFlag: Status=1 | PlayerName=2 | LastSeen=64 | GameExtraInfo=256
const PERSONA_STATE_REQUESTED: u32 = 1 | 2 | 64 | 256;

// EFriendRelationship::Friend
const RELATIONSHIP_FRIEND: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersonaState {
    Offline,
    Online,
    Busy,
    Away,
    Snooze,
    LookingToTrade,
    LookingToPlay,
    Invisible,
}

impl PersonaState {
    pub fn from_raw(raw: u32) -> Self {
        match raw {
            1 => Self::Online,
            2 => Self::Busy,
            3 => Self::Away,
            4 => Self::Snooze,
            5 => Self::LookingToTrade,
            6 => Self::LookingToPlay,
            7 => Self::Invisible,
            _ => Self::Offline,
        }
    }

    pub fn to_raw(self) -> u32 {
        match self {
            Self::Offline => 0,
            Self::Online => 1,
            Self::Busy => 2,
            Self::Away => 3,
            Self::Snooze => 4,
            Self::LookingToTrade => 5,
            Self::LookingToPlay => 6,
            Self::Invisible => 7,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Friend {
    pub steamid: u64,
    pub relationship: u32,
}

#[derive(Debug, Clone)]
pub struct Persona {
    pub steamid: u64,
    pub name: String,
    pub state: PersonaState,
    pub game_app_id: Option<u32>,
    pub game_name: Option<String>,
    pub avatar_hash: Option<Vec<u8>>,
    /// True when this update explicitly included game fields (even if they were cleared).
    /// False when game fields were absent from the protobuf — existing values should be kept.
    pub game_fields_present: bool,
}

/// One entry from an app's PICS `config/launch` block — how Steam itself would start the game.
/// Used by the direct (no-Steam) launch path to resolve the executable without the Steam client.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchEntry {
    /// Executable path, relative to the game's install directory (e.g. `bin/game.exe`).
    pub executable: String,
    /// Launch arguments, if any.
    pub arguments: Option<String>,
    /// Working directory, relative to the install directory; `None` means the install root.
    pub workingdir: Option<String>,
    /// `config/launch[i].type` — "default", "none", "config", … `None`/empty when unset.
    pub launch_type: Option<String>,
    /// `config/launch[i].config.oslist` — comma list ("windows", "linux", "macos").
    pub oslist: Option<String>,
    /// `config/launch[i].config.osarch` — "32" / "64".
    pub osarch: Option<String>,
    /// `config/launch[i].config.betakey` — present only for beta-gated launch options.
    pub betakey: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProtocolGame {
    pub appid: u32,
    pub name: String,
    pub playtime_forever: i32,
    pub rtime_last_played: u32,
    pub img_icon_url: Option<String>,
    /// Steam appinfo `common.type` (lowercased): "game", "application", "tool", … `None` when
    /// the appinfo could not be resolved. Used by the TUI to filter the library by type.
    pub app_type: Option<String>,
    /// Appinfo `config.installdir` — the game's folder name under `steamapps/common/`. `None`
    /// when unresolved or for the recently-played / Web-API fallback paths.
    pub installdir: Option<String>,
    /// Appinfo `config/launch` entries (how Steam would launch it). Empty when unresolved.
    pub launch: Vec<LaunchEntry>,
}

#[derive(Debug, Clone)]
pub struct ProtocolAchievement {
    pub apiname: String,
    pub achieved: bool,
    pub unlocktime: u64,
    pub name: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug)]
pub enum FriendsEvent {
    FriendsList(Vec<Friend>),
    PersonaStates(Vec<Persona>),
    RecentlyPlayedGames(Vec<ProtocolGame>),
    OwnedGames(Vec<ProtocolGame>),
    PlayerAchievements {
        appid: u32,
        achievements: Vec<ProtocolAchievement>,
    },
    /// A 1-on-1 friend message arrived (or a cross-session echo of our own send).
    IncomingMessage(crate::chat::ChatMessage),
    /// One of our own outgoing messages was confirmed by Steam, stamped with the authoritative
    /// `server_timestamp` + `ordinal` (so the UI can append/dedupe it like any other message).
    MessageSent(crate::chat::ChatMessage),
    /// A friend started typing in the 1-on-1 conversation.
    TypingNotification {
        steamid: u64,
    },
    /// Result of a `GetRecentMessages` history fetch for one conversation (oldest first).
    RecentMessages {
        steamid: u64,
        messages: Vec<crate::chat::ChatMessage>,
    },
}

/// Decode a packet into a FriendsEvent, returning None for unrelated packets.
pub fn decode(packet: &Packet) -> Option<FriendsEvent> {
    if packet.emsg == EMsg::ClientFriendsList.raw() {
        let msg = packet.decode_body::<CMsgClientFriendsList>().ok()?;
        let friends = msg
            .friends
            .into_iter()
            .filter(|f| f.efriendrelationship() == RELATIONSHIP_FRIEND)
            .map(|f| Friend {
                steamid: f.ulfriendid(),
                relationship: f.efriendrelationship(),
            })
            .collect();
        return Some(FriendsEvent::FriendsList(friends));
    }

    if packet.emsg == EMsg::ClientPersonaState.raw() {
        let msg = packet.decode_body::<CMsgClientPersonaState>().ok()?;
        let personas = msg
            .friends
            .into_iter()
            .map(|f| {
                let game_fields_present = f.game_played_app_id.is_some() || f.game_name.is_some();
                Persona {
                    steamid: f.friendid(),
                    state: PersonaState::from_raw(f.persona_state()),
                    name: f.player_name.unwrap_or_default(),
                    game_app_id: f.game_played_app_id.filter(|&id| id != 0),
                    game_name: f.game_name.filter(|s| !s.is_empty()),
                    avatar_hash: f.avatar_hash.filter(|h| !h.is_empty()),
                    game_fields_present,
                }
            })
            .collect();
        return Some(FriendsEvent::PersonaStates(personas));
    }

    None
}

pub fn build_request_friend_data(
    connection_state: &crate::connection::ConnectionState,
    steamids: Vec<u64>,
) -> (CMsgProtoBufHeader, CMsgClientRequestFriendData) {
    let header = CMsgProtoBufHeader {
        steamid: connection_state.steamid,
        client_sessionid: connection_state.client_session_id,
        ..Default::default()
    };
    let body = CMsgClientRequestFriendData {
        persona_state_requested: Some(PERSONA_STATE_REQUESTED),
        friends: steamids,
    };
    (header, body)
}

pub fn build_change_status(
    connection_state: &crate::connection::ConnectionState,
    state: PersonaState,
) -> (CMsgProtoBufHeader, CMsgClientChangeStatus) {
    let header = CMsgProtoBufHeader {
        steamid: connection_state.steamid,
        client_sessionid: connection_state.client_session_id,
        ..Default::default()
    };
    let body = CMsgClientChangeStatus {
        persona_state: Some(state.to_raw()),
        persona_set_by_user: Some(true),
        ..Default::default()
    };
    (header, body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        emsg::EMsg,
        message::{decode_frame, encode_message},
        protobuf::{CMsgClientPersonaState, CMsgProtoBufHeader},
    };

    #[test]
    fn decodes_persona_state_into_friends_event() {
        let mut friend = CMsgClientPersonaState::default();
        let f = crate::protobuf::c_msg_client_persona_state::Friend {
            friendid: Some(76561198000000001),
            player_name: Some("Alice".to_owned()),
            persona_state: Some(1), // Online
            game_name: Some("Half-Life 2".to_owned()),
            game_played_app_id: Some(220),
            ..Default::default()
        };
        friend.friends.push(f);

        let header = CMsgProtoBufHeader::default();
        let encoded = encode_message(EMsg::ClientPersonaState, &header, &friend).unwrap();
        let packets = decode_frame(&encoded).unwrap();
        assert_eq!(packets.len(), 1);

        let event = decode(&packets[0]).expect("should decode as FriendsEvent");
        match event {
            FriendsEvent::PersonaStates(personas) => {
                assert_eq!(personas.len(), 1);
                assert_eq!(personas[0].steamid, 76561198000000001);
                assert_eq!(personas[0].name, "Alice");
                assert_eq!(personas[0].state, PersonaState::Online);
                assert_eq!(personas[0].game_app_id, Some(220));
                assert_eq!(personas[0].game_name.as_deref(), Some("Half-Life 2"));
                assert!(personas[0].game_fields_present);
            }
            _ => panic!("expected PersonaStates"),
        }
    }

    #[test]
    fn filters_non_friend_relationships() {
        let mut msg = crate::protobuf::CMsgClientFriendsList::default();
        // relationship 3 = Friend, 1 = Blocked
        let friend_entry = crate::protobuf::c_msg_client_friends_list::Friend {
            ulfriendid: Some(76561198000000002),
            efriendrelationship: Some(3),
        };
        msg.friends.push(friend_entry);
        let blocked_entry = crate::protobuf::c_msg_client_friends_list::Friend {
            ulfriendid: Some(76561198000000099),
            efriendrelationship: Some(1),
        };
        msg.friends.push(blocked_entry);

        let header = CMsgProtoBufHeader::default();
        let encoded = encode_message(EMsg::ClientFriendsList, &header, &msg).unwrap();
        let packets = decode_frame(&encoded).unwrap();

        let event = decode(&packets[0]).expect("should decode");
        match event {
            FriendsEvent::FriendsList(friends) => {
                assert_eq!(friends.len(), 1);
                assert_eq!(friends[0].steamid, 76561198000000002);
            }
            _ => panic!("expected FriendsList"),
        }
    }
}
