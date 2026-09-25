//! The client's own JS context, and the calls this shell makes into it.
//!
//! ## Why there is a browser in here
//!
//! Valve's client is a web application in a trench coat: everything above
//! `steamclient.so` — the library, the store, the settings, and the login
//! screen — is JavaScript running in an embedded Chromium, and the C++ half is
//! reached from it through a `SteamClient` object bound into that page.
//!
//! That matters here because of one method. When somebody signs in to Valve's
//! client, its login screen does exactly what [`crate::auth`] does: it asks
//! `IAuthenticationService` for a session, waits for the phone or the code, and
//! is handed a refresh token. Then it says:
//!
//! ```js
//! SteamClient.Auth.SetLoginToken(strRefreshToken, strAccountName)
//! ```
//!
//! and the client takes it from there — persisting it in whatever private form
//! it likes, and signing itself in. This shell holds the same kind of token,
//! for the same account, got the same way. So rather than reverse-engineer
//! where the client puts it afterwards, this makes that one call.
//!
//! It is worth being plain about why the alternative was abandoned. The client
//! keeps the credential in `local.vdf` under `ConnectCache`, and on Linux the
//! value there is the token in the clear — that much is settled, and Valve's
//! own strings give it away, since the client recognises a modern token by
//! sniffing for the base64 of `{ "typ": "JWT",`. What is *not* settled is the
//! key that entry is filed under. Ten derivations were tried against a real
//! token on a real client and every one of them produced the same line in the
//! client's log: `cached creds not available`. Writing to a private format
//! nobody has published would be a guess that breaks whenever Valve changes it;
//! calling the method its own login screen calls is not.
//!
//! ## What this costs
//!
//! The JS context is only reachable when the client has been told to expose it,
//! which is a file in the Steam directory — `.cef-enable-remote-debugging` —
//! and a port on the loopback interface.
//!
//! [`expose`] makes that file, and the shell does it rather than asking anybody
//! to. It was a line in the README for a while and that was the wrong shape for
//! this session: somebody turning a console on has no terminal to type it into,
//! and a sign-in that refuses until they have found one is a sign-in that does
//! not work. There is no command line switch to use instead — `steamui.so`
//! tests for the file and nothing else — so making it is the whole of the way
//! in.
//!
//! What it costs is worth saying plainly. While the client runs exposed it
//! listens on `127.0.0.1:8080`, and any program running as this user can drive
//! the client's interface through it; nothing is opened to the network.
//!
//! Which is why it is kept off unless it is wanted, and most of the time it is
//! not wanted. Valve's client keeps its own credential once it has been handed
//! one, and comes back up signed in by itself — verified against a client
//! started with no marker and the port shut, which logged itself on from the
//! JWT it had persisted. So the ordinary session starts it plainly and never
//! exposes anything at all. Only two things reach for this: a client that will
//! not sign itself in — the first time, or after its own token has expired —
//! and moving a game on or off the disk, which are calls rather than URLs. See
//! [`crate::client::Need`], which is how a caller says which of those it is.
//!
//! The client reads it once, on the way up, and that cuts both ways. It is why
//! [`expose`] reports whether it had to make the file — a client already
//! running when it appeared read no marker, and [`crate::client::wake`] starts
//! that one again rather than waiting on a port that will never open. And it is
//! why [`withdraw`] can take the file away again the moment the client is up:
//! the port stays open for the life of that client either way, so the marker
//! only has to exist across the instant it starts, and a Steam somebody starts
//! for themselves next week comes up exposing nothing.

use std::time::{Duration, Instant};

/// One install wizard, one caller at a time.
///
/// Valve's client has exactly one of them. `OpenInstallWizard`,
/// `OpenUninstallWizard` and `CancelInstall` are not calls about a game, they
/// are calls about *the* wizard — and every one of the three flows below
/// registers for the same `RegisterForShowInstallWizard` events and cancels on
/// the way out.
///
/// Two of them at once is not a race that is hard to hit. Every one of these
/// runs on a thread of its own, each begins by waking a client that may take a
/// minute, and two rows pressed while that minute passes arrive here together:
/// each then sees the other's wizard states, and the first to fail or be asked
/// a question cancels the other's download.
///
/// Held across the whole of a flow rather than around each call, because what
/// has to be serialised is the flow. Steam downloads several games perfectly
/// well once their wizards are done; it is only the wizard that is single.
///
/// **Single per machine, that is, which is why this is not a `Mutex`.** The
/// wizard belongs to the user's one client, and a `static` here excluded the
/// threads of one process from each other while two sessions — or one session
/// and a `probe-install` run against it — drove and cancelled the same wizard
/// side by side. See [`crate::turns`].
const ONE_WIZARD_AT_A_TIME: crate::turns::What = crate::turns::What::TheWizard;

/// Take the wizard, check the client still has what this flow drives, and open
/// the socket — in that order, which is the whole point of it being one
/// function.
///
/// It used to be the same three lines at the top of each of the three flows.
/// The order between them is the substance and it is quiet when it is wrong:
/// the wizard is taken **first**, before the capability check and before the
/// socket, because what has to be serialised is the flow and not the calls in
/// it. A check made before the wizard is a check made against a client that
/// another flow is still in the middle of, and its answer is already out of
/// date by the time this one is allowed to act on it.
///
/// Nothing here drives the wizard. It waits, asks the client what it still has,
/// and opens a socket, which is why a test may call it on a live machine.
fn ready_to_drive_the_wizard(
    needed: &[&str],
    still: Still<'_>,
) -> Result<(crate::turns::Turn, String), Problem> {
    let turn = the_wizard(still.request);
    // **Asked here, on the far side of the wait.** The wizard is one per
    // machine and a flow may hold it for [`UNTIL_THE_WIZARD_ANSWERS`], so a
    // caller that checked its ground and then queued for this has a check as
    // old as somebody else's whole install. What moved in that time is exactly
    // what this is about: the account signed out, the Steam under it changed,
    // the client was replaced by one belonging to somebody else.
    still.again()?;
    can_be_driven(needed)?;
    let socket = the_client_it_proved(still)?;
    Ok((turn, socket))
}

/// Open the interface, and be sure it is the client this flow proved before
/// anything goes out on it.
///
/// The last two steps of every mutation here, in one place because they are one
/// step: a socket, bound to the client it is supposed to reach, asked in the
/// last instant before the expression that cannot be taken back.
fn the_client_it_proved(still: Still<'_>) -> Result<String, Problem> {
    let socket = context()?;
    still.reaches_the_client_it_proved(&socket)?;
    // And the ground again, because everything above is a round trip into the
    // client apiece and this is the last instant before the caller sends the
    // expression that cannot be taken back.
    still.again()?;
    Ok(socket)
}

/// What a flow asks, in the last moment before it acts, about the ground it is
/// standing on — and about the client on the other end of the socket it is
/// about to act through.
///
/// The ground is passed in rather than read here because none of it is this
/// module's to know. Which account, which Steam, which client process — all of
/// that lives with the caller, and what this module has is the one thing the
/// caller cannot have: the moment *after* the wizard has been waited for and
/// the socket opened, which is the only moment worth asking in.
///
/// ## Why the proof comes in with it
///
/// A ground check answers "is this session still acting for the account it was
/// asked for", and it answers that about the *session*. It says nothing at all
/// about the connection: the interface is a port on loopback, whoever is
/// holding it is whoever came up last, and a check that has just passed can be
/// followed by a call into a client that replaced the proved one and signed
/// itself into whatever it remembered. So the client that was proved is
/// carried in here too, and the socket is bound to it before it is used — see
/// [`Still::reaches_the_client_it_proved`].
///
/// Answering `Err` stops the flow before anything is driven, and what it
/// carries is the caller's own sentence for why.
#[derive(Clone, Copy)]
pub struct Still<'a> {
    ground: &'a dyn Ground,
    /// The client the caller proved, where it has one. `None` is a caller with
    /// nothing to bind to — see [`Still::whatever_happens`].
    proved: Option<crate::client::Proven>,
    /// The request number the caller was asked under, where it has one.
    ///
    /// Nothing here decides on it. It is carried so that the wizard's lock can
    /// say which press is holding it — a flow may hold the wizard for
    /// [`UNTIL_THE_WIZARD_ANSWERS`], and a session queueing behind that wants
    /// to be able to name what it is waiting for rather than to log that it is
    /// waiting. See [`crate::turns::Behalf`].
    request: Option<u64>,
}

/// What the caller can be asked, at any moment, about where it stands.
///
/// A trait rather than a closure because the answer is a job's own bookkeeping
/// — its account, its Steam, the client it proved — and a job hands itself in
/// here rather than building something that borrows itself.
pub trait Ground {
    /// Whether everything this was asked for is still what is there now. `Err`
    /// carries the caller's own sentence for what moved.
    fn about_to_act(&self) -> Result<(), String>;
}

/// The ground of a caller with none: read-only calls and tests.
struct WhateverHappens;

impl Ground for WhateverHappens {
    fn about_to_act(&self) -> Result<(), String> {
        Ok(())
    }
}

impl<'a> Still<'a> {
    /// What a background job stands on: its own ground, and the client it
    /// proved before it got here.
    pub fn standing_on(
        ground: &'a dyn Ground,
        proved: crate::client::Proven,
        request: u64,
    ) -> Still<'a> {
        Still {
            ground,
            proved: Some(proved),
            request: Some(request),
        }
    }

    /// A [`Still`] for a caller with no ground to lose.
    ///
    /// Used by the read-only calls, which cannot do anything that outlives
    /// being wrong, and by tests. Not a default argument, deliberately: every
    /// mutation in this module names what it is standing on, and the way to
    /// say "nothing" is to say it.
    pub fn whatever_happens() -> Still<'static> {
        Still {
            ground: &WhateverHappens,
            proved: None,
            request: None,
        }
    }

    /// The ground, asked again.
    fn again(&self) -> Result<(), Problem> {
        self.ground.about_to_act().map_err(Problem::Moved)
    }

    /// Whether the interface just opened is the client this flow proved.
    ///
    /// Two questions, and the order between them is what keeps a client Valve
    /// renamed something in from being refused for it:
    ///
    /// 1. **Ask the connection who it is signed in as.** This is the whole
    ///    question asked of the one thing that matters — the context the call
    ///    is about to run in — and an answer that names another account is a
    ///    replacement client caught in the act.
    /// 2. **Ask `/proc` who is holding the port**, and only when the first
    ///    said nothing. `m_CurrentUser` is Valve's own internals and promised
    ///    to nobody; a rename of it must not stop every install on the
    ///    machine, so it is read as evidence and never as a requirement.
    ///
    /// Both refuse on evidence and neither refuses on silence. A client that
    /// will not say who it is, listening on a port whose owner cannot be read,
    /// is the state this shell was in before any of this existed: the ground
    /// check is then the whole of what stands behind the call, which is what
    /// it always was.
    fn reaches_the_client_it_proved(&self, socket: &str) -> Result<(), Problem> {
        let Some(proved) = self.proved else {
            return Ok(());
        };
        match signed_in_over(socket) {
            Some(account) if account == proved.account => return Ok(()),
            Some(account) => {
                tracing::warn!(
                    proved = proved.account,
                    said = account,
                    "the client on the other end of this connection is signed in to another account"
                );
                return Err(Problem::Moved(
                    "The Steam this was asked of is signed in to another account now.".to_string(),
                ));
            }
            None => {}
        }
        held_by(PORT, proved.pid)
    }
}

/// Whether the process holding that port is the proved client, or one of its
/// family.
///
/// `Ok` for every way of not knowing, and there are two of them: nobody could
/// be found on the port — a socket that closed while the table was being read,
/// an owner belonging to another user — and no pid was proved in the first
/// place, which is [`crate::client::Proven`]'s own "could not find out". Only
/// a holder that was found and is somebody else refuses.
fn held_by(port: u16, client: Option<u32>) -> Result<(), Problem> {
    let holders = crate::process::listening_on(port);
    if holders.is_empty() {
        tracing::info!(
            port,
            "nothing on this machine says who is holding Steam's interface"
        );
        return Ok(());
    }
    let Some(client) = client else {
        return Ok(());
    };
    if holders
        .iter()
        .any(|holder| crate::process::one_family(*holder, client))
    {
        return Ok(());
    }
    tracing::warn!(
        ?holders,
        client,
        port,
        "Steam's interface is held by a process that is not the client this was proved against"
    );
    Err(Problem::Moved(
        "The Steam this was asked of was replaced before it could act.".to_string(),
    ))
}

/// Which account the client on the other end of this socket is signed in as,
/// if it will say.
///
/// `None` for every way of not saying — the object is not there under either
/// of its names, the expression threw, the client answered something that is
/// not a number — because all of them mean the same thing to the only caller:
/// this said nothing, ask something else.
///
/// The id is a 64-bit SteamID and what is compared is its low half, which is
/// the account number [`crate::client::Proven`] holds and the one the client's
/// own log writes as `[U:1:…]`.
fn signed_in_over(socket: &str) -> Option<u32> {
    let said = evaluate(socket, WHO_IT_IS).ok()?;
    let id = said.trim().parse::<u64>().ok()?;
    (id != 0).then_some((id & u64::from(u32::MAX)) as u32)
}

/// Who the client says it is signed in as.
///
/// Both names it has gone under, and a string either way: an expression that
/// answered `undefined` is [`Problem::Refused`] rather than an answer, and
/// this one has to be able to say "nothing" without that being a failure.
///
/// Asked of a live client and confirmed: on build `1788400362` this answers a
/// 64-bit id whose low half is exactly the account the client's own connection
/// log writes as `[U:1:…]`, which is the number [`crate::client::Proven`]
/// carries.
///
/// **Read as evidence all the same, and never as a requirement.** `SteamClient`
/// and everything beside it is Valve's own internals, promised to nobody and
/// renamed whenever it suits: what is confirmed is the build that was running
/// the day this was written, not the one somebody is holding. A name that has
/// moved costs the account half of the binding and nothing else — the port's
/// owner is asked next, and the ground was asked before either.
const WHO_IT_IS: &str = "(() => { \
     const user = window.App?.m_CurrentUser ?? window.g_App?.m_CurrentUser; \
     return String(user?.strSteamID ?? ''); \
   })()";

/// Take the wizard, waiting for whoever has it.
fn the_wizard(request: Option<u64>) -> crate::turns::Turn {
    crate::turns::take(
        ONE_WIZARD_AT_A_TIME,
        crate::turns::Behalf {
            // Which Steam is not this module's to know — it holds a socket, not
            // a backend — and the lease is diagnostic, so it says what it has.
            backend: None,
            request,
        },
    )
}

/// Where the client exposes its JS context. Chromium's default, which is what
/// the client uses; it is not configurable from the client's side.
const PORT: u16 = 8080;

/// The page the `SteamClient` object lives in.
///
/// Not the login window and not the library: those come and go with what the
/// user is looking at, and with `-silent` there may be no window at all. This
/// one is the client's own always-present context, which is exactly why the
/// call is made here.
const CONTEXT: &str = "SharedJSContext";

/// How long any one step may take. Generous for a machine that has just
/// started a large client, and short enough that a shell waiting on this does
/// not appear to have hung.
const PATIENCE: Duration = Duration::from_secs(20);

/// How long the install flow may take to reach an answer.
///
/// Longer than [`PATIENCE`] because this one is not a call and a reply: the
/// flow is a state machine the client walks, and the first step of it asks
/// Steam for the app's own information over the network. Everything after that
/// is local and immediate.
pub const UNTIL_THE_WIZARD_ANSWERS: Duration = Duration::from_secs(45);

/// How long to wait for a client that has just signed in to learn what the
/// game being asked for actually *is* — see [`knows_the_game`].
///
/// Measured rather than guessed: on a cold client on this machine the wait was
/// 1.3 seconds. This is long enough to cover a slow one and short enough that a
/// press which is never going to work does not sit there for a minute.
const UNTIL_IT_KNOWS_THE_GAME: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// The client is not exposing its JS context. Nothing for the user to do
    /// about it — the shell makes the marker itself and starts the client
    /// again if it had to — so this now means the port was not listening
    /// anyway, which is either a client still on its way up or one that has
    /// stopped honouring the file.
    NotExposed,
    /// It is exposed, but the context this call belongs in was not among the
    /// pages it listed.
    NoContext,
    /// The connection to it failed, or it said something unexpected.
    Unreachable(String),
    /// The context is there and the call was not made: the expression threw
    /// before the client could answer it.
    ///
    /// Which nearly always means one thing. A page is listed by the debugger
    /// the moment it exists, and `SteamClient` is bound into it some time
    /// after that, so a client still building itself answers a reach for one
    /// of its methods with a `TypeError` rather than with a refusal. Kept
    /// apart from [`Problem::Refused`] because the two want opposite
    /// treatment: this one is worth trying again in a second, and a refusal
    /// will say exactly the same thing however long it is left.
    NotReady(String),
    /// The call was made and the client refused it. Carries what the client
    /// said, which is Valve's own wording for why a token was not accepted.
    Refused(String),
    /// The install flow ran and did not finish. Carries Valve's own wording for
    /// why, which reaches the user unchanged.
    ///
    /// Apart from [`Problem::Refused`] because that one is about a credential
    /// and this one is about a game: a refused token and a download that would
    /// not start have nothing to say to each other, and putting both through
    /// one sentence is how "Steam would not take the credential" came to be
    /// printed over a failed install.
    Stopped(String),
    /// The running client no longer has the methods this shell drives it with.
    /// Carries the ones that have gone.
    ///
    /// `SteamClient` is Valve's own internals and is promised to nobody, so a
    /// client update may rename or remove any of it. Kept apart from every
    /// other problem here because the answer is different: there is nothing to
    /// try again and nothing wrong with the game or the machine. This shell
    /// cannot take the step any more, and Steam's own window still can — so the
    /// press falls back to Steam's window rather than reporting a failure the
    /// user could do nothing about, and rather than the silence it used to be.
    Renamed(Vec<String>),
    /// The client will not go on without the person it is acting for: an
    /// agreement to accept, a product key to type. Carries what it is waiting
    /// for.
    ///
    /// Not a failure, and kept apart from one for that reason. Nothing has
    /// gone wrong and nothing needs fixing — there is a question this shell
    /// has no business answering on somebody's behalf, and the only place it
    /// can be asked is Steam's own window. See [`crate::Doing::Install`],
    /// which is what the shell offers instead.
    Asks(String),
    /// The client will not go on until these agreements are accepted, and it
    /// has said which they are.
    ///
    /// Kept apart from [`Problem::Asks`] because this one the shell *can*
    /// put on its own screen: an agreement is a text to read and a yes to
    /// give, and both of those are things a panel with a thumbstick can do.
    /// Nothing is accepted by getting this — the wizard was cancelled on the
    /// way out — and what accepts is a later [`install`] that is handed the
    /// same list back, which only a person pressing Accept makes.
    Agreements(Vec<Eula>),
    /// There is more than one library the game could go into, and the person
    /// is to choose — see [`Place::Ask`]. Carries the libraries and how much
    /// room the game needs, as the wizard counted them.
    ///
    /// Not a failure, on the terms [`Problem::Agreements`] is not one: the
    /// wizard was cancelled on the way out, nothing has been fetched, and what
    /// installs is a later [`install`] handed the place that was chosen.
    WhereTo(Choice),
    /// Steam would not do what was asked of a library, or would not move a
    /// game into one, and said why in a word the shell can put into its own
    /// — see [`Declined`].
    Declined(Declined),
    /// The ground the flow was standing on moved before it acted, so it did
    /// not act. Carries the caller's own account of what moved.
    ///
    /// Not a failure either, and the one problem here that nobody is ever told
    /// about: the account it belonged to has gone, which is precisely why
    /// there is no row left to say it on. It exists so the log says the
    /// install *did not happen* rather than saying nothing — see [`Still`].
    Moved(String),
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Problem::NotExposed => write!(
                f,
                "Steam is not exposing the interface this shell signs it in through."
            ),
            Problem::NoContext => write!(f, "Steam is running but its {CONTEXT} was not there"),
            Problem::Unreachable(why) => write!(f, "Steam could not be reached: {why}"),
            Problem::NotReady(why) => write!(f, "Steam is still starting up: {why}"),
            Problem::Refused(why) => write!(f, "Steam would not take the credential: {why}"),
            // Valve's own sentence, on its own. Anything this shell put in
            // front of it would be a guess at a failure it did not diagnose.
            Problem::Stopped(why) => write!(f, "{why}"),
            Problem::Asks(what) => write!(
                f,
                "This game has {what} first, which only Steam's own window can ask for."
            ),
            Problem::Agreements(listed) => write!(
                f,
                "This game has {} agreement(s) to accept first.",
                listed.len()
            ),
            Problem::WhereTo(choice) => write!(
                f,
                "This game can go into any of {} Steam libraries.",
                choice.libraries.len()
            ),
            // Short, because it is drawn as one line of a panel. Which methods
            // went is a fact for the log, and it is already there.
            Problem::Renamed(_) => {
                write!(f, "This version of Steam cannot be driven from here.")
            }
            Problem::Declined(why) => write!(f, "Steam would not: {why}"),
            // The caller's sentence, on its own, for the same reason
            // `Stopped`'s is: it already says what moved, and anything put in
            // front of it would be this module guessing at somebody else's
            // bookkeeping.
            Problem::Moved(why) => write!(f, "{why}"),
        }
    }
}

/// The file the client looks for to decide whether to expose its JS context.
///
/// Read by `steamui.so`, once, on the way up. There is no command line switch
/// that does the same thing — the strings beside it in that library are all
/// markers of this shape, `.steam-enable-steamrt64-client` among them — so this
/// file is the whole of the mechanism, and creating it is the only way in.
const MARKER: &str = ".cef-enable-remote-debugging";

/// Whether the client has been told to expose its JS context.
///
/// Read from the marker file rather than by trying the port, because the
/// question is asked while the client is *not* running — the shell wants to
/// know whether starting it is worth doing before it starts it.
pub fn available(root: &std::path::Path) -> bool {
    root.join(MARKER).is_file()
}

/// Tell the client to expose its JS context, if it has not been told already.
///
/// Returns whether the marker had to be made. That is not a detail the caller
/// can ignore: the client reads the file once, as it starts, so a client that
/// was *already running* when this created it is a client that read no marker
/// and is exposing nothing. It has to be started again before any of this
/// works. See [`crate::client::wake`], which is the only caller and does
/// exactly that.
///
/// This is the shell's to do rather than the user's. It used to be a line in
/// the README — `touch ~/.local/share/Steam/.cef-enable-remote-debugging` —
/// and a sign-in that refused until somebody had run it, which is a fine thing
/// to ask of a developer and no thing at all to ask of somebody who has just
/// turned a console on.
///
/// Worth being plain about what it costs, because it is a real thing to do to
/// somebody's machine: while the client runs, it listens on
/// `127.0.0.1:{PORT}`, and anything that can reach the loopback interface —
/// which is to say any program running as this user — can drive the client's
/// interface through it. Nothing is opened to the network. The file is made
/// only when the shell is actually about to sign the client in, so a session
/// that never touches Steam never writes to the Steam directory at all, and it
/// is taken back as soon as the client is up — see [`withdraw`], and
/// [`withdraw_what_was_left_behind`] for the run that never got that far.
pub fn expose(root: &std::path::Path) -> std::io::Result<bool> {
    let marker = root.join(MARKER);
    // `create_new` is `O_EXCL`, and the whole answer of this function hangs off
    // it: exactly one caller on the machine can be told it made the file, and
    // whoever is told that is the one — and the only one — that will take it
    // back. Asking `is_file` and then writing is two syscalls with a gap in the
    // middle, and two sessions arriving in that gap both believed they had made
    // it and both withdrew it, the second out from under the first.
    //
    // Empty on purpose: the client tests for the file, never reads it. What
    // says this one is ours is kept somewhere else entirely — see [`note`],
    // and see [`withdraw_what_was_left_behind`] for why it has to outlive the
    // call that made it.
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&marker)
    {
        Ok(_) => {}
        // Somebody was there first: another session inside its own wake, or a
        // developer's own marker. Either way it is not this call's to take
        // back. The same answer the `is_file` above it used to give, reached
        // without the gap.
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(error) => return Err(error),
    }
    note(&marker);
    tracing::info!(
        path = %marker.display(),
        "told Valve's client to expose the interface this shell signs it in through"
    );
    Ok(true)
}

/// Where this shell remembers having made the marker: the marker's own path,
/// in the state directory, for the reason the audit log is there.
///
/// Deliberately *not* written into the marker itself. The client tests for
/// that file and, as far as anybody outside Valve knows, never reads it — and
/// putting something in it to find out would be betting a session's sign-in on
/// a guess about somebody else's parser.
fn ours() -> Option<std::path::PathBuf> {
    crate::turns::beside_the_state("steam-debugging-marker")
}

/// And who made it, beside the note rather than in it.
///
/// A second file, so that the note itself keeps the shape it has always had —
/// the marker's path, raw, and nothing else. A build without this in it reads
/// that note correctly, and this one reads a note that build wrote and finds no
/// lease, which is the honest answer: it does not know whose that marker is.
/// See [`withdraw_what_was_left_behind`] for what is done with each.
fn whose_it_is() -> Option<std::path::PathBuf> {
    crate::turns::beside_the_state("steam-debugging-marker.lease")
}

/// Write down that the marker at this path is one this shell made.
fn note(marker: &std::path::Path) {
    let Some(note) = ours() else {
        return;
    };
    if let Some(parent) = note.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Who is making it, beside where it is. See [`whose_it_is`]: the marker
    // outlives the wake that made it whenever the wake is killed halfway, and
    // this is what lets the next session tell that from a wake still running.
    if let Some((lease, where_it_goes)) =
        crate::turns::Lease::of_this_process(&crate::turns::Behalf {
            backend: marker.parent().map(std::path::Path::to_path_buf),
            request: None,
        })
        .zip(whose_it_is())
    {
        let _ = std::fs::write(where_it_goes, lease.written());
    }
    if let Err(error) = std::fs::write(&note, marker.as_os_str().as_encoded_bytes()) {
        // Not a failure of the exposure, which has already worked. What is lost
        // is the ability to tidy up after a crash, and saying so is the whole
        // of what can be done about it.
        tracing::warn!(
            path = %note.display(),
            %error,
            "could not write down that this shell made Steam's debugging marker"
        );
    }
}

/// Forget the note, because the marker it named has gone.
fn forget_the_note() {
    if let Some(note) = ours() {
        let _ = std::fs::remove_file(note);
    }
    if let Some(lease) = whose_it_is() {
        let _ = std::fs::remove_file(lease);
    }
}

/// Take back a marker this shell made on a *previous* run and never took back.
///
/// The one case [`withdraw`]'s caller cannot cover. The marker is made and
/// taken back inside a single call, so a session that ends between the two —
/// a crash, a power cut, a development run stopped with `Ctrl-C` — leaves it
/// on the disk. From then on every [`expose`] finds it already there, decides
/// correctly that it is not its to remove, and leaves it; and every Steam that
/// user starts, for anything, comes up exposing its whole interface on the
/// loopback interface.
///
/// That is not hypothetical. It is how the machine this was written on was
/// found: a marker dated a fortnight earlier, and a client listening on
/// `127.0.0.1:{PORT}` with nothing having asked it to.
///
/// Only ever a marker this shell recorded making. Valve documents the file and
/// a developer may well want one of their own; one with no note beside it is
/// theirs and is left exactly where it is.
///
/// ## And only ever one no session is still using
///
/// "The run before this one" was the whole of the reasoning above, and it was
/// wrong the moment two of these could run at once. Another session's wake
/// makes the marker, hands its client a credential behind it, and takes it back
/// — and a shell starting up in the middle of that found a note, found the
/// marker it named, and read a file in active use as litter. The client that
/// was starting then came up exposing nothing, and the sign-in behind it
/// failed for no reason anybody could see.
///
/// So two questions are asked before anything is removed, and they are asked
/// separately because they can fail separately:
///
/// **Is anyone inside a wake?** The marker is only ever made and taken back
/// inside [`crate::client::wake`], which holds [`crate::turns::What::TheClient`]
/// for the whole of one. Asking for that turn *without waiting* is therefore
/// exactly the question — and waiting for it would be the wrong thing to do
/// twice over, since a cold client is a minute and a half and this runs on the
/// way to a screen.
///
/// **Is the process that left it still there?** For the machine where the lock
/// could not be made at all, and for the marker whose owner died holding it.
/// See [`crate::turns::Lease::alive`], which is a pid *and* that pid's start
/// time *and* the boot it was taken under, because a bare pid is a number
/// somebody else is wearing by the time this matters.
///
/// Neither question can wrongly remove a marker in use. Both can wrongly leave
/// one behind — a session that cannot make locks, dying between the two halves
/// of a wake — and that is the right way round: a marker left an hour longer is
/// a line in the log, where one taken away mid-wake is somebody's sign-in.
pub fn withdraw_what_was_left_behind() {
    let Some(note) = ours() else {
        return;
    };
    let Ok(marker) = std::fs::read(&note) else {
        return;
    };
    // First question. Nothing is read, let alone removed, while somebody is
    // inside a wake — and the turn is held for the whole of the tidying, so a
    // wake cannot begin in the middle of it either.
    let Some(_turn) = crate::turns::take_if_free(
        crate::turns::What::TheClient,
        crate::turns::Behalf::nothing(),
    ) else {
        tracing::info!(
            "another LineXinBar session is using Valve's client; leaving its debugging marker alone"
        );
        return;
    };
    // Second question, and the one that covers the machine where the first
    // could not be asked. A lease that is missing is a note from a build that
    // wrote none, and the old reading of it — "the run before this one" — is
    // the right one: nothing is holding the turn, so nothing is using it.
    if let Some(lease) = whose_it_is()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| crate::turns::Lease::read(&text))
    {
        if lease.alive() {
            tracing::info!(
                pid = lease.pid,
                session = %lease.session,
                "the session that made Valve's debugging marker is still running; leaving it alone"
            );
            return;
        }
    }
    let marker = std::path::PathBuf::from(std::ffi::OsString::from(
        String::from_utf8_lossy(&marker).into_owned(),
    ));
    // The note outliving its marker is the ordinary case — the marker was taken
    // back and something interrupted the tidying — and there is nothing to do
    // about it but tidy.
    if marker.is_file() {
        tracing::info!(
            path = %marker.display(),
            "taking back a marker this shell left behind on an earlier run"
        );
        let _ = std::fs::remove_file(&marker);
    }
    // Both files, through the one call that knows there are two of them: a
    // lease left beside a note that has gone would be read by the next session
    // as the owner of whatever marker it finds next.
    forget_the_note();
}

/// Take the marker away again, once it has done its work.
///
/// It does not have to stay. The client reads it as it starts and nothing
/// afterwards: with a client up and answering, deleting the file leaves the
/// port open and `SharedJSContext` still listed, which was checked against a
/// running client rather than assumed. So the file only needs to exist across
/// the moment the client starts, and leaving it lying around would mean a Steam
/// somebody starts *themselves*, long after this session has gone, comes up
/// exposing an interface they never asked to expose.
///
/// Only ever called for a marker this shell made — [`expose`] says whether it
/// did. One that was already there is somebody else's and is left alone.
///
/// Never fails outwards. The marker has already served its purpose by the time
/// this runs, and a session that cannot delete it has nothing useful to say to
/// anybody about a file they have never heard of.
pub fn withdraw(root: &std::path::Path) {
    let marker = root.join(MARKER);
    forget_the_note();
    match std::fs::remove_file(&marker) {
        Ok(()) => tracing::info!(
            path = %marker.display(),
            "took back the marker that exposes Valve's client"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!(
            path = %marker.display(),
            %error,
            "could not take back the marker that exposes Valve's client"
        ),
    }
}

/// Whose the marker on the disk is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marker {
    /// This shell made it, is signing the client in behind it, and will take it
    /// back — see [`withdraw`]. Nothing here for anybody to do.
    Ours,
    /// Nobody here made it. Every Steam started while it is there comes up
    /// exposed, whatever it was started for.
    Somebody,
}

/// What the client's debugging interface is doing on this machine, and whether
/// this shell is the reason for it.
///
/// Two facts, and they are genuinely independent rather than one asked twice.
/// The marker decides what a client becomes *as it starts*; the port is what
/// the client that is already up is doing *now*. Deleting the marker while a
/// client runs leaves the port open until that client restarts, and writing one
/// while it runs opens nothing until it does. Neither answers for the other, so
/// both are reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exposure {
    /// Whether `127.0.0.1:{PORT}` is answering — which is whether anything
    /// running as this user can drive Valve's client right now.
    pub open: bool,
    /// The marker on the disk, if there is one.
    pub marker: Option<Marker>,
}

impl Exposure {
    /// The one line a panel gives this, inside the width a panel has.
    ///
    /// Written for somebody who has never heard of any of it, so the sentence
    /// is about who can do what and not about files and ports. "Anything you
    /// run" is the exact truth of it: this is a loopback socket with no
    /// authentication, so every game, every Flatpak and every script started as
    /// this user can drive the client through it.
    pub fn said(&self) -> &'static str {
        match (self.open, self.marker) {
            // Its own, which is on screen for as long as a sign-in takes and is
            // reported so that the answer to "why is it open" is never missing.
            (_, Some(Marker::Ours)) => "open, this session's",
            (true, Some(Marker::Somebody)) => "open to anything you run",
            (false, Some(Marker::Somebody)) => "shut, but marked to open again",
            // A client that started when there was a marker and has outlived
            // it. There is nothing to delete; it closes when that client does.
            (true, None) => "open until Steam restarts",
            (false, None) => "closed",
        }
    }

    /// Whether there is something here somebody could close, which is what
    /// decides whether a panel carries the button.
    ///
    /// Only a marker this shell did not make. Its own is taken back by the run
    /// that made it and stands for seconds; a button that deleted that one
    /// would be a way to break a sign-in in progress. And an open port with no
    /// marker behind it cannot be closed by deleting anything at all.
    pub fn can_be_shut(&self) -> bool {
        self.marker == Some(Marker::Somebody)
    }
}

/// What the debugging interface is doing, read off the disk and off the port.
///
/// Cheap enough for a panel opened at a glance, which nothing else that reaches
/// for the client is. [`reachable`] asks the client for its page listing and
/// will wait out [`PATIENCE`] for the answer; this opens a socket to loopback
/// and drops it. A closed port refuses at once and an open one accepts at once,
/// so there is no case in which this waits — the timeout is there for the case
/// that cannot happen.
pub fn exposure(root: &std::path::Path) -> Exposure {
    Exposure {
        open: listening(),
        marker: available(root).then(|| match noted_as_ours(&root.join(MARKER)) {
            true => Marker::Ours,
            false => Marker::Somebody,
        }),
    }
}

/// Whether the note says this shell is the one that made the marker here.
fn noted_as_ours(marker: &std::path::Path) -> bool {
    let Some(note) = ours() else {
        return false;
    };
    let Ok(noted) = std::fs::read(&note) else {
        return false;
    };
    std::path::Path::new(String::from_utf8_lossy(&noted).as_ref()) == marker
}

/// Whether anything is listening where the client exposes itself.
///
/// The port and nothing more: not whether the context is there yet, which
/// [`reachable`] asks and which a client still starting its browser takes
/// seconds to answer. Public for the one caller that wants exactly this — the
/// first setup, deciding when the client has *read* its marker, which is
/// before it has anything to say through it. See `crate::setup`.
pub fn listening() -> bool {
    let address = std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, PORT));
    std::net::TcpStream::connect_timeout(&address, Duration::from_millis(200)).is_ok()
}

/// Take away a marker this shell did not make, because the person whose machine
/// it is asked for it to go.
///
/// Not an exception to [`withdraw`]'s rule. The rule is that this shell does not
/// decide *on its own* to turn off something somebody else turned on; being
/// asked is not deciding, and the asking is a button on a panel that says in
/// plain words what the marker does.
///
/// It is also the only way out of a one-time gap. A marker made before this
/// shell began writing a note beside its own has nothing on it to say who made
/// it, so [`withdraw_what_was_left_behind`] leaves it alone — correctly, and
/// forever. On the machine this was written on that marker was a fortnight old
/// and its port was open.
///
/// This does not close a port that is already open, and the panel does not
/// pretend it does: the running client read the file as it started and has not
/// looked since. What it changes is every Steam started from now on.
pub fn shut_the_interface(root: &std::path::Path) -> std::io::Result<()> {
    let marker = root.join(MARKER);
    std::fs::remove_file(&marker)?;
    tracing::info!(
        path = %marker.display(),
        "the marker that exposes Valve's client was taken away by hand"
    );
    Ok(())
}

/// Whether the client's JS context can be reached *right now*.
///
/// The real question, where [`available`] only says what the client would have
/// been told had it started since. Both are needed and they answer different
/// things: the marker decides what a client becomes as it comes up, and this
/// decides whether the one that is up is any use.
///
/// Cheap in the case that matters. A closed port refuses the connection at
/// once rather than waiting out [`PATIENCE`], so asking this before every press
/// costs nothing on the machine where it is false.
pub fn reachable() -> bool {
    context().is_ok()
}

/// Hand the running client a refresh token, as its own login screen would.
///
/// The client must already be running. Returns once the client has answered:
/// `Ok` means it took the token and is signing itself in, which the caller then
/// confirms by watching [`crate::client::state`] — this says the call was
/// accepted, not that Steam's servers agreed.
pub fn sign_in(account: &str, refresh_token: &str) -> Result<(), Problem> {
    let socket = context()?;
    // `awaitPromise`, because every `SteamClient` method answers with one: the
    // JS side is a proxy over an IPC to the C++ half, and the value that comes
    // back without waiting is a pending promise and no answer at all.
    let call = format!(
        "SteamClient.Auth.SetLoginToken({}, {}).then(answer => JSON.stringify(answer))",
        json_string(refresh_token),
        json_string(account)
    );
    let answer = evaluate(&socket, &call)?;
    tracing::info!(
        account,
        "gave Valve's client a credential through its own interface"
    );
    took_it(&answer)
}

/// The number Valve's settings message gives "Guide button focuses Steam".
///
/// A number and not a name, because the name exists only in the interface. The
/// client reads this setting on the C++ side and `steamclient.so` carries no
/// string for it; `steamui`'s own settings table is where the two are tied
/// together, as
/// `controller_guide_button_focus_steam: { n: 14002, br: readBool, bw: writeBool }`,
/// beside the Controller page's `#Settings_Controller_GuideButtonFocus` toggle.
const GUIDE_BUTTON_FOCUSES_STEAM: u32 = 14002;

/// Ask Valve's client to stop answering the guide button itself.
///
/// This is the one hole in [the shell's rule that the guide button is its
/// alone](crate::client), and it is here rather than anywhere nearer that rule
/// because it cannot be closed the way the others are. Every controller the
/// kernel drives is taken away and handed back a button short — the pad is
/// grabbed, and applications find a stand-in with the guide button declared and
/// never sent. The second-generation Steam Controller has no such node to take:
/// Valve's client reads that pad's *raw HID reports*, with its own code rather
/// than SDL, and a raw node cannot be held exclusively by anybody. So the
/// button reaches the client whatever this shell does, and the only thing left
/// to ask is that the client not act on it.
///
/// Measured on the hardware, three presses in each state, seconds apart: with
/// the setting on, `GetDesiredSteamUIWindows` goes from the desktop window to
/// Big Picture's on the first press — UI mode 7 to mode 4 — and with it off the
/// window list does not move. The client logs `Guide button sent to JS` either
/// way, so its log is not what says whether this worked; the window is.
///
/// The value is read back rather than assumed. The field is a number, and a
/// number Valve is free to reuse — a client that renumbered it would take this
/// setting silently, which is exactly the failure that is worth a line in the
/// log rather than a user wondering why Big Picture keeps opening.
pub fn leave_the_guide_button_alone() -> Result<(), Problem> {
    let socket = context()?;
    let mut settings = crate::protobuf::Writer::new();
    // Explicitly, because "off" is what is being asked for: a settings message
    // that leaves the field out is a message asking for nothing. See
    // [`crate::protobuf::Writer::bool_even_when_false`].
    settings.bool_even_when_false(GUIDE_BUTTON_FOCUSES_STEAM, false);
    let message = crate::base64::encode(&settings.finish());

    let call = format!(
        "(async () => {{ \
           await SteamClient.Settings.SetSetting({}); \
           return JSON.stringify(\
             window.settingsStore.m_ClientSettings.controller_guide_button_focus_steam); \
         }})()",
        json_string(&message)
    );
    match evaluate(&socket, &call)?.as_str() {
        "false" => {
            tracing::info!("Valve's client will leave the guide button to this shell");
            Ok(())
        }
        other => Err(Problem::Refused(format!(
            "the guide button setting reads {other} after being turned off; \
             Valve may have renumbered it"
        ))),
    }
}

/// How long a *poll* may wait for the client.
///
/// Not [`PATIENCE`], which is what a command that does something is given. This
/// is asked every couple of seconds while a download runs, and a late answer is
/// worth nothing — the next one is already due. A client that has stopped
/// answering must not stall the worker that is reading the disk beside it.
const WHILE_A_ROW_WAITS: Duration = Duration::from_secs(2);

/// What Valve's client says about the download it is running now.
///
/// **This is the only honest source there is**, and that was measured rather
/// than assumed. `BytesDownloaded` in a game's `appmanifest` — the field this
/// shell counted from — is written so rarely that it says nothing: through a
/// whole install of a 626 MB game on real hardware it read
///
/// ```text
/// 21:43:33.32  flags=1026  dl=0          total=656543488
/// 21:43:39.20  flags=1026  dl=560845856  total=656543488
/// 21:43:39.44  flags=1026  dl=0          total=0            <- back to nothing
/// 21:43:41.13  flags=1026  dl=560845856  total=656543488
/// 21:43:42.37  flags=4     dl=656543488  total=656543488
/// ```
///
/// — nought for six of the nine seconds it took, and briefly nought *again*
/// halfway through. The client's own overview over the same nine seconds went
/// 4%, 35%, 47%, 58%, 71%, 82%, 98%, 100%, with a rate beside each. Steam's own
/// window is drawn from this, which is why it is the one that moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Live {
    /// Which game the client is fetching. There is one download at a time.
    pub app_id: u32,
    /// How far along, 0 to 100, as the client's own download page shows it.
    pub percent: u8,
    /// Off the network, bytes per second. Zero for the first second or two,
    /// before the client has two samples to divide.
    pub bytes_per_second: u64,
    /// How much longer the client thinks it will take. `None` where it has not
    /// decided yet, which it says with a negative number.
    pub seconds_left: Option<u32>,
    /// Whether bytes are actually moving, as against a download that is
    /// starting, finishing or paused. The client's own word for it.
    pub moving: bool,
}

impl Live {
    /// How far along, as a share of one, or nothing where the client has not
    /// said yet.
    ///
    /// Nought is *not yet* rather than a reading, which is the same rule the
    /// manifest half is read under — see [`crate::library::fraction`]. A
    /// download that is `Starting` reports nought for a second or two.
    pub fn fraction(self) -> Option<f32> {
        (self.percent > 0).then(|| f32::from(self.percent) / 100.0)
    }

    /// How fast, where that means anything.
    ///
    /// Nothing, rather than nought, for the first second or two: a rate is two
    /// samples divided and the client has not taken the second one yet. And
    /// nothing once the download has stopped arriving, because what the client
    /// keeps reporting then is the average of a thing that has finished.
    pub fn per_second(self) -> Option<u64> {
        (self.moving && self.bytes_per_second > 0).then_some(self.bytes_per_second)
    }
}

/// What the running client is fetching, if it is fetching anything and will say.
///
/// `Ok(None)` is a client with nothing on its download list; the errors are a
/// client that cannot be reached at all. Both are ordinary — this is asked
/// hopefully rather than depended on, and the manifest on the disk is what the
/// row falls back to.
///
/// The client will not simply be *asked*: `SteamClient.Downloads` offers a
/// registration and no getter, so the first call leaves a callback behind that
/// writes each overview into the page, and every call reads what it last wrote.
/// The registration is made once and survives, because it is the same page
/// however many times this connects to it.
pub fn downloading() -> Result<Option<Live>, Problem> {
    let socket = context()?;
    let call = "(async () => {          if (!window.__lxb_downloads) {            window.__lxb_downloads = { seen: null, hook: null };            window.__lxb_downloads.hook = SteamClient.Downloads.RegisterForDownloadOverview(              (overview) => { window.__lxb_downloads.seen = overview; });          }          const seen = window.__lxb_downloads.seen;          if (!seen || !seen.update_appid) { return 'null'; }          return JSON.stringify({            app_id: seen.update_appid,            percent: Math.max(0, Math.min(100, Math.round(seen.overall_percent_complete ?? 0))),            bytes_per_second: Math.max(0, Math.round(seen.update_network_bytes_per_second ?? 0)),            seconds_left: seen.overall_estimated_time_remaining_sec ?? -1,            state: seen.update_state ?? '' });        })()";
    what_the_client_said(&evaluate_within(&socket, call, WHILE_A_ROW_WAITS)?)
}

/// What one answer from [`downloading`] comes to.
///
/// Its own function so the shapes a live client actually produces can be
/// checked without one. The three below were captured off a real install and
/// are what the tests are written against.
fn what_the_client_said(answer: &str) -> Result<Option<Live>, Problem> {
    if answer.trim() == "null" {
        return Ok(None);
    }

    #[derive(serde::Deserialize)]
    struct Said {
        app_id: u32,
        percent: u8,
        bytes_per_second: u64,
        seconds_left: i64,
        state: String,
    }
    let said: Said =
        serde_json::from_str(answer).map_err(|_| Problem::Refused(answer.to_string()))?;
    Ok(Some(Live {
        app_id: said.app_id,
        percent: said.percent.min(100),
        bytes_per_second: said.bytes_per_second,
        // A negative number is the client saying it has not decided; anything
        // else is seconds.
        seconds_left: u32::try_from(said.seconds_left).ok(),
        // The words the client uses for its own state. `Downloading` is the
        // one that means bytes are arriving; `Starting` and `Finalizing` are
        // the two ends of the same job, and a rate quoted over either of them
        // would be an average of something that is no longer happening.
        moving: said.state == "Downloading",
    }))
}

/// A launch Valve's client is in the middle of, and whether it has stopped.
///
/// The client walks a launch as a **game action**: a named job with a task it
/// is on, which for an ordinary press runs `ProcessingInstallScript` →
/// `SynchronizingControllerConfig` and is gone within seconds. Some of those
/// tasks are not steps at all. They are questions — a save in the cloud that
/// disagrees with the one on the disk, an agreement, a launch option, a
/// parental code — and the client stops on them until a person answers, in a
/// window this shell is holding off the screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launching {
    /// The client's own handle for this launch, and what an answer to it has to
    /// be sent against.
    pub action_id: u32,
    pub app_id: u32,
    /// The client's own name for the step it is on — `SynchronizingCloud`,
    /// `ProcessingInstallScript`. For the log, and never for the screen: it is
    /// an internal identifier rather than a sentence, and it is in English
    /// whatever language Steam is running in.
    pub task: String,
    /// What the client says about that step, where it says anything —
    /// `pendingcloudsessions` under `SynchronizingCloud`. The second half of
    /// what decides which question is being asked. See [`the_question`].
    pub details: String,
    /// How much of the work this task counts, where it counts any: `numDone`
    /// against `numTotal`, which the client writes as strings.
    ///
    /// One task in a launch carries them — the shader cache being compiled,
    /// which is the one step long enough to be worth a number. See
    /// [`Launching::how_far`].
    pub done: u32,
    pub total: u32,
    /// Whether the client has stopped and cannot go on until somebody answers.
    ///
    /// **`bWaitingForUI`, and it is the whole of the rule.** One boolean of the
    /// client's own, so nothing here has to recognise Valve's wording or guess
    /// from how long a task has taken. Measured on this machine on 2026-09-02:
    /// a launch of a game with a save the cloud had not caught up with sat on
    /// `SynchronizingCloud` / `pendingcloudsessions` with this true and stayed
    /// there; the same client's ordinary launch of another game never set it.
    pub waiting_for_a_person: bool,
}

impl Launching {
    /// How far through this step the client says it is, as a percentage, or
    /// nothing where it counts nothing.
    ///
    /// **Valve's own arithmetic**, transcribed from the client's own launch
    /// screen: a total of nought, or a count that has run past it, shows
    /// nothing. A step that has finished without saying so would otherwise be
    /// a reading of three hundred per cent.
    pub fn how_far(&self) -> Option<u32> {
        (self.total > 0 && self.done <= self.total).then(|| self.done * 100 / self.total)
    }
}

/// What Valve's client is doing about the launches it has been asked for.
///
/// An empty list is a client with nothing starting, which is the answer for
/// nearly the whole of a session; the errors are a client that cannot be
/// reached at all. Asked hopefully, like [`downloading`] — a launch is watched
/// by waiting for the game's window, and this only ever explains a wait that
/// is not going to end.
///
/// A plain getter, unlike the download overview: `GetActiveGameActions` answers
/// with the list rather than offering a registration, so there is no callback
/// to leave behind in the page.
pub fn launching() -> Result<Vec<Launching>, Problem> {
    let socket = context()?;
    // `LaunchApp` only. The same list carries the client's other actions, and
    // an install stopping to ask something is not a game that will not start.
    let call = "(async () => {                   const actions = await SteamClient.Apps.GetActiveGameActions();                   return JSON.stringify((actions ?? [])                     .filter(one => one.strActionName === 'LaunchApp')                     .map(one => ({                       action_id: Number(one.nGameActionID ?? 0),                       gameid: String(one.gameid ?? '0'),                       task: String(one.strTaskName ?? ''),                       details: String(one.strTaskDetails ?? ''),                       done: Number(one.numDone ?? one.strNumDone ?? 0),                       total: Number(one.numTotal ?? one.strNumTotal ?? 0),                       waiting: one.bWaitingForUI === true })));                 })()";
    what_the_client_is_launching(&evaluate_within(&socket, call, WHILE_A_ROW_WAITS)?)
}

/// What one answer from [`launching`] comes to.
///
/// Its own function so the shapes a live client produces can be checked without
/// one. Those below were captured off a real launch.
fn what_the_client_is_launching(answer: &str) -> Result<Vec<Launching>, Problem> {
    #[derive(serde::Deserialize)]
    struct Said {
        action_id: u32,
        gameid: String,
        task: String,
        details: String,
        // A count the client is not keeping comes back as nought rather than
        // as a missing field; the call above reads both of Valve's spellings
        // for each. Defaulted anyway, so a client that answers an older shape
        // is a launch with no number rather than an unreadable answer.
        #[serde(default)]
        done: u32,
        #[serde(default)]
        total: u32,
        waiting: bool,
    }
    let said: Vec<Said> =
        serde_json::from_str(answer).map_err(|_| Problem::Refused(answer.to_string()))?;
    Ok(said
        .into_iter()
        .filter_map(|one| {
            // `gameid` is a `CGameID`, whose low 32 bits are the app id. For an
            // ordinary title it is simply the app id written out; for a
            // non-Steam shortcut it is a much larger number whose low half is
            // not one, and such a launch is not this shell's to explain.
            let app_id = one.gameid.parse::<u64>().ok()? as u32;
            (app_id != 0).then_some(Launching {
                action_id: one.action_id,
                app_id,
                task: one.task,
                details: one.details,
                done: one.done,
                total: one.total,
                waiting_for_a_person: one.waiting,
            })
        })
        .collect())
}

/// What the shell sends back to carry one answer to a launch that has stopped.
///
/// Transcribed from the client's own `OnGameActionUserRequest`, which is where
/// every one of these strings comes from. Its default arm even prints the call
/// for a request it does not recognise —
/// `SteamClient.Apps.ContinueGameAction( <id>, '<request>' )` — which is what
/// says the string is the protocol rather than a label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Carry {
    /// Go on with the launch, answering with this word.
    Go(String),
    /// Do not. The launch ends and no game starts.
    Stop,
}

/// One answer to a question, as a row of a panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    /// Valve's own word for it, in the language the client is running in.
    pub label: String,
    pub carry: Carry,
    /// What this answer would be remembered by, for the one question that can
    /// be answered in advance — which of a game's ways of starting is wanted.
    ///
    /// The option's own name rather than [`Answer::label`] or the number the
    /// client gave it, because it is the only one of the three that is stable.
    /// The label is a sentence the client builds and translates; the number is
    /// made over whichever options the client decided to offer this machine
    /// today. Somebody who chose "OpenFront (Wayland workaround)" last week
    /// chose *that*, and it is still that after a Steam update has renumbered
    /// the list or a language change has rewritten the row. See [`Way`].
    ///
    /// `None` on every other question's answers: a cloud save conflict is not
    /// a preference and must never be answered from a file.
    pub chooses: Option<String>,
}

/// A question Valve's client has stopped a launch on, in words this shell can
/// put on its own screen.
///
/// **Valve owns the words and this shell owns the buttons**, and that split is
/// the whole design. The heading, the body and every label are fetched from the
/// client's own localisation table by token, so they are the sentences Steam
/// would have shown, in the language it is running in; what the shell supplies
/// is a panel a thumbstick can reach, because Valve's own dialog for these is
/// a desktop one and on a console nobody can press it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    /// What to answer against.
    pub action_id: u32,
    pub app_id: u32,
    /// One or two sentences, already filled in with the game's name.
    ///
    /// Valve's own, and only the ones that *are* sentences. Its headings are
    /// window titles — "Error - Steam", "Steam - Setup failed" — and a window
    /// title read as a line of a panel is a line that says nothing. The two
    /// that do say something ("Cloud Out of Date", "Unable to Sync") are in
    /// here as body lines instead. What heads the panel is the game's name,
    /// which is what heads every other panel in this shell about a game.
    pub body: Vec<String>,
    /// Never empty, and the last of them is always [`Carry::Stop`].
    pub answers: Vec<Answer>,
}

/// The questions this shell will put on its own screen, and what answers them.
///
/// A table rather than a guess, transcribed from the client's own dispatch. It
/// is deliberately **not** all of them: `ShowEula` is an agreement somebody has
/// to read and this shell will not put an OK on it, `ShowCDKey` is a key to be
/// read off the screen, and `cloudconflict` is a choice between two saves that
/// Valve shows with the date of each — asking somebody to pick one blind is
/// worse than asking them to reach for a mouse once. Everything not here and
/// not [`CHOOSING_HOW_TO_START`] is answered the older way, by letting the
/// client be seen.
///
/// `ShowLaunchOption` was in that list of refusals until 2026-09-19 and is now
/// the one question with a branch of its own rather than a row here, because
/// its answers are the game's and have to be asked for. See
/// [`how_it_can_be_started`].
///
/// Each row is: the client's request, the detail under it where the detail is
/// what decides, the heading token, the body tokens, and the answers.
fn known_questions() -> Vec<Asked> {
    // `%1$s` in these is the game's name, and the client's own localiser fills
    // it in — see [`the_question`], which hands it over as an argument rather
    // than substituting anything here.
    vec![
        // The reported one: a save played on this machine that the cloud has
        // not taken yet.
        Asked {
            request: "SynchronizingCloud",
            details: Some("pendingcloudsessions"),
            body: &["#CloudPendingOps_Header", "#CloudPendingOps_Description"],
            answers: &[(
                "#CloudPendingOps_Continue",
                Some("IgnorePendingCloudSessions"),
            )],
        },
        Asked {
            request: "SynchronizingCloud",
            details: Some("syncfailed"),
            body: &[
                "#CloudSyncFailed_AppLaunch_Warning",
                "#CloudSyncFailed_AppLaunch_Description",
            ],
            answers: &[("#CloudSyncFailed_AppLaunch_Continue", Some("IgnoreCloud"))],
        },
        // The same two, about a controller's own configuration rather than a
        // save. The client sends them through the same dialog.
        Asked {
            request: "SynchronizingControllerConfig",
            details: Some("pendingcloudsessions"),
            body: &["#CloudPendingOps_Header", "#CloudPendingOps_Description"],
            answers: &[(
                "#CloudPendingOps_Continue",
                Some("IgnorePendingCloudSessions"),
            )],
        },
        Asked {
            request: "SynchronizingControllerConfig",
            details: Some("syncfailed"),
            body: &[
                "#CloudSyncFailed_AppLaunch_Warning",
                "#CloudSyncFailed_AppLaunch_Description",
            ],
            answers: &[("#CloudSyncFailed_AppLaunch_Continue", Some("IgnoreCloud"))],
        },
        // The game's own installer failed, and Steam is asking whether to run
        // it anyway.
        Asked {
            request: "RunningInstallScript",
            details: None,
            body: &["#LaunchApp_InstallScript_Failed_Text"],
            answers: &[(
                "#LaunchApp_InstallScript_Failed_Continue",
                Some("IgnoreInstallError"),
            )],
        },
        // The account is playing this somewhere else, and starting it here
        // ends that.
        Asked {
            request: "KickingOtherSession",
            details: None,
            body: &["#LaunchApp_OtherSessionPlaying_Text"],
            answers: &[("#LaunchApp_ContineLaunch", Some("KickOtherSession"))],
        },
        // Somebody has set launch arguments on the game and Steam wants that
        // confirmed before it uses them.
        Asked {
            request: "ShowGameArgs",
            details: None,
            body: &["#LaunchApp_ShowGameArgs_Text"],
            answers: &[("#LaunchApp_ContineLaunch", Some("ShowGameArgs"))],
        },
    ]
}

/// The client's own name for the step that compiles a game's shaders before it
/// will start it.
///
/// **Not one of [`known_questions`], and deliberately.** Those are questions:
/// they stop a launch until a person decides something, and the shell answers
/// them with a panel. This one is *work* — it finishes on its own, it counts
/// itself while it does, and Valve stops on it only to offer a way past. A
/// panel over the loading screen would be a modal dialog about a progress bar.
///
/// So it is shown where the rest of a wait is shown, on the loading screen
/// itself, and the way past it is [`SKIP_SHADERS`].
pub const PROCESSING_SHADERS: &str = "ProcessingShaderCache";

/// And the word that says "start the game now, with what has been compiled so
/// far".
///
/// Transcribed from the client's own gamepad launch screen, which is the half
/// of Valve's UI that has a button for this at all:
/// `SteamClient.Apps.ContinueGameAction(n, "SkipShaders")`. The desktop half
/// puts up the dialog with `Skip` and `Cancel` on it that this shell holds off
/// the screen.
pub const SKIP_SHADERS: &str = "SkipShaders";

/// The client's own name for the step that stops a launch to ask **which way
/// the game is to be started**.
///
/// Its own branch rather than a row of [`known_questions`], because its
/// answers are not words in a table. They are the game's own launch options,
/// which are different for every game and are the client's to hand over — it
/// answers with the ones it has already decided may be offered here, and it
/// keeps each one's own number while it does.
///
/// OpenFront is the worked example, and it is also why nothing here reads the
/// game's own `appinfo`. Its `config/launch` has four entries — `0` and `1`
/// locked to Windows and macOS, `3` the plain Linux one and `4` the Wayland
/// workaround — and what the client actually answered, asked on this machine,
/// was **two options numbered 0 and 1**. The number is the client's own, made
/// over the list it decided to offer, and it is neither the game's key nor
/// anything a shell could work out for itself.
///
/// Reported from use 2026-09-19, and OpenFront is the game it was reported
/// against: it ships two of them, and every launch of it stopped here. This
/// machine's own `console_log.txt` has the whole exchange, and the press it
/// begins with is this shell's own way of pressing Play:
///
/// ```text
/// [17:55:50] ExecuteSteamURL: "steam://rungameid/3560670"
/// [17:56:58] GameAction [AppID 3560670, ActionID 2] : LaunchApp changed task to ShowLaunchOption with ""
/// [17:56:58] GameAction [AppID 3560670, ActionID 2] : LaunchApp waiting for user response to ShowLaunchOption ""
/// [17:57:04] GameAction [AppID 3560670, ActionID 2] : LaunchApp continues with user response "1"
/// ```
///
/// **The answer is the option's own index, written out** — Valve's own dialog
/// sends `nIndex.toString()` and nothing else. Those six seconds are somebody
/// reaching Valve's window with a mouse; without one the launch stays there,
/// and the loading screen in front of it runs out of patience and says the
/// game did not start.
pub const CHOOSING_HOW_TO_START: &str = "ShowLaunchOption";

/// Valve's own sentence for that question, whose `%1$s` is the game's name.
///
/// "Make a selection to launch %1$s". Its other two strings for this dialog
/// are titles rather than sentences and are left where they are, for the
/// reason [`Question::body`] gives: `#LaunchOptionsDialog_SelectHeader` is
/// "Select Launch Option (2)", a window title with a count in it, and this
/// panel is already headed with the game's name.
const MAKE_A_SELECTION: &str = "#LaunchOptionsDialog_MakeSelection";

/// One row of [`known_questions`].
struct Asked {
    request: &'static str,
    details: Option<&'static str>,
    body: &'static [&'static str],
    /// Only the ones that go on. The refusal is added to every question by
    /// [`the_question`], with Valve's own word for it, because a question with
    /// no way out is not one.
    answers: &'static [(&'static str, Option<&'static str>)],
}

/// Which row of the table a stopped launch is, if it is one of them.
fn asked_about(request: &str, details: &str) -> Option<Asked> {
    known_questions().into_iter().find(|known| {
        known.request == request && known.details.is_none_or(|wanted| wanted == details)
    })
}

/// The question a launch has stopped on, in the words Steam would have used.
///
/// `Ok(None)` where the launch is not waiting on anybody, or is waiting on
/// something neither [`known_questions`] nor [`how_it_can_be_started`] has a
/// panel for — both are ordinary, and the second is answered by giving the
/// client sight instead.
pub fn the_question(stopped: &Launching) -> Result<Option<Question>, Problem> {
    // The one question whose answers are not a table. Asked first because the
    // table below is keyed on the request and this request is not in it — a
    // lookup that would only ever miss.
    if stopped.task == CHOOSING_HOW_TO_START {
        return how_it_can_be_started(stopped);
    }
    let Some(asked) = asked_about(&stopped.task, &stopped.details) else {
        tracing::info!(
            task = %stopped.task,
            details = %stopped.details,
            "this shell has no panel for what the client is asking"
        );
        return Ok(None);
    };
    // The refusal, last and always. `#LaunchApp_Cancel` is what the client's
    // own launch dialogs call it.
    let tokens: Vec<&str> = asked
        .body
        .iter()
        .copied()
        .chain(asked.answers.iter().map(|(token, _)| *token))
        .chain(std::iter::once(CANCEL))
        .collect();

    let socket = context()?;
    let wanted = tokens
        .iter()
        .map(|token| json_string(token))
        .collect::<Vec<_>>()
        .join(", ");
    // The name comes back beside the words, because the substitution is made
    // here rather than there. `LocalizationManager.LocalizeString` takes a
    // token and returns the sentence with Valve's placeholders still in it —
    // measured, not assumed: asked with two arguments it answered
    // "%1$s is attempting to launch with optional parameters shown below:".
    // Valve's own callers pass the result through a second helper, which is a
    // minified local in a chunk this shell has no business reaching into. See
    // [`fill_in`], which is that helper's whole job.
    let call = format!(
        "(async () => {{ \
           const name = window.appStore?.GetAppOverviewByAppID?.({})?.display_name ?? ''; \
           return JSON.stringify([name].concat([{wanted}].map( \
             token => window.LocalizationManager.LocalizeString(token) ?? ''))); \
         }})()",
        stopped.app_id
    );
    let mut answered: Vec<String> = serde_json::from_str(&evaluate(&socket, &call)?)
        .map_err(|_| Problem::Refused("the client would not localise its own words".to_string()))?;
    if answered.is_empty() {
        return Ok(None);
    }
    // The game's name, and then the words. Both of Valve's arguments: the name
    // is its `%1$s` throughout, and the detail the client gave is its `%2$s` —
    // the other session's game, the arguments a launch is asking about.
    let name = answered.remove(0);
    let said: Vec<String> = answered
        .iter()
        .map(|line| fill_in(line, &[&name, &stopped.details]))
        .collect();
    if said.len() != tokens.len() || said.iter().any(|line| line.trim().is_empty()) {
        // A token the client no longer has comes back empty or missing, and a
        // panel with a blank button on it is worse than Valve's own window.
        tracing::warn!(?tokens, "Valve's client did not have all of these words");
        return Ok(None);
    }

    let mut said = said.into_iter();
    let body: Vec<String> = said.by_ref().take(asked.body.len()).collect();
    let mut answers: Vec<Answer> = asked
        .answers
        .iter()
        .zip(said.by_ref())
        .map(|((_, carry), label)| Answer {
            label,
            carry: match carry {
                Some(word) => Carry::Go((*word).to_string()),
                None => Carry::Stop,
            },
            // Nothing here may be answered in advance — see [`Answer::chooses`].
            chooses: None,
        })
        .collect();
    answers.push(Answer {
        label: said.next().unwrap_or_default(),
        carry: Carry::Stop,
        chooses: None,
    });
    Ok(Some(Question {
        action_id: stopped.action_id,
        app_id: stopped.app_id,
        body,
        answers,
    }))
}

/// The ways this game can be started, as a question with one answer each.
///
/// **Valve owns these words too**, and the same way it owns the others. The
/// options come from `SteamClient.Apps.GetLaunchOptionsForApp`, which is the
/// call the client's own dialog makes, and each label is that option's
/// description put through the client's localiser exactly as the client puts
/// it — a description that looks like a token is localised, and one that does
/// not is already a sentence somebody wrote in the game's own store page. Its
/// argument is that option's game name, which is what Valve hands its own.
///
/// `Ok(None)` for a client that offers nothing to choose between, or that no
/// longer has the words for its own question. Both mean the panel would be
/// worse than Valve's window, and the older answer is still there.
fn how_it_can_be_started(stopped: &Launching) -> Result<Option<Question>, Problem> {
    let socket = context()?;
    let said = evaluate(&socket, &ask_for_the_ways(stopped.app_id))?;
    the_ways_it_can_start(stopped, &said)
}

/// The expression [`how_it_can_be_started`] runs in the client.
///
/// Its own function so it can be read, and so it can be run — against a stub
/// of the three objects it reaches through, which is the only way to find out
/// that a program built out of a `format!` is a program at all. The client
/// answers a broken one with a syntax error and nothing else.
///
/// One round trip, because this only ever matters at the moment a launch has
/// already stopped and somebody is watching a loading screen.
///
/// `LocalizeString` leaves Valve's positional arguments in the sentence — see
/// [`fill_in`], which is the helper the client's own callers use and which is
/// done on this side. A description that is not a token is handed back
/// untouched, which is Valve's `LocalizeIfToken` and is what stops a plain
/// "Play OpenFront" being thrown away for not being in the table.
fn ask_for_the_ways(app_id: u32) -> String {
    format!(
        "(async () => {{ \
           const app = {app}; \
           const found = await SteamClient.Apps.GetLaunchOptionsForApp(app); \
           const say = token => window.LocalizationManager.LocalizeString(token) ?? ''; \
           return JSON.stringify({{ \
             name: window.appStore?.GetAppOverviewByAppID?.(app)?.display_name ?? '', \
             asked: say({sentence}), \
             refuse: say({cancel}), \
             ways: (found ?? []).map(one => {{ \
               const wrote = String(one.strDescription ?? ''); \
               return {{ index: Number(one.nIndex ?? 0), \
                        said: wrote.startsWith('#') ? (say(wrote) || wrote) : wrote, \
                        game: String(one.strGameName ?? '') }}; \
             }}) }}); \
         }})()",
        app = app_id,
        sentence = json_string(MAKE_A_SELECTION),
        cancel = json_string(CANCEL),
    )
}

/// One way a game can be started, as Valve's client offers it.
///
/// The list behind both halves of this: the panel a stopped launch puts up,
/// and the row of the game's own menu that sets which way it starts from now
/// on. One shape for both, because they are one question asked at two moments
/// and a menu that disagreed with the panel would be a setting nobody could
/// trust.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Way {
    /// The client's own number for it, which is what an answer carries —
    /// `ContinueGameAction(id, "1")`.
    pub index: u32,
    /// What to put on a row, in the language the client is running in.
    pub label: String,
    /// The option's own name, and the only part of this stable enough to
    /// remember a choice by. See [`Answer::chooses`].
    pub chooses: String,
}

/// Every way this game can be started here, asked of Valve's client.
///
/// For the menu rather than for a launch: nothing is waiting on an answer and
/// no game is starting, so it is asked whenever somebody wants to *set* which
/// way a game starts. An empty list is a game with nothing to choose between,
/// which is most of a library — the client answers with one option for those,
/// and one way is not a choice.
pub fn the_ways_to_start(app_id: u32) -> Result<Vec<Way>, Problem> {
    let socket = context()?;
    let said = evaluate(&socket, &ask_for_the_ways(app_id))?;
    Ok(what_the_client_offers(&said)?.ways)
}

/// What one answer to [`ask_for_the_ways`] comes to.
///
/// Its own function so the shapes a live client produces can be checked
/// without one, exactly as [`what_the_client_is_launching`] is. The shape
/// below was captured off a running client on 2026-09-19.
fn what_the_client_offers(answer: &str) -> Result<Offered, Problem> {
    #[derive(serde::Deserialize)]
    struct Said {
        #[serde(default)]
        name: String,
        #[serde(default)]
        asked: String,
        #[serde(default)]
        refuse: String,
        #[serde(default)]
        ways: Vec<SaidWay>,
    }
    #[derive(serde::Deserialize)]
    struct SaidWay {
        index: u32,
        #[serde(default)]
        said: String,
        #[serde(default)]
        game: String,
    }
    let said: Said =
        serde_json::from_str(answer).map_err(|_| Problem::Refused(answer.to_string()))?;
    let ways = said
        .ways
        .iter()
        .map(|way| {
            // The option's own words first, then the two names Valve would
            // have had on the screen anyway. Both options of one game
            // routinely carry the *same* description — `#Steam_LaunchOption_Game`,
            // which localises to "Play %1$s" — and what tells them apart is
            // the name filled into it, so that name is also what the choice is
            // remembered by.
            let label = [
                fill_in(&way.said, &[&way.game]),
                way.game.clone(),
                said.name.clone(),
            ]
            .into_iter()
            .find(|line| !line.trim().is_empty())
            .unwrap_or_default();
            Way {
                index: way.index,
                label,
                // The game's own name where the option has none, so that a
                // choice is still remembered by *something* rather than by an
                // empty string that would match every other nameless option.
                chooses: match way.game.trim().is_empty() {
                    true => said.name.clone(),
                    false => way.game.clone(),
                },
            }
        })
        .collect();
    Ok(Offered {
        name: said.name,
        asked: said.asked,
        refuse: said.refuse,
        ways,
    })
}

/// Everything one answer to [`ask_for_the_ways`] carries: the ways, and the
/// words the panel is built out of.
struct Offered {
    name: String,
    asked: String,
    refuse: String,
    ways: Vec<Way>,
}

/// What one answer from [`how_it_can_be_started`] comes to, as a panel.
fn the_ways_it_can_start(stopped: &Launching, answer: &str) -> Result<Option<Question>, Problem> {
    let offered = what_the_client_offers(answer)?;

    // Nothing to choose between. The client asked, so something is there to be
    // answered, but not by this shell: a panel whose only row is the refusal
    // is a panel that can only end the launch.
    if offered.ways.is_empty() {
        tracing::warn!(
            app_id = stopped.app_id,
            "the client stopped to ask how to start this game and then offered no way"
        );
        return Ok(None);
    }
    // Valve's own sentence, with the game's name in it. A client that has lost
    // the token for its own question comes back with nothing, and a panel with
    // a blank line and a blank button on it is worse than the window this is
    // replacing — the same refusal [`the_question`] makes for the same reason.
    let asking = fill_in(&offered.asked, &[&offered.name]);
    if asking.trim().is_empty() || offered.refuse.trim().is_empty() {
        tracing::warn!("Valve's client did not have the words for its own launch-option question");
        return Ok(None);
    }
    // An option with nothing at all to call it. Rare, and Valve's own blank
    // button — but a row somebody cannot read is a row they cannot choose, so
    // the whole panel is refused and the client is shown instead.
    if let Some(way) = offered.ways.iter().find(|way| way.label.trim().is_empty()) {
        tracing::warn!(
            app_id = stopped.app_id,
            index = way.index,
            "a launch option with nothing at all to call it"
        );
        return Ok(None);
    }

    let mut answers: Vec<Answer> = offered
        .ways
        .iter()
        .map(|way| Answer {
            // The number the client put on this option, written out, and
            // that is the whole of the protocol here —
            // `ContinueGameAction(id, "1")`. Taken from the option rather
            // than counted off the panel because that is what Valve's own
            // dialog sends, `nIndex.toString()`.
            carry: Carry::Go(way.index.to_string()),
            label: way.label.clone(),
            chooses: Some(way.chooses.clone()),
        })
        .collect();
    answers.push(Answer {
        label: offered.refuse,
        carry: Carry::Stop,
        // The way out is not a way to start the game, so it is not one of the
        // answers a choice can be remembered as. Somebody who cancels has
        // chosen nothing.
        chooses: None,
    });
    Ok(Some(Question {
        action_id: stopped.action_id,
        app_id: stopped.app_id,
        body: vec![asking],
        answers,
    }))
}

/// Put Valve's arguments into one of Valve's sentences.
///
/// Its localisation table holds positional `printf` specifiers — `%1$s` for the
/// first argument, `%2$s` for the second — and its own `LocalizeString` returns
/// them untouched; the substitution is a second helper, minified into a chunk
/// this shell has no business reaching into. So it is done here.
///
/// **Positional, and that is the point of doing it properly rather than
/// replacing the first `%s` found.** A translation is free to put the arguments
/// in the other order, and several do: the sentence about another session
/// playing names two games, and a language that puts the object first would
/// hand somebody the wrong one.
///
/// Anything this does not recognise is left exactly as it was. A sentence with
/// a specifier still in it reads oddly; a sentence this function has mangled
/// reads wrongly, and only one of those is worth risking.
fn fill_in(text: &str, arguments: &[&str]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('%') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        // `%%` is a per cent sign, and is the only escape in the format.
        if let Some(tail) = after.strip_prefix('%') {
            out.push('%');
            rest = tail;
            continue;
        }
        let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
        let tail = &after[digits.len()..];
        match (digits.parse::<usize>().ok(), tail.strip_prefix("$s")) {
            (Some(which), Some(tail)) if which >= 1 => {
                out.push_str(arguments.get(which - 1).copied().unwrap_or_default());
                rest = tail;
            }
            // Not a specifier this understands. Left whole, sign and all.
            _ => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// What the client calls the way out of a question.
///
/// The generic one rather than the per-dialog `#CloudPendingOps_Cancel` beside
/// each of these: they are all the same word, and one token that is certainly
/// there beats six that have to be kept in step with Valve's table. Checked
/// against the running client — `#LaunchApp_Cancel`, which the shape of the
/// others suggested, does not exist and comes back as nothing at all.
const CANCEL: &str = "#Button_Cancel";

/// Answer a launch the client has stopped.
///
/// The two calls its own dialogs make, and nothing else: `ContinueGameAction`
/// with the word the question takes, or `CancelGameAction`, which ends the
/// launch and starts nothing.
pub fn answer_the_launch(action_id: u32, carry: &Carry, still: Still<'_>) -> Result<(), Problem> {
    // A launch is answered into whichever client holds the port, and what it
    // does with the answer is start — or end — somebody's game. So the port is
    // bound to the client this was proved against before a word goes out on
    // it, and the ground is asked after the socket for the reason every other
    // flow here asks after its own waiting: that is the last instant before
    // the call.
    let socket = the_client_it_proved(still)?;
    let call = match carry {
        Carry::Go(word) => format!(
            "SteamClient.Apps.ContinueGameAction({action_id}, {}), 'sent'",
            json_string(word)
        ),
        Carry::Stop => format!("SteamClient.Apps.CancelGameAction({action_id}), 'sent'"),
    };
    evaluate(&socket, &call)?;
    tracing::info!(
        action_id,
        ?carry,
        "answered the launch Valve's client stopped"
    );
    Ok(())
}

/// The X keysym for Tab, which is what Valve's default overlay key is written
/// as in the client's settings store.
const TAB_KEYSYM: u32 = 0xff09;

/// What Valve's client will bring its overlay up on, and whether it will bring
/// one up at all.
///
/// The overlay is not the client's to raise — it lives inside the game, in the
/// library Steam preloads into it, and what that library is watching for is a
/// keystroke. So a shell that wants to offer the overlay to a controller has to
/// send that keystroke, and this is the only way to find out which one it is:
/// the setting lives in the running client's memory and is written to no file
/// until somebody changes it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct OverlayKey {
    /// The X keysym of the key itself. 65289 is `XK_Tab`.
    pub key_code: u32,
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub meta: bool,
    /// Valve's own spelling of the whole chord, as its settings page shows it.
    pub named: String,
    /// Whether the overlay is switched on at all. A client with it off raises
    /// nothing whatever key arrives.
    pub enabled: bool,
}

impl OverlayKey {
    /// Whether this is Valve's default — Shift with Tab, and nothing else held.
    ///
    /// The one chord a shell can send without a keymap to consult: Tab and
    /// Shift are in the same place on every layout xkb has, which is a large
    /// part of why Valve could pick them.
    pub fn is_shift_tab(&self) -> bool {
        self.key_code == TAB_KEYSYM && self.shift && !self.ctrl && !self.alt && !self.meta
    }
}

impl std::fmt::Display for OverlayKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let named = match self.named.trim() {
            "" => "unnamed",
            named => named,
        };
        match self.enabled {
            true => write!(f, "{named}"),
            false => write!(f, "{named}, but the overlay is switched off"),
        }
    }
}

/// Ask the running client what its overlay comes up on. See [`OverlayKey`].
///
/// Nothing here is required to exist, for the reason nothing in [`build`] is: a
/// client that has renamed the setting reports a keysym of zero rather than
/// failing, because what this is for is telling somebody why a chord did
/// nothing, and a diagnostic that cannot survive the thing it is diagnosing is
/// not one.
pub fn overlay_key() -> Result<OverlayKey, Problem> {
    let socket = context()?;
    let call = "(async () => {          const settings = window.settingsStore?.m_ClientSettings ?? {};          const key = settings.overlay_key ?? {};          return JSON.stringify({            key_code: key.key_code ?? 0,            shift: !!key.shift_key,            ctrl: !!key.ctrl_key,            alt: !!key.alt_key,            meta: !!key.meta_key,            named: key.display_name ?? '',            enabled: settings.enable_overlay !== false });        })()";
    let answer = evaluate(&socket, call)?;
    serde_json::from_str(&answer).map_err(|_| Problem::Refused(answer))
}

/// Whether the running client still has the methods this crate calls.
///
/// These are Valve's own internals and are not promised to anybody, so a
/// client update can take one away — and the shape of that failure is a press
/// that silently does nothing. This is how it is checked: it asks the client
/// what it has, rather than asking it to do something and reading the wreckage.
///
/// Returns the names that are missing, so an empty list is a client this shell
/// can drive.
pub fn missing_methods() -> Result<Vec<String>, Problem> {
    which_are_missing(&EVERYTHING_THIS_CRATE_CALLS)
}

/// What the running client *is*: the build it says it is, and the channel it
/// came down.
///
/// Asked beside [`missing_methods`] and for its sake. A capability check that
/// only says yes or no is a check nobody can act on when it one day says no:
/// the question that follows is always "which client", and by then the client
/// has updated itself again. This is what makes an answer attributable, and it
/// is the whole of what "a canary against stable and beta" needs from this
/// side — the run says which of the two it was, rather than somebody having to
/// remember.
///
/// Nothing here is required to exist. A client that has renamed either method
/// reports a version of zero rather than failing, because a diagnostic that
/// cannot survive the thing it is diagnosing is not one.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct Build {
    /// Valve's own version number for the client.
    pub version: u64,
    /// When it was built, worded as the client words it.
    pub built: String,
    /// Which update channel it is on. Zero is the public one that everybody
    /// gets; anything else is a beta somebody opted into by name.
    pub branch: u32,
    /// That channel's own name, empty on the public one.
    pub branch_name: String,
}

impl Build {
    /// Whether this is the client everybody gets, rather than a beta.
    pub fn is_public(&self) -> bool {
        self.branch == 0
    }
}

impl std::fmt::Display for Build {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let channel = match self.branch_name.trim() {
            "" if self.is_public() => "public".to_string(),
            "" => format!("beta {}", self.branch),
            named => format!("beta {named}"),
        };
        write!(f, "{} ({channel}, built {})", self.version, self.built)
    }
}

/// Ask the running client what it is. See [`Build`].
pub fn build() -> Result<Build, Problem> {
    let socket = context()?;
    let call = "(async () => {          const has = (group, method) =>            typeof SteamClient?.[group]?.[method] === 'function';          const info = has('System', 'GetSystemInfo')            ? await SteamClient.System.GetSystemInfo() : {};          const branch = has('Updates', 'GetCurrentOSBranch')            ? await SteamClient.Updates.GetCurrentOSBranch() : {};          return JSON.stringify({            version: info.nSteamVersion ?? 0,            built: info.sSteamBuildDate ?? '',            branch: branch?.eBranch ?? 0,            branch_name: branch?.sRawName ?? '' });        })()";
    let answer = evaluate(&socket, call)?;
    serde_json::from_str(&answer).map_err(|_| Problem::Refused(answer))
}

/// Every method in Valve's client this crate reaches for.
const EVERYTHING_THIS_CRATE_CALLS: [&str; 16] = [
    "Auth.SetLoginToken",
    "Settings.SetSetting",
    "Installs.RegisterForShowInstallWizard",
    "Installs.OpenInstallWizard",
    "Installs.SetCreateShortcuts",
    "Installs.SetInstallFolder",
    "Installs.ContinueInstall",
    "Installs.CancelInstall",
    "Installs.OpenUninstallWizard",
    "InstallFolder.GetInstallFolders",
    "InstallFolder.SetDefaultInstallFolder",
    "Apps.GetAvailableCompatTools",
    "Apps.RegisterForAppDetails",
    "Apps.SpecifyCompatTool",
    "Settings.GetGlobalCompatTools",
    "Settings.SpecifyGlobalCompatTool",
];

/// The ones an install walks, and the ones a removal does.
const INSTALLING_NEEDS: [&str; 5] = [
    "Installs.RegisterForShowInstallWizard",
    "Installs.OpenInstallWizard",
    "Installs.SetCreateShortcuts",
    "Installs.ContinueInstall",
    "Installs.CancelInstall",
];
const REMOVING_NEEDS: [&str; 1] = ["Installs.OpenUninstallWizard"];

/// Refuse before doing anything, where the client cannot do what is about to be
/// asked of it.
///
/// The check existed and production never made it: only the diagnostic example
/// asked, so a client update that renamed one of these turned a press into
/// nothing at all. The flow would open a wizard that never reported, sit out
/// its patience and come back with Valve's own unhelpful wording, or — worse —
/// take a step and stop between two of them.
///
/// Asked on every call rather than cached. It is one expression over a loopback
/// socket next to a flow that takes seconds, and a cached answer would have to
/// be invalidated when the client updates itself in the background, which is
/// exactly when it stops being true.
fn can_be_driven(needed: &[&str]) -> Result<(), Problem> {
    let missing = which_are_missing(needed)?;
    if missing.is_empty() {
        return Ok(());
    }
    // And which client it stopped being true of. Asked only here, on the one
    // path that has already failed, so it costs nothing in the ordinary case —
    // and it is the difference between a report somebody can act on and one
    // they cannot, because a client updates itself in the background and by
    // the time anybody reads this the build that broke has gone.
    tracing::warn!(
        ?missing,
        client = build().map(|build| build.to_string()).ok(),
        "this Steam client no longer has the methods this shell drives it with"
    );
    Err(Problem::Renamed(missing))
}

/// Which of these the running client does not have.
fn which_are_missing(wanted: &[&str]) -> Result<Vec<String>, Problem> {
    let socket = context()?;
    let wanted = wanted
        .iter()
        .map(|name| json_string(name))
        .collect::<Vec<_>>()
        .join(", ");
    let call = format!(
        "JSON.stringify([{wanted}].filter(name => {{ \
           const [group, method] = name.split('.'); \
           return typeof SteamClient?.[group]?.[method] !== 'function'; \
         }}))"
    );
    let answer = evaluate(&socket, &call)?;
    serde_json::from_str(&answer).map_err(|_| Problem::Refused(answer))
}

/// Have the running client fetch one game, without asking anybody anything.
///
/// ## Why this is driven by the client's own events
///
/// The install flow is not a function. It is a state machine the client walks
/// at its own pace, reporting where it has got to through
/// `RegisterForShowInstallWizard`, and it does not always get to the end: five
/// of its states are questions for the person rather than steps for the
/// program, and it sits in whichever one it reaches until somebody answers.
///
/// This used to be written as three calls in a row — open, no shortcuts,
/// continue — and then it returned `{ result: 1 }`, which was a constant the
/// expression had written into its own answer. Nothing was ever read back from
/// the client. So a flow that had stopped to ask a question came back
/// indistinguishable from one that had queued a download, and the row it was
/// pressed on said "Installing…" for the rest of the session with nothing
/// coming down and nothing said.
///
/// It is not a rare corner. `ShowEULAs` is the state a game with an agreement
/// stops in, and of fifteen titles taken off one real account, six had one —
/// Black Mesa among them, which sat in that state with no window, no download
/// and no error. The other four questions are [`Problem::Asks`], and the
/// wizard is cancelled on the way out, so the next press does not walk into a
/// flow that is still standing.
///
/// ## An agreement comes back as the agreements
///
/// `ShowEULAs` is answered with [`Problem::Agreements`]: the list the client's
/// own dialog would have walked (`SteamClient.Apps.LoadEula`), read *before*
/// the wizard is cancelled, so the shell can put each one on its own screen.
/// Nothing is accepted here. A shell that clicked through somebody's licence
/// agreement for them would be answering a question it was never asked; what
/// it may do is ask it, and pass on the answer.
///
/// That answer is `accepting`: the agreements a person has just pressed Accept
/// on, recorded with `MarkEulaAccepted` — the call Valve's own Accept button
/// makes, with the same three arguments — before the wizard is opened. The
/// wizard then finds them accepted and does not stop. Recorded first rather
/// than answered inside a parked wizard, because the client's own interface
/// reacts to `ShowEULAs` too: it builds a dialog of its own, hidden here, that
/// **cancels the install when it closes** unless its own workflow was the one
/// that finished. A wizard continued from outside that workflow is a wizard
/// that dialog cancels a moment later. Cancelling first and asking the next
/// wizard is the way round it, and it leaves nothing standing while somebody
/// reads for as long as they like.
///
/// Driving it from the events is also what makes the ordering right rather
/// than lucky: `ShowConfig` is the one state where continuing means anything,
/// and it is now waited for instead of assumed to have arrived.
///
/// ## Why it waits before it starts
///
/// A client that has signed in does not yet know what it owns. Signing on and
/// receiving the account's apps are separate things that finish seconds apart,
/// and the wizard cannot install a game the client has never heard of: asked
/// too early it walks two states and then fails, with an empty `rgApps`, a
/// required size of zero, and an error number that is different every time.
/// That failure is what the first install of a session used to be. See
/// [`knows_the_game`], which is the wait that fixes it, and which cost 1.3
/// seconds on the machine it was measured on.
///
/// ## Which library it goes into
///
/// `place` — see [`Place`]. On a machine with one library there is nothing to
/// choose and it is never looked at. With more, the wizard's own `ShowConfig`
/// is where Valve's dialog would have shown its folder list, and it is where
/// this either asks ([`Problem::WhereTo`], answered the way an agreement is:
/// cancelled, asked on the shell's panel, opened again with the answer) or
/// makes the call Valve's list makes, `Installs.SetInstallFolder`, for the
/// library it was handed.
///
/// A named library that is not there any more, or has not the room, is asked
/// about instead of fetched into. It was the person's choice, and it cannot be
/// honoured; Steam's default in its place would be a different choice that
/// nobody made.
///
/// A client that has lost the two calls a choice is made with installs where it
/// always did — into its own default — rather than refusing the game: the
/// question is a nicety over a download that works without it.
pub fn install(
    app_id: u32,
    accepting: &[Eula],
    place: &Place,
    still: Still<'_>,
) -> Result<(), Problem> {
    let (_turn, socket) = ready_to_drive_the_wizard(&INSTALLING_NEEDS, still)?;
    // `SetCreateShortcuts(false, false)` because the two it makes are a
    // desktop file and a start-menu entry on a machine that may have neither
    // — this shell *is* the menu, and the game is already a row on it.
    //
    // The two agreement calls are asked about here rather than in
    // `INSTALLING_NEEDS`, because most games have no agreement: a client that
    // renamed them should still install those, and only the one press that
    // needs them falls back to Steam's own window.
    let call = format!(
        "(async () => {{ \
           {knows} \
           const accepting = {accepting}; \
           if (accepting.length) {{ \
             if (typeof SteamClient.Apps.MarkEulaAccepted !== 'function') \
               return JSON.stringify({{ asks: 'an agreement to accept' }}); \
             for (const agreement of accepting) {{ \
               await SteamClient.Apps.MarkEulaAccepted(agreement.app, agreement.id, agreement.version); \
             }} \
           }} \
           const answer = await new Promise(settle => {{ \
             let done = false; \
             let going = false; \
             let reading = false; \
             let watch = null; \
             const finish = said => {{ \
               if (done) return; \
               done = true; \
               if (watch) {{ try {{ watch.unregister(); }} catch (ignored) {{}} }} \
               settle(said); \
             }}; \
             const asks = what => {{ \
               SteamClient.Installs.CancelInstall(); \
               finish({{ asks: what }}); \
             }}; \
             const failed = (said, error) => {{ \
               SteamClient.Installs.CancelInstall(); \
               finish({{ failed: said, error }}); \
             }}; \
             const agreements = async app => {{ \
               let listed = null; \
               if (typeof SteamClient.Apps.LoadEula === 'function') {{ \
                 try {{ listed = await SteamClient.Apps.LoadEula(app); }} catch (ignored) {{}} \
               }} \
               if (!Array.isArray(listed) || !listed.length) return asks('an agreement to accept'); \
               SteamClient.Installs.CancelInstall(); \
               finish({{ agreements: listed.map(one => ({{ \
                 app, id: one.id, version: one.version, url: one.url \
               }})) }}); \
             }}; \
             {placing} \
             watch = SteamClient.Installs.RegisterForShowInstallWizard(async where => {{ \
               switch (where.eInstallState) {{ \
                 case {config}: \
                   if (going) return; \
                   going = true; \
                   if (!(await placed(where))) return; \
                   await SteamClient.Installs.SetCreateShortcuts(false, false); \
                   await SteamClient.Installs.ContinueInstall(); \
                   return; \
                 case {complete}: return finish({{ ok: true }}); \
                 case {failed_state}: return failed( \
                   where.errorDetail || 'Steam could not start this download', \
                   where.eAppError || 0 \
                 ); \
                 case {canceled}: return failed('Steam stopped it', 0); \
                 case {eulas}: \
                   if (reading) return; \
                   reading = true; \
                   return agreements(where.currentAppID || {app_id}); \
                 case {cd_key}: return asks('a product key to type'); \
                 case {password}: return asks('a password to type'); \
                 case {media}: return asks('a disc to change'); \
                 case {signup}: return asks('an account to sign up for'); \
               }} \
             }}); \
             SteamClient.Installs.OpenInstallWizard([{app_id}]); \
             setTimeout(() => failed('Steam never got as far as starting it', 0), {patience}); \
           }}); \
           return JSON.stringify(answer); \
         }})()",
        knows = knows_the_game(app_id),
        accepting = accepting_literal(accepting),
        placing = placing(place),
        config = state::SHOW_CONFIG,
        complete = state::COMPLETE,
        failed_state = state::FAILED,
        canceled = state::CANCELED,
        eulas = state::SHOW_EULAS,
        cd_key = state::SHOW_CD_KEY,
        password = state::SHOW_PASSWORD,
        media = state::SHOW_CHANGE_MEDIA,
        signup = state::SHOW_SIGNUP,
        patience = UNTIL_THE_WIZARD_ANSWERS.as_millis(),
    );
    // Read for longer than the whole of it is allowed to take, so a wizard that
    // gives up says so itself rather than being cut off by the socket and
    // reported as a client that did not answer.
    let patience = UNTIL_IT_KNOWS_THE_GAME + UNTIL_THE_WIZARD_ANSWERS + PATIENCE;
    evaluate_within(&socket, &call, patience).and_then(|answer| wizard_said(&answer))
}

/// One agreement Valve's client will not install a game without, as the
/// client lists it (`SteamClient.Apps.LoadEula`).
///
/// Only what it takes to find the text and to record the answer. The text
/// itself is not the client's to give — its own dialog fetches it from `url`
/// — and is read by [`crate::agreement`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Eula {
    /// The app it belongs to: the wizard's `currentAppID`, which is nearly
    /// always the game that was pressed, and is carried rather than assumed
    /// because the wizard walks every app an install is made of.
    pub app_id: u32,
    /// Valve's name for it — `4000_eula_0` — which is also the key an
    /// acceptance is recorded under.
    pub id: String,
    /// Which edition of it this is. Accepting one edition does not accept the
    /// next: a publisher that rewrites its terms raises the number, and the
    /// client asks again.
    pub version: u32,
    /// Where its words are.
    pub url: String,
}

/// The agreements an [`install`] is to record as accepted, as a JavaScript
/// array literal.
fn accepting_literal(accepting: &[Eula]) -> String {
    serde_json::Value::Array(
        accepting
            .iter()
            .map(|eula| {
                serde_json::json!({
                    "app": eula.app_id,
                    "id": eula.id,
                    "version": eula.version,
                })
            })
            .collect(),
    )
    .to_string()
}

/// Which Steam library an [`install`] puts the game in.
///
/// Only ever a question on a machine with more than one: with a single library
/// there is one answer, and neither variant changes anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    /// Let the person choose. The wizard is cancelled before anything is
    /// fetched and the libraries come back as [`Problem::WhereTo`].
    Ask,
    /// This library, by the path Steam files it under
    /// ([`Library::path`]).
    ///
    /// `by_default` also makes it Steam's own default — the one its own install
    /// dialog starts on — so that a game started from Steam's window lands
    /// where one started from the shell does. That is what a standing choice
    /// in the shell's settings passes; a choice made for one game on the panel
    /// that asked is that game's answer and nothing more.
    In { path: String, by_default: bool },
}

/// One library a game could be installed into, as the client lists it
/// (`SteamClient.InstallFolder.GetInstallFolders`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Library {
    /// Where it is — the key a choice is made and remembered by, because the
    /// client's own index for it moves when a library before it is removed.
    pub path: String,
    /// The name somebody gave it in Steam's storage settings, empty when
    /// nobody did — which is most of them.
    pub label: String,
    /// Bytes free on the drive it is on, as the client counts them.
    pub free: u64,
    /// Whether it is Steam's own default, which is where its dialog starts.
    pub default: bool,
}

impl Library {
    /// Whether a game needing `needs` bytes fits, by the test Valve's dialog
    /// makes before it lets its Install button be pressed. A size that is not
    /// known fits everywhere, because refusing on a number nobody has would
    /// be refusing on nothing.
    pub fn fits(&self, needs: u64) -> bool {
        needs == 0 || needs < self.free
    }
}

/// What the wizard said when a game could go into more than one library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    /// How many bytes the game takes, or 0 where the wizard did not say.
    pub needs: u64,
    /// Every library that is there today, in Steam's own order.
    pub libraries: Vec<Library>,
}

/// JavaScript defining `placed(where)`, run at `ShowConfig`: `true` to go on
/// with the install, `false` having already answered with the libraries.
///
/// Asked only where Valve's own dialog would have asked — more than one
/// library that is there, and a game the wizard lets be moved — except that a
/// library that was *named* and is not there is asked about even when only one
/// is left, because that one is not what the person chose.
fn placing(place: &Place) -> String {
    let wanted = match place {
        Place::Ask => "null".to_string(),
        Place::In { path, by_default } => serde_json::json!({
            "path": path,
            "default": by_default,
        })
        .to_string(),
    };
    format!(
        "const placed = async where => {{ \
           const folders = SteamClient.InstallFolder; \
           if (typeof folders?.GetInstallFolders !== 'function' \
               || typeof SteamClient.Installs.SetInstallFolder !== 'function') return true; \
           if (!where.bCanChangeInstallFolder) return true; \
           let listed = []; \
           try {{ listed = await folders.GetInstallFolders(); }} catch (ignored) {{ return true; }} \
           const there = (Array.isArray(listed) ? listed : []).filter(one => \
             one && one.bIsMounted && typeof one.strFolderPath === 'string' && one.strFolderPath); \
           const wanted = {wanted}; \
           const ask = needs => {{ \
             SteamClient.Installs.CancelInstall(); \
             finish({{ where: {{ needs, libraries: there.map(one => ({{ \
               path: one.strFolderPath, label: one.strUserLabel || '', \
               free: one.nFreeSpace, default: !!one.bIsDefaultFolder \
             }})) }} }}); \
             return false; \
           }}; \
           if (!wanted) return there.length > 1 ? ask(where.nDiskSpaceRequired) : true; \
           const bare = path => String(path || '').replace(/\\/+$/, ''); \
           const folder = there.find(one => bare(one.strFolderPath) === bare(wanted.path)); \
           if (!folder) return there.length ? ask(where.nDiskSpaceRequired) : true; \
           if (wanted.default && !folder.bIsDefaultFolder \
               && typeof folders.SetDefaultInstallFolder === 'function') {{ \
             try {{ await folders.SetDefaultInstallFolder(folder.nFolderIndex); }} catch (ignored) {{}} \
           }} \
           let now = where; \
           if (where.iInstallFolder !== folder.nFolderIndex) {{ \
             const moved = await SteamClient.Installs.SetInstallFolder(folder.nFolderIndex); \
             if (moved && typeof moved === 'object') now = moved; \
             if (now.iInstallFolder !== undefined && now.iInstallFolder !== folder.nFolderIndex) \
               return ask(where.nDiskSpaceRequired); \
           }} \
           const needs = Number(now.nDiskSpaceRequired) || 0; \
           const room = Number(now.nDiskSpaceAvailable) || Number(folder.nFreeSpace) || 0; \
           if (needs > 0 && room > 0 && needs >= room) return ask(needs); \
           return true; \
         }}; "
    )
}

/// Read the libraries the wizard brought back when it stopped to ask where.
///
/// `None` for a list this cannot use — fewer than one library, or one with no
/// path — so the caller says the install failed rather than putting up a panel
/// whose buttons lead nowhere.
fn choice_said(said: &serde_json::Value) -> Option<Choice> {
    let libraries = said
        .get("libraries")?
        .as_array()?
        .iter()
        .map(|one| {
            Some(Library {
                path: one
                    .get("path")?
                    .as_str()
                    .filter(|path| !path.is_empty())?
                    .to_string(),
                label: one
                    .get("label")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_string(),
                free: bytes(one.get("free")),
                default: one
                    .get("default")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
            })
        })
        .collect::<Option<Vec<_>>>()?;
    if libraries.is_empty() {
        return None;
    }
    Some(Choice {
        needs: bytes(said.get("needs")),
        libraries,
    })
}

/// A byte count as the client writes one: a number, or — for a value too wide
/// for a JavaScript number to hold exactly — a string. Anything else is 0,
/// which every reader of these takes as "not known".
fn bytes(value: Option<&serde_json::Value>) -> u64 {
    match value {
        Some(serde_json::Value::Number(number)) => number
            .as_u64()
            .or_else(|| number.as_f64().filter(|n| *n > 0.0).map(|n| n as u64))
            .unwrap_or(0),
        Some(serde_json::Value::String(text)) => text.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

/// Make this library Steam's own default: where its install dialog starts, and
/// where a game started from its window goes.
///
/// The half of Settings > Games > Steam > Install games to that belongs to
/// Steam rather than to this shell, told on the press — see
/// [`crate::Steam::make_default_library`]. The call is the one Steam's own
/// storage page makes behind its "Make default" row.
///
/// No wizard is driven, so none is waited for. A library the client does not
/// list — it has been removed since the shell read the disk — is a refusal,
/// said as one.
pub fn make_default_library(path: &str, still: Still<'_>) -> Result<(), Problem> {
    can_be_driven(&DEFAULT_LIBRARY_NEEDS)?;
    let call = format!(
        "(async () => {{ \
           const bare = path => String(path || '').replace(/\\/+$/, ''); \
           const listed = await SteamClient.InstallFolder.GetInstallFolders(); \
           const folder = (Array.isArray(listed) ? listed : []) \
             .find(one => one && bare(one.strFolderPath) === bare({path})); \
           if (!folder) return JSON.stringify({{ result: 2, message: 'Steam does not list that library' }}); \
           if (!folder.bIsDefaultFolder) \
             await SteamClient.InstallFolder.SetDefaultInstallFolder(folder.nFolderIndex); \
           return JSON.stringify({{ result: 1 }}); \
         }})()",
        path = json_string(path),
    );
    let socket = the_client_it_proved(still)?;
    evaluate(&socket, &call).and_then(|answer| took_it(&answer))
}

/// What [`make_default_library`] reaches for.
const DEFAULT_LIBRARY_NEEDS: [&str; 2] = [
    "InstallFolder.GetInstallFolders",
    "InstallFolder.SetDefaultInstallFolder",
];

/// Something done to one of Steam's libraries as a whole, as Valve's own
/// storage page does it — Settings > Games > Steam > Storage is the shell's
/// copy of that page. Each carries the library's path, written as Steam
/// writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shelving {
    /// Make a new library here: Steam's **Add Drive**. For a drive, the path is
    /// the folder inside it Steam's own list proposes — see
    /// [`library_on_a_drive`] — because Steam refuses a drive's top folder.
    Add(String),
    /// Stop using this library: Steam's **Remove Library**. Nothing on the
    /// drive is deleted; Steam forgets the folder.
    Remove(String),
    /// Put right what is wrong with the folder's permissions and its own
    /// furniture: Steam's **Repair Folder**.
    Repair(String),
}

impl Shelving {
    /// The library it is about.
    pub fn path(&self) -> &str {
        match self {
            Shelving::Add(path) | Shelving::Remove(path) | Shelving::Repair(path) => path,
        }
    }
}

impl std::fmt::Display for Shelving {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Shelving::Add(path) => write!(f, "add the library {path}"),
            Shelving::Remove(path) => write!(f, "remove the library {path}"),
            Shelving::Repair(path) => write!(f, "repair the library {path}"),
        }
    }
}

/// Why Steam would not do something to a library or move a game between them.
///
/// Valve's client refuses these with a word of its own — a key its storage page
/// looks up in its catalogue, or an update error number — and the words are
/// sorted here into the cases a person can do something about, so that the
/// shell can say each in its own language and in plain terms. What Steam said
/// word for word goes to the log with the refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Declined {
    /// A drive's own top folder: Steam keeps a library in a folder *on* a
    /// drive, never in the drive itself. `NoDriveRoot`.
    DriveRoot,
    /// A folder that already has something in it that is not a library.
    /// `NotEmptyFolder`.
    NotEmpty,
    /// A folder this user cannot write into. `NotWritableFolder`.
    NotWritable,
    /// A drive mounted so that nothing on it may be run, which no game can be
    /// started from. `NotExecutableFolder`.
    NotExecutable,
    /// The drive has a library on it already. `DriveAlreadyHasLibrary`.
    AlreadyALibrary,
    /// Steam does not list that library, or no longer does.
    NotListed,
    /// Something is using the library, so it cannot be removed: the game Steam
    /// named, where it named one.
    InUse(Option<u32>),
    /// Where the game would go already has a folder of its name. Update error
    /// 15.
    FolderThere,
    /// The game shares its files with another. Update error 17.
    Shared,
    /// There is not enough room where it would go. Update error 12.
    NoRoom,
    /// The game is running. Update error 16.
    Running,
    /// Steam says this game cannot be moved at all. Update error 22.
    Unmovable,
    /// Another game is being moved, and the client moves one at a time.
    AnotherMove,
    /// Anything else, with Steam's own word for the log.
    Other(String),
}

impl Declined {
    /// Sort the word Steam's storage page refuses a library with.
    fn from_word(word: &str) -> Declined {
        match word.trim() {
            "NoDriveRoot" => Declined::DriveRoot,
            "NotEmptyFolder" => Declined::NotEmpty,
            "NotWritableFolder" => Declined::NotWritable,
            "NotExecutableFolder" => Declined::NotExecutable,
            "DriveAlreadyHasLibrary" => Declined::AlreadyALibrary,
            "NotListed" => Declined::NotListed,
            other => Declined::Other(other.to_string()),
        }
    }

    /// Sort the update error a move ended with. The numbers are Valve's own
    /// `EAppUpdateError`, and the four with a sentence of their own are the four
    /// its move dialog has one for, plus the two a person can act on.
    fn from_update_error(code: i64) -> Declined {
        match code {
            12 => Declined::NoRoom,
            15 => Declined::FolderThere,
            16 => Declined::Running,
            17 => Declined::Shared,
            22 => Declined::Unmovable,
            other => Declined::Other(format!("update error {other}")),
        }
    }
}

impl std::fmt::Display for Declined {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Declined::DriveRoot => write!(f, "Steam will not use the top folder of a drive"),
            Declined::NotEmpty => write!(f, "that folder is not empty"),
            Declined::NotWritable => write!(f, "that folder cannot be written to"),
            Declined::NotExecutable => write!(f, "nothing on that drive may be run"),
            Declined::AlreadyALibrary => write!(f, "that drive already has a library"),
            Declined::NotListed => write!(f, "Steam does not list that library"),
            Declined::InUse(Some(app_id)) => write!(f, "app {app_id} is using it"),
            Declined::InUse(None) => write!(f, "Steam is using it"),
            Declined::FolderThere => write!(f, "a folder of that name is already there"),
            Declined::Shared => write!(f, "it shares content with another game"),
            Declined::NoRoom => write!(f, "there is not enough room there"),
            Declined::Running => write!(f, "the game is running"),
            Declined::Unmovable => write!(f, "Steam cannot move it"),
            Declined::AnotherMove => write!(f, "another game is being moved"),
            Declined::Other(said) => write!(f, "{said}"),
        }
    }
}

/// Where Steam keeps a library it makes on a drive: a `SteamLibrary` folder at
/// the top of it.
///
/// The folder Valve's own Add Drive list proposes for every drive it offers —
/// `%s%c%s` beside `SteamLibrary` in `steamui.so` — and the reason the shell
/// asks for exactly this rather than for the drive: Steam refuses a drive's
/// top folder outright (`NoDriveRoot`).
pub fn library_on_a_drive(mounted_at: &std::path::Path) -> String {
    mounted_at
        .join("SteamLibrary")
        .to_string_lossy()
        .into_owned()
}

/// What the library calls reach for.
const ADDING_NEEDS: [&str; 1] = ["InstallFolder.AddInstallFolder"];
const REMOVING_LIBRARY_NEEDS: [&str; 2] = [
    "InstallFolder.GetInstallFolders",
    "InstallFolder.RemoveInstallFolder",
];
const REPAIRING_NEEDS: [&str; 3] = [
    "InstallFolder.GetInstallFolders",
    "InstallFolder.RepairInstallFolder",
    "InstallFolder.RegisterForRepairFolderFinished",
];
const MOVING_NEEDS: [&str; 3] = [
    "InstallFolder.GetInstallFolders",
    "InstallFolder.MoveInstallFolderForApp",
    "InstallFolder.RegisterForMoveContentProgress",
];
const CANCELLING_A_MOVE_NEEDS: [&str; 1] = ["InstallFolder.CancelMove"];

/// How long a repair is waited for. Valve's own dialog waits as long as it
/// takes; this waits long enough for any library measured, and says so in the
/// log rather than holding a panel up for ever.
const UNTIL_A_REPAIR_IS_DONE: Duration = Duration::from_secs(180);

/// JavaScript finding one library in the client's own list by its path, into
/// `folder`, or answering that it is not there.
fn the_library(path: &str) -> String {
    format!(
        "const bare = path => String(path || '').replace(/\\/+$/, ''); \
         const listed = await SteamClient.InstallFolder.GetInstallFolders(); \
         const folder = (Array.isArray(listed) ? listed : []) \
           .find(one => one && bare(one.strFolderPath) === bare({path})); \
         if (!folder) return JSON.stringify({{ result: 2, message: 'NotListed' }}); ",
        path = json_string(path),
    )
}

/// Do one thing to one library, the way Valve's storage page does it.
///
/// **Add** is the call behind its Add button: a folder Steam will keep games
/// in from now on, made if it is not there. **Remove** is Remove Library, which
/// forgets the folder and deletes nothing. **Repair** is Repair Folder, and it
/// is waited for — the client says when it has finished, not when it began.
///
/// A refusal comes back as [`Problem::Declined`], sorted into something the
/// shell can say in words; see [`Declined`].
pub fn shelve(job: &Shelving, still: Still<'_>) -> Result<(), Problem> {
    let (needs, call, patience): (&[&str], String, Duration) = match job {
        Shelving::Add(path) => (
            &ADDING_NEEDS,
            format!(
                "(async () => {{ \
                   try {{ await SteamClient.InstallFolder.AddInstallFolder({path}); }} \
                   catch (refused) {{ \
                     return JSON.stringify({{ result: 2, message: String(refused?.message ?? refused ?? '') }}); \
                   }} \
                   return JSON.stringify({{ result: 1 }}); \
                 }})()",
                path = json_string(path),
            ),
            PATIENCE,
        ),
        // Valve's own page reads a refusal's message as the app that is using
        // the library — a number above 7 — and as Steam itself otherwise.
        Shelving::Remove(path) => (
            &REMOVING_LIBRARY_NEEDS,
            format!(
                "(async () => {{ \
                   {find} \
                   try {{ await SteamClient.InstallFolder.RemoveInstallFolder(folder.nFolderIndex); }} \
                   catch (refused) {{ \
                     return JSON.stringify({{ result: 2, message: 'InUse', app: Number(refused?.message ?? 0) || 0 }}); \
                   }} \
                   return JSON.stringify({{ result: 1 }}); \
                 }})()",
                find = the_library(path),
            ),
            PATIENCE,
        ),
        // The registration is left in the page, like the downloads' one: it is
        // the same page however many times this connects to it, and a second
        // registration would count every finish twice.
        Shelving::Repair(path) => (
            &REPAIRING_NEEDS,
            format!(
                "(async () => {{ \
                   {find} \
                   if (!window.__lxb_repairs) {{ \
                     window.__lxb_repairs = {{ finished: 0, hook: null }}; \
                     window.__lxb_repairs.hook = SteamClient.InstallFolder.RegisterForRepairFolderFinished( \
                       () => {{ window.__lxb_repairs.finished += 1; }}); \
                   }} \
                   const before = window.__lxb_repairs.finished; \
                   try {{ await SteamClient.InstallFolder.RepairInstallFolder(folder.nFolderIndex); }} \
                   catch (refused) {{ \
                     return JSON.stringify({{ result: 2, message: String(refused?.message ?? refused ?? '') }}); \
                   }} \
                   const until = Date.now() + {patience}; \
                   while (window.__lxb_repairs.finished === before && Date.now() < until) {{ \
                     await new Promise(settle => setTimeout(settle, 250)); \
                   }} \
                   return JSON.stringify(window.__lxb_repairs.finished !== before \
                     ? {{ result: 1 }} : {{ result: 2, message: 'the repair did not say it had finished' }}); \
                 }})()",
                find = the_library(path),
                patience = UNTIL_A_REPAIR_IS_DONE.as_millis(),
            ),
            UNTIL_A_REPAIR_IS_DONE + PATIENCE,
        ),
    };
    can_be_driven(needs)?;
    let socket = the_client_it_proved(still)?;
    shelved(&evaluate_within(&socket, &call, patience)?)
}

/// What one answer from [`shelve`] comes to.
fn shelved(answer: &str) -> Result<(), Problem> {
    let parsed: serde_json::Value =
        serde_json::from_str(answer).map_err(|_| Problem::Refused(answer.to_string()))?;
    match parsed.get("result").and_then(serde_json::Value::as_i64) {
        Some(1) => Ok(()),
        Some(_) => {
            let word = parsed
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            Err(Problem::Declined(match word {
                "InUse" => Declined::InUse(
                    parsed
                        .get("app")
                        .and_then(serde_json::Value::as_u64)
                        .and_then(|app| u32::try_from(app).ok())
                        // Seven and under is Steam's own numbering for itself,
                        // not a game — Valve's page names "Steam" for those.
                        .filter(|app| *app > 7),
                ),
                other => Declined::from_word(other),
            }))
        }
        None => Err(Problem::Refused(answer.to_string())),
    }
}

/// How far a move has got, as Valve's client last said.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Moving {
    pub app_id: u32,
    /// How far along, 0 to 100 — `flProgress`, which the client counts in
    /// per cent, as its own dialog's bar is drawn from.
    pub percent: f32,
}

/// How a move ended that did not fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Moved {
    /// The game is in the library it was sent to.
    Done,
    /// Somebody stopped it, and it is where it was.
    Cancelled,
}

/// How often a move is asked how it is going. The client says so as often as
/// it likes; the bar only has to be as fresh as a person can see.
const MOVE_LOOK_EVERY: Duration = Duration::from_millis(500);

/// How long a move may go without a word before it is given up on. Long,
/// because a large game's last file can take a while on a slow drive, and a
/// move is never stopped by this — only no longer watched.
const MOVE_SILENCE: Duration = Duration::from_secs(600);

/// Move one game into another library, the way Valve's Move Content dialog
/// does, telling `heard` how far it has got as it goes.
///
/// The call is `MoveInstallFolderForApp`; how it is going arrives through a
/// registration, which is left in the page and read back on every look — the
/// shape [`downloading`] has, for the same reason: the client offers no getter.
/// A move ends when the client says so for this game: code 0 is done, 20 is
/// still going, and anything else is a refusal. One that was cancelled through
/// [`cancel_move`] ends as [`Moved::Cancelled`], whatever code it ends with.
pub fn move_game(
    app_id: u32,
    to: &str,
    still: Still<'_>,
    mut heard: impl FnMut(Moving),
) -> Result<Moved, Problem> {
    can_be_driven(&MOVING_NEEDS)?;
    // Valve's dialog offers only the libraries that do not already hold the
    // game, and a move into the one it is in is answered the same way.
    let start = format!(
        "(async () => {{ \
           {find} \
           if ((folder.vecApps || []).some(one => one && Number(one.nAppID) === {app_id})) \
             return JSON.stringify({{ result: 2, message: 'FolderThere' }}); \
           if (!window.__lxb_moving) {{ \
             window.__lxb_moving = {{ last: null, cancelled: false, hook: null }}; \
             window.__lxb_moving.hook = SteamClient.InstallFolder.RegisterForMoveContentProgress( \
               said => {{ window.__lxb_moving.last = said; }}); \
           }} \
           window.__lxb_moving.last = null; \
           window.__lxb_moving.cancelled = false; \
           try {{ await SteamClient.InstallFolder.MoveInstallFolderForApp({app_id}, folder.nFolderIndex); }} \
           catch (refused) {{ \
             const code = typeof refused === 'number' ? refused : Number(refused?.eError ?? refused?.message ?? -1); \
             return JSON.stringify({{ result: 2, message: 'refused', code: Number.isFinite(code) ? code : -1 }}); \
           }} \
           return JSON.stringify({{ result: 1 }}); \
         }})()",
        find = the_library(to),
    );
    let socket = the_client_it_proved(still)?;
    let answer = evaluate(&socket, &start)?;
    move_began(&answer)?;

    const LOOK: &str = "(async () => { \
        const moving = window.__lxb_moving; \
        if (!moving) return 'null'; \
        const last = moving.last; \
        return JSON.stringify({ \
          cancelled: !!moving.cancelled, \
          app: last ? Number(last.appid ?? 0) : 0, \
          code: last ? Number(last.eError ?? -1) : -1, \
          percent: last ? Number(last.flProgress ?? 0) : 0 }); \
      })()";
    let mut quiet_since = Instant::now();
    let mut last_percent = -1.0_f32;
    let mut misses = 0;
    loop {
        std::thread::sleep(MOVE_LOOK_EVERY);
        let said = match evaluate(&socket, LOOK).and_then(|answer| move_heard(&answer)) {
            Ok(said) => {
                misses = 0;
                said
            }
            // A look that did not land is tried again: the client may be busy
            // writing a file. Ten in a row is a client that has gone.
            Err(problem) => {
                misses += 1;
                if misses >= 10 {
                    return Err(problem);
                }
                continue;
            }
        };
        if let Some(ended) = said.ended(app_id) {
            return ended;
        }
        if let Some(percent) = said.moving(app_id) {
            if (percent - last_percent).abs() > f32::EPSILON {
                last_percent = percent;
                quiet_since = Instant::now();
                heard(Moving { app_id, percent });
            }
        }
        if quiet_since.elapsed() > MOVE_SILENCE {
            return Err(Problem::Unreachable(
                "the move stopped saying how it was going".to_string(),
            ));
        }
    }
}

/// Whether the first call of a move was taken.
fn move_began(answer: &str) -> Result<(), Problem> {
    let parsed: serde_json::Value =
        serde_json::from_str(answer).map_err(|_| Problem::Refused(answer.to_string()))?;
    match parsed.get("result").and_then(serde_json::Value::as_i64) {
        Some(1) => Ok(()),
        Some(_) => Err(Problem::Declined(
            match parsed.get("message").and_then(serde_json::Value::as_str) {
                Some("refused") => Declined::from_update_error(
                    parsed
                        .get("code")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or(-1),
                ),
                Some("FolderThere") => Declined::FolderThere,
                Some(word) => Declined::from_word(word),
                None => Declined::Other(answer.to_string()),
            },
        )),
        None => Err(Problem::Refused(answer.to_string())),
    }
}

/// One look at a move that is under way.
#[derive(Debug, Clone, Copy, PartialEq)]
struct MoveHeard {
    cancelled: bool,
    /// Which game the last word was about, or 0 before there has been one.
    app: u32,
    /// `eError` on that word: 0 done, 20 still going, anything else refused.
    code: i64,
    percent: f32,
}

impl MoveHeard {
    /// How the move of this game ended, where the last word says it has.
    fn ended(&self, app_id: u32) -> Option<Result<Moved, Problem>> {
        if self.app != app_id || self.code == STILL_MOVING || self.code < 0 {
            return None;
        }
        Some(match (self.code, self.cancelled) {
            (0, _) => Ok(Moved::Done),
            (_, true) => Ok(Moved::Cancelled),
            (code, false) => Err(Problem::Declined(Declined::from_update_error(code))),
        })
    }

    /// How far along it is, where the last word is about this game and says
    /// it is still going.
    fn moving(&self, app_id: u32) -> Option<f32> {
        (self.app == app_id && self.code == STILL_MOVING).then(|| self.percent.clamp(0.0, 100.0))
    }
}

/// `k_EAppUpdateErrorBusy`, which is what the client says of a move on every
/// word but its last.
const STILL_MOVING: i64 = 20;

fn move_heard(answer: &str) -> Result<MoveHeard, Problem> {
    if answer.trim() == "null" {
        return Ok(MoveHeard {
            cancelled: false,
            app: 0,
            code: -1,
            percent: 0.0,
        });
    }
    #[derive(serde::Deserialize)]
    struct Said {
        cancelled: bool,
        app: u32,
        code: i64,
        percent: f32,
    }
    let said: Said =
        serde_json::from_str(answer).map_err(|_| Problem::Refused(answer.to_string()))?;
    Ok(MoveHeard {
        cancelled: said.cancelled,
        app: said.app,
        code: said.code,
        percent: said.percent,
    })
}

/// Stop the move that is under way, as the Cancel button on Valve's own dialog
/// does. The game stays where it was. What says it has stopped is the move's
/// own watch — see [`move_game`] — which this marks as cancelled first.
pub fn cancel_move(still: Still<'_>) -> Result<(), Problem> {
    can_be_driven(&CANCELLING_A_MOVE_NEEDS)?;
    let socket = the_client_it_proved(still)?;
    let call = "(async () => { \
        if (window.__lxb_moving) window.__lxb_moving.cancelled = true; \
        await SteamClient.InstallFolder.CancelMove(); \
        return JSON.stringify({ result: 1 }); \
      })()";
    evaluate(&socket, call).and_then(|answer| took_it(&answer))
}

/// Read the list the wizard brought back when it stopped on an agreement.
///
/// `None` for anything that is not all there, so the caller falls back to
/// Steam's own window rather than putting half an agreement on the screen — or
/// recording an acceptance under a name or an edition nobody was shown.
fn agreements_said(listed: &serde_json::Value) -> Option<Vec<Eula>> {
    let listed = listed.as_array()?;
    if listed.is_empty() {
        return None;
    }
    listed
        .iter()
        .map(|one| {
            let version = one.get("version")?;
            Some(Eula {
                app_id: u32::try_from(one.get("app")?.as_u64()?).ok()?,
                id: one
                    .get("id")?
                    .as_str()
                    .filter(|id| !id.is_empty())?
                    .to_string(),
                // A number in every one seen, and a number in the file the
                // client records it in; written out as text is taken too,
                // because the call it is handed back to does not mind which.
                version: version
                    .as_u64()
                    .and_then(|number| u32::try_from(number).ok())
                    .or_else(|| version.as_str()?.trim().parse().ok())?,
                url: one
                    .get("url")?
                    .as_str()
                    .filter(|url| !url.is_empty())?
                    .to_string(),
            })
        })
        .collect()
}

/// JavaScript that waits for the client to know what one game is, and gives up
/// after [`UNTIL_IT_KNOWS_THE_GAME`].
///
/// The client's own catalogue of what the account owns, which is filled in from
/// the network some seconds after it has signed on. Until then every question
/// about a game has the same answer — that there is no such game — and the two
/// flows that name one, installing and removing, both fail on it.
///
/// A question this cannot ask counts as answered. If Valve ever renames the
/// store, `GetAppOverviewByAppID` stops being a function and this waits for
/// nothing at all rather than for the whole of the deadline: the shell would
/// then be back to the press it has always made, which is worse than this and
/// far better than a press that sits there for twenty seconds first.
fn knows_the_game(app_id: u32) -> String {
    format!(
        "const knows = () => {{ \
           const look = window.appStore?.GetAppOverviewByAppID; \
           if (typeof look !== 'function') return true; \
           try {{ return !!look.call(window.appStore, {app_id}); }} catch (ignored) {{ return true; }} \
         }}; \
         const knownBy = Date.now() + {patience}; \
         while (!knows() && Date.now() < knownBy) {{ \
           await new Promise(settle => setTimeout(settle, 200)); \
         }} ",
        patience = UNTIL_IT_KNOWS_THE_GAME.as_millis(),
    )
}

/// Stop one that is going, and take away what had arrived.
///
/// The same call as [`uninstall`], and not the download list. Taking the app
/// out of the download list is what the client's own pause button does: it
/// stops the transfer and leaves everything where it is — a manifest that
/// still says the game is halfway installed, and the part-built copy under
/// `steamapps/downloading`. Measured on a stopped 140 MB install, that was
/// 368 MB left on the disk and a row that read "Downloading" for ever after.
///
/// There is no resuming a download from this shell, so what had arrived is of
/// no use to a later attempt, and the promise the panel makes when it offers to
/// stop — that nothing is left behind — has to be true.
pub fn stop_installing(app_id: u32, still: Still<'_>) -> Result<(), Problem> {
    let (_turn, socket) = ready_to_drive_the_wizard(&INSTALLING_NEEDS, still)?;
    // The wizard first, in case this is a press that landed before the flow
    // had finished: a wizard left standing is one the next install walks into.
    let call = format!(
        "(async () => {{ \
           {knows} \
           await SteamClient.Installs.CancelInstall(); \
           await SteamClient.Installs.OpenUninstallWizard([{app_id}], true); \
           return JSON.stringify({{ result: 1 }}); \
         }})()",
        knows = knows_the_game(app_id),
    );
    evaluate_within(&socket, &call, UNTIL_IT_KNOWS_THE_GAME + PATIENCE)
        .and_then(|answer| took_it(&answer))
}

/// Take one game off the disk, without putting anything on the screen.
///
/// The second argument is the whole point: it is what the client's own library
/// passes when the user has already been asked, and it is what turns the
/// removal from a dialog into an action. Without it — which is what
/// `steam://uninstall/<id>` does — the client raises its own confirmation
/// window over whatever the shell was drawing, which is the one thing this
/// integration exists to avoid.
///
/// Being asked first is not skipped, only moved: the shell asks in its own
/// panel, in its own voice, before this is called. See the Uninstall row of the
/// game menu.
///
/// Nothing comes back but that the client took the request. What says the game
/// has gone is its manifest leaving the disk, which is where every other fact
/// about an installed game is already read from — so the row changes when the
/// library is next looked at rather than when this returns.
///
/// It waits for the same thing an install does, and for the same reason: this
/// names a game, and a client that has not yet been told what it owns has
/// nothing to match the name against. See [`knows_the_game`]. The row it is
/// pressed on was read off the disk, which is ready long before the client is.
pub fn uninstall(app_id: u32, still: Still<'_>) -> Result<(), Problem> {
    let (_turn, socket) = ready_to_drive_the_wizard(&REMOVING_NEEDS, still)?;
    let call = format!(
        "(async () => {{ \
           {knows} \
           await SteamClient.Installs.OpenUninstallWizard([{app_id}], true); \
           return JSON.stringify({{ result: 1 }}); \
         }})()",
        knows = knows_the_game(app_id),
    );
    evaluate_within(&socket, &call, UNTIL_IT_KNOWS_THE_GAME + PATIENCE)
        .and_then(|answer| took_it(&answer))
}

/// Whose compatibility is being asked about.
///
/// Valve keeps these two apart in its own interface — a game's properties call
/// `Apps.SpecifyCompatTool`, the settings page calls
/// `Settings.SpecifyGlobalCompatTool` — and they are kept apart here rather
/// than folded into "app 0", which is how the *file* on the disk writes the
/// second one. Two calls that happen to agree about a sentinel are still two
/// calls, and a caller that passed 0 by accident would silently change what
/// every unverified game on the machine runs under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Which {
    /// One title, as its properties window asks it.
    Game(u32),
    /// Every title Valve has not verified: Steam's own default for "all other
    /// titles", which is what a game with nothing forced on it falls back to.
    OtherTitles,
}

impl std::fmt::Display for Which {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Which::Game(app_id) => write!(f, "app {app_id}"),
            Which::OtherTitles => f.write_str("other titles"),
        }
    }
}

/// One Steam Play compatibility tool, as Valve's client lists it.
///
/// Two names, because they are two different things and only one of them is an
/// identifier. `name` is what Steam files a choice under — `proton_experimental`
/// for Valve's own, and for a third-party tool the name it declares itself with
/// in `compatibilitytools.d` — and `display` is what it calls itself on a
/// screen. Sending the display name back would be sending Valve its own label
/// where it asked for a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tool {
    pub name: String,
    pub display: String,
}

/// What Steam says about running one title — or every unverified title — under
/// a compatibility tool.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Compatibility {
    /// Every tool it may be run with, in the order Valve's own list gives them.
    ///
    /// **Not what is on the disk**, and that is the whole reason this is asked
    /// of the client at all. A machine with three Protons installed is offered
    /// eleven: Valve's client knows every compatibility tool the account may
    /// use and fetches the one that is chosen, so nothing readable here can
    /// produce the list somebody sees in Steam's own properties window. See
    /// [`crate::library::Game`], whose `tool` flag is the other half — the
    /// tools that *are* on the disk, which are kept off the bar.
    pub tools: Vec<Tool>,
    /// The one being forced, by [`Tool::name`], or nothing where Steam is left
    /// to choose for itself.
    pub forced: Option<String>,
}

impl Compatibility {
    /// The tool being forced, as it is written on a screen.
    ///
    /// The client's own label where the list has it, and the bare name where it
    /// does not — a tool somebody has since deleted out of
    /// `compatibilitytools.d` is still what the game is set to run under, and
    /// saying its name is better than saying nothing.
    pub fn forced_display(&self) -> Option<&str> {
        let forced = self.forced.as_deref()?;
        Some(
            self.tools
                .iter()
                .find(|tool| tool.name == forced)
                .map_or(forced, |tool| tool.display.as_str()),
        )
    }
}

/// The methods each half of this is driven with.
const COMPATIBILITY_NEEDS: [&str; 2] =
    ["Apps.GetAvailableCompatTools", "Apps.RegisterForAppDetails"];
const FORCING_NEEDS: [&str; 1] = ["Apps.SpecifyCompatTool"];
const OTHER_TITLES_NEEDS: [&str; 1] = ["Settings.GetGlobalCompatTools"];
const FORCING_OTHER_TITLES_NEEDS: [&str; 1] = ["Settings.SpecifyGlobalCompatTool"];

/// How long to wait for a client that has just come up to know which
/// compatibility tools there are — see [`knows_its_tools`].
///
/// The same wait an install makes for the same reason, and the same length. A
/// client that has signed in has not yet received its own app data, and until
/// it does `GetAvailableCompatTools` answers with the tools *on the disk* and
/// nothing else — which on the machine this was measured on is three, against
/// the nineteen the same call gives a second later.
const UNTIL_IT_KNOWS_ITS_TOOLS: Duration = Duration::from_secs(20);

/// How long the client is given to say what one game's details are.
///
/// Short, because there is nothing to fetch: the client answers a registration
/// for an app it knows out of the store it keeps in memory, and one it has
/// never heard of never answers at all. The list arrives either way — this
/// only decides how long a menu waits before it opens with nothing ticked.
const UNTIL_IT_DESCRIBES_THE_GAME: Duration = Duration::from_secs(5);

/// Which tools Steam offers for this, and which of them it is set to use.
///
/// Both halves come from the client and neither is read off the disk, which is
/// deliberate. `config.vdf` does hold the mapping — that is where Steam
/// persists it — but it is written *out of* a running client's memory, so
/// while one is up the file is behind it, and a client being up is the one
/// condition this call is ever made under.
/// Bound to the client the caller proved, like the calls that change
/// something, and for a reason a read shares with them: this is what the tick
/// beside the row is drawn from, and a list read off a client that replaced
/// the proved one is a tick drawn beside a choice nobody made.
pub fn compatibility(which: Which, still: Still<'_>) -> Result<Compatibility, Problem> {
    match which {
        Which::Game(app_id) => one_game(app_id, still),
        Which::OtherTitles => other_titles(still),
    }
}

/// The list Valve's own properties window is built from, and the tick on it.
///
/// Two questions in one expression, because they are one screen: the tools come
/// back from a plain call, and which is forced is a registration the client
/// answers out of its own app store. It is the *priority* that says forced
/// rather than the name being set — a game Valve has whitelisted carries a tool
/// name too, and drawing that as a choice somebody made would put a tick beside
/// a row nobody had pressed. See [`FORCED`].
fn one_game(app_id: u32, still: Still<'_>) -> Result<Compatibility, Problem> {
    can_be_driven(&COMPATIBILITY_NEEDS)?;
    let socket = the_client_it_proved(still)?;
    let call = format!(
        "(async () => {{ \
           {ready} \
           const tools = await SteamClient.Apps.GetAvailableCompatTools({app_id}); \
           const details = await new Promise(settle => {{ \
             let watch = null; \
             let done = false; \
             const finish = said => {{ \
               if (done) return; \
               done = true; \
               if (watch) {{ try {{ watch.unregister(); }} catch (ignored) {{}} }} \
               settle(said); \
             }}; \
             try {{ \
               watch = SteamClient.Apps.RegisterForAppDetails({app_id}, said => finish(said)); \
               if (done && watch) {{ try {{ watch.unregister(); }} catch (ignored) {{}} }} \
             }} catch (ignored) {{ finish(null); }} \
             setTimeout(() => finish(null), {patience}); \
           }}); \
           return JSON.stringify({{ \
             tools: (tools ?? []).map(one => ({{ \
               name: String(one.strToolName ?? ''), \
               display: String(one.strDisplayName ?? '') }})), \
             forced: details && Number(details.nCompatToolPriority ?? 0) === {forced} \
               ? String(details.strCompatToolName ?? '') : '' }}); \
         }})()",
        ready = knows_its_tools(),
        patience = UNTIL_IT_DESCRIBES_THE_GAME.as_millis(),
        forced = FORCED,
    );
    what_steam_offers(&evaluate_within(
        &socket,
        &call,
        UNTIL_IT_KNOWS_ITS_TOOLS + UNTIL_IT_DESCRIBES_THE_GAME + PATIENCE,
    )?)
}

/// The same, for everything Valve has not verified.
///
/// Its own list rather than the one above asked about app 0: Valve's settings
/// page calls `GetGlobalCompatTools`, and the two lists are not promised to
/// agree — a tool that can run one game is not a tool that may stand as the
/// default for a library.
fn other_titles(still: Still<'_>) -> Result<Compatibility, Problem> {
    can_be_driven(&OTHER_TITLES_NEEDS)?;
    let socket = the_client_it_proved(still)?;
    // `settingsStore` is the same object [`leave_the_guide_button_alone`]
    // reads, and `settings.strCompatTool` is the field Valve's own dropdown
    // takes its selected row from. Guarded at every step, because it is the
    // client's private furniture: a client that has renamed it answers with
    // nothing forced rather than throwing, and the list — which is a promised
    // method and is checked for above — still arrives.
    let call = format!(
        "(async () => {{ \
           {ready} \
           const tools = await SteamClient.Settings.GetGlobalCompatTools(); \
           let forced = ''; \
           try {{ forced = String(window.settingsStore?.settings?.strCompatTool ?? ''); }} \
           catch (ignored) {{}} \
           return JSON.stringify({{ \
             tools: (tools ?? []).map(one => ({{ \
               name: String(one.strToolName ?? ''), \
               display: String(one.strDisplayName ?? '') }})), \
             forced }}); \
         }})()",
        ready = knows_its_tools(),
    );
    what_steam_offers(&evaluate_within(
        &socket,
        &call,
        UNTIL_IT_KNOWS_ITS_TOOLS + PATIENCE,
    )?)
}

/// JavaScript that waits for the client to have its own app data, and gives up
/// after [`UNTIL_IT_KNOWS_ITS_TOOLS`].
///
/// The list of tools has two halves and they do not arrive together. The ones
/// declared in `compatibilitytools.d` are read off the disk and are there
/// immediately; Valve's own — every Proton, the Linux runtimes — come with the
/// client's app data, and until that lands the call answers with the first half
/// alone. A shell that asked too early would show three tools where Steam's own
/// properties window shows nineteen, and would remember the three.
///
/// Measured on this machine, polling a client from the moment it was started
/// again: at 2.8 s `GetAvailableCompatTools` still answered 3, at 4.0 s it
/// answered 19, and `appStore.m_bIsInitialized` turned true at 4.5 s. So the
/// flag is *behind* the list rather than ahead of it, which is what makes it
/// safe to wait on: a client that says it is initialised has the whole list.
///
/// Shaped like [`knows_the_game`] and for its reasons: a client whose internals
/// have been renamed answers `true` and is asked at once, because the
/// alternative is a menu that waits twenty seconds for a field that will never
/// appear.
fn knows_its_tools() -> String {
    format!(
        "const ready = () => {{ \
           const store = window.appStore; \
           if (!store || !('m_bIsInitialized' in store)) return true; \
           return store.m_bIsInitialized === true; \
         }}; \
         const readyBy = Date.now() + {patience}; \
         while (!ready() && Date.now() < readyBy) {{ \
           await new Promise(settle => setTimeout(settle, 200)); \
         }} ",
        patience = UNTIL_IT_KNOWS_ITS_TOOLS.as_millis(),
    )
}

/// The priority Steam writes against a tool somebody chose for one game, as
/// against one it chose for itself.
///
/// Valve's own numbers, read out of the client that draws the checkbox: 250 is
/// a per-title choice, 75 is the default for other titles, and 100 is a game
/// Valve has whitelisted. Its own constant because the number is the whole of
/// the difference between "this is set" and "somebody set this".
const FORCED: u32 = 250;

/// What one answer to either of those comes to.
///
/// Its own function so the shapes a live client produces can be checked without
/// one, in the way [`what_the_client_is_launching`] is.
fn what_steam_offers(answer: &str) -> Result<Compatibility, Problem> {
    #[derive(serde::Deserialize)]
    struct Said {
        tools: Vec<SaidTool>,
        forced: String,
    }
    #[derive(serde::Deserialize)]
    struct SaidTool {
        name: String,
        display: String,
    }
    let said: Said =
        serde_json::from_str(answer).map_err(|_| Problem::Refused(answer.to_string()))?;
    Ok(Compatibility {
        tools: said
            .tools
            .into_iter()
            // A tool with no name is one nothing could be sent back about.
            .filter(|tool| !tool.name.is_empty())
            .map(|tool| Tool {
                display: match tool.display.is_empty() {
                    // Valve's own list has always carried both; a client that
                    // stops sending the label leaves rows that can still be
                    // read rather than a list of blanks.
                    true => tool.name.clone(),
                    false => tool.display,
                },
                name: tool.name,
            })
            .collect(),
        forced: (!said.forced.is_empty()).then_some(said.forced),
    })
}

/// Run it under this tool from now on, or under whatever Steam chooses.
///
/// The empty string is Valve's own way of saying "stop forcing one" — it is
/// what its checkbox sends when it is turned off — so this takes an `Option`
/// and writes that, rather than offering a second method for the same call.
///
/// Nothing here restarts anything. A per-title choice is read when the game is
/// next started, so it takes effect on the next press; the default for other
/// titles is read by the client as it comes up, which is why the row that sets
/// it says so.
pub fn force(which: Which, tool: Option<&str>, still: Still<'_>) -> Result<(), Problem> {
    let tool = tool.unwrap_or_default();
    let call = match which {
        Which::Game(app_id) => {
            can_be_driven(&FORCING_NEEDS)?;
            format!(
                "(async () => {{ \
                   await SteamClient.Apps.SpecifyCompatTool({app_id}, {tool}); \
                   return JSON.stringify({{ result: 1 }}); \
                 }})()",
                tool = json_string(tool),
            )
        }
        Which::OtherTitles => {
            can_be_driven(&FORCING_OTHER_TITLES_NEEDS)?;
            format!(
                "(async () => {{ \
                   await SteamClient.Settings.SpecifyGlobalCompatTool({tool}); \
                   return JSON.stringify({{ result: 1 }}); \
                 }})()",
                tool = json_string(tool),
            )
        }
    };
    // No wizard to queue behind, and the checks are still worth their
    // microsecond: `can_be_driven` and `context` are a round trip into the
    // client apiece, and this is what a title runs under from its next press
    // onwards — on whichever client is holding the port by then.
    let socket = the_client_it_proved(still)?;
    evaluate(&socket, &call).and_then(|answer| took_it(&answer))
}

/// Where the client's install flow can be, as Valve's own enumeration numbers
/// them.
///
/// Only the states this shell has an answer for are named. The rest — waiting
/// on a licence, waiting on the app's information, creating the app, reading
/// from media — are steps the flow walks through on its own, and the right
/// thing to do about every one of them is nothing.
mod state {
    pub const SHOW_CD_KEY: u8 = 4;
    pub const SHOW_PASSWORD: u8 = 6;
    /// Everything it wanted to ask has been answered, and this is the one
    /// state in which continuing means anything.
    pub const SHOW_CONFIG: u8 = 7;
    pub const SHOW_EULAS: u8 = 8;
    pub const SHOW_CHANGE_MEDIA: u8 = 11;
    pub const SHOW_SIGNUP: u8 = 13;
    /// The download is queued. Not that the game has arrived — that takes as
    /// long as it takes, and the manifest on the disk is what tells anybody
    /// how it is going.
    pub const COMPLETE: u8 = 14;
    pub const FAILED: u8 = 15;
    pub const CANCELED: u8 = 16;
}

/// What the install flow came back with.
///
/// Its own reader rather than [`took_it`], because the two answer different
/// questions and used to be run together. A call the client refuses says so
/// with an `EResult`; the wizard is a state machine and ends in one of three
/// places — it queued the download, it stopped to ask the person something, or
/// it failed. The number it carries when it fails is an `EAppUpdateError` and
/// not an `EResult` at all, so it is neither compared against 1 nor named as
/// one.
///
/// `asks` is read first and outranks everything else: nothing has gone wrong
/// there, and the flow was cancelled on the way out so it would otherwise
/// arrive as a cancellation somebody had asked for.
fn wizard_said(answer: &str) -> Result<(), Problem> {
    let parsed: serde_json::Value =
        serde_json::from_str(answer).map_err(|_| Problem::Refused(answer.to_string()))?;
    if let Some(what) = parsed.get("asks").and_then(serde_json::Value::as_str) {
        return Err(Problem::Asks(what.to_string()));
    }
    // An agreement the wizard stopped on, with the client's list of what it
    // wants accepted. A list that cannot be read in full is the older answer:
    // Steam's own window, which can still ask it.
    if let Some(listed) = parsed.get("agreements") {
        return Err(match agreements_said(listed) {
            Some(eulas) => Problem::Agreements(eulas),
            None => {
                tracing::warn!(%listed, "the client listed agreements this shell could not read");
                Problem::Asks("an agreement to accept".to_string())
            }
        });
    }
    // More than one place the game could go, and the person is to say which.
    // The expression only offers libraries that have a path, so a list this
    // cannot read is an answer this cannot read, like any other.
    if let Some(said) = parsed.get("where") {
        return Err(match choice_said(said) {
            Some(choice) => Problem::WhereTo(choice),
            None => {
                tracing::warn!(%said, "the client listed libraries this shell could not read");
                Problem::Refused(answer.to_string())
            }
        });
    }
    if parsed.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
        return Ok(());
    }
    let Some(said) = parsed.get("failed").and_then(serde_json::Value::as_str) else {
        return Err(Problem::Refused(answer.to_string()));
    };
    // Valve's own wording where it gave any, and its error number after it —
    // which is all there is to go on, because `errorDetail` has been empty
    // every time this has been seen fail.
    let error = parsed.get("error").and_then(serde_json::Value::as_i64);
    Err(Problem::Stopped(match error {
        Some(0) | None => said.to_string(),
        Some(number) => format!("{said} (error {number})"),
    }))
}

/// Whether the client's answer means it did what it was asked.
///
/// `EResult` 1 is `k_EResultOK` and everything else is a refusal that says
/// why — the same numbering every other Steam answer uses.
fn took_it(answer: &str) -> Result<(), Problem> {
    let parsed: serde_json::Value =
        serde_json::from_str(answer).map_err(|_| Problem::Refused(answer.to_string()))?;
    let result = parsed.get("result").and_then(serde_json::Value::as_i64);
    match result {
        Some(1) => Ok(()),
        Some(other) => {
            let said = parsed
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("no reason given");
            Err(Problem::Refused(format!("{said} (EResult {other})")))
        }
        None => Err(Problem::Refused(answer.to_string())),
    }
}

/// The debugger address of the client's shared context.
fn context() -> Result<String, Problem> {
    let listing = ureq::get(&format!("http://127.0.0.1:{PORT}/json"))
        .config()
        .timeout_global(Some(PATIENCE))
        .build()
        .call()
        .map_err(|_| Problem::NotExposed)?
        .body_mut()
        .read_to_string()
        .map_err(|error| Problem::Unreachable(error.to_string()))?;

    let pages: Vec<serde_json::Value> =
        serde_json::from_str(&listing).map_err(|error| Problem::Unreachable(error.to_string()))?;
    pages
        .iter()
        .find(|page| {
            page.get("title")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|title| title == CONTEXT)
        })
        .and_then(|page| page.get("webSocketDebuggerUrl"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or(Problem::NoContext)
}

/// Run one expression in that context and give back what it evaluated to.
fn evaluate(socket: &str, expression: &str) -> Result<String, Problem> {
    evaluate_within(socket, expression, PATIENCE)
}

/// The same, for an expression that is allowed to take longer than a call and
/// a reply — which is the install flow and nothing else.
fn evaluate_within(socket: &str, expression: &str, patience: Duration) -> Result<String, Problem> {
    let unreachable = |error: tungstenite::Error| Problem::Unreachable(error.to_string());

    let (mut stream, _) = tungstenite::connect(socket).map_err(unreachable)?;
    // Or the deadline below would be decoration: a read with no timeout on a
    // client that has stopped saying anything blocks for as long as the socket
    // stays open, which is longer than anybody is prepared to watch a loading
    // screen for.
    if let tungstenite::stream::MaybeTlsStream::Plain(plain) = stream.get_ref() {
        let _ = plain.set_read_timeout(Some(patience));
    }
    let request = serde_json::json!({
        "id": 1,
        "method": "Runtime.evaluate",
        "params": {
            "expression": expression,
            "awaitPromise": true,
            "returnByValue": true,
        },
    });
    stream
        .send(tungstenite::Message::Text(request.to_string().into()))
        .map_err(unreachable)?;

    // The protocol interleaves events with answers, so this reads until the
    // answer to *this* request arrives rather than taking the first message.
    let deadline = Instant::now() + patience;
    while Instant::now() < deadline {
        let message = stream.read().map_err(unreachable)?;
        let tungstenite::Message::Text(text) = message else {
            continue;
        };
        let Ok(answer) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        if answer.get("id").and_then(serde_json::Value::as_i64) != Some(1) {
            continue;
        }
        let _ = stream.close(None);
        // An expression that threw did not reach the client at all — it fell
        // over on the way to it, which for these expressions means the object
        // it was reaching through is not there yet. See [`Problem::NotReady`].
        if let Some(thrown) = answer.pointer("/result/exceptionDetails/exception/description") {
            return Err(Problem::NotReady(thrown.to_string()));
        }
        if let Some(thrown) = answer.pointer("/result/exceptionDetails/text") {
            return Err(Problem::NotReady(thrown.to_string()));
        }
        return answer
            .pointer("/result/result/value")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| Problem::Refused(text.to_string()));
    }
    Err(Problem::Unreachable("it did not answer".to_string()))
}

/// One string, quoted the way JavaScript wants it.
///
/// Through the JSON encoder rather than by putting quotes round it: this is a
/// credential being pasted into a program that is about to be run, and the one
/// way that goes wrong is a value that ends the string it is in.
fn json_string(value: &str) -> String {
    serde_json::Value::String(value.to_string()).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Steam keeps a library it makes on a drive in a `SteamLibrary` folder at
    /// the top of it, because it refuses the top of the drive itself.
    #[test]
    fn a_drive_is_given_the_folder_steams_own_list_proposes() {
        assert_eq!(
            library_on_a_drive(std::path::Path::new("/run/media/kate/GamesHDD")),
            "/run/media/kate/GamesHDD/SteamLibrary"
        );
    }

    /// Each word Valve's storage page refuses a library with is sorted into a
    /// reason the shell can say, and a word it does not know is kept for the
    /// log rather than guessed at.
    #[test]
    fn a_library_refusal_is_sorted_by_steams_own_word() {
        let refused = |message: &str| shelved(&format!(r#"{{"result":2,"message":"{message}"}}"#));
        assert_eq!(shelved(r#"{"result":1}"#), Ok(()));
        assert_eq!(
            refused("NoDriveRoot"),
            Err(Problem::Declined(Declined::DriveRoot))
        );
        assert_eq!(
            refused("NotEmptyFolder"),
            Err(Problem::Declined(Declined::NotEmpty))
        );
        assert_eq!(
            refused("NotWritableFolder"),
            Err(Problem::Declined(Declined::NotWritable))
        );
        assert_eq!(
            refused("NotExecutableFolder"),
            Err(Problem::Declined(Declined::NotExecutable))
        );
        assert_eq!(
            refused("DriveAlreadyHasLibrary"),
            Err(Problem::Declined(Declined::AlreadyALibrary))
        );
        assert_eq!(
            refused("NotListed"),
            Err(Problem::Declined(Declined::NotListed))
        );
        assert_eq!(
            refused("FailedToAdd"),
            Err(Problem::Declined(Declined::Other(
                "FailedToAdd".to_string()
            )))
        );
        // A library in use names the game using it — and Steam itself, which
        // numbers itself seven and under, is nobody's game.
        assert_eq!(
            shelved(r#"{"result":2,"message":"InUse","app":440}"#),
            Err(Problem::Declined(Declined::InUse(Some(440))))
        );
        assert_eq!(
            shelved(r#"{"result":2,"message":"InUse","app":7}"#),
            Err(Problem::Declined(Declined::InUse(None)))
        );
    }

    /// A move is refused with Valve's update error number, as its own dialog
    /// reads it, or with the shell's own word for a library that already holds
    /// the game.
    #[test]
    fn a_move_refusal_is_sorted_by_its_update_error() {
        let refused = |code: i64| {
            move_began(&format!(
                r#"{{"result":2,"message":"refused","code":{code}}}"#
            ))
        };
        assert_eq!(move_began(r#"{"result":1}"#), Ok(()));
        assert_eq!(refused(15), Err(Problem::Declined(Declined::FolderThere)));
        assert_eq!(refused(17), Err(Problem::Declined(Declined::Shared)));
        assert_eq!(refused(22), Err(Problem::Declined(Declined::Unmovable)));
        assert_eq!(refused(12), Err(Problem::Declined(Declined::NoRoom)));
        assert_eq!(refused(16), Err(Problem::Declined(Declined::Running)));
        assert!(matches!(
            refused(5),
            Err(Problem::Declined(Declined::Other(_)))
        ));
        assert_eq!(
            move_began(r#"{"result":2,"message":"FolderThere"}"#),
            Err(Problem::Declined(Declined::FolderThere))
        );
        assert_eq!(
            move_began(r#"{"result":2,"message":"NotListed"}"#),
            Err(Problem::Declined(Declined::NotListed))
        );
    }

    /// A move is going while the client says 20, done when it says 0 of the
    /// game, and refused on anything else — or stopped, where Stop was pressed,
    /// whatever it ends with. A word about another game is not this move's.
    #[test]
    fn a_move_is_read_off_the_last_word_about_its_game() {
        let heard = |answer: &str| move_heard(answer).expect("a look");
        let look = |app: u32, code: i64, percent: f32, cancelled: bool| {
            heard(&format!(
                r#"{{"cancelled":{cancelled},"app":{app},"code":{code},"percent":{percent}}}"#
            ))
        };

        let nothing = heard("null");
        assert_eq!(nothing.ended(440), None);
        assert_eq!(nothing.moving(440), None);

        let before = look(0, -1, 0.0, false);
        assert_eq!(before.ended(440), None, "no word yet");

        let going = look(440, 20, 42.5, false);
        assert_eq!(going.ended(440), None);
        assert_eq!(going.moving(440), Some(42.5));
        assert_eq!(going.moving(730), None, "not that game's move");

        assert_eq!(look(440, 0, 100.0, false).ended(440), Some(Ok(Moved::Done)));
        assert_eq!(
            look(440, 3, 42.0, true).ended(440),
            Some(Ok(Moved::Cancelled))
        );
        assert_eq!(
            look(440, 0, 100.0, true).ended(440),
            Some(Ok(Moved::Done)),
            "a move that finished before Stop arrived is done"
        );
        assert_eq!(
            look(440, 17, 3.0, false).ended(440),
            Some(Err(Problem::Declined(Declined::Shared)))
        );
        assert_eq!(look(730, 0, 100.0, false).ended(440), None);
        assert_eq!(
            look(440, 20, 250.0, false).moving(440),
            Some(100.0),
            "clamped"
        );
    }

    /// A client whose private interface has moved is refused before anything
    /// is driven, and says so in a sentence a panel can hold.
    ///
    /// The check existed from the start and production never called it: only
    /// the diagnostic example asked. So a Steam update that renamed one of
    /// these turned a press into nothing at all — a wizard opened with nobody
    /// listening, its whole patience spent, and Valve's own unhelpful wording
    /// at the end of it.
    #[test]
    fn a_moved_private_interface_is_a_sentence_rather_than_a_silence() {
        let gone = Problem::Renamed(vec!["Installs.ContinueInstall".to_string()]);
        let said = gone.to_string();
        assert!(said.starts_with("This version of Steam"), "{said}");
        assert!(said.len() < 60, "too long for one line of a panel: {said}");
        // The names of what went belong in the log, not on somebody's screen.
        assert!(!said.contains("Installs."), "{said}");

        // And every method the two flows need is one this crate declares, so a
        // call added to a flow without being declared cannot go unchecked.
        for needed in INSTALLING_NEEDS
            .iter()
            .chain(REMOVING_NEEDS.iter())
            .chain(COMPATIBILITY_NEEDS.iter())
            .chain(FORCING_NEEDS.iter())
            .chain(OTHER_TITLES_NEEDS.iter())
            .chain(FORCING_OTHER_TITLES_NEEDS.iter())
        {
            assert!(
                EVERYTHING_THIS_CRATE_CALLS.contains(needed),
                "{needed} is driven but not declared"
            );
        }
    }

    /// What the client answers about compatibility is read as Valve's own two
    /// fields, and a tool with no name is not a row.
    ///
    /// The shape below is what the client's own properties window is built
    /// from: `strToolName` is the key a choice is sent back as and
    /// `strDisplayName` is the label, and only a priority of [`FORCED`] means
    /// somebody chose it — which is why the expression asks for the number
    /// rather than for the name being set.
    #[test]
    fn what_steam_offers_is_a_list_of_keys_and_labels() {
        let said = what_steam_offers(
            r#"{"tools":[
                 {"name":"proton_experimental","display":"Proton Experimental"},
                 {"name":"Proton-GE Latest","display":""},
                 {"name":"","display":"nothing to send back"}],
               "forced":"Proton-GE Latest"}"#,
        )
        .expect("the client's own shape");
        assert_eq!(said.tools.len(), 2, "a tool with no key is not a row");
        assert_eq!(said.tools[0].name, "proton_experimental");
        assert_eq!(said.tools[0].display, "Proton Experimental");
        // A client that stops sending the label leaves a row that can still be
        // read rather than a blank one.
        assert_eq!(said.tools[1].display, "Proton-GE Latest");
        assert_eq!(said.forced_display(), Some("Proton-GE Latest"));

        // Nothing forced is nothing forced, and not an empty name.
        let free = what_steam_offers(r#"{"tools":[],"forced":""}"#).expect("an empty answer");
        assert_eq!(free.forced, None);
        assert_eq!(free.forced_display(), None);

        // And a tool that has since been deleted out of `compatibilitytools.d`
        // is still what the game is set to run under: it says its name rather
        // than nothing at all.
        let gone = what_steam_offers(r#"{"tools":[],"forced":"DW-Proton Latest"}"#)
            .expect("a tool that is no longer offered");
        assert_eq!(gone.forced_display(), Some("DW-Proton Latest"));
    }

    /// A flow that stopped to ask the person something is not a flow that
    /// failed, and the two must not arrive as one: a failure is a panel that
    /// says something went wrong, where this is a panel that offers to open
    /// Steam so the question can be answered.
    #[test]
    fn a_question_is_not_a_failure() {
        assert_eq!(
            wizard_said(r#"{"asks":"an agreement to accept"}"#),
            Err(Problem::Asks("an agreement to accept".to_string()))
        );
        // And it outranks anything carried alongside it, because the flow was
        // cancelled in order to ask and would otherwise read as a flow that
        // somebody stopped.
        assert_eq!(
            wizard_said(r#"{"failed":"Steam stopped it","asks":"a product key to type"}"#),
            Err(Problem::Asks("a product key to type".to_string()))
        );
    }

    /// An agreement comes back as what it is, so the shell can put it on its
    /// own screen — and only when all of it is there. An edition written as
    /// text is still an edition; one that is missing, or an agreement with no
    /// name, is Steam's own window, because an acceptance recorded under the
    /// wrong name or edition is one nobody gave.
    #[test]
    fn an_agreement_comes_back_as_the_agreements() {
        let garrys_mod = Eula {
            app_id: 4000,
            id: "4000_eula_0".to_string(),
            version: 3,
            url: "http://store.steampowered.com/eula/4000_eula_0".to_string(),
        };
        assert_eq!(
            wizard_said(
                r#"{"agreements":[{"app":4000,"id":"4000_eula_0","version":3,
                    "url":"http://store.steampowered.com/eula/4000_eula_0"}]}"#
            ),
            Err(Problem::Agreements(vec![garrys_mod.clone()]))
        );
        assert_eq!(
            wizard_said(
                r#"{"agreements":[{"app":4000,"id":"4000_eula_0","version":"3",
                    "url":"http://store.steampowered.com/eula/4000_eula_0"}]}"#
            ),
            Err(Problem::Agreements(vec![garrys_mod]))
        );
        for broken in [
            r#"{"agreements":[]}"#,
            r#"{"agreements":[{"app":4000,"id":"4000_eula_0","url":"https://x"}]}"#,
            r#"{"agreements":[{"app":4000,"id":"","version":1,"url":"https://x"}]}"#,
            r#"{"agreements":[{"app":4000,"id":"a","version":1,"url":"https://x"},{"app":4000}]}"#,
        ] {
            assert_eq!(
                wizard_said(broken),
                Err(Problem::Asks("an agreement to accept".to_string())),
                "{broken}"
            );
        }
    }

    /// A wizard that stopped to ask where comes back with the libraries, in
    /// Steam's order, with sizes read whether the client wrote them as numbers
    /// or as strings — and a list with a library nobody could name is an
    /// answer that cannot be read, not a panel with a blank button on it.
    #[test]
    fn a_choice_of_libraries_comes_back_as_the_libraries() {
        assert_eq!(
            wizard_said(
                r#"{"where":{"needs":12884901888,"libraries":[
                    {"path":"/home/x/.local/share/Steam","label":"","free":85899345920,"default":true},
                    {"path":"/mnt/GamesSSD/SteamLibrary","label":" Games ","free":"442381631488","default":false}
                ]}}"#
            ),
            Err(Problem::WhereTo(Choice {
                needs: 12_884_901_888,
                libraries: vec![
                    Library {
                        path: "/home/x/.local/share/Steam".to_string(),
                        label: String::new(),
                        free: 85_899_345_920,
                        default: true,
                    },
                    Library {
                        path: "/mnt/GamesSSD/SteamLibrary".to_string(),
                        label: "Games".to_string(),
                        free: 442_381_631_488,
                        default: false,
                    },
                ],
            }))
        );
        // A size the wizard did not give is not known, rather than nothing.
        let Err(Problem::WhereTo(choice)) =
            wizard_said(r#"{"where":{"libraries":[{"path":"/a","free":10}]}}"#)
        else {
            panic!("a list of one library is still a list");
        };
        assert_eq!(choice.needs, 0);
        assert!(choice.libraries[0].fits(0));

        for unreadable in [
            r#"{"where":{"needs":1,"libraries":[]}}"#,
            r#"{"where":{"needs":1,"libraries":[{"path":"","free":1}]}}"#,
            r#"{"where":{"needs":1}}"#,
        ] {
            assert!(
                matches!(wizard_said(unreadable), Err(Problem::Refused(_))),
                "{unreadable}"
            );
        }
    }

    /// A game fits where there is more room than it needs, by Valve's own test:
    /// exactly as much room as it needs is not enough.
    #[test]
    fn a_library_fits_a_game_only_with_room_to_spare() {
        let library = Library {
            path: "/a".to_string(),
            label: String::new(),
            free: 100,
            default: false,
        };
        assert!(library.fits(99));
        assert!(!library.fits(100));
        assert!(!library.fits(101));
    }

    /// The place is handed to the expression as data, whatever is in the path,
    /// and asking hands it nothing at all.
    #[test]
    fn a_place_is_handed_over_as_data() {
        let odd = r#"/mnt/it's "odd"/SteamLibrary"#;
        let named = placing(&Place::In {
            path: odd.to_string(),
            by_default: true,
        });
        assert!(
            named.contains(&serde_json::json!({ "path": odd, "default": true }).to_string()),
            "{named}"
        );
        assert!(placing(&Place::Ask).contains("const wanted = null;"));
    }

    /// What an install records as accepted is exactly what was read, under the
    /// names the client's own call takes them by.
    #[test]
    fn accepting_is_handed_over_as_it_was_read() {
        let literal = accepting_literal(&[Eula {
            app_id: 4000,
            id: "4000_eula_0".to_string(),
            version: 3,
            url: "https://store.steampowered.com/eula/4000_eula_0".to_string(),
        }]);
        let read: serde_json::Value = serde_json::from_str(&literal).expect("an array literal");
        assert_eq!(
            read,
            serde_json::json!([{ "app": 4000, "id": "4000_eula_0", "version": 3 }])
        );
        assert_eq!(accepting_literal(&[]), "[]");
    }

    /// A download that would not start is not a credential that was not
    /// accepted, and the sentence the user reads must not say it was. This is
    /// the whole of why the wizard has a reader of its own: with both answers
    /// going through [`took_it`], a failed install reached the screen as "Steam
    /// would not take the credential: Steam could not start this download".
    #[test]
    fn a_download_that_would_not_start_is_not_a_refused_credential() {
        assert_eq!(wizard_said(r#"{"ok":true}"#), Ok(()));
        assert_eq!(
            wizard_said(r#"{"failed":"Steam could not start this download","error":29}"#),
            Err(Problem::Stopped(
                "Steam could not start this download (error 29)".to_string()
            ))
        );
        assert_eq!(
            Problem::Stopped("Steam could not start this download".to_string()).to_string(),
            "Steam could not start this download"
        );
        // The number is Valve's `EAppUpdateError` and nothing is said about it
        // when there is none to say.
        assert_eq!(
            wizard_said(r#"{"failed":"it was stopped","error":0}"#),
            Err(Problem::Stopped("it was stopped".to_string()))
        );
        // And an answer that is none of these is not read as success.
        assert!(matches!(wizard_said("not json"), Err(Problem::Refused(_))));
        assert!(matches!(wizard_said("{}"), Err(Problem::Refused(_))));
        assert!(matches!(
            wizard_said(r#"{"result":1}"#),
            Err(Problem::Refused(_))
        ));
    }

    /// The client answers every call with an `EResult`, and only one of them
    /// means it took the token. Anything else has to reach the user with
    /// Valve's own wording, because this shell cannot improve on a reason it
    /// does not understand.
    #[test]
    fn only_an_ok_result_is_a_sign_in() {
        assert_eq!(took_it(r#"{"result":1}"#), Ok(()));
        assert_eq!(
            took_it(r#"{"result":5,"message":"Invalid password"}"#),
            Err(Problem::Refused("Invalid password (EResult 5)".to_string()))
        );
        // A refusal with nothing said about it is still a refusal.
        assert!(matches!(
            took_it(r#"{"result":84}"#),
            Err(Problem::Refused(_))
        ));
        // And an answer that is not one of these is not read as success.
        assert!(matches!(took_it("not json"), Err(Problem::Refused(_))));
        assert!(matches!(
            took_it(r#"{"ok":true}"#),
            Err(Problem::Refused(_))
        ));
    }

    /// The wait before the wizard is opened is a real deadline and a real
    /// question, and it is written into a program — so both halves are checked
    /// here rather than only on a machine with Steam on it.
    #[test]
    fn it_waits_for_the_client_to_know_the_game() {
        let waiting = knows_the_game(945360);
        assert!(waiting.contains("GetAppOverviewByAppID"));
        assert!(waiting.contains("945360"));
        assert!(waiting.contains(&UNTIL_IT_KNOWS_THE_GAME.as_millis().to_string()));
        // A question that cannot be asked counts as answered, or a renamed
        // store would turn every press into a twenty-second pause.
        assert!(waiting.contains("if (typeof look !== 'function') return true;"));
    }

    /// The same wait before the tools are asked for, and the same escape.
    ///
    /// This one is worth a test of its own because the failure it prevents is
    /// silent and is then *remembered*: a client asked too early answers with
    /// the three tools on this machine's disk instead of the nineteen it has,
    /// and the shell keeps that answer for the session. See
    /// [`knows_its_tools`], where the measurement is.
    #[test]
    fn it_waits_for_the_client_to_know_its_tools() {
        let waiting = knows_its_tools();
        assert!(waiting.contains("m_bIsInitialized"));
        assert!(waiting.contains(&UNTIL_IT_KNOWS_ITS_TOOLS.as_millis().to_string()));
        assert!(waiting.contains("if (!store || !('m_bIsInitialized' in store)) return true;"));
    }

    /// The token is pasted into a program before it is run, so the quoting is
    /// load-bearing rather than cosmetic.
    #[test]
    fn a_value_cannot_end_the_string_it_is_in() {
        assert_eq!(json_string("plain"), r#""plain""#);
        assert_eq!(json_string(r#"a" + evil() + ""#), r#""a\" + evil() + \"""#);
        assert_eq!(json_string("two\nlines"), r#""two\nlines""#);
    }

    /// Whether the client will expose its context is a file, and it is asked
    /// about before the client is started rather than after.
    #[test]
    fn the_marker_file_is_what_says_it_is_exposed() {
        let root = std::env::temp_dir().join(format!("lxb-webui-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        assert!(!available(&root));
        std::fs::write(root.join(MARKER), b"").unwrap();
        assert!(available(&root));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Making the marker is the shell's job, and saying whether it *had* to be
    /// made is the other half of it: the client reads the file as it starts, so
    /// only a fresh one means a running client has to be started again.
    #[test]
    fn exposing_says_whether_the_client_missed_it() {
        // Held because `expose` writes a note saying whose the marker is, and
        // where that note goes is `$XDG_STATE_HOME` — which the tidying tests
        // below point at scratch directories of their own. Without this, a note
        // written here lands in one of theirs and names a marker in another
        // directory entirely, so the tidying reads the wrong path and leaves
        // the marker it was asked about where it is. That is what
        // `the marker outlived the wake that left it` was, about one run of
        // this crate's tests in three.
        let _turn = crate::one_at_a_time_with_the_environment();
        let root = std::env::temp_dir().join(format!("lxb-expose-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        assert!(!available(&root));
        assert!(expose(&root).unwrap(), "it had to be made");
        assert!(available(&root));

        // And again, on a session that finds it already there. Nothing was
        // made, so nothing needs starting again.
        assert!(!expose(&root).unwrap(), "it was already there");
        assert!(available(&root));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Up for the moment the client starts, and down again afterwards. The
    /// file does not have to stay — the client reads it once — and leaving it
    /// would expose a Steam started long after this session had gone.
    #[test]
    fn the_marker_does_not_outlive_what_it_was_for() {
        let _turn = crate::one_at_a_time_with_the_environment();
        let root = std::env::temp_dir().join(format!("lxb-withdraw-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // The note goes to the state directory, and a test must not write to
        // the one belonging to whoever is running it.
        let state = root.join("state");
        unsafe { std::env::set_var("XDG_STATE_HOME", &state) };

        assert!(expose(&root).unwrap());
        assert!(available(&root));
        withdraw(&root);
        assert!(!available(&root), "the marker was left behind");

        // And taking away one that is not there is not an error: a session
        // that failed before it made one still runs this on the way out.
        withdraw(&root);
        assert!(!available(&root));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A marker, and a state directory of this test's own to keep the note and
    /// the lease out of the one belonging to whoever is running it.
    ///
    /// Answers the root, which the caller removes on its way out.
    fn a_marker_left_by(name: &str, owner: OwnedBy) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("lxb-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        unsafe { std::env::set_var("XDG_STATE_HOME", root.join("state")) };
        assert!(expose(&root).unwrap());
        assert!(available(&root));
        if let OwnedBy::ASessionThatDied = owner {
            // `expose` has just written a lease naming *this* process, which is
            // plainly alive — so a run that died has to be said rather than
            // acted out. Init's number, with this process's start time against
            // it: a pid somebody else is wearing, which is the whole of what
            // [`crate::turns::Lease::alive`] is there to tell apart.
            let mine = crate::turns::Lease::of_this_process(&crate::turns::Behalf::nothing())
                .expect("this process is in /proc");
            let dead = crate::turns::Lease { pid: 1, ..mine };
            assert!(!dead.alive());
            std::fs::write(whose_it_is().unwrap(), dead.written()).unwrap();
        }
        root
    }

    /// Whose the marker a test has just made is to be taken for.
    enum OwnedBy {
        /// A run that made one and then died, which is what the tidying is for.
        ASessionThatDied,
        /// This process, which is running — the case that used to be tidied
        /// away out from under a live sign-in.
        ASessionStillRunning,
    }

    /// A session killed between making the marker and taking it back leaves it
    /// on the disk, and nothing would ever have removed it: every later
    /// `expose` finds it there and correctly decides it is not its to touch.
    ///
    /// Found on a real machine, where a marker a fortnight old was making every
    /// Steam that user started expose its whole interface on the loopback
    /// interface — including the ones started for a game and nothing else.
    #[test]
    fn a_marker_left_behind_by_a_dead_session_is_taken_back_by_the_next() {
        let _turn = crate::one_at_a_time_with_the_environment();
        let root = a_marker_left_by("orphan", OwnedBy::ASessionThatDied);

        withdraw_what_was_left_behind();
        assert!(!available(&root), "the next session left it lying there");

        // And the note goes with it, so a second run has nothing to act on and
        // does not go looking for a file that was taken back long ago.
        withdraw_what_was_left_behind();
        assert!(!available(&root));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// And the other half, which is the defect this was written for: a marker
    /// whose session is **still using it** is not litter.
    ///
    /// Two of these can run at once — two sessions, or one of this crate's own
    /// `probe-*` examples against a live shell — and the tidying above ran at
    /// every startup on the reasoning that a marker with a note beside it must
    /// be from "the run before this one". It is not: another session makes the
    /// marker, hands its client a credential behind it and takes it back, and a
    /// shell starting up in the middle of that read a file in active use as
    /// something to clean away. The client then came up exposing nothing and
    /// the sign-in behind it failed with nothing anywhere saying why.
    ///
    /// Here the owner is this process, so the lease answers; the turn is free,
    /// which is what makes this the lease's own test rather than the lock's.
    #[test]
    fn a_marker_a_living_session_is_using_is_left_where_it_is() {
        let _turn = crate::one_at_a_time_with_the_environment();
        let root = a_marker_left_by("in-use", OwnedBy::ASessionStillRunning);

        withdraw_what_was_left_behind();
        assert!(
            available(&root),
            "a session took away a marker another was signing a client in behind"
        );

        // And it is still this session's to take back when it is done with it,
        // which is the ordinary end of every wake.
        withdraw(&root);
        assert!(!available(&root));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The lock's own half of the same question, asked where the lease would
    /// give the wrong answer.
    ///
    /// The marker here says its owner is dead, so nothing but the turn can stop
    /// the tidying — and something must, because a wake is what holds that turn
    /// and a wake is exactly when the marker is on the disk. Held from this
    /// thread on purpose: a second worker being built in one process before the
    /// first is dropped is the shell switching its Steam integration back on,
    /// and the process-local half is the only thing that knows about it.
    #[test]
    fn a_marker_is_left_alone_while_somebody_is_inside_a_wake() {
        let _turn = crate::one_at_a_time_with_the_environment();
        let root = a_marker_left_by("mid-wake", OwnedBy::ASessionThatDied);

        let wake = crate::turns::take(
            crate::turns::What::TheClient,
            crate::turns::Behalf::nothing(),
        );
        withdraw_what_was_left_behind();
        assert!(
            available(&root),
            "a marker was taken back while a wake was still running"
        );

        // And the moment the wake is over, the marker really was litter.
        drop(wake);
        withdraw_what_was_left_behind();
        assert!(
            !available(&root),
            "the marker outlived the wake that left it"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A second flow waits at the door, and waits there before it has done
    /// anything at all.
    ///
    /// Valve has exactly one install wizard and `CancelInstall()` is a call
    /// about *the* wizard rather than about a game, so two flows inside it at
    /// once means the first of them to be asked a question cancels the other's
    /// download. Every one of these runs on a thread of its own and begins by
    /// waking a client that can take a minute, so two rows pressed inside that
    /// minute arrive here together — this is not a race that is hard to hit.
    ///
    /// What is checked is not that a mutex excludes, which is the standard
    /// library's business. It is that the waiting happens **first**: before the
    /// capability check and before the socket, so that what a flow learns about
    /// the client is learned after it has the client to itself.
    #[test]
    fn a_second_flow_waits_at_the_door_before_it_asks_anything() {
        let held = the_wizard(None);

        let (running, is_running) = std::sync::mpsc::channel();
        let (through, is_through) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let _ = running.send(());
            // Takes the wizard, asks the client what it has, opens a socket.
            // Drives nothing, which is what makes this safe to run on a machine
            // with a live client on it.
            let outcome = ready_to_drive_the_wizard(&INSTALLING_NEEDS, Still::whatever_happens());
            let _ = through.send(());
            drop(outcome);
        });
        is_running
            .recv_timeout(Duration::from_secs(5))
            .expect("the second flow never started");

        assert!(
            is_through.recv_timeout(Duration::from_millis(250)).is_err(),
            "a flow walked past a wizard somebody else was holding"
        );

        // And it goes through the moment the wizard is free. Generously waited
        // for: on a machine with a client up, the capability check behind the
        // door is a real call with its own patience.
        drop(held);
        is_through
            .recv_timeout(PATIENCE + Duration::from_secs(5))
            .expect("the wizard was let go of and nobody took it");
        waiter.join().expect("the second flow panicked");
    }

    /// A connection is bound to the client it was proved against, and the
    /// binding refuses only on evidence.
    ///
    /// The half of finding 5 the ground check cannot answer. A generation says
    /// this session is still acting for the account it was asked for; it says
    /// nothing about *which client is on the other end of the socket*, and the
    /// interface is a port on loopback that whoever came up last is holding. A
    /// client killed and started again between the wake and the wizard signs
    /// itself into whatever it remembered, and every check made before that
    /// gap passes.
    ///
    /// Asked against a real socket this test is holding open, because what is
    /// under test is the reading of `/proc` and not a table written to suit it.
    #[test]
    fn the_call_goes_out_on_the_client_that_was_proved_and_no_other() {
        let held = std::net::TcpListener::bind("127.0.0.1:0").expect("a port of our own");
        let port = held.local_addr().unwrap().port();

        // This test process is holding it, and this test process is what was
        // proved: the call may go out.
        assert!(held_by(port, Some(std::process::id())).is_ok());

        // Somebody else is holding it. `1` is init, which is nothing this
        // shell ever proved and is certainly not a family of ours.
        assert!(
            matches!(held_by(port, Some(1)), Err(Problem::Moved(_))),
            "a call would have gone out on a client this session never proved"
        );

        // Not knowing is not evidence, and neither of the two ways of not
        // knowing refuses: a client whose process could not be found —
        // [`crate::client::Proven`]'s own `None` — and a port whose owner
        // could not be read. Both leave the ground check standing behind the
        // call, which is what stood there before any of this existed.
        assert!(held_by(port, None).is_ok());
        drop(held);
        assert!(held_by(port, Some(1)).is_ok());
    }

    /// And a caller with nothing to bind to asks the machine nothing at all.
    ///
    /// [`Still::whatever_happens`] is the read-only calls and the tests. The
    /// socket below is a lie, and the assertion is that it is never dialled:
    /// anything that reached for it would answer `Unreachable` rather than
    /// `Ok`.
    #[test]
    fn a_caller_with_nothing_proved_binds_to_nothing() {
        assert!(Still::whatever_happens()
            .reaches_the_client_it_proved("ws://127.0.0.1:1/nothing")
            .is_ok());
    }

    /// A flow whose ground moves **while it waits for the wizard** is refused
    /// at the door.
    ///
    /// The check nowhere else could make. A caller looks at its own account
    /// before it queues here; by the time it is let in, that look is as old as
    /// somebody else's whole install — up to [`UNTIL_THE_WIZARD_ANSWERS`] — and
    /// acting on it is stale authority, not merely a stale answer. So the
    /// question is asked again on this side of the wait, and before a
    /// capability is checked or a socket opened: the assertion is that the
    /// refusal is [`Problem::Moved`] and not `NotExposed`, which is what a
    /// machine with no client running would otherwise say.
    #[test]
    fn a_flow_whose_ground_moved_while_it_queued_is_refused_at_the_door() {
        use std::sync::atomic::{AtomicBool, Ordering};

        /// A caller whose account signs out while it is queueing.
        struct Moving<'a>(&'a AtomicBool);

        impl Ground for Moving<'_> {
            fn about_to_act(&self) -> Result<(), String> {
                match self.0.load(Ordering::SeqCst) {
                    true => Err("the account this was asked for has signed out".to_string()),
                    false => Ok(()),
                }
            }
        }

        let held = the_wizard(None);
        let moved = AtomicBool::new(false);
        let (running, is_running) = std::sync::mpsc::channel();
        let (answered, is_answered) = std::sync::mpsc::channel();

        std::thread::scope(|scope| {
            scope.spawn(|| {
                let _ = running.send(());
                let ground = Moving(&moved);
                let outcome = ready_to_drive_the_wizard(
                    &INSTALLING_NEEDS,
                    Still {
                        ground: &ground,
                        // Nothing proved: what is under test is the ground
                        // half, and a machine with a live client on it would
                        // otherwise have its interface asked whose it is.
                        proved: None,
                        request: None,
                    },
                );
                let _ = answered.send(matches!(outcome, Err(Problem::Moved(_))));
            });
            is_running
                .recv_timeout(Duration::from_secs(5))
                .expect("the flow never started");
            // It is outside the door, and the account goes while it is there.
            moved.store(true, Ordering::SeqCst);
            drop(held);
            assert!(
                is_answered
                    .recv_timeout(PATIENCE + Duration::from_secs(5))
                    .expect("the flow never answered"),
                "a flow drove Valve's client for an account that had gone while it queued"
            );
        });
    }

    /// Whose the marker is, told apart on the disk, and what happens when
    /// somebody presses the button.
    ///
    /// The port is not asserted on: whether anything is listening on this
    /// machine is this machine's business and not the test's, which is the
    /// whole reason [`Exposure`] carries the two facts separately.
    #[test]
    fn a_marker_this_shell_made_is_told_from_one_it_did_not() {
        let _turn = crate::one_at_a_time_with_the_environment();
        let root = std::env::temp_dir().join(format!("lxb-exposure-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // SAFETY: the environment guard above is what makes this test the only
        // one touching this for its duration.
        unsafe { std::env::set_var("XDG_STATE_HOME", root.join("state")) };

        assert_eq!(exposure(&root).marker, None, "nothing has been made yet");

        // One this shell made: there is a note beside it, and the panel says so
        // rather than offering a button that would break the sign-in it is for.
        assert!(expose(&root).unwrap());
        assert_eq!(exposure(&root).marker, Some(Marker::Ours));
        assert!(!exposure(&root).can_be_shut());

        // And one it did not. Same file, no note.
        withdraw(&root);
        std::fs::write(root.join(MARKER), b"").unwrap();
        assert_eq!(exposure(&root).marker, Some(Marker::Somebody));
        assert!(exposure(&root).can_be_shut());

        // Which is the one a person can ask to have taken away.
        shut_the_interface(&root).unwrap();
        assert!(!available(&root));
        assert_eq!(exposure(&root).marker, None);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Every way it can stand says something different, and only one of them
    /// offers a button.
    ///
    /// The pair that matters is the last two. An open port with a marker behind
    /// it and one without are the same to anything reading the port, and they
    /// are not the same thing to do about: deleting a file fixes the first and
    /// does nothing whatever to the second, which goes when the client does.
    #[test]
    fn every_way_the_interface_can_stand_says_which_it_is() {
        let standing = |open, marker| Exposure { open, marker };
        let all = [
            standing(false, None),
            standing(true, None),
            standing(false, Some(Marker::Somebody)),
            standing(true, Some(Marker::Somebody)),
            standing(true, Some(Marker::Ours)),
        ];
        let said: std::collections::BTreeSet<&str> = all.iter().map(Exposure::said).collect();
        assert_eq!(said.len(), all.len(), "two of them say the same thing");

        // A shut port and no marker is the only one that is nothing to report.
        assert_eq!(standing(false, None).said(), "closed");
        assert!(!standing(false, None).can_be_shut());
        // And the only one with a way out is somebody else's marker, whether or
        // not a client has already read it.
        assert!(standing(false, Some(Marker::Somebody)).can_be_shut());
        assert!(standing(true, Some(Marker::Somebody)).can_be_shut());
        assert!(!standing(true, None).can_be_shut());
        assert!(!standing(true, Some(Marker::Ours)).can_be_shut());
    }

    /// One somebody made themselves is theirs. Valve documents the file, a
    /// developer may well want one, and a shell that deleted it would be
    /// turning off something they had turned on.
    #[test]
    fn a_marker_this_shell_did_not_make_is_left_alone() {
        let _turn = crate::one_at_a_time_with_the_environment();
        let root = std::env::temp_dir().join(format!("lxb-theirs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let state = root.join("state");
        unsafe { std::env::set_var("XDG_STATE_HOME", &state) };

        std::fs::write(root.join(MARKER), b"").unwrap();
        assert!(available(&root));
        // And this shell notices it is already there and makes nothing.
        assert!(!expose(&root).unwrap(), "it should not have made a second");

        withdraw_what_was_left_behind();
        assert!(available(&root), "somebody else's marker was deleted");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A Steam directory that cannot be written to is reported rather than
    /// swallowed — the sign-in that follows would fail anyway, and it would
    /// fail saying nothing useful about why.
    #[test]
    fn a_marker_that_cannot_be_made_is_an_error() {
        let root = std::env::temp_dir().join(format!("lxb-expose-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        // Deliberately not created: no such directory to write into.
        assert!(expose(&root).is_err());
    }

    /// A launch that has stopped is one boolean, and every shape here was
    /// captured off a real client on 2026-09-02.
    ///
    /// The stopped one is the reported bug: a game whose save the cloud had not
    /// caught up with sat on `SynchronizingCloud` / `pendingcloudsessions` with
    /// `bWaitingForUI` set, and stayed there. The one beside it is the same
    /// client's ordinary launch of another game, which never set it and was
    /// gone from the list in six seconds.
    #[test]
    fn a_launch_that_has_stopped_says_so_in_the_clients_own_words() {
        assert_eq!(what_the_client_is_launching("[]").unwrap(), Vec::new());

        let walking = what_the_client_is_launching(
            r#"[{"action_id":2,"gameid":"1304550","task":"SynchronizingControllerConfig",
                 "details":"","waiting":false}]"#,
        )
        .unwrap();
        assert_eq!(walking.len(), 1);
        assert_eq!(walking[0].app_id, 1304550);
        assert!(!walking[0].waiting_for_a_person, "it is getting on with it");

        let stopped = what_the_client_is_launching(
            r#"[{"action_id":1,"gameid":"250900","task":"SynchronizingCloud",
                 "details":"pendingcloudsessions","waiting":true}]"#,
        )
        .unwrap();
        assert_eq!(stopped[0].action_id, 1, "what an answer is sent against");
        assert_eq!(stopped[0].app_id, 250900);
        assert_eq!(stopped[0].task, "SynchronizingCloud");
        assert_eq!(stopped[0].details, "pendingcloudsessions");
        assert!(stopped[0].waiting_for_a_person);
        assert_eq!(
            stopped[0].how_far(),
            None,
            "a step that counts nothing has no reading, and nought is not one"
        );

        // The one step that counts itself: the shader cache being compiled,
        // which is where Valve's own dialog says "Processing Vulkan shaders
        // (34%)" and offers to be skipped. The client writes the two numbers as
        // strings — `strNumDone` and `strNumTotal` — and the call reads either
        // spelling.
        let compiling = what_the_client_is_launching(
            r#"[{"action_id":1,"gameid":"730","task":"ProcessingShaderCache",
                 "details":"","done":174,"total":512,"waiting":true}]"#,
        )
        .unwrap();
        assert_eq!(compiling[0].how_far(), Some(33));
        assert!(compiling[0].waiting_for_a_person, "and it stops there");

        // A count that has run past its total is a step that has finished
        // without saying so — Valve's own arithmetic refuses it, and a reading
        // of three hundred per cent is worse than none.
        let overrun = what_the_client_is_launching(
            r#"[{"action_id":1,"gameid":"730","task":"ProcessingShaderCache",
                 "details":"","done":600,"total":512,"waiting":true}]"#,
        )
        .unwrap();
        assert_eq!(overrun[0].how_far(), None);

        // A non-Steam shortcut's game id is a whole `CGameID` whose low half is
        // not an app id. Nothing here is about one, and taking the low half
        // would name some other game entirely.
        let shortcut = what_the_client_is_launching(
            r#"[{"action_id":3,"gameid":"18446744073709551615","task":"","details":"",
                     "waiting":true}]"#,
        )
        .unwrap();
        assert_eq!(
            shortcut.first().map(|one| one.app_id),
            Some(u32::MAX),
            "the low half is what the client stamps its own windows with"
        );

        // And a client that answered with something else is a problem rather
        // than an empty list, because an empty list means "nothing starting".
        assert!(what_the_client_is_launching("not json").is_err());
    }

    /// Which questions this shell puts on its own screen, and which it does
    /// not — and the second half is the deliberate half.
    #[test]
    fn only_the_questions_with_a_panel_are_this_shells_to_ask() {
        // The reported one, and the detail is what picks it: the same request
        // carries three different questions.
        let pending = asked_about("SynchronizingCloud", "pendingcloudsessions").expect("a panel");
        assert_eq!(
            pending.answers,
            &[(
                "#CloudPendingOps_Continue",
                Some("IgnorePendingCloudSessions")
            )]
        );
        assert!(asked_about("SynchronizingCloud", "syncfailed").is_some());
        // A choice between two saves, which Valve shows with the date of each.
        // Asking somebody to pick one blind is worse than asking them to reach
        // for a mouse once.
        assert!(asked_about("SynchronizingCloud", "cloudconflict").is_none());

        // A row with no detail matches whatever detail comes with it, because
        // there the detail is the *content* of the question rather than which
        // question it is — the arguments a launch is asking about.
        assert!(asked_about("ShowGameArgs", "-nosound").is_some());

        // An agreement to read, and a key to copy down. Neither is a thing to
        // put an OK on.
        assert!(asked_about("ShowEula", "").is_none());
        assert!(asked_about("ShowCDKey", "").is_none());
        assert!(asked_about("ProcessingInstallScript", "").is_none());
    }

    /// Which way to start the game, which is the one question built from the
    /// game rather than from a table.
    ///
    /// Every shape here is the one OpenFront produced on this machine, whose
    /// `console_log.txt` carries both halves of the exchange: the client
    /// stopped on `ShowLaunchOption` and went on again when it was answered
    /// `"1"`. The reported failure was that this shell had no panel for it, so
    /// a press sat on a loading screen until somebody reached Valve's own
    /// window with a mouse.
    #[test]
    fn the_way_a_game_starts_is_asked_with_the_clients_own_list() {
        let stopped = Launching {
            action_id: 2,
            app_id: 3560670,
            task: CHOOSING_HOW_TO_START.to_string(),
            details: String::new(),
            done: 0,
            total: 0,
            waiting_for_a_person: true,
        };

        // Captured off the running client on 2026-09-19, by evaluating the
        // expression this module ships — see [`ask_for_the_ways`]. Every
        // string below is Valve's.
        let asked = the_ways_it_can_start(
            &stopped,
            r#"{"name":"OpenFront","asked":"Make a selection to launch %1$s",
                "refuse":"Cancel",
                "ways":[{"index":0,"said":"Play %1$s","game":"OpenFront"},
                        {"index":1,"said":"Play %1$s",
                         "game":"OpenFront (Wayland workaround)"}]}"#,
        )
        .unwrap()
        .expect("a panel");

        assert_eq!(asked.action_id, 2, "what an answer is sent against");
        assert_eq!(asked.app_id, 3560670);
        assert_eq!(asked.body, vec!["Make a selection to launch OpenFront"]);
        // **The whole of what makes the rows tell each other apart.** Both
        // options carry the same description — `#Steam_LaunchOption_Game`,
        // which localises to "Play %1$s" — and what differs is `strGameName`,
        // which is the argument Valve's own dialog hands its own localiser.
        // Filling the placeholder from the *app's* name instead would put two
        // rows reading "Play OpenFront" in front of somebody and make the
        // panel useless.
        assert_eq!(
            asked
                .answers
                .iter()
                .map(|answer| answer.label.as_str())
                .collect::<Vec<_>>(),
            vec![
                "Play OpenFront",
                "Play OpenFront (Wayland workaround)",
                "Cancel"
            ]
        );
        // **What a choice is remembered by**, which is the option's own name
        // and not the label: both labels here come from one description
        // token, so the half that differs is the only half worth keeping. See
        // [`Answer::chooses`].
        assert_eq!(
            asked
                .answers
                .iter()
                .map(|answer| answer.chooses.clone())
                .collect::<Vec<_>>(),
            vec![
                Some("OpenFront".to_string()),
                Some("OpenFront (Wayland workaround)".to_string()),
                // The way out is not a way to start.
                None,
            ]
        );

        // And the answer is the index, written out. `"1"` is the word the
        // client wrote down when somebody answered this by hand.
        assert_eq!(
            asked
                .answers
                .iter()
                .map(|answer| answer.carry.clone())
                .collect::<Vec<_>>(),
            vec![
                Carry::Go("0".to_string()),
                Carry::Go("1".to_string()),
                Carry::Stop
            ],
            "the refusal is last and always"
        );

        // What is sent is the number that came with the option, whatever row
        // it landed on. The client seen on this machine numbers its own list
        // from nought, so this shape is the one that has not been observed —
        // and it is here because the answer starts somebody's game, and a
        // panel that counted rows would be wrong silently.
        let sparse = the_ways_it_can_start(
            &stopped,
            r#"{"name":"A Game","asked":"Make a selection to launch %1$s","refuse":"Cancel",
                "ways":[{"index":2,"said":"DirectX 11","game":"A Game"},
                        {"index":5,"said":"DirectX 12","game":"A Game"}]}"#,
        )
        .unwrap()
        .expect("a panel");
        assert_eq!(
            sparse.answers[0].carry,
            Carry::Go("2".to_string()),
            "the first row is the client's option two"
        );
        assert_eq!(sparse.answers[1].carry, Carry::Go("5".to_string()));

        // An option the game gave no words to is still a row somebody can aim
        // at, headed with a name Valve would have shown anyway.
        let nameless = the_ways_it_can_start(
            &stopped,
            r#"{"name":"OpenFront","asked":"Make a selection to launch %1$s","refuse":"Cancel",
                "ways":[{"index":0,"said":"","game":"OpenFront"},
                        {"index":1,"said":"  ","game":""}]}"#,
        )
        .unwrap()
        .expect("a panel");
        assert_eq!(nameless.answers[0].label, "OpenFront");
        assert_eq!(
            nameless.answers[1].label, "OpenFront",
            "the game's own name"
        );
        // An option with no name of its own is remembered by the game's, which
        // is something rather than an empty string — and an empty string would
        // match every other nameless option there ever is.
        assert_eq!(
            nameless.answers[1].chooses.as_deref(),
            Some("OpenFront"),
            "a choice is remembered by something"
        );

        // A client that asked how to start a game and then offered no way to
        // start it. There is nothing to put on a panel, and a panel whose only
        // row ends the launch is worse than Valve's window.
        assert!(the_ways_it_can_start(
            &stopped,
            r#"{"name":"OpenFront","asked":"Make a selection to launch %1$s","refuse":"Cancel",
                "ways":[]}"#,
        )
        .unwrap()
        .is_none());

        // And a client that no longer has the words for its own question. The
        // same refusal the table's questions make, for the same reason.
        for lost in [
            r#"{"name":"OpenFront","asked":"","refuse":"Cancel",
                "ways":[{"index":0,"said":"Play","game":"OpenFront"}]}"#,
            r#"{"name":"OpenFront","asked":"Make a selection to launch %1$s","refuse":"",
                "ways":[{"index":0,"said":"Play","game":"OpenFront"}]}"#,
        ] {
            assert!(the_ways_it_can_start(&stopped, lost).unwrap().is_none());
        }

        // An answer that is not an answer is a problem rather than a silence:
        // silence here reads as "no panel for this", which would hide a client
        // that had stopped saying anything sensible at all.
        assert!(the_ways_it_can_start(&stopped, "not json").is_err());
    }

    /// Valve's sentences carry positional arguments, and its own localiser
    /// leaves them in.
    ///
    /// Measured rather than assumed: `LocalizeString` asked with two arguments
    /// answered "%1$s is attempting to launch with optional parameters shown
    /// below:", placeholders and all.
    #[test]
    fn valves_arguments_go_where_valve_put_them() {
        assert_eq!(
            fill_in(
                "%1$s is attempting to launch with %2$s",
                &["Isaac", "-nosound"]
            ),
            "Isaac is attempting to launch with -nosound"
        );
        // Positional, and this is why: a translation is free to reorder them,
        // and the sentence about another session names two different games.
        assert_eq!(
            fill_in("%2$s, then %1$s", &["first", "second"]),
            "second, then first"
        );
        assert_eq!(fill_in("100%% sure", &[]), "100% sure");
        // An argument that is not there leaves nothing rather than a panic.
        assert_eq!(fill_in("a %3$s b", &["one"]), "a  b");
        // And anything this does not understand is left exactly as it was: a
        // sentence with a specifier still in it reads oddly, one that has been
        // mangled reads wrongly.
        assert_eq!(fill_in("50% of %s and %1$d", &["x"]), "50% of %s and %1$d");
        assert_eq!(fill_in("nothing to do", &["x"]), "nothing to do");
    }

    /// What the client says about a download it is running, in the shapes it
    /// actually says them in.
    ///
    /// All three came off a live install of a 626 MB game on real hardware,
    /// with the manifest beside them reading `BytesDownloaded 0` throughout —
    /// which is the whole reason this half exists. See [`Live`].
    #[test]
    fn the_clients_own_account_of_a_download_is_read_as_it_gives_it() {
        // Nothing on the download list. Not an error: the ordinary state of a
        // client that is simply running.
        assert_eq!(what_the_client_said("null").unwrap(), None);

        // Starting. Nought per cent is *not yet* rather than a reading, and a
        // rate nobody has two samples for is not one either.
        let starting = what_the_client_said(
            r#"{"app_id":945360,"percent":0,"bytes_per_second":0,
                "seconds_left":-1,"state":"Starting"}"#,
        )
        .unwrap()
        .expect("a download");
        assert_eq!(starting.app_id, 945360);
        assert_eq!(starting.fraction(), None);
        assert_eq!(starting.per_second(), None);
        assert_eq!(starting.seconds_left, None, "a negative is not an estimate");
        assert!(!starting.moving);

        // Arriving. This is the line the row is drawn from.
        let moving = what_the_client_said(
            r#"{"app_id":945360,"percent":47,"bytes_per_second":61937624,
                "seconds_left":5,"state":"Downloading"}"#,
        )
        .unwrap()
        .expect("a download");
        assert_eq!(moving.fraction(), Some(0.47));
        assert_eq!(moving.per_second(), Some(61_937_624));
        assert_eq!(moving.seconds_left, Some(5));
        assert!(moving.moving);

        // Finishing: still a hundred per cent, and no rate — what the client
        // goes on reporting there is the average of something that has already
        // stopped arriving.
        let finishing = what_the_client_said(
            r#"{"app_id":945360,"percent":100,"bytes_per_second":71187895,
                "seconds_left":0,"state":"Finalizing"}"#,
        )
        .unwrap()
        .expect("a download");
        assert_eq!(finishing.fraction(), Some(1.0));
        assert_eq!(finishing.per_second(), None);

        // And a client that answers with something else is a refusal rather
        // than a panic.
        assert!(what_the_client_said("{\"app_id\":").is_err());
    }

    /// Only Valve's own default counts as the chord a shell can send, and
    /// "switched off" is a separate answer from "moved".
    ///
    /// The exact shape came off a live client — Steam stable build 1785799196,
    /// with the setting never touched — which is also where the keysym did:
    /// 65289 is `XK_Tab`.
    #[test]
    fn the_overlay_is_only_where_the_shell_can_reach_it_on_valves_own_default() {
        let read = |json: &str| serde_json::from_str::<OverlayKey>(json).expect(json);

        let default = read(
            r#"{"key_code":65289,"shift":true,"ctrl":false,"alt":false,"meta":false,
                "named":"Shift+Tab","enabled":true}"#,
        );
        assert!(default.is_shift_tab());
        assert!(default.enabled);
        assert_eq!(default.to_string(), "Shift+Tab");

        // An extra modifier is a different chord, not a near miss: what the
        // shell sends would arrive as the plain default and raise nothing.
        let with_ctrl = read(
            r#"{"key_code":65289,"shift":true,"ctrl":true,"alt":false,"meta":false,
                "named":"Shift+Ctrl+Tab","enabled":true}"#,
        );
        assert!(!with_ctrl.is_shift_tab());

        // A different key, and Shift dropped altogether.
        for json in [
            r#"{"key_code":65470,"shift":true,"ctrl":false,"alt":false,"meta":false,
                "named":"Shift+F1","enabled":true}"#,
            r#"{"key_code":65289,"shift":false,"ctrl":false,"alt":false,"meta":false,
                "named":"Tab","enabled":true}"#,
        ] {
            assert!(!read(json).is_shift_tab(), "{json}");
        }

        // The overlay switched off is the other silent failure, and it is the
        // one the key alone cannot say anything about.
        let switched_off = read(
            r#"{"key_code":65289,"shift":true,"ctrl":false,"alt":false,"meta":false,
                "named":"Shift+Tab","enabled":false}"#,
        );
        assert!(switched_off.is_shift_tab());
        assert!(!switched_off.enabled);
        assert_eq!(
            switched_off.to_string(),
            "Shift+Tab, but the overlay is switched off"
        );

        // A client that renamed the setting answers with nothing rather than
        // failing, and nothing is not the default either.
        let gone = read(
            r#"{"key_code":0,"shift":false,"ctrl":false,"alt":false,"meta":false,
                "named":"","enabled":true}"#,
        );
        assert!(!gone.is_shift_tab());
        assert_eq!(gone.to_string(), "unnamed");
    }
}
