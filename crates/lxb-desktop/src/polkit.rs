//! The session's authentication agent: what answers when something on this
//! machine asks the user to prove they may do a thing.
//!
//! Mounting a disk, installing a package, changing the clock — none of those
//! are done by the program the user pressed. That program asks `polkitd`, which
//! looks up the action's policy, and where the policy says a human has to
//! agree, `polkitd` turns round and asks *the session's agent* to collect
//! proof. A desktop with no agent registered is a desktop where every one of
//! those requests is refused with no way for the user to allow it, which is
//! what LineXinBar was until this module.
//!
//! ## The three parties
//!
//! ```text
//!   an application ──CheckAuthorization──▶ polkitd
//!                                            │ BeginAuthentication(cookie)
//!                                            ▼
//!                                      this shell ────▶ the panel, the field
//!                                            │            and what is typed
//!                                            │ user\ncookie\npassword\n
//!                                            ▼
//!                              polkit-agent-helper-1 (root) ──▶ PAM
//!                                            │ AuthenticationAgentResponse
//!                                            ▼
//!                                         polkitd
//! ```
//!
//! The shell never decides anything. It collects a password, hands it to
//! polkit's own helper — the only part of this that runs as root, and the only
//! part that talks to PAM — and the helper tells `polkitd` directly whether it
//! was right. Nothing the shell can get wrong turns a "no" into a "yes": the
//! most a bug here can do is fail to ask.
//!
//! ## Why it is in the shell rather than beside the portal
//!
//! [`crate::screenshot`] and the screen-sharing consent question are asked by a
//! process that cannot draw, and the question travels to the shell over
//! `lxb_shell_v1`. This one is the other way round for one reason: what travels
//! back is a **password**. [`Secret`] exists so that it lives in one
//! allocation, is never printed, and is overwritten when it is dropped; sending
//! it across a Wayland connection, through the compositor, to a second process
//! would undo all three. So the D-Bus is here, on a thread of its own, and the
//! password goes from the field straight down the helper's socket without
//! leaving this process.
//!
//! ## Two helpers, one conversation
//!
//! polkit ships the PAM half as a separate root program, and there are two ways
//! to reach it depending on how the machine was built:
//!
//! * **socket-activated** (polkit 126 and later, where systemd starts it):
//!   connect to `/run/polkit/agent-helper.socket` and write the user's name and
//!   the cookie down it. Nothing is setuid.
//! * **setuid** (everything older): run
//!   `/usr/lib/polkit-1/polkit-agent-helper-1 <user>` and write the cookie to
//!   its standard input.
//!
//! After that first line the two are identical, and [`Session`] speaks the same
//! conversation to both: the helper writes `PAM_PROMPT_ECHO_OFF <prompt>` and
//! reads back a line, and ends with `SUCCESS` or `FAILURE`. The text is escaped
//! the way GLib escapes it, which is why [`unescape`] exists.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

use zbus::zvariant::{OwnedValue, Value};

use crate::secret::Secret;

/// Where this agent's object hangs on the bus.
///
/// Under LineXinBar's own name rather than polkit's: the path is ours to
/// choose, it is handed to `polkitd` at registration, and a path in somebody
/// else's namespace is a claim on a name we do not own.
const AGENT_PATH: &str = "/org/linexinbar/PolicyKit1/AuthenticationAgent";

const AUTHORITY_NAME: &str = "org.freedesktop.PolicyKit1";
const AUTHORITY_PATH: &str = "/org/freedesktop/PolicyKit1/Authority";
const AUTHORITY_INTERFACE: &str = "org.freedesktop.PolicyKit1.Authority";

/// Where the helper listens on a machine whose polkit is socket-activated.
const HELPER_SOCKET: &str = "/run/polkit/agent-helper.socket";

/// Where the setuid helper is installed, in the order the distributions that
/// still ship one put it.
///
/// A list searched at runtime rather than one path: the same shell runs on
/// machines that put it in three different places, and the first one that is
/// there is the right one. Nothing here is particular to any one machine —
/// these are layouts, not this computer's.
const HELPER_PROGRAMS: [&str; 5] = [
    "/usr/lib/polkit-1/polkit-agent-helper-1",
    "/usr/libexec/polkit-agent-helper-1",
    "/usr/lib64/polkit-1/polkit-agent-helper-1",
    "/usr/lib/policykit-1/polkit-agent-helper-1",
    "/usr/local/lib/polkit-1/polkit-agent-helper-1",
];

/// One authorisation `polkitd` has asked this session to prove, as the panel
/// needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// What identifies this question to `polkitd`. Opaque, and the one thing
    /// every answer has to carry back.
    pub cookie: String,
    /// What the authorisation is for, in the session's own language —
    /// `polkitd` has already translated it.
    pub message: String,
    /// The action being authorised, for the log. Not shown: it is a name for
    /// programs, and the sentence above is the one for people.
    pub action_id: String,
    /// Whose password will do.
    pub user: String,
    /// Whether that is the person sitting at the machine, which is what decides
    /// whether the panel says "your password" or names somebody.
    pub yourself: bool,
}

/// How a question ended, as the shell reports it back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// The helper said `SUCCESS`, and has already told `polkitd` so. Returning
    /// from the call is all that is left.
    Proved,
    /// The user closed the panel, or gave up. `polkitd` is told the question
    /// was not answered, which it reports to whoever asked as a refusal.
    Declined,
}

/// The agent, as the shell holds it.
///
/// Everything D-Bus is behind this: one thread owns the system bus connection
/// and answers `polkitd`, and the shell only ever sees [`Request`]s arriving
/// and [`Answer`]s going back. Dropping it lets the connection go, which is how
/// `polkitd` learns the agent is gone — it watches for the name disconnecting,
/// so a shell that crashes unregisters exactly as tidily as one that exits.
pub struct Agent {
    shared: Arc<Shared>,
}

/// What the two threads share: questions coming in, answers going out.
#[derive(Default)]
struct Shared {
    inbox: Mutex<Inbox>,
}

#[derive(Default)]
struct Inbox {
    /// Questions the shell has not put on screen yet, oldest first.
    ///
    /// A queue rather than a single slot: the shell asks one thing at a time,
    /// and a second authorisation arriving while a panel is up is made to wait
    /// rather than refused. Nobody is kept waiting by this who was not already
    /// waiting on the panel in front of them.
    waiting: Vec<Request>,
    /// Where to send the answer for each question that is still open.
    answering: HashMap<String, async_channel::Sender<Answer>>,
    /// Questions `polkitd` has withdrawn — the application that asked gave up,
    /// or somebody else answered it first. The panel comes down without being
    /// answered.
    withdrawn: Vec<String>,
}

impl Agent {
    /// Register with `polkitd` and start answering it.
    ///
    /// Returns `None` when this session cannot have an agent at all — no system
    /// bus, no `polkitd`, or a session that already has one — after saying why.
    /// That is a session where authorisations are refused rather than asked
    /// about, which is exactly what happens today and is never worth failing to
    /// start the shell over.
    pub fn start() -> Option<Agent> {
        let shared = Arc::new(Shared::default());
        let listener = Listener {
            shared: Arc::clone(&shared),
        };

        // Everything up to and including registration happens here, on the
        // shell's own thread, so that a failure is reported once, plainly, at
        // startup — a worker that failed quietly would leave a session that
        // looks like it has an agent and has not.
        let connection = match connect(listener) {
            Ok(connection) => connection,
            Err(err) => {
                tracing::info!(%err, "no polkit agent: could not reach the system bus");
                return None;
            }
        };
        // The session first and this process second. The second is not a
        // consolation prize: it is what a machine with no `logind` gets, and
        // what a LineXinBar started inside somebody else's desktop gets, since
        // `polkitd` allows one agent per session and that desktop's own got
        // there first.
        let mut registered = None;
        for subject in subjects(&connection) {
            match register(&connection, &subject) {
                Ok(()) => {
                    registered = Some(subject);
                    break;
                }
                Err(err) => {
                    tracing::info!(%err, ?subject, "polkitd would not take an agent for this")
                }
            }
        }
        let Some(subject) = registered else {
            tracing::info!("no polkit agent: this session cannot register one");
            return None;
        };
        tracing::info!(?subject, "registered as this session's polkit agent");

        // The connection has to outlive this call and there is nothing else to
        // hold it: its own executor answers `polkitd` on a thread of its own,
        // and this one exists only so that the connection is not dropped.
        std::thread::Builder::new()
            .name("lxb-polkit".to_string())
            .spawn(move || {
                let _connection = connection;
                loop {
                    std::thread::park();
                }
            })
            .ok()?;

        Some(Agent { shared })
    }

    /// The next question to put on screen, if there is one waiting.
    pub fn asked(&self) -> Option<Request> {
        let mut inbox = self.shared.inbox.lock().ok()?;
        if inbox.waiting.is_empty() {
            return None;
        }
        Some(inbox.waiting.remove(0))
    }

    /// The questions `polkitd` has taken back since this was last asked.
    pub fn withdrawn(&self) -> Vec<String> {
        match self.shared.inbox.lock() {
            Ok(mut inbox) => std::mem::take(&mut inbox.withdrawn),
            Err(_) => Vec::new(),
        }
    }

    /// Answer one, which is what lets the application that started all this
    /// carry on.
    pub fn answer(&self, cookie: &str, answer: Answer) {
        let Ok(mut inbox) = self.shared.inbox.lock() else {
            return;
        };
        inbox.waiting.retain(|request| request.cookie != cookie);
        if let Some(sender) = inbox.answering.remove(cookie) {
            // Never blocks: the channel holds one and is used once.
            let _ = sender.try_send(answer);
        }
    }
}

/// The object `polkitd` calls. One method to ask, one to take it back.
struct Listener {
    shared: Arc<Shared>,
}

/// What a refused question is answered with.
///
/// polkit's own error names, because that is what every other agent returns and
/// what `polkitd` logs. It does not act on which one it is — anything other
/// than a plain return means the user was not proved to be who they said — but
/// a journal that says "cancelled" where the user pressed Cancel is worth the
/// two lines.
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.freedesktop.PolicyKit1.Error")]
enum Refused {
    Cancelled(String),
    Failed(String),
}

#[zbus::interface(name = "org.freedesktop.PolicyKit1.AuthenticationAgent")]
impl Listener {
    /// Collect proof for one action, and do not return until there is some or
    /// the user has given up.
    ///
    /// Returning is the answer: a plain return means the helper has already
    /// told `polkitd` the password was right, and an error means it has not.
    /// That is why this call is held open for as long as the panel is up —
    /// answering early would be answering "no".
    #[allow(clippy::too_many_arguments)]
    async fn begin_authentication(
        &self,
        action_id: String,
        message: String,
        icon_name: String,
        details: HashMap<String, String>,
        cookie: String,
        identities: Vec<(String, HashMap<String, OwnedValue>)>,
    ) -> Result<(), Refused> {
        // Neither is used: the icon on the panel is the shell's own — see
        // `crate::icons::AUTHENTICATE` for why a themed name is not welcome on
        // a panel that asks for a password — and the details are keys for
        // programs, next to a message polkitd has already written for people.
        let _ = (icon_name, details);

        let Some((user, yourself)) = whose_password(&identities) else {
            tracing::warn!(action_id, "polkitd asked for a password from nobody");
            return Err(Refused::Failed("no identity to authenticate".to_string()));
        };
        tracing::info!(action_id, user, "polkitd is asking for authentication");

        let (sender, receiver) = async_channel::bounded(1);
        {
            let Ok(mut inbox) = self.shared.inbox.lock() else {
                return Err(Refused::Failed("the shell lost its inbox".to_string()));
            };
            inbox.answering.insert(cookie.clone(), sender);
            inbox.waiting.push(Request {
                cookie: cookie.clone(),
                message,
                action_id,
                user,
                yourself,
            });
        }

        // And now nothing happens here until the shell has finished asking.
        // zbus runs each call on a task of its own, so a panel that stands for
        // a minute does not stop `CancelAuthentication` — or a second question
        // — from arriving in the meantime.
        match receiver.recv().await {
            Ok(Answer::Proved) => Ok(()),
            Ok(Answer::Declined) => Err(Refused::Cancelled("the user declined".to_string())),
            // The shell dropped the answer without sending one, which it does
            // only on its way out.
            Err(_) => Err(Refused::Failed("the shell is gone".to_string())),
        }
    }

    /// Take a question back. Whoever asked has given up, or somebody else has
    /// answered it.
    async fn cancel_authentication(&self, cookie: String) {
        tracing::info!(cookie, "polkitd withdrew a question");
        let Ok(mut inbox) = self.shared.inbox.lock() else {
            return;
        };
        inbox.waiting.retain(|request| request.cookie != cookie);
        if let Some(sender) = inbox.answering.remove(&cookie) {
            let _ = sender.try_send(Answer::Declined);
        }
        // Told to the shell as well, because the panel may be on screen and a
        // question nobody is waiting for an answer to must not stay up.
        inbox.withdrawn.push(cookie);
    }
}

/// Open the system bus with the agent's object already on it.
fn connect(listener: Listener) -> zbus::Result<zbus::blocking::Connection> {
    zbus::blocking::connection::Builder::system()?
        .serve_at(AGENT_PATH, listener)?
        .build()
}

/// Tell `polkitd` this session has an agent, and where it is.
fn register(connection: &zbus::blocking::Connection, subject: &Subject) -> zbus::Result<()> {
    connection
        .call_method(
            Some(AUTHORITY_NAME),
            AUTHORITY_PATH,
            Some(AUTHORITY_INTERFACE),
            "RegisterAuthenticationAgent",
            &(subject.as_value(), locale(), AGENT_PATH),
        )
        .map(|_| ())
}

/// What the agent is registered *for*: this login session, or failing that this
/// process.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Subject {
    /// The whole login session, which is what an agent is normally for: every
    /// program the user starts is inside it, so every one of them finds this
    /// agent.
    Session(String),
    /// Only this process and what it asks for itself.
    ///
    /// The fallback, and it covers two real cases: a machine with no `logind`
    /// at all, and a LineXinBar started *inside* somebody else's desktop, whose
    /// session already has an agent — `polkitd` allows exactly one per session,
    /// and the one that got there first keeps it.
    Process { pid: u32, started: u64 },
}

impl Subject {
    fn as_value(&self) -> (&str, HashMap<&str, Value<'_>>) {
        match self {
            Subject::Session(id) => (
                "unix-session",
                HashMap::from([("session-id", Value::from(id.as_str()))]),
            ),
            Subject::Process { pid, started } => (
                "unix-process",
                HashMap::from([
                    ("pid", Value::from(*pid)),
                    ("start-time", Value::from(*started)),
                ]),
            ),
        }
    }
}

/// What to offer `polkitd`, best first.
///
/// The session's own identifier, from the environment where `logind` put it or
/// from `logind` itself when the environment does not say — a shell started by
/// something that did not pass `XDG_SESSION_ID` on is still in a session, and
/// asking is one call on a bus that is already open. Then this process, which
/// is always worth trying and never wrong: an agent registered for it is an
/// agent for everything the shell asks on its own behalf.
fn subjects(connection: &zbus::blocking::Connection) -> Vec<Subject> {
    let mut subjects = Vec::new();
    let session = std::env::var("XDG_SESSION_ID")
        .ok()
        .filter(|id| !id.trim().is_empty())
        .or_else(|| session_from_logind(connection));
    if let Some(id) = session {
        subjects.push(Subject::Session(id));
    }
    subjects.push(Subject::Process {
        pid: std::process::id(),
        started: started_at(std::process::id()).unwrap_or(0),
    });
    subjects
}

/// Ask `logind` which session this process is in.
fn session_from_logind(connection: &zbus::blocking::Connection) -> Option<String> {
    let session: zbus::zvariant::OwnedObjectPath = connection
        .call_method(
            Some("org.freedesktop.login1"),
            "/org/freedesktop/login1",
            Some("org.freedesktop.login1.Manager"),
            "GetSessionByPID",
            &(std::process::id(),),
        )
        .ok()?
        .body()
        .deserialize()
        .ok()?;
    let id: String = connection
        .call_method(
            Some("org.freedesktop.login1"),
            session.as_str(),
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.freedesktop.login1.Session", "Id"),
        )
        .ok()?
        .body()
        .deserialize::<Value<'_>>()
        .ok()?
        .downcast_ref::<String>()
        .ok()?;
    Some(id)
}

/// When a process started, in clock ticks since the machine booted.
///
/// Half of what names a process to polkit: a bare pid is a name that can be
/// reused, and the pair cannot be. Field 22 of `/proc/N/stat`, counted from the
/// last `)` because a program is free to have spaces and brackets in its name
/// and everything before that bracket is unparseable.
fn started_at(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_name = stat.rsplit_once(')')?.1;
    // Field 3 is the state, so the start time is the twentieth after the name.
    after_name.split_whitespace().nth(19)?.parse().ok()
}

/// The language `polkitd` should write its message in.
fn locale() -> String {
    for name in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(value) = std::env::var(name) {
            if !value.trim().is_empty() {
                return value;
            }
        }
    }
    "C".to_string()
}

/// Whose password to ask for, out of everyone `polkitd` says would do, and
/// whether that is the person at the machine.
///
/// The user themselves whenever they are on the list, which is both the kindest
/// answer and the likeliest to succeed — somebody who is in `wheel` should be
/// asked for their own password, not for root's. Otherwise the first identity
/// offered, which is the order the machine's admin rule put them in.
///
/// Groups are skipped rather than expanded. `polkitd` sends the users a group
/// stands for, so one arriving here would be a machine whose admin rule names a
/// group with nobody in it, and a group is not something a password belongs to.
fn whose_password(identities: &[(String, HashMap<String, OwnedValue>)]) -> Option<(String, bool)> {
    let mine = unsafe { libc::getuid() };
    let mut users = identities.iter().filter_map(|(kind, fields)| {
        (kind == "unix-user")
            .then(|| fields.get("uid")?.downcast_ref::<u32>().ok())
            .flatten()
    });
    let uid = users.clone().find(|uid| *uid == mine).or_else(|| {
        let first = users.next();
        if first.is_none() {
            tracing::warn!(?identities, "no user among the identities polkitd offered");
        }
        first
    })?;
    Some((user_name(uid)?, uid == mine))
}

/// What the machine calls the user with this id.
///
/// Through `getpwuid_r` rather than by reading `/etc/passwd`, because an
/// account can come from anywhere the machine's name service is pointed at, and
/// the helper is going to look it up the same way.
fn user_name(uid: u32) -> Option<String> {
    let mut buffer = vec![0i8; 4096];
    let mut passwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: `passwd` and `buffer` outlive the call, and `found` is only read
    // when the call reports success.
    let rc = unsafe {
        libc::getpwuid_r(
            uid as libc::uid_t,
            &mut passwd,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut found,
        )
    };
    if rc != 0 || found.is_null() {
        tracing::warn!(uid, "no account on this machine has that id");
        return None;
    }
    // SAFETY: a successful `getpwuid_r` leaves `pw_name` pointing into
    // `buffer`, which is still alive.
    let name = unsafe { std::ffi::CStr::from_ptr(passwd.pw_name) };
    name.to_str().ok().map(str::to_string)
}

/// What the helper has said that the shell has not drawn yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Said {
    /// PAM wants something typed. `echo` is whether it asked for it to be
    /// shown — see [`Session`] for what the shell does about that.
    Asked { prompt: String, echo: bool },
    /// PAM has something to say, and wants nothing back.
    Told(String),
    /// PAM is complaining. The same thing as far as the panel is concerned,
    /// kept apart from [`Said::Told`] because one of them is worth a line in
    /// the log.
    Complained(String),
    /// It is over. `true` means the helper has already told `polkitd` that the
    /// password was right.
    Finished(bool),
}

/// One conversation with polkit's PAM helper.
///
/// Held by the shell for as long as the panel is up. Everything blocking — the
/// connection, the reading, and the waiting for a password that may never be
/// typed — is on a thread of its own, and what crosses back is [`Said`].
///
/// Dropping it ends the conversation wherever it had got to: the socket is shut
/// down, or the helper is killed, and PAM is left without the answer it was
/// waiting for. That is what Cancel does, and what a question `polkitd` takes
/// back does.
pub struct Session {
    said: Arc<Mutex<Vec<Said>>>,
    answers: Sender<Secret>,
    stop: Arc<Mutex<Option<Stop>>>,
}

/// The end of the conversation the shell keeps hold of, so that walking away
/// from the panel really does end it.
enum Stop {
    Socket(UnixStream),
    Program(Child),
}

impl Session {
    /// Start proving that `user` is who they say, for the question `cookie`
    /// names.
    pub fn start(user: &str, cookie: &str) -> Session {
        let said = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(Mutex::new(None));
        let (sender, answers) = std::sync::mpsc::channel();

        let into = Arc::clone(&said);
        let keep = Arc::clone(&stop);
        let user = user.to_string();
        let cookie = cookie.to_string();
        std::thread::spawn(move || {
            let outcome = converse(&user, &cookie, &answers, &into, &keep);
            if let Err(why) = &outcome {
                tracing::warn!(why, "the polkit helper could not be talked to");
            }
            // Whatever happened, the panel is told it is over exactly once. A
            // conversation that ended without a verdict is a "no": nothing has
            // been proved.
            let ended = matches!(outcome, Ok(true));
            if let Ok(mut said) = into.lock() {
                if !matches!(said.last(), Some(Said::Finished(_))) {
                    said.push(Said::Finished(ended));
                }
            }
        });

        Session {
            said,
            answers: sender,
            stop,
        }
    }

    /// Everything the helper has said since this was last asked.
    pub fn said(&self) -> Vec<Said> {
        match self.said.lock() {
            Ok(mut said) => std::mem::take(&mut *said),
            // A worker that panicked will never say anything again, and a panel
            // waiting on it for ever is worse than being told it went wrong.
            Err(_) => vec![Said::Finished(false)],
        }
    }

    /// Hand over what was typed. It goes to the helper on the worker thread and
    /// is dropped there.
    pub fn answer(&self, password: Secret) {
        // A failed send drops the password, which zeroes it — the worker is
        // gone, and there is nothing to hand it to.
        let _ = self.answers.send(password);
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // The worker is almost certainly blocked reading, and dropping the
        // channel alone would not wake it. Taking the socket or the helper out
        // from under it does.
        if let Ok(mut stop) = self.stop.lock() {
            match stop.take() {
                Some(Stop::Socket(socket)) => {
                    let _ = socket.shutdown(Shutdown::Both);
                }
                Some(Stop::Program(mut child)) => {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                None => {}
            }
        }
    }
}

/// The whole conversation, on the worker thread. `Ok(true)` means the helper
/// said `SUCCESS`.
fn converse(
    user: &str,
    cookie: &str,
    answers: &Receiver<Secret>,
    said: &Mutex<Vec<Said>>,
    keep: &Mutex<Option<Stop>>,
) -> Result<bool, String> {
    let (mut reader, mut writer) = open_helper(user, keep)?;
    // The cookie proves this conversation is about the question `polkitd`
    // asked. It goes down the same channel as the password and never onto a
    // command line — see `read_cookie` in polkit's own helper for the history
    // there.
    writeln!(writer, "{cookie}").map_err(|err| err.to_string())?;
    writer.flush().map_err(|err| err.to_string())?;

    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            // The helper closed without a verdict: it was killed, or the
            // connection went. Not an error to report — Cancel takes this path
            // every time.
            Ok(0) => return Ok(false),
            Ok(_) => {}
            Err(err) => return Err(err.to_string()),
        }
        match parse(&line) {
            Some(Said::Finished(proved)) => return Ok(proved),
            Some(Said::Asked { prompt, echo }) => {
                push(said, Said::Asked { prompt, echo });
                // And here the worker waits, for as long as the user does.
                // Cancel closes the channel, which ends this.
                let Ok(password) = answers.recv() else {
                    return Ok(false);
                };
                password
                    .hand_to(&mut writer)
                    .map_err(|err| err.to_string())?;
                // Gone, well before the helper has decided anything about it.
                drop(password);
            }
            Some(told) => push(said, told),
            None => {
                tracing::warn!(said = %line.trim(), "the polkit helper said something unfamiliar");
                return Ok(false);
            }
        }
    }
}

/// The two ends of a conversation with the helper: what it says, and what it is
/// told. Boxed because the two ways of reaching it are a socket and a pair of
/// pipes, and nothing past this point cares which.
type Helper = (Box<dyn BufRead + Send>, Box<dyn Write + Send>);

/// Reach the helper, whichever of the two ways this machine has, and hand it
/// the name of the user to authenticate.
///
/// The socket first, because a machine that has it has no setuid helper to fall
/// back on — and one that has both is a machine where the socket is the way
/// polkit's own library would go.
fn open_helper(user: &str, keep: &Mutex<Option<Stop>>) -> Result<Helper, String> {
    if Path::new(HELPER_SOCKET).exists() {
        match UnixStream::connect(HELPER_SOCKET) {
            Ok(socket) => {
                let reading = socket.try_clone().map_err(|err| err.to_string())?;
                let mut writing = socket.try_clone().map_err(|err| err.to_string())?;
                // The name goes first: with no command line to put it on, it is
                // the helper's first line of input.
                writeln!(writing, "{user}").map_err(|err| err.to_string())?;
                if let Ok(mut keep) = keep.lock() {
                    *keep = Some(Stop::Socket(socket));
                }
                return Ok((Box::new(BufReader::new(reading)), Box::new(writing)));
            }
            Err(err) => {
                // Worth saying rather than falling through silently: a socket
                // that is there and refuses connections is a broken machine,
                // not an old one.
                tracing::warn!(?err, "could not reach the polkit helper's socket");
            }
        }
    }

    let program = HELPER_PROGRAMS
        .iter()
        .find(|path| Path::new(path).exists())
        .ok_or_else(|| "polkit's authentication helper is not installed".to_string())?;
    let mut child = Command::new(program)
        .arg(user)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| err.to_string())?;
    let reading = child.stdout.take().ok_or("the helper has no output")?;
    let writing = child.stdin.take().ok_or("the helper takes no input")?;
    if let Ok(mut keep) = keep.lock() {
        *keep = Some(Stop::Program(child));
    }
    Ok((Box::new(BufReader::new(reading)), Box::new(writing)))
}

fn push(said: &Mutex<Vec<Said>>, what: Said) {
    if let Ok(mut said) = said.lock() {
        said.push(what);
    }
}

/// Read one line of what the helper said.
///
/// `None` is a line this does not recognise, which ends the conversation: the
/// helper's vocabulary is five words long and something outside it means the
/// two ends disagree about what they are speaking.
fn parse(line: &str) -> Option<Said> {
    let line = unescape(line.trim_end_matches(['\n', '\r']));
    // The keyword and the text are separated by exactly one space, and the text
    // may be empty — the helper writes the space whether or not PAM gave it
    // anything to say.
    let (keyword, text) = match line.split_once(' ') {
        Some((keyword, text)) => (keyword, text),
        None => (line.as_str(), ""),
    };
    match keyword {
        "PAM_PROMPT_ECHO_OFF" => Some(Said::Asked {
            prompt: text.to_string(),
            echo: false,
        }),
        "PAM_PROMPT_ECHO_ON" => Some(Said::Asked {
            prompt: text.to_string(),
            echo: true,
        }),
        "PAM_TEXT_INFO" => Some(Said::Told(text.to_string())),
        "PAM_ERROR_MSG" => Some(Said::Complained(text.to_string())),
        "SUCCESS" => Some(Said::Finished(true)),
        "FAILURE" => Some(Said::Finished(false)),
        _ => None,
    }
}

/// Undo the escaping the helper writes its text with.
///
/// GLib's `g_strescape`, which is what is on the other end, turns everything
/// outside printable ASCII into `\ooo` and the usual handful of control
/// characters into their letters. A prompt in a language that is not English
/// arrives entirely as octal, so this is not decoration: without it the panel
/// would show `Mot de passe` as a row of backslashes.
fn unescape(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'\\' || index + 1 == bytes.len() {
            out.push(bytes[index]);
            index += 1;
            continue;
        }
        index += 1;
        let escape = bytes[index];
        index += 1;
        match escape {
            b'b' => out.push(0x08),
            b'f' => out.push(0x0c),
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'v' => out.push(0x0b),
            b'0'..=b'7' => {
                // Up to three octal digits, the first of which is in hand.
                let mut value = u32::from(escape - b'0');
                for _ in 0..2 {
                    match bytes.get(index) {
                        Some(digit @ b'0'..=b'7') => {
                            value = value * 8 + u32::from(digit - b'0');
                            index += 1;
                        }
                        _ => break,
                    }
                }
                out.push(value as u8);
            }
            // A backslash, a quote, or something nothing escapes: the character
            // itself, which is what GLib's own `g_strcompress` does with it.
            other => out.push(other),
        }
    }
    // Lossy, and deliberately: a helper on a machine with a mangled locale can
    // produce bytes that are not a string, and a panel with a replacement
    // character in it is better than a panel that says nothing.
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(kind: &str, field: &str, value: u32) -> (String, HashMap<String, OwnedValue>) {
        (
            kind.to_string(),
            HashMap::from([(
                field.to_string(),
                Value::from(value)
                    .try_to_owned()
                    .expect("a number owns itself"),
            )]),
        )
    }

    /// The user themselves whenever they are on the list, wherever they are on
    /// it: somebody in `wheel` is asked for their own password rather than for
    /// root's.
    #[test]
    fn the_user_themselves_is_asked_before_anybody_else() {
        let mine = unsafe { libc::getuid() };
        let offered = vec![
            identity("unix-user", "uid", 0),
            identity("unix-user", "uid", mine),
        ];
        let (name, yourself) = whose_password(&offered).expect("somebody was offered");
        assert!(yourself);
        assert_eq!(Some(name), user_name(mine));
    }

    /// And the first one otherwise, with the panel told it is not about them.
    #[test]
    fn somebody_else_is_named_rather_than_assumed_to_be_you() {
        let root = vec![identity("unix-user", "uid", 0)];
        let (name, yourself) = whose_password(&root).expect("root was offered");
        assert_eq!(name, "root");
        assert_eq!(yourself, unsafe { libc::getuid() } == 0);
    }

    /// A group is not something a password belongs to, and an empty list is not
    /// somebody to ask.
    #[test]
    fn a_question_with_nobody_to_ask_is_not_asked() {
        assert_eq!(whose_password(&[]), None);
        assert_eq!(whose_password(&[identity("unix-group", "gid", 998)]), None);
    }

    /// The five words the helper speaks, and the trailing space that is part of
    /// the line rather than part of the prompt.
    #[test]
    fn every_line_the_helper_speaks_is_understood() {
        assert_eq!(
            parse("PAM_PROMPT_ECHO_OFF Password: \n"),
            Some(Said::Asked {
                prompt: "Password: ".to_string(),
                echo: false,
            })
        );
        assert_eq!(
            parse("PAM_PROMPT_ECHO_ON One-time code: \n"),
            Some(Said::Asked {
                prompt: "One-time code: ".to_string(),
                echo: true,
            })
        );
        assert_eq!(
            parse("PAM_TEXT_INFO Place your finger on the reader\n"),
            Some(Said::Told("Place your finger on the reader".to_string()))
        );
        assert_eq!(
            parse("PAM_ERROR_MSG Account locked\n"),
            Some(Said::Complained("Account locked".to_string()))
        );
        assert_eq!(parse("SUCCESS\n"), Some(Said::Finished(true)));
        assert_eq!(parse("FAILURE\n"), Some(Said::Finished(false)));

        // A prompt PAM gave no text for is still a prompt, and anything outside
        // the vocabulary is not.
        assert_eq!(
            parse("PAM_PROMPT_ECHO_OFF \n"),
            Some(Said::Asked {
                prompt: String::new(),
                echo: false,
            })
        );
        assert_eq!(parse("what?\n"), None);
        assert_eq!(parse("\n"), None);
    }

    /// The escaping is not decoration: a prompt in any language but English
    /// arrives as octal, and a panel that showed the octal would be unreadable.
    #[test]
    fn a_prompt_comes_back_out_of_its_escaping() {
        // `g_strescape ("Mot de passe : ")` leaves the ASCII alone; the accents
        // and the box-drawing of a real prompt do not survive it.
        assert_eq!(
            unescape("Passwort f\\303\\274r root: "),
            "Passwort für root: "
        );
        assert_eq!(unescape("a\\tb\\nc"), "a\tb\nc");
        assert_eq!(
            unescape("back\\\\slash and \\\"quotes\\\""),
            "back\\slash and \"quotes\""
        );
        // A trailing backslash is a line that was cut off, not an escape.
        assert_eq!(unescape("half\\"), "half\\");
        // Octal runs at most three digits, so the character after one is text.
        assert_eq!(unescape("\\1011"), "A1");
        assert_eq!(unescape("nothing to undo"), "nothing to undo");
    }

    /// Whichever way the helper is reached, the panel must not be left waiting
    /// on a machine that has neither.
    #[test]
    fn a_machine_with_no_helper_at_all_is_a_refusal_rather_than_a_wait() {
        let has_one = Path::new(HELPER_SOCKET).exists()
            || HELPER_PROGRAMS.iter().any(|path| Path::new(path).exists());
        let keep = Mutex::new(None);
        match open_helper("nobody", &keep) {
            Ok(_) => assert!(has_one, "a helper was opened on a machine with none"),
            Err(why) => assert!(!why.is_empty()),
        }
    }

    /// The subject registration falls back rather than failing: a session with
    /// no `logind` still gets an agent for the shell's own process.
    #[test]
    fn a_process_subject_names_the_process_and_when_it_started() {
        let subject = Subject::Process {
            pid: std::process::id(),
            started: started_at(std::process::id()).expect("this process has a stat"),
        };
        let (kind, fields) = subject.as_value();
        assert_eq!(kind, "unix-process");
        assert_eq!(fields.get("pid"), Some(&Value::from(std::process::id())));
        assert!(matches!(fields.get("start-time"), Some(Value::U64(ticks)) if *ticks > 0));
    }
}
