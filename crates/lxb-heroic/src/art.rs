//! The pictures an Epic game is drawn with, fetched into the shell's cache.
//!
//! Three per game, all out of the game's own `keyImages` — see `library.rs`:
//! the tall box, which is the cover on the card; the wide box, which stands
//! behind the display while the cursor is on the row; and the logo, which the
//! loading screen puts in the middle of that picture while the game starts —
//! where there is one, which is 16 titles of 230 on the account this was
//! written against (Heroic has no other source either). Epic's image
//! server resizes on request, so none is fetched at its full size: a cover
//! at 1200×1600 is 459 KB and at 675×900 is 110 KB, a backdrop at 3840 wide is
//! 1.7 MB and at 1920 is 262 KB (measured on 2026-09-24).
//!
//! **Covers for the whole column, backdrops one at a time.** The shell asks
//! for every missing cover once the library arrives — about 23 MB for a
//! library of two hundred — and for a backdrop only when the cursor reaches a
//! game that has none, which is how the Steam column asks for its heroes too.
//!
//! Into `$XDG_CACHE_HOME/lxb/epic-art/<app_name>/`, the shell's own cache and
//! never Heroic's: what Heroic's own window keeps is Heroic's business.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::time::Duration;

use crate::heroic::Paths;
use crate::report::{Artwork, Game, PROTOCOL};

/// Which of a game's pictures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Cover,
    Hero,
    Logo,
}

impl Kind {
    fn file(self) -> &'static str {
        match self {
            Kind::Cover => "cover.jpg",
            Kind::Hero => "hero.jpg",
            // A PNG with its transparency, which is the whole of a wordmark.
            Kind::Logo => "logo.png",
        }
    }

    /// What Epic's image server is asked to scale it to. It keeps the
    /// picture's own shape either way: a logo 2602 wide comes back 1000 by
    /// 596 (measured on 2026-09-25).
    fn size(self) -> &'static str {
        match self {
            Kind::Cover => "h=900&w=600&resize=1",
            Kind::Hero => "h=1080&w=1920&resize=1",
            Kind::Logo => "w=1000&resize=1",
        }
    }
}

/// How many pictures come down at once. Epic's image server answers each in
/// about a second from here, and a library of two hundred fetched one at a
/// time kept a backdrop somebody was waiting for behind four minutes of covers.
const WORKERS: usize = 6;

/// How many failures, with nothing fetched at all, mean the server is not
/// going to answer.
const GIVE_UP: u32 = 8;

/// The largest picture this will keep: far above anything the resized
/// answers are, far below anything that is not a picture.
const LARGEST: u64 = 16 * 1024 * 1024;

/// Where the pictures are kept.
pub fn cache() -> Option<PathBuf> {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|at| at.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|home| home.is_absolute())
                .map(|home| home.join(".cache"))
        })
        .map(|cache| cache.join("lxb/epic-art"))
}

/// Where one of a game's pictures is, if it has been fetched.
pub fn cached(root: &Path, app_name: &str, kind: Kind) -> Option<PathBuf> {
    let at = folder(root, app_name)?.join(kind.file());
    at.is_file().then_some(at)
}

/// A game's own folder, refusing any name that would step outside the cache.
/// Epic's names are letters and digits; a name that is not is not a folder.
pub fn folder(root: &Path, app_name: &str) -> Option<PathBuf> {
    let plain = !app_name.is_empty()
        && app_name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    plain.then(|| root.join(app_name))
}

/// Fetch what is missing, one line per game that gained a picture, and a last
/// line with `done` set.
///
/// `only` narrows it to those games, in the order given; empty is every game
/// in the library, installed ones first — the order the column is drawn in,
/// so the top of it fills first. `heroes` asks for the backdrops as well as
/// the covers.
pub fn fetch(out: &mut impl Write, paths: &Paths, only: &[String], heroes: bool) -> bool {
    let Some(root) = cache() else {
        eprintln!("art: there is no cache directory");
        finish(out);
        return false;
    };
    let mut games = crate::library::read(paths);
    if only.is_empty() {
        games.sort_by_key(|game| game.installed.is_none());
    } else {
        games.retain(|game| only.contains(&game.app_name));
        games.sort_by_key(|game| only.iter().position(|app| *app == game.app_name));
    }

    let agent = agent();
    let of = games.len() as u32;
    let next = AtomicUsize::new(0);
    let landed_any = AtomicBool::new(false);
    let failures = AtomicU32::new(0);
    let stop = AtomicBool::new(false);
    let (tell, told) = std::sync::mpsc::channel::<(usize, bool)>();
    let mut answered = true;
    std::thread::scope(|scope| {
        for _ in 0..WORKERS {
            let tell = tell.clone();
            let (agent, games, root) = (&agent, &games, &root);
            let (next, landed_any, failures, stop) = (&next, &landed_any, &failures, &stop);
            scope.spawn(move || loop {
                let at = next.fetch_add(1, Ordering::Relaxed);
                if at >= games.len() || stop.load(Ordering::Relaxed) {
                    break;
                }
                let game = &games[at];
                let mut landed = false;
                for (kind, url) in [
                    (Kind::Cover, &game.cover),
                    (Kind::Hero, &game.hero),
                    (Kind::Logo, &game.logo),
                ] {
                    // The backdrop and the logo are asked for together, when
                    // the cursor reaches the game: they are the two pictures
                    // the display is covered with while it is on the row and
                    // while the game starts.
                    if kind != Kind::Cover && !heroes {
                        continue;
                    }
                    let Some(url) = url else {
                        continue;
                    };
                    if cached(root, &game.app_name, kind).is_some() {
                        continue;
                    }
                    match download(agent, root, game, kind, url) {
                        Ok(()) => {
                            landed = true;
                            landed_any.store(true, Ordering::Relaxed);
                        }
                        Err(why) => {
                            eprintln!("art: {} ({kind:?}): {why}", game.title);
                            // A server that answers nothing to the first
                            // several questions is not going to answer the
                            // next two hundred either.
                            let failed = failures.fetch_add(1, Ordering::Relaxed) + 1;
                            if failed >= GIVE_UP && !landed_any.load(Ordering::Relaxed) {
                                eprintln!("art: giving up for now");
                                stop.store(true, Ordering::Relaxed);
                            }
                        }
                    }
                }
                if tell.send((at, landed)).is_err() {
                    break;
                }
            });
        }
        drop(tell);
        for (at, landed) in told {
            if !(landed || !only.is_empty()) || !answered {
                continue;
            }
            let game = &games[at];
            let line = Artwork {
                protocol: PROTOCOL,
                app_name: game.app_name.clone(),
                cover: path_of(cached(&root, &game.app_name, Kind::Cover)),
                hero: path_of(cached(&root, &game.app_name, Kind::Hero)),
                logo: path_of(cached(&root, &game.app_name, Kind::Logo)),
                at: at as u32 + 1,
                of,
                done: false,
            };
            if !say(out, &line) {
                // Nobody is reading: stop fetching what nobody will be told.
                answered = false;
                stop.store(true, Ordering::Relaxed);
            }
        }
    });
    if answered {
        finish(out);
    }
    answered && !stop.load(Ordering::Relaxed)
}

fn path_of(at: Option<PathBuf>) -> Option<String> {
    at.map(|at| at.to_string_lossy().into_owned())
}

fn finish(out: &mut impl Write) {
    say(
        out,
        &Artwork {
            protocol: PROTOCOL,
            app_name: String::new(),
            cover: None,
            hero: None,
            logo: None,
            at: 0,
            of: 0,
            done: true,
        },
    );
}

fn say(out: &mut impl Write, line: &Artwork) -> bool {
    let Ok(json) = serde_json::to_string(line) else {
        return false;
    };
    writeln!(out, "{json}").and_then(|()| out.flush()).is_ok()
}

pub fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .user_agent(concat!(
            "LXB/",
            env!("CARGO_PKG_VERSION"),
            " Heroic companion"
        ))
        .build()
        .into()
}

/// The address to ask for, scaled: Epic's own pictures take the size as a
/// query, and any that already carry one are asked as they are.
pub fn sized(url: &str, kind: Kind) -> String {
    let url = escaped(url);
    if url.contains('?') {
        url
    } else {
        format!("{url}?{}", kind.size())
    }
}

/// An address as a request may carry it. Epic's own picture addresses have
/// spaces in them — `…/GTAV_EGS_Artwork_1200x1600_Portrait Store Banner-…jpg`
/// — which a request refuses; every byte outside what an address may hold is
/// written as `%XX`, and what is already escaped is left alone.
fn escaped(url: &str) -> String {
    let mut out = String::with_capacity(url.len());
    for byte in url.bytes() {
        let allowed = byte.is_ascii_alphanumeric() || b"-._~:/?#[]@!$&'()*+,;=%".contains(&byte);
        if allowed {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn download(
    agent: &ureq::Agent,
    root: &Path,
    game: &Game,
    kind: Kind,
    url: &str,
) -> Result<(), String> {
    let folder = folder(root, &game.app_name).ok_or("an app name the cache cannot hold")?;
    std::fs::create_dir_all(&folder).map_err(|err| err.to_string())?;
    let mut response = agent
        .get(&sized(url, kind))
        .call()
        .map_err(|err| err.to_string())?;
    let mut bytes = Vec::new();
    response
        .body_mut()
        .with_config()
        .limit(LARGEST)
        .reader()
        .read_to_end(&mut bytes)
        .map_err(|err| err.to_string())?;
    if !is_a_picture(&bytes) {
        return Err("the answer is not a picture".to_string());
    }
    let at = folder.join(kind.file());
    let temp = folder.join(format!(".{}.lxb-{}", kind.file(), std::process::id()));
    std::fs::write(&temp, &bytes).map_err(|err| err.to_string())?;
    std::fs::rename(&temp, &at).map_err(|err| {
        let _ = std::fs::remove_file(&temp);
        err.to_string()
    })
}

/// Fetch one picture to `at`, beside it first and then over it. Nothing is
/// kept that is not a picture.
pub fn fetch_to(agent: &ureq::Agent, url: &str, at: &Path) -> Result<(), String> {
    let mut response = agent
        .get(&escaped(url))
        .call()
        .map_err(|err| err.to_string())?;
    let mut bytes = Vec::new();
    response
        .body_mut()
        .with_config()
        .limit(LARGEST)
        .reader()
        .read_to_end(&mut bytes)
        .map_err(|err| err.to_string())?;
    if !is_a_picture(&bytes) {
        return Err("the answer is not a picture".to_string());
    }
    if let Some(folder) = at.parent() {
        std::fs::create_dir_all(folder).map_err(|err| err.to_string())?;
    }
    let temp = at.with_extension(format!("lxb-{}", std::process::id()));
    std::fs::write(&temp, &bytes).map_err(|err| err.to_string())?;
    std::fs::rename(&temp, at).map_err(|err| {
        let _ = std::fs::remove_file(&temp);
        err.to_string()
    })
}

/// JPEG, PNG or WebP, by the first bytes — what Epic serves, and what the
/// shell's decoder reads.
fn is_a_picture(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xff, 0xd8, 0xff])
        || bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || (bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epics_pictures_are_asked_for_at_the_size_they_are_drawn() {
        assert_eq!(
            sized("https://cdn1.epicgames.com/a/b-tall.jpg", Kind::Cover),
            "https://cdn1.epicgames.com/a/b-tall.jpg?h=900&w=600&resize=1"
        );
        assert_eq!(
            sized("https://cdn1.epicgames.com/a/b-wide.jpg", Kind::Hero),
            "https://cdn1.epicgames.com/a/b-wide.jpg?h=1080&w=1920&resize=1"
        );
        assert_eq!(
            sized("https://x/y.jpg?v=2", Kind::Cover),
            "https://x/y.jpg?v=2"
        );
        // Epic's own addresses, spaces and all, as the library carries them.
        assert_eq!(
            sized(
                "https://cdn1.epicgames.com/a/item/UNO S2 -BANNERS-1280x1440-a43c.jpg",
                Kind::Cover
            ),
            "https://cdn1.epicgames.com/a/item/UNO%20S2%20-BANNERS-1280x1440-a43c.jpg?h=900&w=600&resize=1"
        );
        assert_eq!(
            sized("https://x/a%20b (1).jpg", Kind::Hero),
            "https://x/a%20b%20(1).jpg?h=1080&w=1920&resize=1"
        );
    }

    /// An app name is a folder name here, so one that could climb out of
    /// the cache is not one.
    #[test]
    fn a_game_cannot_name_a_folder_outside_the_cache() {
        let root = Path::new("/cache/lxb/epic-art");
        assert_eq!(
            folder(root, "051eaac0842c46d7a5a62858ad534d5a"),
            Some(root.join("051eaac0842c46d7a5a62858ad534d5a"))
        );
        assert_eq!(folder(root, "Quail"), Some(root.join("Quail")));
        assert_eq!(folder(root, "../../etc"), None);
        assert_eq!(folder(root, "a/b"), None);
        assert_eq!(folder(root, ""), None);
    }

    #[test]
    fn only_a_picture_is_kept() {
        assert!(is_a_picture(&[0xff, 0xd8, 0xff, 0xe0]));
        assert!(is_a_picture(b"\x89PNG\r\n\x1a\n...."));
        assert!(is_a_picture(b"RIFF\0\0\0\0WEBPVP8 "));
        assert!(!is_a_picture(b"<html>Access Denied</html>"));
    }

    #[test]
    fn a_picture_counts_as_fetched_only_where_it_is() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(cached(dir.path(), "Quail", Kind::Cover), None);
        std::fs::create_dir_all(dir.path().join("Quail")).unwrap();
        std::fs::write(dir.path().join("Quail/cover.jpg"), [0xff, 0xd8, 0xff]).unwrap();
        assert_eq!(
            cached(dir.path(), "Quail", Kind::Cover),
            Some(dir.path().join("Quail/cover.jpg"))
        );
        assert_eq!(cached(dir.path(), "Quail", Kind::Hero), None);
    }
}
