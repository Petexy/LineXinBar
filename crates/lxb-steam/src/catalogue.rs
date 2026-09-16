//! The account's library, kept so that it is still there when Steam is not.
//!
//! What the shell shows in the Steam column is two halves put together: what
//! the account *owns*, which only Steam can say, and what is *installed*, which
//! is read off this machine's own disk. The second half needs nothing but the
//! disk. The first used to need the network every single time — so a session
//! that came up without one had an empty column, and a machine with thirty
//! games installed on it could not list one of them.
//!
//! That is the wrong way round. Offline is exactly when somebody wants the
//! games that are already on the disk, and it is the one moment the shell used
//! to be least able to help.
//!
//! So the owned list is written down after every successful read, and read back
//! at startup before Steam is so much as reached for. Nothing secret is in it:
//! it is a list of titles the account owns, which is the same list the column
//! draws in front of whoever is sitting there. The credential lives in
//! [`crate::session`] and stays there.
//!
//! Three things make a stale file safe to use. It carries a **version**, and a
//! file written by a shape this build does not know is discarded rather than
//! guessed at. It carries the **SteamID** it belongs to, so signing in as
//! somebody else cannot show them the last person's library. And it carries
//! **when it was read**, so the shell can say how old what it is showing is
//! rather than presenting last week's library as today's.

use std::path::PathBuf;

use crate::library::Game;

/// The shape of the file. Bump it when the record below changes meaning; an
/// older or newer file is then dropped, which costs one catalogue refresh and
/// nothing else.
const VERSION: u32 = 2;

/// How old a catalogue may be and still be worth showing.
///
/// Long, because the thing it guards against is not staleness — a library
/// changes when somebody buys something, which is rare — but a file left behind
/// by a machine nobody has signed into for a year. What is actually shown is
/// always dated on the screen, so the user is never being told last month's
/// library is this minute's.
const KEEPS: std::time::Duration = std::time::Duration::from_secs(60 * 60 * 24 * 30);

/// One title, as the account's own catalogue has it.
///
/// Deliberately *not* [`Game`], which is the two halves already merged. Half of
/// a `Game` is a fact about this machine's disk — whether it is installed, how
/// big it is, how far a download has got — and a fact about the disk that was
/// written down an hour ago is a fact that is wrong now. Those are read fresh
/// every time and only the account's half is kept.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Owned {
    app_id: u32,
    name: String,
    #[serde(default)]
    playtime_minutes: u32,
    #[serde(default)]
    last_played: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    launch: Vec<crate::library::Launch>,
    #[serde(default, skip_serializing_if = "crate::art::Published::is_empty")]
    pictures: crate::art::Published,
}

/// The whole file.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Stored {
    version: u32,
    steam_id: u64,
    /// When this was read from Steam, in seconds since the epoch.
    saved_at: u64,
    games: Vec<Owned>,
}

/// A catalogue read back off the disk.
#[derive(Debug, Clone)]
pub struct Restored {
    /// The account's library, in the same shape a live answer arrives in.
    pub games: Vec<Game>,
    /// When Steam last said so.
    pub read_at: std::time::SystemTime,
}

/// Write down what Steam just said this account owns.
///
/// Best effort throughout. A catalogue that cannot be written costs one
/// network read on the next start and nothing else, so every failure here is a
/// line in the log rather than something a caller has to answer.
pub fn keep(steam_id: u64, games: &[Game]) {
    let Some(path) = path() else {
        return;
    };
    let stored = Stored {
        version: VERSION,
        steam_id,
        saved_at: now(),
        games: games
            .iter()
            .map(|game| Owned {
                app_id: game.app_id,
                name: game.name.clone(),
                playtime_minutes: game.playtime_minutes,
                last_played: game.last_played,
                launch: game.launch.clone(),
                pictures: game.pictures.clone(),
            })
            .collect(),
    };
    if let Err(error) = write(&path, &stored) {
        tracing::debug!(%error, path = %path.display(), "the Steam catalogue was not written down");
        return;
    }
    tracing::debug!(games = stored.games.len(), "wrote down the Steam catalogue");
}

/// Read back what this account owned the last time Steam said.
///
/// `None` for every way of not having one, and they are all ordinary: no file,
/// a file this build does not understand, a file belonging to another account,
/// or one old enough that showing it would be a claim rather than a memory.
pub fn restore(steam_id: u64) -> Option<Restored> {
    let path = path()?;
    let raw = std::fs::read_to_string(&path).ok()?;
    let stored: Stored = serde_json::from_str(&raw)
        .map_err(|error| {
            tracing::info!(%error, "the stored Steam catalogue could not be read; asking Steam instead");
        })
        .ok()?;

    if stored.version != VERSION {
        tracing::info!(
            was = stored.version,
            now = VERSION,
            "the stored Steam catalogue is of another shape"
        );
        return None;
    }
    // Somebody else's library is not a stale version of this one. Showing it
    // for the second it takes Steam to answer would be showing this person
    // somebody else's games, which is worse than showing them nothing.
    if stored.steam_id != steam_id {
        tracing::info!("the stored Steam catalogue belongs to another account");
        return None;
    }

    let read_at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(stored.saved_at);
    if read_at.elapsed().is_ok_and(|since| since > KEEPS) {
        tracing::info!("the stored Steam catalogue is too old to show");
        return None;
    }

    let games = crate::library::owned_from_records(stored.games.into_iter().map(|owned| {
        crate::library::Remembered {
            app_id: owned.app_id,
            name: owned.name,
            playtime_minutes: owned.playtime_minutes,
            last_played: owned.last_played,
            launch: owned.launch,
            pictures: owned.pictures,
        }
    }));
    tracing::info!(
        games = games.len(),
        "restored the Steam catalogue from the disk"
    );
    Some(Restored { games, read_at })
}

/// Throw it away. There is nothing secret in it, and it is still nobody's
/// business once the account it belongs to has gone.
pub fn forget() {
    if let Some(path) = path() {
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("writing"));
    }
}

/// Into place by rename, so a session interrupted mid-write leaves the previous
/// catalogue rather than half of a new one.
fn write(path: &std::path::Path, stored: &Stored) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let scratch = path.with_extension("writing");
    std::fs::write(
        &scratch,
        serde_json::to_vec(stored).map_err(std::io::Error::other)?,
    )?;
    std::fs::rename(&scratch, path)
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or_default()
}

/// `$XDG_CACHE_HOME/lxb/steam-library.json`.
///
/// The cache directory, and that is the whole statement of what this is: losing
/// it costs one network read. The credential is in the *data* directory,
/// because losing that costs somebody a sign-in — see [`crate::session`], whose
/// file this is deliberately not kept beside.
fn path() -> Option<PathBuf> {
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            let home = std::env::var_os("HOME").map(PathBuf::from)?;
            Some(home.join(".cache"))
        })?;
    Some(cache.join("lxb").join("steam-library.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("lxb-catalogue-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("a scratch cache");
        path
    }

    fn a_library() -> Vec<Game> {
        vec![
            Game::invented(504230, "Celeste".to_string(), false),
            Game::invented(220200, "Kerbal Space Program".to_string(), false),
        ]
    }

    /// A catalogue comes back as the library it was, so a session that starts
    /// with no network still has a column.
    #[test]
    fn what_steam_said_survives_a_reboot() {
        let _turn = crate::one_at_a_time_with_the_environment();
        let cache = scratch("round-trip");
        // SAFETY: the environment guard above is what makes this a test that
        // owns the variable for its duration.
        unsafe { std::env::set_var("XDG_CACHE_HOME", &cache) };

        keep(76561198042371721, &a_library());
        let back = restore(76561198042371721).expect("the catalogue");
        assert_eq!(back.games.len(), 2);
        assert_eq!(back.games[0].name, "Celeste");
        // Sorted the way a live answer is, rather than in the order it was
        // written: a restored column and a live one must not be different
        // columns.
        assert!(back.games[0].order_key() <= back.games[1].order_key());
        // And the disk half is *not* restored — it is read fresh every time,
        // because an hour-old fact about this machine's disk is a wrong one.
        assert!(!back.games[0].installed);

        let _ = std::fs::remove_dir_all(&cache);
    }

    /// Somebody else's library is not a stale version of this one.
    #[test]
    fn another_accounts_catalogue_is_not_shown() {
        let _turn = crate::one_at_a_time_with_the_environment();
        let cache = scratch("wrong-account");
        // SAFETY: as above.
        unsafe { std::env::set_var("XDG_CACHE_HOME", &cache) };

        keep(76561198042371721, &a_library());
        assert!(restore(76561198042371721).is_some());
        assert!(
            restore(76561198000000001).is_none(),
            "one account was shown another's games"
        );

        let _ = std::fs::remove_dir_all(&cache);
    }

    /// A file of a shape this build does not know is dropped rather than
    /// guessed at, and so is one that is not a catalogue at all.
    #[test]
    fn a_file_this_build_does_not_understand_is_dropped() {
        let _turn = crate::one_at_a_time_with_the_environment();
        let cache = scratch("version");
        // SAFETY: as above.
        unsafe { std::env::set_var("XDG_CACHE_HOME", &cache) };

        let path = path().expect("a path");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"version":99,"steam_id":1,"saved_at":0,"games":[]}"#,
        )
        .unwrap();
        assert!(restore(1).is_none());

        std::fs::write(&path, "not json at all").unwrap();
        assert!(restore(1).is_none());

        let _ = std::fs::remove_dir_all(&cache);
    }

    /// And one old enough that showing it would be a claim rather than a
    /// memory.
    #[test]
    fn a_catalogue_left_behind_a_year_ago_is_not_this_library() {
        let _turn = crate::one_at_a_time_with_the_environment();
        let cache = scratch("stale");
        // SAFETY: as above.
        unsafe { std::env::set_var("XDG_CACHE_HOME", &cache) };

        keep(7, &a_library());
        let path = path().expect("a path");
        let mut stored: Stored =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        stored.saved_at = now() - KEEPS.as_secs() - 1;
        std::fs::write(&path, serde_json::to_vec(&stored).unwrap()).unwrap();

        assert!(restore(7).is_none());

        let _ = std::fs::remove_dir_all(&cache);
    }
}
