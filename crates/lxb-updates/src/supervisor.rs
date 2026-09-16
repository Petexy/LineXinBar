//! Start outside the compositor's session scope where a systemd user manager
//! exists. This does not enable lingering or promise survival after final logout.
use std::{
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn launcher(helper: &Path, state: &Path) -> Option<Command> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
    if !Path::new(&runtime).join("systemd/private").exists() {
        return None;
    }
    let helper = helper.canonicalize().ok()?;
    // systemd expands dollar expressions in ExecStart. Installed paths are
    // plain paths; unusual development paths use the ordinary detached fallback.
    if helper.to_string_lossy().contains(['$', '%']) {
        return None;
    }
    let executable = crate::process::find("systemd-run")?;
    let mut command = Command::new(executable);
    command
        .args([
            "--user",
            "--quiet",
            "--collect",
            "--no-block",
            "--service-type=exec",
            "--property=Restart=no",
        ])
        .arg(format!("--unit=lxb-updates-{:x}", crate::stamp(state)));
    // Deliberately forward named runtime settings only, never the complete
    // desktop environment (which may contain tokens or other credentials).
    for key in [
        "HOME",
        "PATH",
        "XDG_STATE_HOME",
        "XDG_DATA_HOME",
        "XDG_CONFIG_HOME",
        "XDG_RUNTIME_DIR",
        "DBUS_SESSION_BUS_ADDRESS",
        "NIX_PATH",
        "GUIX_PROFILE",
    ] {
        if std::env::var_os(key).is_some() {
            command.arg(format!("--setenv={key}"));
        }
    }
    command
        .arg("--setenv=LXB_UPDATES_SUPERVISED=1")
        .arg("--")
        .arg(helper)
        .arg("serve")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Some(command)
}

pub fn start(helper: &Path, state: &Path) -> bool {
    let Some(mut command) = launcher(helper, state) else {
        return false;
    };
    let Ok(mut child) = command.spawn() else {
        return false;
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Err(_) => return false,
            _ => {}
        }
        if Instant::now() >= deadline {
            // Only the service-registration client is stopped. A registered
            // coordinator belongs to the user manager and retains its own lock.
            let _ = child.kill();
            let _ = child.wait();
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
