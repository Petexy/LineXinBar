//! Signing in: `IAuthenticationService`, both ways round.
//!
//! Steam has one sign-in and two ways to start it. Either way the client asks
//! for a *session*, is told how often to ask whether it has been approved yet,
//! and polls until it is handed a durable refresh token. What differs is only how the
//! account proves it is the account:
//!
//! * **By code.** The client asks for the RSA key belonging to that account
//!   name, encrypts the password under it (see [`crate::rsa`]), and begins a
//!   session with the ciphertext. Steam answers with the ways this account may
//!   be confirmed — a code from an email, a code from the authenticator, or a
//!   press on the phone — and the session is polled until one happens.
//! * **By code on a screen.** The client begins a session with no account name
//!   at all and is handed a URL. Photographed by the Steam app on a phone that
//!   is already signed in, that URL confirms the session, and the same poll
//!   ends with the same credential. No password is typed at any point,
//!   which on a machine driven with a controller is the difference between
//!   signing in and not bothering.
//!
//! ## The credential
//!
//! The refresh token makes this machine an authorised device on the account,
//! listed by name in Steam Guard and revocable from there. It is sent as the
//! CM logon's `access_token` field (Steam's protocol name) and is never used as
//! a Web API bearer. That lets the session survive a reboot without storing a
//! password; signing this machine out from a phone ends it.

use crate::base64;
use crate::password::Password;
use crate::protobuf::{self, Writer};
use crate::rsa::PublicKey;
use crate::web::{Failed, Wire};

/// The service every message in this module belongs to.
const SERVICE: &str = "IAuthenticationService";

/// What kind of client this says it is.
///
/// `SteamClient`, because that is what it is: a session on a machine in front
/// of the user, which should appear in their authorised devices under this
/// shell's name and be revocable from there like any other. The alternative —
/// claiming to be a web browser — would produce a token that works just as
/// well for reading a library and would describe this session to the user as
/// something it is not.
const PLATFORM_STEAM_CLIENT: u64 = 1;

/// The operating system, as Steam's own enumeration spells it. Negative, which
/// is why the field is written as a signed one.
const OS_LINUX: i32 = -203;

/// A session that lasts beyond this one, which is what makes the refresh token
/// worth keeping.
const PERSISTENT: u64 = 1;

/// How a sign-in may be confirmed, as Steam offers them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirmation {
    /// Nothing further is needed; the poll will simply succeed.
    None,
    /// A code sent to the account's email address. The string is the hint
    /// Steam gives for which address that is — never the address itself.
    EmailCode(String),
    /// A code from the Steam Guard authenticator on the user's phone.
    DeviceCode,
    /// A press on the phone. Nothing to type, and nothing to ask for: the poll
    /// is the whole of the wait.
    DeviceConfirmation,
    /// The same, sent to the account's email address.
    EmailConfirmation,
    /// A kind this crate has not been taught. Kept rather than dropped so the
    /// shell can say that a sign-in needs something it cannot offer, instead
    /// of waiting for a confirmation that will never come.
    Unknown(u64),
}

impl Confirmation {
    fn of(kind: u64, hint: Option<String>) -> Confirmation {
        match kind {
            1 => Confirmation::None,
            2 => Confirmation::EmailCode(hint.unwrap_or_default()),
            3 => Confirmation::DeviceCode,
            4 => Confirmation::DeviceConfirmation,
            5 => Confirmation::EmailConfirmation,
            // 6 and 7 are machine tokens, which are handled by sending the
            // stored guard data with the next attempt rather than by asking
            // the user for anything.
            other => Confirmation::Unknown(other),
        }
    }

    /// The number this kind is sent back as when a code is submitted.
    fn code(&self) -> u64 {
        match self {
            Confirmation::None => 1,
            Confirmation::EmailCode(_) => 2,
            Confirmation::DeviceCode => 3,
            Confirmation::DeviceConfirmation => 4,
            Confirmation::EmailConfirmation => 5,
            Confirmation::Unknown(other) => *other,
        }
    }

    /// Whether this is one the user types rather than one they press.
    pub fn is_typed(&self) -> bool {
        matches!(self, Confirmation::EmailCode(_) | Confirmation::DeviceCode)
    }

    /// What to say above the field, or above the wait.
    pub fn asked(&self) -> String {
        match self {
            Confirmation::None => "Signing in.".to_string(),
            Confirmation::EmailCode(hint) if hint.is_empty() => {
                "Enter the code Steam emailed you.".to_string()
            }
            Confirmation::EmailCode(hint) => {
                format!("Enter the code Steam emailed to {hint}.")
            }
            Confirmation::DeviceCode => {
                "Enter the code from Steam Guard on your phone.".to_string()
            }
            Confirmation::DeviceConfirmation => {
                "Confirm this sign-in in the Steam app on your phone.".to_string()
            }
            Confirmation::EmailConfirmation => {
                "Confirm this sign-in from the email Steam sent you.".to_string()
            }
            Confirmation::Unknown(kind) => {
                format!(
                    "This account needs a kind of confirmation LineXinBar cannot offer ({kind})."
                )
            }
        }
    }
}

/// A sign-in that has been begun and is waiting to be confirmed.
#[derive(Debug, Clone)]
pub struct Session {
    pub client_id: u64,
    request_id: Vec<u8>,
    /// How long to leave between polls, in seconds, as Steam asked.
    pub interval: f32,
    /// The account, once Steam knows which one it is. Zero throughout a
    /// sign-in by code on a screen, where the account is not named until the
    /// phone has said which one it is.
    pub steam_id: u64,
    /// The URL the code on screen stands for. `None` for a sign-in by
    /// password.
    pub challenge_url: Option<String>,
    /// The ways this sign-in may be confirmed, as Steam offered them.
    pub confirmations: Vec<Confirmation>,
}

impl Session {
    /// The confirmation the user has to be asked to type, if any.
    ///
    /// Steam lists every way an account *could* be confirmed, most-preferred
    /// first, and a typical account with an authenticator offers both a press
    /// on the phone and a code from it. The press is the better of the two on
    /// a machine with no keyboard, so a session offering it is left to the
    /// poll; only a session with nothing but codes on it puts up a field.
    pub fn code_wanted(&self) -> Option<&Confirmation> {
        if self
            .confirmations
            .iter()
            .any(|way| matches!(way, Confirmation::DeviceConfirmation))
        {
            return None;
        }
        self.confirmations.iter().find(|way| way.is_typed())
    }
}

/// A sign-in that succeeded.
#[derive(Clone)]
pub struct Granted {
    pub account: String,
    pub steam_id: u64,
    pub refresh_token: String,
    /// What Steam gave to make the next sign-in on this machine skip the email
    /// code. Kept beside the token; worth nothing on its own.
    pub guard_data: Option<String>,
}

impl std::fmt::Debug for Granted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Granted")
            .field("account", &self.account)
            .field("steam_id", &self.steam_id)
            .field("refresh_token", &"[redacted]")
            .field(
                "guard_data",
                &self.guard_data.as_ref().map(|_| "[redacted]"),
            )
            .finish()
    }
}

/// What one poll of a waiting sign-in found.
#[derive(Debug, Clone)]
pub enum Polled {
    /// Nothing yet. Wait the interval and ask again.
    Waiting,
    /// The code on screen has been replaced, and the old one no longer works.
    /// Steam rotates these every twenty seconds or so.
    Renewed(String),
    Granted(Box<Granted>),
}

/// Begin a sign-in that will be confirmed by photographing a code.
pub fn begin_with_qr(wire: &Wire) -> Result<Session, Failed> {
    let mut request = Writer::new();
    request
        .string(1, crate::web::CLIENT_NAME)
        .varint(2, PLATFORM_STEAM_CLIENT)
        .message(3, device_details());

    let answer = wire.call(SERVICE, "BeginAuthSessionViaQR", request)?;
    Ok(Session {
        client_id: protobuf::number(&answer, 1),
        challenge_url: protobuf::text(&answer, 2),
        request_id: protobuf::field(&answer, 3)
            .and_then(protobuf::Value::as_bytes)
            .unwrap_or_default()
            .to_vec(),
        interval: interval(&answer, 4),
        steam_id: 0,
        confirmations: confirmations(&answer, 5),
    })
}

/// Begin a sign-in with an account name and a password.
///
/// The password is encrypted here, under the key Steam sends for that account
/// name, and the plaintext never leaves this function's caller.
pub fn begin_with_password(
    wire: &Wire,
    account: &str,
    password: &Password,
    guard_data: Option<&str>,
) -> Result<Session, Failed> {
    let key = password_key(wire, account)?;
    let encrypted = key.encrypt(password.plain()).ok_or_else(|| {
        Failed::Unreadable("that password is longer than Steam's key can carry".to_string())
    })?;

    let mut request = Writer::new();
    request
        .string(1, crate::web::CLIENT_NAME)
        .string(2, account)
        .string(3, &base64::encode(&encrypted))
        .varint(4, key.timestamp)
        .bool(5, true)
        .varint(6, PLATFORM_STEAM_CLIENT)
        .varint(7, PERSISTENT)
        .message(9, device_details())
        .string(10, guard_data.unwrap_or_default());

    let answer = wire.call(SERVICE, "BeginAuthSessionViaCredentials", request)?;
    Ok(Session {
        client_id: protobuf::number(&answer, 1),
        request_id: protobuf::field(&answer, 2)
            .and_then(protobuf::Value::as_bytes)
            .unwrap_or_default()
            .to_vec(),
        interval: interval(&answer, 3),
        confirmations: confirmations(&answer, 4),
        steam_id: protobuf::number(&answer, 5),
        challenge_url: None,
    })
}

/// Ask whether a waiting sign-in has been confirmed.
pub fn poll(wire: &Wire, session: &Session) -> Result<Polled, Failed> {
    let mut request = Writer::new();
    request
        .varint(1, session.client_id)
        .bytes(2, &session.request_id);

    let answer = wire.call(SERVICE, "PollAuthSessionStatus", request)?;

    // The refresh token is the whole answer: a poll that has not been
    // confirmed yet succeeds and carries nothing.
    let Some(refresh_token) = protobuf::text(&answer, 3) else {
        return Ok(match protobuf::text(&answer, 2) {
            Some(renewed) => Polled::Renewed(renewed),
            None => Polled::Waiting,
        });
    };

    let steam_id = session
        .steam_id
        // A sign-in by code on a screen never named an account, so the token
        // is the only thing that says whose it is.
        .max(steam_id_of(&refresh_token).unwrap_or_default());
    Ok(Polled::Granted(Box::new(Granted {
        account: protobuf::text(&answer, 6).unwrap_or_default(),
        steam_id,
        refresh_token,
        guard_data: protobuf::text(&answer, 7),
    })))
}

/// Hand Steam the code the user typed, so the next poll succeeds.
pub fn submit_code(
    wire: &Wire,
    session: &Session,
    code: &str,
    confirmation: &Confirmation,
) -> Result<(), Failed> {
    let mut request = Writer::new();
    request
        .varint(1, session.client_id)
        .fixed64(2, session.steam_id)
        .string(3, code)
        .varint(4, confirmation.code());

    wire.call(SERVICE, "UpdateAuthSessionWithSteamGuardCode", request)?;
    Ok(())
}

/// Give up a refresh token, so this machine stops being an authorised device.
///
/// Best-effort by design: what actually ends the session as far as this shell
/// is concerned is the token being deleted from the disk, which happens
/// whether or not Steam is reachable to be told. A user signing out on a
/// machine with no network still expects to be signed out.
pub fn revoke(wire: &Wire, refresh_token: &str, steam_id: u64) -> Result<(), Failed> {
    let mut request = Writer::new();
    request
        .string(1, refresh_token)
        .fixed64(2, steam_id)
        // 0: this token, rather than every token the account has.
        .varint(3, 0);
    wire.call(SERVICE, "RevokeRefreshToken", request)?;
    Ok(())
}

/// Whose account a token is for.
///
/// A refresh token is a JSON Web Token: three parts separated by dots, of
/// which the middle one is the claims as base 64. The signature is Steam's to
/// check and is not looked at here — nothing is being *authorised* by reading
/// this, it is being addressed, and a token whose claims were tampered with
/// would simply be refused by Steam on the first call made with it.
pub fn steam_id_of(token: &str) -> Option<u64> {
    let claims = token.split('.').nth(1)?;
    let claims: serde_json::Value = serde_json::from_slice(&base64::decode(claims)?).ok()?;
    claims.get("sub")?.as_str()?.parse().ok()
}

/// Ask for the RSA key belonging to one account name.
fn password_key(wire: &Wire, account: &str) -> Result<PublicKey, Failed> {
    let mut request = Writer::new();
    request.string(1, account);

    let answer = wire.call(SERVICE, "GetPasswordRSAPublicKey", request)?;
    let modulus = protobuf::text(&answer, 1);
    let exponent = protobuf::text(&answer, 2);
    let timestamp = protobuf::number(&answer, 3);

    modulus
        .zip(exponent)
        .and_then(|(modulus, exponent)| PublicKey::from_hex(&modulus, &exponent, timestamp))
        .ok_or_else(|| Failed::Unreadable("Steam sent no usable key for that account".to_string()))
}

/// What this machine says it is.
///
/// The friendly name is what the user sees on their phone when they confirm
/// the sign-in and in the list of devices they can revoke; everything else is
/// what Steam files the session under. No machine ID is sent: it is optional,
/// it is a fingerprint of the hardware, and nothing in this integration needs
/// Steam to be able to tell one LineXinBar machine from another.
fn device_details() -> Writer {
    let mut details = Writer::new();
    details
        .string(1, crate::web::CLIENT_NAME)
        .varint(2, PLATFORM_STEAM_CLIENT)
        .int32(3, OS_LINUX);
    details
}

/// How long Steam asked to be left between polls.
///
/// Clamped rather than trusted: an interval of zero would be a busy loop
/// against Steam's own servers, and one of an hour would be a sign-in that
/// never appeared to complete. Steam sends five seconds.
fn interval(answer: &protobuf::Message, field: u32) -> f32 {
    protobuf::field(answer, field)
        .and_then(protobuf::Value::as_f32)
        .filter(|interval| interval.is_finite())
        .unwrap_or(5.0)
        .clamp(1.0, 30.0)
}

/// The ways Steam said this sign-in may be confirmed, in the order it offered
/// them — which is its own order of preference.
fn confirmations(answer: &protobuf::Message, field: u32) -> Vec<Confirmation> {
    protobuf::every(answer, field)
        .into_iter()
        .map(|allowed| {
            let allowed = allowed.as_message();
            Confirmation::of(protobuf::number(&allowed, 1), protobuf::text(&allowed, 2))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A token is read only for who it belongs to. Built here rather than
    /// taken from a real one: a real refresh
    /// token is a credential, and one in a test file is one in a repository.
    #[test]
    fn a_token_says_whose_account_it_belongs_to() {
        let claims =
            base64::encode(br#"{"iss":"steam","sub":"76561197960287930","exp":1900000000}"#);
        let token = format!(
            "{}.{claims}.{}",
            base64::encode(b"{}"),
            base64::encode(b"x")
        );

        assert_eq!(steam_id_of(&token), Some(76561197960287930));
    }

    /// Anything that is not a token reads as nothing, rather than as an
    /// account.
    #[test]
    fn rubbish_is_not_a_token() {
        for not_a_token in ["", "....", "not.a.token", "onlyonepart"] {
            assert_eq!(steam_id_of(not_a_token), None, "{not_a_token:?}");
        }
    }

    /// A press on the phone beats a code to type, because this shell is driven
    /// with a thumb. A field only goes up when there is nothing else on offer.
    #[test]
    fn a_press_on_the_phone_is_preferred_to_a_field() {
        let session = |confirmations: Vec<Confirmation>| Session {
            client_id: 1,
            request_id: vec![],
            interval: 5.0,
            steam_id: 0,
            challenge_url: None,
            confirmations,
        };

        assert_eq!(
            session(vec![
                Confirmation::DeviceConfirmation,
                Confirmation::DeviceCode,
            ])
            .code_wanted(),
            None,
            "a field went up over a confirmation that needs no typing"
        );
        assert_eq!(
            session(vec![Confirmation::DeviceCode]).code_wanted(),
            Some(&Confirmation::DeviceCode)
        );
        assert_eq!(
            session(vec![Confirmation::EmailCode(
                "t****@example.com".to_string()
            )])
            .code_wanted(),
            Some(&Confirmation::EmailCode("t****@example.com".to_string()))
        );
        assert_eq!(
            session(vec![Confirmation::None]).code_wanted(),
            None,
            "nothing to confirm is nothing to type"
        );
    }

    /// Every kind survives the trip out to Steam and back as the same kind,
    /// which is what makes a submitted code answer the confirmation it was
    /// asked for.
    #[test]
    fn a_confirmation_kind_is_the_number_it_came_as() {
        for kind in 1..=5u64 {
            assert_eq!(Confirmation::of(kind, None).code(), kind);
        }
        assert_eq!(Confirmation::of(99, None), Confirmation::Unknown(99));
        assert_eq!(Confirmation::Unknown(99).code(), 99);
    }

    /// The hint Steam gives for an email address is repeated to the user, and
    /// a missing one does not produce a sentence with a hole in it.
    #[test]
    fn what_is_asked_for_is_a_sentence() {
        assert_eq!(
            Confirmation::EmailCode("t****@example.com".to_string()).asked(),
            "Enter the code Steam emailed to t****@example.com."
        );
        assert_eq!(
            Confirmation::EmailCode(String::new()).asked(),
            "Enter the code Steam emailed you."
        );
        for way in [
            Confirmation::None,
            Confirmation::DeviceCode,
            Confirmation::DeviceConfirmation,
            Confirmation::EmailConfirmation,
            Confirmation::Unknown(9),
        ] {
            assert!(way.asked().ends_with('.'), "{way:?}");
        }
    }

    /// An interval Steam did not send, or sent as something silly, becomes one
    /// a poll can actually run at — a zero here would be a busy loop against
    /// Steam's own servers.
    #[test]
    fn the_poll_interval_is_never_silly() {
        let with = |value: protobuf::Value| interval(&vec![(3, value)], 3);
        assert_eq!(with(protobuf::Value::Fixed32(5.0f32.to_bits())), 5.0);
        assert_eq!(with(protobuf::Value::Fixed32(0.0f32.to_bits())), 1.0);
        assert_eq!(with(protobuf::Value::Fixed32(9999.0f32.to_bits())), 30.0);
        assert_eq!(with(protobuf::Value::Fixed32(f32::NAN.to_bits())), 5.0);
        assert_eq!(interval(&vec![], 3), 5.0, "no field at all");
    }
}
