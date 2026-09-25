//! Signing Heroic in to Epic with a code approved on a phone.
//!
//! ```text
//! code ──► (poll every few seconds) ──► approved ──► exchange ──► legendary auth --token
//!   ▲            │ ten minutes pass                                    │
//!   └────────────┘ a fresh code, up to three                           ▼
//!                                                               signed-in / failed
//! ```
//!
//! The shell ends a sign-in somebody cancelled by ending this process; nothing
//! is held that needs putting away until a code has been approved, and from
//! then on the run is a few seconds long.
//!
//! **The exchange code goes to legendary as an argument**, because that is the
//! only way legendary takes one — Heroic passes its authorization code the same
//! way. It is single-use, lasts five minutes, and is on a command line only
//! this user's own processes can read, for the second legendary takes to
//! redeem it.

use std::io::Write;
use std::process::Stdio;
use std::time::{Duration, Instant};

use crate::epic::{Epic, Poll, Session};
use crate::heroic::{self, Paths};
use crate::report::{Installation, Reason, SignIn, PROTOCOL};
use crate::secret::Secret;

/// How many codes are offered before the sign-in gives up: half an hour.
const ROUNDS: u32 = 3;

/// Sign Heroic in, saying each step on `out`. Whether it ended signed in.
pub fn run(out: &mut impl Write, installation: &Installation, paths: &Paths) -> bool {
    if heroic::in_the_way() {
        return failed(out, Reason::HeroicRunning, None, "Heroic is open");
    }
    let epic = Epic::new();
    for round in 1..=ROUNDS {
        let code = match epic.device_code() {
            Ok(code) => code,
            Err(reason) => return failed(out, reason, None, "no code was issued"),
        };
        eprintln!(
            "sign-in: code {round} of {ROUNDS}, for {} seconds, asked every {} seconds",
            code.expires_in, code.interval
        );
        let shown = say(
            out,
            &SignIn::Code {
                protocol: PROTOCOL,
                code: code.user_code.clone(),
                url: code.url.clone(),
                short_url: code.short_url.clone(),
                expires_in: code.expires_in,
            },
        );
        if !shown {
            // Nobody is reading any more: the shell has gone, or put the
            // panel away without ending this.
            return false;
        }

        let until = Instant::now() + Duration::from_secs(u64::from(code.expires_in));
        let mut interval = code.interval;
        let mut unanswered = false;
        let outcome = loop {
            std::thread::sleep(Duration::from_secs(u64::from(interval)));
            if Instant::now() >= until {
                break if unanswered {
                    Poll::Offline
                } else {
                    Poll::Expired
                };
            }
            match epic.poll(&code) {
                Poll::Pending => unanswered = false,
                Poll::SlowDown => interval = (interval + 5).min(60),
                // A moment without a connection is not the end of a sign-in:
                // the phone may well be on a different network. It only
                // becomes the answer if it outlasts the code.
                Poll::Offline => unanswered = true,
                done => break done,
            }
        };
        match outcome {
            Poll::Approved(session) => return finish(out, &epic, session, installation, paths),
            Poll::Expired if round < ROUNDS => continue,
            Poll::Expired => return failed(out, Reason::Expired, None, "every code ran out"),
            Poll::Offline => return failed(out, Reason::Offline, None, "Epic stopped answering"),
            Poll::ActionNeeded(url) => {
                return failed(
                    out,
                    Reason::ActionNeeded,
                    url,
                    "Epic wants something done first",
                )
            }
            Poll::Declined(said) => return failed(out, Reason::Declined, None, &said),
            Poll::Pending | Poll::SlowDown => unreachable!("the loop only ends on an answer"),
        }
    }
    false
}

/// Hand the approved sign-in to Heroic's legendary.
fn finish(
    out: &mut impl Write,
    epic: &Epic,
    session: Session,
    installation: &Installation,
    paths: &Paths,
) -> bool {
    say(
        out,
        &SignIn::Approved {
            protocol: PROTOCOL,
            name: session.name.clone(),
        },
    );
    let handed = epic
        .exchange(&session)
        .and_then(|code| redeem(installation, paths, &code));
    // Whatever happened, the device client's session is no longer wanted.
    epic.end(session);
    match handed {
        Ok(()) => match heroic::account(paths) {
            Some(name) => say(
                out,
                &SignIn::SignedIn {
                    protocol: PROTOCOL,
                    name,
                },
            ),
            None => failed(out, Reason::Legendary, None, "legendary wrote no account"),
        },
        Err(reason) => failed(out, reason, None, "Heroic could not be signed in"),
    }
}

/// `legendary auth --token`, which redeems an exchange code as the launcher
/// and writes the session where Heroic reads it.
fn redeem(installation: &Installation, paths: &Paths, code: &Secret) -> Result<(), Reason> {
    // legendary keeps a session it already holds, if it is still good, and
    // ignores the code it was given — so a sign-in over an account would
    // quietly change nothing. The person has just approved an account on
    // their phone; that is the one Heroic is to hold.
    if paths.user().exists() {
        eprintln!("sign-in: replacing the account Heroic held");
        let cleared = heroic::legendary(installation, paths)
            .args(["auth", "--delete"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if !cleared.is_ok_and(|status| status.success()) || paths.user().exists() {
            return Err(Reason::Legendary);
        }
    }
    let out = heroic::legendary(installation, paths)
        .args(["auth", "--token", code.expose()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|err| {
            eprintln!("sign-in: flatpak could not be run: {err}");
            Reason::NoHeroic
        })?;
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    for line in said.lines() {
        eprintln!("legendary: {line}");
    }
    if out.status.success() && paths.user().exists() {
        Ok(())
    } else {
        Err(Reason::Legendary)
    }
}

/// Sign Heroic out: `legendary auth --delete`.
///
/// Heroic's own sign-out also clears the web session of its sign-in window;
/// that is Heroic's, and a sign-in made here never used it.
pub fn sign_out(installation: &Installation, paths: &Paths) -> Result<(), Reason> {
    if heroic::in_the_way() {
        return Err(Reason::HeroicRunning);
    }
    let out = heroic::legendary(installation, paths)
        .args(["auth", "--delete"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|err| {
            eprintln!("sign-out: flatpak could not be run: {err}");
            Reason::NoHeroic
        })?;
    for line in String::from_utf8_lossy(&out.stderr).lines() {
        eprintln!("legendary: {line}");
    }
    if paths.user().exists() {
        return Err(Reason::Legendary);
    }
    // What Heroic runs straight after its own sign-out: legendary takes the
    // signed-out account's cached files away with it.
    match heroic::legendary(installation, paths)
        .arg("cleanup")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) if status.success() => {}
        _ => eprintln!("sign-out: legendary did not tidy up after itself"),
    }
    Ok(())
}

fn failed(out: &mut impl Write, reason: Reason, url: Option<String>, note: &str) -> bool {
    eprintln!("sign-in: {note} ({reason:?})");
    say(
        out,
        &SignIn::Failed {
            protocol: PROTOCOL,
            reason,
            url,
            note: note.to_string(),
        },
    );
    false
}

/// Write one line; whether anybody could be written to.
fn say(out: &mut impl Write, record: &SignIn) -> bool {
    let Ok(json) = serde_json::to_string(record) else {
        return false;
    };
    writeln!(out, "{json}").and_then(|()| out.flush()).is_ok()
}
