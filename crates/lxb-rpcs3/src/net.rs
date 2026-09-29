//! The two places this helper reaches over the network, and how.
//!
//! Sony's own update server, for the PlayStation 3 system software, and
//! GitHub, for Redump's published collection of disc keys. Both only when
//! something the user did asks for them: setting RPCS3 up, and starting a
//! disc that is still encrypted.

use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use crate::report::Trouble;

/// How long a whole request may take. The system software is two hundred
/// megabytes, so this is long; a machine that is not online at all is found
/// out by the connection's own, much shorter, limit.
const PATIENCE: Duration = Duration::from_secs(30 * 60);
const CONNECT: Duration = Duration::from_secs(20);

pub fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(PATIENCE))
        .timeout_connect(Some(CONNECT))
        .user_agent(concat!("LineXinBar/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// What a failed request was: a server answering no is not the machine being
/// offline, and the shell says the two differently.
pub fn trouble(err: &ureq::Error) -> Trouble {
    match err {
        ureq::Error::StatusCode(_) => Trouble::Other,
        _ => Trouble::Offline,
    }
}

/// Download `url` into `at`, calling `progress` with the fraction done as it
/// arrives. Written beside and renamed over, so a stopped download is never
/// mistaken for a finished one.
pub fn download(
    url: &str,
    at: &Path,
    mut progress: impl FnMut(Option<f32>),
) -> Result<u64, (Trouble, String)> {
    let mut response = agent()
        .get(url)
        .call()
        .map_err(|err| (trouble(&err), err.to_string()))?;
    let total: Option<u64> = response
        .headers()
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok());
    let partial = at.with_extension("partial");
    if let Some(parent) = at.parent() {
        std::fs::create_dir_all(parent).map_err(|err| (disk(&err), err.to_string()))?;
    }
    let mut file = std::fs::File::create(&partial).map_err(|err| (disk(&err), err.to_string()))?;
    let mut reader = response.body_mut().with_config().limit(u64::MAX).reader();
    let mut buffer = vec![0u8; 1 << 20];
    let mut done = 0u64;
    let mut said = None;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|err| (Trouble::Offline, err.to_string()))?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])
            .map_err(|err| (disk(&err), err.to_string()))?;
        done += read as u64;
        let fraction = total.map(|total| (done as f64 / total.max(1) as f64) as f32);
        // A line per whole percent, not per megabyte.
        let percent = fraction.map(|fraction| (fraction * 100.0) as u32);
        if percent != said || said.is_none() {
            said = percent;
            progress(fraction);
        }
    }
    if total.is_some_and(|total| total != done) {
        let _ = std::fs::remove_file(&partial);
        return Err((Trouble::Offline, "the download stopped short".to_string()));
    }
    file.sync_all()
        .map_err(|err| (disk(&err), err.to_string()))?;
    std::fs::rename(&partial, at).map_err(|err| (disk(&err), err.to_string()))?;
    Ok(done)
}

/// A full disk is a thing the shell says; any other failure to write is not
/// worth naming.
pub fn disk(err: &std::io::Error) -> Trouble {
    if err.raw_os_error() == Some(28) {
        Trouble::NoSpace
    } else {
        Trouble::Other
    }
}
