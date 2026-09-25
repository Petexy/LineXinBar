//! Which Steam this machine has, and where everything it owns is.
//!
//! There are two Steams on Linux — the native client and Valve's Flatpak — and
//! a machine can have the leavings of both. They keep their libraries, their
//! logs, their pipe and their artwork cache in entirely different places, so
//! *which one this session is driving* has to be a single answer that
//! everything else is read from.
//!
//! It was not one. The client was controlled through
//! [`crate::client::Options`], which is asked of the client that was actually
//! found and is right; the installed library and the artwork cache each went
//! looking on their own, took the first Steam-shaped directory they saw, and
//! preferred the native paths. On a machine where the native client had been
//! *removed* — its `steam` no longer on `PATH`, its `~/.local/share/Steam`
//! still on the disk, as an uninstall routinely leaves it — that is a Flatpak
//! being started, signed in and asked for games while the library, the download
//! progress, the uninstall watch and the covers were all read out of the dead
//! native directory. Every row would be wrong, every download would appear
//! never to start, and nothing anywhere would say why.
//!
//! So: one [`Backend`], resolved from the client that was found, and handed to
//! everything that reads a Steam directory.

use std::path::{Path, PathBuf};

use crate::client::{Options, Where};

/// The Steam this session is driving, and where it keeps things.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backend {
    /// How it is started, or `None` on a machine whose client has been removed
    /// and whose games are still on the disk.
    ///
    /// A library with no client is a real state and the shell says so plainly:
    /// the games are listed, and nothing in the column can be played or fetched
    /// until there is something to play them with.
    pub client: Option<Where>,
    /// Its directories — the root it unpacks itself into, and the small home
    /// beside it that holds the pipe and the registry.
    pub options: Options,
}

impl Backend {
    /// The one this session drives, or `None` on a machine with neither a
    /// client nor anything a client left behind.
    ///
    /// Cheap: a `PATH` walk, a Flatpak deployment check and a handful of
    /// `is_dir` calls. Deliberately resolved again rather than remembered,
    /// wherever it is asked often enough to matter — see
    /// [`crate::library::installed_for`], which is asked every ten seconds.
    /// Steam can be installed or removed while the session runs, and an answer
    /// worked out once at startup is one that goes quietly wrong.
    pub fn chosen() -> Option<Backend> {
        let home = PathBuf::from(std::env::var_os("HOME")?);
        Backend::in_home(&home)
    }

    /// The same, below a given home directory, so a test can put a whole
    /// machine's worth of Steam somewhere harmless.
    pub(crate) fn in_home(home: &Path) -> Option<Backend> {
        // The client that was found decides, and this is the whole point of
        // the module: what is *controlled* and what is *read* must be the same
        // Steam.
        if let Some(client) = Where::find() {
            let options = Options::in_home(&client, home);
            return Some(Backend {
                client: Some(client),
                options,
            });
        }

        // No client to drive. The games may still be there, and a machine that
        // has had Steam taken off it is exactly the machine where the shell
        // should be able to say which games it used to be able to play. This
        // is the only place the old guess survives, and here it is not a guess:
        // there is no client for it to disagree with.
        let leftovers = Options::every_layout_in(home)
            .into_iter()
            .find(|options| crate::library::looks_like_a_root(&options.root))?;
        Some(Backend {
            client: None,
            options: leftovers,
        })
    }

    /// Where this Steam is installed.
    pub fn root(&self) -> &Path {
        &self.options.root
    }

    /// Every Steam library belonging to it, this one first.
    pub fn libraries(&self) -> Vec<PathBuf> {
        crate::library::libraries_below(self.root())
    }

    /// Every library it lists, the ones not there today included, with the
    /// names they were given. See [`crate::library::listed_below`].
    pub fn listed_libraries(&self) -> Vec<crate::library::Listed> {
        crate::library::listed_below(self.root())
    }

    /// Where this client keeps the pictures it has already fetched.
    ///
    /// Read where it lies and never written to: it is Valve's cache, and a
    /// shell that tidied up after it would be deleting pictures out from under
    /// a program that is still running.
    pub fn client_art_cache(&self) -> PathBuf {
        self.root().join("appcache").join("librarycache")
    }

    /// Where this client keeps the icons it has already fetched.
    ///
    /// Not the same directory as the store artwork, and not a subdirectory of
    /// it: a flat `steam/games` of `{hash}.ico` — and, for the Linux client's
    /// own purposes, `{hash}.zip`. Read where it lies and never written to, on
    /// the same terms. See [`crate::art::in_the_icon_cache`].
    pub fn client_icon_cache(&self) -> PathBuf {
        self.root().join("steam").join("games")
    }
}

/// Say which Steam is being driven, and complain where the answer is not the
/// only one it could have been.
///
/// A machine with both clients installed is a machine where this shell has
/// picked one and the user may have meant the other, and nothing on the screen
/// says which. There is no honest way to guess — a native client on `PATH` is
/// as likely to be the one somebody uses as a Flatpak they installed last week
/// — so what this does is make the choice *findable* rather than silent, which
/// is what a bug report about "the wrong library" needs and did not have.
///
/// Logged once per call rather than remembered, because it is called from the
/// places that already walk the disk.
pub fn say_which_steam(backend: &Backend) {
    let all = Where::all();
    match backend.client.as_ref() {
        Some(client) if all.len() > 1 => tracing::info!(
            using = ?client,
            found = ?all,
            root = %backend.root().display(),
            "this machine has more than one Steam; driving and reading this one"
        ),
        Some(client) => tracing::debug!(
            using = ?client,
            root = %backend.root().display(),
            "driving and reading this Steam"
        ),
        None => tracing::info!(
            root = %backend.root().display(),
            "no Steam client on this machine; reading the library one left behind"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch home of this test's own.
    fn scratch(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("lxb-backend-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("a scratch home");
        path
    }

    /// Make a directory look like a Steam that has been run.
    fn run_steam_in(root: &Path) {
        std::fs::create_dir_all(root.join("steamapps")).unwrap();
        std::fs::create_dir_all(root.join("config")).unwrap();
    }

    /// The bug this module exists for: a native Steam that has been removed
    /// leaves its directory behind, and that directory used to win.
    ///
    /// `Where::find` had already answered — there is no native client here, so
    /// the Flatpak is what gets started, signed in and asked for games — and
    /// the library, the download progress and the covers went looking again on
    /// their own and found the dead one. Nothing disagreed out loud; the rows
    /// were simply the wrong machine's.
    #[test]
    fn a_removed_native_steam_does_not_shadow_the_flatpak() {
        let home = scratch("shadow");
        let native = crate::library::unpacks_into(&home);
        let flatpak = crate::library::unpacks_into(&crate::library::flatpak_home(&home));
        run_steam_in(&native);
        run_steam_in(&flatpak);

        // What the disk alone says, which is what the old guess asked, and it
        // is the native leftovers every time.
        let leftovers = Options::every_layout_in(&home)
            .into_iter()
            .find(|options| crate::library::looks_like_a_root(&options.root))
            .unwrap();
        assert_eq!(leftovers.root, native);

        // And what the *client* says, which is the answer that counts. Asked
        // through the same path `chosen` takes for a client that was found.
        let driven = Options::in_home(&Where::Flatpak, &home);
        assert_eq!(driven.root, flatpak);
        assert_ne!(
            driven.root, leftovers.root,
            "the test is not testing anything"
        );

        let _ = std::fs::remove_dir_all(&home);
    }

    /// With no client at all the leavings are all there is, and a library that
    /// cannot be played is still a library worth listing.
    #[test]
    fn a_machine_with_no_client_reads_what_is_left_of_one() {
        let home = scratch("leftovers");
        let native = crate::library::unpacks_into(&home);
        run_steam_in(&native);

        let backend = Backend {
            client: None,
            options: Options::every_layout_in(&home)
                .into_iter()
                .find(|options| crate::library::looks_like_a_root(&options.root))
                .expect("the leftovers"),
        };
        assert_eq!(backend.root(), native);
        assert_eq!(
            backend.client_art_cache(),
            native.join("appcache").join("librarycache")
        );

        let _ = std::fs::remove_dir_all(&home);
    }

    /// And a home with nothing Steam-shaped in it at all is no backend, rather
    /// than a backend pointing at a directory that does not exist.
    #[test]
    fn nothing_on_the_disk_is_no_backend() {
        let home = scratch("empty");
        assert!(Options::every_layout_in(&home)
            .into_iter()
            .all(|options| !crate::library::looks_like_a_root(&options.root)));
        let _ = std::fs::remove_dir_all(&home);
    }
}
