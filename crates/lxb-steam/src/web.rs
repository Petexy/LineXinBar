//! The wire to Steam: one HTTPS agent, and the two shapes of call made over it.
//!
//! Steam authentication is exposed on the plain HTTPS front at
//! `api.steampowered.com`: a service method is a URL, its protobuf request
//! travels base 64 under the name `input_protobuf_encoded`, and the answer
//! carries its result code in a header. The refresh token it grants is
//! deliberately not used as a Web API bearer. [`crate::cm`] hands it to
//! Steam's persistent Connection Manager session, which supplies licenses and
//! the PICS catalogue.
//!
//! ## Two ways round, and no choosing between them
//!
//! Where that encoded request travels is not this crate's preference. A method
//! that *changes* something is posted and its request is a form field; a
//! method that only *asks* something is fetched and its request is a query
//! parameter. Each of them refuses the other way round with HTTP 405, so
//! [`Wire::call`] and [`Wire::fetch`] are two calls rather than one with a
//! flag: which one a method wants is a fact about that method, written down at
//! the call.
//!
//! ## What a failure is
//!
//! Steam answers a failed call with HTTP 200 and an `x-eresult` header more
//! often than it answers with a failing status, so both are read and both come
//! back as the same [`Failed`]. A call that could not be made at all — no
//! route, no DNS, a certificate that does not check out — is
//! [`Failed::Unreachable`], which the shell says out loud as being offline
//! rather than as Steam refusing anything.

use std::time::Duration;

use crate::base64;
use crate::protobuf;

/// Where the services live.
const HOST: &str = "https://api.steampowered.com";

/// How long any one call may take. Generous, because a poll of a sign-in is a
/// long call by design and a library of a thousand games is a large answer;
/// short enough that a wire that has gone away is noticed within one panel's
/// worth of patience rather than never.
const TIMEOUT: Duration = Duration::from_secs(30);

/// What this session calls itself to Steam.
///
/// It is what the user sees in the confirmation on their phone and in the list
/// of devices they can sign out again from, so it is the name of this shell
/// and not of an HTTP library.
pub const CLIENT_NAME: &str = "LineXinBar";

/// Steam's own name for what went wrong, as the `x-eresult` header carries it.
///
/// Only the ones this crate acts on differently are named. Everything else is
/// [`EResult::Other`] and is reported by number, because a code the shell
/// cannot explain is still worth putting in the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EResult {
    Ok,
    /// The account name or the password is wrong. One code for both, on
    /// purpose: Steam will not say which, and neither will this.
    InvalidPassword,
    /// The sign-in needs a Steam Guard code that was not supplied.
    AccountLogonDenied,
    /// The code that was supplied is not the right one.
    TwoFactorCodeMismatch,
    /// The sign-in was not confirmed in time, or the session is being polled
    /// after it lapsed.
    Expired,
    /// Too many attempts from here, too quickly.
    RateLimitExceeded,
    /// The token this call was made with is no longer good — the user signed
    /// this device out, or changed their password.
    AccessDenied,
    Other(u32),
}

impl EResult {
    fn of(code: u32) -> EResult {
        match code {
            1 => EResult::Ok,
            5 => EResult::InvalidPassword,
            15 => EResult::AccessDenied,
            27 => EResult::Expired,
            65 => EResult::TwoFactorCodeMismatch,
            63 | 66 | 85 | 88 => EResult::AccountLogonDenied,
            84 => EResult::RateLimitExceeded,
            other => EResult::Other(other),
        }
    }

    /// What to put in front of the user when a call fails.
    ///
    /// Sentences rather than codes: this ends up on a panel in the middle of a
    /// television, read by somebody who is trying to sign in and not by
    /// somebody reading a log.
    pub fn said(self) -> String {
        match self {
            EResult::Ok => "Steam said that worked.".to_string(),
            EResult::InvalidPassword => {
                "That account name and password were not accepted.".to_string()
            }
            EResult::AccountLogonDenied => "This account needs a Steam Guard code.".to_string(),
            EResult::TwoFactorCodeMismatch => "That code was not right.".to_string(),
            EResult::Expired => "That sign-in took too long. Start again.".to_string(),
            EResult::RateLimitExceeded => {
                "Too many attempts. Wait a few minutes and try again.".to_string()
            }
            EResult::AccessDenied => {
                "Steam no longer accepts this sign-in. Sign in again.".to_string()
            }
            EResult::Other(code) => format!("Steam refused that (error {code})."),
        }
    }
}

/// Why a call did not produce an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failed {
    /// Steam answered, and the answer was no.
    Refused(EResult),
    /// Steam could not be reached at all: no network, no name, no route, or a
    /// certificate that does not check out.
    Unreachable(String),
    /// Steam answered with something that is not the message that was asked
    /// for. Rare, and worth telling apart from a refusal: it means this crate
    /// and Steam disagree about a message rather than about an account.
    Unreadable(String),
    /// Steam's front door turned the call away with a failing HTTP status and
    /// no result code of its own.
    ///
    /// Told apart from [`Failed::Refused`] because it is not a refusal of
    /// anything the user did — nothing of theirs was looked at. A 405 is a
    /// method asked for the wrong way round, a 5xx is Steam's own trouble, and
    /// neither is answered by trying a different password. The status is kept
    /// so the log says which.
    Rejected(u16),
}

impl Failed {
    pub fn said(&self) -> String {
        match self {
            Failed::Refused(result) => result.said(),
            Failed::Unreachable(_) => {
                "Steam could not be reached. Check this machine's network.".to_string()
            }
            Failed::Unreadable(_) => "Steam answered with something unexpected.".to_string(),
            // Nothing about the account, because this never reached one.
            Failed::Rejected(_) => "Steam would not take that request.".to_string(),
        }
    }
}

impl std::fmt::Display for Failed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failed::Refused(result) => write!(f, "Steam refused the call: {result:?}"),
            Failed::Unreachable(why) => write!(f, "Steam could not be reached: {why}"),
            Failed::Unreadable(why) => write!(f, "Steam's answer could not be read: {why}"),
            Failed::Rejected(status) => {
                write!(f, "Steam would not take the call: HTTP {status}")
            }
        }
    }
}

/// One HTTPS agent, held for the life of the session.
///
/// Held rather than made per call because it is what keeps the connection —
/// and therefore the TLS handshake — between one poll of a sign-in and the
/// next, and a sign-in is polled every few seconds for as long as the code is
/// on screen.
pub struct Wire {
    agent: ureq::Agent,
    /// Where the services live: [`HOST`] in every session there has ever been.
    ///
    /// A field rather than the constant read at each call so that a test can
    /// put a stand-in on the loopback and watch how a method is asked for.
    /// Which verb a method wants is the one thing about this wire that cannot
    /// be established by reading it — it is a fact about Steam's front door —
    /// and getting it wrong is silent until somebody tries to sign in.
    host: String,
}

impl Default for Wire {
    fn default() -> Self {
        Self::new()
    }
}

impl Wire {
    pub fn new() -> Wire {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            // Steam says what went wrong in a header, and does it under a
            // failing status as often as under 200. Letting the status raise
            // an error of its own would throw away the half of the answer that
            // says which failure it was.
            .http_status_as_error(false)
            .timeout_global(Some(TIMEOUT))
            .user_agent(concat!("LineXinBar/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Wire {
            agent,
            host: HOST.to_string(),
        }
    }

    /// A wire that talks to a stand-in rather than to Steam.
    #[cfg(test)]
    pub(crate) fn to(host: &str) -> Wire {
        Wire {
            host: host.to_string(),
            ..Wire::new()
        }
    }

    /// Post one service method a protobuf message, and read the protobuf
    /// message that comes back.
    ///
    /// For the methods that *do* something: beginning a sign-in, polling one,
    /// handing over a Steam Guard code, giving a token up. Posting a method
    /// that only answers a question is refused — see [`Self::fetch`].
    pub fn call(
        &self,
        interface: &str,
        method: &str,
        request: protobuf::Writer,
    ) -> Result<protobuf::Message, Failed> {
        let body = format!("input_protobuf_encoded={}", encoded(request));
        let response = self
            .agent
            .post(self.address(interface, method))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .send(body.as_bytes())
            .map_err(|err| Failed::Unreachable(err.to_string()))?;
        answer(response)
    }

    /// Ask one service method a question, and read the protobuf message that
    /// comes back.
    ///
    /// The same envelope as [`Self::call`] with the request in the query
    /// string instead of the body, because the methods that only *answer*
    /// something are fetched and refuse a post with HTTP 405.
    ///
    /// `GetPasswordRSAPublicKey` is one of them, and it is the first call of
    /// every sign-in by account name and password: posted, it answered 405
    /// before the password had been so much as encrypted, and the panel said
    /// Steam had refused error 405 — a sentence about nothing the user had
    /// done, in front of somebody whose account was perfectly good.
    pub fn fetch(
        &self,
        interface: &str,
        method: &str,
        request: protobuf::Writer,
    ) -> Result<protobuf::Message, Failed> {
        let url = format!(
            "{}?input_protobuf_encoded={}",
            self.address(interface, method),
            encoded(request)
        );
        let response = self
            .agent
            .get(&url)
            .call()
            .map_err(|err| Failed::Unreachable(err.to_string()))?;
        answer(response)
    }

    /// Where one service method lives.
    fn address(&self, interface: &str, method: &str) -> String {
        format!("{}/{interface}/{method}/v1/", self.host)
    }
}

/// One protobuf request as it travels: base 64, and then safe to put in a
/// form body or a query string.
fn encoded(request: protobuf::Writer) -> String {
    form_encode(&base64::encode(&request.finish()))
}

/// Read one answer, whichever way it was asked for.
fn answer(response: ureq::http::Response<ureq::Body>) -> Result<protobuf::Message, Failed> {
    // The verdict first: an answer that says no has a body, and the body of a
    // refusal is not the message that was asked for.
    let status = response.status().as_u16();
    let code = response
        .headers()
        .get("x-eresult")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    verdict(status, code.as_deref())?;

    let bytes = response
        .into_body()
        .read_to_vec()
        .map_err(|err| Failed::Unreadable(err.to_string()))?;
    Ok(protobuf::read(&bytes))
}

/// Whether an answer carries the message that was asked for, read off its
/// status and the result code Steam puts in a header.
///
/// Its own function because the two do not agree and the order matters:
/// Steam's own code is the answer wherever there is one, whatever the status
/// alongside it, and the status only speaks where there is none.
fn verdict(status: u16, code: Option<&str>) -> Result<(), Failed> {
    let code = code
        .and_then(|value| value.parse::<u32>().ok())
        .map(EResult::of);
    match code {
        Some(EResult::Ok) => Ok(()),
        Some(result) => Err(Failed::Refused(result)),
        // No code of Steam's own. A 2xx is an answer, which is what the older
        // methods do; anything else is the front door turning the call away
        // before any account was looked at, which is not a refusal of one.
        None if (200..300).contains(&status) => Ok(()),
        None => Err(Failed::Rejected(status)),
    }
}

/// Percent-encode one value of a form or a query string.
///
/// Base 64 is why this exists: `+`, `/` and `=` all mean something in a form
/// body, and a token or a ciphertext sent raw would arrive as a different
/// string. Everything outside the unreserved set is encoded, which is more
/// than the minimum and is never wrong.
fn form_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three characters base 64 ends up with are all reserved in a form
    /// body, so a ciphertext sent unencoded arrives as a different one.
    #[test]
    fn base_sixty_four_survives_a_form_body() {
        assert_eq!(form_encode("ab+/=="), "ab%2B%2F%3D%3D");
        assert_eq!(form_encode("plain-value_1.0~"), "plain-value_1.0~");
        assert_eq!(form_encode("a b"), "a%20b");
    }

    /// The codes the shell tells apart really are told apart, and everything
    /// else keeps its number rather than being flattened into one failure.
    #[test]
    fn the_result_codes_that_change_what_happens_are_named() {
        assert_eq!(EResult::of(1), EResult::Ok);
        assert_eq!(EResult::of(5), EResult::InvalidPassword);
        assert_eq!(EResult::of(65), EResult::TwoFactorCodeMismatch);
        assert_eq!(EResult::of(84), EResult::RateLimitExceeded);
        assert_eq!(EResult::of(999), EResult::Other(999));
        assert!(EResult::Other(999).said().contains("999"));
    }

    /// The bug this guards put "Steam refused that (error 405)" in front of
    /// everybody who signed in with an account name and a password: 405 is not
    /// a result code at all, it is the front door saying the method was asked
    /// for the wrong way round, and it was being read as a number Steam had
    /// refused an account with.
    #[test]
    fn a_status_steam_sent_no_code_with_is_not_a_refusal_of_an_account() {
        assert_eq!(verdict(405, None), Err(Failed::Rejected(405)));
        assert_eq!(verdict(500, None), Err(Failed::Rejected(500)));
        // A code of Steam's own is the answer wherever there is one, whatever
        // the status it came alongside.
        assert_eq!(verdict(200, None), Ok(()));
        assert_eq!(verdict(204, Some("1")), Ok(()));
        assert_eq!(
            verdict(200, Some("5")),
            Err(Failed::Refused(EResult::InvalidPassword))
        );
        assert_eq!(
            verdict(401, Some("5")),
            Err(Failed::Refused(EResult::InvalidPassword))
        );
        // And a header that is not a number at all is no code.
        assert_eq!(verdict(403, Some("")), Err(Failed::Rejected(403)));

        // What it says blames nothing of the user's, because nothing of theirs
        // was looked at.
        let said = Failed::Rejected(405).said();
        assert!(!said.contains("405"), "{said:?}");
        assert!(!said.to_lowercase().contains("password"), "{said:?}");
        assert!(!said.to_lowercase().contains("account"), "{said:?}");
    }

    /// A fetched method carries its request in the query string, escaped the
    /// same way a form body escapes it.
    #[test]
    fn a_fetched_method_carries_its_request_in_the_query_string() {
        let wire = Wire::new();
        assert_eq!(
            wire.address("IAuthenticationService", "GetPasswordRSAPublicKey"),
            "https://api.steampowered.com/IAuthenticationService/GetPasswordRSAPublicKey/v1/"
        );
        let mut request = protobuf::Writer::new();
        request.string(1, "someone");
        assert_eq!(encoded(request), "Cgdzb21lb25l");
        let mut padded = protobuf::Writer::new();
        padded.string(1, "gaben");
        assert_eq!(encoded(padded), "CgVnYWJlbg%3D%3D");
    }

    /// Everything the user can be shown is a sentence, because it is read off
    /// a television by somebody signing in.
    #[test]
    fn every_failure_says_something_a_person_can_read() {
        for failure in [
            Failed::Refused(EResult::InvalidPassword),
            Failed::Unreachable("connection refused".to_string()),
            Failed::Unreadable("eof".to_string()),
            Failed::Rejected(405),
        ] {
            let said = failure.said();
            assert!(said.ends_with('.'), "{said:?}");
            assert!(
                said.chars().next().is_some_and(char::is_uppercase),
                "{said:?}"
            );
        }
    }
}
