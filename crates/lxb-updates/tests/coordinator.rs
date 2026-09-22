//! Exercise the coordinator and PTY protocol with a test-only power guard and fake tools.
//! No fixture invokes a real package manager or firmware installation.
use lxb_updates::{Phase, Request, Response, SourceId};
use std::{
    fs,
    io::{Read, Write},
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Fixture {
    root: PathBuf,
    daemon: Child,
    /// Seconds of idleness before the coordinator quits; the default is a
    /// quarter of an hour, which no test waits for.
    idle: Option<u64>,
}
impl Fixture {
    fn new() -> Self {
        Self::with_idle(None)
    }
    fn with_idle(idle: Option<u64>) -> Self {
        let root = std::env::temp_dir().join(format!(
            "lxb-update-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("bin")).unwrap();
        let devices = serde_json::json!({"Devices":[
            {"DeviceId":"1111111111111111111111111111111111111111", "Name":"Test SSD", "Plugin":"nvme", "Protocol":"org.nvmexpress", "Flags":["updatable"], "Version":"1", "Releases":[{"Version":"2"}]},
            {"DeviceId":"2222222222222222222222222222222222222222", "Name":"System Firmware", "Plugin":"uefi_capsule", "Protocol":"org.uefi.capsule", "Flags":["updatable"], "Releases":[{"Version":"2"}]},
            {"DeviceId":"3333333333333333333333333333333333333333", "Name":"Unknown peripheral", "Plugin":"future", "Protocol":"future.device", "Flags":["updatable"], "Releases":[{"Version":"2"}]}
        ]});
        fs::write(root.join("bin/devices.json"), devices.to_string()).unwrap();
        script(
            &root.join("bin/fwupdmgr"),
            r#"#!/bin/sh
case "$1" in
refresh) printf '%s\n' "$*" > "${0%/*}/refresh.log"; printf '{}\n' ;;
get-devices|get-updates) cat "${0%/*}/devices.json" ;;
update)
    printf '%s\n' "$*" >> "$FIXTURE_LOG"
    printf 'Install the test SSD? [y/N] '
    read -r answer
    test "$answer" = y || exit 3
    stty -echo
    printf 'Private test password: '
    read -r secret
    stty echo
    printf '\nDevice operation finished\n'
    ;;
*) exit 1 ;;
esac
"#,
        );
        script(&root.join("bin/pacman"), "#!/bin/sh\nexit 0\n");
        let daemon = Self::spawn(&root, idle);
        let fixture = Self { root, daemon, idle };
        fixture.wait(|_| true);
        fixture
    }
    fn spawn(root: &Path, idle: Option<u64>) -> Child {
        let mut command = Command::new(std::env::current_exe().unwrap());
        if let Some(idle) = idle {
            command.env("LXB_UPDATES_IDLE_SECONDS", idle.to_string());
        }
        if root.join("deny-protection").exists() {
            command.env("LXB_FIXTURE_NO_PROTECTION", "1");
        }
        command
            .args(["--exact", "fixture_daemon", "--nocapture"])
            .env("LXB_FIXTURE_DAEMON", "1")
            .env("XDG_STATE_HOME", root.join("state"))
            .env("HOME", root)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", root.join("bin").display()),
            )
            .env("FIXTURE_LOG", root.join("install.log"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap()
    }
    fn restart(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
        let _ = fs::remove_file(self.socket());
        self.daemon = Self::spawn(&self.root, self.idle);
        self.wait(|_| true);
    }
    fn state(&self) -> PathBuf {
        self.root.join("state/lxb/updates")
    }
    fn socket(&self) -> PathBuf {
        lxb_updates::service::socket_path(&self.root.join("state/lxb/updates")).unwrap()
    }
    fn request(&self, request: Request) -> Response {
        let mut socket = UnixStream::connect(self.socket()).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        serde_json::to_writer(
            &mut socket,
            &serde_json::json!({"protocol": lxb_updates::PROTOCOL, "request": request}),
        )
        .unwrap();
        socket.write_all(b"\n").unwrap();
        serde_json::from_reader(socket).unwrap()
    }
    fn input(&self, job: u64, response: &str) -> Response {
        let mut socket = UnixStream::connect(self.socket()).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        writeln!(socket, "INPUT {} {job}\n{response}", lxb_updates::PROTOCOL).unwrap();
        serde_json::from_reader(socket).unwrap()
    }
    fn wait(&self, ready: impl Fn(&Response) -> bool) -> Response {
        let start = Instant::now();
        loop {
            if self.socket().exists() {
                let r = self.request(Request::Status);
                if ready(&r) {
                    return r;
                }
            }
            assert!(
                start.elapsed() < Duration::from_secs(15),
                "The coordinator did not reach the expected state: {:?}",
                fs::read_to_string(self.root.join("state/lxb/updates/status.json"))
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
        let _ = fs::remove_file(self.socket());
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn script(path: &Path, source: &str) {
    fs::write(path, source).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn detached_updates_require_current_review_preserve_prompts_and_exclude_bios() {
    assert_ne!(
        unsafe { libc::geteuid() },
        0,
        "Run update integration tests as an ordinary user"
    );
    let fixture = Fixture::new();
    assert!(fixture.request(Request::Install { job: 0 }).error.is_some());
    fixture.request(Request::Check {
        selected: vec![SourceId::Firmware],
    });
    let review = fixture.wait(|r| r.snapshot.phase == Phase::Reviewing);
    let refresh = fs::read_to_string(fixture.root.join("bin/refresh.log")).unwrap();
    assert!(refresh.contains("--no-remote-check"));
    assert!(refresh.contains("--no-unreported-check"));
    assert!(!refresh.contains("--force"));
    let firmware = review
        .snapshot
        .sources
        .iter()
        .find(|s| s.id == SourceId::Firmware)
        .unwrap();
    assert_eq!(firmware.items.len(), 1);
    assert_eq!(firmware.excluded.len(), 2);
    let job = review.snapshot.job;
    assert!(fixture
        .request(Request::Install { job: job + 1 })
        .error
        .is_some());
    assert!(!fixture.root.join("install.log").exists());
    fixture.request(Request::Install { job });
    let prompt = fixture.wait(|r| {
        r.snapshot.output.iter().any(|l| l.contains("[y/N]"))
            && r.events.iter().any(|e| e.attention)
    });
    let first_event = prompt
        .events
        .iter()
        .find(|e| e.attention)
        .unwrap()
        .id
        .clone();
    let delivered = fixture.request(Request::Delivered {
        event: first_event.clone(),
    });
    assert!(delivered
        .events
        .iter()
        .any(|e| e.id == first_event && e.delivered));
    // A reconnect gets the same durable event, not a fresh toast for every poll.
    assert_eq!(
        fixture
            .request(Request::Events)
            .events
            .iter()
            .filter(|e| e.attention)
            .count(),
        1
    );
    // Each request closes its connection. The native transaction is still alive
    // when another client reconnects; closing the view cannot kill its PTY.
    assert!(fixture
        .request(Request::Check { selected: vec![] })
        .error
        .is_some());
    assert!(fixture.input(job + 1, "y").error.is_some());
    assert!(fixture.input(job, "y").error.is_none());
    let password_prompt = fixture.wait(|r| {
        r.snapshot.secret
            && r.snapshot
                .output
                .iter()
                .any(|l| l.contains("Private test password"))
            && r.events.iter().any(|e| e.attention)
    });
    assert!(!password_prompt.events.iter().any(|e| e.id == first_event));
    assert!(password_prompt
        .events
        .iter()
        .any(|e| e.attention && !e.delivered));
    let secret = "fixture-private-never-journal-this";
    fixture.input(job, secret);
    // Two devices were excluded — a BIOS and a peripheral whose type is not
    // validated — and the one device that was updated finished. Excluding a
    // device the shell was never going to touch is not a part of the job
    // that failed, so the outcome is Completed and not Partial: a person who
    // reads "some updates could not be installed" goes looking for a failure
    // that never happened. The reasons stay in the source's excluded list.
    let completed = fixture.wait(|r| r.snapshot.phase == Phase::Completed);
    let event = fixture.wait(|r| r.events.iter().any(|e| !e.attention && e.job == job));
    assert!(!event.events.iter().any(|e| e.attention));
    assert!(completed.snapshot.results[0].success);
    assert_eq!(completed.snapshot.results.len(), 1);
    assert_eq!(
        completed
            .snapshot
            .sources
            .iter()
            .find(|s| s.id == lxb_updates::SourceId::Firmware)
            .map(|s| s.excluded.len()),
        Some(2)
    );
    let log = fs::read_to_string(fixture.root.join("install.log")).unwrap();
    assert!(log.contains("update 1111111111111111111111111111111111111111"));
    assert!(log.contains("--filter-protocol org.nvmexpress"));
    assert!(!log.contains("222222"));
    assert!(!log.contains("333333"));
    assert!(!log.contains("--assume-yes"));
    let history = fixture.request(Request::History);
    assert_eq!(history.history.len(), 1);
    assert!(!serde_json::to_string(&history).unwrap().contains(secret));
    let mut journal = String::new();
    fs::File::open(fixture.root.join("state/lxb/updates/history.json"))
        .unwrap()
        .read_to_string(&mut journal)
        .unwrap();
    assert!(!journal.contains(secret));
}

/// A power action and a running installation refuse each other.
///
/// This is the whole point of the activity lock and it is always between two
/// processes — the coordinator running the job, and the shell about to take a
/// power permit for a shutdown somebody just asked for. So it is tested with
/// two: the coordinator here is a real child process, and this test stands in
/// for the shell by taking the same lock the shell's `power_permit` takes.
///
/// Shared while updating, exclusive for a power action: a shutdown cannot
/// start behind an installation, and an installation cannot start behind a
/// shutdown.
#[test]
fn a_power_action_cannot_start_behind_a_running_installation() {
    use std::os::fd::AsRawFd;
    let fixture = Fixture::new();
    let lock = fixture.state().join("activity.lock");

    // As the shell would: the exclusive side of the same file.
    let power = |expect: bool| {
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock)
            .unwrap();
        let got = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
        assert_eq!(
            got,
            expect,
            "power permit {} while the job was {}",
            if got { "granted" } else { "refused" },
            if expect { "finished" } else { "running" }
        );
        drop(file);
    };

    // Idle: the machine may be powered off.
    power(true);

    fixture.request(Request::Check {
        selected: vec![SourceId::Firmware],
    });
    let job = fixture
        .wait(|r| r.snapshot.phase == Phase::Reviewing)
        .snapshot
        .job;
    fixture.request(Request::Install { job });
    fixture.wait(|r| r.snapshot.output.iter().any(|l| l.contains("[y/N]")));

    // A tool is writing to the machine. No power action may begin.
    power(false);

    fixture.input(job, "y");
    fixture.wait(|r| {
        r.snapshot.secret
            && r.snapshot
                .output
                .iter()
                .any(|l| l.contains("Private test password"))
    });
    // Still held, all the way through the job and not merely at its start.
    power(false);
    fixture.input(job, "anything");
    fixture.wait(|r| !r.snapshot.busy());

    // And released when the job ends, rather than kept for the coordinator's
    // lifetime — a machine that could not be turned off until the updater
    // decided to quit would be worse than one that never locked at all.
    power(true);
}

#[test]
fn cancellation_of_final_query_cannot_become_an_install_review() {
    let fixture = Fixture::new();
    script(
        &fixture.root.join("bin/fwupdmgr"),
        r#"#!/bin/sh
case "$1" in
refresh) printf '{}\n' ;;
get-devices) cat "${0%/*}/devices.json" ;;
get-updates)
    touch "${0%/*}/query-started"
    sleep 1
    cat "${0%/*}/devices.json" ;;
*) exit 7 ;;
esac
"#,
    );
    let checking = fixture.request(Request::Check {
        selected: vec![SourceId::Firmware],
    });
    fixture.wait(|_| fixture.root.join("bin/query-started").exists());
    assert!(fixture
        .request(Request::CancelCheck {
            job: checking.snapshot.job
        })
        .error
        .is_none());
    let cancelled = fixture.wait(|r| r.snapshot.phase == Phase::Cancelled);
    assert!(fixture
        .request(Request::Install {
            job: cancelled.snapshot.job
        })
        .error
        .is_some());
    assert!(!fixture.root.join("install.log").exists());
}

#[test]
fn journal_failure_rejects_install_and_preserves_review() {
    let fixture = Fixture::new();
    fixture.request(Request::Check {
        selected: vec![SourceId::Firmware],
    });
    let review = fixture.wait(|r| r.snapshot.phase == Phase::Reviewing);
    let journal = fixture.root.join("state/lxb/updates/status.json");
    fs::remove_file(&journal).unwrap();
    fs::create_dir(&journal).unwrap();
    let rejected = fixture.request(Request::Install {
        job: review.snapshot.job,
    });
    assert!(rejected.error.is_some());
    assert_eq!(rejected.snapshot.phase, Phase::Reviewing);
    assert!(!fixture.root.join("install.log").exists());
}

#[test]
fn restart_recovers_interruption_without_replaying_and_preserves_staged_state() {
    let mut fixture = Fixture::new();
    fixture.daemon.kill().unwrap();
    fixture.daemon.wait().unwrap();
    let mut saved = lxb_updates::Snapshot {
        job: 42,
        phase: Phase::Running,
        boot_id: fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap(),
        ..Default::default()
    };
    let journal = fixture.root.join("state/lxb/updates/status.json");
    fs::write(&journal, serde_json::to_vec(&saved).unwrap()).unwrap();
    fixture.restart();
    assert_eq!(
        fixture.request(Request::Status).snapshot.phase,
        Phase::Interrupted
    );
    assert!(!fixture.root.join("install.log").exists());
    fixture.daemon.kill().unwrap();
    fixture.daemon.wait().unwrap();
    saved.phase = Phase::Completed;
    saved.restart = Some(lxb_updates::Restart::Normal);
    fs::write(&journal, serde_json::to_vec(&saved).unwrap()).unwrap();
    fixture.restart();
    fixture.request(Request::Check {
        selected: vec![SourceId::Firmware],
    });
    let checked = fixture.wait(|r| r.snapshot.phase == Phase::Reviewing);
    assert_eq!(checked.snapshot.restart, Some(lxb_updates::Restart::Normal));
    fixture.daemon.kill().unwrap();
    fixture.daemon.wait().unwrap();
    saved.boot_id = "previous-boot".into();
    fs::write(&journal, serde_json::to_vec(&saved).unwrap()).unwrap();
    fixture.restart();
    let rebooted = fixture.request(Request::Status).snapshot;
    assert_eq!(rebooted.phase, Phase::AwaitingVerification);
    assert_eq!(rebooted.restart, None);
    assert!(!fixture.root.join("install.log").exists());
}

#[test]
fn journal_failure_during_a_transaction_stops_the_next_device() {
    let fixture = Fixture::new();
    let path = fixture.root.join("bin/devices.json");
    let mut devices: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let mut second = devices["Devices"][0].clone();
    second["DeviceId"] = "4444444444444444444444444444444444444444".into();
    devices["Devices"].as_array_mut().unwrap().push(second);
    fs::write(path, serde_json::to_vec(&devices).unwrap()).unwrap();
    script(
        &fixture.root.join("bin/fwupdmgr"),
        r#"#!/bin/sh
case "$1" in
refresh) printf '{}\n' ;;
get-devices|get-updates) cat "${0%/*}/devices.json" ;;
update)
    printf '%s\n' "$*" >> "$FIXTURE_LOG"
    printf 'Wait for response: '
    read -r answer
    printf 'Transaction finished\n' ;;
*) exit 7 ;;
esac
"#,
    );
    fixture.request(Request::Check {
        selected: vec![SourceId::Firmware],
    });
    let review = fixture.wait(|r| r.snapshot.phase == Phase::Reviewing);
    fixture.request(Request::Install {
        job: review.snapshot.job,
    });
    fixture.wait(|r| {
        r.snapshot
            .output
            .iter()
            .any(|s| s.contains("Wait for response"))
    });
    let journal = fixture.root.join("state/lxb/updates/status.json");
    fs::remove_file(&journal).unwrap();
    fs::create_dir(&journal).unwrap();
    fixture.input(review.snapshot.job, "continue");
    let result = fixture.wait(|r| !r.snapshot.busy());
    assert!(!result.snapshot.results[0].success);
    let log = fs::read_to_string(fixture.root.join("install.log")).unwrap();
    assert_eq!(log.lines().count(), 1);
    assert!(!log.contains("4444444444444444444444444444444444444444"));
}

#[test]
fn failed_firmware_refresh_is_not_reported_as_up_to_date() {
    let fixture = Fixture::new();
    script(
        &fixture.root.join("bin/fwupdmgr"),
        r#"#!/bin/sh
case "$1" in
refresh) printf 'fixture network failure\n' >&2; exit 1 ;;
get-devices|get-updates) cat "${0%/*}/devices.json" ;;
*) exit 9 ;;
esac
"#,
    );
    fixture.request(Request::Check {
        selected: vec![SourceId::Firmware],
    });
    let review = fixture.wait(|r| r.snapshot.phase == Phase::Reviewing);
    let firmware = review
        .snapshot
        .sources
        .iter()
        .find(|s| s.id == SourceId::Firmware)
        .unwrap();
    assert!(!firmware.fresh);
    assert!(!firmware.executable);
    assert!(firmware
        .error
        .as_ref()
        .unwrap()
        .contains("fixture network failure"));
    assert_eq!(firmware.excluded.len(), 2);
    assert!(!fixture.root.join("install.log").exists());
}

#[test]
fn echo_disabled_progress_does_not_notify_a_password_request() {
    let fixture = Fixture::new();
    script(
        &fixture.root.join("bin/fwupdmgr"),
        r#"#!/bin/sh
case "$1" in
refresh) printf '{}\n' ;;
get-devices|get-updates) cat "${0%/*}/devices.json" ;;
update)
    stty -echo
    printf 'Updating 1/2... 66%%\n'
    sleep 1
    stty echo
    printf 'Transaction finished\n' ;;
*) exit 7 ;;
esac
"#,
    );
    fixture.request(Request::Check {
        selected: vec![SourceId::Firmware],
    });
    let review = fixture.wait(|r| r.snapshot.phase == Phase::Reviewing);
    fixture.request(Request::Install {
        job: review.snapshot.job,
    });
    let progress =
        fixture.wait(|r| r.snapshot.secret && r.snapshot.output.iter().any(|s| s.contains("66%")));
    assert!(!lxb_updates::prompt::password(&progress.snapshot));
    assert!(!progress.events.iter().any(|e| e.attention));
    let finished = fixture
        .wait(|r| r.snapshot.phase == Phase::Completed && r.events.iter().any(|e| !e.attention));
    assert!(!finished.events.iter().any(|e| e.attention));
}

/// A coordinator with nothing running and nothing staged is not kept for
/// life: it quits after its idle time and takes its socket with it, and the
/// shell starts another on the next request. A staged restart is the one
/// thing only a live coordinator can carry out, so that keeps it.
#[test]
fn an_idle_coordinator_quits_and_a_staged_restart_keeps_it() {
    let mut fixture = Fixture::with_idle(Some(1));
    let start = Instant::now();
    loop {
        if let Ok(Some(status)) = fixture.daemon.try_wait() {
            assert!(status.success(), "{status:?}");
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "the idle coordinator did not quit"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!fixture.socket().exists(), "the socket was left behind");

    let staged = lxb_updates::Snapshot {
        phase: Phase::Completed,
        restart: Some(lxb_updates::Restart::Normal),
        boot_id: fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap(),
        ..lxb_updates::Snapshot::default()
    };
    fs::write(
        fixture.state().join("status.json"),
        serde_json::to_string(&staged).unwrap(),
    )
    .unwrap();
    fixture.restart();
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        fixture.daemon.try_wait().unwrap().is_none(),
        "a coordinator with a staged restart quit"
    );
    assert!(fixture.request(Request::Status).snapshot.restart.is_some());
}

// This entry point exists only in the test executable. The shipped helper has
// no environment flag that disables mandatory protection or authorization.
#[test]
fn fixture_daemon() {
    if std::env::var_os("LXB_FIXTURE_DAEMON").is_none() {
        return;
    }
    struct FakeRuntime;
    impl lxb_updates::service::Runtime for FakeRuntime {
        fn begin(
            &self,
            _: &lxb_updates::Snapshot,
        ) -> anyhow::Result<lxb_updates::service::JobContext> {
            if std::env::var_os("LXB_FIXTURE_NO_PROTECTION").is_some() {
                anyhow::bail!("fixture power protection unavailable");
            }
            Ok(lxb_updates::service::JobContext::locally_protected(()))
        }
    }
    lxb_updates::service::serve_with(std::sync::Arc::new(FakeRuntime)).unwrap();
}

#[test]
fn unavailable_protection_stops_before_installation_and_releases_power_lock() {
    let mut fixture = Fixture::new();
    fs::write(fixture.root.join("deny-protection"), "").unwrap();
    fixture.restart();
    fixture.request(Request::Check {
        selected: vec![SourceId::Firmware],
    });
    let review = fixture.wait(|r| r.snapshot.phase == Phase::Reviewing);
    fixture.request(Request::Install {
        job: review.snapshot.job,
    });
    let failed = fixture.wait(|r| r.snapshot.phase == Phase::Failed);
    assert!(failed
        .snapshot
        .message
        .contains("fixture power protection unavailable"));
    assert!(!failed.snapshot.protected);
    assert!(!failed.snapshot.authorized);
    assert!(failed.snapshot.results.is_empty());
    assert!(!fixture.root.join("install.log").exists());
    // Completion must release the live lock; the journal alone cannot block power.
    let activity = fs::File::open(fixture.state().join("activity.lock")).unwrap();
    use std::os::fd::AsRawFd;
    assert_eq!(
        unsafe { libc::flock(activity.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
}

#[test]
fn incompatible_clients_cannot_start_an_installation() {
    let fixture = Fixture::new();
    fixture.request(Request::Check {
        selected: vec![SourceId::Firmware],
    });
    let review = fixture.wait(|r| r.snapshot.phase == Phase::Reviewing);
    for request in [
        serde_json::json!({"Install": {"job": review.snapshot.job}}),
        serde_json::json!({"protocol": 1, "request": {"Install": {"job": review.snapshot.job}}}),
    ] {
        let mut socket = UnixStream::connect(fixture.socket()).unwrap();
        serde_json::to_writer(&mut socket, &request).unwrap();
        socket.write_all(b"\n").unwrap();
        let response: Response = serde_json::from_reader(socket).unwrap();
        assert!(response.error.is_some());
        assert_eq!(response.snapshot.phase, Phase::Reviewing);
    }
    assert!(!fixture.root.join("install.log").exists());
}

#[test]
fn completion_notification_survives_reconnect_until_acknowledged() {
    let mut fixture = Fixture::new();
    // Fail before any native writer starts, then reconnect as a shell would.
    fs::write(fixture.root.join("deny-protection"), "").unwrap();
    fixture.restart();
    fixture.request(Request::Check {
        selected: vec![SourceId::Firmware],
    });
    let review = fixture.wait(|r| r.snapshot.phase == Phase::Reviewing);
    let job = review.snapshot.job;
    fixture.request(Request::Install { job });
    let finished = fixture.wait(|r| {
        r.events
            .iter()
            .any(|e| e.job == job && e.phase == Phase::Failed)
    });
    let id = finished
        .events
        .iter()
        .find(|e| e.job == job)
        .unwrap()
        .id
        .clone();
    assert!(fixture
        .request(Request::Delivered { event: id.clone() })
        .error
        .is_none());
    fixture.restart();
    let pending = fixture.request(Request::Events);
    assert_eq!(pending.events.len(), 1);
    assert_eq!(pending.events[0].id, id);
    assert!(
        pending.events[0].delivered,
        "reconnecting should retain the centre entry without another toast"
    );
    assert!(fixture
        .request(Request::Acknowledge { event: id })
        .error
        .is_none());
    fixture.restart();
    assert!(fixture.request(Request::Events).events.is_empty());
    assert!(!fixture.root.join("install.log").exists());
}

/// What is left is counted when the job ends, without anybody asking again.
///
/// A review is a photograph of what was waiting *before* the tools ran, and
/// nothing in a snapshot stops being true when a package is installed — so the
/// Settings column went on offering the ten updates it had just installed until
/// somebody pressed a row and made it look again. The count now follows the
/// machine: when the tools stop, each source that ran is asked once more, with
/// the same bounded read-only query the check uses.
///
/// It is a *count*, not an assumption. The fixture's tool has exactly one
/// update to give and gives it once, so a coordinator that believed the review
/// would still say one is waiting, and one that asked says none is. And it is
/// the same job throughout: a job number that moved would mean a whole new
/// check had been started behind the result on screen, which is the thing the
/// user would have had to press anyway.
#[test]
fn a_finished_job_counts_what_is_left_without_being_asked_again() {
    let fixture = Fixture::new();
    let after = serde_json::json!({"Devices":[
        {"DeviceId":"1111111111111111111111111111111111111111", "Name":"Test SSD", "Plugin":"nvme", "Protocol":"org.nvmexpress", "Flags":["updatable"], "Version":"2"},
        {"DeviceId":"2222222222222222222222222222222222222222", "Name":"System Firmware", "Plugin":"uefi_capsule", "Protocol":"org.uefi.capsule", "Flags":["updatable"], "Releases":[{"Version":"2"}]},
        {"DeviceId":"3333333333333333333333333333333333333333", "Name":"Unknown peripheral", "Plugin":"future", "Protocol":"future.device", "Flags":["updatable"], "Releases":[{"Version":"2"}]}
    ]});
    fs::write(fixture.root.join("bin/after.json"), after.to_string()).unwrap();
    script(
        &fixture.root.join("bin/fwupdmgr"),
        r#"#!/bin/sh
case "$1" in
refresh) printf '{}\n' ;;
get-devices|get-updates)
    if [ -f "${0%/*}/installed" ]; then cat "${0%/*}/after.json"; else cat "${0%/*}/devices.json"; fi ;;
update)
    printf 'Device operation finished\n'
    : > "${0%/*}/installed"
    ;;
*) exit 1 ;;
esac
"#,
    );
    fixture.request(Request::Check {
        selected: vec![SourceId::Firmware],
    });
    let review = fixture.wait(|r| r.snapshot.phase == Phase::Reviewing);
    let job = review.snapshot.job;
    let firmware = |r: &Response| {
        r.snapshot
            .sources
            .iter()
            .find(|s| s.id == SourceId::Firmware)
            .cloned()
            .unwrap()
    };
    assert_eq!(firmware(&review).items.len(), 1);
    fixture.request(Request::Install { job });
    let counted =
        fixture.wait(|r| r.snapshot.phase == Phase::Completed && firmware(r).items.is_empty());
    assert_eq!(counted.snapshot.job, job, "counting is not a new job");
    // The result the panel is about is untouched by the counting: the phase,
    // what each source did, and the devices that were never eligible.
    assert!(counted.snapshot.results.iter().all(|r| r.success));
    assert_eq!(counted.snapshot.results.len(), 1);
    assert_eq!(firmware(&counted).excluded.len(), 2);
}
