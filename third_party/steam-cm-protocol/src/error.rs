use thiserror::Error;
use tokio_tungstenite::tungstenite;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("steam-cm-protocol feature is not implemented yet: {0}")]
    Unsupported(&'static str),
    #[error("protocol error: {0}")]
    Protocol(String),
    /// Steam answered the request and refused it, with its own `EResult`.
    ///
    /// Kept as a number rather than folded into [`Error::Protocol`]'s string,
    /// because callers act on it: the difference between "too long" and "too
    /// fast" is the difference between a message that must be shortened and one
    /// that must simply be sent again, and both arrive here as a bare integer.
    #[error("Steam refused this with result {result}: {}", detail.as_deref().unwrap_or("no detail"))]
    Refused {
        result: i32,
        detail: Option<String>,
    },
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
