#![doc = include_str!("../README.md")]

pub mod achievements;
pub mod auth {
    pub mod credentials;
    pub mod qr;
}
pub mod chat;
pub mod client;
pub mod connection;
pub mod emsg;
pub mod eresult;
pub mod error;
pub mod friends;
pub(crate) mod kv;
pub mod library;
pub mod message;
pub mod pics;
pub mod protobuf {
    include!(concat!(env!("OUT_DIR"), "/_includes.rs"));
}
pub mod serverlist;
pub mod service_method;
pub mod token;
pub mod transport {
    pub mod websocket;
}

pub use chat::ChatMessage;
pub use client::{AuthEvent, AuthMethod, GuardKind, LoggedOn, RunCommand, SteamClient};
pub use error::{Error, Result};
pub use friends::{
    Friend, FriendsEvent, LaunchEntry, Persona, PersonaState, ProtocolAchievement, ProtocolGame,
};
