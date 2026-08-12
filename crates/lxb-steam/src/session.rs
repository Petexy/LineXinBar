//! What is kept between one session and the next, and what is not.
//!
//! Kept: the refresh token, whose account it belongs to, the account name to
//! greet the user by, the machine token Steam gave to stop asking for an email
//! code on this machine every time, and a stable non-secret identity for CM.
//!
//! Not kept, ever: the password. There is nowhere in this crate it could be
//! written to — it exists as [`crate::Password`] for as long as it takes to
//! encrypt it and is overwritten when that is done — and there is nothing it
//! could be written for, because the refresh token is what signs in to Steam's
//! Connection Manager from now on.
//!
//! The file is written with only the user able to read it, into the same place
//! the rest of this session's own state lives. It is a credential for a Steam
//! account, and a machine where another user can read it is a machine where
//! they can play as that account.

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;

/// A signed-in account, as it survives a reboot.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Stored {
    /// The account name, which is what the user signs in with and what the
    /// shell greets them by.
    pub account: String,
    pub steam_id: u64,
    pub refresh_token: String,
    /// Steam's machine token for this device, if it gave one. Skips the email
    /// code on the next sign-in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard_data: Option<String>,
    /// A random, stable identity for this installation's CM logons. It is not
    /// secret, but keeping it stable stops every shell restart looking like a
    /// different computer to Steam.
    #[serde(default = "new_machine_id")]
    pub machine_id: String,
}

impl std::fmt::Debug for Stored {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stored")
            .field("account", &self.account)
            .field("steam_id", &self.steam_id)
            .field("refresh_token", &"[redacted]")
            .field(
                "guard_data",
                &self.guard_data.as_ref().map(|_| "[redacted]"),
            )
            .field("machine_id", &self.machine_id)
            .finish()
    }
}

impl Stored {
    pub fn granted(
        account: String,
        steam_id: u64,
        refresh_token: String,
        guard_data: Option<String>,
    ) -> Stored {
        let machine_id = Stored::load()
            .filter(|stored| stored.steam_id == steam_id)
            .map(|stored| stored.machine_id)
            .unwrap_or_else(new_machine_id);
        Stored {
            account,
            steam_id,
            refresh_token,
            guard_data,
            machine_id,
        }
    }

    /// Read the stored session, if there is one and it still looks like one.
    ///
    /// A file that cannot be parsed is treated as no session rather than as an
    /// error: the only thing to do about it is sign in again, which is what a
    /// missing session already leads to.
    pub fn load() -> Option<Stored> {
        let path = path()?;
        let raw = std::fs::read_to_string(&path).ok()?;
        match serde_json::from_str::<Stored>(&raw) {
            Ok(stored) if !stored.refresh_token.is_empty() && stored.steam_id != 0 => Some(stored),
            Ok(_) => {
                tracing::warn!(path = %path.display(), "the stored Steam session is incomplete");
                None
            }
            Err(err) => {
                tracing::warn!(path = %path.display(), %err, "the stored Steam session could not be read");
                None
            }
        }
    }

    /// Write it down, readable by this user and nobody else.
    ///
    /// Written to a neighbouring file and renamed over the old one, so a
    /// session interrupted mid-write leaves the previous token intact rather
    /// than half of a new one. The mode is set on the way in rather than
    /// afterwards: a file that is briefly world-readable is a file that was
    /// world-readable.
    pub fn save(&self) -> std::io::Result<()> {
        let path = path()
            .ok_or_else(|| std::io::Error::other("no directory to keep a Steam session in"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
            // The directory too: a token in a private file inside a directory
            // anybody may list is still a token whose existence is public.
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
        }

        let raw = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        let scratch = path.with_extension("writing");
        {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&scratch)?;
            file.write_all(&raw)?;
            file.sync_all()?;
        }
        std::fs::rename(&scratch, &path)
    }

    /// Forget it. Says nothing about whether there was one: signing out of a
    /// session that had already gone is not a failure.
    pub fn forget() {
        if let Some(path) = path() {
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_file(path.with_extension("writing"));
        }
    }
}

/// `$XDG_DATA_HOME/lxb/steam.json`, beside the rest of what this session keeps.
///
/// The data directory rather than the config one: this is not something the
/// user edits, and it is not something they would want copied along with their
/// settings to another machine — it authorises *this* machine.
fn path() -> Option<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            let home = std::env::var_os("HOME").map(PathBuf::from)?;
            Some(home.join(".local").join("share"))
        })?;
    Some(data.join("lxb").join("steam.json"))
}

fn new_machine_id() -> String {
    let random: [u8; 32] = rand::random();
    let mut id = String::with_capacity(random.len() * 2);
    use std::fmt::Write as _;
    for byte in random {
        let _ = write!(&mut id, "{byte:02x}");
    }
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A session written and read back is the same session, and the file it
    /// went into cannot be read by anybody else.
    ///
    /// `XDG_DATA_HOME` is moved for the duration, which is why this is one
    /// test rather than three: the variable is process-wide, and three tests
    /// setting it would race each other. For the same reason it takes its turn
    /// with every other test that moves the environment.
    #[test]
    fn a_session_survives_being_written_down_and_nobody_else_can_read_it() {
        let _turn = crate::one_at_a_time_with_the_environment();
        let root = std::env::temp_dir().join(format!("lxb-steam-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        // SAFETY: single-threaded test, and the variable is put back below.
        unsafe { std::env::set_var("XDG_DATA_HOME", &root) };

        assert!(Stored::load().is_none(), "nothing has been signed in yet");

        let stored = Stored {
            account: "someone".to_string(),
            steam_id: 76561197960287930,
            refresh_token: "a.b.c".to_string(),
            guard_data: Some("machine".to_string()),
            machine_id: new_machine_id(),
        };
        stored.save().expect("a scratch directory is writable");

        let path = path().expect("a path under the scratch directory");
        let mode = std::fs::metadata(&path)
            .expect("it was written")
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "another user can read the token: {mode:o}");

        let read = Stored::load().expect("what was just written");
        assert_eq!(read.account, stored.account);
        assert_eq!(read.steam_id, stored.steam_id);
        assert_eq!(read.refresh_token, stored.refresh_token);
        assert_eq!(read.guard_data, stored.guard_data);
        assert_eq!(read.machine_id, stored.machine_id);

        // A session with no token in it is not a session, however well-formed
        // the file is.
        std::fs::write(
            &path,
            br#"{"account":"someone","steam_id":0,"refresh_token":""}"#,
        )
        .expect("writable");
        assert!(Stored::load().is_none());

        // Nor is a file that is not JSON at all, and it is not an error
        // either: the answer to both is to sign in again.
        std::fs::write(&path, b"not json").expect("writable");
        assert!(Stored::load().is_none());

        Stored::forget();
        assert!(!path.exists());
        assert!(Stored::load().is_none());

        unsafe { std::env::remove_var("XDG_DATA_HOME") };
        let _ = std::fs::remove_dir_all(&root);
    }
}
