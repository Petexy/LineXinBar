//! A disc's key, for a disc image that is still encrypted.
//!
//! RPCS3 boots an encrypted Redump image only with the disc's key in a file
//! named after the image — beside it, or in its own `data/redump` folder. The
//! user asked for the key to be fetched rather than asked for, so this looks
//! for it in **Redump's own published collection of PS3 disc keys**, the
//! zip the PlayStation 3 IRD database mirrors on GitHub (the collection PS3
//! Disc Dumper's authors keep), and saves the one that fits in RPCS3's folder
//! under the image's own name. Nothing is ever written beside the user's
//! games.
//!
//! ## A key is proven, never matched by name
//!
//! Redump names a key after the dump it belongs to, and people rename dumps:
//! the user's own `Skate 3 (USA) (En,Fr,Es).iso` is Redump's `Skate 3 (USA,
//! Asia) (En,Fr,Es)`. So a name is only where the search *starts*; what
//! decides is whether the key decrypts the disc's own `LIC.DAT` to the bytes
//! it begins with (see [`crate::iso::Probe`]). Four and a half thousand keys
//! is four and a half thousand single AES blocks — no time at all.
//!
//! ## Never for a disc that does not need one
//!
//! An image that has already been decrypted keeps Redump's region table, and
//! RPCS3 applies a key it finds by name without checking it — a key saved for
//! such an image would make noise of every sector. So a key is only ever
//! looked for once [`Iso::encryption`] has found the disc still encrypted.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::iso::{Encryption, Iso, Probe};
use crate::report::{Key, KeyResult, Trouble, PROTOCOL};
use crate::rpcs3::Console;

/// The folder of the IRD database that holds Redump's key collection, as
/// GitHub's contents API lists it. The zip's own name carries a count and a
/// date and changes with every update, so it is listed rather than named.
const COLLECTION: &str =
    "https://api.github.com/repos/FlexBy420/playstation_3_ird_database/contents/dkeys";

/// How long a downloaded collection is trusted before a disc it has no key
/// for sends this back for a newer one.
const FRESH: Duration = Duration::from_secs(24 * 60 * 60);

/// The key RPCS3 would find for this image by its name, where there is one:
/// `<image>.dkey` or `.key` beside it, then the same in RPCS3's folder — its
/// own order, from `iso_file_decryption::check_type`.
pub fn named_key(image: &Path, console: Option<&Console>) -> Option<PathBuf> {
    let stem = image.file_stem()?.to_string_lossy().into_owned();
    let beside = image.parent()?;
    let mut places = vec![
        beside.join(format!("{stem}.dkey")),
        beside.join(format!("{stem}.key")),
    ];
    if let Some(console) = console {
        places.push(console.keys().join(format!("{stem}.dkey")));
        places.push(console.keys().join(format!("{stem}.key")));
    }
    places.into_iter().find(|place| place.is_file())
}

/// Make sure RPCS3 has this image's key, fetching it if it has not.
pub fn fetch(image: &Path, console: &Console, cache: Option<&Path>) -> KeyResult {
    let iso = match Iso::open(image) {
        Ok(iso) => iso,
        Err(err) => return answer(Key::Missing, Some(Trouble::Other), format!("{err}")),
    };
    let Encryption::Encrypted { probe } = iso.encryption() else {
        return answer(
            Key::Unneeded,
            None,
            "the image is not encrypted".to_string(),
        );
    };
    if let Some(named) = named_key(image, Some(console)) {
        match read_key(&std::fs::read(&named).unwrap_or_default()) {
            Some(key) if probe.fits(&key) => {
                return answer(Key::Present, None, format!("{} fits", named.display()));
            }
            _ if named.starts_with(console.keys()) => {
                eprintln!("keys: {} does not fit and is replaced", named.display());
            }
            _ => {
                // A key of the user's own, beside the image, which RPCS3 reads
                // first. Not this helper's to overwrite.
                return answer(
                    Key::Missing,
                    Some(Trouble::Other),
                    format!("{} does not fit this disc", named.display()),
                );
            }
        }
    }

    let stem = image
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let Some(cache) = cache else {
        return answer(Key::Missing, Some(Trouble::Other), "no cache".to_string());
    };
    let kept = cache.join("keys");

    // The collection already downloaded first, where it is; a newer one only
    // when that has no key for this disc and is more than a day old.
    let mut tried_fresh = false;
    loop {
        let collection = match newest(&kept) {
            Some(collection) => collection,
            None => match download(&kept) {
                Ok(collection) => {
                    tried_fresh = true;
                    collection
                }
                Err(trouble) => return answer(Key::Missing, Some(trouble.0), trouble.1),
            },
        };
        match find(&collection, &stem, &probe) {
            Ok(Some((name, key))) => {
                return match save(console, &stem, &key) {
                    Ok(at) => answer(
                        Key::Present,
                        None,
                        format!("Redump's {name} fits; saved as {}", at.display()),
                    ),
                    Err(err) => answer(Key::Missing, Some(crate::net::disk(&err)), err.to_string()),
                };
            }
            Ok(None) => {}
            Err(err) => eprintln!("keys: {} could not be read: {err}", collection.display()),
        }
        let stale = std::fs::metadata(&collection)
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|when| SystemTime::now().duration_since(when).ok())
            .is_none_or(|age| age > FRESH);
        if tried_fresh || !stale {
            return answer(
                Key::Missing,
                Some(Trouble::Other),
                "no key in Redump's collection fits this disc".to_string(),
            );
        }
        // Once more, with whatever is newest.
        let _ = std::fs::remove_file(&collection);
    }
}

/// Every key in the collection, the one named like the image first, and the
/// first that fits.
fn find(
    collection: &Path,
    stem: &str,
    probe: &Probe,
) -> std::io::Result<Option<(String, [u8; 16])>> {
    let entries = crate::zip::entries(collection)?;
    let mut order: Vec<&crate::zip::Entry> = entries
        .iter()
        .filter(|entry| {
            let name = entry.file_name().to_lowercase();
            name.ends_with(".key") || name.ends_with(".dkey")
        })
        .collect();
    order.sort_by_key(|entry| {
        let name = entry.file_name();
        let base = name.rsplit_once('.').map_or(name, |(base, _)| base);
        base != stem
    });
    for entry in order {
        if entry.size > 64 {
            continue;
        }
        let mut reader = crate::zip::EntryReader::open(collection, entry)?;
        let mut bytes = Vec::new();
        reader.stream().read_to_end(&mut bytes)?;
        if let Some(key) = read_key(&bytes) {
            if probe.fits(&key) {
                return Ok(Some((entry.file_name().to_string(), key)));
            }
        }
    }
    Ok(None)
}

/// A key file's key: sixteen raw bytes (`.key`), or thirty-two hex digits
/// (`.dkey`) — RPCS3 reads both by their length, and so does this.
fn read_key(bytes: &[u8]) -> Option<[u8; 16]> {
    if bytes.len() == 16 {
        return bytes.try_into().ok();
    }
    let text = std::str::from_utf8(bytes).ok()?.trim();
    if text.len() != 32 {
        return None;
    }
    let mut key = [0u8; 16];
    for (index, pair) in text.as_bytes().chunks(2).enumerate() {
        key[index] = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(key)
}

/// Where RPCS3 will find it: `<image name>.key` in its own folder, sixteen raw
/// bytes, written beside and renamed over.
fn save(console: &Console, stem: &str, key: &[u8; 16]) -> std::io::Result<PathBuf> {
    let folder = console.keys();
    std::fs::create_dir_all(&folder)?;
    let at = folder.join(format!("{stem}.key"));
    let partial = folder.join(format!(".{stem}.key.partial"));
    std::fs::write(&partial, key)?;
    std::fs::rename(&partial, &at)?;
    Ok(at)
}

/// The newest collection already downloaded.
fn newest(kept: &Path) -> Option<PathBuf> {
    let listing = std::fs::read_dir(kept).ok()?;
    listing
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "zip"))
        .max_by_key(|path| {
            std::fs::metadata(path)
                .and_then(|meta| meta.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH)
        })
}

/// Download the collection as it is today, replacing any kept before.
fn download(kept: &Path) -> Result<PathBuf, (Trouble, String)> {
    let agent = crate::net::agent();
    let listing: serde_json::Value = agent
        .get(COLLECTION)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|err| (crate::net::trouble(&err), err.to_string()))?
        .body_mut()
        .read_to_string()
        .map_err(|err| {
            (
                Trouble::Offline,
                format!("the listing did not arrive: {err}"),
            )
        })
        .and_then(|text| {
            serde_json::from_str(&text).map_err(|err| {
                (
                    Trouble::Other,
                    format!("the listing could not be read: {err}"),
                )
            })
        })?;
    let (name, url) = listing
        .as_array()
        .into_iter()
        .flatten()
        .find_map(|entry| {
            let name = entry.get("name")?.as_str()?;
            let url = entry.get("download_url")?.as_str()?;
            name.to_lowercase()
                .ends_with(".zip")
                .then(|| (name.to_string(), url.to_string()))
        })
        .ok_or_else(|| {
            (
                Trouble::Other,
                "the listing names no collection".to_string(),
            )
        })?;
    let mut response = agent
        .get(&url)
        .call()
        .map_err(|err| (crate::net::trouble(&err), err.to_string()))?;
    let mut bytes = Vec::new();
    response
        .body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .reader()
        .read_to_end(&mut bytes)
        .map_err(|err| {
            (
                Trouble::Offline,
                format!("the collection did not arrive: {err}"),
            )
        })?;

    std::fs::create_dir_all(kept).map_err(|err| (crate::net::disk(&err), err.to_string()))?;
    if let Ok(listing) = std::fs::read_dir(kept) {
        for old in listing.filter_map(Result::ok) {
            let _ = std::fs::remove_file(old.path());
        }
    }
    let at = kept.join(&name);
    let partial = kept.join(format!(".{name}.partial"));
    std::fs::File::create(&partial)
        .and_then(|mut file| file.write_all(&bytes))
        .and_then(|()| std::fs::rename(&partial, &at))
        .map_err(|err| (crate::net::disk(&err), err.to_string()))?;
    eprintln!("keys: downloaded {name} ({} bytes)", bytes.len());
    Ok(at)
}

fn answer(key: Key, trouble: Option<Trouble>, note: String) -> KeyResult {
    KeyResult {
        protocol: PROTOCOL,
        key,
        trouble,
        note,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iso::{fixture, SECTOR};

    fn lic() -> Vec<u8> {
        let mut data = b"PS3LICDA".to_vec();
        data.resize(4096, 0x5a);
        data
    }

    /// An encrypted image under `key`, named as the user named theirs.
    fn encrypted(dir: &Path, name: &str, key: &[u8; 16]) -> PathBuf {
        let mut built = fixture::build(
            "BLUS-30464",
            &[("PS3_GAME/LICDIR/LIC.DAT", &lic())],
            &[(32, 33)],
        );
        for sector in 32..=33u32 {
            let at = sector as usize * SECTOR as usize;
            crate::crypt::encrypt_sector(key, sector, &mut built.bytes[at..at + SECTOR as usize]);
        }
        let at = dir.join(name);
        std::fs::File::create(&at)
            .unwrap()
            .write_all(&built.bytes)
            .unwrap();
        at
    }

    #[test]
    fn both_spellings_of_a_key_file_are_read() {
        let key = [0xabu8; 16];
        assert_eq!(read_key(&key), Some(key));
        assert_eq!(read_key(b"abababababababababababababababab\n"), Some(key));
        assert_eq!(read_key(b"not a key"), None);
    }

    /// The Skate 3 case: the collection names the key after a dump called
    /// something else, and it is still the one found — by what it decrypts.
    #[test]
    fn the_key_that_fits_is_found_whatever_it_is_called() {
        let dir = tempfile::tempdir().unwrap();
        let key = [0x5cu8; 16];
        let image = encrypted(dir.path(), "Skate 3 (USA) (En,Fr,Es).iso", &key);
        let collection = crate::zip::build(
            &[
                ("Skate 3 (Japan).key", &[0x11u8; 16]),
                ("Skate 3 (USA, Asia) (En,Fr,Es).key", &key),
                ("Tekken 6 (USA).dkey", b"22222222222222222222222222222222"),
            ],
            true,
        );
        let cache = dir.path().join("cache");
        std::fs::create_dir_all(cache.join("keys")).unwrap();
        std::fs::write(cache.join("keys/Keys (3).zip"), collection).unwrap();
        let console = Console::of(&dir.path().join("rpcs3"));

        let result = fetch(&image, &console, Some(&cache));
        assert_eq!(result.key, Key::Present, "{}", result.note);
        let saved = console.keys().join("Skate 3 (USA) (En,Fr,Es).key");
        assert_eq!(std::fs::read(&saved).unwrap(), key);
        assert_eq!(named_key(&image, Some(&console)), Some(saved));

        // Asked again, the saved key is found and proven without the collection.
        std::fs::remove_dir_all(cache.join("keys")).unwrap();
        assert_eq!(fetch(&image, &console, Some(&cache)).key, Key::Present);
    }

    /// The real collection, over the network: a disc encrypted under Tekken 6
    /// (USA)'s published key and named something else is still given that
    /// key. `cargo test -p lxb-rpcs3 -- --ignored` to run it; everything it
    /// writes is in a scratch directory.
    #[test]
    #[ignore = "reaches GitHub"]
    fn the_real_collection_proves_a_renamed_disc() {
        let dir = tempfile::tempdir().unwrap();
        let key = read_key(b"def2b0fb1a35bcfcb1e8b55360162a43").unwrap();
        let image = encrypted(dir.path(), "my tekken.iso", &key);
        let console = Console::of(&dir.path().join("rpcs3"));
        let cache = dir.path().join("cache");
        let result = fetch(&image, &console, Some(&cache));
        assert_eq!(result.key, Key::Present, "{}", result.note);
        assert!(result.note.contains("Tekken 6 (USA)"), "{}", result.note);
        assert_eq!(
            std::fs::read(console.keys().join("my tekken.key")).unwrap(),
            key
        );
    }

    /// Both of the user's own images: decrypted already, and never given a
    /// key, because a key would break them.
    #[test]
    fn a_decrypted_image_is_never_given_a_key() {
        let dir = tempfile::tempdir().unwrap();
        let built = fixture::build(
            "BLUS-30359",
            &[("PS3_GAME/LICDIR/LIC.DAT", &lic())],
            &[(32, 33)],
        );
        let image = dir.path().join("Tekken 6 (USA).iso");
        std::fs::write(&image, &built.bytes).unwrap();
        let console = Console::of(&dir.path().join("rpcs3"));
        let result = fetch(&image, &console, Some(&dir.path().join("cache")));
        assert_eq!(result.key, Key::Unneeded);
        assert!(!console.keys().exists());
    }
}
