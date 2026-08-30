//! Putting a *file* question to the session shell, and waiting for the answer.
//!
//! The mirror of [`crate::cast::ask_to_share`], and it exists for exactly the
//! same reason: this process cannot draw. The shell owns the renderer, the
//! glass, the fonts and every list of files this session has ever put on a
//! screen, and a portal that grew a toolkit of its own to draw one file chooser
//! would be a second desktop. So the question goes over `lxb_shell_v1`, the
//! session's own channel: the compositor carries it to whichever shell is up —
//! over the top of a fullscreen application, which is the case that matters —
//! and carries the answer back.
//!
//! ## Everything that is not a file is a refusal
//!
//! A cancelled question, a shell that never answers, a session with no shell at
//! all, a compositor that is not LineXinBar: all of them come back as an empty
//! [`Chosen::files`], and every one of them is answered up the chain as the
//! user having cancelled. There is no path through here that hands an
//! application a file because something was missing.

use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};

use lxb_protocol::client::lxb_shell_v1::{self, LxbShellV1, Matching, Picking};

/// First version of `lxb_shell_v1` that can put a file question to the shell.
/// Below it there is nobody to ask, which is a refusal like any other.
const PICK_SINCE: u32 = 35;

/// What `answer_pick` says when no kind of file was in force. The protocol's
/// own number for it, quoted here so the two halves cannot disagree about which
/// index means "not one of them".
const NO_KIND: u32 = u32::MAX;

/// What the application wants chosen.
///
/// One enum rather than a set of flags, because the four are four different
/// questions the user is being asked and exactly one of them is true at a time.
/// It maps onto `lxb_shell_v1`'s own `picking` enum, and the mapping is the one
/// line of [`Wanted::purpose`] — the protocol's names are the ones this reads
/// from, so the shell and the portal cannot come to different conclusions about
/// what the user is standing in front of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum For {
    /// One file that is already there.
    OneFile,
    /// Any number of files that are already there.
    ManyFiles,
    /// A folder, whatever is in it. Both `OpenFile` with `directory` set and
    /// `SaveFiles`, which asks for somewhere to put a list of names it has
    /// already decided on.
    AFolder,
    /// A folder and a name to write into it.
    ANewFile,
}

/// One kind of file the application will accept: what it is called, and the
/// patterns that are it.
///
/// The portal's own `a(sa(us))` with the nesting taken out, because that shape
/// is a list of names each carrying a list of patterns and the protocol carries
/// one pattern per request. Regrouping happens in the shell, where the rows are
/// drawn; here they are simply sent in order.
#[derive(Debug, Clone)]
pub struct Kind {
    pub name: String,
    pub pattern: String,
    /// Whether `pattern` is a shell glob or a media type.
    pub mime: bool,
}

/// The whole of one file question, as the portal received it.
#[derive(Debug, Clone)]
pub struct Wanted {
    /// What the application calls itself. May be empty, which the shell says
    /// out loud rather than showing a blank.
    pub app_id: String,
    pub purpose: For,
    /// What the application called the question. May be empty, and is never
    /// trusted for anything but its own line of text.
    pub title: String,
    /// The word the application wants on the row that answers. May be empty.
    pub accept: String,
    /// What a new file is called to begin with. Meaningless for anything but
    /// [`For::ANewFile`].
    pub name: String,
    /// An absolute directory to open in, or empty. A shell may ignore it.
    pub at: String,
    pub kinds: Vec<Kind>,
}

impl Wanted {
    fn purpose(&self) -> Picking {
        match self.purpose {
            For::OneFile => Picking::OneFile,
            For::ManyFiles => Picking::ManyFiles,
            For::AFolder => Picking::AFolder,
            For::ANewFile => Picking::ANewFile,
        }
    }
}

/// What came back: the files, and which kind was in force when they were
/// chosen.
///
/// An empty `files` is the user having chosen nothing, whatever the reason —
/// see this module's own head, where the reasons are listed and why they are
/// all one answer.
#[derive(Debug, Clone, Default)]
pub struct Chosen {
    pub files: Vec<String>,
    /// Which of [`Wanted::kinds`]' *names* was in force, numbered from zero in
    /// the order they were offered. `None` where none was, which is both an
    /// application that offered nothing and a user who was looking at the whole
    /// disk.
    pub kind: Option<usize>,
}

/// One question, and the state of hearing it answered.
#[derive(Default)]
struct Asking {
    control: Option<LxbShellV1>,
    answered: bool,
    chosen: Chosen,
}

impl Dispatch<wl_registry::WlRegistry, ()> for Asking {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        if interface == "lxb_shell_v1" {
            state.control = Some(registry.bind(name, version.min(PICK_SINCE), queue, ()));
        }
    }
}

impl Dispatch<LxbShellV1, ()> for Asking {
    fn event(
        state: &mut Self,
        _: &LxbShellV1,
        event: lxb_shell_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            // The files, which always arrive before the answer that ends the
            // question — so by the time `answered` is set, `chosen` is whole.
            lxb_shell_v1::Event::PickChosen { path, .. } => state.chosen.files.push(path),
            lxb_shell_v1::Event::PickAnswered { kind, .. } => {
                state.answered = true;
                state.chosen.kind = (kind != NO_KIND).then_some(kind as usize);
            }
            // Everything else on this interface is the session talking to its
            // shell, and none of it is this process's business. `pick_request`
            // in particular is this very question going *out*: it reaches every
            // other client bound to the interface, and never comes back here.
            _ => {}
        }
    }
}

/// Put the question to the shell and wait for it to be answered.
///
/// Blocking, on whatever thread calls it, because the whole call it is
/// answering is "which file" — there is nothing to get on with until somebody
/// says. It is therefore never called on the thread that serves D-Bus; see
/// [`crate::filechooser`], where that mistake is written down.
pub fn ask(wanted: &Wanted, patience: std::time::Duration) -> anyhow::Result<Chosen> {
    let connection = Connection::connect_to_env()
        .map_err(|err| anyhow::anyhow!("no LineXinBar session to ask: {err}"))?;
    let mut queue = connection.new_event_queue::<Asking>();
    let handle = queue.handle();
    connection.display().get_registry(&handle, ());
    let mut asking = Asking::default();
    queue.roundtrip(&mut asking)?;
    queue.roundtrip(&mut asking)?;

    let control = asking.control.clone().ok_or_else(|| {
        anyhow::anyhow!("this compositor is not LineXinBar, so there is nobody to ask")
    })?;
    if control.version() < PICK_SINCE {
        anyhow::bail!("this compositor cannot put a file question to its shell");
    }

    // One question at a time from this connection — a fresh one is made for
    // each — so the number never has to mean more than "the one being asked".
    const THIS_QUESTION: u32 = 1;
    for kind in &wanted.kinds {
        control.offer_kind(
            THIS_QUESTION,
            kind.name.clone(),
            kind.pattern.clone(),
            if kind.mime {
                Matching::Mime
            } else {
                Matching::Glob
            },
        );
    }
    control.ask_to_pick_files(
        THIS_QUESTION,
        wanted.app_id.clone(),
        wanted.purpose(),
        wanted.title.clone(),
        wanted.accept.clone(),
        wanted.name.clone(),
        wanted.at.clone(),
    );
    connection.flush()?;

    let until = std::time::Instant::now() + patience;
    while !asking.answered {
        if std::time::Instant::now() >= until {
            tracing::warn!("nobody answered the file question; taking that as a cancellation");
            return Ok(Chosen::default());
        }
        // Blocking, but bounded: the compositor answers outright when there is
        // no shell, so the wait is the user walking their own disk.
        queue.blocking_dispatch(&mut asking)?;
    }
    Ok(asking.chosen)
}
