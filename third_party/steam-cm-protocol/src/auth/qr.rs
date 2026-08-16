use std::time::Duration;

use crate::{
    connection::Connection,
    error::{Error, Result},
    protobuf::{
        CAuthenticationBeginAuthSessionViaQrRequest, CAuthenticationBeginAuthSessionViaQrResponse,
        CAuthenticationDeviceDetails, CAuthenticationPollAuthSessionStatusRequest,
        CAuthenticationPollAuthSessionStatusResponse,
    },
    service_method::{ServiceMethod, call},
};

const BEGIN_QR_METHOD: &str = "Authentication.BeginAuthSessionViaQR#1";
const POLL_METHOD: &str = "Authentication.PollAuthSessionStatus#1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QrChallenge {
    pub client_id: u64,
    pub request_id: Vec<u8>,
    pub challenge_url: String,
    pub interval: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedAuth {
    pub refresh_token: String,
    pub account_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollState {
    Pending { challenge_changed: bool },
    Complete(CompletedAuth),
}

pub async fn begin(
    connection: &Connection,
    device_friendly_name: &str,
    platform_type: i32,
    device_details: CAuthenticationDeviceDetails,
    website_id: &str,
) -> Result<QrChallenge> {
    let response: CAuthenticationBeginAuthSessionViaQrResponse = call(
        connection,
        &ServiceMethod::new(BEGIN_QR_METHOD),
        &CAuthenticationBeginAuthSessionViaQrRequest {
            device_friendly_name: Some(device_friendly_name.to_owned()),
            platform_type: Some(platform_type),
            device_details: Some(device_details),
            website_id: Some(website_id.to_owned()),
        },
    )
    .await?;

    Ok(QrChallenge {
        client_id: response.client_id.ok_or(Error::MissingField(
            "CAuthenticationBeginAuthSessionViaQrResponse.client_id",
        ))?,
        request_id: response.request_id.ok_or(Error::MissingField(
            "CAuthenticationBeginAuthSessionViaQrResponse.request_id",
        ))?,
        challenge_url: response.challenge_url.ok_or(Error::MissingField(
            "CAuthenticationBeginAuthSessionViaQrResponse.challenge_url",
        ))?,
        interval: Duration::from_secs_f32(response.interval.unwrap_or(5.0).max(1.0)),
    })
}

pub async fn poll(connection: &Connection, challenge: &mut QrChallenge) -> Result<PollState> {
    let response: CAuthenticationPollAuthSessionStatusResponse = call(
        connection,
        &ServiceMethod::new(POLL_METHOD),
        &CAuthenticationPollAuthSessionStatusRequest {
            client_id: Some(challenge.client_id),
            request_id: Some(challenge.request_id.clone()),
            token_to_revoke: None,
        },
    )
    .await?;

    if let Some(refresh_token) = response.refresh_token {
        return Ok(PollState::Complete(CompletedAuth {
            refresh_token,
            account_name: response.account_name.ok_or(Error::MissingField(
                "CAuthenticationPollAuthSessionStatusResponse.account_name",
            ))?,
        }));
    }

    let mut challenge_changed = false;
    if let Some(new_client_id) = response.new_client_id {
        challenge.client_id = new_client_id;
    }
    if let Some(new_challenge_url) = response.new_challenge_url
        && new_challenge_url != challenge.challenge_url
    {
        challenge.challenge_url = new_challenge_url;
        challenge_changed = true;
    }

    Ok(PollState::Pending { challenge_changed })
}
