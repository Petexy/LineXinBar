//! Ending an application, as against ending a process.
//!
//! Close used to send `SIGKILL` to the pid behind the window and stop there,
//! which is right whenever the window's process *is* the application. Two of
//! the things this shell exists to run are not like that, and both were
//! measured on this machine rather than reasoned about:
//!
//! * **Steam.** The window belongs to `steamwebhelper`, which the `steam`
//!   client supervises. Killing it left `steam` running and a fresh webhelper
//!   — and a fresh window — turned up seconds later. The client sits four
//!   processes further up the tree and pressure-vessel gives the webhelper a
//!   session and a process group of its own on the way, so neither the
//!   window's group nor its session reaches the client. Signalling the
//!   *launch* instead — the process the shell started, and everything
//!   descended from it — took the whole of Steam down in about three seconds.
//!
//! * **Waydroid.** The window belongs to an Android HAL service inside the
//!   container, which Android's own init restarts, as root. There is no signal
//!   a user session can send that ends it: killing it produced a replacement,
//!   with a new window, within seconds. What ends it is `waydroid session
//!   stop`. What closes a single Android app is the polite close this path
//!   deliberately skips — Waydroid honours that one and finishes the task,
//!   while ignoring it on the full-UI window.
//!
//! So Close works out *what the application is* first, and only then how to
//! end it. The one thing that does not change is the promise: whatever is
//! chosen here, the application cannot decline it. A client that ignores the
//! polite close is signalled, and a process that ignores `SIGTERM` is killed
//! once [`GRACE`] is up.

use std::collections::HashMap;
use std::time::Duration;

/// How long an application gets between being asked to end and being ended.
///
/// Steam, measured: everything from `steam.sh` down was gone about three
/// seconds after `SIGTERM`, having spent the first two writing its library
/// state out. The grace sits past that, because the point of asking first is
/// to let an application finish exactly that kind of work — and nothing waits
/// on this, the window itself unmaps within a second.
pub const GRACE: Duration = Duration::from_secs(4);

/// How far up a process tree to walk before concluding it is not a tree.
const MAX_DEPTH: usize = 32;

/// One process, as `/proc` describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Process {
    pub pid: i32,
    pub parent: i32,
    pub group: i32,
    /// The executable's name — `/proc`'s `comm`, which the kernel truncates to
    /// 15 bytes, so every name matched below is short enough to survive it.
    pub name: String,
    /// Boot ticks at which it started. The only way to tell a process from the
    /// unrelated one that inherited its pid while we were waiting out the
    /// grace.
    pub started: u64,
}

/// A process to end, and the identity it must still have when the grace is up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Doomed {
    pub pid: i32,
    pub started: u64,
}

/// Everything running, at one instant.
///
/// Read once and then consulted, because a process tree read a piece at a time
/// is not a tree: the children of a process that exits mid-read are reparented
/// to init, and the walk that was going to find them loses them.
#[derive(Debug, Default)]
pub struct Processes {
    by_pid: HashMap<i32, Process>,
}

impl Processes {
    /// Everything `/proc` will admit to. Processes that exit while it is being
    /// read are simply absent, which is the same as having exited.
    pub fn read() -> Self {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            tracing::warn!("cannot read /proc; Close can only reach the window's own process");
            return Self::default();
        };
        let by_pid = entries
            .flatten()
            .filter_map(|entry| entry.file_name().to_str()?.parse::<i32>().ok())
            .filter_map(read_process)
            .map(|process| (process.pid, process))
            .collect();
        Self { by_pid }
    }

    #[cfg(test)]
    pub fn from_list(processes: impl IntoIterator<Item = Process>) -> Self {
        Self {
            by_pid: processes
                .into_iter()
                .map(|process| (process.pid, process))
                .collect(),
        }
    }

    pub fn get(&self, pid: i32) -> Option<&Process> {
        self.by_pid.get(&pid)
    }

    /// Every process that has to go for the application rooted at `root` to be
    /// gone: the root, everything descended from it, and — only when the root
    /// leads a process group of its own — the rest of that group.
    ///
    /// The group is a safety net for a child that double-forked out of the
    /// tree but kept the group it was born into. It is taken only from a root
    /// that leads its own group, because a root that does not is sharing
    /// somebody else's: a game whose process group is still Steam's would
    /// otherwise take Steam with it, and an application started without
    /// `setsid` would take the shell.
    pub fn application(&self, root: i32) -> Vec<Doomed> {
        let mut doomed: Vec<i32> = vec![root];
        let mut next = 0;
        while next < doomed.len() {
            let parent = doomed[next];
            next += 1;
            for process in self.by_pid.values() {
                if process.parent == parent && !doomed.contains(&process.pid) {
                    doomed.push(process.pid);
                }
            }
        }

        if self.get(root).is_some_and(|process| process.group == root) {
            for process in self.by_pid.values() {
                if process.group == root && !doomed.contains(&process.pid) {
                    doomed.push(process.pid);
                }
            }
        }

        doomed
            .into_iter()
            .filter_map(|pid| self.get(pid))
            .map(|process| Doomed {
                pid: process.pid,
                started: process.started,
            })
            .collect()
    }
}

/// Where the walk up the process tree has to stop, because past it lies the
/// session rather than the application.
#[derive(Debug, Clone, Copy, Default)]
pub struct Boundary {
    /// The shell that starts applications. Its children are the launches, so
    /// a launch is the last process before it.
    pub shell: Option<i32>,
    /// Us. Reached by anything the compositor started itself.
    pub compositor: Option<i32>,
}

impl Boundary {
    fn stops_at(&self, pid: i32) -> bool {
        pid <= 1 || self.shell == Some(pid) || self.compositor == Some(pid)
    }

    /// Whether this is the session itself rather than something inside it.
    fn is_the_session(&self, pid: i32) -> bool {
        self.shell == Some(pid) || self.compositor == Some(pid)
    }
}

/// Whether `pid` belongs to this session at all: a walk up from it reaches the
/// shell or the compositor before it reaches init.
///
/// The question every signal has to ask first, and the one that was missing.
/// [`application_root`] answers "how far up does this application go", and it
/// answers it by walking until the parent is the session — which is the right
/// answer for an application the session started and a catastrophic one for
/// anything else. A client that connected to the socket from outside has no
/// ancestor in this session at all, so the walk runs out of tree instead, stops
/// at the process below init, and hands back a root that is somebody else's
/// whole login: `systemd --user`, and every desktop, terminal and browser
/// under it. Signalling that is signalling the machine.
///
/// Measured, on this machine, by stopping it: a nested session was given a
/// window belonging to a client started from a terminal, decided nothing of it
/// was on screen, walked up out of its own session and `SIGSTOP`ped the user's
/// entire Plasma desktop.
///
/// So: not inside the session, not ours to signal. The cost of being wrong in
/// that direction is a Close that falls back to asking politely, or an
/// application that goes on running behind the start screen. The cost of being
/// wrong the other way is the one above.
pub fn inside_the_session(processes: &Processes, pid: i32, boundary: Boundary) -> bool {
    let mut at = pid;
    for _ in 0..MAX_DEPTH {
        if boundary.is_the_session(at) {
            return true;
        }
        let Some(process) = processes.get(at) else {
            return false;
        };
        if process.parent <= 1 {
            return false;
        }
        at = process.parent;
    }
    false
}

/// An application that runs other applications for the user, and rebuilds its
/// own interface when part of it dies.
///
/// A supervisor is where one application ends and the next begins. Which side
/// of that line a window is on cannot be read off the tree — the Steam client
/// is an ancestor of both its own interface and every game it launches — so it
/// is read off the window's own process instead.
struct Supervisor {
    /// What the supervisor's own processes are called.
    processes: &'static [&'static str],
    /// Which processes' windows are the supervisor's *interface*, as against
    /// the applications it runs. A window from one of these means the user is
    /// closing the supervisor itself.
    interface: &'static [&'static str],
}

/// Measured on this machine: the Steam client is `steam`, its interface is
/// drawn by `steamwebhelper`, and a game arrives under `reaper` with `steam`
/// as its grandparent. Closing the interface has to reach past `steam` to the
/// launch; closing a game must stop below it, or quitting a game would quit
/// Steam.
const SUPERVISORS: &[Supervisor] = &[Supervisor {
    processes: &["steam"],
    interface: &["steamwebhelper", "steam"],
}];

/// The topmost process that is still this window's application.
///
/// Returns `None` only when the window's process is already gone.
pub fn application_root(processes: &Processes, pid: i32, boundary: Boundary) -> Option<i32> {
    let window = processes.get(pid)?;
    let mut root = window.pid;

    for _ in 0..MAX_DEPTH {
        let Some(parent) = processes.get(processes.get(root)?.parent) else {
            break;
        };
        if boundary.stops_at(parent.pid) {
            break;
        }
        let supervisor = SUPERVISORS
            .iter()
            .find(|supervisor| supervisor.processes.contains(&parent.name.as_str()));
        if let Some(supervisor) = supervisor {
            if !supervisor.interface.contains(&window.name.as_str()) {
                // Somebody else's application, started by this supervisor.
                break;
            }
        }
        root = parent.pid;
    }

    Some(root)
}

/// Whether this window belongs to something a supervisor is running *for* the
/// user, rather than to the supervisor itself.
///
/// The same tree [`application_root`] walks, asked the plainer question: is a
/// game being played here. What makes the answer possible at all is that a
/// supervisor is where one application ends and the next begins — so a window
/// with a supervisor above it and a name that is not the supervisor's own
/// interface is something that supervisor started, which for Valve's client
/// means a game.
///
/// Asked of the process rather than of the `app_id`, because the `app_id` is
/// exactly what cannot be relied on: a game reaches the screen as
/// `steam_app_…` under Proton, as its own name when it speaks Wayland for
/// itself, and as nothing at all often enough. Which client is running it is
/// the one thing every one of them has in common.
///
/// False for anything this session did not start — see [`inside_the_session`],
/// which is asked here for the reason it is asked before a signal: a client
/// that connected to the socket from outside has no ancestor here, and a walk
/// up from it runs out of tree into somebody else's login.
pub fn supervised_application(processes: &Processes, pid: i32, boundary: Boundary) -> bool {
    let Some(window) = processes.get(pid) else {
        return false;
    };
    if !inside_the_session(processes, pid, boundary) {
        return false;
    }
    let mut at = window.parent;
    for _ in 0..MAX_DEPTH {
        if boundary.stops_at(at) {
            // The walk reached the session without passing a supervisor, so
            // whatever this is, the shell started it directly.
            return false;
        }
        let Some(process) = processes.get(at) else {
            return false;
        };
        let supervisor = SUPERVISORS
            .iter()
            .find(|supervisor| supervisor.processes.contains(&process.name.as_str()));
        if let Some(supervisor) = supervisor {
            // The first supervisor above the window decides. Its own interface
            // is the client, and everything else under it is somebody playing.
            return !supervisor.interface.contains(&window.name.as_str());
        }
        at = process.parent;
    }
    false
}

/// What Close does to one window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ending {
    /// Ask the window's client to close it and touch no process at all, then
    /// run this command if the window is still there when the grace is up.
    Ask(&'static str),
    /// Run this command: it, rather than any signal, is what owns this
    /// application's lifetime.
    Run(&'static str),
    /// End these processes — `SIGTERM` now, `SIGKILL` for whatever is left
    /// when the grace is up.
    Signal(Vec<Doomed>),
    /// Nothing here can reach it: a client with no process of its own, or one
    /// forwarded from another machine. The caller falls back to asking.
    Unreachable,
}

/// The applications whose lifetime is not their window's process, keyed by the
/// name the application gives itself.
fn recipe(app_id: &str) -> Option<Ending> {
    // The whole Android session, in one window. It ignores the polite close,
    // and its processes belong to root inside the container, so the only way
    // out is the command that owns the session.
    if app_id == "Waydroid" {
        return Some(Ending::Run(WAYDROID_STOP));
    }
    // One Android app, which Waydroid names `waydroid.<package>`. Android
    // finishes the task on a polite close and does not bring it back — and a
    // signal would hit the HAL service shared by every Waydroid window, which
    // Android's init would restart underneath us. Stopping the session is the
    // fallback for a window that stays regardless.
    if app_id.starts_with("waydroid.") {
        return Some(Ending::Ask(WAYDROID_STOP));
    }
    None
}

const WAYDROID_STOP: &str = "waydroid session stop";

/// How to end the application this window belongs to.
pub fn ending(app_id: &str, pid: Option<i32>, processes: &Processes, boundary: Boundary) -> Ending {
    if let Some(recipe) = recipe(app_id) {
        return recipe;
    }
    let Some(pid) = pid.filter(|pid| *pid > 1) else {
        return Ending::Unreachable;
    };
    // And the same containment Close needs and never had: the walk below finds
    // the topmost ancestor that is not this session, which for a process the
    // session never started is a stranger's login. Unreachable rather than
    // fatal — the caller falls back to asking the window to close itself,
    // which is the right answer for somebody else's client anyway. See
    // [`inside_the_session`].
    if !inside_the_session(processes, pid, boundary) {
        tracing::debug!(pid, "this process is not in this session; only asking");
        return Ending::Unreachable;
    }
    let Some(root) = application_root(processes, pid, boundary) else {
        return Ending::Unreachable;
    };
    if boundary.stops_at(root) {
        // The walk landed on the session itself. Nothing up there is an
        // application, and all of it is load-bearing.
        tracing::warn!(pid, root, "refusing to end the session itself");
        return Ending::Unreachable;
    }
    match processes.application(root) {
        doomed if doomed.is_empty() => Ending::Unreachable,
        doomed => Ending::Signal(doomed),
    }
}

/// The processes to stop while nobody can see this application, or `None` for
/// one that has to go on running whether it can be seen or not.
///
/// The same walk Close makes, asked for a different reason and answered more
/// cautiously, because the two mistakes are not alike. Ending the wrong thing
/// takes an application away from somebody; *stopping* the wrong thing leaves
/// a machine that looks broken — a session with no shell, a client that will
/// never start the game it was asked for — and says nothing in the log about
/// why.
///
/// Two applications are refused outright:
///
/// * **A supervisor's own interface.** Valve's client is the one that matters:
///   it starts games, downloads them, and answers the shell over its own
///   socket. Stopped behind the start screen it would do none of that, and the
///   next press of a game would hand its request to a process that is not
///   listening. [`SUPERVISORS`] already says which windows are the client
///   itself rather than something it is running, for exactly this distinction.
/// * **Anything with a recipe of its own.** Waydroid's processes belong to
///   root inside a container; a stop that cannot be undone from here is not one
///   to send.
pub fn sleeping(
    app_id: &str,
    pid: Option<i32>,
    processes: &Processes,
    boundary: Boundary,
) -> Option<Vec<Doomed>> {
    if recipe(app_id).is_some() {
        return None;
    }
    let pid = pid.filter(|pid| *pid > 1)?;
    let window = processes.get(pid)?;
    if SUPERVISORS
        .iter()
        .any(|supervisor| supervisor.interface.contains(&window.name.as_str()))
    {
        return None;
    }
    // Not ours, not stopped. See [`inside_the_session`], which is the whole of
    // why this is asked before anything else about the tree.
    if !inside_the_session(processes, pid, boundary) {
        tracing::debug!(pid, "not stopping a process this session did not start");
        return None;
    }
    let root = application_root(processes, pid, boundary)?;
    if boundary.stops_at(root) {
        tracing::warn!(pid, root, "refusing to stop the session itself");
        return None;
    }
    match processes.application(root) {
        doomed if doomed.is_empty() => None,
        doomed => Some(doomed),
    }
}

/// Send one signal to every process that is still the one we meant.
///
/// A pid on its own is not an identity: between reading the tree and
/// signalling it, a process can exit and its number be handed to something
/// else. `started` is checked against `/proc` again here so that the recycled
/// pid is skipped rather than killed.
pub fn signal(doomed: &[Doomed], signal: i32) -> usize {
    let mut sent = 0;
    for target in doomed {
        let Some(current) = read_process(target.pid) else {
            continue;
        };
        if current.started != target.started {
            tracing::debug!(pid = target.pid, "pid was recycled; leaving it alone");
            continue;
        }
        // SAFETY: a positive pid that /proc has just confirmed exists. A
        // process that exits between the two is ESRCH, which is ignored.
        if unsafe { libc::kill(target.pid, signal) } == 0 {
            sent += 1;
        } else {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::ESRCH) {
                tracing::warn!(?err, pid = target.pid, signal, "could not signal");
            }
        }
    }
    sent
}

/// Read one process's line of `/proc`, or `None` if it has gone.
fn read_process(pid: i32) -> Option<Process> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_stat(&stat)
}

/// Pull the fields out of a `/proc/<pid>/stat` line.
///
/// The executable name is the awkward part: it sits in parentheses, is not
/// escaped, and may itself contain spaces and brackets, so everything after it
/// is found from the *last* `)` rather than by splitting the line.
fn parse_stat(stat: &str) -> Option<Process> {
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    let pid = stat[..open].trim().parse().ok()?;
    let name = stat[open + 1..close].to_string();
    let fields: Vec<&str> = stat.get(close + 1..)?.split_whitespace().collect();
    // Field 3 (state) is the first one here, so stat's field N is fields[N - 3].
    Some(Process {
        pid,
        parent: fields.get(1)?.parse().ok()?,
        group: fields.get(2)?.parse().ok()?,
        name,
        started: fields.get(19)?.parse().ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(pid: i32, parent: i32, group: i32, name: &str) -> Process {
        Process {
            pid,
            parent,
            group,
            name: name.to_string(),
            started: pid as u64 * 10,
        }
    }

    /// Steam as it actually was on this machine, read out of `ps` with the
    /// shell in the place the session shell occupies at runtime, plus a game
    /// under the client. Every pid and every process group below is measured.
    fn steam() -> Processes {
        Processes::from_list([
            process(100, 1, 100, "lxb-desktop"),
            // The launch: `sh -c steam`, which becomes steam.sh. setsid made
            // it a group leader, and `steam` stayed in its group.
            process(147779, 100, 147779, "bash"),
            process(147893, 147779, 147779, "steam"),
            process(148257, 147893, 147779, "steam-runtime-l"),
            // pressure-vessel puts the interface in a session and a group of
            // its own — which is exactly why the window's own group is no use.
            process(147937, 147893, 147937, "srt-bwrap"),
            process(148121, 147937, 148121, "pv-adverb"),
            process(148158, 148121, 148121, "steamwebhelper"),
            process(148166, 148158, 148121, "steamwebhelper"),
            process(148161, 148121, 148160, "steamwebhelper"),
            // A game, launched from inside Steam rather than by the shell.
            process(200000, 147893, 200000, "reaper"),
            process(200001, 200000, 200000, "KSP.x86_64"),
        ])
    }

    fn boundary() -> Boundary {
        Boundary {
            shell: Some(100),
            compositor: Some(50),
        }
    }

    /// The bug this module exists for: the window's process is a helper the
    /// client rebuilds, so closing it has to reach the launch.
    #[test]
    fn closing_steam_reaches_the_process_that_would_restart_it() {
        let root = application_root(&steam(), 148158, boundary()).unwrap();
        assert_eq!(root, 147779, "the launch, not the webhelper's group leader");

        let doomed: Vec<i32> = steam().application(root).iter().map(|d| d.pid).collect();
        for pid in [
            147779, 147893, 148257, 147937, 148121, 148158, 148166, 148161,
        ] {
            assert!(doomed.contains(&pid), "{pid} would have been left running");
        }
    }

    /// A game under the client is somebody playing; the client's own windows
    /// are not, however far up the same tree they sit.
    #[test]
    fn a_game_under_the_client_is_the_only_thing_that_is_being_played() {
        let processes = steam();
        assert!(
            supervised_application(&processes, 200001, boundary()),
            "the game"
        );
        for interface in [148158, 148166, 148161] {
            assert!(
                !supervised_application(&processes, interface, boundary()),
                "the client's own interface, at {interface}"
            );
        }
        assert!(
            !supervised_application(&processes, 147893, boundary()),
            "the client itself"
        );
    }

    /// And nothing else is. An application the shell started walks up to the
    /// shell without passing a supervisor, and a stranger's process is not this
    /// session's to have an opinion about.
    #[test]
    fn nothing_the_shell_started_itself_is_a_game() {
        let processes = Processes::from_list([
            process(100, 1, 100, "lxb-desktop"),
            process(900, 100, 900, "bash"),
            process(901, 900, 900, "firefox"),
            // Somebody else's login entirely.
            process(902, 1, 902, "sshd"),
            process(903, 902, 902, "htop"),
        ]);
        assert!(!supervised_application(&processes, 901, boundary()));
        assert!(!supervised_application(&processes, 903, boundary()));
        assert!(!supervised_application(&processes, 999_999, boundary()));
    }

    /// And the thing that must not follow from it: a game is its own
    /// application, even though the client that started it is its ancestor.
    #[test]
    fn closing_a_game_leaves_the_client_that_launched_it_alone() {
        let root = application_root(&steam(), 200001, boundary()).unwrap();
        assert_eq!(root, 200000, "the game's own tree, stopping below steam");

        let doomed: Vec<i32> = steam().application(root).iter().map(|d| d.pid).collect();
        assert_eq!(doomed, vec![200000, 200001]);
        for surviving in [147779, 147893, 148158] {
            assert!(!doomed.contains(&surviving), "{surviving} would have died");
        }
    }

    /// The bug this cost a desktop to find: a window whose process this session
    /// never started is not this session's to signal.
    ///
    /// What happened, on this machine. A nested compositor was handed a client
    /// started from a terminal, decided nothing of it was on screen, and walked
    /// up the tree looking for the top of that application. There was no top —
    /// nothing in that chain belonged to the nested session — so the walk ran
    /// until the process below init, took *that* as the application, and
    /// stopped it and everything under it: the user's whole Plasma desktop, in
    /// one signal, with no way left to send the one that undoes it.
    #[test]
    fn a_process_this_session_never_started_is_never_signalled() {
        let processes = Processes::from_list([
            // The session: a shell and a compositor, as the boundary names them.
            process(100, 1, 100, "lxb-desktop"),
            process(50, 1, 50, "lxb"),
            // Somebody else's login, with a window of its own that happens to
            // have connected to this compositor's socket.
            process(900, 1, 900, "systemd"),
            process(901, 900, 901, "plasmashell"),
            process(902, 901, 902, "konsole"),
            process(903, 902, 903, "stray-client"),
        ]);

        assert!(
            !inside_the_session(&processes, 903, boundary()),
            "a client from another login is not in this session"
        );
        assert!(
            sleeping("stray", Some(903), &processes, boundary()).is_none(),
            "it would have stopped the other login"
        );
        assert_eq!(
            ending("stray", Some(903), &processes, boundary()),
            Ending::Unreachable,
            "and Close would have killed it"
        );

        // Everything the session did start is still reachable, by both.
        for pid in [100, 50] {
            assert!(inside_the_session(&processes, pid, boundary()));
        }
    }

    /// The three shapes that are inside the session: a launch of the shell's,
    /// something the compositor started, and a game several processes below
    /// the client that started it.
    #[test]
    fn everything_the_session_started_is_inside_it() {
        assert!(inside_the_session(&steam(), 200001, boundary()), "a game");
        assert!(
            inside_the_session(&steam(), 148158, boundary()),
            "the client's own interface"
        );

        let processes = Processes::from_list([
            process(100, 1, 100, "lxb-desktop"),
            process(50, 1, 50, "lxb"),
            process(400, 50, 400, "autostarted"),
        ]);
        assert!(
            inside_the_session(&processes, 400, boundary()),
            "the compositor's own child"
        );

        // And the one that is not: a process reparented to init has no chain
        // left to prove it by, so it is left alone rather than guessed at.
        let orphaned = Processes::from_list([
            process(100, 1, 100, "lxb-desktop"),
            process(500, 1, 500, "reparented"),
        ]);
        assert!(!inside_the_session(&orphaned, 500, boundary()));
    }

    /// A game behind the start screen is stopped at its own tree, exactly as
    /// Close would end it there.
    #[test]
    fn a_game_nobody_can_see_is_stopped_below_the_client() {
        let stopped = sleeping("steam_app_3812600", Some(200001), &steam(), boundary())
            .expect("a game is an application like any other");
        let stopped: Vec<i32> = stopped.iter().map(|d| d.pid).collect();
        assert_eq!(stopped, vec![200000, 200001]);
    }

    /// And the client that starts games is not, whichever of its windows the
    /// question is asked about. A stopped Steam is a session where the next
    /// game the user presses never starts, with nothing on screen to say so.
    #[test]
    fn the_client_that_starts_games_is_never_stopped() {
        for window in [148158, 147893] {
            assert!(
                sleeping("steam", Some(window), &steam(), boundary()).is_none(),
                "the client would have been stopped through pid {window}"
            );
        }
    }

    /// Nor is anything whose lifetime is not a process this session owns.
    #[test]
    fn an_application_with_a_recipe_of_its_own_is_left_running() {
        assert!(sleeping("Waydroid", Some(200001), &steam(), boundary()).is_none());
        assert!(sleeping("waydroid.com.example", Some(200001), &steam(), boundary()).is_none());
    }

    /// A window with no process behind it, or one whose process is already
    /// gone, is nothing to stop rather than something to guess at.
    #[test]
    fn a_window_with_no_process_is_not_stopped() {
        assert!(sleeping("x", None, &steam(), boundary()).is_none());
        assert!(sleeping("x", Some(1), &steam(), boundary()).is_none());
        assert!(sleeping("x", Some(999_999), &steam(), boundary()).is_none());
    }

    /// A process group is only ever taken from a root that leads one. A game
    /// Steam left in the client's group must not drag the client in with it.
    #[test]
    fn a_shared_process_group_is_not_the_application() {
        let processes = Processes::from_list([
            process(100, 1, 100, "lxb-desktop"),
            process(147779, 100, 147779, "bash"),
            process(147893, 147779, 147779, "steam"),
            // Same group as the client, unlike the measured reaper.
            process(200000, 147893, 147779, "reaper"),
            process(200001, 200000, 147779, "game"),
        ]);
        let doomed: Vec<i32> = processes
            .application(200000)
            .iter()
            .map(|d| d.pid)
            .collect();
        assert_eq!(doomed, vec![200000, 200001]);
    }

    /// An ordinary application is its launch, whatever it forked on the way.
    #[test]
    fn an_ordinary_application_is_ended_from_its_launch() {
        let processes = Processes::from_list([
            process(100, 1, 100, "lxb-desktop"),
            process(300, 100, 300, "celeste.sh"),
            process(301, 300, 300, "Celeste"),
            process(302, 301, 300, "Celeste"),
        ]);
        let root = application_root(&processes, 302, boundary()).unwrap();
        assert_eq!(root, 300);
        let doomed: Vec<i32> = processes.application(root).iter().map(|d| d.pid).collect();
        assert_eq!(doomed, vec![300, 301, 302]);
    }

    /// The walk stops at the session however it is reached — the shell that
    /// started the launch, the compositor, or init for anything that
    /// double-forked away from both.
    #[test]
    fn the_walk_never_leaves_the_application() {
        let processes = Processes::from_list([
            process(100, 1, 100, "lxb-desktop"),
            process(50, 1, 50, "lxb"),
            process(300, 100, 300, "app"),
            process(400, 50, 400, "autostarted"),
            process(500, 1, 500, "reparented"),
        ]);
        for pid in [300, 400, 500] {
            let root = application_root(&processes, pid, boundary()).unwrap();
            assert_eq!(root, pid, "walked past the session");
        }
    }

    /// Nothing at all is ended for the shell itself, or for the compositor.
    #[test]
    fn the_session_is_never_the_application() {
        let processes = Processes::from_list([
            process(100, 1, 100, "lxb-desktop"),
            process(50, 1, 50, "lxb"),
        ]);
        assert_eq!(
            ending("lxb-desktop", Some(100), &processes, boundary()),
            Ending::Unreachable
        );
        assert_eq!(
            ending("", Some(1), &processes, boundary()),
            Ending::Unreachable
        );
        assert_eq!(
            ending("", None, &processes, boundary()),
            Ending::Unreachable
        );
    }

    /// Waydroid is not ended by signals at all: the full Android session has
    /// its own off switch, and a single Android app closes politely.
    #[test]
    fn waydroid_is_ended_the_way_waydroid_ends() {
        let processes = Processes::from_list([process(161628, 152275, 161628, "composer@2.1-s")]);
        assert_eq!(
            ending("Waydroid", Some(161628), &processes, boundary()),
            Ending::Run(WAYDROID_STOP)
        );
        assert_eq!(
            ending(
                "waydroid.com.android.calculator2",
                Some(161628),
                &processes,
                boundary()
            ),
            Ending::Ask(WAYDROID_STOP)
        );
    }

    /// A name that merely starts like Waydroid's is not Waydroid.
    #[test]
    fn only_waydroids_own_windows_take_waydroids_recipe() {
        let processes = Processes::from_list([
            process(100, 1, 100, "lxb-desktop"),
            process(300, 100, 300, "waydroid-helper"),
        ]);
        assert!(matches!(
            ending("waydroidctl", Some(300), &processes, boundary()),
            Ending::Signal(_)
        ));
    }

    /// The executable name is not a word: it can contain spaces and the
    /// bracket the parser looks for.
    #[test]
    fn stat_is_parsed_around_an_awkward_executable_name() {
        // state, ppid, pgrp, session, then the twelve fields between the
        // session and the start time, which nothing here reads.
        let line = "148158 (steam (bad) name) S 148121 148121 148121 \
                    0 -1 4194304 1 2 3 4 5 6 7 8 9 10 11 12 \
                    4242 rest";
        let process = parse_stat(line).unwrap();
        assert_eq!(process.pid, 148158);
        assert_eq!(process.name, "steam (bad) name");
        assert_eq!(process.parent, 148121);
        assert_eq!(process.group, 148121);
        assert_eq!(process.started, 4242);
    }

    /// The real thing: this process is in there, and it is its own parent's
    /// child.
    #[test]
    fn the_running_process_tree_can_be_read() {
        let processes = Processes::read();
        let me = std::process::id() as i32;
        let mine = processes.get(me).expect("this process is not in /proc");
        assert_eq!(mine.pid, me);
        assert!(mine.parent > 0);
        assert!(processes.get(mine.parent).is_some());
    }

    /// And the field the recycled-pid check hangs on is the one it thinks it
    /// is. Read off by index from a line of counters, a wrong `started` is
    /// silent: it compares equal to itself for as long as nothing recycles a
    /// pid, and then kills a stranger. This test process was started moments
    /// ago, so its start time has to sit just below the uptime it is measured
    /// against.
    #[test]
    fn the_start_time_is_the_start_time() {
        let mine = read_process(std::process::id() as i32).unwrap();
        // SAFETY: sysconf with a constant name, and the value is only read.
        let ticks_per_second = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as u64;
        // Whole seconds would be up to a second short of the truth, and this
        // process started within the last one of them.
        let uptime = std::fs::read_to_string("/proc/uptime").unwrap();
        let seconds: f64 = uptime.split_whitespace().next().unwrap().parse().unwrap();
        let uptime_ticks = (seconds * ticks_per_second as f64) as u64;

        assert!(mine.started > 0, "no start time at all");
        // A tick of slack in each direction: both numbers are quantised, and
        // this process started a moment *before* the uptime it is compared
        // with was read. The check is for a field index that is wrong by
        // orders of magnitude, not for a clock that is wrong by a hundredth of
        // a second.
        assert!(
            mine.started <= uptime_ticks + ticks_per_second,
            "started {} ticks into a {uptime_ticks}-tick uptime",
            mine.started
        );
        assert!(
            mine.started + 300 * ticks_per_second > uptime_ticks,
            "this test did not start five minutes ago; {} is not a start time",
            mine.started
        );
    }
}
