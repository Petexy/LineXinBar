//! The session's notification daemon, and what it collects.
//!
//! Two halves, and they are deliberately in one file because neither is worth
//! anything without the other:
//!
//! * [`Service`] is `org.freedesktop.Notifications` on the session bus — the
//!   interface every program on a Linux desktop announces things through, from
//!   a download finishing to a battery running out. It runs on a thread of its
//!   own, exactly as the polkit agent does and for the same reason: a bus call
//!   must be answered whether or not the shell is drawing a frame.
//! * [`Center`] is what the shell keeps: the announcements that have arrived,
//!   newest first, and the ones still to be shown in the corner of the screen.
//!
//! ## Why the shell and not a program beside it
//!
//! The same reason the polkit agent is here (see [`crate::polkit`]): a console
//! has no tray, no panel and no second process to put a bubble on top of a
//! fullscreen game. A notification has to be drawn *over* whatever owns the
//! screen, and the only thing in this session that can draw over an
//! application is the shell — it already holds an overlay layer for the guide
//! and already knows which display the user is driving.
//!
//! It is also the only way the two presentations can agree. The bubble in the
//! corner and the list behind the bell are the same object seen twice, and a
//! daemon outside the shell would mean two copies of every announcement and
//! two ideas about which of them had been read.
//!
//! ## What is honoured and what is not
//!
//! The specification is old and permissive, and most of it is about a desktop
//! this shell is not. What is taken:
//!
//! * `summary` and `body`, which are the announcement.
//! * `app_name`, which is what a row says when the announcement itself said
//!   nothing.
//! * The picture, from whichever of the four places the sender put one, in
//!   the order the specification prefers them: the `image-data` hint, the
//!   `image-path` hint, `app_icon`, and the `desktop-entry` hint — see
//!   [`Notification::icon_name`]. The more specific to *this* announcement a
//!   source is, the earlier it comes.
//!
//!   Three of the four are a name or a path: looked up in the icon theme or
//!   decoded off the disk, and either may be something no application on the
//!   machine uses, which is why the atlas keeps cells back for them. The
//!   fourth is raw pixels — see [`Image`] — which is how album art and a
//!   correspondent's face arrive, neither of which has a name anywhere.
//! * `replaces_id`, which is how a download's progress is one row that changes
//!   rather than forty rows.
//! * `actions`, offered on the row the user opens.
//! * The `urgency` hint, which decides only whether the bubble is shown at all
//!   for the lowest of them.
//! * `transient`, which says the announcement is not worth keeping once it has
//!   been seen.
//!
//! What is deliberately not taken is `expire_timeout`. Every program picks its
//! own, they range from half a second to forever, and the result on a normal
//! desktop is a corner where things appear and vanish at unrelated speeds. The
//! bubble here is on the shell's clock — see [`DWELL`] — so that a person
//! glancing up from a game learns *once* how long they have to read one. What
//! the program asked for is not lost: it decides how long the announcement is
//! worth showing, and this shell's answer to that question is the same for all
//! of them.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use zbus::zvariant::OwnedValue;

/// Where the daemon lives on the session bus. Both are fixed by the
/// specification: a program looking for a notification server looks here.
const BUS_NAME: &str = "org.freedesktop.Notifications";
const OBJECT_PATH: &str = "/org/freedesktop/Notifications";

/// What the shell tells a program about itself when asked.
///
/// The spec version is what this implements, not what it is; `1.2` is the
/// current one and is what the capability list below is drawn from.
const SPEC_VERSION: &str = "1.2";

/// How long a bubble stays once it has finished arriving, in seconds.
///
/// The clock starts when the entrance animation *ends*, not when the
/// announcement does. Those are different by the length of the slide, and
/// starting the count at the beginning would mean a bubble that is legible for
/// less time than it took to appear — which is the failure mode of every
/// notification corner that measures from the wrong end.
pub const DWELL: f32 = 4.0;

/// How long the bubble takes to come in from beyond the right-hand edge, and
/// how long it takes to leave, in seconds.
///
/// It leaves faster than it arrives. Arriving is the shell asking to be
/// noticed and is worth the time; leaving is the shell getting out of the way,
/// and a slow exit is a distraction that lasts after the thing worth reading
/// has been read.
pub const FLY_IN: f32 = 0.34;
pub const FLY_OUT: f32 = 0.22;

/// How urgent the program said it was.
///
/// Three levels because the specification has three. Only the ends do
/// anything: the lowest is filed without a bubble, because a program that
/// marked something *low* has said it is not worth interrupting anyone over,
/// and the highest is the one thing here that is allowed to sit on the screen
/// through a fullscreen game without asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Urgency {
    Low,
    #[default]
    Normal,
    Critical,
}

impl Urgency {
    fn from_hint(value: Option<&OwnedValue>) -> Urgency {
        // The hint is a byte in the spec, and is sent as one by everything
        // that sends it at all — but zvariant will not widen a `u8` into a
        // `u32` for us, and a program that sent the wrong width should get its
        // urgency read rather than an error. Try the width the spec names
        // first, then the two it does not.
        let level = value.and_then(|value| {
            u8::try_from(value)
                .ok()
                .or_else(|| u32::try_from(value).ok().and_then(|n| u8::try_from(n).ok()))
                .or_else(|| i32::try_from(value).ok().and_then(|n| u8::try_from(n).ok()))
        });
        match level {
            Some(0) => Urgency::Low,
            Some(2) => Urgency::Critical,
            _ => Urgency::Normal,
        }
    }
}

/// The largest picture that will be taken out of a hint, per side.
///
/// A sender describes its own picture, and the shell believes the description
/// far enough to walk the block of bytes behind it. Four thousand a side is
/// wider than any screen this draws on and sixty-four megabytes of samples,
/// which is a generous ceiling for something that ends up 128 pixels square —
/// and it is a ceiling, which is the point. Without one, a program with a
/// wrong number in its header asks the shell to allocate whatever that number
/// says.
const MAX_IMAGE_EDGE: u32 = 4096;

/// A picture a program sent as pixels rather than naming one.
///
/// The `image-data` hint, which is how album art, a correspondent's face, or a
/// screenshot of what just finished arrive — none of which has a name in any
/// icon theme, so there is nothing to look up and the bytes are the whole of
/// the picture.
///
/// Kept as the sender described it and turned into something drawable later,
/// by [`crate::icons::Icon::from_pixels`]. Scaling pixels is not the bus's
/// business, and this module deliberately owns no drawing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    /// The distance from one row of `data` to the next, which is not always
    /// the width: a sender may pad its rows out to a convenient boundary.
    pub stride: u32,
    /// Three or four. Three is a picture with no transparency in it.
    pub channels: u32,
    pub data: Vec<u8>,
}

impl Image {
    /// Take one out of the hint a program sent, if what it sent was one.
    ///
    /// The wire form is `(iiibiiay)`: width, height, rowstride, whether there
    /// is an alpha channel, how many bits each sample is, how many channels,
    /// and the samples. Every one of those is checked against the others
    /// rather than trusted, because they arrive from any program on the
    /// session bus and they describe a walk over memory.
    fn from_hint(value: &OwnedValue) -> Option<Self> {
        let fields = zbus::zvariant::Structure::try_from(value.try_clone().ok()?).ok()?;
        let fields = fields.fields();
        if fields.len() < 7 {
            return None;
        }
        let number = |index: usize| -> Option<u32> {
            u32::try_from(i32::try_from(fields.get(index)?.try_clone().ok()?).ok()?).ok()
        };

        let width = number(0)?;
        let height = number(1)?;
        let stride = number(2)?;
        let has_alpha = bool::try_from(fields.get(3)?.try_clone().ok()?).ok()?;
        let bits = number(4)?;
        let channels = number(5)?;
        let data = Vec::<u8>::try_from(fields.get(6)?.try_clone().ok()?).ok()?;

        // Eight bits a sample is the only width the specification defines and
        // the only one anything sends. A picture at some other depth is one
        // this cannot read, and reading it as though it were eight would draw
        // noise rather than refuse.
        if bits != 8 || channels != if has_alpha { 4 } else { 3 } {
            return None;
        }
        if width == 0 || height == 0 || width > MAX_IMAGE_EDGE || height > MAX_IMAGE_EDGE {
            return None;
        }
        // The rest of the arithmetic is checked where the walk happens, which
        // is the one place that must agree with the buffer it is walking.
        Some(Image {
            width,
            height,
            stride,
            channels,
            data,
        })
    }

    /// A name for this picture, since it has none of its own.
    ///
    /// The atlas is keyed by name, and raw pixels are exactly the thing with
    /// no name — so one is made out of what the picture *is*. Two consequences
    /// and both are wanted: a program that replaces an announcement with new
    /// art gets a new key and therefore a new cell rather than keeping the old
    /// picture, and two announcements carrying the same art share one cell.
    fn key(&self) -> String {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.width.hash(&mut hasher);
        self.height.hash(&mut hasher);
        self.data.hash(&mut hasher);
        // The `lxb:` prefix every built-in glyph wears, so a hash can never
        // collide with a theme name — see [`crate::icons`].
        format!("lxb:announced-image:{:016x}", hasher.finish())
    }
}

/// The picture a program named, with a `file://` URI turned back into the path
/// it stands for.
///
/// Only that one scheme, and only when it names no host. Anything else is
/// handed on untouched: a name that merely contains a colon is still a name,
/// and an icon at `http://` is not something a shell should be fetching
/// because an announcement asked it to.
fn unwrap_file_uri(value: String) -> String {
    let Some(rest) = value.strip_prefix("file://") else {
        return value;
    };
    // `file:///path` — the empty authority. `file://host/path` names a file on
    // another machine, which this could not open in any case.
    match rest.strip_prefix('/') {
        Some(path) => format!("/{path}"),
        None => value,
    }
}

/// One thing a program has announced.
#[derive(Debug, Clone)]
pub struct Notification {
    /// The number the bus gave it, which is how the program that sent it and
    /// the shell that is showing it talk about the same announcement.
    pub id: u32,
    /// The program's name for itself, as it sent it.
    pub app: String,
    /// The three places a program may put the picture it wants drawn at the
    /// head of the row, in the order they are preferred — see
    /// [`Notification::icon_name`], which is the only thing that reads them.
    ///
    /// Each is a theme name or a path, and which of the two is not decided
    /// here: telling them apart, searching the theme and decoding the file is
    /// the shell's business rather than the bus's, and it depends on what is
    /// installed on the machine. A `file://` URI is unwrapped on the way in,
    /// though, because that *is* the bus's business — a URI is how the
    /// specification says to write a path in the `image-path` hint, and the
    /// rest of the shell should never have to know that.
    app_icon: String,
    image_path: Option<String>,
    desktop_entry: Option<String>,
    /// A picture sent as pixels, which beats all three of those — see
    /// [`Image`] — and the name made for it, kept beside it so that the one
    /// accessor everything else uses can answer with a name whatever the
    /// sender chose.
    image: Option<Image>,
    image_key: Option<String>,
    /// The one line the announcement is about.
    pub summary: String,
    /// Everything else it had to say, which is often nothing.
    pub body: String,
    /// The buttons the program offered, as `(key, label)` in the order it gave
    /// them. The key is what goes back over the bus; the label is what the row
    /// says.
    pub actions: Vec<(String, String)>,
    pub urgency: Urgency,
    /// Whether the program said this is not worth keeping. A transient
    /// announcement is shown and then forgotten rather than filed — it is what
    /// a volume popup or a "copied" confirmation marks itself as, and a list
    /// full of those is a list nobody reads.
    pub transient: bool,
    /// When it arrived, for the age on the row.
    ///
    /// Not for the order of the list: that is the order the list is *kept* in
    /// — see [`Center`] — and sorting by this instead would be a second
    /// opinion about which announcement is newest.
    pub arrived: Instant,
}

impl Notification {
    /// A plain announcement, for a test somewhere else in the crate that needs
    /// one to build a row out of.
    ///
    /// Here rather than written out where it is wanted because two of these
    /// fields are private — what the program called its icon is nobody's
    /// business but [`Notification::icon_name`]'s — and a fixture is not a
    /// reason to open them up.
    #[cfg(test)]
    pub fn heard(id: u32, summary: &str) -> Self {
        Self {
            id,
            app: "Test".to_string(),
            app_icon: String::new(),
            image_path: None,
            image: None,
            image_key: None,
            desktop_entry: None,
            summary: summary.to_string(),
            body: String::new(),
            actions: Vec::new(),
            urgency: Urgency::Normal,
            transient: false,
            arrived: Instant::now(),
        }
    }

    /// The picture this announcement should wear: a theme name or a path,
    /// whichever of the three places a program can put one it used.
    ///
    /// In the specification's order, which is the opposite of what this used to
    /// do. The `image-path` hint comes first, then `app_icon`, and the
    /// desktop entry last.
    ///
    /// The old order put the desktop entry first, reasoning that it is the
    /// identifier of an *installation* — the same string the launcher filed
    /// the program under — so the row in the panel would match the tile on the
    /// start screen. That is a good thing for it to fall back to and the wrong
    /// thing to prefer: it makes every announcement from one program look the
    /// same, which is exactly what a program is overriding when it names an
    /// icon. `notify-send --icon=software-update-available` is a user asking
    /// for that picture and no other, and a shell that answered with the
    /// sender's own icon would be ignoring the only instruction it was given.
    ///
    /// So the general rule: the more specific to *this announcement* a source
    /// is, the earlier it comes. A hint chosen per announcement beats the
    /// program's own icon, which beats knowing merely which program it was.
    pub fn icon_name(&self) -> Option<&str> {
        [
            self.image_key.as_deref(),
            self.image_path.as_deref(),
            Some(self.app_icon.as_str()),
            self.desktop_entry.as_deref(),
        ]
        .into_iter()
        .flatten()
        .find(|name| !name.is_empty())
    }

    /// The pixels behind [`Self::icon_name`], when the name it gave is one
    /// made up for a picture that arrived as bytes.
    ///
    /// `None` for every announcement that named something, which is nearly all
    /// of them — those are looked up in the icon theme instead.
    pub fn image(&self) -> Option<&Image> {
        self.image.as_ref()
    }

    /// What the row says when the program sent nothing to say.
    ///
    /// An announcement with an empty summary is not a bug worth dropping it
    /// over — it is a program that put everything in the body, and the row
    /// still has to be readable. The body's first line stands in, and failing
    /// that the program's own name: a row that says only where it came from is
    /// still a row that can be dismissed.
    pub fn title(&self) -> &str {
        if !self.summary.is_empty() {
            return &self.summary;
        }
        match self.body.lines().find(|line| !line.trim().is_empty()) {
            Some(line) => line,
            None if !self.app.is_empty() => &self.app,
            None => crate::i18n::text("shell-notification"),
        }
    }
}

/// Why an announcement was taken off the list, in the numbers the
/// specification uses for the `NotificationClosed` signal.
///
/// The program that sent it is told which of these happened, and the
/// difference matters to it: a chat client that hears *dismissed* knows the
/// user saw the message, and one that hears *expired* knows they may not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Closed {
    /// It was only ever a bubble and its time ran out.
    Expired = 1,
    /// The user put it away.
    Dismissed = 2,
    /// The program that sent it asked for it back.
    ByProgram = 3,
}

/// What one pass over the bus turned up, for the frame that made it.
///
/// Two answers rather than one because they drive different things: anything
/// at all means the screen has to be redrawn, and a bubble going up is the
/// only one of them that makes a noise. Withdrawing an announcement changes
/// the list and must not chime; a program replacing one it already sent
/// changes a bubble already on screen and must not either.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Arrivals {
    /// Whether the list or the corner changed at all.
    pub changed: bool,
    /// Whether a bubble went up that was not there before.
    pub raised: bool,
}

/// Something the bus has said, waiting for the shell to pick it up.
#[derive(Debug)]
enum Event {
    Arrived(Box<Notification>),
    /// A program withdrew one it had sent — a download that was cancelled, a
    /// message read on another device.
    Withdrawn(u32),
}

/// What the bus thread and the shell share.
#[derive(Default)]
struct Shared {
    events: Mutex<Vec<Event>>,
}

/// The daemon: the bus name, the object on it, and the thread that answers.
///
/// Held by the shell for as long as the session lasts. Dropping it drops the
/// connection, which is how every program on the machine learns at once that
/// there is no longer a notification server — the same way `polkitd` learns
/// the agent has gone.
pub struct Service {
    shared: Arc<Shared>,
    /// Kept so the shell can answer the programs it is showing: a signal when
    /// an announcement is closed, and another when one of its buttons is
    /// pressed. Without this the daemon would be able to hear and not to
    /// reply, which for half the programs that send notifications is the
    /// difference between a button that works and a button that is decoration.
    connection: zbus::blocking::Connection,
}

impl Service {
    /// Take `org.freedesktop.Notifications` on the session bus and start
    /// answering it.
    ///
    /// Returns `None` when this session cannot have one — no session bus, or
    /// another daemon already holds the name — after saying which. That is a
    /// session where the shell shows nothing rather than one that fails to
    /// start: a LineXinBar run inside somebody else's desktop for testing has
    /// their notification daemon already running, and taking the name off it
    /// would break the desktop the user is actually using.
    pub fn start() -> Option<Service> {
        let shared = Arc::new(Shared::default());
        let listener = Listener {
            shared: Arc::clone(&shared),
            next: AtomicU32::new(1),
        };

        // On this thread and not a worker, so a failure is reported once and
        // plainly at startup. A daemon that failed quietly would leave a
        // session that looks like it collects notifications and does not.
        let connection = match connect(listener) {
            Ok(connection) => connection,
            Err(err) => {
                tracing::info!(%err, "no notification daemon: could not take the bus name");
                return None;
            }
        };
        tracing::info!("serving {BUS_NAME} for this session");

        Some(Service { shared, connection })
    }

    /// Everything the bus has said since this was last asked.
    ///
    /// Drained rather than read, and by the frame that was going to be drawn
    /// anyway — the same bargain the volume worker and the media library
    /// strike. The bus thread never touches the shell's state; it leaves
    /// things here and the shell picks them up when it is between frames.
    fn drain(&self) -> Vec<Event> {
        match self.shared.events.lock() {
            Ok(mut events) => std::mem::take(&mut *events),
            // A panic on the bus thread must not take the shell down with it.
            // The events in flight are lost, which is one missed notification
            // and not a session.
            Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
        }
    }

    /// Tell the program that sent `id` that it is no longer on screen.
    ///
    /// Best-effort by design: the program may have exited since it announced
    /// something, which is not an error and is in fact the common case for a
    /// script that pops one up and ends.
    fn closed(&self, id: u32, why: Closed) {
        let _ = self.connection.emit_signal(
            None::<&str>,
            OBJECT_PATH,
            BUS_NAME,
            "NotificationClosed",
            &(id, why as u32),
        );
    }

    /// Tell it one of its buttons was pressed.
    fn invoked(&self, id: u32, action: &str) {
        let _ = self.connection.emit_signal(
            None::<&str>,
            OBJECT_PATH,
            BUS_NAME,
            "ActionInvoked",
            &(id, action),
        );
    }
}

/// Open the session bus with the daemon's object on it, and take the name.
///
/// `ReplaceExisting` is deliberately *not* asked for. If something else on
/// this session is already the notification server, it keeps the name and this
/// call fails — which is the answer that leaves the user's own desktop working
/// when the shell is being run inside it.
fn connect(listener: Listener) -> zbus::Result<zbus::blocking::Connection> {
    zbus::blocking::connection::Builder::session()?
        .name(BUS_NAME)?
        .serve_at(OBJECT_PATH, listener)?
        .build()
}

/// The object on the bus. Every method here is called from zbus's own threads.
struct Listener {
    shared: Arc<Shared>,
    /// The next announcement's number.
    ///
    /// Starts at 1 and never reaches 0, because 0 is reserved: it is what a
    /// program passes as `replaces_id` to mean *this is a new one*.
    next: AtomicU32,
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Listener {
    /// What this server can do, in the names the specification gives them.
    ///
    /// `persistence` is the honest one and the one worth listing: it says
    /// announcements are kept after their bubble has gone, which is exactly
    /// what the bell in the guide is. A program that sees it may reasonably
    /// stop drawing its own history.
    fn get_capabilities(&self) -> Vec<&'static str> {
        vec!["actions", "body", "icon-static", "persistence"]
    }

    /// Name, vendor, version, spec version — in that order, which is fixed.
    fn get_server_information(&self) -> (&'static str, &'static str, &'static str, &'static str) {
        (
            "LineXinBar",
            "LineXinBar",
            env!("CARGO_PKG_VERSION"),
            SPEC_VERSION,
        )
    }

    /// A program announcing something. The number it gets back is how it
    /// refers to this announcement afterwards.
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        _expire_timeout: i32,
    ) -> u32 {
        // Replacing keeps the number, which is the whole point of it: the
        // program goes on talking about one announcement while its contents
        // change under it.
        let id = if replaces_id != 0 {
            replaces_id
        } else {
            self.next.fetch_add(1, Ordering::Relaxed)
        };

        // Three spellings across three versions of the specification, and all
        // three are still sent. The newest first, so a program that sends more
        // than one is read as its newest self.
        let image = ["image-data", "image_data", "icon_data"]
            .into_iter()
            .find_map(|key| Image::from_hint(hints.get(key)?));

        let notification = Notification {
            id,
            app: app_name,
            app_icon: unwrap_file_uri(app_icon),
            // Both spellings, because both are in the wild: `image-path` is
            // what the specification has said since 1.2 and `image_path` is
            // what it said before that, and programs that have not been
            // touched in a decade still send the old one.
            image_path: hints
                .get("image-path")
                .or_else(|| hints.get("image_path"))
                .and_then(|value| String::try_from(value.try_clone().ok()?).ok())
                .map(unwrap_file_uri),
            desktop_entry: hints
                .get("desktop-entry")
                .and_then(|value| String::try_from(value.try_clone().ok()?).ok()),
            image_key: image.as_ref().map(Image::key),
            image,
            summary,
            body,
            // The list arrives flat — key, label, key, label — and an odd
            // trailing entry is a program that got it wrong. Pairing rather
            // than indexing means that one is dropped instead of taken as a
            // button whose label is its own key.
            actions: actions
                .chunks_exact(2)
                .map(|pair| (pair[0].clone(), pair[1].clone()))
                .collect(),
            urgency: Urgency::from_hint(hints.get("urgency")),
            transient: hints
                .get("transient")
                .and_then(|value| bool::try_from(value.try_clone().ok()?).ok())
                .unwrap_or(false),
            arrived: Instant::now(),
        };

        self.post(Event::Arrived(Box::new(notification)));
        id
    }

    /// A program taking one back.
    fn close_notification(&self, id: u32) {
        self.post(Event::Withdrawn(id));
    }

    /// The two signals the specification defines, declared so that zbus knows
    /// their shapes and puts them in the introspection data. They are emitted
    /// from the shell's side through [`Service`] rather than from here — what
    /// closed an announcement, and whether a button was pressed, is something
    /// only the screen knows.
    #[zbus(signal)]
    async fn notification_closed(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        id: u32,
        reason: u32,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn action_invoked(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        id: u32,
        action_key: &str,
    ) -> zbus::Result<()>;
}

impl Listener {
    fn post(&self, event: Event) {
        match self.shared.events.lock() {
            Ok(mut events) => events.push(event),
            Err(poisoned) => poisoned.into_inner().push(event),
        }
    }
}

/// A bubble on its way in, sitting, or on its way out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    In,
    Sitting,
    Out,
}

/// One announcement being shown in the corner of the screen.
///
/// Separate from the [`Notification`] it is about, and holding only its number,
/// because the two have different lifetimes in both directions: a bubble goes
/// after [`DWELL`] while its announcement stays on the list until somebody
/// puts it away, and a transient one is a bubble whose announcement was never
/// on the list at all.
#[derive(Debug, Clone)]
pub struct Toast {
    pub id: u32,
    /// A copy rather than a lookup, for the transient case above: the bubble
    /// has to go on being drawn for its own announcement after that
    /// announcement has been forgotten.
    pub about: Notification,
    /// When the current stage began.
    since: Instant,
    stage: Stage,
}

impl Toast {
    pub fn stage(&self) -> Stage {
        self.stage
    }

    /// How far through the current stage it is, 0 at the start and 1 at the
    /// end. Sitting is measured against [`DWELL`] like the other two are
    /// against their flights, so one number drives the whole life of a bubble.
    pub fn progress(&self) -> f32 {
        let over = match self.stage {
            Stage::In => FLY_IN,
            Stage::Sitting => DWELL,
            Stage::Out => FLY_OUT,
        };
        (self.since.elapsed().as_secs_f32() / over).clamp(0.0, 1.0)
    }

    /// Start it leaving, wherever it had got to.
    ///
    /// Used when the user puts the announcement away while its bubble is still
    /// up: the bubble must not outlive the row it is about, and it must not
    /// vanish either — see the shell's motion rules, where nothing disappears
    /// before its transition has ended.
    fn dismiss(&mut self) {
        if self.stage != Stage::Out {
            self.stage = Stage::Out;
            self.since = Instant::now();
        }
    }
}

/// Everything the shell keeps about what has been announced.
///
/// The list and the bubbles are one object because they are one thing seen
/// twice, and the several ways an announcement can leave — read in the panel,
/// timed out in the corner, withdrawn by the program that sent it — all have
/// to reach both presentations. Two structures would mean three chances for
/// them to disagree.
#[derive(Debug)]
pub struct Center {
    /// Newest first. The order the panel wants, and the order the corner wants
    /// too, so it is the order they are kept in rather than one either of them
    /// has to sort into.
    list: Vec<Notification>,
    /// Oldest first: the top of the stack in the corner is the bubble that has
    /// been there longest, and new ones arrive under it. The opposite of the
    /// list, and deliberately — a stack that reordered itself as things
    /// arrived would move the bubble somebody was part-way through reading.
    toasts: Vec<Toast>,
    /// Programs to be told, gathered here rather than sent from wherever the
    /// announcement was taken off the list: every path that removes one goes
    /// through this, so no route can forget to answer.
    outbox: Vec<(u32, Closed)>,
    /// Buttons the user pressed, on the same terms.
    invoked: Vec<(u32, String)>,
    /// Whether the guide's do-not-disturb switch is on, in which case nothing
    /// reaches the corner and nothing chimes.
    ///
    /// Held here, on the thing that decides what a bubble is, rather than
    /// tested wherever a bubble is drawn or a sound is asked for. The corner
    /// and the chime are one answer — see [`Arrivals::raised`] — so there is
    /// exactly one place that answer is given, and a switch enforced at that
    /// place cannot be enforced for the picture and forgotten for the noise.
    quiet: bool,
    /// What has been announced and not yet looked at, by id.
    ///
    /// A set rather than a count, because the badge has to go out when the last
    /// unread announcement is *taken off the list* as readily as when it is
    /// read — a bubble put away from the corner is one nobody will ever open
    /// the list to find. A bare flag could not tell that from a list that still
    /// holds something, and would leave a mark on the bell pointing at nothing.
    ///
    /// Every id in here is on [`Self::list`]: a transient announcement is never
    /// filed, so it could never be read from the list either, and one that
    /// counted as unread would light the bell for good.
    unseen: std::collections::HashSet<u32>,
    /// Where the badge was when the answer last changed, and when that was.
    ///
    /// The position and not just the moment, so a mark that is told to go out
    /// while it is still growing leaves from where it had got to rather than
    /// from the top. `None` is a badge that has never moved.
    badge_from: f32,
    badge_at: Option<Instant>,
    /// The number the shell's *own* next announcement takes.
    ///
    /// Counting **down** from the top, where every program on the machine
    /// counts up from one — see [`Listener::next`]. That is the whole of how
    /// the two are kept apart, and it is worth having rather than sharing one
    /// counter for two reasons. The daemon may not exist at all: another
    /// desktop's holds the bus name and this session has no [`Service`], and
    /// the shell must still be able to say something about the machine it is
    /// running. And an id that collided with a program's would be worse than
    /// untidy — [`Center::arrived`] treats a number it has seen as a
    /// *replacement*, so the shell would silently overwrite somebody's
    /// download.
    ///
    /// They would meet after four billion announcements between them, which is
    /// a session nobody has ever had.
    ours: u32,
}

impl Default for Center {
    fn default() -> Self {
        Self {
            list: Vec::new(),
            toasts: Vec::new(),
            outbox: Vec::new(),
            invoked: Vec::new(),
            quiet: false,
            unseen: std::collections::HashSet::new(),
            badge_from: 0.0,
            badge_at: None,
            ours: u32::MAX,
        }
    }
}

/// How long the unread mark takes to grow onto the bell, and to go out again,
/// in seconds.
///
/// Short. It is a small thing appearing in the corner of a tile, and the reason
/// it moves at all is that it must not be *found* already there: something
/// arriving while the guide is open should be seen to arrive. Going out is the
/// same length, because that half is watched too — the tile stands beside the
/// panel it opens rather than behind it, so the user is looking straight at the
/// mark as it clears.
const BADGE_FLIGHT: f32 = 0.2;

/// How many bubbles are shown at once.
///
/// Three, because the corner of the screen is not a list and a fourth would be
/// the shell competing with the application for the display. Anything beyond
/// it is still filed — it simply does not interrupt, which is what the bell is
/// for.
pub const STACK: usize = 3;

impl Center {
    /// One that starts wherever the settings file left the do-not-disturb
    /// switch, rather than one that has to be told a moment after the session
    /// is up — which would be a window, however short, in which the first
    /// thing the machine announced got through a switch the user had left on.
    pub fn new(quiet: bool) -> Self {
        Center {
            quiet,
            ..Center::default()
        }
    }

    pub fn list(&self) -> &[Notification] {
        &self.list
    }

    pub fn toasts(&self) -> &[Toast] {
        &self.toasts
    }

    /// Whether anything has been announced that nobody has looked at.
    ///
    /// What the bell wears a mark for. Deliberately *unread* rather than
    /// *anything on the list*: the list is a record and keeps what it is given
    /// until it is thrown away, so a mark that meant "not empty" would be lit
    /// for as long as the user left anything in it and would say nothing at all
    /// about whether they had missed something.
    pub fn unread(&self) -> bool {
        !self.unseen.is_empty()
    }

    /// How far that mark has arrived: 0 for a bell with nothing to say, 1 for
    /// one carrying the mark, and in between while it grows or goes out.
    pub fn badge(&self) -> f32 {
        let to = if self.unread() { 1.0 } else { 0.0 };
        let Some(at) = self.badge_at else {
            return to;
        };
        let flight = crate::ui::ease(at.elapsed().as_secs_f32() / BADGE_FLIGHT);
        self.badge_from + (to - self.badge_from) * flight
    }

    /// Whether it is still moving, which is what keeps the frames coming.
    fn badge_moving(&self) -> bool {
        self.badge_at
            .is_some_and(|at| at.elapsed().as_secs_f32() < BADGE_FLIGHT)
    }

    /// Say that everything on the list has now been looked at, because the
    /// panel listing it is up. Returns whether that changed anything.
    ///
    /// Opening the list is the only thing that counts as reading it. A bubble
    /// in the corner deliberately does not: it appears whether or not anybody
    /// is in the room, which is the whole reason there is a list behind the
    /// bell to catch what it said.
    pub fn mark_seen(&mut self) -> bool {
        if self.unseen.is_empty() {
            return false;
        }
        self.change_unseen(|unseen| unseen.clear());
        true
    }

    /// Change what has not been looked at, and start the badge on its way if
    /// that changed the answer.
    ///
    /// Every route that touches the set goes through here — something
    /// arriving, something being read, something leaving the list — so there is
    /// one place the mark's flight begins and no route can change what the bell
    /// says without the bell being told to move.
    fn change_unseen(&mut self, change: impl FnOnce(&mut std::collections::HashSet<u32>)) {
        let (was, from) = (self.unread(), self.badge());
        change(&mut self.unseen);
        if self.unread() != was {
            self.badge_from = from;
            self.badge_at = Some(Instant::now());
        }
    }

    /// Whether anything may interrupt.
    ///
    /// What the tile draws, and deliberately asked of the centre rather than of
    /// the settings file both of them are written to: the switch is drawn on by
    /// the very thing that enforces it, so a tile that says the session is
    /// quiet cannot be a session that is not.
    pub fn quiet(&self) -> bool {
        self.quiet
    }

    /// Say whether anything may interrupt.
    ///
    /// Turning it on sends whatever is standing in the corner on its way:
    /// somebody has just said they do not want to be interrupted, and a bubble
    /// left sitting over the game would be the shell agreeing and then
    /// carrying on. They leave rather than vanish — the flight is the same one
    /// a dismissal starts — and the announcements themselves stay on the list,
    /// because this switch was never about deleting anything.
    ///
    /// Returns whether the corner has to be redrawn on the strength of it.
    pub fn set_quiet(&mut self, quiet: bool) -> bool {
        if self.quiet == quiet {
            return false;
        }
        self.quiet = quiet;
        if !quiet {
            return false;
        }
        let mut leaving = false;
        for toast in self
            .toasts
            .iter_mut()
            .filter(|toast| toast.stage != Stage::Out)
        {
            toast.dismiss();
            leaving = true;
        }
        leaving
    }

    /// How many bubbles are occupying the corner rather than leaving it.
    fn standing(&self) -> usize {
        self.toasts
            .iter()
            .filter(|toast| toast.stage != Stage::Out)
            .count()
    }

    /// Say something on the shell's own account.
    ///
    /// The one way anything reaches this list that did not come over the bus,
    /// and it exists because some of what a console has to announce is about
    /// the *session* rather than about a program in it: a device that has just
    /// been paired with, and one that would not pair. There is no program to
    /// send those — the shell is what did the thing — and a desktop's answer,
    /// "have some daemon call `notify-send`", is a process this session does
    /// not have and a round trip through the bus to talk to itself.
    ///
    /// It goes through [`Center::arrived`] like everything else, so it is
    /// filed, it gets a bubble, it lights the bell, and it is silenced by
    /// do-not-disturb on exactly the same terms as an announcement from a chat
    /// client. The shell overruling its own switch would be the one exception
    /// that made the switch not mean anything.
    ///
    /// `icon` is one of the shell's own glyph names — see [`crate::icons`] —
    /// which is already in the atlas, so unlike a program's picture this one
    /// never costs a look through the icon theme.
    ///
    /// Returns whether a bubble went up, which is what the caller asks before
    /// making a noise.
    pub fn announce(&mut self, summary: &str, body: &str, icon: &str) -> bool {
        self.announce_message(summary, body, icon).1
    }

    /// The same, and which announcement it became.
    ///
    /// For the one thing the shell says that has somewhere to go back to: a
    /// message from a friend, whose row opens the conversation it arrived in.
    /// The number is how it is found again — the sender is filed under it
    /// beside this list, because who somebody is on Steam is no business of a
    /// notification centre — and in every other way this is an announcement
    /// like any other.
    pub fn announce_message(&mut self, summary: &str, body: &str, icon: &str) -> (u32, bool) {
        // Down, and never through zero: zero is what a program passes to mean
        // "a new one", so it is not a number an announcement may have.
        let id = self.ours;
        self.ours = self.ours.saturating_sub(1).max(1);
        let raised = self.arrived(Notification {
            id,
            app: "LineXinBar".to_string(),
            app_icon: icon.to_string(),
            image_path: None,
            image: None,
            image_key: None,
            desktop_entry: None,
            summary: summary.to_string(),
            body: body.to_string(),
            actions: Vec::new(),
            urgency: Urgency::Normal,
            // Kept, not transient. What this says is the answer to something
            // the user pressed and may well have walked away from — the whole
            // reason it is announced rather than drawn on the row is that they
            // are not necessarily looking at the row — so it has to be there
            // to be found behind the bell.
            transient: false,
            arrived: Instant::now(),
        });
        (id, raised)
    }

    pub fn announce_update(
        &mut self,
        summary: &str,
        body: &str,
        action: &str,
        silent: bool,
    ) -> (u32, bool) {
        let quiet = self.quiet;
        self.quiet |= silent;
        // Down, and never through zero: zero is what a program passes to mean
        // "a new one", so it is not a number an announcement may have.
        let id = self.ours;
        self.ours = self.ours.saturating_sub(1).max(1);
        let raised = self.arrived(Notification {
            id,
            app: "LineXinBar".to_string(),
            app_icon: crate::icons::SETTING_UPDATES.to_string(),
            image_path: None,
            image: None,
            image_key: None,
            desktop_entry: None,
            summary: summary.to_string(),
            body: body.to_string(),
            actions: vec![("default".into(), action.into())],
            urgency: Urgency::Normal,
            // Kept, not transient. What this says is the answer to something
            // the user pressed and may well have walked away from — the whole
            // reason it is announced rather than drawn on the row is that they
            // are not necessarily looking at the row — so it has to be there
            // to be found behind the bell.
            transient: false,
            arrived: Instant::now(),
        });
        self.quiet = quiet;
        (id, raised)
    }

    /// Take everything the bus has said and file it.
    pub fn collect(&mut self, service: &Service) -> Arrivals {
        let mut arrivals = Arrivals::default();
        for event in service.drain() {
            match event {
                Event::Arrived(notification) => {
                    arrivals.raised |= self.arrived(*notification);
                    arrivals.changed = true;
                }
                Event::Withdrawn(id) => arrivals.changed |= self.remove(id, Closed::ByProgram),
            }
        }
        arrivals
    }

    /// File one announcement, and put it in the corner if it is worth
    /// interrupting over. Returns whether a bubble went up for it, which is
    /// what the shell asks before making a noise.
    fn arrived(&mut self, notification: Notification) -> bool {
        let id = notification.id;
        // A replacement takes the place of what it replaces rather than
        // arriving above it. A download that reports every percent would
        // otherwise walk up the list as it went, and the row the user is
        // reading would be a different row by the time they had read it.
        let filed = match self.list.iter_mut().find(|held| held.id == id) {
            Some(held) => {
                *held = notification.clone();
                true
            }
            None if !notification.transient => {
                self.list.insert(0, notification.clone());
                true
            }
            None => false,
        };
        // Anything that reached the list is something to be read, and a
        // replacement is too: the row is on the list under an id the user may
        // well have read already, but what it says now is not what they read.
        // The one thing that is not is an announcement that was never filed —
        // there is no row for the mark to be pointing at.
        if filed {
            self.change_unseen(|unseen| {
                unseen.insert(id);
            });
        }

        // Filed and nothing else, whoever sent it and however urgent they said
        // it was. The switch is the user overruling every program on the
        // machine at once, which is the only thing it could mean: a
        // do-not-disturb that let the loudest senders through would be
        // do-not-disturb-unless-somebody-insists, and every program that
        // wanted to interrupt would insist. What was announced is still on the
        // list behind the bell, which is where it is read afterwards.
        if self.quiet {
            return false;
        }

        if notification.urgency == Urgency::Low {
            // Filed without a bubble: the program said this is not worth
            // interrupting anyone over, and this is the one hint that is taken
            // at its word. No bubble and, because the two are one answer, no
            // sound either.
            return false;
        }

        // A bubble for an announcement already in the corner is that bubble
        // changing, not a second one — and its clock starts again, because the
        // words are new even if the number is not.
        if let Some(toast) = self.toasts.iter_mut().find(|toast| toast.id == id) {
            toast.about = notification;
            if toast.stage == Stage::Out {
                toast.stage = Stage::In;
            }
            toast.since = Instant::now();
            // The same bubble saying something new, not a second one arriving.
            // Its clock starts again because the words are new; the corner does
            // not announce itself again, because nothing has appeared there
            // that was not there a moment ago. A download reporting every
            // percent would otherwise be a hundred chimes.
            return false;
        }

        // The oldest bubble is pushed out to make room rather than the newest
        // refused. What has just arrived is the thing the user has not seen.
        //
        // Counted over the ones that are *standing*, not over the list. A
        // bubble on its way out is still drawn — see the sweep in
        // [`Center::animate`] — and counting those as occupying the corner
        // would send the whole stack away to make room for one arrival.
        while self.standing() >= STACK {
            match self
                .toasts
                .iter_mut()
                .find(|toast| toast.stage != Stage::Out)
            {
                Some(oldest) => oldest.dismiss(),
                // Nothing left standing to hurry along.
                None => break,
            }
        }

        self.toasts.push(Toast {
            id,
            about: notification,
            since: Instant::now(),
            stage: Stage::In,
        });
        true
    }

    /// Advance the corner. Returns whether anything is still moving, which is
    /// what keeps the shell drawing.
    pub fn animate(&mut self) -> bool {
        let mut moving = false;
        for toast in &mut self.toasts {
            if toast.progress() < 1.0 {
                moving = true;
                continue;
            }
            match toast.stage {
                Stage::In => {
                    // The dwell begins here and not when the announcement
                    // arrived: see DWELL.
                    toast.stage = Stage::Sitting;
                    toast.since = Instant::now();
                    moving = true;
                }
                Stage::Sitting => {
                    toast.stage = Stage::Out;
                    toast.since = Instant::now();
                    moving = true;
                }
                // Finished leaving; swept below.
                Stage::Out => {}
            }
        }

        // Swept only once they have finished leaving. A bubble is taken off
        // the list of announcements the moment the user puts it away, and goes
        // on being drawn for as long as its flight lasts — nothing in this
        // shell vanishes before its transition has ended.
        let before = self.toasts.len();
        let mut expired = Vec::new();
        self.toasts.retain(|toast| {
            if toast.stage != Stage::Out || toast.progress() < 1.0 {
                return true;
            }
            // A bubble that has gone and whose announcement was never filed is
            // the end of that announcement: nobody will ever put it away,
            // because it is not on any list to be put away from. So this is
            // the one and only place the program behind a transient one can be
            // told, and the one place `Expired` is the true reason — the user
            // did not dismiss it, its time simply ran out.
            //
            // Anything still on the list is a bubble leaving, not an
            // announcement ending, and says nothing.
            if !self.list.iter().any(|held| held.id == toast.id) {
                expired.push((toast.id, Closed::Expired));
            }
            false
        });
        self.outbox.append(&mut expired);

        // The mark on the bell rides this too: it is a drawing that moves on
        // its own clock, and the frame loop asks this one question to know
        // whether the corner still needs frames.
        moving || self.badge_moving() || self.toasts.len() != before
    }

    /// Put one away because the user did. The bubble goes with it.
    pub fn dismiss(&mut self, id: u32) -> bool {
        self.remove(id, Closed::Dismissed)
    }

    /// Empty the list. The bubbles still up are sent on their way too: the
    /// user has just said they have read everything, and a bubble left sitting
    /// would be the shell disagreeing.
    pub fn dismiss_all(&mut self) -> bool {
        if self.list.is_empty() {
            return false;
        }
        for notification in self.list.drain(..) {
            self.outbox.push((notification.id, Closed::Dismissed));
        }
        for toast in &mut self.toasts {
            toast.dismiss();
        }
        // Nothing left to read, whether or not it was read before it went.
        self.change_unseen(|unseen| unseen.clear());
        true
    }

    /// Record that the user pressed one of an announcement's buttons.
    ///
    /// The announcement goes away with it. A button is a thing the user asked
    /// the *program* to do, and once they have asked, the row telling them
    /// they could has done its job.
    pub fn invoke(&mut self, id: u32, action: &str) -> bool {
        if !self.list.iter().any(|held| held.id == id) {
            return false;
        }
        self.invoked.push((id, action.to_string()));
        self.remove(id, Closed::Dismissed)
    }

    fn remove(&mut self, id: u32, why: Closed) -> bool {
        let held = self.list.iter().position(|held| held.id == id);
        if let Some(index) = held {
            self.list.remove(index);
            self.outbox.push((id, why));
        }
        // Off the list is off the bell, whichever route took it off and whether
        // or not anybody read it on the way. An unread mark outliving the only
        // thing it stood for would send the user to an empty list.
        self.change_unseen(|unseen| {
            unseen.remove(&id);
        });
        let mut touched = held.is_some();
        for toast in self.toasts.iter_mut().filter(|toast| toast.id == id) {
            toast.dismiss();
            touched = true;
        }
        touched
    }

    /// Send the programs everything owed to them. Called by the frame, so that
    /// nothing on the shell's side has to hold the connection.
    pub fn answer(&mut self, service: &Service) {
        for (id, why) in self.outbox.drain(..) {
            service.closed(id, why);
        }
        for (id, action) in self.invoked.drain(..) {
            service.invoked(id, &action);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shell can say something itself, and what it says is an announcement
    /// like any other: filed, bubbled, and counted against the bell.
    ///
    /// It has to be a real one rather than a special case drawn somewhere else,
    /// because the two presentations are the same object seen twice — a bubble
    /// the list did not know about would be a thing the user saw and could
    /// never find again.
    #[test]
    fn the_shell_can_announce_something_of_its_own() {
        let mut center = Center::default();
        assert!(center.announce("Ears is connected", "Paired.", "lxb:x"));
        assert_eq!(center.list().len(), 1);
        assert_eq!(center.toasts().len(), 1);
        assert!(center.unread(), "and the bell says so");
        assert_eq!(center.list()[0].title(), "Ears is connected");
        assert_eq!(center.list()[0].body, "Paired.");
        assert_eq!(center.list()[0].icon_name(), Some("lxb:x"));
        // Kept rather than transient: the whole reason it is announced is that
        // the user may not be looking at the page, so it has to be findable
        // afterwards.
        assert!(!center.list()[0].transient);
    }

    /// The shell's numbers count down from the top and a program's count up
    /// from one, which is the whole of how the two are kept apart.
    ///
    /// Not tidiness: [`Center::arrived`] treats a number it has seen before as
    /// a *replacement*, so a collision would have the shell silently overwrite
    /// somebody's download with a word about a headset.
    #[test]
    fn the_shells_own_numbers_cannot_collide_with_a_programs() {
        let mut center = Center::default();
        center.announce("One", "", "lxb:x");
        center.announce("Two", "", "lxb:x");
        let ours: Vec<u32> = center.list().iter().map(|held| held.id).collect();
        assert_eq!(
            ours,
            [u32::MAX - 1, u32::MAX],
            "newest first, counting down"
        );
        // Two announcements, two rows: the second did not replace the first.
        assert_eq!(center.list().len(), 2);
        // And a program's number is nowhere near them. The daemon hands those
        // out from one — see `Listener::next`.
        assert!(ours.iter().all(|id| *id > 1));
    }

    /// Do-not-disturb silences the shell exactly as it silences everything
    /// else. The one exception that let the shell through would be the
    /// exception that made the switch not mean anything.
    #[test]
    fn the_shells_own_announcement_obeys_do_not_disturb() {
        let mut center = Center::new(true);
        assert!(
            !center.announce("Ears would not pair", "", "lxb:x"),
            "nothing in the corner and nothing to hear"
        );
        assert!(center.toasts().is_empty());
        // Filed all the same, which is what the switch means: not shouted, not
        // thrown away.
        assert_eq!(center.list().len(), 1);
        assert!(center.unread());
    }

    /// The picture the announcement wears is the most specific one its sender
    /// gave, and the sender's identity is only the last resort.
    ///
    /// This is the order that makes `--icon=` mean anything. Preferring the
    /// desktop entry — which is what this did — makes every announcement from
    /// one program wear one picture, which is the exact thing a program is
    /// overriding when it names an icon.
    #[test]
    fn the_picture_a_program_chose_beats_the_one_it_is_known_by() {
        let mut held = Notification::heard(1, "Update ready");
        assert_eq!(held.icon_name(), None, "nothing given, nothing to draw");

        held.desktop_entry = Some("org.example.Updater".to_string());
        assert_eq!(held.icon_name(), Some("org.example.Updater"));

        held.app_icon = "software-update-available".to_string();
        assert_eq!(
            held.icon_name(),
            Some("software-update-available"),
            "what the sender asked for, not what it is filed under"
        );

        held.image_path = Some("/usr/share/pixmaps/this-one.png".to_string());
        assert_eq!(held.icon_name(), Some("/usr/share/pixmaps/this-one.png"));

        // An empty string is a program filling the argument in rather than
        // choosing a picture, and is stepped over like the absence it is.
        held.image_path = Some(String::new());
        held.app_icon = String::new();
        assert_eq!(held.icon_name(), Some("org.example.Updater"));
    }

    /// Pixels sent in a hint are taken as sent, and every number in the header
    /// that describes them is checked against the others first.
    ///
    /// They arrive from any program on the session bus and they describe a
    /// walk over memory, so a header that does not add up is a picture that is
    /// refused rather than one that is read anyway.
    #[test]
    fn a_picture_sent_as_pixels_is_believed_only_as_far_as_it_adds_up() {
        let sent = |width: i32, height: i32, stride: i32, alpha: bool, bits: i32, channels: i32| {
            let len = (stride.max(0) * height.max(0)) as usize;
            let body = zbus::zvariant::Value::from((
                width,
                height,
                stride,
                alpha,
                bits,
                channels,
                vec![7u8; len],
            ));
            Image::from_hint(&OwnedValue::try_from(body).unwrap())
        };

        // Four channels with alpha, three without: the plain forms.
        let rgba = sent(2, 2, 8, true, 8, 4).expect("a picture that adds up");
        assert_eq!((rgba.width, rgba.height, rgba.channels), (2, 2, 4));
        assert!(sent(2, 2, 6, false, 8, 3).is_some());

        // The channel count has to agree with the alpha flag. A sender saying
        // both "no alpha" and "four channels" has told the shell two different
        // things about the same buffer, and guessing which it meant is how a
        // picture comes out with its colours rotated.
        assert!(sent(2, 2, 8, false, 8, 4).is_none());
        assert!(sent(2, 2, 6, true, 8, 3).is_none());

        // Eight bits a sample is the only depth the specification defines and
        // the only one anything sends.
        assert!(sent(2, 2, 8, true, 16, 4).is_none());

        // Nothing, and more than any screen: both are headers no real picture
        // has, and the large one is a request to allocate whatever number a
        // program happened to put in a field.
        assert!(sent(0, 2, 8, true, 8, 4).is_none());
        assert!(sent(2, 0, 8, true, 8, 4).is_none());
        assert!(sent(9000, 2, 36000, true, 8, 4).is_none());

        // A picture is named by what it *is*, since it arrived with no name:
        // the same pixels are the same key, and different pixels are not — so
        // an announcement replaced with new art gets a new cell in the atlas
        // rather than keeping the picture it had.
        let same = sent(2, 2, 8, true, 8, 4).unwrap();
        assert_eq!(rgba.key(), same.key());
        let mut other = same.clone();
        other.data[0] = 9;
        assert_ne!(rgba.key(), other.key());
        assert!(rgba.key().starts_with("lxb:"), "{}", rgba.key());
    }

    /// A path written as a URI, which is how the specification says to write
    /// one, comes out as a path — and nothing else is touched.
    #[test]
    fn a_file_uri_is_unwrapped_and_a_name_is_left_alone() {
        assert_eq!(unwrap_file_uri("file:///tmp/a.png".into()), "/tmp/a.png");
        assert_eq!(
            unwrap_file_uri("software-update-available".into()),
            "software-update-available",
            "a theme name is not a URI"
        );
        assert_eq!(unwrap_file_uri("/tmp/a.png".into()), "/tmp/a.png");

        // A host is a file on another machine, and a scheme the shell will not
        // go and fetch something over. Both are handed on whole rather than
        // mangled into a path that would then be looked up as a theme name.
        assert_eq!(
            unwrap_file_uri("file://elsewhere/a.png".into()),
            "file://elsewhere/a.png"
        );
        assert_eq!(
            unwrap_file_uri("http://example.com/a.png".into()),
            "http://example.com/a.png"
        );
    }

    fn arrived(center: &mut Center, id: u32, summary: &str) {
        center.arrived(Notification {
            id,
            app: "Test".to_string(),
            app_icon: String::new(),
            image_path: None,
            image: None,
            image_key: None,
            desktop_entry: None,
            summary: summary.to_string(),
            body: String::new(),
            actions: Vec::new(),
            urgency: Urgency::Normal,
            transient: false,
            arrived: Instant::now(),
        });
    }

    /// The panel reads top to bottom and the newest thing is the one the user
    /// came to see, so it is the one at the top. Nothing sorts the list to
    /// achieve that — it is the order things are kept in.
    #[test]
    fn the_newest_announcement_is_at_the_top_of_the_list() {
        let mut center = Center::default();
        arrived(&mut center, 1, "first");
        arrived(&mut center, 2, "second");
        arrived(&mut center, 3, "third");
        let summaries: Vec<&str> = center
            .list()
            .iter()
            .map(|held| held.summary.as_str())
            .collect();
        assert_eq!(summaries, ["third", "second", "first"]);
    }

    /// A replacement changes a row where it stands. The alternative — taking
    /// it off and putting it back on top — is what makes a progress bar walk
    /// up the list while somebody is trying to read the row above it.
    #[test]
    fn replacing_one_leaves_it_where_it_was() {
        let mut center = Center::default();
        arrived(&mut center, 1, "downloading");
        arrived(&mut center, 2, "something else");
        arrived(&mut center, 1, "downloaded");
        let summaries: Vec<&str> = center
            .list()
            .iter()
            .map(|held| held.summary.as_str())
            .collect();
        assert_eq!(summaries, ["something else", "downloaded"]);
    }

    /// The corner is not the list: a bubble is shown and forgotten, and the
    /// announcement behind it is kept until somebody puts it away.
    #[test]
    fn a_transient_announcement_is_shown_without_being_filed() {
        let mut center = Center::default();
        center.arrived(Notification {
            id: 7,
            app: "Test".to_string(),
            app_icon: String::new(),
            image_path: None,
            image: None,
            image_key: None,
            desktop_entry: None,
            summary: "copied".to_string(),
            body: String::new(),
            actions: Vec::new(),
            urgency: Urgency::Normal,
            transient: true,
            arrived: Instant::now(),
        });
        assert!(center.list().is_empty(), "a transient one is not filed");
        assert_eq!(center.toasts().len(), 1, "but it is still shown");
    }

    /// A program that marked something low has said it is not worth
    /// interrupting anyone over, and that is the one hint taken at its word.
    #[test]
    fn a_low_urgency_announcement_is_filed_without_a_bubble() {
        let mut center = Center::default();
        center.arrived(Notification {
            id: 9,
            app: "Test".to_string(),
            app_icon: String::new(),
            image_path: None,
            image: None,
            image_key: None,
            desktop_entry: None,
            summary: "indexing finished".to_string(),
            body: String::new(),
            actions: Vec::new(),
            urgency: Urgency::Low,
            transient: false,
            arrived: Instant::now(),
        });
        assert_eq!(center.list().len(), 1);
        assert!(center.toasts().is_empty());
    }

    /// The guide's do-not-disturb switch: everything is filed and nothing
    /// interrupts.
    ///
    /// Both halves of *interrupt* are one assertion here, and they have to be:
    /// the bubble and the chime are the same answer — see [`Arrivals::raised`]
    /// — so a switch that stopped one and not the other would be a silent
    /// screen that still made a noise, or a chime with nothing to look at.
    #[test]
    fn do_not_disturb_files_an_announcement_without_a_bubble_or_a_chime() {
        let sent = |id, urgency| Notification {
            id,
            app: "Test".to_string(),
            app_icon: String::new(),
            image_path: None,
            image: None,
            image_key: None,
            desktop_entry: None,
            summary: "something".to_string(),
            body: String::new(),
            actions: Vec::new(),
            urgency,
            transient: false,
            arrived: Instant::now(),
        };

        let mut center = Center::new(true);
        assert!(center.quiet());
        assert!(
            !center.arrived(sent(1, Urgency::Normal)),
            "nothing went up, so nothing chimes"
        );
        // Including the urgency that is otherwise allowed to sit on the screen
        // through a fullscreen game. The switch is the user overruling every
        // program on the machine, and a program that insists is exactly what
        // it is there to overrule.
        assert!(!center.arrived(sent(2, Urgency::Critical)));
        assert!(center.toasts().is_empty(), "the corner stays empty");

        // Delivered all the same: this is a switch about interrupting, not
        // about discarding, and the bell beside it is where they are read.
        assert_eq!(center.list().len(), 2);
        assert_eq!(center.list()[0].id, 2, "newest first, as ever");

        // And the next one through once it is off.
        assert!(
            !center.set_quiet(false),
            "nothing was standing to send away"
        );
        assert!(center.arrived(sent(3, Urgency::Normal)));
        assert_eq!(center.toasts().len(), 1);
    }

    /// The mark on the bell says *something arrived that you have not looked
    /// at*, and the list being on screen is what looking at it means.
    ///
    /// Not *the list is not empty*. The list is a record and keeps what it is
    /// given until it is thrown away, so a mark that meant that would be lit
    /// for as long as the user left anything in it and would stop meaning
    /// anything at all.
    #[test]
    fn the_bell_is_marked_until_the_list_has_been_looked_at() {
        let mut center = Center::default();
        assert!(!center.unread(), "nothing has happened yet");

        arrived(&mut center, 1, "a message");
        arrived(&mut center, 2, "another");
        assert!(center.unread());

        assert!(center.mark_seen(), "the panel went up");
        assert!(!center.unread());
        assert_eq!(center.list().len(), 2, "read is not thrown away");
        assert!(!center.mark_seen(), "and saying it twice is not an event");

        // Anything new after that is unread again, however much has been read
        // before it.
        arrived(&mut center, 3, "a third");
        assert!(center.unread());

        // A replacement is unread too: the row is one the user may well have
        // read, but what it says now is not what they read.
        center.mark_seen();
        arrived(&mut center, 3, "a third, corrected");
        assert!(center.unread());
        assert_eq!(center.list().len(), 3, "and it replaced rather than added");
    }

    /// The mark goes out when the last unread announcement *leaves*, not only
    /// when it is read — a bubble put away from the corner is one nobody will
    /// ever open the list to find.
    ///
    /// This is the whole reason unread is a set of ids rather than a flag: a
    /// flag cannot tell an empty list from a list somebody has read.
    #[test]
    fn the_mark_goes_out_with_the_last_thing_it_stood_for() {
        let mut center = Center::default();
        arrived(&mut center, 1, "a message");
        arrived(&mut center, 2, "another");

        assert!(center.dismiss(1));
        assert!(center.unread(), "one of the two is still unread");
        assert!(center.dismiss(2));
        assert!(!center.unread(), "and now neither is");

        // The same by the other two routes off the list: the program taking it
        // back, and the row that empties the whole thing.
        arrived(&mut center, 3, "a third");
        assert!(center.remove(3, Closed::ByProgram));
        assert!(!center.unread());

        arrived(&mut center, 4, "a fourth");
        assert!(center.dismiss_all());
        assert!(!center.unread());

        // And a transient one never lights it at all: it is never filed, so
        // there is no row for the mark to be pointing at and no way it could
        // ever be read from the list.
        center.arrived(Notification {
            id: 5,
            app: "Test".to_string(),
            app_icon: String::new(),
            image_path: None,
            image: None,
            image_key: None,
            desktop_entry: None,
            summary: "50%".to_string(),
            body: String::new(),
            actions: Vec::new(),
            urgency: Urgency::Normal,
            transient: true,
            arrived: Instant::now(),
        });
        assert!(center.list().is_empty());
        assert!(!center.unread());
    }

    /// It grows on and goes out rather than appearing and disappearing: the
    /// tile stands beside the panel it opens rather than behind it, so the
    /// user is looking straight at the mark when the press clears it.
    #[test]
    fn the_mark_grows_on_and_goes_out_again() {
        let mut center = Center::default();
        assert_eq!(center.badge(), 0.0);
        assert!(!center.badge_moving(), "nothing to watch on an empty bell");

        arrived(&mut center, 1, "a message");
        assert!(center.badge_moving());
        assert!(
            center.badge() < 1.0,
            "it is not there the instant it arrives"
        );

        // Run the flight out.
        center.badge_at = Some(Instant::now() - std::time::Duration::from_secs_f32(BADGE_FLIGHT));
        assert_eq!(center.badge(), 1.0);
        assert!(!center.badge_moving());

        // And out again from where it had got to, rather than from the top: a
        // mark told to go out part-way through growing must not jump up to
        // full first and fall from there.
        let mut center = Center::default();
        arrived(&mut center, 1, "a message");
        center.badge_at =
            Some(Instant::now() - std::time::Duration::from_secs_f32(BADGE_FLIGHT * 0.5));
        let midway = center.badge();
        assert!(midway > 0.0 && midway < 1.0, "part-way on: {midway}");

        center.mark_seen();
        let leaving = center.badge();
        assert!(
            (leaving - midway).abs() < 0.02,
            "it left from {midway} and not from the top: {leaving}"
        );
        center.badge_at = Some(Instant::now() - std::time::Duration::from_secs_f32(BADGE_FLIGHT));
        assert_eq!(center.badge(), 0.0);
        assert!(!center.badge_moving());
    }

    /// Throwing the switch with something already in the corner sends it on
    /// its way: the user has just said they do not want to be interrupted, and
    /// a bubble left sitting over the game would be the shell agreeing and
    /// then carrying on regardless.
    ///
    /// It *leaves* rather than vanishing — the same flight a dismissal starts,
    /// because nothing in this shell disappears before its transition has
    /// ended — and the announcement itself stays on the list, because the
    /// switch was never about throwing anything away.
    #[test]
    fn turning_do_not_disturb_on_sends_the_corner_away_and_keeps_the_list() {
        let mut center = Center::default();
        arrived(&mut center, 1, "a message");
        arrived(&mut center, 2, "another");
        assert_eq!(center.toasts().len(), 2);

        assert!(center.set_quiet(true), "the corner has to be redrawn");
        assert!(
            center
                .toasts()
                .iter()
                .all(|toast| toast.stage == Stage::Out),
            "on their way, not gone"
        );
        assert_eq!(center.list().len(), 2, "and still delivered");
        // Nothing was closed, so no program is owed an answer about it.
        assert!(center.outbox.is_empty());

        // Saying it twice is not an event.
        assert!(!center.set_quiet(true));
    }

    /// Pressing one of a program's buttons tells that program, and takes the
    /// announcement off the list with it: the row existed to say the button
    /// was there, and once it has been pressed it has done its job.
    #[test]
    fn a_button_press_reaches_the_program_and_ends_the_row() {
        let mut center = Center::default();
        center.arrived(Notification {
            id: 4,
            app: "Mail".to_string(),
            app_icon: String::new(),
            image_path: None,
            image: None,
            image_key: None,
            desktop_entry: None,
            summary: "A message".to_string(),
            body: String::new(),
            actions: vec![
                ("default".to_string(), "Open".to_string()),
                ("reply".to_string(), "Reply".to_string()),
            ],
            urgency: Urgency::Normal,
            transient: false,
            arrived: Instant::now(),
        });

        assert!(center.invoke(4, "reply"));
        assert_eq!(center.invoked, vec![(4, "reply".to_string())]);
        assert!(center.list().is_empty());
        // Dismissed, not expired: the user acted on it, and a program that
        // hears the difference knows its message was read.
        assert_eq!(center.outbox, vec![(4, Closed::Dismissed)]);

        // And an announcement that has already gone answers nothing rather
        // than inventing a press — the panel can outlive the row it was
        // opened from.
        assert!(!center.invoke(4, "reply"));
        assert_eq!(center.invoked.len(), 1);
    }

    /// A bubble that was never filed ends when its bubble does, so that is the
    /// one place the program behind it can be told — and *expired* is the true
    /// reason there, because nobody dismissed anything.
    #[test]
    fn a_transient_bubble_tells_its_program_when_it_times_out() {
        let mut center = Center::default();
        let transient = Notification {
            id: 5,
            app: "Volume".to_string(),
            app_icon: String::new(),
            image_path: None,
            image: None,
            image_key: None,
            desktop_entry: None,
            summary: "50%".to_string(),
            body: String::new(),
            actions: Vec::new(),
            urgency: Urgency::Normal,
            transient: true,
            arrived: Instant::now(),
        };
        center.arrived(transient);
        assert!(center.list().is_empty(), "never filed");

        // Run it to the end of its life.
        for toast in &mut center.toasts {
            toast.stage = Stage::Out;
            toast.since = Instant::now() - std::time::Duration::from_secs(1);
        }
        center.animate();
        assert!(center.toasts().is_empty());
        assert_eq!(center.outbox, vec![(5, Closed::Expired)]);

        // One that *is* on the list says nothing when its bubble goes: the
        // announcement has not ended, it has merely stopped interrupting.
        let mut center = Center::default();
        arrived(&mut center, 6, "kept");
        for toast in &mut center.toasts {
            toast.stage = Stage::Out;
            toast.since = Instant::now() - std::time::Duration::from_secs(1);
        }
        center.animate();
        assert!(center.toasts().is_empty());
        assert_eq!(center.list().len(), 1);
        assert!(center.outbox.is_empty());
    }

    /// The noise belongs to the bubble, not to the announcement.
    ///
    /// Which is the whole of the rule: the corner is what interrupts somebody,
    /// so anything that does not put something new there has nothing to say
    /// out loud. Three ways of not doing that, and each is a real case — a
    /// program that marked its news unimportant, a download reporting every
    /// percent through one announcement, and a program taking one back.
    #[test]
    fn only_a_bubble_going_up_makes_a_noise() {
        let mut center = Center::default();
        let quiet = |urgency, transient, id| Notification {
            id,
            app: "Test".to_string(),
            app_icon: String::new(),
            image_path: None,
            image: None,
            image_key: None,
            desktop_entry: None,
            summary: "something".to_string(),
            body: String::new(),
            actions: Vec::new(),
            urgency,
            transient,
            arrived: Instant::now(),
        };

        assert!(
            center.arrived(quiet(Urgency::Normal, false, 1)),
            "a bubble going up is the one thing that does"
        );
        assert!(
            !center.arrived(quiet(Urgency::Low, false, 2)),
            "a program that said this is not worth interrupting over is taken \
             at its word, in both senses at once"
        );
        assert!(
            !center.arrived(quiet(Urgency::Normal, false, 1)),
            "and a bubble already in the corner saying something new is that \
             bubble changing, not a second one arriving"
        );

        // Even a transient one chimes: it is *only* a bubble, so if the corner
        // is not what tells the user about it, nothing is.
        assert!(center.arrived(quiet(Urgency::Normal, true, 3)));

        // Nothing else on the way in or out has anything to say.
        let taken_back = center.remove(1, Closed::ByProgram);
        assert!(taken_back, "it still changes the screen");
    }

    /// The corner holds three. A fourth pushes the one that has been there
    /// longest on its way rather than being refused — what has just arrived is
    /// the thing nobody has seen.
    #[test]
    fn the_corner_holds_three_and_the_oldest_leaves_first() {
        let mut center = Center::default();
        for id in 1..=4 {
            arrived(&mut center, id, "something");
        }
        assert_eq!(center.toasts().len(), 4, "the fourth is not refused");
        assert_eq!(
            center
                .toasts()
                .iter()
                .filter(|toast| toast.stage() == Stage::Out)
                .map(|toast| toast.id)
                .collect::<Vec<_>>(),
            vec![1],
            "and the oldest is the one on its way out"
        );
    }

    /// Every way an announcement can leave has to reach the program that sent
    /// it, and each of them has its own reason: a program hearing *dismissed*
    /// knows the user saw it.
    #[test]
    fn the_program_is_told_why_its_announcement_went() {
        let mut center = Center::default();
        arrived(&mut center, 1, "one");
        arrived(&mut center, 2, "two");
        arrived(&mut center, 3, "three");
        center.dismiss(2);
        center.remove(3, Closed::ByProgram);
        center.dismiss_all();
        assert_eq!(
            center.outbox,
            vec![
                (2, Closed::Dismissed),
                (3, Closed::ByProgram),
                (1, Closed::Dismissed),
            ]
        );
    }

    /// Dismissing a row takes its bubble with it — but sends it on its way
    /// rather than deleting it, because nothing in this shell disappears
    /// before its transition has ended.
    #[test]
    fn putting_one_away_starts_its_bubble_leaving() {
        let mut center = Center::default();
        arrived(&mut center, 1, "one");
        center.dismiss(1);
        assert!(center.list().is_empty());
        assert_eq!(center.toasts().len(), 1);
        assert_eq!(center.toasts()[0].stage(), Stage::Out);
    }

    /// An announcement with nothing in its summary is a program that put
    /// everything in the body, not a row worth dropping.
    #[test]
    fn a_row_with_no_summary_still_has_something_to_say() {
        let mut held = Notification {
            id: 1,
            app: "Backup".to_string(),
            app_icon: String::new(),
            image_path: None,
            image: None,
            image_key: None,
            desktop_entry: None,
            summary: String::new(),
            body: "\n  \nCopied 412 files".to_string(),
            actions: Vec::new(),
            urgency: Urgency::Normal,
            transient: false,
            arrived: Instant::now(),
        };
        assert_eq!(held.title(), "Copied 412 files");
        held.body = String::new();
        assert_eq!(held.title(), "Backup");
    }

    /// The launcher's own identifier is what a row falls back to, so that an
    /// announcement from a program that named no picture still wears the one
    /// the bar draws for it.
    ///
    /// Only as a fallback. This test used to assert the opposite — that the
    /// identifier is *preferred* — on the reasoning that the row should match
    /// the tile on the start screen. It is a real advantage and it is not
    /// worth what it costs: it throws away the only picture the sender
    /// actually asked for.
    #[test]
    fn the_desktop_entry_is_what_an_announcement_falls_back_to() {
        let mut held = Notification {
            id: 1,
            app: "Zen".to_string(),
            app_icon: "/opt/zen/share/notify.png".to_string(),
            image_path: None,
            image: None,
            image_key: None,
            desktop_entry: Some("zen-browser".to_string()),
            summary: "Download finished".to_string(),
            body: String::new(),
            actions: Vec::new(),
            urgency: Urgency::Normal,
            transient: false,
            arrived: Instant::now(),
        };
        assert_eq!(held.icon_name(), Some("/opt/zen/share/notify.png"));
        held.app_icon = String::new();
        assert_eq!(held.icon_name(), Some("zen-browser"));
        held.desktop_entry = None;
        assert_eq!(held.icon_name(), None);
    }

    /// A flat list of actions with an odd entry on the end is a program that
    /// got it wrong; the odd one is dropped rather than becoming a button
    /// labelled with its own key.
    #[test]
    fn a_lone_trailing_action_is_dropped_rather_than_guessed_at() {
        let actions = ["default", "Open", "reply", "Reply", "stray"];
        let paired: Vec<(String, String)> = actions
            .chunks_exact(2)
            .map(|pair| (pair[0].to_string(), pair[1].to_string()))
            .collect();
        assert_eq!(
            paired,
            vec![
                ("default".to_string(), "Open".to_string()),
                ("reply".to_string(), "Reply".to_string()),
            ]
        );
    }
}
