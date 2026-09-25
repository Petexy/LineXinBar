//! Epic's account service, for as much of it as a code on a phone needs.
//!
//! ## Why a password field is not here
//!
//! Epic signs people in on its own web page, behind bot protection, and gives
//! no client this shell could use a password grant. So the shell never sees a
//! password at all: it shows a code, the person approves it on their phone at
//! `epicgames.com/activate` — with whatever second factor Epic asks there — and
//! this polls until Epic says it was approved. It is how televisions and
//! consoles sign in to Epic.
//!
//! ## Two clients, on purpose
//!
//! The launcher's client, which legendary and Heroic use, is not allowed the
//! device-code grant. Fortnite's Switch client is, so the code is asked for
//! as that client, and the session it yields is used for one thing only: an
//! **exchange code**, which is how Epic hands a sign-in from one client to
//! another. legendary redeems it as the launcher (`legendary auth --token`),
//! with whatever credentials it currently holds — legendary may replace its own
//! from `api.legendary.gl`, which is why this never names the launcher's — and
//! the Switch session is ended straight after. Epic's approval page names the
//! client asking, so the person approving may be told it is Fortnite.
//!
//! Proven on the account this was written against, on 2026-09-24: see
//! `EPIC-TO-DO.MD`.

use std::time::Duration;

use serde_json::Value;

use crate::report::Reason;
use crate::secret::Secret;

/// Epic's OAuth endpoints.
const OAUTH: &str = "https://account-public-service-prod.ol.epicgames.com/account/api/oauth";

/// `fortniteNewSwitchGameClient`, as published in MixV2/EpicResearch's
/// `docs/auth/auth_clients.md` — a client that is allowed the device-code
/// grant. Public by nature: it ships in the game.
const DEVICE_CLIENT: (&str, &str) = (
    "98f7e42c2e3a4f86a74eb43fbb41ed39",
    "0a2449a2-001a-451e-afec-3e812901c4d7",
);

/// The most any one answer from Epic is read to.
const LIMIT: u64 = 1024 * 1024;

/// A code to show, and what polling for it needs.
pub struct DeviceCode {
    pub user_code: String,
    pub url: String,
    pub short_url: String,
    pub expires_in: u32,
    pub interval: u32,
    device_code: Secret,
}

/// A signed-in session of the device client.
pub struct Session {
    access: Secret,
    pub name: Option<String>,
}

/// What one poll found.
#[derive(Debug)]
pub enum Poll {
    /// Not approved yet.
    Pending,
    /// Epic wants fewer questions.
    SlowDown,
    Approved(Session),
    /// The code is no longer one Epic knows.
    Expired,
    /// Epic wants something done in a browser first, at this address.
    ActionNeeded(Option<String>),
    /// Refused; the error code, for the log.
    Declined(String),
    /// Epic could not be reached this time.
    Offline,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

pub struct Epic {
    agent: ureq::Agent,
}

impl Epic {
    pub fn new() -> Epic {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(20)))
            // Epic answers a refusal with a 4xx and a JSON body saying which
            // refusal; that body is the whole of how one is told from another.
            .http_status_as_error(false)
            .user_agent(concat!(
                "LXB/",
                env!("CARGO_PKG_VERSION"),
                " Heroic companion"
            ))
            .build()
            .into();
        Epic { agent }
    }

    /// Ask for a code to show.
    pub fn device_code(&self) -> Result<DeviceCode, Reason> {
        let (status, answer) = read(
            self.agent
                .post(&format!("{OAUTH}/token"))
                .header("Authorization", &basic(DEVICE_CLIENT))
                .send_form([("grant_type", "client_credentials")]),
        )?;
        let client = answer.token("access_token").ok_or_else(|| {
            eprintln!("sign-in: no client token ({status}, {})", answer.error());
            Reason::Declined
        })?;

        let (status, answer) = read(
            self.agent
                .post(&format!("{OAUTH}/deviceAuthorization"))
                .header("Authorization", &format!("bearer {}", client.expose()))
                .send_form([("prompt", "login")]),
        )?;
        drop(client);
        let device_code = answer.token("device_code");
        let (Some(device_code), Some(user_code), Some(url)) = (
            device_code,
            answer.text("user_code"),
            answer.text("verification_uri_complete"),
        ) else {
            eprintln!("sign-in: no device code ({status}, {})", answer.error());
            return Err(Reason::Declined);
        };
        Ok(DeviceCode {
            short_url: answer
                .text("verification_uri")
                .unwrap_or_else(|| "https://www.epicgames.com/activate".to_string()),
            user_code,
            url,
            expires_in: answer.number("expires_in").unwrap_or(600).clamp(60, 3600),
            interval: answer.number("interval").unwrap_or(10).clamp(2, 60),
            device_code,
        })
    }

    /// Ask once whether the code has been approved.
    pub fn poll(&self, code: &DeviceCode) -> Poll {
        match read(
            self.agent
                .post(&format!("{OAUTH}/token"))
                .header("Authorization", &basic(DEVICE_CLIENT))
                .send_form([
                    ("grant_type", "device_code"),
                    ("device_code", code.device_code.expose()),
                ]),
        ) {
            Ok((status, answer)) => classify(status, answer),
            Err(Reason::Offline) => Poll::Offline,
            Err(_) => Poll::Declined("unreadable answer".to_string()),
        }
    }

    /// Turn the device client's session into a one-use code legendary can
    /// redeem as the launcher. Lasts five minutes.
    pub fn exchange(&self, session: &Session) -> Result<Secret, Reason> {
        let (status, answer) = read(
            self.agent
                .get(&format!("{OAUTH}/exchange"))
                .header(
                    "Authorization",
                    &format!("bearer {}", session.access.expose()),
                )
                .call(),
        )?;
        answer.token("code").ok_or_else(|| {
            eprintln!("sign-in: no exchange code ({status}, {})", answer.error());
            Reason::Declined
        })
    }

    /// End the device client's session, so that nothing is left signed in as
    /// Fortnite once legendary holds the launcher's.
    pub fn end(&self, session: Session) {
        let answer = self
            .agent
            .delete(&format!(
                "{OAUTH}/sessions/kill/{}",
                session.access.expose()
            ))
            .header(
                "Authorization",
                &format!("bearer {}", session.access.expose()),
            )
            .call();
        // The error itself is not printed: the token is in this address, and
        // an error about a request may repeat the address it was sent to.
        match answer {
            Ok(response) => eprintln!("sign-in: device session ended ({})", response.status()),
            Err(_) => eprintln!("sign-in: device session could not be ended; it expires by itself"),
        }
    }
}

/// What a poll's answer means.
fn classify(status: u16, answer: Answer) -> Poll {
    if let Some(access) = answer.token("access_token") {
        return Poll::Approved(Session {
            access,
            name: answer.text("displayName"),
        });
    }
    let error = answer.error();
    if error.contains("authorization_pending") {
        Poll::Pending
    } else if error.contains("slow_down") {
        Poll::SlowDown
    } else if error.contains("corrective_action_required") {
        Poll::ActionNeeded(answer.text("continuationUrl"))
    } else if error.contains("expired") || error.contains("not_found") {
        Poll::Expired
    } else {
        Poll::Declined(format!("{status} {error}"))
    }
}

/// One answer from Epic, parsed, with every string in it wiped when it goes.
struct Answer(Value);

impl Answer {
    fn text(&self, key: &str) -> Option<String> {
        self.0[key]
            .as_str()
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    }

    fn token(&self, key: &str) -> Option<Secret> {
        self.text(key).map(Secret::new)
    }

    fn number(&self, key: &str) -> Option<u32> {
        self.0[key].as_u64().and_then(|n| u32::try_from(n).ok())
    }

    /// Epic's error code, which names the refusal and carries nothing secret.
    fn error(&self) -> String {
        self.0["errorCode"].as_str().unwrap_or("").to_string()
    }
}

impl Drop for Answer {
    fn drop(&mut self) {
        crate::secret::wipe(&mut self.0);
    }
}

fn read(
    sent: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<(u16, Answer), Reason> {
    let mut response = sent.map_err(|err| {
        eprintln!("sign-in: Epic could not be reached: {err}");
        Reason::Offline
    })?;
    let status = response.status().as_u16();
    let mut bytes = response
        .body_mut()
        .with_config()
        .limit(LIMIT)
        .read_to_vec()
        .map_err(|_| Reason::Offline)?;
    let parsed = if bytes.is_empty() {
        Ok(Value::Null)
    } else {
        serde_json::from_slice(&bytes)
    };
    bytes.fill(0);
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
    let value = parsed.map_err(|_| {
        eprintln!("sign-in: Epic answered {status} with something that is not JSON");
        Reason::Declined
    })?;
    Ok((status, Answer(value)))
}

/// `Authorization: basic …` for a client.
fn basic((id, secret): (&str, &str)) -> String {
    format!("basic {}", base64(format!("{id}:{secret}").as_bytes()))
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for place in 0..4 {
            if place <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * place) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_client_is_introduced_the_way_epic_expects() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        // The header every tool that uses this client sends.
        assert_eq!(
            basic(DEVICE_CLIENT),
            "basic OThmN2U0MmMyZTNhNGY4NmE3NGViNDNmYmI0MWVkMzk6MGEyNDQ5YTItMDAxYS00NTFlLWFmZWMtM2U4MTI5MDFjNGQ3"
        );
    }

    /// Epic's answers while a code waits, as they came back on 2026-09-24,
    /// and the ones it gives when something else is wrong.
    #[test]
    fn a_poll_is_read_by_the_error_code_epic_gives() {
        let pending = json!({
            "errorCode": "errors.com.epicgames.account.oauth.authorization_pending",
            "errorMessage": "The authorization server is awaiting user authorization."
        });
        assert!(matches!(classify(400, Answer(pending)), Poll::Pending));
        assert!(matches!(
            classify(
                400,
                Answer(json!({"errorCode": "errors.com.epicgames.account.oauth.slow_down"}))
            ),
            Poll::SlowDown
        ));
        assert!(matches!(
            classify(
                400,
                Answer(
                    json!({"errorCode": "errors.com.epicgames.account.oauth.expired_device_code"})
                )
            ),
            Poll::Expired
        ));
        assert!(matches!(
            classify(
                400,
                Answer(
                    json!({"errorCode": "errors.com.epicgames.account.oauth.authorization_code_not_found"})
                )
            ),
            Poll::Expired
        ));
        match classify(
            400,
            Answer(json!({
                "errorCode": "errors.com.epicgames.oauth.corrective_action_required",
                "continuationUrl": "https://www.epicgames.com/id/login/correction"
            })),
        ) {
            Poll::ActionNeeded(Some(url)) => {
                assert_eq!(url, "https://www.epicgames.com/id/login/correction")
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            classify(
                403,
                Answer(json!({"errorCode": "errors.com.epicgames.account.account_locked"}))
            ),
            Poll::Declined(_)
        ));
        match classify(
            200,
            Answer(json!({"access_token": "t", "displayName": "Somebody", "account_id": "a"})),
        ) {
            Poll::Approved(session) => assert_eq!(session.name.as_deref(), Some("Somebody")),
            other => panic!("{other:?}"),
        }
    }

    /// A session can be put in a log line without its token going with it.
    #[test]
    fn a_session_never_prints_its_token() {
        let session = Session {
            access: Secret::new("eg1~very-secret".to_string()),
            name: Some("Somebody".to_string()),
        };
        let shown = format!("{session:?}");
        assert!(!shown.contains("very-secret"), "{shown}");
        assert!(shown.contains("Somebody"), "{shown}");
    }
}
