use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use crate::{
    error::{Error, Result},
    serverlist::CmServer,
};

pub type SteamWebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebSocketEndpoint {
    pub url: String,
}

impl WebSocketEndpoint {
    pub fn from_cm_server(server: &CmServer) -> Self {
        Self {
            url: server.websocket_url(),
        }
    }
}

pub async fn connect(url: &str) -> Result<SteamWebSocket> {
    let (socket, response) = connect_async(url).await?;

    if response.status() != StatusCode::SWITCHING_PROTOCOLS {
        return Err(Error::Transport(format!(
            "Steam CM websocket upgrade failed with status {}",
            response.status()
        )));
    }

    Ok(socket)
}
