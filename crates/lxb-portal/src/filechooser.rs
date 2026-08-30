//! `org.freedesktop.impl.portal.FileChooser`: the D-Bus side of choosing a
//! file.
//!
//! An application never asks this directly. It asks `xdg-desktop-portal`, the
//! session-wide front desk every desktop shares — usually without knowing it,
//! because GTK's and Qt's own file dialogs quietly become portal calls when
//! there is a portal to call — and that hands the question down to whichever
//! backend the desktop installed. This is the backend half, and the whole of
//! what it does is turn one D-Bus call into one question on the user's screen
//! and the answer back into a list of URIs.
//!
//! ## The three calls
//!
//! `OpenFile` is "give me something that is already there": one file, several,
//! or a folder, depending on the options. `SaveFile` is "somewhere to write,
//! and what to call it". `SaveFiles` is the same for a list of names the
//! application has already decided on — it asks only for the folder, and works
//! out the paths itself.
//!
//! Every one of them answers with a response code first: nought for a file
//! chosen, one for the user cancelling, two for anything that went wrong. An
//! application that is cancelled is told so and carries on; that is the whole
//! point of the portal being between them.
//!
//! ## Nothing here draws
//!
//! The question is drawn by the session shell and reaches it over
//! `lxb_shell_v1` — see [`crate::pick`], which is the whole of the road. What
//! is left here is the two translations either end of it: the portal's options
//! into a question, and the user's answer into URIs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

use crate::pick;

/// Response codes, which every portal call answers with first.
const OK: u32 = 0;
const CANCELLED: u32 = 1;
const FAILED: u32 = 2;

/// The interface version implemented.
///
/// Three is the one that added `SaveFiles`; four added the `directory` option
/// to `OpenFile`, which is how an application asks for a folder rather than a
/// file. Both are here. What came after is nothing at all — four is current.
const VERSION: u32 = 4;

/// How long an unanswered question is left standing.
///
/// Much longer than a screen-share question's, because it is a different kind
/// of wait: that one is a yes or a no somebody reads in a second, and this one
/// is a person walking their own disk looking for something they last saved in
/// 2019. It is a backstop and not a policy — a shell that goes away
/// mid-question is answered at once by the compositor, and a user who closes
/// the panel is answered on the press — so the only thing it actually bounds is
/// an application that asked into a session nobody is sitting at.
const PATIENCE: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// The portal itself. It holds nothing: every file question is answered whole
/// inside the call that asked it, so there is no conversation to keep.
pub struct FileChooser;

#[zbus::interface(name = "org.freedesktop.impl.portal.FileChooser")]
impl FileChooser {
    /// Choose something that is already on the disk.
    async fn open_file(
        &self,
        _handle: OwnedObjectPath,
        app_id: String,
        _parent_window: String,
        title: String,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        let directory = flag(&options, "directory");
        let purpose = match (directory, flag(&options, "multiple")) {
            (true, _) => pick::For::AFolder,
            (false, true) => pick::For::ManyFiles,
            (false, false) => pick::For::OneFile,
        };
        let kinds = kinds_of(&options);
        let wanted = pick::Wanted {
            app_id,
            purpose,
            title,
            accept: text(&options, "accept_label"),
            name: String::new(),
            at: opening_folder(&options),
            // The kinds are about files. A folder chooser that offered
            // "Images" would be narrowing a list it is not showing.
            kinds: if directory { Vec::new() } else { kinds.clone() },
        };
        let (response, chosen) = put_the_question(wanted).await;
        if response != OK {
            return (response, HashMap::new());
        }
        let mut results = HashMap::new();
        results.insert("uris".to_string(), uris(&chosen.files));
        if let Some(filter) = chosen.kind.and_then(|index| filter_at(&kinds, index)) {
            results.insert("current_filter".to_string(), filter);
        }
        (OK, results)
    }

    /// Choose somewhere to write, and what to call it.
    async fn save_file(
        &self,
        _handle: OwnedObjectPath,
        app_id: String,
        _parent_window: String,
        title: String,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        let kinds = kinds_of(&options);
        // `current_file` is the whole path of something being saved over, and
        // it answers both halves of the question at once: which folder, and
        // what the thing is called. It is read first for that reason — an
        // application that named a file meant that file, and `current_name`
        // beside it is only ever the same name again.
        let existing = bytes(&options, "current_file").map(PathBuf::from);
        let name = match existing.as_ref().and_then(|path| path.file_name()) {
            Some(name) => name.to_string_lossy().into_owned(),
            None => text(&options, "current_name"),
        };
        let at = match existing.as_ref().and_then(|path| path.parent()) {
            Some(folder) if folder.is_dir() => folder.to_string_lossy().into_owned(),
            _ => opening_folder(&options),
        };
        let wanted = pick::Wanted {
            app_id,
            purpose: pick::For::ANewFile,
            title,
            accept: text(&options, "accept_label"),
            name,
            at,
            kinds: kinds.clone(),
        };
        let (response, chosen) = put_the_question(wanted).await;
        if response != OK {
            return (response, HashMap::new());
        }
        let mut results = HashMap::new();
        results.insert("uris".to_string(), uris(&chosen.files));
        if let Some(filter) = chosen.kind.and_then(|index| filter_at(&kinds, index)) {
            results.insert("current_filter".to_string(), filter);
        }
        (OK, results)
    }

    /// Choose somewhere to write a list of files the application has already
    /// named.
    ///
    /// The user picks a folder and nothing else; the names come from the
    /// application, and the paths are worked out here. Nothing is created and
    /// nothing is checked — a name already taken in that folder is the
    /// application's to notice when it writes, which is the only moment the
    /// answer is still true.
    async fn save_files(
        &self,
        _handle: OwnedObjectPath,
        app_id: String,
        _parent_window: String,
        title: String,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        let names = names_of(&options);
        if names.is_empty() {
            tracing::warn!("an application asked where to save nothing at all");
            return (FAILED, HashMap::new());
        }
        let wanted = pick::Wanted {
            app_id,
            purpose: pick::For::AFolder,
            title,
            accept: text(&options, "accept_label"),
            name: String::new(),
            at: opening_folder(&options),
            kinds: Vec::new(),
        };
        let (response, chosen) = put_the_question(wanted).await;
        if response != OK {
            return (response, HashMap::new());
        }
        let Some(folder) = chosen.files.first().map(PathBuf::from) else {
            return (CANCELLED, HashMap::new());
        };
        // Only the last part of each name is used. An application asking to
        // write `../../.bashrc` into the folder somebody chose is asking to
        // write somewhere they did not choose, and the whole of what this call
        // hands over is one folder.
        let files: Vec<String> = names
            .iter()
            .filter_map(|name| Path::new(name).file_name())
            .map(|name| folder.join(name).to_string_lossy().into_owned())
            .collect();
        if files.len() != names.len() {
            tracing::warn!(
                asked = names.len(),
                kept = files.len(),
                "some of the names an application gave were not names of files"
            );
        }
        let mut results = HashMap::new();
        results.insert("uris".to_string(), uris(&files));
        (OK, results)
    }

    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        VERSION
    }
}

/// Ask the shell, off the thread that serves D-Bus.
///
/// The asking is minutes long and every bit of it is blocking — a panel drawn
/// by another process, answered by a hand — so it is done elsewhere. Doing it
/// on this thread would silence the whole portal while somebody browsed: zbus
/// serves these objects on one executor, and a file question parked on it would
/// take screen sharing and every other call down with it. That defect has been
/// had once already; see [`crate::screencast::ScreenCast::start`].
async fn put_the_question(wanted: pick::Wanted) -> (u32, pick::Chosen) {
    let named = if wanted.app_id.trim().is_empty() {
        "an application".to_string()
    } else {
        wanted.app_id.clone()
    };
    tracing::info!(application = %named, purpose = ?wanted.purpose, "asking for a file");

    let answer = blocking::unblock(move || pick::ask(&wanted, PATIENCE)).await;
    let chosen = match answer {
        Ok(chosen) => chosen,
        Err(err) => {
            // Nobody to ask is not a file. It is reported as a failure rather
            // than as a cancellation, because the user did not cancel anything
            // — nothing was ever put in front of them.
            tracing::warn!(?err, application = %named, "could not ask");
            return (FAILED, pick::Chosen::default());
        }
    };
    if chosen.files.is_empty() {
        tracing::info!(application = %named, "the user chose nothing");
        return (CANCELLED, pick::Chosen::default());
    }
    tracing::info!(application = %named, files = chosen.files.len(), "the user chose");
    (OK, chosen)
}

// -- reading what the application asked for ---------------------------------

/// One boolean option, false where it is absent or is not one.
fn flag(options: &HashMap<String, OwnedValue>, key: &str) -> bool {
    options
        .get(key)
        .and_then(|value| bool::try_from(value).ok())
        .unwrap_or(false)
}

/// One string option, empty where it is absent or is not one.
fn text(options: &HashMap<String, OwnedValue>, key: &str) -> String {
    options
        .get(key)
        .and_then(|value| String::try_from(value.clone()).ok())
        .unwrap_or_default()
}

/// One `ay` option as a path: a byte string, NUL-terminated, which is how the
/// portal carries a filename that need not be UTF-8.
///
/// `None` rather than an empty string for an absent one, because the two are
/// different answers — "the application said nothing" and "the application
/// said the empty path" — and only the first has a sensible fallback.
fn bytes(options: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    let raw = Vec::<u8>::try_from(options.get(key)?.clone()).ok()?;
    let raw = raw.strip_suffix(&[0]).unwrap_or(&raw);
    if raw.is_empty() {
        return None;
    }
    // Lossy, and that is the honest limit of this road: the answer travels back
    // as a URI, which is a string, so a name that is not UTF-8 could not be
    // returned even if it could be read.
    Some(String::from_utf8_lossy(raw).into_owned())
}

/// The folder the question should open in, or empty for the shell's own choice.
///
/// Refused unless it is a directory that is there: an application does not get
/// to decide what the user is looking at, and one naming a folder that has been
/// deleted would otherwise open the panel on nothing.
fn opening_folder(options: &HashMap<String, OwnedValue>) -> String {
    match bytes(options, "current_folder") {
        Some(folder) if Path::new(&folder).is_dir() => folder,
        _ => String::new(),
    }
}

/// The names `SaveFiles` was given: an `aay`, each a NUL-terminated byte
/// string.
fn names_of(options: &HashMap<String, OwnedValue>) -> Vec<String> {
    let Some(value) = options.get("files") else {
        return Vec::new();
    };
    let Ok(list) = Vec::<Vec<u8>>::try_from(value.clone()) else {
        return Vec::new();
    };
    list.into_iter()
        .map(|raw| {
            let raw = raw.strip_suffix(&[0]).unwrap_or(&raw);
            String::from_utf8_lossy(raw).into_owned()
        })
        .filter(|name| !name.is_empty())
        .collect()
}

/// The kinds of file an application will accept, flattened to one pattern each.
///
/// The portal's `filters` is `a(sa(us))` — a list of named kinds, each holding
/// a list of patterns, each of which is either a shell glob or a media type.
/// The protocol to the shell carries one pattern per request, so the nesting is
/// taken out here and put back by whoever draws the rows.
///
/// `current_filter` is honoured by putting that kind first, which is what the
/// protocol says the shell opens on. An application that named a current filter
/// not in its own list is taken at its word and gets it anyway: it is a kind the
/// application will accept, which is the only thing the list means.
fn kinds_of(options: &HashMap<String, OwnedValue>) -> Vec<pick::Kind> {
    let mut named = filters_in(options.get("filters"));
    if let Some(current) = filters_in(options.get("current_filter")).into_iter().next() {
        named.retain(|kind| kind.0 != current.0);
        named.insert(0, current);
    }
    named
        .into_iter()
        .flat_map(|(name, patterns)| {
            patterns.into_iter().map(move |(mime, pattern)| pick::Kind {
                name: name.clone(),
                pattern,
                mime,
            })
        })
        .collect()
}

/// One `a(sa(us))` or one `(sa(us))`, read as a list of named kinds.
///
/// Walked field by field rather than deserialised into a tuple, because what
/// arrives is whatever the application sent: a filter with the wrong shape in
/// it is one filter to leave out, not a reason to fail the whole call and put
/// nothing on screen.
#[allow(clippy::type_complexity)]
fn filters_in(value: Option<&OwnedValue>) -> Vec<(String, Vec<(bool, String)>)> {
    let Some(value) = value else {
        return Vec::new();
    };
    let one = |value: &Value<'_>| -> Option<(String, Vec<(bool, String)>)> {
        let Value::Structure(kind) = value else {
            return None;
        };
        let fields = kind.fields();
        let Some(Value::Str(name)) = fields.first() else {
            return None;
        };
        let Some(Value::Array(patterns)) = fields.get(1) else {
            return None;
        };
        let patterns = patterns
            .iter()
            .filter_map(|pattern| {
                let Value::Structure(pattern) = pattern else {
                    return None;
                };
                let fields = pattern.fields();
                let (Some(Value::U32(how)), Some(Value::Str(text))) =
                    (fields.first(), fields.get(1))
                else {
                    return None;
                };
                // One is a media type and everything else is a glob, which is
                // the safe way round: a pattern read as a name is matched
                // against the name and matches nothing, where a name read as a
                // media type would claim every file of that type.
                Some((*how == 1, text.to_string()))
            })
            .collect::<Vec<_>>();
        (!patterns.is_empty()).then(|| (name.to_string(), patterns))
    };
    match Value::from(value.clone()) {
        Value::Array(kinds) => kinds.iter().filter_map(one).collect(),
        single => one(&single).into_iter().collect(),
    }
}

/// Which kind the user was looking at, put back into the portal's own
/// `(sa(us))` shape.
///
/// `index` numbers the *names* in the order they were offered, which is how the
/// shell counts them — a kind is one row of the panel however many patterns are
/// behind it, and the flattening this module does on the way out is undone
/// here on the way back.
fn filter_at(kinds: &[pick::Kind], index: usize) -> Option<OwnedValue> {
    let mut names: Vec<&str> = Vec::new();
    for kind in kinds {
        if !names.contains(&kind.name.as_str()) {
            names.push(&kind.name);
        }
    }
    let name = *names.get(index)?;
    let patterns: Vec<(u32, String)> = kinds
        .iter()
        .filter(|kind| kind.name == name)
        .map(|kind| (u32::from(kind.mime), kind.pattern.clone()))
        .collect();
    OwnedValue::try_from(Value::from((name.to_string(), patterns))).ok()
}

// -- saying what the user chose ---------------------------------------------

/// The chosen paths, as the `as` of `file://` URIs the portal answers with.
fn uris(files: &[String]) -> OwnedValue {
    let list: Vec<String> = files.iter().map(|path| uri(path)).collect();
    OwnedValue::try_from(Value::from(list)).expect("a list of strings")
}

/// One absolute path as a `file://` URI.
///
/// Everything outside the unreserved set is escaped, `/` excepted — it is what
/// separates the parts of the path and is the one reserved character that has
/// to survive. Written out here rather than pulled from a crate because it is
/// nine lines and the alternative is a dependency for nine lines.
fn uri(path: &str) -> String {
    let mut out = String::from("file://");
    for byte in path.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Answer file questions until the session ends, alongside screen sharing.
///
/// Not a `serve` of its own: a backend is one bus name and one object path, and
/// this hangs a second interface off the object [`crate::screencast::serve`]
/// already puts there.
pub fn serve_at<'a>(
    builder: zbus::connection::Builder<'a>,
    path: &ObjectPath<'a>,
) -> zbus::Result<zbus::connection::Builder<'a>> {
    builder.serve_at(path, FileChooser)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_becomes_a_uri_with_the_slashes_left_alone() {
        assert_eq!(uri("/home/me/a b.txt"), "file:///home/me/a%20b.txt");
        assert_eq!(uri("/tmp/plain.png"), "file:///tmp/plain.png");
        // The one character that must not be escaped, and one that must.
        assert_eq!(uri("/a/#b"), "file:///a/%23b");
    }

    #[test]
    fn the_kind_a_user_was_looking_at_is_put_back_together() {
        let kinds = vec![
            pick::Kind {
                name: "Images".into(),
                pattern: "*.png".into(),
                mime: false,
            },
            pick::Kind {
                name: "Images".into(),
                pattern: "image/jpeg".into(),
                mime: true,
            },
            pick::Kind {
                name: "Films".into(),
                pattern: "*.mkv".into(),
                mime: false,
            },
        ];
        // Two names behind three patterns, so the second kind is Films and
        // there is no third.
        assert!(filter_at(&kinds, 0).is_some());
        assert!(filter_at(&kinds, 1).is_some());
        assert!(filter_at(&kinds, 2).is_none());
    }
}
