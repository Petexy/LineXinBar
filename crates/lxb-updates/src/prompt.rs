//! Recognized native questions share the same interpretation in the shell and
//! coordinator. This observes output; it never supplies an answer to a tool.
use crate::{Phase, Snapshot};
use std::hash::{Hash, Hasher};

/// Identity of the text before the last nonempty line. Unlike a line count,
/// this also changes when the bounded snapshot drops an old line. It separates
/// two identically worded prompts while allowing an echoed answer on the same
/// line to be suppressed. This is local UI state, not an authorization token.
pub fn position(snapshot: &Snapshot) -> u64 {
    let end = snapshot
        .output
        .iter()
        .rposition(|s| !s.trim_end().is_empty())
        .unwrap_or(0);
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    snapshot.child.hash(&mut hash);
    snapshot.output[..end].hash(&mut hash);
    hash.finish()
}

pub fn yes_or_no(line: &str) -> bool {
    let line = line.to_ascii_lowercase();
    ["[y/n", "[y|n", "(y/n", "[n/y", "(n/y", "[yes/no", "(yes/no"]
        .iter()
        .any(|form| line.contains(form))
}

/// Echo is a terminal setting, not evidence that a tool is awaiting a secret.
/// Flatpak disables it while drawing progress. Only offer a password field
/// automatically when the last output also contains an explicit secret prompt.
/// Tools run with LC_ALL=C; unfamiliar prompts remain accessible via Respond.
pub fn password(snapshot: &Snapshot) -> bool {
    if snapshot.phase != Phase::Running || !snapshot.secret {
        return false;
    }
    let Some(line) = snapshot
        .output
        .iter()
        .rev()
        .map(|s| s.trim())
        .find(|s| !s.is_empty())
    else {
        return false;
    };
    let line = line.to_ascii_lowercase();
    let question = line.ends_with(':')
        || line.ends_with('?')
        || matches!(line.as_str(), "password" | "passphrase" | "pin");
    question
        && line
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|word| matches!(word, "password" | "passphrase" | "pin"))
}

#[derive(Clone, PartialEq, Eq)]
enum Question {
    Secret(u32),
    Text(u64, String),
}

#[derive(Default)]
pub(crate) struct Tracker {
    active: Option<Question>,
    answered: Option<Question>,
}

impl Tracker {
    /// Returns whether the active question changed. Echo-off questions use the
    /// child identity alone, so output fragments cannot create password copies
    /// or repeated notifications. Repeated questions after a response get a new
    /// event, even when their wording is identical.
    pub fn observe(&mut self, snapshot: &Snapshot) -> bool {
        let next = snapshot
            .child
            .filter(|_| snapshot.phase == Phase::Running)
            .and_then(|pid| {
                if password(snapshot) {
                    Some(Question::Secret(pid))
                } else if snapshot.secret {
                    None
                } else {
                    snapshot
                        .output
                        .iter()
                        .rev()
                        .map(|s| s.trim_end())
                        .find(|s| !s.is_empty())
                        .filter(|s| yes_or_no(s))
                        .map(|s| Question::Text(position(snapshot), s.to_owned()))
                }
            });
        let answered = match (&self.answered, &next) {
            (Some(Question::Text(at, text)), Some(Question::Text(next_at, next_text))) => {
                at == next_at && next_text.starts_with(text)
            }
            (Some(answered), Some(next)) => answered == next,
            _ => false,
        };
        if !answered {
            self.answered = None;
        }
        let next = if answered { None } else { next };
        let changed = self.active != next;
        self.active = next;
        changed
    }

    pub fn waiting(&self) -> bool {
        self.active.is_some()
    }

    pub fn answered(&mut self) {
        self.answered = self.active.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_lifecycle_deduplicates_output_and_allows_later_questions() {
        let mut tracker = Tracker::default();
        let mut snapshot = Snapshot {
            phase: Phase::Running,
            child: Some(42),
            output: vec!["Proceed? [Y/n]".into()],
            ..Snapshot::default()
        };
        assert!(tracker.observe(&snapshot));
        assert!(tracker.waiting());
        assert!(!tracker.observe(&snapshot));
        tracker.answered();
        snapshot.output[0].push('y');
        assert!(!tracker.observe(&snapshot));
        assert!(!tracker.waiting());
        // The tool can ask the identical question again in the same output
        // chunk as the echoed answer, with no intermediate snapshot.
        snapshot.output.push("Proceed? [Y/n]".into());
        assert!(tracker.observe(&snapshot));
        snapshot.secret = true;
        snapshot.output.push("Password: ".into());
        assert!(tracker.observe(&snapshot));
        assert!(!tracker.observe(&snapshot));
        tracker.answered();
        assert!(!tracker.observe(&snapshot));
        snapshot.secret = false;
        tracker.observe(&snapshot);
        snapshot.secret = true;
        assert!(tracker.observe(&snapshot));
        snapshot.phase = Phase::Completed;
        assert!(tracker.observe(&snapshot));
        assert!(!tracker.waiting());
    }

    #[test]
    fn echo_off_progress_is_not_a_password_request() {
        let mut tracker = Tracker::default();
        let mut snapshot = Snapshot {
            phase: Phase::Running,
            child: Some(42),
            secret: true,
            ..Snapshot::default()
        };
        for line in [
            "",
            "Looking for updates?",
            "Updating 1/2… 66%",
            "Checking password database…",
            ":: Running post-transaction hooks...",
        ] {
            snapshot.output = vec![line.into()];
            assert!(!password(&snapshot), "{line}");
            tracker.observe(&snapshot);
            assert!(!tracker.waiting(), "{line}");
        }
        for line in [
            "Password: ",
            "[sudo] password for someone:",
            "Enter passphrase for key '/key':",
            "Enter PIN:",
        ] {
            snapshot.output = vec![line.into()];
            assert!(password(&snapshot), "{line}");
        }
        snapshot.secret = false;
        assert!(!password(&snapshot));
    }
}
