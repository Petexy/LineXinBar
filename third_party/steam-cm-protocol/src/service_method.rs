use prost::Message;

use crate::{
    connection::{Connection, ConnectionState},
    emsg::EMsg,
    error::Result,
    protobuf::CMsgProtoBufHeader,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceMethod {
    pub target_job_name: String,
}

impl ServiceMethod {
    pub fn new(target_job_name: impl Into<String>) -> Self {
        Self {
            target_job_name: target_job_name.into(),
        }
    }
}

pub async fn call<Req, Resp>(
    connection: &Connection,
    method: &ServiceMethod,
    request: &Req,
) -> Result<Resp>
where
    Req: Message,
    Resp: Message + Default,
{
    let packet = connection
        .request(
            EMsg::ServiceMethodCallFromClientNonAuthed,
            CMsgProtoBufHeader {
                target_job_name: Some(method.target_job_name.clone()),
                ..Default::default()
            },
            request,
        )
        .await?;

    packet.ensure_success()?;
    packet.decode_body::<Resp>()
}

/// Like `call` but uses `ServiceMethodCallFromClient` with the logged-on session
/// headers (steamid + client_sessionid). Required for user-scoped service methods.
pub async fn call_authed<Req, Resp>(
    connection: &Connection,
    state: &ConnectionState,
    method: &ServiceMethod,
    request: &Req,
) -> Result<Resp>
where
    Req: Message,
    Resp: Message + Default,
{
    let packet = connection
        .request(
            EMsg::ServiceMethodCallFromClient,
            CMsgProtoBufHeader {
                steamid: state.steamid,
                client_sessionid: state.client_session_id,
                target_job_name: Some(method.target_job_name.clone()),
                ..Default::default()
            },
            request,
        )
        .await?;

    packet.ensure_success()?;
    packet.decode_body::<Resp>()
}
