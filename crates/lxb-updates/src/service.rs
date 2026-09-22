//! One unprivileged coordinator per account. The socket and journal are private
//! to that account. Installation is opt-in, serial, and never tied to a shell
//! window's lifetime. Authorization belongs to polkit/native package services.
use crate::{
    discovery, firmware, now, process, Phase, Provider, Request, Response, ResultEntry, Snapshot,
    Source, SourceId, PROTOCOL,
};
use anyhow::{bail, Context, Result};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const MAX_MESSAGE: u64 = 8_000_000;

/// How long the coordinator stays after the last request, with nothing
/// running and nothing staged, before it quits. Long enough that a person
/// reading a review and coming back to it finds the same process, short
/// enough that a machine left alone is not carrying one. A review itself
/// keeps for thirty minutes on disk either way.
const IDLE_SECONDS: u64 = 15 * 60;

/// How many lines a job's output holds altogether, earlier sources' and the
/// running one's together. See [`crate::TRANSCRIPT_LINES`], whose number it
/// is: a job of many sources is one transcript to the person reading it,
/// and it is bounded once, at the front.
const KEPT_LINES: usize = crate::TRANSCRIPT_LINES;

pub fn directory() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/state")))
        .context("No user state directory")?;
    Ok(base.join("lxb/updates"))
}

/// Where the coordinator listens, and where the shell knocks.
///
/// Not beside the journal: a Unix socket path is limited to 107 bytes, and a
/// state directory under a long home — or a long `XDG_STATE_HOME` — runs past
/// that, at which point binding fails with a message about `SUN_LEN` and
/// nothing about why. The runtime directory is short, private to the account,
/// and gone at logout, which is the right lifetime for a socket. It is used
/// only when it really is private — the shell writes a typed password down
/// this socket, so a directory anybody else could plant a socket in is not
/// one to knock on. Without one the socket falls back beside the state, and
/// the length is checked there too so the failure at least says what it is.
pub fn socket_path(state: &Path) -> Result<PathBuf> {
    socket_path_in(
        std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from),
        state,
    )
}

fn socket_path_in(runtime: Option<PathBuf>, state: &Path) -> Result<PathBuf> {
    let runtime = runtime.filter(|p| p.is_absolute()).filter(|p| {
        fs::symlink_metadata(p).is_ok_and(|m| {
            m.is_dir() && m.uid() == unsafe { libc::geteuid() } && m.mode() & 0o077 == 0
        })
    });
    let path = match runtime {
        Some(runtime) => runtime.join(format!("lxb-updates-{:016x}.sock", crate::stamp(state))),
        None => state.join("control.sock"),
    };
    if path.as_os_str().len() >= 108 {
        bail!(
            "The update socket path is too long for a Unix socket: {}",
            path.display()
        );
    }
    Ok(path)
}

fn private_directory(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)?;
    let m = fs::symlink_metadata(dir)?;
    if !m.is_dir() || m.uid() != unsafe { libc::geteuid() } {
        bail!("The update state directory is not owned by this account");
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn atomic<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&tmp)?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(tmp, path)?;
    File::open(path.parent().unwrap())?.sync_all()?;
    Ok(())
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    let file = File::open(path).ok()?;
    serde_json::from_reader(file.take(MAX_MESSAGE)).ok()
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Preferences {
    daily_check: bool,
}

struct State {
    snapshot: Snapshot,
    terminal: Option<File>,
    directory: PathBuf,
    daily_check: bool,
    cancel_check: bool,
    persistence_error: Option<String>,
    runtime: Arc<dyn Runtime>,
    activity: Option<File>,
    transcript: Option<crate::journal::Writer>,
    reply_log: Option<crate::journal::Chunk>,
    events: Vec<crate::JobEvent>,
    prompt: crate::prompt::Tracker,
}
type Shared = Arc<Mutex<State>>;

fn record(shared: &Shared, bytes: &[u8]) {
    let mut state = shared.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(log) = &mut state.transcript {
        if let Err(error) = log.append(bytes) {
            state.persistence_error = Some(format!(
                "Cannot record full output: {error}. No further updates will start."
            ));
        }
    }
}
/// Add a notice to the snapshot and to the job's transcript at once.
///
/// The preamble writes whatever notices the check left, but a notice raised
/// after the transcript was opened — the supervisor warning, a provider that
/// refused before it started — would otherwise be on the panel and nowhere
/// in the output somebody is told to read. Never twice.
fn notice(shared: &Shared, text: String) {
    {
        let state = shared.lock().unwrap_or_else(|p| p.into_inner());
        if state.snapshot.notices.contains(&text) {
            return;
        }
    }
    record(shared, format!("{text}\n").as_bytes());
    change(shared, |s| s.notices.push(text));
}

fn queue_event(state: &mut State, event: crate::JobEvent) {
    if state.events.iter().any(|e| e.id == event.id) {
        return;
    }
    if !event.attention {
        state.events.retain(|e| e.job != event.job || !e.attention);
    }
    state.events.push(event);
    if state.events.len() > 32 {
        state.events.remove(0);
    }
    if let Err(error) = atomic(&state.directory.join("events.json"), &state.events) {
        state
            .snapshot
            .notices
            .push(format!("Could not save update notification: {error}"));
    }
}

fn clear_attention(state: &mut State) {
    let before = state.events.len();
    state.events.retain(|event| !event.attention);
    if before != state.events.len() {
        if let Err(error) = atomic(&state.directory.join("events.json"), &state.events) {
            state
                .snapshot
                .notices
                .push(format!("Could not save update notification: {error}"));
        }
    }
}

fn change(shared: &Shared, f: impl FnOnce(&mut Snapshot)) {
    let mut state = shared.lock().unwrap_or_else(|p| p.into_inner());
    f(&mut state.snapshot);
    let State {
        prompt, snapshot, ..
    } = &mut *state;
    let prompt_changed = prompt.observe(snapshot);
    if prompt_changed {
        clear_attention(&mut state);
    }
    if prompt_changed && state.prompt.waiting() {
        let job = state.snapshot.job;
        let revision = state.snapshot.revision;
        queue_event(
            &mut state,
            crate::JobEvent {
                id: format!("{job}:attention:{revision}"),
                job,
                phase: Phase::Running,
                restart: None,
                attention: true,
                delivered: false,
            },
        );
    }
    state.snapshot.revision += 1;
    if let Err(error) = atomic(&state.directory.join("status.json"), &state.snapshot) {
        let message = format!(
            "Cannot save update history: {error}. No further native operations will start."
        );
        state.persistence_error = Some(message.clone());
        state.snapshot.message = message;
    }
}

// An output checkpoint failure must never be followed by another mutation.
// Let an already-running native operation finish; stopping it can corrupt its DB.
fn checkpoint(shared: &Shared) -> Result<()> {
    let mut state = shared.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(error) = &state.persistence_error {
        bail!("{error}");
    }
    if let Err(error) = atomic(&state.directory.join("status.json"), &state.snapshot) {
        let message = format!("Cannot save update history: {error}");
        state.persistence_error = Some(message.clone());
        bail!("{message}");
    }
    Ok(())
}

fn launch(shared: &Shared, name: &str, work: fn(&Shared)) -> Result<()> {
    let worker = shared.clone();
    if let Err(error) = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || work(&worker))
    {
        change(shared, |s| {
            s.phase = Phase::Failed;
            s.message = format!("Cannot start update worker: {error}");
        });
        shared.lock().unwrap_or_else(|p| p.into_inner()).activity = None;
        return Err(error.into());
    }
    Ok(())
}

fn archive(shared: &Shared) {
    let final_message = shared
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .snapshot
        .message
        .clone();
    record(shared, format!("\n{final_message}\n").as_bytes());
    let mut state = shared.lock().unwrap_or_else(|p| p.into_inner());
    state.transcript = None;
    let path = state.directory.join("history.json");
    let mut history: Vec<Snapshot> = read_json(&path).unwrap_or_default();
    history.retain(|s| s.job != state.snapshot.job);
    let mut summary = state.snapshot.clone();
    for source in &mut summary.sources {
        source.items.clear();
        let omitted = source.excluded.len().saturating_sub(8);
        source.excluded.truncate(8);
        for item in &mut source.excluded {
            item.name = item.name.chars().take(160).collect();
            item.detail = item.detail.chars().take(240).collect();
        }
        if omitted > 0 {
            source.note.push_str(&format!(
                " · {omitted} additional exclusions omitted from archived summary"
            ));
        }
    }
    if summary.output.len() > 100 {
        summary.output.drain(..summary.output.len() - 100);
    }
    history.push(summary);
    if history.len() > 3 {
        history.drain(..history.len() - 3);
    }
    let result = atomic(&path, &history).and_then(|_| {
        crate::journal::rotate(
            &state.directory,
            &history.iter().map(|s| s.job).collect::<Vec<_>>(),
        )
    });
    let snapshot = state.snapshot.clone();
    queue_event(
        &mut state,
        crate::JobEvent {
            id: format!("{}:finished", snapshot.job),
            job: snapshot.job,
            phase: snapshot.phase,
            restart: snapshot.restart,
            attention: false,
            delivered: false,
        },
    );
    drop(state);
    if let Err(error) = result {
        change(shared, |s| {
            s.message = format!("Could not archive this job: {error}")
        });
    }
}

fn same_user(stream: &UnixStream) -> bool {
    let mut cred = std::mem::MaybeUninit::<libc::ucred>::uninit();
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            cred.as_mut_ptr().cast(),
            &mut len,
        ) == 0
            && cred.assume_init().uid == libc::geteuid()
    }
}

/// Explicit dependency boundary: tests supply fake protection without weakening
/// the shipped executable with an environment variable or command-line bypass.
pub trait Runtime: Send + Sync {
    fn begin(&self, snapshot: &Snapshot) -> Result<JobContext>;
}
pub struct JobContext {
    authorization: Option<crate::authorization::Session>,
    _protection: Box<dyn Send>,
}
impl JobContext {
    pub fn locally_protected(protection: impl Send + 'static) -> Self {
        Self {
            authorization: None,
            _protection: Box::new(protection),
        }
    }
    fn close(mut self) -> Result<()> {
        if let Some(authorization) = self.authorization.take() {
            authorization.close()?;
        }
        Ok(())
    }
}
struct NativeRuntime;
impl Runtime for NativeRuntime {
    fn begin(&self, snapshot: &Snapshot) -> Result<JobContext> {
        // User-manager services may be outside logind's session attribution.
        // Try unprivileged protection first; otherwise the authorized worker
        // acquires and transfers a duplicate inhibitor over its private socket.
        let guard = crate::protection::Protection::acquire().ok();
        if guard.is_some() && !crate::authorization::Session::needed(snapshot)? {
            return Ok(JobContext::locally_protected(guard));
        }
        Ok(JobContext {
            authorization: Some(crate::authorization::Session::open(snapshot)?),
            _protection: Box::new(guard),
        })
    }
}

fn activity_lock(dir: &Path, mode: i32) -> Result<File> {
    private_directory(dir)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dir.join("activity.lock"))?;
    if !crate::process::flock(&file, mode)? {
        bail!("Updates are in progress. Please wait until they finish.");
    }
    Ok(file)
}
/// Held across the shell's actual power action, closing the gap between a
/// status poll and another client's Install request. No secret is in this file.
pub struct PowerPermit {
    _local: File,
    _global: Option<File>,
}
pub fn power_permit() -> Result<PowerPermit> {
    let local = activity_lock(&directory()?, libc::LOCK_EX)?;
    let global = crate::protection::power_permit()?;
    Ok(PowerPermit {
        _local: local,
        _global: global,
    })
}

pub fn serve() -> Result<()> {
    serve_with(Arc::new(NativeRuntime))
}
pub fn serve_with(runtime: Arc<dyn Runtime>) -> Result<()> {
    if unsafe { libc::geteuid() } == 0 {
        bail!("Run lxb-updates as the desktop user, never as root");
    }
    let dir = directory()?;
    private_directory(&dir)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dir.join("coordinator.lock"))?;
    // Another coordinator has the door. An error here is not that, and is
    // not swallowed: a coordinator that cannot tell the two apart declines
    // to start and says nothing about why.
    if !crate::process::flock(&lock, libc::LOCK_EX)? {
        return Ok(());
    }
    let socket = socket_path(&dir)?;
    if socket.exists() {
        fs::remove_file(&socket)?;
    }
    let listener = UnixListener::bind(&socket)
        .with_context(|| format!("Could not listen on {}", socket.display()))?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    let mut snapshot: Snapshot = read_json(&dir.join("status.json")).unwrap_or_default();
    if snapshot.protocol != PROTOCOL {
        // Preserve interruption/restart evidence from older journals. Only a
        // review is invalidated; it must not authorize a changed implementation.
        snapshot.protocol = PROTOCOL;
        if snapshot.phase == Phase::Reviewing {
            snapshot.phase = Phase::Idle;
            snapshot.message = "The update helper changed. Check again before updating.".into();
        }
    }
    let interrupted_install = snapshot.phase == Phase::Running;
    let boot_id = fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap_or_default();
    if !snapshot.boot_id.is_empty() && snapshot.boot_id != boot_id && snapshot.restart.is_some() {
        snapshot.phase = Phase::AwaitingVerification;
        snapshot.child = None;
        snapshot.restart = None;
        snapshot.message = "The machine restarted. Check again and review the native provider's status to verify the update; a reboot alone does not prove installation succeeded.".into();
        snapshot.revision += 1;
    } else if snapshot.busy() {
        snapshot.phase = Phase::Interrupted;
        snapshot.message = "The coordinator stopped during a job. Check native package history and locks before retrying; no transaction was restarted automatically.".into();
        snapshot.revision += 1;
    }
    snapshot.protected = false;
    snapshot.authorized = false;
    let daily_check = read_json::<Preferences>(&dir.join("preferences.json"))
        .map(|p| p.daily_check)
        .unwrap_or(true);
    let shared = Arc::new(Mutex::new(State {
        snapshot,
        terminal: None,
        directory: dir,
        daily_check,
        cancel_check: false,
        persistence_error: None,
        runtime,
        activity: None,
        transcript: None,
        reply_log: None,
        events: read_json(&directory()?.join("events.json")).unwrap_or_default(),
        prompt: crate::prompt::Tracker::default(),
    }));
    change(&shared, |_| {});
    if interrupted_install {
        archive(&shared);
    }
    // The coordinator is not a daemon for life. It is here for the job it
    // is doing and for a while after the last shell asked it anything, and
    // then it goes: its state is on disk, the shell starts it again on the
    // next request, and a process per login sitting on a socket for hours
    // is nothing anybody asked for. It stays while a job runs or a restart
    // is staged — those are the two things only a live coordinator knows.
    let idle_limit = std::env::var("LXB_UPDATES_IDLE_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(IDLE_SECONDS);
    listener.set_nonblocking(true)?;
    let mut last_request = std::time::Instant::now();
    loop {
        let mut ready = libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let woke = unsafe { libc::poll(&mut ready, 1, 1000) };
        if woke < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            break;
        }
        if woke == 0 {
            if last_request.elapsed() >= Duration::from_secs(idle_limit) {
                let state = shared.lock().unwrap_or_else(|p| p.into_inner());
                if !state.snapshot.busy() && state.snapshot.restart.is_none() {
                    drop(state);
                    let _ = fs::remove_file(&socket);
                    return Ok(());
                }
            }
            continue;
        }
        let mut stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(_) => continue,
        };
        last_request = std::time::Instant::now();
        if stream.set_nonblocking(false).is_err() || !same_user(&stream) {
            continue;
        }
        let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
        let response = receive(&shared, &mut stream);
        let mut state = shared.lock().unwrap_or_else(|p| p.into_inner());
        let history = if matches!(&response, Ok(true)) {
            read_json(&state.directory.join("history.json")).unwrap_or_default()
        } else {
            vec![]
        };
        let reply = Response {
            snapshot: state.snapshot.clone(),
            daily_check: state.daily_check,
            history,
            history_requested: matches!(&response, Ok(true)),
            log: state.reply_log.take(),
            events: state.events.clone(),
            error: response.err().map(|e| e.to_string()),
        };
        drop(state);
        let _ = serde_json::to_writer(&mut stream, &reply);
        let _ = stream.write_all(b"\n");
    }
    Ok(())
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    protocol: u32,
    request: Request,
}

fn receive(shared: &Shared, stream: &mut UnixStream) -> Result<bool> {
    // Do not prefetch a password into a second, unerased buffer.
    let mut reader = BufReader::with_capacity(1, stream.take(4096));
    let mut line = String::new();
    reader.read_line(&mut line)?;
    if let Some(id) = line
        .strip_prefix("INPUT ")
        .and_then(|v| v.trim().split_once(' '))
        .filter(|(version, _)| version.parse::<u32>().ok() == Some(PROTOCOL))
        .and_then(|(_, job)| job.parse::<u64>().ok())
    {
        let input = process::Input::read(reader.get_mut())?;
        let result = {
            let mut state = shared.lock().unwrap_or_else(|p| p.into_inner());
            if state.snapshot.job != id || state.snapshot.phase != Phase::Running {
                Err(anyhow::anyhow!(
                    "This transaction is no longer accepting responses"
                ))
            } else {
                let result = match state.terminal.as_mut() {
                    Some(terminal) => process::write_line(terminal, input.bytes()),
                    None => Err(anyhow::anyhow!("No active package-manager prompt")),
                };
                if result.is_ok() {
                    state.prompt.answered();
                    clear_attention(&mut state);
                }
                result
            }
        };
        drop(input);
        result?;
        return Ok(false);
    }
    let envelope: Envelope = serde_json::from_str(&line)?;
    if envelope.protocol != PROTOCOL {
        bail!("The update helper and desktop versions do not match; no action was taken");
    }
    match envelope.request {
        Request::Status | Request::Preferences => {}
        Request::History => return Ok(true),
        Request::Events => {}
        Request::Delivered { event } => {
            let mut state = shared.lock().unwrap_or_else(|p| p.into_inner());
            for pending in &mut state.events {
                if pending.id == event {
                    pending.delivered = true;
                }
            }
            atomic(&state.directory.join("events.json"), &state.events)?;
        }
        Request::Acknowledge { event } => {
            let mut state = shared.lock().unwrap_or_else(|p| p.into_inner());
            let mut pending = state.events.clone();
            pending.retain(|e| e.id != event);
            atomic(&state.directory.join("events.json"), &pending)?;
            state.events = pending;
        }
        Request::Output { job, offset } => {
            let mut state = shared.lock().unwrap_or_else(|p| p.into_inner());
            let history: Vec<Snapshot> =
                read_json(&state.directory.join("history.json")).unwrap_or_default();
            if state.snapshot.job != job && !history.iter().any(|s| s.job == job) {
                bail!("This update is no longer retained");
            }
            state.reply_log = Some(crate::journal::read(&state.directory, job, offset)?);
        }
        Request::CancelCheck { job } => {
            let mut state = shared.lock().unwrap_or_else(|p| p.into_inner());
            if state.snapshot.job != job || state.snapshot.phase != Phase::Checking {
                bail!("There is no matching check to cancel");
            }
            state.cancel_check = true;
            state.snapshot.message = "Stopping after the current read-only query finishes".into();
            state.snapshot.revision += 1;
        }
        Request::Restart { job } => {
            let permit = power_permit()?;
            let restart = {
                let mut state = shared.lock().unwrap_or_else(|p| p.into_inner());
                if state.snapshot.job != job || state.snapshot.busy() {
                    bail!("Another update job is active or this result changed");
                }
                if let Some(error) = &state.persistence_error {
                    bail!("{error}");
                }
                let previous = state.snapshot.clone();
                let restart = state
                    .snapshot
                    .restart
                    .context("No staged restart was reported")?;
                state.snapshot.phase = Phase::Restarting;
                state.snapshot.message = "Restart requested. Save your work; authorization and other applications' inhibitors still apply.".into();
                state.snapshot.revision += 1;
                if let Err(error) = atomic(&state.directory.join("status.json"), &state.snapshot) {
                    state.snapshot = previous;
                    return Err(error);
                }
                restart
            };
            let shared = shared.clone();
            std::thread::spawn(move || {
                let _permit = permit;
                let result: Result<()> = match restart {
                    crate::Restart::Dnf5Offline => transaction(
                        &shared,
                        &process::Step::new("dnf5", &["offline", "reboot"], true),
                    ),
                    crate::Restart::Normal => (|| {
                        let connection = zbus::blocking::Connection::system()?;
                        let proxy = zbus::blocking::Proxy::new(
                            &connection,
                            "org.freedesktop.login1",
                            "/org/freedesktop/login1",
                            "org.freedesktop.login1.Manager",
                        )?;
                        proxy.call::<_, _, ()>("Reboot", &(true,))?;
                        Ok(())
                    })(),
                };
                if let Err(error) = result {
                    change(&shared, |s| {
                        s.phase = Phase::Partial;
                        s.message = format!("Restart was not completed: {error}");
                    });
                }
            });
        }
        Request::SetDailyCheck(value) => {
            let mut state = shared.lock().unwrap_or_else(|p| p.into_inner());
            atomic(
                &state.directory.join("preferences.json"),
                &Preferences { daily_check: value },
            )?;
            state.daily_check = value;
        }
        Request::Check { selected } => {
            if selected.contains(&SourceId::Aur) {
                bail!("AUR packages must be updated manually");
            }
            {
                let mut state = shared.lock().unwrap_or_else(|p| p.into_inner());
                if state.snapshot.busy() {
                    bail!("An update job is already active");
                }
                if state
                    .snapshot
                    .child
                    .is_some_and(|pid| Path::new(&format!("/proc/{pid}")).exists())
                {
                    bail!("The previous native transaction may still be running. Review native history first.");
                }
                let previous = state.snapshot.clone();
                let job = now().max(state.snapshot.job + 1);
                state.cancel_check = false;
                let restart = state.snapshot.restart;
                state.snapshot = Snapshot {
                    job,
                    phase: Phase::Checking,
                    selected,
                    started: now(),
                    restart,
                    boot_id: fs::read_to_string("/proc/sys/kernel/random/boot_id")
                        .unwrap_or_default(),
                    revision: state.snapshot.revision + 1,
                    ..Snapshot::default()
                };
                // Save before work starts. If storage is unavailable, no work
                // is launched under a job that cannot be recovered.
                if let Err(error) = atomic(&state.directory.join("status.json"), &state.snapshot) {
                    state.snapshot = previous;
                    return Err(error);
                }
                state.persistence_error = None;
            }
            launch(shared, "update-check", check_job)?;
        }
        Request::Install { job } => {
            if shared
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .snapshot
                .selected
                .contains(&SourceId::Aur)
            {
                bail!(
                    "This review includes AUR. Check again; AUR packages must be updated manually"
                );
            }
            {
                let mut state = shared.lock().unwrap_or_else(|p| p.into_inner());
                if state.snapshot.phase != Phase::Reviewing || state.snapshot.job != job {
                    bail!("The update review changed. Check again before installing.");
                }
                if now().saturating_sub(state.snapshot.started) > 1800 {
                    bail!("The review is older than 30 minutes. Check again before installing.");
                }
                if let Some(error) = &state.persistence_error {
                    bail!("{error}");
                }
                if state.snapshot.restart.is_some()
                    && state.snapshot.selected.contains(&SourceId::System)
                {
                    bail!("A system deployment is already staged. Restart and verify it before preparing another system update.");
                }
                let previous = state.snapshot.clone();
                let activity = activity_lock(&state.directory, libc::LOCK_SH)?;
                let mut transcript = crate::journal::Writer::create(&state.directory, job)?;
                let mut preamble = format!("Update {}\n\n", job);
                for source in &state.snapshot.sources {
                    if !state.snapshot.selected.contains(&source.id) {
                        continue;
                    }
                    preamble.push_str(&format!("— {} —\n{}\n", source.id.name(), source.note));
                    if let Some(error) = &source.error {
                        preamble.push_str(&format!("Check failed: {error}\n"));
                    }
                    for item in &source.items {
                        preamble.push_str(&format!("{}  {}\n", item.name, item.detail));
                    }
                    for item in &source.excluded {
                        preamble.push_str(&format!("Excluded: {} — {}\n", item.name, item.detail));
                    }
                }
                for notice in &state.snapshot.notices {
                    preamble.push_str(&format!("{notice}\n"));
                }
                if let Err(error) = transcript.append(preamble.as_bytes()) {
                    drop(transcript);
                    let _ = fs::remove_file(state.directory.join(format!("{job}.log")));
                    return Err(error);
                }
                state.transcript = Some(transcript);
                state.activity = Some(activity);
                state.snapshot.phase = Phase::Running;
                state.snapshot.revision += 1;
                if let Err(error) = atomic(&state.directory.join("status.json"), &state.snapshot) {
                    state.activity = None;
                    state.transcript = None;
                    let _ = fs::remove_file(state.directory.join(format!("{job}.log")));
                    state.snapshot = previous;
                    return Err(error);
                }
            }
            launch(shared, "update-install", install_job)?;
        }
    }
    Ok(false)
}

fn check_job(shared: &Shared) {
    let mut sources = discovery::discover();
    let selected = {
        let state = shared.lock().unwrap_or_else(|p| p.into_inner());
        let mut selected = state.snapshot.selected.clone();
        if selected.is_empty() {
            selected = sources.iter().map(|s| s.id).collect();
        }
        selected
    };
    change(shared, |s| {
        s.selected = selected.clone();
        s.sources = sources.clone();
        s.message = "Checking update sources".into();
    });
    for source in &mut sources {
        if shared
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .cancel_check
        {
            change(shared, |s| {
                s.phase = Phase::Cancelled;
                s.active = None;
                s.message = "Check cancelled. No installation was started.".into();
            });
            return;
        }
        if selected.contains(&source.id) {
            change(shared, |s| {
                s.active = Some(source.id);
                s.message = format!("Checking {}", source.id.title());
            });
            discovery::check(source);
            change(shared, |s| {
                if let Some(old) = s.sources.iter_mut().find(|s| s.id == source.id) {
                    *old = source.clone();
                }
            });
        }
    }
    {
        // Serialize the final transition with CancelCheck, including cancellation
        // received while the last provider query was running.
        let mut state = shared.lock().unwrap_or_else(|p| p.into_inner());
        let cancelled = state.cancel_check;
        let s = &mut state.snapshot;
        s.phase = if cancelled {
            Phase::Cancelled
        } else {
            Phase::Reviewing
        };
        s.active = None;
        s.sources = sources;
        s.message = if cancelled { "Check cancelled. No installation was started." }
            else { "Review sources and exclusions. Native package managers confirm their final transaction before applying changes; some providers start after Continue. BIOS/UEFI is never installed." }.into();
    }
    change(shared, |_| {});
    match crate::preflight::check() {
        Ok(notices) => change(shared, |s| s.notices = notices),
        Err(error) => change(shared, |s| s.notices = vec![error.to_string()]),
    }
}

fn install_job(shared: &Shared) {
    if std::env::var_os("LXB_UPDATES_SUPERVISED").is_none() {
        notice(
            shared,
            // A statement rather than an instruction: this notice outlives
            // the job it is about, and "keep this session open until the
            // update finishes" read as an order on the panel of an update
            // that had already finished.
            "Updates stop if this session ends · the coordinator is detached and has no user-service supervisor".into(),
        );
    }
    if let Err(error) = crate::preflight::check() {
        notice(shared, error.to_string());
        change(shared, |s| {
            s.phase = Phase::Failed;
            s.message = error.to_string();
        });
        shared.lock().unwrap_or_else(|p| p.into_inner()).activity = None;
        archive(shared);
        return;
    }
    let (snapshot, runtime) = {
        let state = shared.lock().unwrap_or_else(|p| p.into_inner());
        (state.snapshot.clone(), state.runtime.clone())
    };
    change(shared, |s| {
        s.message = "Preparing updates and protecting your system".into()
    });
    let mut context = match runtime.begin(&snapshot) {
        Ok(context) => context,
        Err(error) => {
            notice(shared, error.to_string());
            change(shared, |s| {
                s.phase = Phase::Failed;
                s.message = format!("Updates have not started: {error}");
            });
            shared.lock().unwrap_or_else(|p| p.into_inner()).activity = None;
            archive(shared);
            return;
        }
    };
    change(shared, |s| {
        s.protected = true;
        s.authorized = context.authorization.is_some();
    });
    let current = discovery::Host::read().system();
    // No source waits on another. Each is a transaction of its own, with its
    // own confirmation, and each was chosen in the review on its own, so a
    // system update that failed or needs attention is no reason to skip the
    // Flatpaks or the firmware. The one source that ever waited was AUR,
    // which is no longer run at all.
    for source in &snapshot.sources {
        if !snapshot.selected.contains(&source.id) {
            continue;
        }
        let result = if let Some(reason) = held_back(source, current) {
            Err(anyhow::anyhow!(reason))
        } else {
            change(shared, |s| {
                s.active = Some(source.id);
                // The job's output runs on under a heading for each source
                // rather than starting over: a result that says "see its
                // output" has to have output to see after the next source
                // has run. Bounded below, at the start of each transaction.
                if !s.output.is_empty() {
                    s.output.push(String::new());
                }
                s.output.push(format!("— {} —", source.id.name()));
                s.message = source.id.title().into();
            });
            record(shared, format!("\n— {} —\n", source.id.name()).as_bytes());
            install_source(shared, source, &mut context)
        };
        let success = result.is_ok();
        let note = match result {
            Ok(note) => note,
            Err(error) => error.to_string(),
        };
        record(
            shared,
            format!("\n[{}: {}]\n", source.id.name(), note).as_bytes(),
        );
        change(shared, |s| {
            // What became of the source goes into the transcript as well as
            // the results, so the output reads as the whole story: a source
            // refused before its tool was ever started — held back, or a
            // root step whose executable is not trusted — is a heading with
            // nothing under it otherwise, and a tool that exited badly has
            // its exit written after its last line, where a terminal would
            // have shown it.
            s.output.push(if success {
                format!("[{}]", note)
            } else {
                format!("[{}: {}]", source.id.name(), note)
            });
            if s.output.len() > KEPT_LINES {
                s.output.drain(..s.output.len() - KEPT_LINES);
            }
            s.results.push(ResultEntry {
                source: source.id,
                note,
                success,
            });
        });
    }
    // Revoke the finite grant and release power protection before publishing
    // a finished state. Even failure/cancellation never leaves cached authority.
    let cleanup = context.close();
    change(shared, |s| {
        s.active = None;
        s.child = None;
        s.secret = false;
        s.protected = false;
        s.authorized = false;
        let good = s.results.iter().any(|r| r.success);
        let bad = s.results.iter().any(|r| !r.success);
        s.phase = if bad {
            if good {
                Phase::Partial
            } else {
                Phase::Failed
            }
        } else {
            Phase::Completed
        };
        s.message = if bad {
            "Some updates could not be installed. Review each source's result."
        } else {
            "Update commands finished. Counting what is left to update."
        }
        .into();
    });
    // Neither of the two below is an installation outcome, and neither may
    // move the phase. What installed, installed: a job where every provider
    // succeeded and the log could not be written is a job that succeeded and
    // could not be written down, and calling it "partly installed" sends
    // somebody hunting for a package that failed and never existed. The
    // other direction is worse — Partial over a job where *nothing*
    // installed would claim a success there was none of.
    if let Err(error) = cleanup {
        notice(
            shared,
            format!("Authorization cleanup needs attention: {error}"),
        );
    }
    shared.lock().unwrap_or_else(|p| p.into_inner()).activity = None;
    let persistence_error = shared
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .persistence_error
        .clone();
    if let Some(error) = persistence_error {
        // Recorded on the snapshot only. `record` is what just failed, so
        // writing this into the transcript is the one place that would be
        // asking the broken thing to report its own breakage.
        change(shared, |s| {
            if !s.notices.contains(&error) {
                s.notices.push(error);
            }
        });
    }
    archive(shared);
    // Last, and after the finish has been announced: the count the page
    // shows is about the machine as it is now, not as the review found it.
    recount(shared, snapshot.job);
}

/// Count what is still waiting, now that the tools have run.
///
/// A review is a photograph of what was waiting *before* the job, and
/// installing a package does not change a photograph — so the Settings column
/// went on saying “10 updates available” over a machine that had just
/// installed all ten, until somebody pressed a row and made it check again.
/// The user's words for it, 2026-09-22: “after updates has been done, the
/// number of available updates in the description is not changing until
/// pressed again”.
///
/// It asks rather than assumes. A tool that exited nought is not proof that
/// every package it was handed went on, and a transaction that failed
/// half-way may still have installed the first half; the only honest count is
/// the one the provider gives when it is asked again — the same bounded,
/// read-only query the check itself runs, no root and no more network than a
/// check already uses.
///
/// After [`archive`], deliberately: the finish is announced from there, and a
/// notification that waited for `flatpak remote-ls` to come back would be an
/// update that had been over for ten seconds before it said so.
///
/// Quiet, too. The phase, the results, the restart and the transcript are what
/// the finished panel is about and none of them is touched; only the sources
/// move, one at a time as each answers, and only while this is still the job
/// the coordinator is holding — Check again on that same panel starts a job of
/// its own, and this must never write over its findings.
fn recount(shared: &Shared, job: u64) {
    let sources: Vec<crate::Source> = {
        let state = shared.lock().unwrap_or_else(|p| p.into_inner());
        if state.snapshot.job != job {
            return;
        }
        state
            .snapshot
            .sources
            .iter()
            .filter(|source| state.snapshot.selected.contains(&source.id))
            .cloned()
            .collect()
    };
    for mut source in sources {
        if shared
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .snapshot
            .job
            != job
        {
            return;
        }
        discovery::check(&mut source);
        change(shared, |s| {
            if s.job != job {
                return;
            }
            if let Some(held) = s.sources.iter_mut().find(|held| held.id == source.id) {
                *held = source.clone();
            }
        });
    }
}

/// Why a reviewed source is not run now, if it is not.
///
/// Nothing waits on anything: each source is a transaction of its own with
/// its own confirmation, and each was chosen in the review on its own.
/// Photographed the other way on 2026-09-15, when AUR still waited on the
/// system: one failed system step and every source after it read "deferred".
///
/// AUR is the one identity that is never run. A review carried over from an
/// older helper can still name it, so it is refused here as well as at the
/// door — a stale review is a reason to say what the rule is, not to build a
/// package nobody asked for.
fn held_back(source: &crate::Source, current: crate::System) -> Option<String> {
    if source.id == SourceId::Aur {
        return Some("AUR packages must be updated manually".into());
    }
    if let Some(error) = &source.error {
        return Some(error.clone());
    }
    if !source.executable {
        return Some(source.note.clone());
    }
    if matches!(source.provider, Provider::System(system) if system != current) {
        return Some("Skipped · the system provider changed; check again".into());
    }
    None
}

fn install_source(
    shared: &Shared,
    source: &crate::Source,
    context: &mut JobContext,
) -> Result<String> {
    if let Provider::Custom(provider) = &source.provider {
        provider.verify()?;
        if source.listed && source.items.is_empty() {
            return Ok("No updates available".into());
        }
        change(shared, |s| s.custom_result = None);
        execute(shared, source, 0, &provider.step()?, context)?;
        let result = shared
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .snapshot
            .custom_result
            .clone()
            .context("The provider did not report its outcome")?;
        if result.restart {
            change(shared, |s| s.restart = Some(crate::Restart::Normal));
        }
        if result.outcome == crate::custom::Outcome::NeedsAttention {
            bail!("{}", result.summary);
        }
        return Ok(result.summary);
    }
    if matches!(source.provider, Provider::Firmware) {
        if source.items.is_empty() {
            return Ok(
                "No eligible device updates · BIOS/UEFI and unclassified devices excluded".into(),
            );
        }
        for (index, item) in source.items.iter().enumerate() {
            let id = item
                .detail
                .split_whitespace()
                .next()
                .context("No firmware device identity")?;
            let step = firmware::installation(id)?;
            execute(shared, source, index, &step, context)?;
        }
        return Ok("Finished · follow fwupd's restart or power-cycle advice".into());
    }
    let steps = discovery::steps(source)?;
    let mut staged = false;
    for (index, step) in steps.into_iter().enumerate() {
        execute(shared, source, index, &step, context)?;
        staged |= step.staged;
    }
    if matches!(source.provider, Provider::System(crate::System::Pacman)) {
        let remaining = process::probe("pacman", &["-Qu", "--color", "never"], &[0, 1])?;
        record(
            shared,
            format!(
                "\npacman -Qu --color never (verification)\n{}\n",
                remaining.text
            )
            .as_bytes(),
        );
        let ignored = verify_pacman(&remaining.text)?;
        if ignored > 0 {
            notice(
                shared,
                format!(
                    "{ignored} packages were kept at their configured versions. See Full output."
                ),
            );
        }
    }
    if staged {
        let restart = if matches!(source.provider, Provider::System(crate::System::Dnf5)) {
            crate::Restart::Dnf5Offline
        } else {
            crate::Restart::Normal
        };
        change(shared, |s| s.restart = Some(restart));
    }
    Ok(if staged {
        "Staged for the next boot · restart to finish"
    } else if source.id == SourceId::System {
        "Finished · a restart may be needed; check again to verify"
    } else {
        "Finished · open applications keep the old version until restarted"
    }
    .into())
}

/// `pacman -Qu` includes configured IgnorePkg/IgnoreGroup entries, marked in
/// LC_ALL=C output. A successful upgrade that respected those settings is not
/// a failure. All other remaining output still requires attention, including
/// unrecognized formats; never bypass an ignore rule to make the list empty.
fn verify_pacman(text: &str) -> Result<usize> {
    let mut ignored = 0;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() == 5 && fields[2] == "->" && fields[4] == "[ignored]" {
            ignored += 1;
        } else {
            bail!("Repository updates remain · review the verification list in Full output");
        }
    }
    Ok(ignored)
}

fn execute(
    shared: &Shared,
    source: &Source,
    index: usize,
    step: &process::Step,
    context: &mut JobContext,
) -> Result<()> {
    if !crate::authorization::privileged(source, step, index) || context.authorization.is_none() {
        return transaction(shared, step);
    }
    checkpoint(shared)?;
    let session = context.authorization.as_mut().unwrap();
    let mut before = shared
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .snapshot
        .output
        .clone();
    before.push(step.program.clone());
    record(shared, format!("\n{}\n", step.program).as_bytes());
    let mut transcript = process::Transcript::default();
    shared.lock().unwrap_or_else(|p| p.into_inner()).terminal = Some(session.input()?);
    let result = session.run(source.id, index, |event| {
        use crate::authorization::Event;
        match event {
            Event::Started(pid) => change(shared, |s| s.child = Some(pid)),
            Event::CustomResult(result) => change(shared, |s| s.custom_result = Some(result)),
            Event::Echo(secret) => {
                let old = shared
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .snapshot
                    .secret;
                if old != secret {
                    change(shared, |s| s.secret = secret);
                }
            }
            Event::Output(bytes) => {
                record(shared, &bytes);
                transcript.push_bytes(&bytes);
                let mut output = before.clone();
                output.extend(transcript.lines());
                if output.len() > KEPT_LINES {
                    output.drain(..output.len() - KEPT_LINES);
                }
                change(shared, |s| s.output = output);
            }
            _ => {}
        }
    });
    shared.lock().unwrap_or_else(|p| p.into_inner()).terminal = None;
    change(shared, |s| {
        s.child = None;
        s.secret = false;
    });
    result
}

fn transaction(shared: &Shared, step: &process::Step) -> Result<()> {
    checkpoint(shared)?;
    let mut transaction = process::start(step)?;
    let mut terminal = transaction.terminal.try_clone()?;
    {
        let mut state = shared.lock().unwrap_or_else(|p| p.into_inner());
        state.terminal = Some(transaction.terminal.try_clone()?);
    }
    change(shared, |s| {
        s.child = Some(transaction.child.id());
        s.message = format!(
            "{} — read the output before sending a response",
            step.program
        );
    });
    let reader_shared = shared.clone();
    let name = step.program.clone();
    let reader_done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let finished = reader_done.clone();
    // What the job has said so far, kept in front of this transaction's
    // transcript; the oldest of it goes first so the whole stays bounded.
    let mut before = shared
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .snapshot
        .output
        .clone();
    if before.len() > KEPT_LINES {
        before.drain(..before.len() - KEPT_LINES);
    }
    record(shared, format!("\n{name}\n").as_bytes());
    let reader = std::thread::spawn(move || {
        let mut transcript = process::Transcript::default();
        transcript.push(&format!("{name}\n"));
        let mut buf = [0u8; 4096];
        let mut drain_deadline = None;
        loop {
            if finished.load(std::sync::atomic::Ordering::Acquire) {
                let deadline = drain_deadline
                    .get_or_insert_with(|| std::time::Instant::now() + Duration::from_millis(500));
                if std::time::Instant::now() >= *deadline {
                    break;
                }
            }
            let mut ready = libc::pollfd {
                fd: terminal.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let result = unsafe { libc::poll(&mut ready, 1, 200) };
            if result == 0 {
                if finished.load(std::sync::atomic::Ordering::Acquire) {
                    break;
                }
                continue;
            }
            if result < 0 {
                break;
            }
            let n = match terminal.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            record(&reader_shared, &buf[..n]);
            transcript.push_bytes(&buf[..n]);
            let mut output = before.clone();
            output.extend(transcript.lines());
            // The one bound on the whole: the oldest of the earlier sources'
            // lines go as this one's arrive.
            if output.len() > KEPT_LINES {
                output.drain(..output.len() - KEPT_LINES);
            }
            change(&reader_shared, |s| s.output = output);
        }
    });
    let status = loop {
        if let Some(status) = transaction.child.try_wait()? {
            break status;
        }
        let secret = shared
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .terminal
            .as_ref()
            .map(process::secret)
            .unwrap_or(true);
        let previous = shared
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .snapshot
            .secret;
        if previous != secret {
            change(shared, |s| s.secret = secret);
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    shared.lock().unwrap_or_else(|p| p.into_inner()).terminal = None;
    reader_done.store(true, std::sync::atomic::Ordering::Release);
    let _ = reader.join();
    change(shared, |s| {
        s.child = None;
        s.secret = false;
    });
    if !status.success() {
        bail!(
            "{} exited {} · see its output before retrying",
            step.program,
            status.code().unwrap_or(-1)
        );
    }
    if let Some(result) = process::custom_result(&mut transaction)? {
        change(shared, |s| s.custom_result = Some(result));
    }
    Ok(())
}

fn connect(helper: &Path) -> Result<UnixStream> {
    let dir = directory()?;
    let socket = socket_path(&dir)?;
    if let Ok(stream) = UnixStream::connect(&socket) {
        return Ok(stream);
    }
    private_directory(&dir)?;
    if crate::supervisor::start(helper, &dir) {
        for _ in 0..20 {
            if let Ok(stream) = UnixStream::connect(&socket) {
                return Ok(stream);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    // The lock in serve() prevents two coordinators if the user manager starts
    // slowly and wins the race with this fallback.
    let mut command = std::process::Command::new(helper);
    command
        .arg("serve")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().context("Could not start lxb-updates")?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    for _ in 0..30 {
        if let Ok(stream) = UnixStream::connect(&socket) {
            return Ok(stream);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    bail!("The update coordinator is unavailable")
}

pub fn request(helper: &Path, request: &Request) -> Result<Response> {
    let mut stream = connect(helper)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    serde_json::to_writer(
        &mut stream,
        &serde_json::json!({"protocol": PROTOCOL, "request": request}),
    )?;
    stream.write_all(b"\n")?;
    let response: Response = serde_json::from_reader(BufReader::new(stream.take(MAX_MESSAGE)))?;
    if response.snapshot.protocol != PROTOCOL {
        bail!("The update helper and desktop versions do not match");
    }
    Ok(response)
}

/// Input is intentionally not serialized into a Request. Callers can write a
/// Secret directly into this stream, then erase their buffer.
pub fn input(
    helper: &Path,
    job: u64,
    write: impl FnOnce(&mut UnixStream) -> std::io::Result<()>,
) -> Result<Response> {
    let mut stream = connect(helper)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    writeln!(stream, "INPUT {PROTOCOL} {job}")?;
    write(&mut stream)?;
    Ok(serde_json::from_reader(BufReader::new(
        stream.take(MAX_MESSAGE),
    ))?)
}

#[cfg(test)]
mod tests {
    #[test]
    fn successful_pacman_upgrade_respects_configured_ignores() {
        assert_eq!(verify_pacman("").unwrap(), 0);
        let held =
            "libc++ 20.1.6-2 -> 22.1.8-1.1 [ignored]\nlibc++abi 20.1.6-2 -> 22.1.8-1.1 [ignored]\n";
        assert_eq!(verify_pacman(held).unwrap(), 2);
        assert!(verify_pacman(&format!("{held}linux 6.17-1 -> 6.18-1\n")).is_err());
        assert!(verify_pacman("warning: cannot check [ignored]\n").is_err());
        assert!(verify_pacman("linux 1 -> 2 [unknown]\n").is_err());
    }

    /// The lock file is private to this account, and taking it twice from
    /// one process is not what it is for.
    ///
    /// What it *is* for — an installation and a power action refusing each
    /// other — is between two processes, always: the coordinator and the
    /// shell. That is tested where there are two, in
    /// `tests/coordinator.rs`. It used to be tested here by asking the
    /// kernel whether one process may hold two conflicting `flock`s on one
    /// file, which `flock(2)` answers with "may", and which failed about one
    /// run in ten — but only when the test harness was running other tests
    /// beside it, because a `fork` in a multi-threaded binary inherits every
    /// descriptor the other threads have open.
    #[test]
    fn the_activity_lock_is_private_to_this_account() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("lxb-activity-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let held = super::activity_lock(&dir, libc::LOCK_SH).unwrap();
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let file = dir.join("activity.lock");
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // Nothing of the machine's is written into it.
        assert_eq!(std::fs::metadata(&file).unwrap().len(), 0);
        drop(held);
        // Taking it again on a file that is already there is not an error.
        super::activity_lock(&dir, libc::LOCK_SH).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    use super::*;

    fn reviewed(id: SourceId, provider: Provider) -> crate::Source {
        crate::Source {
            id,
            provider,
            note: "Updates available".into(),
            items: vec![],
            excluded: vec![],
            error: None,
            checked: Some(1),
            fresh: true,
            listed: true,
            executable: true,
            policy: None,
        }
    }

    /// No source waits on another, and AUR is never run. A review carried
    /// over from an older helper can still name AUR, and naming it is
    /// refused rather than deferred — there is no system step that would
    /// let it through. A source the check could not read is held back by its
    /// own reason, whatever any other source did.
    #[test]
    fn aur_is_refused_and_nothing_else_waits_on_anything() {
        use crate::System as S;
        let aur = reviewed(
            SourceId::Aur,
            Provider::Aur {
                helper: Some("paru".into()),
            },
        );
        assert_eq!(
            held_back(&aur, S::Pacman).as_deref(),
            Some("AUR packages must be updated manually")
        );

        for source in [
            reviewed(
                SourceId::Flatpak,
                Provider::Flatpak {
                    installations: vec!["user".into()],
                },
            ),
            reviewed(SourceId::Firmware, Provider::Firmware),
            reviewed(SourceId::Snap, Provider::Snap),
        ] {
            assert_eq!(held_back(&source, S::Pacman), None, "{:?}", source.id);
        }

        let mut broken = reviewed(
            SourceId::Flatpak,
            Provider::Flatpak {
                installations: vec![],
            },
        );
        broken.error = Some("flatpak is not installed".into());
        assert_eq!(
            held_back(&broken, S::Pacman).as_deref(),
            Some("flatpak is not installed")
        );

        // The system itself runs only on the host it was reviewed on.
        let system = reviewed(SourceId::System, Provider::System(S::Pacman));
        assert_eq!(held_back(&system, S::Pacman), None);
        assert!(held_back(&system, S::Apt).is_some());
    }

    /// A state directory under a long home is the case that produced this:
    /// the socket beside it ran past what a Unix socket path may be, and the
    /// coordinator died on start with a message about `SUN_LEN`. With a
    /// runtime directory the socket goes there and stays short whatever the
    /// state path is; without one the failure at least says what it is.
    #[test]
    fn the_socket_stays_short_under_a_long_state_directory() {
        let runtime = std::env::temp_dir().join(format!("lxb-updates-rt-{}", std::process::id()));
        let _ = fs::remove_dir_all(&runtime);
        fs::create_dir_all(&runtime).unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        let state = PathBuf::from(format!("/{}/lxb/updates", "h".repeat(120)));

        let socket = socket_path_in(Some(runtime.clone()), &state).unwrap();
        assert_eq!(socket.parent(), Some(runtime.as_path()));
        assert!(socket.as_os_str().len() < 108, "{}", socket.display());
        // Two state directories are two sockets; the same one is the same.
        assert_eq!(
            socket,
            socket_path_in(Some(runtime.clone()), &state).unwrap()
        );
        assert_ne!(
            socket,
            socket_path_in(Some(runtime.clone()), &state.join("other")).unwrap()
        );

        let error = socket_path_in(None, &state).unwrap_err().to_string();
        assert!(error.contains("too long"), "{error}");

        // A runtime directory anybody else could write into is not used: the
        // shell sends a typed password down this socket.
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            socket_path_in(Some(runtime.clone()), Path::new("/short")).unwrap(),
            Path::new("/short/control.sock")
        );
        let _ = fs::remove_dir_all(&runtime);
    }
}
