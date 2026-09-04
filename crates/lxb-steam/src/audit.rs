//! A record of everything this session drove through Valve's client, and how
//! each of it went.
//!
//! Steam is the one part of this shell that reaches out of the session and
//! operates a program nobody here wrote, over an interface Valve does not
//! promise — a debugging port on the loopback address, expressions evaluated in
//! the client's own JavaScript context. What it is used for is ordinary: start
//! this game, fetch that one, take this one off the disk. But the shape of it is
//! not, and the session that does it should be able to say what it did.
//!
//! That is the whole of what this is for, and it has two readers.
//!
//! A **person**, who has a game that will not install and a shell that told them
//! a sentence about it a minute ago and has since gone back to the bar. The
//! diagnostics panel reads the last of these, so what happened is still there to
//! be read — and the file outlives the session, so it is also what goes in a bug
//! report.
//!
//! And **whoever is asking what this session did to Steam**, which on a machine
//! where the loopback port is open is a fair question. Every operation is here,
//! in order, with what came back.
//!
//! ## Nothing secret is in it
//!
//! Enforced here rather than promised by callers, because the failure is silent
//! and permanent: a line written once into a file that goes into a bug report
//! cannot be taken back.
//!
//! The specific danger is real and not hypothetical. [`crate::webui::sign_in`]
//! evaluates `SteamClient.Auth.SetLoginToken(<the refresh token>, …)` in the
//! client's context, and an expression that throws comes back with Chromium's
//! own exception description — which quotes the source it was thrown from. So
//! every string that passes through here goes through [`safe`] first, and a run
//! of characters long enough and credential-shaped enough to be a token is
//! replaced rather than written.
//!
//! The credential itself lives in [`crate::session`] and is never handled here.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::SystemTime;

/// How many entries are kept in memory for the diagnostics panel to read.
///
/// Enough to cover what somebody just did and its outcome, and no more: this is
/// what the panel shows, and a panel is something read at a glance. The file
/// keeps the rest.
const REMEMBERED: usize = 24;

/// How large the file may get before the oldest half is dropped.
///
/// Small, because every line is short and a session does a handful of these an
/// hour. What this guards against is not a busy session but a stuck one — a
/// client that refuses the same call every ten seconds for a week.
const LARGEST: u64 = 64 * 1024;

/// What a completed operation came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum How {
    /// It was done.
    Done,
    /// Nothing went wrong; Steam will not go on without the person, or this
    /// session is not the one that may. Carries what the user was told.
    Refused(String),
    /// It failed, and this is what was said about it.
    Failed(String),
}

impl How {
    fn said(&self) -> String {
        match self {
            How::Done => "done".to_string(),
            How::Refused(why) => format!("refused: {}", safe(why)),
            How::Failed(why) => format!("failed: {}", safe(why)),
        }
    }
}

/// One line of the record, as the panel reads it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub at: SystemTime,
    /// The operation, in the words the log uses: `install 504230`.
    pub what: String,
    /// What it came to, already safe to show.
    pub how: String,
}

/// The last few, for whoever is looking at the panel.
static LATELY: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

/// Say that an operation has begun.
///
/// Separate from its outcome because the two are minutes apart and the gap is
/// where the interesting failures live: an install that never came back at all
/// is a line with nothing after it, which is exactly what it should look like.
pub fn asked(what: impl std::fmt::Display) {
    write_down(&format!("{what}"), "asked");
}

/// And how it went.
pub fn went(what: impl std::fmt::Display, how: How) {
    write_down(&format!("{what}"), &how.said());
}

/// The last few entries, newest last.
pub fn lately() -> Vec<Entry> {
    LATELY
        .lock()
        .unwrap_or_else(|held| held.into_inner())
        .clone()
}

/// Throw the record away.
///
/// Called when the account goes. It says what somebody did with their Steam,
/// and once they have signed out of it that is no longer this machine's to keep
/// — the same argument the catalogue is dropped on. See [`crate::catalogue::forget`].
pub fn forget() {
    LATELY
        .lock()
        .unwrap_or_else(|held| held.into_inner())
        .clear();
    if let Some(path) = path() {
        let _ = std::fs::remove_file(path);
    }
}

fn write_down(what: &str, how: &str) {
    let what = safe(what);
    let entry = Entry {
        at: SystemTime::now(),
        what: what.clone(),
        how: how.to_string(),
    };
    {
        let mut lately = LATELY.lock().unwrap_or_else(|held| held.into_inner());
        if lately.len() >= REMEMBERED {
            lately.remove(0);
        }
        lately.push(entry);
    }
    // Best effort throughout, and quietly: a record that could not be written
    // is not a reason to fail the operation it is about, and a warning here
    // would be one line of noise per operation on a read-only home.
    let Some(path) = path() else {
        return;
    };
    let _ = append(&path, &format!("{} {what}: {how}\n", stamp()));
}

fn append(path: &std::path::Path, line: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::fs::metadata(path).is_ok_and(|about| about.len() > LARGEST) {
        trim(path)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(line.as_bytes())
}

/// Drop the oldest half.
///
/// Whole lines, and by rewriting rather than by seeking: a record cut in the
/// middle of a line is a record whose first entry is a fragment of somebody
/// else's.
fn trim(path: &std::path::Path) -> std::io::Result<()> {
    let whole = std::fs::read_to_string(path)?;
    let lines: Vec<&str> = whole.lines().collect();
    let keep = lines.split_at(lines.len() / 2).1.join("\n");
    std::fs::write(path, format!("{keep}\n"))
}

/// `$XDG_STATE_HOME/linexinbar/steam-actions.log`.
///
/// The state directory, and the choice is the point of it. Not the cache, which
/// is for what can be fetched again — this cannot, it is a record of what
/// happened. Not the data directory either, which is where the credential is:
/// this is the one Steam file that is meant to be read by a person and sent to
/// somebody, and it should not be sitting next to the one thing that must never
/// leave the machine. See [`crate::session`].
fn path() -> Option<PathBuf> {
    crate::turns::beside_the_state("steam-actions.log")
}

/// The time, as a line of the record spells it.
fn stamp() -> String {
    // Seconds since the epoch rather than a date. This file is read beside a
    // session's own log, whose timestamps are the ones that matter, and turning
    // a `SystemTime` into a local calendar date is a dependency and a set of
    // decisions about a machine's clock that a diagnostic line does not need.
    // The panel formats what it shows from the `SystemTime` it kept.
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

/// The longest a credential-shaped run may be before it is taken out.
///
/// Steam's refresh token is a JWT — three base64url runs separated by dots, the
/// shortest of which is far longer than this. Forty is well above anything that
/// occurs in the sentences this file is otherwise made of: an app id is six
/// digits, an account name is short, and the longest ordinary word in a Steam
/// error is nothing like it.
const TOO_LONG_TO_BE_A_WORD: usize = 40;

/// Take out of a string anything long enough and credential-shaped enough to be
/// a token.
///
/// Deliberately blunt, and deliberately biased. A record with a sentence in it
/// that has had a long identifier replaced is a record that is still readable;
/// a record with somebody's refresh token in it is a credential in a file that
/// goes into bug reports. The two mistakes are not the same size.
///
/// What counts as credential-shaped is base64url's alphabet with the dots a JWT
/// joins its parts with — which is also every hexadecimal digest and every long
/// opaque id, and taking those out too is no loss.
pub fn safe(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        if run.len() >= TOO_LONG_TO_BE_A_WORD {
            out.push_str("[…]");
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for character in text.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
            run.push(character);
        } else {
            flush(&mut run, &mut out);
            out.push(character);
        }
    }
    flush(&mut run, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole reason this module sanitizes anything: a credential can arrive
    /// inside an error, because the expression that carries it is what threw.
    #[test]
    fn a_credential_never_reaches_the_record() {
        // Shaped like the real thing: a JWT is three base64url runs joined by
        // dots, and this is what an exception description quoting the source
        // that threw would carry.
        let token = "eyJ0eXAiOiJKV1QiLCJhbGciOiJFZERTQSJ9.\
                     eyJpc3MiOiJyOmFBQkNfMTIzIiwic3ViIjoiNzY1NjExOTgwNDIzNzE3MjEifQ.\
                     Zm9vYmFyYmF6cXV1eGNvcmdlZ3JhdWx0Z2FycGx5";
        let thrown =
            format!("SyntaxError: SteamClient.Auth.SetLoginToken(\"{token}\", \"someone\")");
        let written = safe(&thrown);
        assert!(!written.contains(token), "{written}");
        assert!(
            !written.contains("eyJ0eXAiOiJKV1Qi"),
            "a token in pieces is still a token: {written}"
        );
        // And what is left still says what went wrong, which is the only
        // reason the line is being kept at all.
        assert!(written.contains("SyntaxError"), "{written}");
        assert!(written.contains("SetLoginToken"), "{written}");
    }

    /// And an ordinary sentence goes through untouched, or the record would be
    /// unreadable.
    #[test]
    fn what_the_user_was_told_is_kept_as_it_was() {
        for sentence in [
            "install 504230",
            "Steam is not signed in, so it cannot fetch this game.",
            "This game needs an agreement accepted before it can be fetched.",
            "uninstall 220200",
            "wake the client",
        ] {
            assert_eq!(safe(sentence), sentence);
        }
    }

    /// The failure that is worth naming: an app id is not credential-shaped and
    /// must never be taken out, however this rule is tightened.
    #[test]
    fn an_app_id_is_not_a_secret() {
        assert_eq!(safe("install 3164500"), "install 3164500");
        assert_eq!(safe("76561198042371721"), "76561198042371721");
    }

    /// An operation and its outcome are two lines, minutes apart, and the gap
    /// is the interesting part: an install that never came back is a line with
    /// nothing after it.
    #[test]
    fn an_operation_and_its_outcome_are_both_remembered() {
        let _turn = crate::one_at_a_time_with_the_environment();
        let state = std::env::temp_dir().join(format!("lxb-audit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        // SAFETY: the environment guard above is what makes this a test that
        // owns the variable for its duration.
        unsafe { std::env::set_var("XDG_STATE_HOME", &state) };
        forget();

        asked("install 504230");
        went(
            "install 504230",
            How::Failed("it would not answer".to_string()),
        );

        let record = lately();
        assert_eq!(record.len(), 2);
        assert_eq!(record[0].how, "asked");
        assert_eq!(record[1].how, "failed: it would not answer");

        let written = std::fs::read_to_string(path().expect("a path")).expect("the record");
        assert_eq!(written.lines().count(), 2);
        assert!(written.contains("install 504230: asked"), "{written}");

        // And signing out takes it away, for the reason the catalogue goes.
        forget();
        assert!(lately().is_empty());
        assert!(!path().expect("a path").exists());

        let _ = std::fs::remove_dir_all(&state);
    }

    /// A stuck session — the same refused call every ten seconds for a week —
    /// must not become a file nobody can open.
    #[test]
    fn the_record_does_not_grow_without_end() {
        let _turn = crate::one_at_a_time_with_the_environment();
        let state = std::env::temp_dir().join(format!("lxb-audit-big-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        // SAFETY: as above.
        unsafe { std::env::set_var("XDG_STATE_HOME", &state) };
        forget();

        let path = path().expect("a path");
        for _ in 0..2000 {
            went(
                "install 504230",
                How::Failed("Valve's client would not answer this call".to_string()),
            );
        }
        let written = std::fs::metadata(&path).expect("the record").len();
        assert!(written <= LARGEST + 1024, "{written} bytes");
        // And what is left is whole lines, newest last.
        let whole = std::fs::read_to_string(&path).expect("the record");
        assert!(whole.lines().all(|line| line.contains("install 504230")));

        forget();
        let _ = std::fs::remove_dir_all(&state);
    }
}
