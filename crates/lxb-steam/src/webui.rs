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
const UNTIL_THE_WIZARD_ANSWERS: Duration = Duration::from_secs(45);

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
/// is left behind afterwards because taking it away again would only mean
/// making it once more at the next press.
pub fn expose(root: &std::path::Path) -> std::io::Result<bool> {
    let marker = root.join(MARKER);
    if marker.is_file() {
        return Ok(false);
    }
    // Empty on purpose: the client tests for the file, never reads it.
    std::fs::write(&marker, b"")?;
    tracing::info!(
        path = %marker.display(),
        "told Valve's client to expose the interface this shell signs it in through"
    );
    Ok(true)
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
    const WANTED: [&str; 7] = [
        "Auth.SetLoginToken",
        "Installs.RegisterForShowInstallWizard",
        "Installs.OpenInstallWizard",
        "Installs.SetCreateShortcuts",
        "Installs.ContinueInstall",
        "Installs.CancelInstall",
        "Installs.OpenUninstallWizard",
    ];
    let socket = context()?;
    let wanted = WANTED
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
/// and no error. Those are [`Problem::Asks`] now, because a shell that clicked
/// through somebody's licence agreement for them would be answering a question
/// it was never asked. The wizard is cancelled on the way out, so the next
/// press does not walk into a flow that is still standing.
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
/// The install folder is deliberately not chosen: the client's default is the
/// one the user set in the client, and a shell that overrode it would put games
/// somewhere they did not ask for and did not expect.
pub fn install(app_id: u32) -> Result<(), Problem> {
    let socket = context()?;
    // `SetCreateShortcuts(false, false)` because the two it makes are a
    // desktop file and a start-menu entry on a machine that may have neither
    // — this shell *is* the menu, and the game is already a row on it.
    let call = format!(
        "(async () => {{ \
           {knows} \
           const answer = await new Promise(settle => {{ \
             let done = false; \
             let going = false; \
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
             watch = SteamClient.Installs.RegisterForShowInstallWizard(async where => {{ \
               switch (where.eInstallState) {{ \
                 case {config}: \
                   if (going) return; \
                   going = true; \
                   await SteamClient.Installs.SetCreateShortcuts(false, false); \
                   await SteamClient.Installs.ContinueInstall(); \
                   return; \
                 case {complete}: return finish({{ ok: true }}); \
                 case {failed_state}: return failed( \
                   where.errorDetail || 'Steam could not start this download', \
                   where.eAppError || 0 \
                 ); \
                 case {canceled}: return failed('Steam stopped it', 0); \
                 case {eulas}: return asks('an agreement to accept'); \
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
pub fn stop_installing(app_id: u32) -> Result<(), Problem> {
    let socket = context()?;
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
pub fn uninstall(app_id: u32) -> Result<(), Problem> {
    let socket = context()?;
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
        let root = std::env::temp_dir().join(format!("lxb-withdraw-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

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
}
