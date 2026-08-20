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
//! built like the lattice:
//!
//! * the **cover** — the portrait capsule, which is the picture a person
//!   recognises a game by and the only one drawn at the size of a row. Named
//!   `library_600x900.jpg` or, for a game published recently,
//!   `library_capsule.jpg`;
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
//! ## Where a picture is, and why it cannot be guessed
//!
//! A picture's *path* is whatever the app's own record says it is, and since
//! store artwork became content-addressed that is usually a directory named
//! after the file's contents: `28dbb24430c7…/library_600x900.jpg` rather than
//! `library_600x900.jpg`. Both forms are current, and they are mixed within one
//! game: an old title that had a cover before the change keeps a bare path for
//! it and may still have a hashed logo.
//!
//! The *name* is not fixed either. A capsule published lately is
//! `library_capsule.jpg`, and a third of the games in a library that has been
//! added to for years are of that sort. So there is nothing about a game that
//! can be turned into where its cover is.
//!
//! The path is therefore carried rather than assembled, as [`Published`], out of
//! the same PICS record the library itself is read from. Asking by name for a
//! picture that was published under a hash finds nothing in either place: the
//! content network answers 404, and the client's cache — which mirrors these
//! paths exactly — has it one directory deeper, under a name the asking may not
//! even have. That is what leaves a recent game as a row with no cover while
//! every older one in the same library has one.
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

/// Where Steam says a game's pictures are, as its PICS record spells it.
///
/// Re-exported because [`Published`] is made from one, and a crate that holds
/// this shell's pictures should not have to depend on the protocol crate to
/// name the thing it converts.
pub use steam_cm_protocol::pics::LibraryArt;

/// Steam's content network, as this crate reaches it.
///
/// The same host the client itself pulls its library pictures from. Plain
/// HTTPS with no account and no key: store artwork is public, and asking for
/// it as an anonymous client is what every page on the store already does.
///
/// Only for a picture whose published path is not known — see [`ASSETS`]. This
/// path is Valve's older one and it answers for artwork that was published
/// before store assets were content-addressed, which is most of a long-standing
/// library and none of a game released since.
const CDN: &str = "https://cdn.cloudflare.steamstatic.com/steam/apps";

/// Where store artwork is served by the path the app publishes it under.
///
/// `shared.steamstatic.com` and not `shared.cloudflare.steamstatic.com`, which
/// redirects here: one hop rather than two, for a host that is asked for a
/// dozen pictures whenever somebody scrolls a library.
///
/// Every published path answers here, hashed or bare, which is why a game whose
/// path is known is fetched from this host and nothing else.
const ASSETS: &str = "https://shared.steamstatic.com/store_item_assets/steam/apps";

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
    /// The name Valve's older content network serves it under, and the one this
    /// shell files it under when nothing has said otherwise.
    ///
    /// One name, because this is the name that host knows: a cover is
    /// `library_600x900.jpg` there whatever the game's own record calls it, and
    /// asking that host for any other name is a 404 for every game at once.
    pub fn file_name(self) -> &'static str {
        match self {
            Piece::Cover => "library_600x900.jpg",
            Piece::Hero => "library_hero.jpg",
            Piece::Logo => "logo.png",
        }
    }

    /// Every name a published picture of this piece is known to end in.
    ///
    /// More than one for a cover: Valve publishes recent capsules as
    /// `library_capsule.jpg` and older ones as `library_600x900.jpg`, and a
    /// third of the games in a long-standing library are the newer sort. Only
    /// for looking through the client's cache, where the name on the file is
    /// whatever the game published — a search that knew one name would walk
    /// past the cover of every recent game it owns.
    ///
    /// Canonical name first, so a cache holding both answers with the piece as
    /// this crate elsewhere names it.
    pub fn file_names(self) -> &'static [&'static str] {
        match self {
            Piece::Cover => &["library_600x900.jpg", "library_capsule.jpg"],
            Piece::Hero => &["library_hero.jpg"],
            Piece::Logo => &["logo.png"],
        }
    }
}

/// Where one game's pictures are published, as its own PICS record says.
///
/// Each is a path below the game's asset directory: either a bare file name or
/// a content-addressed `{hash}/{file}`. Held for every game in the library and
/// handed to both halves of the search — the client's cache is laid out by these
/// paths, and so is the host that serves them.
///
/// Everything one holds has been through [`is_a_plain_path`], because it can
/// only be made from a PICS record and that is where the check is. So a caller
/// may join what [`Self::of`] gives it onto a directory or onto a URL without
/// asking again where the string came from.
///
/// Absent for a game whose record says nothing, which is not the same as a game
/// with no pictures: a title that is on the disk but not in this account's
/// catalogue — one somebody in the family shared — has a cover and no record
/// here to say where. Those fall back to asking by name, which is what this
/// crate did before any of this existed and is right for everything published
/// before the change.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Published {
    cover: Option<String>,
    hero: Option<String>,
    logo: Option<String>,
}

impl Published {
    /// Where one piece is, if the record said.
    pub fn of(&self, piece: Piece) -> Option<&str> {
        match piece {
            Piece::Cover => self.cover.as_deref(),
            Piece::Hero => self.hero.as_deref(),
            Piece::Logo => self.logo.as_deref(),
        }
    }

    /// Whether anything at all is known about where this game's pictures are.
    pub fn is_empty(&self) -> bool {
        self.cover.is_none() && self.hero.is_none() && self.logo.is_none()
    }
}

impl From<LibraryArt> for Published {
    fn from(art: LibraryArt) -> Published {
        Published {
            cover: art.capsule.filter(|path| is_a_plain_path(path)),
            hero: art.hero.filter(|path| is_a_plain_path(path)),
            logo: art.logo.filter(|path| is_a_plain_path(path)),
        }
    }
}

/// Whether a path Steam sent may be joined onto a directory and onto a URL.
///
/// Checked once, here, because this is a string from the network that becomes
/// part of a path on the local disk: a segment of `..` in it would be a read
/// out of the picture cache and into whatever is above it, and anything needing
/// escaping would be a URL this crate had assembled wrongly. What Valve
/// publishes is a file name, or a hex directory and a file name, and nothing in
/// either needs more than this alphabet.
///
/// A path that fails is dropped rather than repaired, and the piece it belongs
/// to is then asked for by name like any other — one game with a cover Valve
/// named strangely, rather than a shell reaching somewhere it should not.
fn is_a_plain_path(path: &str) -> bool {
    !path.is_empty()
        && path.split('/').all(|segment| {
            !segment.is_empty()
                && segment != "."
                && segment != ".."
                && segment
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        })
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
/// Three layouts, because Steam changed its mind twice: the current client
/// keeps each picture at the path the app publishes it under, below a directory
/// per app; before that the file sat directly in that directory under its plain
/// name; and older clients still flattened the app id into the file name. Each
/// is one `is_file` away and a machine has one of them at a time, so all are
/// looked for rather than guessed at from a version nobody can see.
///
/// The file is read where it lies and never written to. It is the client's
/// cache, and a shell that tidied up after Valve would be deleting pictures
/// out from under a program that is still running.
pub fn in_the_client_cache(app_id: u32, piece: Piece, published: Option<&str>) -> Option<PathBuf> {
    let root = crate::library::root()?;
    in_cache(
        &root.join("appcache").join("librarycache"),
        app_id,
        piece,
        published,
    )
}

/// The same, against a cache directory that is handed over — which is what
/// makes every layout testable without a Steam installation to point at.
fn in_cache(cache: &Path, app_id: u32, piece: Piece, published: Option<&str>) -> Option<PathBuf> {
    let app = cache.join(app_id.to_string());
    let published = published.filter(|path| is_a_plain_path(path));
    // The layouts that need no telling, under each name the piece is published
    // as. Tried even when the published path is known and was not there: a
    // client that cached this picture before Valve moved it left it under a
    // plain name, and that file is still this game's cover.
    let by_name = piece
        .file_names()
        .iter()
        .flat_map(|file| [app.join(file), cache.join(format!("{app_id}_{file}"))]);
    published
        .map(|path| app.join(path))
        .into_iter()
        .chain(by_name)
        .find(|path| path.is_file())
        // And, only for a game nothing said where to look, the directories the
        // client named after the pictures in them.
        .or_else(|| published.is_none().then(|| under_some_hash(&app, piece))?)
}

/// The current layout, for a game whose published path is not known.
///
/// The client files each picture in a directory named after its contents, so
/// without the name of that directory there is nothing to join — but there is
/// something to look in: the app's own directory holds a handful of them, and
/// each piece appears under its own name in exactly one, because the variants
/// that would share a name with it are the localized ones and Valve names those
/// differently.
///
/// Every name the piece is published under, not just the canonical one: the
/// game this is for is precisely the one nothing has said anything about, so its
/// cover is as likely to be a `library_capsule.jpg` as not.
///
/// Last of everything, and only for a game with no published path: it is a
/// `read_dir` where the rest of this is a `stat`, and where the path *is* known
/// and that file is not here, what the client holds is a picture Valve has since
/// replaced — better asked for than dug out.
fn under_some_hash(app: &Path, piece: Piece) -> Option<PathBuf> {
    let directories: Vec<PathBuf> = std::fs::read_dir(app)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .collect();
    piece.file_names().iter().find_map(|file| {
        let mut found: Vec<PathBuf> = directories
            .iter()
            .map(|directory| directory.join(file))
            .filter(|path| path.is_file())
            .collect();
        // Sorted, so a game that somehow has two answers has the same one every
        // session: a cover that changed between one look and the next would be a
        // picture the atlas holds under two keys.
        found.sort();
        found.into_iter().next()
    })
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
    pub fn fetch(
        &self,
        app_id: u32,
        piece: Piece,
        published: Option<&str>,
    ) -> Result<Vec<u8>, Missing> {
        let url = url_of(app_id, piece, published);
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

/// Where to ask Steam for one picture.
///
/// The published path when there is one, from the host that serves published
/// paths; otherwise the piece's plain name from the older one, which is the
/// best that can be done for a game with no record to read and is what answers
/// for everything published before store artwork was content-addressed.
fn url_of(app_id: u32, piece: Piece, published: Option<&str>) -> String {
    match published.filter(|path| is_a_plain_path(path)) {
        Some(path) => format!("{ASSETS}/{app_id}/{path}"),
        None => format!("{CDN}/{app_id}/{}", piece.file_name()),
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

    /// Every one of Valve's cache layouts is found, and a game the client has
    /// never shown is found in none of them — which is the answer that sends
    /// the caller to the network rather than to a file that is not there.
    #[test]
    fn any_of_the_client_cache_layouts_answers() {
        let scratch = scratch("layouts");
        std::fs::create_dir_all(scratch.join("504230")).expect("a scratch directory");
        std::fs::write(scratch.join("504230").join("library_600x900.jpg"), [0xff])
            .expect("the plain layout");
        std::fs::write(scratch.join("400_library_hero.jpg"), [0xff]).expect("the older layout");
        // And the current one: the picture at the path the app publishes it under.
        let hashed = scratch.join("3288210").join("28dbb244");
        std::fs::create_dir_all(&hashed).expect("a scratch directory");
        std::fs::write(hashed.join("library_600x900.jpg"), [0xff]).expect("the current layout");

        assert_eq!(
            in_cache(&scratch, 504230, Piece::Cover, None),
            Some(scratch.join("504230").join("library_600x900.jpg"))
        );
        assert_eq!(
            in_cache(&scratch, 400, Piece::Hero, None),
            Some(scratch.join("400_library_hero.jpg"))
        );
        assert_eq!(
            in_cache(
                &scratch,
                3288210,
                Piece::Cover,
                Some("28dbb244/library_600x900.jpg")
            ),
            Some(hashed.join("library_600x900.jpg"))
        );
        assert_eq!(in_cache(&scratch, 504230, Piece::Hero, None), None);
        assert_eq!(in_cache(&scratch, 999999, Piece::Cover, None), None);
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// A game with a hashed cover and no record saying where it is — one on the
    /// disk that this account does not own — is still found, by looking in the
    /// directories the client made rather than by knowing their names.
    #[test]
    fn a_hashed_picture_is_found_without_being_told_where() {
        let scratch = scratch("hashed");
        let hashed = scratch.join("3288210").join("28dbb244");
        std::fs::create_dir_all(&hashed).expect("a scratch directory");
        std::fs::write(hashed.join("library_600x900.jpg"), [0xff]).expect("a cover");
        // A picture of another piece, in its own directory, which must not be
        // mistaken for this one.
        let other = scratch.join("3288210").join("67a1c596");
        std::fs::create_dir_all(&other).expect("a scratch directory");
        std::fs::write(other.join("library_hero.jpg"), [0xff]).expect("a hero");

        assert_eq!(
            in_cache(&scratch, 3288210, Piece::Cover, None),
            Some(hashed.join("library_600x900.jpg"))
        );
        assert_eq!(
            in_cache(&scratch, 3288210, Piece::Hero, None),
            Some(other.join("library_hero.jpg"))
        );
        assert_eq!(in_cache(&scratch, 3288210, Piece::Logo, None), None);
        // But a path that *is* known and is not here is a picture Valve has
        // replaced since the client cached it, so the answer is to go and ask
        // rather than to hand back the old one out of some other directory.
        assert_eq!(
            in_cache(
                &scratch,
                3288210,
                Piece::Cover,
                Some("a1b2c3d4/library_600x900.jpg")
            ),
            None
        );
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// A published path that the client has not cached still finds the picture
    /// under its plain name: a client that filed it before Valve moved the
    /// artwork left it there, and it is the same game's cover.
    #[test]
    fn a_picture_the_client_filed_under_its_plain_name_still_answers() {
        let scratch = scratch("plain");
        std::fs::create_dir_all(scratch.join("220")).expect("a scratch directory");
        std::fs::write(scratch.join("220").join("library_600x900.jpg"), [0xff]).expect("a cover");

        assert_eq!(
            in_cache(
                &scratch,
                220,
                Piece::Cover,
                Some("f0e1d2c3/library_600x900.jpg")
            ),
            Some(scratch.join("220").join("library_600x900.jpg"))
        );
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// A published path is asked for at the host that serves published paths,
    /// and a game with no path falls back to Valve's older one by name. Getting
    /// this the wrong way round is a library of rows with no covers.
    #[test]
    fn a_published_picture_is_asked_for_where_it_is_published() {
        assert_eq!(
            url_of(3288210, Piece::Cover, Some("28dbb244/library_600x900.jpg")),
            format!("{ASSETS}/3288210/28dbb244/library_600x900.jpg")
        );
        // A bare published name is still the published path: the same host
        // answers for it, so there is one road for everything Steam has said
        // where to find.
        assert_eq!(
            url_of(400, Piece::Hero, Some("library_hero.jpg")),
            format!("{ASSETS}/400/library_hero.jpg")
        );
        assert_eq!(
            url_of(400, Piece::Hero, None),
            format!("{CDN}/400/library_hero.jpg")
        );
    }

    /// A path from the network is a path onto this machine's disk, so anything
    /// that is not plainly a file below the app's own directory is dropped and
    /// the piece asked for by name instead.
    #[test]
    fn a_path_that_leaves_the_cache_is_not_a_path() {
        assert!(is_a_plain_path("library_600x900.jpg"));
        assert!(is_a_plain_path(
            "28dbb24430c7fe4732fafd7ce3a3b701d1d805eb/logo.png"
        ));
        assert!(!is_a_plain_path(""));
        assert!(!is_a_plain_path("../../../../etc/passwd"));
        assert!(!is_a_plain_path("/etc/passwd"));
        assert!(!is_a_plain_path("hash//library_600x900.jpg"));
        assert!(!is_a_plain_path("hash/../../secrets.jpg"));
        assert!(!is_a_plain_path("hash/cover.jpg?t=1"));

        let art = Published::from(LibraryArt {
            capsule: Some("../../../../etc/passwd".to_string()),
            hero: Some("67a1c596/library_hero.jpg".to_string()),
            logo: None,
        });
        assert_eq!(art.of(Piece::Cover), None);
        assert_eq!(art.of(Piece::Hero), Some("67a1c596/library_hero.jpg"));
        assert_eq!(art.of(Piece::Logo), None);
        assert!(!art.is_empty());
        assert!(Published::default().is_empty());
    }

    fn scratch(what: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("lxb-art-{what}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("a scratch directory");
        path
    }
}
