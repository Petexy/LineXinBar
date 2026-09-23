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
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
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
            //
            // Asked of the directory the write actually lands in, which is why
            // the link is followed rather than stopped at. What has to be true
            // is that *this user* owns what is written into; a link is not
            // that, it is a name for it. Somebody else's directory reached
            // through a link somebody else planted still fails here, which is
            // the case this refuses — and a data directory the user moved to
            // another disk and left behind as a link, which is an ordinary
            // thing to have done, goes on working.
            let metadata = std::fs::metadata(parent)?;
            if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
                return Err(std::io::Error::other(
                    "the Steam session directory is not this user's own",
                ));
            }
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        }

        let raw = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        write_privately(&path, &raw)
    }

    /// Forget it. Says nothing about whether there was one: signing out of a
    /// session that had already gone is not a failure.
    ///
    /// Every copy of it, including the ones [`Stored::save`] does not get to
    /// clean up itself. A save that is killed between the open and the rename
    /// leaves its scratch file behind with the whole token in it, and a
    /// sign-out that left one of those on the disk would be a sign-out that
    /// did not sign out. They are swept by name rather than remembered,
    /// because the process that made one is by then gone.
    pub fn forget() {
        if let Some(path) = path() {
            let _ = std::fs::remove_file(&path);
            // What a version before the scratch name was randomised wrote.
            let _ = std::fs::remove_file(path.with_extension("writing"));
            for scratch in scratches(&path) {
                let _ = std::fs::remove_file(scratch);
            }
        }
        // Including the status it was owed. Signing out and in as somebody else
        // must not hand the next account a status this one chose.
        Owed::forget();
    }
}

/// A status chosen at this shell that no client on this machine has worn yet.
///
/// **Kept apart from [`Stored`] on purpose, in its own file.** That one is a
/// credential and is rewritten whenever Steam hands this session a new token;
/// a status folded into it would be a status silently dropped by the next
/// refresh, which is the sort of loss that only shows up on somebody else's
/// machine a week later.
///
/// It is written down at all because the gap it covers outlives the shell. A
/// status chosen with Valve's client shut down has nowhere to go at the time —
/// see [`crate::client::recorded_status`], which is where a delivered one is
/// read back from — and if the shell is restarted before the client is ever
/// started, an unwritten choice is simply gone, and the client comes up wearing
/// what it last remembered. The account it belongs to is kept beside it so that
/// signing in as somebody else does not inherit a stranger's status.
///
/// It lasts exactly as long as it is undelivered. The moment a client's own
/// record agrees with it, this file is removed and the client's record is the
/// status from then on — which is what keeps the shell from arguing with
/// somebody who changes their status in Valve's window.
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct Owed {
    pub steam_id: u64,
    /// Steam's own number for the state, rather than this crate's enum: a file
    /// on the disk outlives the shape of a Rust type, and the number is the
    /// thing Steam and Valve's client both already speak.
    pub status: u32,
}

impl Owed {
    /// What is owed to a client, if it is owed to *this* account.
    pub fn load(steam_id: u64) -> Option<Owed> {
        let raw = std::fs::read_to_string(owed_path()?).ok()?;
        serde_json::from_str::<Owed>(&raw)
            .ok()
            .filter(|owed| owed.steam_id == steam_id)
    }

    /// Write it down. A status that cannot be written is still the status this
    /// session announces and still the one a running client is handed, so a
    /// failure here loses a restart's worth of memory and nothing else.
    pub fn save(&self) {
        let Some(path) = owed_path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
        }
        match serde_json::to_vec_pretty(self) {
            // Through [`write_privately`] rather than `fs::write`, which is
            // what this used to be. Not because a status is a secret — it is
            // one number and an account id — but because `fs::write` opens the
            // name it is given and follows it wherever it leads, and this name
            // sits in the same directory as the token beside it. A link
            // planted here is a way to have this session write a file of
            // somebody else's choosing; a rename over the name is not.
            Ok(raw) => {
                if let Err(error) = write_privately(&path, &raw) {
                    tracing::warn!(%error, path = %path.display(), "the chosen Steam status could not be written down");
                }
            }
            Err(error) => {
                tracing::warn!(%error, "the chosen Steam status could not be written down")
            }
        }
    }

    /// Nothing is owed any more: it landed, or the account changed.
    pub fn forget() {
        if let Some(path) = owed_path() {
            let _ = std::fs::remove_file(&path);
            for scratch in scratches(&path) {
                let _ = std::fs::remove_file(scratch);
            }
        }
    }
}

/// Write `raw` where nobody but this user can read it, and leave either the
/// whole of it or what was there before.
///
/// Three things at once, and each of them is why this is one function rather
/// than a `fs::write` at either call site.
///
/// The mode is set on the way *in*, because a file that is briefly
/// world-readable is a file that was world-readable — `set_permissions`
/// afterwards is always too late.
///
/// The scratch name is random and the open refuses to reuse one. A fixed
/// neighbouring name is a name anything else on this machine can work out and
/// get to first, and `create`+`truncate` would then follow whatever it found
/// there; `create_new` fails on a name that is taken, link or not.
///
/// And the last step is a rename, which replaces the destination *name* rather
/// than writing through it. That is what makes an interrupted write leave the
/// previous file intact, and it is also what a symbolic link at the
/// destination cannot turn into a write somewhere else.
fn write_privately(path: &std::path::Path, raw: &[u8]) -> std::io::Result<()> {
    let scratch = path.with_extension(format!("writing-{:032x}", rand::random::<u128>()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&scratch)?;
    let wrote = (|| {
        file.write_all(raw)?;
        file.sync_all()?;
        std::fs::rename(&scratch, path)
    })();
    if wrote.is_err() {
        let _ = std::fs::remove_file(&scratch);
    }
    wrote
}

/// Every scratch file [`write_privately`] may have left beside `path`.
///
/// Recognised by the shape of the name rather than remembered, because the
/// process that made one is by then gone: it is the destination's own stem and
/// then `.writing-`, which nothing else in that directory is called. A
/// directory that cannot be read answers the same as an empty one — the caller
/// is removing things, and has nothing to do in either case.
fn scratches(path: &std::path::Path) -> Vec<PathBuf> {
    let (Some(parent), Some(stem)) = (path.parent(), path.file_stem()) else {
        return Vec::new();
    };
    let prefix = format!("{}.writing-", stem.to_string_lossy());
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
        .map(|entry| entry.path())
        .collect()
}

/// `$XDG_DATA_HOME/lxb/steam-status.json`, beside the session it belongs to.
fn owed_path() -> Option<PathBuf> {
    Some(path()?.with_file_name("steam-status.json"))
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
        let path = path().expect("a path under the scratch directory");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let victim = root.join("unrelated-file");
        std::fs::write(&victim, b"must survive").unwrap();
        std::os::unix::fs::symlink(&victim, path.with_extension("writing")).unwrap();
        stored.save().expect("a scratch directory is writable");
        assert_eq!(std::fs::read(&victim).unwrap(), b"must survive");

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

        // And what is owed to Valve's client, which lives beside it in its own
        // file so that a token refresh rewriting the one above cannot drop it.
        stored.save().expect("writable");
        assert!(
            Owed::load(stored.steam_id).is_none(),
            "nothing has been chosen yet"
        );
        Owed {
            steam_id: stored.steam_id,
            status: 7,
        }
        .save();
        assert_eq!(Owed::load(stored.steam_id).map(|owed| owed.status), Some(7));
        // Not this account's, so not this account's status. Signing in as
        // somebody else must not inherit what the last person chose.
        assert!(Owed::load(stored.steam_id + 1).is_none());

        // The status lands *over* its name rather than through it. This used
        // to be an `fs::write`, which opens what the name leads to, so a link
        // planted here was a way to have this session write a file of
        // somebody else's choosing — the token's own neighbour, in the one
        // directory this session keeps private things in.
        let owed = owed_path().expect("a path beside the session");
        Owed::forget();
        std::os::unix::fs::symlink(&victim, &owed).unwrap();
        Owed {
            steam_id: stored.steam_id,
            status: 3,
        }
        .save();
        assert_eq!(std::fs::read(&victim).unwrap(), b"must survive");
        assert_eq!(Owed::load(stored.steam_id).map(|owed| owed.status), Some(3));

        // A save killed between the open and the rename leaves its scratch
        // file behind with the whole token in it. A sign-out that left one of
        // those on the disk would be a sign-out that did not sign out.
        let left_behind = path.with_extension("writing-0123456789abcdef");
        std::fs::write(&left_behind, br#"{"refresh_token":"a.b.c"}"#).unwrap();
        let owed_left_behind = owed.with_extension("writing-fedcba9876543210");
        std::fs::write(&owed_left_behind, b"{}").unwrap();

        Stored::forget();
        assert!(!path.exists());
        assert!(Stored::load().is_none());
        assert!(
            Owed::load(stored.steam_id).is_none(),
            "signing out left a status behind"
        );
        assert!(
            !left_behind.exists(),
            "signing out left a copy of the token behind"
        );
        assert!(!owed_left_behind.exists());

        // A data directory moved to another disk and left behind as a link is
        // an ordinary thing for somebody to have done. What has to be this
        // user's own is the directory the write lands in, which is what the
        // link leads to — not the link.
        let elsewhere = root.join("on-another-disk");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let parent = path.parent().unwrap();
        std::fs::remove_dir_all(parent).unwrap();
        std::os::unix::fs::symlink(&elsewhere, parent).unwrap();
        stored
            .save()
            .expect("a linked session directory is still this user's own");
        assert!(
            elsewhere.join("steam.json").exists(),
            "the session went somewhere other than through the link"
        );
        assert_eq!(
            Stored::load().map(|read| read.refresh_token),
            Some(stored.refresh_token.clone())
        );

        unsafe { std::env::remove_var("XDG_DATA_HOME") };
        let _ = std::fs::remove_dir_all(&root);
    }
}
