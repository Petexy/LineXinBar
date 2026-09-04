//! `org.freedesktop.impl.portal.ScreenCast`: the D-Bus side of screen sharing.
//!
//! An application never asks this directly. It asks `xdg-desktop-portal`, the
//! session-wide front desk every desktop shares, and that hands the question
//! down to whichever backend the desktop installed. This is the backend half,
//! and the whole of what it does is decide *which* screen and then start one
//! [`crate::cast`] of it — the frames themselves never go over D-Bus at all,
//! only the number of the PipeWire node they will arrive on.
//!
//! ## The three calls, in the order they arrive
//!
//! `CreateSession` opens a conversation and gets an object to hang it on.
//! `SelectSources` says what kind of thing the application wants and whether
//! it wants the pointer in the picture. `Start` is where a desktop asks the
//! user, and answers with the streams the application may read.
//!
//! Every one of them answers with a response code first: nought for yes, one
//! for the user saying no, two for anything that went wrong. An application
//! that is refused is told so and carries on; that is the whole point of the
//! portal being between them.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use zbus::object_server::SignalEmitter;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

use crate::cast;

/// What kinds of thing this portal can hand over. A whole display, and nothing
/// else: there is no per-window sharing here, because a window belongs to an
/// application and the compositor hands out screens.
const SOURCE_MONITOR: u32 = 1;

/// Cursor modes, as the portal numbers them: hidden, drawn into the picture,
/// or sent alongside it as metadata. The last is not offered — this session's
/// cursor is drawn by the compositor, and taking it out to send separately
/// would be work done for a consumer that has to composite it back.
const CURSOR_HIDDEN: u32 = 1;
const CURSOR_EMBEDDED: u32 = 2;

/// The interface version implemented. Two is the one that added cursor modes;
/// what came after it is restore tokens, which this does not keep — every share
/// is asked for afresh.
const VERSION: u32 = 2;

/// Response codes, which every portal call answers with first.
const OK: u32 = 0;
const REFUSED: u32 = 1;
const FAILED: u32 = 2;

/// One conversation with one application.
struct Share {
    /// Whether the pointer goes in the picture, from `SelectSources`.
    cursor: bool,
    /// The cast, once it is running: the way to stop it, and the thread it is
    /// running on.
    stop: Option<pipewire::channel::Sender<()>>,
    running: Option<std::thread::JoinHandle<()>>,
}

impl Share {
    /// End the cast, if there is one, and wait for its thread to notice.
    fn end(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.running.take() {
            let _ = thread.join();
        }
    }
}

type Shares = Arc<Mutex<HashMap<OwnedObjectPath, Share>>>;

/// The portal itself: one object, and a note of every conversation in flight.
pub struct ScreenCast {
    shares: Shares,
}

#[zbus::interface(name = "org.freedesktop.impl.portal.ScreenCast")]
impl ScreenCast {
    /// Open a conversation. Nothing is decided here and nothing is shared;
    /// what comes back is a session object the two later calls hang off.
    async fn create_session(
        &self,
        _handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        app_id: String,
        _options: HashMap<String, OwnedValue>,
        #[zbus(object_server)] server: &zbus::ObjectServer,
    ) -> (u32, HashMap<String, OwnedValue>) {
        tracing::info!(%app_id, session = %session_handle, "an application wants to see the screen");
        self.shares.lock().unwrap().insert(
            session_handle.clone(),
            Share {
                cursor: false,
                stop: None,
                running: None,
            },
        );
        let session = Session {
            path: session_handle.clone(),
            shares: self.shares.clone(),
        };
        if let Err(err) = server.at(&session_handle, session).await {
            tracing::warn!(?err, "could not open a session");
            return (FAILED, HashMap::new());
        }
        (OK, HashMap::new())
    }

    /// What the application would like: a screen, and whether the pointer is in
    /// it. Which screen is not decided here — that is `Start`, because that is
    /// where the user is asked.
    async fn select_sources(
        &self,
        _handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        app_id: String,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        let cursor = options
            .get("cursor_mode")
            .and_then(|mode| u32::try_from(mode).ok())
            .unwrap_or(CURSOR_HIDDEN);
        let types = options
            .get("types")
            .and_then(|types| u32::try_from(types).ok())
            .unwrap_or(SOURCE_MONITOR);
        tracing::info!(%app_id, cursor, types, "an application said what it wants");

        if types & SOURCE_MONITOR == 0 {
            // It asked only for windows, which this cannot hand over.
            tracing::info!(%app_id, "refusing: only whole displays can be shared");
            return (FAILED, HashMap::new());
        }
        let mut shares = self.shares.lock().unwrap();
        let Some(share) = shares.get_mut(&session_handle) else {
            return (FAILED, HashMap::new());
        };
        share.cursor = cursor & CURSOR_EMBEDDED != 0;
        (OK, HashMap::new())
    }

    /// Start sharing, and say on which PipeWire node.
    ///
    /// This is where the user is asked. Nothing about the screen has been read
    /// before this point, and if the answer is no, nothing ever is.
    ///
    /// The asking is minutes long and every bit of it is blocking — a question
    /// drawn by another process, answered by a hand — so it is done *off* this
    /// thread. It used to be done on it, and that made one unanswered question
    /// enough to silence the whole portal: zbus serves this object on one
    /// executor, so a `Start` parked on the panel took `CreateSession` and
    /// every other call down with it, and the next application to ask got
    /// nothing at all. Nothing on screen said why.
    async fn start(
        &self,
        _handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        app_id: String,
        _parent_window: String,
        _options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        let cursor = {
            let shares = self.shares.lock().unwrap();
            match shares.get(&session_handle) {
                Some(share) => share.cursor,
                None => return (FAILED, HashMap::new()),
            }
        };

        let asked = app_id.clone();
        let started = blocking::unblock(move || ask_and_cast(&asked, cursor)).await;
        let (chosen, live, stop, running) = match started {
            Ok(started) => started,
            Err(outcome) => return (outcome, HashMap::new()),
        };
        tracing::info!(%app_id, display = %chosen, node = live.node, "sharing a screen");

        {
            let mut shares = self.shares.lock().unwrap();
            match shares.get_mut(&session_handle) {
                Some(share) => {
                    share.stop = Some(stop);
                    share.running = Some(running);
                }
                None => {
                    // The session was closed while the cast was starting.
                    let _ = stop.send(());
                    return (FAILED, HashMap::new());
                }
            }
        }

        let mut stream: HashMap<String, OwnedValue> = HashMap::new();
        stream.insert(
            "size".to_string(),
            OwnedValue::try_from(Value::from((live.width as i32, live.height as i32)))
                .expect("a pair of numbers"),
        );
        stream.insert(
            "position".to_string(),
            OwnedValue::try_from(Value::from((0i32, 0i32))).expect("a pair of numbers"),
        );
        stream.insert(
            "source_type".to_string(),
            OwnedValue::try_from(Value::from(SOURCE_MONITOR)).expect("a number"),
        );

        let mut results: HashMap<String, OwnedValue> = HashMap::new();
        results.insert(
            "streams".to_string(),
            OwnedValue::try_from(Value::from(vec![(live.node, stream)])).expect("one stream"),
        );
        (OK, results)
    }

    /// A whole display, and nothing smaller. Per-window sharing would mean
    /// handing over one application's surfaces, which is a different request to
    /// a different half of the compositor.
    #[zbus(property)]
    fn available_source_types(&self) -> u32 {
        SOURCE_MONITOR
    }

    /// The pointer can be left out or drawn in. It cannot be sent separately:
    /// this session's cursor is the compositor's own drawing, and pulling it
    /// back out to ship as coordinates would be work done so that a consumer
    /// could paint it again.
    #[zbus(property)]
    fn available_cursor_modes(&self) -> u32 {
        CURSOR_HIDDEN | CURSOR_EMBEDDED
    }

    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        VERSION
    }
}

/// One open conversation, as an object the front desk can close.
struct Session {
    path: OwnedObjectPath,
    shares: Shares,
}

#[zbus::interface(name = "org.freedesktop.impl.portal.Session")]
impl Session {
    /// The application went away, or stopped sharing. Either way the cast ends
    /// here: a screen that goes on being read after the thing reading it has
    /// gone is the failure this whole arrangement exists to prevent.
    async fn close(
        &self,
        #[zbus(object_server)] server: &zbus::ObjectServer,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) {
        tracing::info!(session = %self.path, "the share was closed");
        if let Some(mut share) = self.shares.lock().unwrap().remove(&self.path) {
            share.end();
        }
        let _ = Session::closed(&emitter).await;
        let _ = server.remove::<Session, _>(&self.path).await;
    }

    #[zbus(signal)]
    async fn closed(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        VERSION
    }
}

/// Everything one `Start` does that blocks: ask the user, and get a cast as
/// far as the PipeWire node it will arrive on.
///
/// Split out so it can be handed to a thread whole — see [`ScreenCast::start`],
/// which must not hold the portal's own executor while a question is on screen.
/// The error is the response code to answer with, because that is all the
/// caller can do with any of these: a refusal is the user's answer and
/// everything else is a failure they never saw.
type Started = (
    String,
    cast::Live,
    pipewire::channel::Sender<()>,
    std::thread::JoinHandle<()>,
);

fn ask_and_cast(app_id: &str, cursor: bool) -> Result<Started, u32> {
    let displays = match cast::outputs() {
        Ok(displays) if !displays.is_empty() => displays,
        Ok(_) => {
            tracing::warn!("this session has no displays to share");
            return Err(FAILED);
        }
        Err(err) => {
            tracing::warn!(?err, "cannot see this session's displays");
            return Err(FAILED);
        }
    };

    let Some(chosen) = crate::consent::ask(app_id, &displays) else {
        tracing::info!(%app_id, "the user refused");
        return Err(REFUSED);
    };

    let (stop, receiver) = pipewire::channel::channel::<()>();
    let (announce, live) = std::sync::mpsc::channel::<cast::Live>();
    let wanted = cast::Wanted {
        output: Some(chosen.clone()),
        cursor,
    };
    let running = std::thread::Builder::new()
        .name("lxb-cast".to_string())
        .spawn(move || {
            if let Err(err) = cast::run(
                wanted,
                move |live| {
                    let _ = announce.send(live);
                },
                receiver,
            ) {
                tracing::warn!(?err, "the cast ended badly");
            }
        });
    let running = match running {
        Ok(running) => running,
        Err(err) => {
            tracing::warn!(?err, "could not start a cast");
            return Err(FAILED);
        }
    };

    // The node id is the whole answer, and it does not exist until PipeWire has
    // made the node. A cast that cannot get that far in a few seconds is one
    // that is not going to.
    match live.recv_timeout(std::time::Duration::from_secs(10)) {
        Ok(live) => Ok((chosen, live, stop, running)),
        Err(_) => {
            tracing::warn!(%app_id, "the cast never started");
            let _ = stop.send(());
            Err(FAILED)
        }
    }
}

/// Say so when nothing will ever call this portal.
///
/// Listening is not the same as being *findable*. `xdg-desktop-portal` chooses
/// its backends by reading `.portal` files out of the data directories, once,
/// when it starts; a session whose portal was never registered has this process
/// sitting on the bus with the right name, answering nothing, while every
/// application that asks for a screen is told there is no ScreenCast portal at
/// all. Nothing in that is an error anybody sees — it looks like an application
/// that cannot capture screens, which is how it was reported.
///
/// So the one thing this can check for itself is checked, and said plainly. It
/// is a warning rather than a refusal to start: the file may be somewhere this
/// does not know to look, and a portal that refused to run over its own guess
/// would be worse than one that is merely unreachable.
fn warn_if_unregistered() {
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    if let Some(home) = std::env::var_os("XDG_DATA_HOME").filter(|home| !home.is_empty()) {
        dirs.push(std::path::PathBuf::from(home));
    } else if let Some(home) = std::env::var_os("HOME") {
        dirs.push(std::path::Path::new(&home).join(".local/share"));
    }
    let system = std::env::var("XDG_DATA_DIRS")
        .unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string());
    dirs.extend(
        system
            .split(':')
            .filter(|dir| !dir.is_empty())
            .map(std::path::PathBuf::from),
    );

    let found = dirs
        .iter()
        .map(|dir| dir.join("xdg-desktop-portal/portals/lxb.portal"))
        .find(|path| path.is_file());
    let Some(path) = found else {
        tracing::warn!(
            "no lxb.portal in any data directory: xdg-desktop-portal cannot know this \
             backend exists, so nothing will be offered to any application. Install \
             the package, or run scripts/install-portal.sh from the checkout"
        );
        return;
    };
    tracing::debug!(registration = %path.display(), "registered with xdg-desktop-portal");

    // And that the registration found is *this* build's. It is a separate file
    // on disk from the binary, installed at a different time, and
    // xdg-desktop-portal reads it once at startup — so a backend that has grown
    // an interface since the last install answers a front desk that has never
    // heard of it. Nothing says so: the application is quietly handed whichever
    // other backend the machine has, which is exactly what a session with no
    // portal of its own looks like.
    //
    // This is not hypothetical. The file chooser shipped against a registration
    // naming ScreenCast alone, and every Save dialog in the session opened
    // GTK's instead.
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let listed = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("Interfaces="))
        .unwrap_or_default();
    let missing: Vec<&str> = ANSWERED
        .iter()
        .copied()
        .filter(|interface| !listed.split(';').any(|named| named.trim() == *interface))
        .collect();
    if !missing.is_empty() {
        tracing::warn!(
            registration = %path.display(),
            ?missing,
            "this registration does not name every interface this build answers, so \
             xdg-desktop-portal will hand those questions to another backend. \
             Reinstall the package, or run scripts/install-portal.sh from the checkout, \
             and restart xdg-desktop-portal"
        );
    }
}

/// What this backend answers, as `xdg-desktop-portal` spells it.
///
/// Here rather than only in the `.portal` file so the two can be compared: a
/// build and its registration are installed separately and drift apart
/// silently. See [`warn_if_unregistered`].
const ANSWERED: [&str; 2] = [
    "org.freedesktop.impl.portal.ScreenCast",
    "org.freedesktop.impl.portal.FileChooser",
];

/// The path every desktop portal backend answers on.
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";

/// The name this backend is known by. It has to match the `DBusName` in the
/// `.portal` file, which is how `xdg-desktop-portal` finds it at all.
pub const PORTAL_NAME: &str = "org.freedesktop.impl.portal.desktop.lxb";

/// Answer portal calls until the session ends.
///
/// And no longer than that. The name this claims is the *user's* bus name, not
/// the session's — one bus serves every session a user logs into — so a portal
/// that outlived its compositor would go on answering for a session that no
/// longer exists, and the screen it was asked for could not be handed over by
/// anybody. It waits on its own Wayland connection for exactly that reason:
/// see [`cast::wait_for_the_session_to_end`], which is the only thing this
/// process holds open while it is idle.
pub async fn serve() -> anyhow::Result<()> {
    let shares: Shares = Arc::new(Mutex::new(HashMap::new()));
    let path = ObjectPath::try_from(PORTAL_PATH)?;
    let connection = zbus::connection::Builder::session()?
        // Take the name even if something already holds it, and let the next
        // one take it from this. The compositor starts this process and knows
        // its pid, which is what lets it ask the shell anything at all; the
        // bus can start one too, from the activation file, and that one has no
        // way to the shell. Whichever gets there first, the session's own copy
        // is the one that ends up answering — and when the compositor starts a
        // replacement, that one takes over in turn.
        .replace_existing_names(true)
        .allow_name_replacements(true)
        .name(PORTAL_NAME)?
        .serve_at(
            &path,
            ScreenCast {
                shares: shares.clone(),
            },
        )?;
    // The second interface on the same object, because a portal backend is one
    // bus name and one path however many questions it answers. See
    // [`crate::filechooser`].
    let connection = crate::filechooser::serve_at(connection, &path)?
        .build()
        .await?;
    tracing::info!(name = PORTAL_NAME, "the portal is listening");
    warn_if_unregistered();

    // Blocking, on this thread, exactly where the wait for nothing at all used
    // to be: zbus drives the connection on an executor of its own, so what
    // happens here is that this thread parks — as it did before — but on
    // something that can end.
    if let Err(err) = cast::wait_for_the_session_to_end() {
        tracing::warn!(%err, "this portal has no session to belong to");
    }

    // Dropped rather than left to the process exiting, so the name is given up
    // before this returns and the next session's portal can take it cleanly.
    drop(connection);
    Ok(())
}
