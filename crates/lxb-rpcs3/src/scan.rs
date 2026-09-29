//! What somebody's PlayStation 3 games are: the ones RPCS3 has installed, and
//! the ones in their PS3 folder — disc images, unpacked folders, and packages
//! waiting to be installed, loose or in the zip they were downloaded in.
//!
//! One pass, one [`Library`]. Every game is read for what it says about
//! itself (its `PARAM.SFO`) and for its pictures, film and music (see
//! [`crate::art`]); nothing is fetched from anywhere and nothing is written but
//! the shell's cache.
//!
//! ## One row per game
//!
//! A package that RPCS3 has already installed is not listed: the installed
//! game is, and it is the same game. Nor is anything that is not a game a
//! person starts — the system's own folders under `dev_hdd0/game`, and the
//! updates to disc games RPCS3 keeps there (category `GD`), which RPCS3 applies
//! by itself when the disc boots.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::art;
use crate::iso::{Encryption, Iso};
use crate::pkg::{self, Package};
use crate::report::{Form, Game, Key, Library, PROTOCOL};
use crate::rpcs3::Console;
use crate::sfo::Sfo;
use crate::zip;

/// How far below the chosen folder games are looked for — a folder per game,
/// a folder per region above that — the same as RetroArch's scan.
const DEPTH: usize = 3;

/// Read everything.
///
/// `folder` is the PS3 folder, where one is chosen; `console` is RPCS3's
/// PlayStation 3, where there is an RPCS3; `art` is where the pictures of the
/// games that are images and packages are kept (see [`crate::art`]), or `None`
/// for a machine with no cache to keep them in.
pub fn library(folder: Option<&Path>, console: Option<&Console>, art: Option<&Path>) -> Library {
    let mut games = Vec::new();
    let mut kept = HashSet::new();
    let mut complete = true;

    let mut installed = console.map(installed_games).unwrap_or_default();
    // What uninstalling one would give back, which is what its size is for.
    for game in &mut installed {
        game.size = crate::remove::size_of(Path::new(&game.path));
    }
    let mut unreadable = None;
    if let Some(folder) = folder {
        match std::fs::read_dir(folder) {
            Ok(_) => {
                let mut found = Vec::new();
                walk(folder, 0, &mut found);
                for candidate in found {
                    match candidate {
                        Candidate::Disc(path) => {
                            if let Some(game) = disc(&path, console, art, &mut kept) {
                                games.push(game);
                            }
                        }
                        Candidate::Folder(path) => {
                            if let Some(game) = unpacked(&path) {
                                games.push(game);
                            }
                        }
                        Candidate::Package(path) => {
                            if let Some(game) = package(&path, console, art, &mut kept) {
                                games.push(game);
                            }
                        }
                        Candidate::Zip(path) => {
                            if let Some(game) = zipped(&path, console, art, &mut kept) {
                                games.push(game);
                            }
                        }
                    }
                }
            }
            Err(err) => {
                complete = false;
                unreadable = Some(err.to_string());
            }
        }
    }

    // A package already installed is the installed game; the installed game's
    // row is the one kept.
    let installed_dirs: HashSet<String> = installed
        .iter()
        .filter_map(|game| game.id.strip_prefix("hdd0/").map(str::to_string))
        .collect();
    games.retain(|game| {
        game.form != Form::Package
            || !game
                .serial
                .as_ref()
                .is_some_and(|serial| installed_dirs.contains(serial))
    });
    one_row_per_game(&mut games);
    games.extend(installed);
    games.sort_by(|a, b| {
        a.title
            .to_lowercase()
            .cmp(&b.title.to_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });

    if let Some(art) = art.filter(|_| complete) {
        art::sweep(art, &kept);
    }
    Library {
        protocol: PROTOCOL,
        folder: folder.map(|folder| folder.to_string_lossy().into_owned()),
        games,
        unreadable,
    }
}

/// A game's packages — the game and the updates downloaded beside it — are one
/// row: the earliest version's, which carries the others as its `updates`, in
/// version order, so that one press installs the game and brings it up to
/// date. Two rows with one name, one of which cannot be played without the
/// other, would be a column asking somebody to know which to press first.
fn one_row_per_game(games: &mut Vec<Game>) {
    let mut by_serial: std::collections::BTreeMap<String, Vec<usize>> =
        std::collections::BTreeMap::new();
    for (index, game) in games.iter().enumerate() {
        if let (Form::Package, Some(serial)) = (game.form, game.serial.as_ref()) {
            by_serial.entry(serial.clone()).or_default().push(index);
        }
    }
    let mut folded = HashSet::new();
    for (_, mut indices) in by_serial {
        if indices.len() < 2 {
            continue;
        }
        indices.sort_by(|a, b| {
            games[*a]
                .version
                .cmp(&games[*b].version)
                .then_with(|| games[*a].path.cmp(&games[*b].path))
        });
        let (first, rest) = indices.split_first().expect("two or more");
        games[*first].updates = rest.iter().map(|at| games[*at].path.clone()).collect();
        folded.extend(rest.iter().copied());
    }
    let mut index = 0;
    games.retain(|_| {
        let keep = !folded.contains(&index);
        index += 1;
        keep
    });
}

enum Candidate {
    Disc(PathBuf),
    Folder(PathBuf),
    Package(PathBuf),
    Zip(PathBuf),
}

/// Everything under `at` that could be a game, in name order.
fn walk(at: &Path, depth: usize, found: &mut Vec<Candidate>) {
    let Ok(listing) = std::fs::read_dir(at) else {
        return;
    };
    let mut entries: Vec<PathBuf> = listing.filter_map(Result::ok).map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            if is_game_folder(&path) {
                found.push(Candidate::Folder(path));
            } else if depth + 1 < DEPTH {
                walk(&path, depth + 1, found);
            }
        } else if name.ends_with(".iso") {
            found.push(Candidate::Disc(path));
        } else if name.ends_with(".pkg") {
            found.push(Candidate::Package(path));
        } else if name.ends_with(".zip") {
            found.push(Candidate::Zip(path));
        }
    }
}

/// A disc copied to a folder (`PS3_GAME/PARAM.SFO` under it), or a game
/// unpacked from a package (`PARAM.SFO` and `USRDIR/EBOOT.BIN`).
fn is_game_folder(at: &Path) -> bool {
    at.join("PS3_GAME/PARAM.SFO").is_file()
        || (at.join("PARAM.SFO").is_file() && at.join("USRDIR/EBOOT.BIN").is_file())
}

/// The installed games, from RPCS3's `dev_hdd0/game`.
fn installed_games(console: &Console) -> Vec<Game> {
    let Ok(listing) = std::fs::read_dir(console.games()) else {
        return Vec::new();
    };
    let mut games = Vec::new();
    for entry in listing.filter_map(Result::ok) {
        let dir = entry.path();
        let Some(name) = dir
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        let Some(sfo) = std::fs::read(dir.join("PARAM.SFO"))
            .ok()
            .and_then(|bytes| Sfo::parse(&bytes))
        else {
            continue;
        };
        // A game somebody starts; not the system's folders and not an update.
        if sfo.category() != Some("HG") {
            continue;
        }
        let boot = dir.join("USRDIR/EBOOT.BIN");
        if !boot.is_file() {
            continue;
        }
        games.push(game_from_folder(
            format!("hdd0/{name}"),
            &sfo,
            &dir,
            &dir,
            Form::Installed,
            Some(boot),
        ));
    }
    games
}

/// A game unpacked into a folder of the PS3 folder.
fn unpacked(at: &Path) -> Option<Game> {
    let (base, boot) = if at.join("PS3_GAME/PARAM.SFO").is_file() {
        (at.join("PS3_GAME"), at.join("PS3_GAME/USRDIR/EBOOT.BIN"))
    } else {
        (at.to_path_buf(), at.join("USRDIR/EBOOT.BIN"))
    };
    let sfo = Sfo::parse(&std::fs::read(base.join("PARAM.SFO")).ok()?)?;
    Some(game_from_folder(
        at.to_string_lossy().into_owned(),
        &sfo,
        at,
        &base,
        Form::Folder,
        boot.is_file().then_some(boot),
    ))
}

/// A game whose files are already on the disk: their art is named in place.
fn game_from_folder(
    id: String,
    sfo: &Sfo,
    path: &Path,
    base: &Path,
    form: Form,
    boot: Option<PathBuf>,
) -> Game {
    let file = |name: &str| {
        let at = base.join(name);
        at.is_file().then(|| at.to_string_lossy().into_owned())
    };
    Game {
        id,
        serial: sfo.title_id().map(str::to_string),
        title: title(sfo, path),
        titles: sfo.titles(),
        form,
        path: path.to_string_lossy().into_owned(),
        boot: boot.map(|boot| boot.to_string_lossy().into_owned()),
        version: sfo.app_version().map(str::to_string),
        category: sfo.category().map(str::to_string),
        icon: file("ICON0.PNG"),
        backdrop: file("PIC1.PNG"),
        overlay: file("PIC0.PNG"),
        preview: file("ICON1.PAM"),
        music: file("SND0.AT3"),
        key: Key::Unneeded,
        trophies: trophy_set(&base.join("TROPDIR")),
        size: 0,
        cache: 0,
        updates: Vec::new(),
        licence: None,
    }
}

/// The trophy set a game carries, by the folder `TROPDIR` names it with.
fn trophy_set(tropdir: &Path) -> Option<String> {
    let listing = std::fs::read_dir(tropdir).ok()?;
    let mut sets: Vec<String> = listing
        .filter_map(Result::ok)
        .filter(|entry| entry.path().join("TROPHY.TRP").is_file())
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        .collect();
    sets.sort();
    sets.into_iter().next()
}

/// A disc image.
fn disc(
    path: &Path,
    console: Option<&Console>,
    art: Option<&Path>,
    kept: &mut HashSet<PathBuf>,
) -> Option<Game> {
    let iso = match Iso::open(path) {
        Ok(iso) => iso,
        Err(err) => {
            eprintln!("scan: {} is not a disc image: {err}", path.display());
            return None;
        }
    };
    // A PS2 or PSP image left in the PS3 folder is not a PS3 game.
    if !iso.has_directory("PS3_GAME") {
        eprintln!("scan: {} is not a PlayStation 3 disc", path.display());
        return None;
    }
    let sfo = iso
        .file("PS3_GAME/PARAM.SFO", 1 << 20, None)
        .and_then(|bytes| Sfo::parse(&bytes))?;
    let id = path.to_string_lossy().into_owned();
    let key = match iso.encryption() {
        Encryption::Encrypted { .. } => {
            if crate::keys::named_key(path, console).is_some() {
                Key::Present
            } else {
                Key::Missing
            }
        }
        Encryption::Decrypted | Encryption::Plain => Key::Unneeded,
    };

    let art = art.and_then(|root| {
        let folder = art::folder(root, &id);
        let stamp = art::stamp(path)?;
        if !art::fresh(&folder, &stamp) {
            art::clear(&folder);
            for name in art::FILES {
                // A picture on the encrypted part of an encrypted disc is
                // left out rather than kept as noise.
                if let Some(bytes) = iso.file(&format!("PS3_GAME/{name}"), art::LIMIT, None) {
                    art::keep(&folder, name, &bytes);
                }
            }
            art::seal(&folder, &stamp);
        }
        kept.insert(folder.clone());
        Some(folder)
    });
    let kept_file = |name: &str| {
        art.as_ref()
            .and_then(|folder| art::kept(folder, name))
            .map(|at| at.to_string_lossy().into_owned())
    };
    let trophies = disc_trophy_set(&iso);
    Some(Game {
        id: id.clone(),
        serial: sfo
            .title_id()
            .map(str::to_string)
            .or_else(|| iso.serial().map(str::to_string)),
        title: title(&sfo, path),
        titles: sfo.titles(),
        form: Form::Disc,
        path: id.clone(),
        boot: Some(id),
        version: sfo.app_version().map(str::to_string),
        category: sfo.category().map(str::to_string),
        icon: kept_file("ICON0.PNG"),
        backdrop: kept_file("PIC1.PNG"),
        overlay: kept_file("PIC0.PNG"),
        preview: kept_file("ICON1.PAM"),
        music: kept_file("SND0.AT3"),
        key,
        trophies,
        size: std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0),
        cache: 0,
        updates: Vec::new(),
        licence: None,
    })
}

/// The trophy set on a disc: the `NPWR…` folder under `PS3_GAME/TROPDIR` that
/// has a `TROPHY.TRP` in it.
fn disc_trophy_set(iso: &Iso) -> Option<String> {
    let mut sets: Vec<String> = iso
        .list("PS3_GAME/TROPDIR")
        .into_iter()
        .filter(|(name, directory)| {
            *directory
                && iso
                    .find(&format!("PS3_GAME/TROPDIR/{name}/TROPHY.TRP"))
                    .is_some()
        })
        .map(|(name, _)| name)
        .collect();
    sets.sort();
    sets.into_iter().next()
}

/// A package on its own.
fn package(
    path: &Path,
    console: Option<&Console>,
    art: Option<&Path>,
    kept: &mut HashSet<PathBuf>,
) -> Option<Game> {
    let source = pkg::Source::file(path).ok()?;
    game_from_package(path, source, loose_licence(path), console, art, kept)
}

/// A zip with a package in it — the way a downloaded game nearly always
/// arrives, with its licence beside it.
fn zipped(
    path: &Path,
    console: Option<&Console>,
    art: Option<&Path>,
    kept: &mut HashSet<PathBuf>,
) -> Option<Game> {
    let entries = zip::entries(path).ok()?;
    let packaged = entries.iter().find(|entry| {
        !entry.is_directory() && entry.file_name().to_lowercase().ends_with(".pkg")
    })?;
    let licence = entries
        .iter()
        .find(|entry| entry.file_name().to_lowercase().ends_with(".rap"))
        .map(|entry| entry.name.clone());
    let source = pkg::Source::zipped(path, packaged)
        .map_err(|err| eprintln!("scan: {} cannot be read: {err}", path.display()))
        .ok()?;
    game_from_package(path, source, licence, console, art, kept)
}

/// A `.rap` beside a loose package, named after its content ID the way RPCS3
/// and every store page names one.
fn loose_licence(package: &Path) -> Option<String> {
    let dir = package.parent()?;
    let listing = std::fs::read_dir(dir).ok()?;
    let raps: Vec<PathBuf> = listing
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("rap"))
        })
        .collect();
    // Which one belongs to this package is only known once it is open; the
    // caller checks the name against the content ID.
    (raps.len() == 1).then(|| raps[0].to_string_lossy().into_owned())
}

fn game_from_package(
    path: &Path,
    source: pkg::Source,
    licence: Option<String>,
    console: Option<&Console>,
    art: Option<&Path>,
    kept: &mut HashSet<PathBuf>,
) -> Option<Game> {
    let mut package = match Package::open(source) {
        Ok(package) => package,
        Err(err) => {
            eprintln!(
                "scan: {} is not a package this can read: {err}",
                path.display()
            );
            return None;
        }
    };
    if !package.content.is_played() {
        eprintln!(
            "scan: {} is not a game ({:?})",
            path.display(),
            package.content
        );
        return None;
    }
    let sfo = package
        .item("PARAM.SFO")
        .cloned()
        .and_then(|item| package.read(&item, 1 << 20).ok().flatten())
        .and_then(|bytes| Sfo::parse(&bytes))
        .unwrap_or_default();
    let id = path.to_string_lossy().into_owned();

    let art = art.and_then(|root| {
        let folder = art::folder(root, &id);
        let stamp = art::stamp(path)?;
        if !art::fresh(&folder, &stamp) {
            art::clear(&folder);
            // In the order they are in the package, so a zipped one is read
            // forwards once.
            let mut wanted: Vec<pkg::Item> = art::FILES
                .iter()
                .filter_map(|name| package.item(name).cloned())
                .collect();
            wanted.sort_by_key(|item| package_position(&package, item));
            for item in wanted {
                match package.read(&item, art::LIMIT) {
                    Ok(Some(bytes)) => {
                        art::keep(&folder, &item.name.to_uppercase(), &bytes);
                    }
                    Ok(None) => {}
                    Err(err) => eprintln!("scan: {} in {}: {err}", item.name, path.display()),
                }
            }
            art::seal(&folder, &stamp);
        }
        kept.insert(folder.clone());
        Some(folder)
    });
    let kept_file = |name: &str| {
        art.as_ref()
            .and_then(|folder| art::kept(folder, name))
            .map(|at| at.to_string_lossy().into_owned())
    };
    let licence = licence.filter(|licence| {
        Path::new(licence)
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(&package.content_id))
    });
    let installed_licence = console.is_some_and(|console| {
        console
            .licences()
            .join(format!("{}.rap", package.content_id))
            .is_file()
    });
    let title = sfo.title().unwrap_or_else(|| file_title(path));
    Some(Game {
        id: id.clone(),
        serial: Some(package.install_dir.clone()),
        title,
        titles: sfo.titles(),
        form: Form::Package,
        path: id,
        boot: None,
        version: sfo.app_version().map(str::to_string),
        category: sfo.category().map(str::to_string),
        icon: kept_file("ICON0.PNG"),
        backdrop: kept_file("PIC1.PNG"),
        overlay: kept_file("PIC0.PNG"),
        preview: kept_file("ICON1.PAM"),
        music: kept_file("SND0.AT3"),
        key: Key::Unneeded,
        trophies: None,
        size: std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0),
        cache: 0,
        updates: Vec::new(),
        licence: licence
            .or_else(|| installed_licence.then(|| format!("{}.rap", package.content_id))),
    })
}

/// Where an item's bytes start in the package, for reading several in order.
fn package_position(package: &Package, item: &pkg::Item) -> usize {
    package
        .items
        .iter()
        .position(|candidate| candidate == item)
        .unwrap_or(usize::MAX)
}

/// The game's own name, or the file's where it gives none.
fn title(sfo: &Sfo, path: &Path) -> String {
    sfo.title().unwrap_or_else(|| file_title(path))
}

/// A file's name as a game's: the extension off, and the tags a dump's name
/// carries — `(USA) (En,Fr,Es)` — left on, because they are how somebody tells
/// two copies apart.
fn file_title(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iso::fixture;
    use crate::sfo;

    /// Every test's cache is a scratch directory of its own, so no test reads
    /// or sweeps the real one — or another test's.
    fn scratch_cache() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn console_with(games: &[(&str, &[(&str, &str)])]) -> (tempfile::TempDir, Console) {
        let dir = tempfile::tempdir().unwrap();
        for (folder, values) in games {
            let at = dir.path().join("dev_hdd0/game").join(folder);
            std::fs::create_dir_all(at.join("USRDIR")).unwrap();
            std::fs::write(at.join("USRDIR/EBOOT.BIN"), b"SCE\0").unwrap();
            std::fs::write(at.join("PARAM.SFO"), sfo::build(values, &[])).unwrap();
            std::fs::write(at.join("ICON0.PNG"), b"\x89PNG").unwrap();
        }
        let console = Console::of(dir.path());
        (dir, console)
    }

    #[test]
    fn installed_games_are_listed_and_updates_are_not() {
        let cache = scratch_cache();
        let (_dir, console) = console_with(&[
            (
                "NPEA00019",
                &[
                    ("TITLE", "TEKKEN 5 DR"),
                    ("TITLE_ID", "NPEA00019"),
                    ("CATEGORY", "HG"),
                ],
            ),
            (
                "BLUS30359",
                &[
                    ("TITLE", "TEKKEN 6"),
                    ("TITLE_ID", "BLUS30359"),
                    ("CATEGORY", "GD"),
                ],
            ),
        ]);
        let library = library(None, Some(&console), Some(cache.path()));
        assert_eq!(library.games.len(), 1);
        let game = &library.games[0];
        assert_eq!(game.id, "hdd0/NPEA00019");
        assert_eq!(game.form, Form::Installed);
        assert!(game
            .boot
            .as_deref()
            .is_some_and(|boot| boot.ends_with("USRDIR/EBOOT.BIN")));
        assert!(game
            .icon
            .as_deref()
            .is_some_and(|icon| icon.ends_with("ICON0.PNG")));
        assert_eq!(game.backdrop, None);
    }

    #[test]
    fn a_disc_image_is_read_for_its_name_and_pictures() {
        let cache = scratch_cache();
        let games = tempfile::tempdir().unwrap();
        let sfo = sfo::build(
            &[
                ("TITLE", "TEKKEN 6"),
                ("TITLE_ID", "BLUS30359"),
                ("CATEGORY", "DG"),
            ],
            &[],
        );
        let built = fixture::build(
            "BLUS-30359",
            &[
                ("PS3_GAME/PARAM.SFO", &sfo),
                ("PS3_GAME/ICON0.PNG", b"\x89PNG icon"),
                ("PS3_GAME/PIC1.PNG", b"\x89PNG backdrop"),
                ("PS3_GAME/ICON1.PAM", b"PAMF film"),
            ],
            &[],
        );
        let iso = games.path().join("Tekken 6 (USA).iso");
        std::fs::write(&iso, &built.bytes).unwrap();
        // A PSP image in the wrong folder is passed over.
        let psp = fixture::build("ULUS-10000", &[("UMD_DATA.BIN", b"psp")], &[]);
        std::fs::write(games.path().join("psp.iso"), &psp.bytes).unwrap();

        let library = library(Some(games.path()), None, Some(cache.path()));
        assert_eq!(library.games.len(), 1);
        let game = &library.games[0];
        assert_eq!(game.title, "TEKKEN 6");
        assert_eq!(game.serial.as_deref(), Some("BLUS30359"));
        assert_eq!(game.form, Form::Disc);
        assert_eq!(game.key, Key::Unneeded);
        assert_eq!(game.boot.as_deref(), Some(iso.to_str().unwrap()));
        let icon = game.icon.as_deref().expect("the icon was kept");
        assert_eq!(std::fs::read(icon).unwrap(), b"\x89PNG icon");
        let film = game.preview.as_deref().expect("the film was kept");
        assert_eq!(std::fs::read(film).unwrap(), b"PAMF film");
        assert_eq!(game.music, None);
    }

    /// The Tekken 5 shape, end to end: a zip holding a package and its
    /// licence lists as the game, until RPCS3 has installed it — and then the
    /// installed game is the row.
    #[test]
    fn a_zipped_package_is_listed_until_it_is_installed() {
        let cache = scratch_cache();
        let games = tempfile::tempdir().unwrap();
        let content = "EP9000-NPEA00019_00-TEKKENRETAIL0000";
        let sfo = sfo::build(
            &[
                ("TITLE", "TEKKEN 5 DR"),
                ("TITLE_ID", "NPEA00019"),
                ("CATEGORY", "HG"),
            ],
            &[],
        );
        let package = crate::pkg::build(
            content,
            0x05,
            &[
                ("PARAM.SFO", &sfo),
                ("ICON0.PNG", b"\x89PNG t5"),
                ("USRDIR/EBOOT.BIN", b"SCE\0"),
            ],
        );
        let zipped = crate::zip::build(
            &[
                ("tekken.pkg", &package),
                (&format!("{content}.rap"), &[1; 16]),
            ],
            true,
        );
        std::fs::write(games.path().join("Tekken 5.zip"), zipped).unwrap();

        let (_dir, console) = console_with(&[]);
        let listed = library(Some(games.path()), Some(&console), Some(cache.path()));
        assert_eq!(listed.games.len(), 1);
        let game = &listed.games[0];
        assert_eq!(game.form, Form::Package);
        assert_eq!(game.title, "TEKKEN 5 DR");
        assert_eq!(game.serial.as_deref(), Some("NPEA00019"));
        assert_eq!(game.boot, None);
        assert_eq!(game.licence.as_deref(), Some(&*format!("{content}.rap")));
        assert_eq!(
            std::fs::read(game.icon.as_deref().expect("the icon")).unwrap(),
            b"\x89PNG t5"
        );

        let (_dir, console) = console_with(&[(
            "NPEA00019",
            &[
                ("TITLE", "TEKKEN 5 DR"),
                ("TITLE_ID", "NPEA00019"),
                ("CATEGORY", "HG"),
            ],
        )]);
        let listed = library(Some(games.path()), Some(&console), Some(cache.path()));
        assert_eq!(listed.games.len(), 1);
        assert_eq!(listed.games[0].form, Form::Installed);
    }

    #[test]
    fn a_missing_folder_says_so_rather_than_listing_nothing() {
        let cache = scratch_cache();
        let listed = library(
            Some(Path::new("/nonexistent/ps3")),
            None,
            Some(cache.path()),
        );
        assert!(listed.unreadable.is_some());
        assert!(listed.games.is_empty());
    }

    /// A game and its update downloaded side by side are one row — the game's,
    /// carrying the update — and a package of another game is left alone.
    #[test]
    fn a_game_and_its_update_are_one_row() {
        let package = |path: &str, serial: &str, version: &str| Game {
            id: path.to_string(),
            serial: Some(serial.to_string()),
            title: "SUPER STARDUST HD".to_string(),
            titles: Default::default(),
            form: Form::Package,
            path: path.to_string(),
            boot: None,
            version: Some(version.to_string()),
            category: Some("HG".to_string()),
            icon: None,
            backdrop: None,
            overlay: None,
            preview: None,
            music: None,
            key: Key::Unneeded,
            trophies: None,
            size: 0,
            cache: 0,
            updates: Vec::new(),
            licence: None,
        };
        let mut games = vec![
            package("/ps3/Update-6.00.pkg", "NPEA00014", "06.00"),
            package("/ps3/Tekken.zip", "NPEA00019", "01.00"),
            package("/ps3/Full.pkg", "NPEA00014", "04.00"),
            package("/ps3/Update-5.00.pkg", "NPEA00014", "05.00"),
        ];
        one_row_per_game(&mut games);
        assert_eq!(games.len(), 2);
        assert_eq!(games[0].path, "/ps3/Tekken.zip");
        assert!(games[0].updates.is_empty());
        assert_eq!(games[1].path, "/ps3/Full.pkg");
        assert_eq!(
            games[1].updates,
            ["/ps3/Update-5.00.pkg", "/ps3/Update-6.00.pkg"]
        );
    }
}
