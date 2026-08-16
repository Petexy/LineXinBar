use thiserror::Error;
use tokio_tungstenite::tungstenite;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("steam-cm-protocol feature is not implemented yet: {0}")]
    Unsupported(&'static str),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("authentication error: {0}")]
    Authentication(String),
    #[error("transport error: {0}")]
    Transport(String),
    #[error("unexpected websocket close")]
    Closed,
    #[error("invalid packet: {0}")]
    InvalidPacket(&'static str),
    #[error("missing field: {0}")]
    MissingField(&'static str),
    #[error("invalid response: {0}")]
    InvalidResponse(&'static str),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    ProstDecode(#[from] prost::DecodeError),
    #[error(transparent)]
    ProstEncode(#[from] prost::EncodeError),
    #[error(transparent)]
    WebSocket(Box<tungstenite::Error>),
}

impl Error {
    pub fn unsupported(feature: &'static str) -> Self {
        Self::Unsupported(feature)
    }
}

impl From<tungstenite::Error> for Error {
    fn from(error: tungstenite::Error) -> Self {
        Self::WebSocket(Box::new(error))
    }
}
