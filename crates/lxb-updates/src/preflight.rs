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
        let bytes = free(path)?;
        // Stop an already-full filesystem. Only the native solver knows the
        // transaction's exact space requirement, including download caches.
        if bytes < 16 * 1024 * 1024 {
            bail!("{path} has less than 16 MiB available. Free space before updating.");
        }
        if bytes < 512 * 1024 * 1024 {
            notices.push(format!(
                "Storage is nearly full · {path} has {} MiB free; the native manager checks whether the transaction fits",
                bytes / 1024 / 1024
            ));
        }
    }
    notices.extend(power(Path::new("/sys/class/power_supply"))?);
    Ok(notices)
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
