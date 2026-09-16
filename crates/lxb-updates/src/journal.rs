//! Bounded, private transcripts independent of the frequently polled snapshot.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

pub const LIMIT: u64 = 16 * 1024 * 1024;
pub const CHUNK: u64 = 64 * 1024;
const MARKER: &[u8] = b"\n[Output limit reached; further native output was not recorded.]\n";
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Chunk {
    pub job: u64,
    pub offset: u64,
    pub next: u64,
    pub total: u64,
    pub bytes: Vec<u8>,
}
fn path(directory: &Path, job: u64) -> PathBuf {
    directory.join(format!("{job}.log"))
}
pub struct Writer {
    file: File,
    size: u64,
    capped: bool,
}
impl Writer {
    pub fn create(directory: &Path, job: u64) -> Result<Self> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path(directory, job))?;
        Ok(Self {
            file,
            size: 0,
            capped: false,
        })
    }
    pub fn append(&mut self, bytes: &[u8]) -> Result<()> {
        if self.capped {
            return Ok(());
        }
        let left = LIMIT.saturating_sub(self.size + MARKER.len() as u64) as usize;
        let take = bytes.len().min(left);
        self.file.write_all(&bytes[..take])?;
        self.size += take as u64;
        if take < bytes.len() {
            self.file.write_all(MARKER)?;
            self.size += MARKER.len() as u64;
            self.capped = true;
        }
        self.file.sync_data()?;
        Ok(())
    }
}
pub fn read(directory: &Path, job: u64, offset: u64) -> Result<Chunk> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path(directory, job))
        .context("Full output is unavailable for this older update")?;
    let total = file.metadata()?.len();
    if total > LIMIT || offset > total {
        bail!("Invalid output offset or transcript size");
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = vec![];
    file.take(CHUNK).read_to_end(&mut bytes)?;
    Ok(Chunk {
        job,
        offset,
        next: offset + bytes.len() as u64,
        total,
        bytes,
    })
}
pub fn rotate(directory: &Path, keep: &[u64]) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(job) = name
            .strip_suffix(".log")
            .and_then(|s| s.parse::<u64>().ok())
        else {
            continue;
        };
        if !keep.contains(&job) {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn full_log_survives_chunks_caps_and_rotation() {
        let dir = std::env::temp_dir().join(format!("lxb-log-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        let mut log = Writer::create(&dir, 1).unwrap();
        let output = "line of native output\n".repeat(7000);
        log.append(output.as_bytes()).unwrap();
        let mut got = vec![];
        let mut offset = 0;
        loop {
            let part = read(&dir, 1, offset).unwrap();
            got.extend(part.bytes);
            offset = part.next;
            if offset == part.total {
                break;
            }
        }
        assert_eq!(got, output.as_bytes());
        log.append(&vec![b'x'; LIMIT as usize]).unwrap();
        let tail = read(&dir, 1, LIMIT - MARKER.len() as u64).unwrap();
        assert_eq!(tail.bytes, MARKER);
        for job in 2..=4 {
            Writer::create(&dir, job).unwrap();
        }
        rotate(&dir, &[2, 3, 4]).unwrap();
        assert!(!path(&dir, 1).exists());
        assert!(path(&dir, 4).exists());
        assert!(read(&dir, 4, 1).is_err());
        drop(log);
        fs::remove_dir_all(dir).unwrap();
    }
}
