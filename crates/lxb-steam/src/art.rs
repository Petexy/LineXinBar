//! The pictures Steam keeps of a game: the cover it is recognised by, and the
//! picture that stands behind it.
//!
//! Both are Valve's, both are named the same everywhere, and both can be had
//! from two places — which is the whole of this module. A machine with the
//! Steam client on it has already downloaded them for its own library screen,
//! so the first place to look is that cache: it costs a `stat`, it is already
//! the right picture, and it works with no network at all. Only a game the
//! client has never shown has to be asked for over the wire.
//!
//! ## What the pieces are
//!
//! Valve's store artwork has a dozen shapes. Three of them matter to a shell
//! built like a cross media bar:
//!
//! * the **cover** — `library_600x900`, the portrait capsule, which is the
//!   picture a person recognises a game by and the only one drawn at the size
//!   of a row;
//! * the **hero** — `library_hero`, the wide picture Steam puts behind a
//!   game's own page, which is the one thing in the catalogue big enough to
//!   stand behind a whole display; and
//! * the **logo** — `logo.png`, the game's title drawn as its own artwork on
//!   a transparent ground, which is what a game is called when the calling is
//!   the only thing on the screen.
//!
//! Nothing else is fetched. A header and a blurred hero are in the same cache
//! and neither is drawn by this shell, and a picture that is never drawn is
//! half a megabyte of somebody's disk and a request to Valve for nothing.
//!
//! ## Why a missing picture is not a failure
//!
//! Not every app has every piece — a very old title, a tool, a demo. Steam
//! answers 404 for those, which is a *fact about the game* and not a fault:
//! the shell draws what it drew before this existed and never asks again. That
//! is why [`Missing`] tells the two apart. Something that could not be reached
//! is worth trying later; something Steam says is not there never is.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// Steam's content network, as this crate reaches it.
///
/// The same host the client itself pulls its library pictures from. Plain
/// HTTPS with no account and no key: store artwork is public, and asking for
/// it as an anonymous client is what every page on the store already does.
const CDN: &str = "https://cdn.cloudflare.steamstatic.com/steam/apps";

/// How long one picture is given to arrive.
///
/// A hero is around half a megabyte, which is nothing on a working line and
/// forever on one that has gone away. Short enough that a worker is not held
/// for a minute by a game nobody is looking at any more.
const TIMEOUT: Duration = Duration::from_secs(20);

/// One of the pictures Steam holds for a game.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Piece {
    /// The portrait capsule, 600 × 900: what a game is recognised by.
    Cover,
    /// The wide picture from the game's own page, 1920 × 620.
    Hero,
    /// The game's title as artwork, on a transparent ground.
    ///
    /// No fixed size, unlike the two above: Valve stores whatever shape the
    /// wordmark is, up to 640 across. A title set in one line comes back a
    /// tenth as tall as a stacked one, so nothing may assume a shape for it.
    Logo,
}

impl Piece {
    /// Valve's file name for it, which is the same in the client's cache and
    /// on the content network — the cache is a mirror of that path, not a
    /// format of its own.
    pub fn file_name(self) -> &'static str {
        match self {
            Piece::Cover => "library_600x900.jpg",
            Piece::Hero => "library_hero.jpg",
            Piece::Logo => "logo.png",
        }
    }
}

/// Why a picture did not arrive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Missing {
    /// Steam has no such picture for this game. A permanent answer: the caller
    /// must stop asking, because it will be the same tomorrow.
    NotThere,
    /// It could not be asked for, or the answer could not be read. Worth
    /// asking again — this is a wire that is down, not a game without a cover.
    Unreachable(String),
}

impl Missing {
    /// Whether asking again could ever give a different answer.
    pub fn worth_retrying(&self) -> bool {
        matches!(self, Missing::Unreachable(_))
    }
}

/// Where Valve's own client has already put this picture, if it has.
///
/// Two layouts, because Steam changed its mind: the current client keeps a
/// directory per app, older ones flattened the app id into the file name. Both
/// are one `is_file` away and a machine only ever has one of them, so both are
/// looked for rather than guessed at from a version nobody can see.
///
/// The file is read where it lies and never written to. It is the client's
/// cache, and a shell that tidied up after Valve would be deleting pictures
/// out from under a program that is still running.
pub fn in_the_client_cache(app_id: u32, piece: Piece) -> Option<PathBuf> {
    let root = crate::library::root()?;
    in_cache(&root.join("appcache").join("librarycache"), app_id, piece)
}

/// The same, against a cache directory that is handed over — which is what
/// makes both layouts testable without a Steam installation to point at.
fn in_cache(cache: &Path, app_id: u32, piece: Piece) -> Option<PathBuf> {
    let file = piece.file_name();
    [
        cache.join(app_id.to_string()).join(file),
        cache.join(format!("{app_id}_{file}")),
    ]
    .into_iter()
    .find(|path| path.is_file())
}

/// Steam's content network, as one agent that can be kept.
///
/// Held rather than built per call: a library being scrolled asks for a dozen
/// covers in a second, and a fresh TLS session for each of them is most of the
/// cost of the picture.
pub struct Cdn {
    agent: ureq::Agent,
}

impl Default for Cdn {
    fn default() -> Self {
        Self::new()
    }
}

impl Cdn {
    pub fn new() -> Cdn {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            // A picture that is not there answers 404, and that is an answer
            // this caller acts on rather than an error to be raised.
            .http_status_as_error(false)
            .timeout_global(Some(TIMEOUT))
            .user_agent(concat!("LineXinBar/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Cdn { agent }
    }

    /// Fetch one picture, as the bytes Steam stores it as.
    ///
    /// Nothing is decoded here and nothing is written to the disk: this crate
    /// has no image codec in it and no opinion about where a shell keeps what
    /// it has fetched.
    pub fn fetch(&self, app_id: u32, piece: Piece) -> Result<Vec<u8>, Missing> {
        let url = format!("{CDN}/{app_id}/{}", piece.file_name());
        let response = self
            .agent
            .get(&url)
            .call()
            .map_err(|err| Missing::Unreachable(err.to_string()))?;

        let status = response.status().as_u16();
        if status == 404 || status == 403 {
            // 403 as well as 404: the network answers a path it has never
            // heard of either way depending on which edge is asked, and both
            // mean the same thing about the game.
            return Err(Missing::NotThere);
        }
        if !(200..300).contains(&status) {
            return Err(Missing::Unreachable(format!("Steam answered {status}")));
        }

        let bytes = response
            .into_body()
            .read_to_vec()
            .map_err(|err| Missing::Unreachable(err.to_string()))?;
        if bytes.is_empty() {
            return Err(Missing::NotThere);
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The names are Valve's, and getting one wrong is a 404 for every game in
    /// somebody's library at once — a library that draws no pictures at all,
    /// with nothing in the log to say why.
    #[test]
    fn the_pieces_are_named_as_steam_names_them() {
        assert_eq!(Piece::Cover.file_name(), "library_600x900.jpg");
        assert_eq!(Piece::Hero.file_name(), "library_hero.jpg");
        assert_eq!(Piece::Logo.file_name(), "logo.png");
    }

    /// The two kinds of absence are not the same thing to do about, which is
    /// the only reason they are separate values.
    #[test]
    fn only_an_unreachable_picture_is_worth_asking_for_twice() {
        assert!(!Missing::NotThere.worth_retrying());
        assert!(Missing::Unreachable("no route".to_string()).worth_retrying());
    }

    /// Both of Valve's cache layouts are found, and a game the client has
    /// never shown is found in neither — which is the answer that sends the
    /// caller to the network rather than to a file that is not there.
    #[test]
    fn either_of_the_client_cache_layouts_answers() {
        let scratch = std::env::temp_dir().join(format!("lxb-art-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(scratch.join("504230")).expect("a scratch directory");
        std::fs::write(scratch.join("504230").join("library_600x900.jpg"), [0xff])
            .expect("the current layout");
        std::fs::write(scratch.join("400_library_hero.jpg"), [0xff]).expect("the older layout");

        assert_eq!(
            in_cache(&scratch, 504230, Piece::Cover),
            Some(scratch.join("504230").join("library_600x900.jpg"))
        );
        assert_eq!(
            in_cache(&scratch, 400, Piece::Hero),
            Some(scratch.join("400_library_hero.jpg"))
        );
        assert_eq!(in_cache(&scratch, 504230, Piece::Hero), None);
        assert_eq!(in_cache(&scratch, 999999, Piece::Cover), None);
        let _ = std::fs::remove_dir_all(&scratch);
    }
}
