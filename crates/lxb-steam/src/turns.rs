//! One turn at a time, at the things on this machine there is one of.
//!
//! ## Why a file and not a `Mutex`
//!
//! Everything guarded here is *the user's*, not this process's. There is one
//! Valve client per user, one install wizard inside it, and one
//! `loginusers.vdf` naming every account on the machine — and a `static Mutex`
//! serialises the threads of one program against each other and nothing else.
//!
//! That would be an academic distinction if one shell were the only thing that
//! ever ran, and it is not. This crate ships five `examples/probe-*` programs
//! that each call [`crate::Steam::start`], and running one against a live
//! session is how the Steam integration is verified here; a session can be
//! started a second time beside the one already up; and the shell itself builds
//! a second worker before it drops the first when the integration is switched
//! back on. Each of those is another process with its own copy of every
//! `static` in this crate, queueing behind nothing.
//!
//! What that costs is not theoretical either. Two of them can stop and start
//! the same client at once, hand it two different credentials, drive and cancel
//! each other's install wizard, and read-modify-write the same account list.
//! And one of them, as it starts, can take away the debugging marker another is
//! at that moment signing a client in behind — see
//! [`crate::webui::withdraw_what_was_left_behind`], which is the one that was
//! found first and is the reason this module exists.
//!
//! ## What a turn is
//!
//! `flock` on a file per resource, under the state directory. Two things make
//! that the right primitive rather than a lock file whose existence is the
//! lock: the kernel drops it when the descriptor closes, **including when the
//! process dies**, so there is no such thing as a stale one somebody has to
//! decide to break; and it does not care whether the file was already there, so
//! nothing has to tell a lock from litter.
//!
//! The process-local `Mutex` is **kept as well**, and taken first. A machine
//! whose state directory cannot be written — a read-only home, no `$HOME` at
//! all — gets no file lock, and must not thereby lose the serialisation it
//! already had. Taking both, always in that order, makes the degraded case
//! exactly as good as the code this replaced and no worse.
//!
//! ## The order they are taken in
//!
//! Two turns are held at once in exactly one place, and it is always the same
//! way round: [`crate::client::wake`] holds [`What::TheClient`] for the whole of
//! a wake, and a wake that has to put Valve's own Offline Mode on or take it off
//! reaches [`What::TheAccountList`] from inside it. Nothing goes the other way.
//! Both writers of the account list — [`crate::client::offline`] and
//! [`crate::client::autologin::stop`], which is the sign-out — read and write a
//! file and start no client, and the sign-out is not inside a wake to begin
//! with. Nothing holds [`What::TheWizard`] and either of the others together
//! either, because a flow finishes its wake before it queues for the wizard.
//! That is what makes three locks safe to have rather than one, and it is worth
//! checking against if a fourth is ever added.
//!
//! ## The lease
//!
//! Whoever holds a turn writes down who they are in the file they are holding:
//! the pid and that pid's start time, the session it draws into, which Steam it
//! is driving, and the request number it was taken under. Almost nothing reads
//! it to decide anything — the lock decides — so a lease that is missing,
//! half-written or left by a version that wrote none costs nothing but a vaguer
//! line in the log.
//!
//! The exception is [`Lease::alive`], and it is worth being exact about what it
//! adds, because it is less than it first appears. The lock is what decides
//! whether anybody is inside a wake, and it decides it correctly for every
//! session that could take one — including this process's own other threads,
//! which nothing else knows about. The lease is a second and weaker guard for
//! the case where the lock silently does not exclude at all: a state directory
//! on a filesystem whose `flock` is a no-op. It costs a `/proc` read and it
//! cannot wrongly remove anything, so it is worth having; it is not what makes
//! this correct.

use std::io::{Read, Seek, Write};
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// The things there is exactly one of per user, and which this shell drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum What {
    /// Valve's client as a whole: stopping it, starting it, making and taking
    /// back its debugging marker, and handing it a credential. See
    /// [`crate::client::wake`], which holds this for the whole of one.
    TheClient,
    /// The install wizard inside it. Valve's client has exactly one, and
    /// `OpenInstallWizard`, `OpenUninstallWizard` and `CancelInstall` are calls
    /// about *the* wizard rather than about a game. See
    /// [`crate::webui::ready_to_drive_the_wizard`].
    TheWizard,
    /// `config/loginusers.vdf`, which is every account on the machine and is
    /// written by reading all of it and putting all of it back. See
    /// [`crate::client::offline`].
    ///
    /// This one is only ever half a guard, and honestly so: Valve's client
    /// writes the same file and has never heard of this lock. What it makes
    /// impossible is two of *these* losing each other's edit, which is the
    /// whole of what this shell can be responsible for.
    TheAccountList,
}

impl What {
    /// The lock's own file, under the state directory.
    fn file(self) -> &'static str {
        match self {
            What::TheClient => "steam-the-client.lock",
            What::TheWizard => "steam-the-wizard.lock",
            What::TheAccountList => "steam-the-account-list.lock",
        }
    }

    /// What it is called in a line somebody reads.
    fn said(self) -> &'static str {
        match self {
            What::TheClient => "Valve's client",
            What::TheWizard => "Valve's install wizard",
            What::TheAccountList => "Steam's list of accounts",
        }
    }

    /// The process-local half. Taken first, and for the reason the module note
    /// gives: it is the whole of the serialisation on a machine where the file
    /// lock cannot be made.
    fn locally(self) -> &'static Mutex<()> {
        static THE_CLIENT: Mutex<()> = Mutex::new(());
        static THE_WIZARD: Mutex<()> = Mutex::new(());
        static THE_ACCOUNT_LIST: Mutex<()> = Mutex::new(());
        match self {
            What::TheClient => &THE_CLIENT,
            What::TheWizard => &THE_WIZARD,
            What::TheAccountList => &THE_ACCOUNT_LIST,
        }
    }
}

/// What a turn is being taken for, for whoever reads the lease afterwards.
///
/// Every field is diagnostic. Nothing decides anything on them, which is why
/// they are all optional and why not knowing one is never a failure.
#[derive(Debug, Clone, Default)]
pub struct Behalf {
    /// Which Steam this session drives — the root, because that is what tells
    /// the native client from the Flatpak. See [`crate::backend::Backend`].
    pub backend: Option<PathBuf>,
    /// The request number it was taken under, as the shell numbered it. The
    /// same number the audit log stamps its lines with, so a lock and a line in
    /// `steam-actions.log` can be put beside each other.
    pub request: Option<u64>,
}

impl Behalf {
    /// For a caller with neither to give: the tidying at startup, and tests.
    pub fn nothing() -> Behalf {
        Behalf::default()
    }
}

/// A turn, held for as long as this value is.
///
/// `!Send`, because the process-local guard inside it is — which is what the
/// code this replaced was already, so nothing that used to compile stops.
pub struct Turn {
    /// The machine-wide half. Declared first so it is released first: another
    /// process getting in while this one still holds its own local mutex is
    /// harmless, and the other order would have a thread of ours take the local
    /// mutex and immediately block on a file lock we had not let go of yet.
    ///
    /// `None` where the state directory could not be written. See the module
    /// note: that machine is no worse off than it was, and saying so once in
    /// the log is the whole of what can be done about it.
    held: Option<std::fs::File>,
    _locally: std::sync::MutexGuard<'static, ()>,
}

/// Take a turn, waiting for it however long it takes.
///
/// Blocking, and a wait can be as long as whatever the other session is doing:
/// a cold client is a minute and a half, an install wizard rather more. That is
/// the point of it. What it must never be is *invisible*, so a turn that has to
/// wait says whose it is waiting for before it blocks — see [`whose`].
pub fn take(what: What, on: Behalf) -> Turn {
    // A caller that panicked while holding it has already reported why; the
    // next one wants the turn, not a second failure about the lock.
    let locally = what
        .locally()
        .lock()
        .unwrap_or_else(|held| held.into_inner());
    let held = open(what).and_then(|file| {
        // The common case is uncontended, and says nothing. Only a turn that
        // actually has to queue is worth a line, which is what asking for it
        // without waiting first buys.
        if lock(&file, Wait::No) {
            return Some(file);
        }
        tracing::info!(
            what = what.said(),
            holder = %whose(what),
            "waiting for another LineXinBar session to finish with it"
        );
        lock(&file, Wait::Yes).then_some(file)
    });
    let turn = Turn {
        held,
        _locally: locally,
    };
    turn.sign(what, &on);
    turn
}

/// Take it only if it is free this instant.
///
/// `None` means somebody has it, and the caller wanted to know that rather than
/// to wait: the tidying at startup is the only such caller, and for it a held
/// lock is the answer — somebody is inside a wake, so nothing on the disk is
/// litter. See [`crate::webui::withdraw_what_was_left_behind`].
///
/// **Both halves are asked without waiting, and the local one matters most.**
/// It is tempting to wait for the process-local mutex on the grounds that
/// nothing holds it for long, and that is exactly backwards:
/// [`crate::client::wake`] holds it for the whole of a wake, on a worker
/// thread, and the caller of this is a *second* worker being built in the same
/// process before the first is dropped — which is the shell switching its Steam
/// integration back on. Waiting there would stop a session coming up for as
/// long as a cold client takes, over a file it was only going to tidy.
///
/// That case is also the one this answers best. A wake of this process's own is
/// as good a reason to leave the marker alone as another session's, and the
/// local mutex is the only thing that knows about it.
pub fn take_if_free(what: What, on: Behalf) -> Option<Turn> {
    let locally = match what.locally().try_lock() {
        Ok(locally) => locally,
        // A holder that panicked has already reported why, and what it was
        // holding is free. The same recovery every other lock in this crate
        // makes.
        Err(std::sync::TryLockError::Poisoned(held)) => held.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => return None,
    };
    let file = open(what)?;
    if !lock(&file, Wait::No) {
        return None;
    }
    let turn = Turn {
        held: Some(file),
        _locally: locally,
    };
    turn.sign(what, &on);
    Some(turn)
}

impl Turn {
    /// Write down who is holding this, over whatever the last holder wrote.
    ///
    /// Best effort throughout. The lock is held either way, and a lease that
    /// could not be written costs a log line its detail and nothing else.
    fn sign(&self, what: What, on: &Behalf) {
        let Some(mut file) = self.held.as_ref() else {
            return;
        };
        let Some(lease) = Lease::of_this_process(on) else {
            return;
        };
        let _ = file.set_len(0);
        let _ = file.seek(std::io::SeekFrom::Start(0));
        if file.write_all(lease.written().as_bytes()).is_err() {
            tracing::debug!(what = what.said(), "could not write down who holds it");
        }
    }
}

/// Whether to wait for the lock or answer at once.
enum Wait {
    Yes,
    No,
}

/// `flock`, which is the whole of the machine-wide half.
///
/// Not `fcntl` locking, deliberately. `fcntl` locks are owned by the *process*
/// and are dropped by closing any descriptor on the file — including one opened
/// and closed by an unrelated library in the same program — and they do not
/// exclude two threads of one process from each other at all. `flock` is owned
/// by the open file description, so a fresh handle per turn gives threads and
/// processes the same answer, and only the last handle closing releases it.
fn lock(file: &std::fs::File, wait: Wait) -> bool {
    let how = match wait {
        Wait::Yes => libc::LOCK_EX,
        Wait::No => libc::LOCK_EX | libc::LOCK_NB,
    };
    // SAFETY: a live descriptor this function borrows for the length of the
    // call, and one of the two flag combinations `flock` documents.
    loop {
        if unsafe { libc::flock(file.as_raw_fd(), how) } == 0 {
            return true;
        }
        let error = std::io::Error::last_os_error();
        // A blocking `flock` is interruptible: a signal arriving while this
        // waits answers `EINTR` and has said nothing about the lock.
        if error.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        return false;
    }
}

/// The lock's file, made if it is not there.
///
/// Opened for writing as well as reading because the holder writes its lease
/// into it, and `create(true)` rather than `create_new`: the file is not the
/// lock, the `flock` on it is, so finding one already there is the ordinary
/// case and not a race.
fn open(what: What) -> Option<std::fs::File> {
    let path = beside_the_state(what.file())?;
    if let Some(folder) = path.parent() {
        let _ = std::fs::create_dir_all(folder);
    }
    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
    {
        Ok(file) => Some(file),
        Err(error) => {
            // Once, at the level somebody looking for it will find. See the
            // module note: this is a degradation to what the code did before,
            // not a failure of anything the caller asked for.
            tracing::warn!(
                path = %path.display(),
                %error,
                "could not make the lock that keeps two LineXinBar sessions off Steam at once"
            );
            None
        }
    }
}

/// Whoever is holding a turn, in the words a log line wants.
///
/// Read without the lock, on purpose: this is only ever called *because* the
/// lock is held by somebody, and waiting for it to find out who has it would be
/// the wait this line is meant to explain. What that costs is that a lease
/// rewritten in the same instant reads as the holder before it, which is a name
/// in a line and nothing more.
fn whose(what: What) -> String {
    match beside_the_state(what.file())
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| Lease::read(&text))
    {
        Some(lease) => lease.said(),
        None => "another session".to_string(),
    }
}

/// Under `$XDG_STATE_HOME/linexinbar/`, which is where everything this shell
/// remembers about Steam between runs lives — the audit log, the note about the
/// debugging marker, and these locks.
///
/// Read every time rather than worked out once: `XDG_STATE_HOME` is what a test
/// moves to keep its scratch state out of the user's own, and an answer cached
/// at startup would send every one of them into the real directory.
pub(crate) fn beside_the_state(name: &str) -> Option<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            let home = std::env::var_os("HOME").map(PathBuf::from)?;
            Some(home.join(".local").join("state"))
        })?;
    Some(state.join("linexinbar").join(name))
}

/// Who holds something, and enough about them to say whether they are still
/// there.
///
/// A pid on its own is not an answer. Pids are reused, and the failure this has
/// to survive is precisely the one where the holder died — so the process that
/// wears its number next is as likely to be anything as it is to be a shell.
/// The pid's own start time is what makes the pair unique, and the machine's
/// boot time is what stops a lease written before a restart from matching a
/// process that happens to have come up at the same tick after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    /// The process holding it.
    pub pid: u32,
    /// When that process started, in clock ticks since boot, out of field 22 of
    /// `/proc/<pid>/stat`.
    pub started: u64,
    /// When the machine booted, in seconds, out of `btime` in `/proc/stat`.
    pub boot: u64,
    /// Which session it draws into — the same two variables that decide whether
    /// Valve's client is this session's. See [`crate::client::DRAWS_INTO`].
    pub session: String,
    /// The Steam root it is driving, where the holder knew one.
    pub backend: Option<String>,
    /// The request number it was taken under, where the holder had one.
    pub request: Option<u64>,
    /// When it was taken, in seconds since the epoch. For a person reading the
    /// file; nothing decides on it, because a clock that has been set backwards
    /// would then decide wrongly.
    pub at: u64,
}

impl Lease {
    /// This process, right now.
    pub fn of_this_process(on: &Behalf) -> Option<Lease> {
        let pid = std::process::id();
        Some(Lease {
            pid,
            started: started(pid)?,
            boot: boot()?,
            session: this_session(),
            backend: on
                .backend
                .as_ref()
                .map(|root| root.display().to_string())
                .map(|root| one_line(&root)),
            request: on.request,
            at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|since| since.as_secs())
                .unwrap_or_default(),
        })
    }

    /// Whether the process this names is still the process that took it.
    ///
    /// All three have to agree. A `/proc` entry that is not there is a process
    /// that has gone; one that is there with another start time is a pid that
    /// has been handed to somebody else; and either of those read against a
    /// different boot is a lease from before a restart, which names nothing at
    /// all.
    ///
    /// It answers about *any* process, this one included, and it answers
    /// honestly: a lease this process wrote a moment ago names a process that
    /// is plainly running. Whether that means the thing it was taken for is
    /// still under way is a different question, and one this cannot see — which
    /// is why it is the second guard and not the first. See
    /// [`crate::webui::withdraw_what_was_left_behind`], where the turn is what
    /// answers "is anybody inside a wake" and this only answers "is the process
    /// that left this still there".
    pub fn alive(&self) -> bool {
        boot().is_some_and(|boot| boot == self.boot)
            && started(self.pid).is_some_and(|started| started == self.started)
    }

    /// The lease as it goes into the file: one `key value` per line, nothing
    /// escaped, and every value already known to hold no newline.
    pub(crate) fn written(&self) -> String {
        let mut text = String::new();
        text.push_str(&format!("pid {}\n", self.pid));
        text.push_str(&format!("started {}\n", self.started));
        text.push_str(&format!("boot {}\n", self.boot));
        text.push_str(&format!("session {}\n", self.session));
        text.push_str(&format!("at {}\n", self.at));
        if let Some(backend) = &self.backend {
            text.push_str(&format!("backend {backend}\n"));
        }
        if let Some(request) = self.request {
            text.push_str(&format!("request {request}\n"));
        }
        text
    }

    /// And back again. `None` for anything that is not one — an empty file, a
    /// half-written one, a note left by a version that wrote something else —
    /// because every caller has a right answer for not knowing and none of them
    /// wants a default that looks like knowledge.
    pub fn read(text: &str) -> Option<Lease> {
        let field = |name: &str| {
            text.lines().find_map(|line| {
                line.split_once(' ')
                    .filter(|(key, _)| *key == name)
                    .map(|(_, value)| value.trim().to_string())
            })
        };
        let number = |name: &str| field(name).and_then(|value| value.parse::<u64>().ok());
        Some(Lease {
            pid: number("pid")?.try_into().ok()?,
            started: number("started")?,
            boot: number("boot")?,
            session: field("session").unwrap_or_default(),
            backend: field("backend"),
            request: number("request"),
            at: number("at").unwrap_or_default(),
        })
    }

    /// The holder, named in a line somebody reads.
    fn said(&self) -> String {
        let mut said = format!("pid {}", self.pid);
        if !self.session.is_empty() {
            said.push_str(&format!(" in {}", self.session));
        }
        if let Some(request) = self.request {
            said.push_str(&format!(" on request {request}"));
        }
        said
    }
}

/// Which session this process draws into, as one string.
fn this_session() -> String {
    let said: Vec<String> = crate::client::DRAWS_INTO
        .iter()
        .map(|name| match std::env::var(name) {
            Ok(value) => format!("{name}={value}"),
            Err(_) => format!("{name}="),
        })
        .collect();
    one_line(&said.join(" "))
}

/// Anything that goes into a line of the lease, with the one character that
/// would make it two lines taken out.
///
/// Paths and environment values may hold a newline on Linux, however unlikely
/// that is of a `$HOME` or a `$DISPLAY`. A lease is diagnostic, so the honest
/// thing is to keep it readable rather than to invent an escaping nobody would
/// ever exercise.
fn one_line(text: &str) -> String {
    text.replace(['\n', '\r'], " ")
}

/// When a process started, in clock ticks since boot.
///
/// Field 22 of `/proc/<pid>/stat`, which cannot be reached by counting from the
/// left: field 2 is the executable's name in parentheses and may hold both
/// spaces and parentheses of its own. The documented way round it is to cut at
/// the **last** `)` — everything after it is fields 3 onwards, whatever the
/// name was — so field 22 is the twentieth token of what is left.
fn started(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_the_name = &stat[stat.rfind(')')? + 1..];
    after_the_name
        .split_ascii_whitespace()
        .nth(19)
        .and_then(|ticks| ticks.parse().ok())
}

/// When the machine booted, in seconds since the epoch.
fn boot() -> Option<u64> {
    let stat = std::fs::read_to_string("/proc/stat").ok()?;
    stat.lines()
        .find_map(|line| line.strip_prefix("btime "))
        .and_then(|seconds| seconds.trim().parse().ok())
}

/// Read a lock's lease off the disk, for a caller that wants to know who left
/// something behind rather than to take the turn.
pub fn lease_on(what: What) -> Option<Lease> {
    let path = beside_the_state(what.file())?;
    let mut file = std::fs::File::open(path).ok()?;
    let mut text = String::new();
    file.read_to_string(&mut text).ok()?;
    Lease::read(&text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A scratch state directory, so no test ever takes the lock the live
    /// session is using.
    fn scratch(name: &str) -> PathBuf {
        let state = std::env::temp_dir().join(format!(
            "lxb-turns-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&state);
        std::fs::create_dir_all(&state).unwrap();
        state
    }

    /// This process is alive, and a lease naming it says so.
    #[test]
    fn a_lease_this_process_wrote_is_alive() {
        let lease = Lease::of_this_process(&Behalf::nothing()).expect("this process is in /proc");
        assert_eq!(lease.pid, std::process::id());
        assert!(lease.alive());
    }

    /// And the two ways of not being: a pid nobody is wearing, and a pid
    /// somebody else is.
    ///
    /// The second is the one that matters. A lease whose owner died and whose
    /// number has been handed on reads, on the pid alone, exactly like a lease
    /// whose owner is right there — and acting on that is a session taking a
    /// live marker away from another.
    #[test]
    fn a_lease_is_dead_when_its_process_is() {
        let mine = Lease::of_this_process(&Behalf::nothing()).expect("this process is in /proc");

        // Init is running and is certainly not this process. Same pid space,
        // same boot, another start time.
        let reused = Lease {
            pid: 1,
            ..mine.clone()
        };
        assert!(
            !reused.alive(),
            "a reused pid read as the process that left the lease"
        );

        // And a reboot in between, which is a lease naming nothing at all.
        let before_a_reboot = Lease {
            boot: mine.boot - 1,
            ..mine.clone()
        };
        assert!(
            !before_a_reboot.alive(),
            "a lease from before a restart read as live"
        );
    }

    /// What is written is what is read.
    #[test]
    fn a_lease_survives_the_round_trip() {
        let mut lease = Lease::of_this_process(&Behalf {
            backend: Some(PathBuf::from("/home/somebody/.local/share/Steam")),
            request: Some(17),
        })
        .expect("this process is in /proc");
        lease.at = 1_756_938_123;
        assert_eq!(Lease::read(&lease.written()), Some(lease));
    }

    /// Anything that is not a lease is not read as one.
    ///
    /// The note this replaces held a bare path, and a build carrying that
    /// version can have written one an hour ago. Reading it as a lease with
    /// zeroes in it would make it a lease naming pid 0, which `alive` would
    /// answer about honestly and by luck — this is the same answer, on purpose.
    #[test]
    fn what_is_not_a_lease_reads_as_nothing() {
        assert_eq!(Lease::read(""), None);
        assert_eq!(
            Lease::read("/home/somebody/.local/share/Steam/.cef-enable-remote-debugging"),
            None
        );
        assert_eq!(Lease::read("pid 12\nstarted 4\n"), None, "no boot time");
    }

    /// A second process cannot have a turn the first is holding, and gets it
    /// the moment the first lets go.
    ///
    /// Two `flock`s on two handles of one file, which is the same exclusion a
    /// second process gets and the reason the handle is opened per turn rather
    /// than kept. What is under test is the file half alone — the local mutex
    /// would make this pass whatever the file did — so the lock is taken here
    /// through `lock` rather than through `take`.
    #[test]
    fn a_turn_excludes_a_second_holder_of_the_same_file() {
        let state = scratch("exclusion");
        let path = state.join("one.lock");
        let first = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .unwrap();
        let second = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .unwrap();

        assert!(lock(&first, Wait::No), "nobody had it and it was refused");
        assert!(
            !lock(&second, Wait::No),
            "two holders of one turn at the same time"
        );

        drop(first);
        assert!(
            lock(&second, Wait::No),
            "the turn was let go of and nobody could take it"
        );
        let _ = std::fs::remove_dir_all(&state);
    }

    /// `take_if_free` answers rather than waits, which is the whole of what the
    /// tidying at startup needs of it.
    ///
    /// **[`What::TheClient`] on purpose, and it is not arbitrary.** A turn is
    /// one per name per process, so a test that asserts one is *free* is
    /// asserting about every other test running beside it — and `cargo test`
    /// runs them on threads of one process. Every test in this crate that takes
    /// the client's turn holds [`crate::one_at_a_time_with_the_environment`]
    /// first, as this one does, so they are in a queue; the wizard's is taken
    /// by two tests in [`crate::webui`] that have no reason to hold that mutex
    /// and correctly do not, and asserting the wizard was free here failed
    /// whenever one of them was between its two halves.
    #[test]
    fn a_turn_that_is_taken_is_not_free() {
        let _environment = crate::one_at_a_time_with_the_environment();
        let was = std::env::var_os("XDG_STATE_HOME");
        let state = scratch("free");
        unsafe { std::env::set_var("XDG_STATE_HOME", &state) };

        let held = take(What::TheClient, Behalf::nothing());

        // From this thread the local mutex would deadlock rather than answer,
        // so the second asker is a thread of its own — which is also the shape
        // of the case: another session asking while this one holds it.
        let (answered, was_free) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = answered.send(take_if_free(What::TheClient, Behalf::nothing()).is_some());
        });
        assert_eq!(
            was_free.recv_timeout(Duration::from_secs(5)),
            Ok(false),
            "a turn somebody was holding answered that it was free"
        );

        drop(held);
        let (answered, was_free) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = answered.send(take_if_free(What::TheClient, Behalf::nothing()).is_some());
        });
        assert_eq!(
            was_free.recv_timeout(Duration::from_secs(5)),
            Ok(true),
            "a turn nobody was holding would not be taken"
        );

        match was {
            Some(was) => unsafe { std::env::set_var("XDG_STATE_HOME", was) },
            None => unsafe { std::env::remove_var("XDG_STATE_HOME") },
        }
        let _ = std::fs::remove_dir_all(&state);
    }

    /// The holder writes down who it is, and it can be read back off the disk
    /// without taking the turn.
    #[test]
    fn a_held_turn_says_whose_it_is() {
        let _environment = crate::one_at_a_time_with_the_environment();
        let was = std::env::var_os("XDG_STATE_HOME");
        let state = scratch("lease");
        unsafe { std::env::set_var("XDG_STATE_HOME", &state) };

        let held = take(
            What::TheAccountList,
            Behalf {
                backend: Some(PathBuf::from("/home/somebody/.local/share/Steam")),
                request: Some(42),
            },
        );
        let lease = lease_on(What::TheAccountList).expect("the holder wrote nothing down");
        assert_eq!(lease.pid, std::process::id());
        assert_eq!(lease.request, Some(42));
        assert!(lease.alive());
        assert!(whose(What::TheAccountList).contains("request 42"));
        drop(held);

        match was {
            Some(was) => unsafe { std::env::set_var("XDG_STATE_HOME", was) },
            None => unsafe { std::env::remove_var("XDG_STATE_HOME") },
        }
        let _ = std::fs::remove_dir_all(&state);
    }
}
