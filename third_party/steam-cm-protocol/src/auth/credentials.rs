use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use rsa::{BigUint, Pkcs1v15Encrypt, RsaPublicKey};

use crate::{
    client::GuardKind,
    connection::Connection,
    error::{Error, Result},
    protobuf::{
        CAuthenticationBeginAuthSessionViaCredentialsRequest,
        CAuthenticationBeginAuthSessionViaCredentialsResponse, CAuthenticationDeviceDetails,
        CAuthenticationGetPasswordRsaPublicKeyRequest,
        CAuthenticationGetPasswordRsaPublicKeyResponse,
        CAuthenticationPollAuthSessionStatusRequest, CAuthenticationPollAuthSessionStatusResponse,
        CAuthenticationUpdateAuthSessionWithSteamGuardCodeRequest, EAuthSessionGuardType,
        EAuthTokenPlatformType, ESessionPersistence,
    },
    service_method::{ServiceMethod, call},
};

const GET_PASSWORD_RSA_KEY_METHOD: &str = "Authentication.GetPasswordRSAPublicKey#1";
const BEGIN_CREDENTIALS_METHOD: &str = "Authentication.BeginAuthSessionViaCredentials#1";
const POLL_METHOD: &str = "Authentication.PollAuthSessionStatus#1";
const UPDATE_GUARD_CODE_METHOD: &str = "Authentication.UpdateAuthSessionWithSteamGuardCode#1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialSession {
    pub client_id: u64,
    pub request_id: Vec<u8>,
    pub steamid: u64,
    pub interval: Duration,
    pub allowed_confirmations: Vec<GuardKind>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedAuth {
    pub refresh_token: String,
    pub account_name: String,
}

impl CredentialSession {
    pub fn preferred_guard_kind(&self) -> Option<GuardKind> {
        self.allowed_confirmations
            .iter()
            .find_map(|kind| match kind {
                GuardKind::EmailCode => Some(GuardKind::EmailCode),
                GuardKind::DeviceCode => Some(GuardKind::DeviceCode),
                GuardKind::DeviceConfirmation => None,
            })
            .or_else(|| {
                self.allowed_confirmations
                    .iter()
                    .find(|kind| **kind == GuardKind::DeviceConfirmation)
                    .cloned()
            })
    }
}

pub async fn begin(
    connection: &Connection,
    account_name: &str,
    password: &str,
    device_friendly_name: &str,
    device_details: CAuthenticationDeviceDetails,
    website_id: &str,
) -> Result<CredentialSession> {
    let rsa_key = get_password_rsa_key(connection, account_name).await?;
    let encrypted_password = encrypt_password(password, &rsa_key)?;

    let response: CAuthenticationBeginAuthSessionViaCredentialsResponse = call(
        connection,
        &ServiceMethod::new(BEGIN_CREDENTIALS_METHOD),
        &CAuthenticationBeginAuthSessionViaCredentialsRequest {
            device_friendly_name: Some(device_friendly_name.to_owned()),
            account_name: Some(account_name.to_owned()),
            encrypted_password: Some(encrypted_password),
            encryption_timestamp: rsa_key.timestamp,
            remember_login: Some(true),
            platform_type: Some(EAuthTokenPlatformType::KEAuthTokenPlatformTypeSteamClient as i32),
            persistence: Some(ESessionPersistence::KESessionPersistencePersistent as i32),
            website_id: Some(website_id.to_owned()),
            device_details: Some(device_details),
            guard_data: None,
            language: None,
            qos_level: Some(2),
        },
    )
    .await?;

    Ok(CredentialSession {
        client_id: response.client_id.ok_or(Error::MissingField(
            "CAuthenticationBeginAuthSessionViaCredentialsResponse.client_id",
        ))?,
        request_id: response.request_id.ok_or(Error::MissingField(
            "CAuthenticationBeginAuthSessionViaCredentialsResponse.request_id",
        ))?,
        steamid: response.steamid.ok_or(Error::MissingField(
            "CAuthenticationBeginAuthSessionViaCredentialsResponse.steamid",
        ))?,
        interval: Duration::from_secs_f32(response.interval.unwrap_or(5.0).max(1.0)),
        allowed_confirmations: response
            .allowed_confirmations
            .into_iter()
            .filter_map(|confirmation| map_guard_kind(confirmation.confirmation_type))
            .collect(),
    })
}

pub async fn submit_guard_code(
    connection: &Connection,
    session: &CredentialSession,
    code: &str,
    kind: GuardKind,
) -> Result<()> {
    let _: crate::protobuf::CAuthenticationUpdateAuthSessionWithSteamGuardCodeResponse = call(
        connection,
        &ServiceMethod::new(UPDATE_GUARD_CODE_METHOD),
        &CAuthenticationUpdateAuthSessionWithSteamGuardCodeRequest {
            client_id: Some(session.client_id),
            steamid: Some(session.steamid),
            code: Some(code.to_owned()),
            code_type: Some(guard_kind_to_proto(kind) as i32),
        },
    )
    .await?;

    Ok(())
}

pub async fn poll(
    connection: &Connection,
    session: &CredentialSession,
) -> Result<Option<CompletedAuth>> {
    let response: CAuthenticationPollAuthSessionStatusResponse = call(
        connection,
        &ServiceMethod::new(POLL_METHOD),
        &CAuthenticationPollAuthSessionStatusRequest {
            client_id: Some(session.client_id),
            request_id: Some(session.request_id.clone()),
            token_to_revoke: None,
        },
    )
    .await?;

    match response.refresh_token {
        Some(refresh_token) => Ok(Some(CompletedAuth {
            refresh_token,
            account_name: response.account_name.ok_or(Error::MissingField(
                "CAuthenticationPollAuthSessionStatusResponse.account_name",
            ))?,
        })),
        None => Ok(None),
    }
}

struct PasswordRsaKey {
    public_key: RsaPublicKey,
    timestamp: Option<u64>,
}

async fn get_password_rsa_key(
    connection: &Connection,
    account_name: &str,
) -> Result<PasswordRsaKey> {
    let response: CAuthenticationGetPasswordRsaPublicKeyResponse = call(
        connection,
        &ServiceMethod::new(GET_PASSWORD_RSA_KEY_METHOD),
        &CAuthenticationGetPasswordRsaPublicKeyRequest {
            account_name: Some(account_name.to_owned()),
        },
    )
    .await?;

    let modulus = parse_hex_biguint(&response.publickey_mod.ok_or(Error::MissingField(
        "CAuthenticationGetPasswordRsaPublicKeyResponse.publickey_mod",
    ))?)?;
    let exponent = parse_hex_biguint(&response.publickey_exp.ok_or(Error::MissingField(
        "CAuthenticationGetPasswordRsaPublicKeyResponse.publickey_exp",
    ))?)?;

    Ok(PasswordRsaKey {
        public_key: RsaPublicKey::new(modulus, exponent)
            .map_err(|error| Error::Authentication(error.to_string()))?,
        timestamp: response.timestamp,
    })
}

fn encrypt_password(password: &str, key: &PasswordRsaKey) -> Result<String> {
    let mut rng = rand::thread_rng();
    let encrypted = key
        .public_key
        .encrypt(&mut rng, Pkcs1v15Encrypt, password.as_bytes())
        .map_err(|error| Error::Authentication(error.to_string()))?;
    Ok(STANDARD.encode(encrypted))
}

fn parse_hex_biguint(value: &str) -> Result<BigUint> {
    BigUint::parse_bytes(value.as_bytes(), 16)
        .ok_or(Error::Authentication("invalid RSA key response".to_owned()))
}

fn map_guard_kind(code: Option<i32>) -> Option<GuardKind> {
    match code.and_then(|value| EAuthSessionGuardType::try_from(value).ok()) {
        Some(EAuthSessionGuardType::KEAuthSessionGuardTypeEmailCode) => Some(GuardKind::EmailCode),
        Some(EAuthSessionGuardType::KEAuthSessionGuardTypeDeviceCode) => {
            Some(GuardKind::DeviceCode)
        }
        Some(EAuthSessionGuardType::KEAuthSessionGuardTypeDeviceConfirmation) => {
            Some(GuardKind::DeviceConfirmation)
        }
        _ => None,
    }
}

fn guard_kind_to_proto(kind: GuardKind) -> EAuthSessionGuardType {
    match kind {
        GuardKind::EmailCode => EAuthSessionGuardType::KEAuthSessionGuardTypeEmailCode,
        GuardKind::DeviceCode => EAuthSessionGuardType::KEAuthSessionGuardTypeDeviceCode,
        GuardKind::DeviceConfirmation => {
            EAuthSessionGuardType::KEAuthSessionGuardTypeDeviceConfirmation
        }
    }
}
