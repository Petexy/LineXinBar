//! A live inhibitor, never a persisted promise. Keep its descriptor open until
//! native processes have exited and job authorization has been destroyed.
use anyhow::{bail, Context, Result};
use std::{
    fs::{File, OpenOptions},
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::Path,
};
use zbus::blocking::{Connection, Proxy};

pub struct Protection {
    _connection: Connection,
    _activity: Option<File>,
    _lock: zbus::zvariant::OwnedFd,
}
impl Protection {
    pub(crate) fn descriptor(&self) -> std::os::fd::RawFd {
        self._lock.as_raw_fd()
    }
    pub fn acquire() -> Result<Self> {
        let activity = if unsafe { libc::geteuid() } == 0 {
            Some(root_activity()?)
        } else {
            None
        };
        let connection = Connection::system().context("Power protection is unavailable")?;
        let proxy = Proxy::new(
            &connection,
            "org.freedesktop.login1",
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
        )?;
        // Refuse to race an already accepted shutdown/suspend. Acquiring a new
        // inhibitor cannot undo a power transition that has already begun.
        for property in ["PreparingForShutdown", "PreparingForSleep"] {
            if proxy.get_property::<bool>(property)? {
                bail!("A power transition is already in progress. Updates have not started.");
            }
        }
        let lock = proxy.call("Inhibit", &(
            "shutdown:sleep:idle:handle-power-key:handle-suspend-key:handle-hibernate-key:handle-lid-switch",
            "LineXinBar updates", "Updates are being installed", "block",
        )).context("Cannot protect this update from shutdown or sleep")?;
        for property in ["PreparingForShutdown", "PreparingForSleep"] {
            if proxy.get_property::<bool>(property)? {
                bail!("A power transition started before protection was acquired. Updates have not started.");
            }
        }
        Ok(Self {
            _connection: connection,
            _activity: activity,
            _lock: lock,
        })
    }
}

const ACTIVITY: &str = "/run/linexinbar/updates.lock";
fn lock(file: &File) -> Result<()> {
    if !crate::process::flock(file, libc::LOCK_EX)? {
        bail!("Another update or power operation is in progress");
    }
    Ok(())
}
fn root_activity() -> Result<File> {
    let directory = Path::new(ACTIVITY).parent().unwrap();
    match std::fs::create_dir(directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    crate::policy::protected(directory)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o644)
        .custom_flags(libc::O_NOFOLLOW)
        .open(ACTIVITY)?;
    crate::policy::protected(Path::new(ACTIVITY))?;
    lock(&file)?;
    Ok(file)
}
/// Also guards an orphaned root writer after the unprivileged coordinator exits.
pub fn power_permit() -> Result<Option<File>> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(ACTIVITY)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    crate::policy::protected(Path::new(ACTIVITY))?;
    lock(&file)?;
    Ok(Some(file))
}
