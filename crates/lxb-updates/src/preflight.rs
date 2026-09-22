//! Basic host conditions supplement the native manager's exact requirements.
//!
//! A notice is written as `headline · detail`: the headline is the plain
//! sentence the panel puts in front of somebody about to press Update, and
//! the detail is the mount path, the number of megabytes and the reason,
//! which belong in Full output with everything else technical. Anything
//! without a `·` is short enough to be both.
use anyhow::{bail, Result};
use std::ffi::CString;
use std::path::Path;

pub fn check() -> Result<Vec<String>> {
    let mut notices = vec![];
    // One notice a filesystem, not one a path: on a machine whose ESP is
    // mounted at /boot, /boot/efi is a directory on it and the same 176 MiB
    // was reported twice.
    let mut seen = std::collections::HashSet::new();
    for path in ["/", "/var", "/boot", "/boot/efi"] {
        let Ok(metadata) = std::fs::metadata(path) else {
            continue;
        };
        if !seen.insert(std::os::unix::fs::MetadataExt::dev(&metadata)) {
            continue;
        }
        if let Some(notice) = storage(path, free(path)?)? {
            notices.push(notice);
        }
    }
    notices.extend(power(Path::new("/sys/class/power_supply"))?);
    Ok(notices)
}

/// What one filesystem has to say about an update about to be installed on
/// it: nothing, a notice, or a refusal.
///
/// Stop an already-full filesystem outright. Only the native solver knows a
/// transaction's exact space requirement, including its download caches, so
/// everything above that floor is a notice beside the Update button rather
/// than an answer instead of it.
fn storage(path: &str, bytes: u64) -> Result<Option<String>> {
    if bytes < 16 * 1024 * 1024 {
        bail!("{path} has less than 16 MiB available. Free space before updating.");
    }
    Ok((bytes < nearly_full(path)).then(|| format!(
        "Storage is nearly full · {path} has {} MiB free; the native manager checks whether the transaction fits",
        bytes / 1024 / 1024
    )))
}

/// How little free space is worth saying something about, on the filesystem
/// mounted here.
///
/// **A boot filesystem is not a small root filesystem.** It is sized for what
/// it holds — a few kernels and their initramfs images — and it is meant to be
/// most of the way full: 300 MiB to a gigabyte is the usual partition, and two
/// thirds of it in use is a perfectly healthy machine with two kernels
/// installed. Judging it by the half-gigabyte a root filesystem is judged by
/// puts a warning on the review that nobody can ever clear, because clearing
/// it would mean deleting the kernel they are running.
///
/// This developer's machine is the case that found it: a 511 MiB `/boot` with
/// 173 MiB free — room for another kernel and its initramfs twice over — read
/// as "Storage is nearly full" on every single check. A hundred is what one
/// more kernel and its initramfs want, and under it the warning is about
/// something.
///
/// Where `/boot` is not a filesystem of its own it is never asked about: the
/// loop above takes one notice per *device*, so a machine with the kernel on
/// its root filesystem is judged by the root's own number, which is the
/// honest way round.
fn nearly_full(path: &str) -> u64 {
    if Path::new(path).starts_with("/boot") {
        100 * 1024 * 1024
    } else {
        512 * 1024 * 1024
    }
}

fn power(directory: &Path) -> Result<Vec<String>> {
    let mut notices = vec![];
    let mut batteries = vec![];
    let mut mains = false;
    if let Ok(entries) = std::fs::read_dir(directory) {
        for entry in entries.flatten() {
            let p = entry.path();
            if std::fs::read_to_string(p.join("scope")).is_ok_and(|s| s.trim() == "Device") {
                continue; // A mouse/controller battery does not power the host.
            }
            let kind = std::fs::read_to_string(p.join("type")).unwrap_or_default();
            if kind.trim() == "Battery" {
                let charge = std::fs::read_to_string(p.join("capacity"))
                    .ok()
                    .and_then(|s| s.trim().parse::<u8>().ok());
                let status = std::fs::read_to_string(p.join("status")).unwrap_or_default();
                batteries.push((charge, status.trim().to_owned()));
            } else if std::fs::read_to_string(p.join("online")).is_ok_and(|s| s.trim() == "1") {
                mains = true;
            }
        }
    }
    if !mains {
        for (capacity, status) in batteries {
            if status == "Charging" {
                continue;
            }
            match capacity {
                Some(p) if p < 20 => {
                    bail!("Battery charge is below 20%. Connect power before updating.")
                }
                Some(_) => notices.push(
                    "Running on battery · connect power for long system or firmware updates".into(),
                ),
                None => {
                    notices.push("Battery charge is unknown · connect power before updating".into())
                }
            }
        }
    }
    Ok(notices)
}

fn free(path: &str) -> Result<u64> {
    let path = CString::new(path)?;
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let stats = unsafe { stats.assume_init() };
    Ok(stats.f_bavail.saturating_mul(stats.f_frsize))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A boot filesystem is judged by what a kernel needs, not by what a root
    /// filesystem needs.
    ///
    /// The user's machine on 2026-09-22: 173 MiB free on a 511 MiB `/boot`,
    /// which is room for another kernel and was called nearly full on every
    /// check. "Right now I have 170 MB free which is plenty for kernels and
    /// initramfs, hence this should not be displayed."
    #[test]
    fn a_boot_filesystem_is_only_nearly_full_under_a_hundred_megabytes() {
        let megabytes = |n: u64| n * 1024 * 1024;
        for path in ["/boot", "/boot/efi"] {
            assert_eq!(storage(path, megabytes(173)).unwrap(), None);
            assert_eq!(storage(path, megabytes(100)).unwrap(), None);
            assert!(storage(path, megabytes(99))
                .unwrap()
                .is_some_and(|notice| notice.contains("99 MiB free")));
            // And the floor is the floor, wherever it is: an update has to be
            // written somewhere.
            assert!(storage(path, megabytes(15)).is_err());
        }
        // Everything else keeps the half gigabyte it had. A root filesystem
        // holds the machine and an update's caches, and is not sized for one
        // job the way a boot partition is.
        for path in ["/", "/var"] {
            assert!(storage(path, megabytes(173)).unwrap().is_some());
            assert_eq!(storage(path, megabytes(512)).unwrap(), None);
        }
        // A path that merely begins with the same letters is not a boot
        // filesystem.
        assert!(storage("/bootleg", megabytes(173)).unwrap().is_some());
    }

    #[test]
    fn peripheral_batteries_do_not_block_host_updates() {
        let path = std::env::temp_dir().join(format!("lxb-update-power-{}", std::process::id()));
        let battery = path.join("battery");
        std::fs::create_dir_all(&battery).unwrap();
        for (file, text) in [
            ("type", "Battery"),
            ("scope", "Device"),
            ("capacity", "5"),
            ("status", "Discharging"),
        ] {
            std::fs::write(battery.join(file), text).unwrap();
        }
        assert!(power(&path).unwrap().is_empty());
        std::fs::write(battery.join("scope"), "System").unwrap();
        assert!(power(&path).is_err());
        let mains = path.join("mains");
        std::fs::create_dir(&mains).unwrap();
        std::fs::write(mains.join("type"), "Mains").unwrap();
        std::fs::write(mains.join("online"), "1").unwrap();
        assert!(power(&path).unwrap().is_empty());
        std::fs::remove_dir_all(path).unwrap();
    }
}
