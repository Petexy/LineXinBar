//! Being this session's file manager: `org.freedesktop.FileManager1`.
//!
//! "Show in folder" is not something an application does by itself. Firefox
//! finishing a download, an archiver that has just unpacked something, a chat
//! client that has saved a picture — none of them opens a file manager. They
//! call one method on one well-known bus name, and whatever holds that name is
//! the desktop's file manager for as long as it holds it:
//!
//! ```text
//! org.freedesktop.FileManager1.ShowItems(["file:///home/…/thing.zip"], "")
//! ```
//!
//! On a machine with a normal desktop installed, that name is *activatable* —
//! Dolphin, Nautilus and Thunar each ship a D-Bus service file claiming it — so
//! a LineXinBar session that answers nothing does not get silence. It gets
//! Dolphin, started on demand, drawn over the shell, in a session that has no
//! window management for it and no way back. Which is the bug this module
//! exists to fix, and it is fixed by being there first: a name already owned is
//! never activated.
//!
//! ## Why the shell and not a program beside it
//!
//! The same answer [`crate::notify`] and [`crate::polkit`] give. There is no
//! file manager to start here — Files is a *column of the bar*, four rows into
//! System, drawn by the shell out of the shell's own tree (see
//! [`crate::files`]). Nothing else in this session can put it on a screen, and
//! nothing else knows which display the user is driving or what is in front of
//! it. A separate process holding this name would have to ask the shell to do
//! every part of the work anyway.
//!
//! ## What is honoured
//!
//! The interface is not in a specification anybody publishes; it is what
//! Nautilus defined and everything else copied. Three methods, all of them
//! `(as uris, s startup_id)`:
//!
//! * `ShowItems` — stand in the folder holding each item, with the item
//!   itself under the cursor. The one that matters: it is what "Show in
//!   folder" is, and what `xdg-desktop-portal` calls for `OpenDirectory` when
//!   a sandboxed application asks the same question.
//! * `ShowFolders` — stand *in* the folders themselves.
//! * `ShowItemProperties` — see [`Listener::show_item_properties`], which is
//!   the one place this shell answers with something other than what the name
//!   says, and says why.
//!
//! `startup_id` is taken and dropped. It is a startup-notification token for
//! raising an X11 window that is about to map, and nothing here maps a window:
//! the bar is already on this display, on a layer the compositor owns.
//!
//! Several URIs in one call come down to the first of them. The shell has one
//! cursor per display and one row can be under it — see [`Asked`].
//!
//! ## Everything that is not a file on this machine is a refusal
//!
//! A URI that is not `file://`, a path that is not absolute, a name that is
//! not on this disk: each is answered on the bus as an error, immediately, on
//! the thread the call arrived on. Nothing reaches the shell but a folder that
//! exists, so the frame that picks one up never has a reason to refuse it for
//! being malformed.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The name every "Show in folder" on this machine goes looking for.
const BUS_NAME: &str = "org.freedesktop.FileManager1";

/// And the object on it, which is fixed by the same convention.
const OBJECT_PATH: &str = "/org/freedesktop/FileManager1";

/// One thing the rest of the machine has asked this shell to show.
///
/// Resolved here rather than in the shell: whether a path names a folder or a
/// file in one is a question about the disk, it is asked once on the bus
/// thread, and what the shell is handed is already the two things it needs —
/// the column to open, and the row to leave under the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asked {
    /// The folder to stand in. Always a directory that existed when the call
    /// arrived.
    pub folder: PathBuf,
    /// What to leave under the cursor once the column is open, where the
    /// question named something in it rather than the folder itself.
    pub item: Option<PathBuf>,
}

impl Asked {
    /// What `ShowItems` means for one path: the folder holding it, with it
    /// under the cursor.
    ///
    /// A directory is shown this way too, and deliberately — `ShowItems` on a
    /// folder means *that folder, pointed at*, which is how a browser reveals a
    /// download that was an unpacked directory. Standing inside it instead
    /// would be answering `ShowFolders`.
    fn holding(path: &Path) -> Option<Self> {
        match path.parent() {
            Some(folder) => Some(Self {
                folder: folder.to_path_buf(),
                item: Some(path.to_path_buf()),
            }),
            // `/` is the one path with nothing holding it. There is no column
            // above it to point at it from, so it is shown as itself.
            None => Self::standing_in(path),
        }
    }

    /// And what `ShowFolders` means: the folder itself, stood in.
    ///
    /// A path that is not a directory cannot be stood in, so it is revealed
    /// instead of refused. That is not a substitution: it is the same folder
    /// the caller named, opened one row short of where it asked, which is the
    /// closest thing to the question that exists.
    fn standing_in(path: &Path) -> Option<Self> {
        if !path.is_dir() {
            return Self::holding(path);
        }
        Some(Self {
            folder: path.to_path_buf(),
            item: None,
        })
    }
}

/// What the bus thread and the shell share.
#[derive(Default)]
struct Shared {
    asked: Mutex<Vec<Asked>>,
}

/// The name, the object on it, and the connection that answers.
///
/// Held by the shell for as long as the session lasts. Dropping it drops the
/// name, which is how the rest of the machine learns at once that this session
/// no longer has a file manager — and, on a machine that has one installed,
/// how the next "Show in folder" goes back to activating it.
pub struct Service {
    shared: Arc<Shared>,
    /// Kept only to hold the name. Nothing here signals or calls out; the
    /// connection exists so that dropping the service releases the name at a
    /// moment the shell decides rather than whenever the last reference to
    /// some inner object happens to go.
    _connection: zbus::blocking::Connection,
}

impl Service {
    /// Take `org.freedesktop.FileManager1` on the session bus and start
    /// answering it.
    ///
    /// `None` when this session cannot have it — no session bus, or another
    /// file manager already holds the name — after saying which. Not an error,
    /// on the same terms the notification daemon is not: a LineXinBar started
    /// inside somebody else's desktop for testing must not take the name off
    /// the file manager that desktop is using.
    pub fn start() -> Option<Service> {
        let shared = Arc::new(Shared::default());
        let listener = Listener {
            shared: Arc::clone(&shared),
        };

        // On this thread, so a failure is reported once and plainly at
        // startup. A file manager that failed quietly would leave a session
        // where every "Show in folder" opens somebody else's.
        let connection = match connect(listener) {
            Ok(connection) => connection,
            Err(err) => {
                tracing::info!(%err, "not this session's file manager: could not take the bus name");
                return None;
            }
        };
        tracing::info!("serving {BUS_NAME} for this session");

        Some(Service {
            shared,
            _connection: connection,
        })
    }

    /// Everything the bus has asked for since this was last drained.
    ///
    /// The bargain the notification daemon strikes, for the reason it strikes
    /// it: the bus thread never touches the shell's state, it leaves questions
    /// here, and the frame that was going to be drawn anyway picks them up.
    pub fn drain(&self) -> Vec<Asked> {
        match self.shared.asked.lock() {
            Ok(mut asked) => std::mem::take(&mut *asked),
            // A panic on the bus thread must not take the shell down with it.
            // What is lost is one press of Show in folder, and not a session.
            Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
        }
    }
}

/// Open the session bus with the object on it, and take the name.
///
/// `ReplaceExisting` is deliberately not asked for, exactly as it is not for
/// the notification daemon: if something else on this session is already the
/// file manager, it keeps the name and this call fails.
fn connect(listener: Listener) -> zbus::Result<zbus::blocking::Connection> {
    zbus::blocking::connection::Builder::session()?
        .name(BUS_NAME)?
        .serve_at(OBJECT_PATH, listener)?
        .build()
}

/// The object on the bus. Every method here is called from zbus's own threads.
struct Listener {
    shared: Arc<Shared>,
}

#[zbus::interface(name = "org.freedesktop.FileManager1")]
impl Listener {
    /// Show these items, each in the folder that holds it.
    ///
    /// The method behind "Show in folder", "Open Containing Folder" and
    /// "Reveal in file manager", whichever application they are pressed in —
    /// and behind `org.freedesktop.portal.OpenURI.OpenDirectory`, which is how
    /// the same press reaches here from a sandboxed one.
    fn show_items(&self, uris: Vec<String>, _startup_id: String) -> zbus::fdo::Result<()> {
        self.take(uris, Asked::holding)
    }

    /// Stand in these folders.
    fn show_folders(&self, uris: Vec<String>, _startup_id: String) -> zbus::fdo::Result<()> {
        self.take(uris, Asked::standing_in)
    }

    /// Show what is known about these items.
    ///
    /// This shell has no properties window, and the honest answer to that is
    /// not an error: the row a file gets in a Files column already carries how
    /// big it is and when it was last written — see [`crate::files::Item`] —
    /// so what a properties window would be opened to read is what the walk
    /// puts under the cursor. It is answered as [`Self::show_items`] is, and it
    /// is the one method here that does something other than what its name
    /// says.
    fn show_item_properties(
        &self,
        uris: Vec<String>,
        _startup_id: String,
    ) -> zbus::fdo::Result<()> {
        tracing::debug!("asked for a file's properties; showing the file instead");
        self.take(uris, Asked::holding)
    }
}

impl Listener {
    /// Turn a call's URIs into one question for the shell, or refuse it here.
    ///
    /// Refused on this thread and not the shell's, so that a caller hears why
    /// on the call it made rather than getting a silent success and a bar that
    /// never moves.
    fn take(&self, uris: Vec<String>, how: fn(&Path) -> Option<Asked>) -> zbus::fdo::Result<()> {
        // Only the first. The shell has one cursor per display, one row is
        // under it, and a list of files does not name one place to stand — the
        // rest of a multi-file call is the caller saying more than this
        // desktop can show. Said out loud rather than silently dropped.
        let Some(first) = uris.first() else {
            return Err(zbus::fdo::Error::InvalidArgs("no file was named".into()));
        };
        if uris.len() > 1 {
            tracing::info!(
                named = uris.len(),
                "showing the first of the files a caller named"
            );
        }

        let Some(path) = path_of(first) else {
            return Err(zbus::fdo::Error::InvalidArgs(
                crate::message!("reveal-not-a-file", "path" => first),
            ));
        };
        // Before it is handed on, because a path that is not there is the one
        // failure the caller can do something about — and because a walk that
        // ended in a folder with nothing under the cursor would look like the
        // shell having lost the file rather than like the file being gone.
        //
        // Asked of the name and not of what it points at, unlike everything
        // else here: a symbolic link with nothing on the other end is still a
        // row in the folder it is in — the explorer lists it, and pointing at
        // it is exactly how somebody would find out that it is broken. See
        // [`crate::files::listing`], which reads the link's own facts for the
        // row and follows it only to decide whether it leads to a folder.
        if std::fs::symlink_metadata(&path).is_err() {
            return Err(zbus::fdo::Error::FileNotFound(
                crate::message!("reveal-not-on-this-machine", "path" => path.display().to_string()),
            ));
        }
        let Some(asked) = how(&path) else {
            return Err(zbus::fdo::Error::InvalidArgs(
                crate::message!("reveal-cannot-be-shown", "path" => path.display().to_string()),
            ));
        };

        tracing::info!(
            folder = %asked.folder.display(),
            item = ?asked.item.as_deref().map(Path::display),
            "asked to show a file"
        );
        match self.shared.asked.lock() {
            Ok(mut waiting) => waiting.push(asked),
            Err(poisoned) => poisoned.into_inner().push(asked),
        }
        Ok(())
    }
}

/// Ask this session's file manager to show something, from a process that is
/// not the shell.
///
/// The other end of `--show-in-files`, and the whole of what that flag does.
/// It is how the desktop entry registered for `inode/directory` reaches the
/// running shell: `xdg-open` on a folder starts *a program*, the program it
/// starts is this binary, and what this binary does about it is make the call
/// a browser would have made itself.
///
/// [`Asked::standing_in`] is asked for rather than [`Asked::holding`], because
/// what a folder handler is given is a folder and what somebody wants from it
/// is to be *in* there. A file named instead is revealed, which is that
/// method's own fallback.
///
/// The name is checked for an owner before it is called, and this is the one
/// reason to check: the bus would otherwise *activate* whatever service file
/// on the machine claims the name, so running this outside a LineXinBar
/// session would silently start somebody else's file manager under this
/// desktop entry's name.
pub fn ask_the_session(named: &str) -> anyhow::Result<()> {
    // A path relative to wherever this was run from, which is not something a
    // desktop entry ever sends and is exactly what somebody typing it will.
    let named = match path_of(named) {
        Some(path) => path,
        None if !named.contains("://") => std::env::current_dir()?.join(named),
        None => anyhow::bail!("{named} does not name a file on this machine"),
    };
    let named = named.to_str().ok_or_else(|| {
        // The bus carries strings. A file whose name is not UTF-8 can be shown
        // — see `path_of` — but not named on a command line that has to become
        // one, and saying so is better than sending something else.
        anyhow::anyhow!("{} cannot be named on the bus", named.display())
    })?;

    let connection = zbus::blocking::Connection::session()?;
    let bus = zbus::blocking::fdo::DBusProxy::new(&connection)?;
    if !bus.name_has_owner(BUS_NAME.try_into()?)? {
        anyhow::bail!("no session is answering {BUS_NAME}, so there is nothing to show this in");
    }
    connection.call_method(
        Some(BUS_NAME),
        OBJECT_PATH,
        Some(BUS_NAME),
        "ShowFolders",
        // No startup token: nothing here maps a window. See this module's own
        // head, where the argument is explained away once for all three calls.
        &(vec![named.to_string()], ""),
    )?;
    Ok(())
}

/// The path a `file://` URI stands for, or nothing.
///
/// Nothing for every other scheme, and that is the whole of the security here:
/// this shell is being asked to *walk to* whatever comes back, so an `http://`
/// or an `smb://` that were let through would be a bar pointed at a name it
/// cannot read and a folder that does not exist.
///
/// A bare absolute path is taken as itself. It is not a URI and the interface
/// asks for URIs, but callers pass one often enough — and it is what somebody
/// running the handler by hand will type — that reading it is worth more than
/// being right about it.
pub fn path_of(uri: &str) -> Option<PathBuf> {
    let uri = uri.trim();
    if uri.starts_with('/') {
        return Some(PathBuf::from(uri));
    }
    let rest = uri.strip_prefix("file://")?;
    // `file:///path` is the empty authority, and `file://localhost/path` says
    // the same thing the long way round. Any other host names a file on
    // another machine, which this could not open in any case.
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return None;
    }
    // Bytes and not a string, deliberately. A file name on Linux is bytes, a
    // URI escapes each of them, and decoding into a `String` first would refuse
    // every file on this disk whose name is not UTF-8 — which the file explorer
    // itself lists and this would then be unable to point at.
    let path = PathBuf::from(OsString::from_vec(unescaped(rest)?));
    path.is_absolute().then_some(path)
}

/// Undo the percent-escaping a URI carries, as bytes.
///
/// `None` for an escape that is not one — a bare `%`, or two characters after
/// it that are not hexadecimal. A malformed URI is refused rather than
/// half-read: the alternative is a path with a literal `%` in it standing for a
/// file the caller never named.
fn unescaped(raw: &str) -> Option<Vec<u8>> {
    let raw = raw.as_bytes();
    let mut out = Vec::with_capacity(raw.len());
    let mut at = 0;
    while at < raw.len() {
        match raw[at] {
            b'%' => {
                let digits = std::str::from_utf8(raw.get(at + 1..at + 3)?).ok()?;
                out.push(u8::from_str_radix(digits, 16).ok()?);
                at += 3;
            }
            byte => {
                out.push(byte);
                at += 1;
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_uri_is_the_path_it_stands_for() {
        assert_eq!(
            path_of("file:///home/somebody/Downloads/thing.zip"),
            Some(PathBuf::from("/home/somebody/Downloads/thing.zip"))
        );
    }

    /// The case that made this take bytes rather than a string: what a browser
    /// sends for a download whose name has a space in it.
    #[test]
    fn the_escapes_are_undone() {
        assert_eq!(
            path_of("file:///home/somebody/My%20Files/a%2Bb%23c.txt"),
            Some(PathBuf::from("/home/somebody/My Files/a+b#c.txt"))
        );
    }

    #[test]
    fn a_name_that_is_not_utf8_survives() {
        // `%FF` is not a character in any encoding this shell reads, and is a
        // perfectly ordinary byte in a file name.
        let path = path_of("file:///tmp/caf%FF").expect("a path");
        assert_eq!(path.as_os_str().as_encoded_bytes(), b"/tmp/caf\xff");
    }

    #[test]
    fn localhost_is_this_machine() {
        assert_eq!(
            path_of("file://localhost/etc/hostname"),
            Some(PathBuf::from("/etc/hostname"))
        );
    }

    #[test]
    fn a_bare_path_is_read_as_itself() {
        assert_eq!(
            path_of("  /etc/hostname "),
            Some(PathBuf::from("/etc/hostname"))
        );
    }

    /// Everything that is not a file on this machine, refused before the shell
    /// ever hears about it.
    #[test]
    fn nothing_else_is_a_path() {
        for uri in [
            "https://example.invalid/thing.zip",
            "smb://server/share/thing",
            "file://server/share/thing",
            "trash:///",
            "file://relative",
            "thing.zip",
            "",
            // A malformed escape, which is refused rather than half-read.
            "file:///tmp/%zz",
            "file:///tmp/%",
        ] {
            assert_eq!(path_of(uri), None, "{uri} should not name a file");
        }
    }

    #[test]
    fn an_item_is_shown_in_the_folder_that_holds_it() {
        let asked = Asked::holding(Path::new("/home/somebody/Downloads/thing.zip")).expect("asked");
        assert_eq!(asked.folder, PathBuf::from("/home/somebody/Downloads"));
        assert_eq!(
            asked.item,
            Some(PathBuf::from("/home/somebody/Downloads/thing.zip"))
        );
    }

    /// The one path with nothing holding it.
    #[test]
    fn the_root_is_shown_as_itself() {
        let asked = Asked::holding(Path::new("/")).expect("asked");
        assert_eq!(asked.folder, PathBuf::from("/"));
        assert_eq!(asked.item, None);
    }

    #[test]
    fn a_folder_asked_for_by_itself_is_stood_in() {
        let asked = Asked::standing_in(Path::new("/")).expect("asked");
        assert_eq!(asked.folder, PathBuf::from("/"));
        assert_eq!(asked.item, None);
    }

    /// A file cannot be stood in, so it is revealed instead.
    ///
    /// Named somewhere that cannot exist, so the answer is the rule and not
    /// this machine's disk — see the other tests in this workspace that had to
    /// learn the same thing.
    #[test]
    fn a_file_asked_for_as_a_folder_is_revealed() {
        let asked = Asked::standing_in(Path::new("/nowhere-on-this-disk/thing")).expect("asked");
        assert_eq!(asked.folder, PathBuf::from("/nowhere-on-this-disk"));
        assert_eq!(
            asked.item,
            Some(PathBuf::from("/nowhere-on-this-disk/thing"))
        );
    }
}
