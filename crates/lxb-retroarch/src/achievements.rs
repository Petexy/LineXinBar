//! Read-only RetroAchievements integration. One private stdin request; JSON events on stdout.
//! Passwords are never command arguments. Only RetroArch awards achievements.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    ffi::CString,
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

struct Sensitive(Value);
impl std::ops::Deref for Sensitive {
    type Target = Value;
    fn deref(&self) -> &Value {
        &self.0
    }
}
impl Drop for Sensitive {
    fn drop(&mut self) {
        fn wipe(v: &mut Value) {
            match v {
                Value::String(s) => unsafe { s.as_bytes_mut().fill(0) },
                Value::Array(a) => a.iter_mut().for_each(wipe),
                Value::Object(o) => o.values_mut().for_each(wipe),
                _ => {}
            }
        }
        wipe(&mut self.0);
        std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
    }
}

const API: &str = "https://retroachievements.org/dorequest.php";
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn base(variable: &str, fallback: &str) -> Result<PathBuf, String> {
    std::env::var_os(variable)
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(fallback)))
        .ok_or_else(|| "Home directory is unavailable".into())
}
/// `$XDG_CACHE_HOME/lxb/retroachievements`, beside the shell's other caches.
fn cache_root() -> Result<PathBuf, String> {
    Ok(base("XDG_CACHE_HOME", ".cache")?.join("lxb/retroachievements"))
}
/// `$XDG_CONFIG_HOME/lxb/retroachievements/account.json`, under the directory
/// every other setting of this shell is kept in. Its own directory rather than
/// a file beside `shell.toml`, because the directory holding a token is made
/// private and `lxb/` itself is not a secret.
fn account_path() -> Result<PathBuf, String> {
    Ok(base("XDG_CONFIG_HOME", ".config")?.join("lxb/retroachievements/account.json"))
}
fn private_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("Invalid cache path")?;
    std::fs::create_dir_all(parent).map_err(|_| "Could not create the account directory")?;
    // Existing RetroArch directories are not made private: only files we write carry secrets.
    if path.extension().is_some_and(|e| e == "json") {
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| "Could not protect the account directory")?;
    }
    let temp = path.with_extension(format!("lxb-{}", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)
        .map_err(|_| "Could not save account data")?;
    let result = file
        .write_all(bytes)
        .and_then(|_| file.sync_all())
        .and_then(|_| std::fs::rename(&temp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result.map_err(|_| "Could not save account data".into())
}
fn read(path: &Path) -> Option<Value> {
    let mut bytes = std::fs::read(path).ok()?;
    let parsed = serde_json::from_slice(&bytes).ok();
    bytes.fill(0);
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
    parsed
}
fn save(path: &Path, value: &Value) -> Result<(), String> {
    let mut bytes = serde_json::to_vec(value).map_err(|_| "Could not encode account data")?;
    let result = private_write(path, &bytes);
    bytes.fill(0);
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
    result
}
fn emit(value: Value) {
    let value = Sensitive(value);
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", value.0);
    let _ = out.flush();
}
fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .user_agent(concat!(
            "LXB/",
            env!("CARGO_PKG_VERSION"),
            " RetroArch companion"
        ))
        .build()
        .into()
}
fn api(fields: &[(&str, &str)]) -> Result<Value, String> {
    let mut response = agent()
        .post(API)
        .send_form(fields.iter().copied())
        .map_err(|_| "RetroAchievements could not be reached. Please try again.")?;
    let mut bytes = response
        .body_mut()
        .with_config()
        .limit(8 * 1024 * 1024)
        .read_to_vec()
        .map_err(|_| "RetroAchievements returned an unreadable response")?;
    let parsed = serde_json::from_slice(&bytes);
    bytes.fill(0);
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
    let value: Value = parsed.map_err(|_| "RetroAchievements returned an invalid response")?;
    if value["Success"].as_bool() != Some(true) {
        // Do not echo arbitrary remote error text into diagnostics (it may contain credentials).
        return Err(if value["Code"].as_str().is_some_and(|c| {
            c.contains("credential") || c.contains("token") || c.contains("password")
        }) || value["Error"]
            .as_str()
            .is_some_and(|e| e.starts_with("Credentials invalid"))
        {
            "Your RetroAchievements login needs to be renewed."
        } else {
            "RetroAchievements declined the request. Check your login and try again."
        }
        .into());
    }
    Ok(value)
}
fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str, String> {
    v[key]
        .as_str()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| format!("Missing {key} in RetroAchievements response"))
}
fn safe(value: &str) -> bool {
    !value.is_empty()
        && value.len() < 256
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_- .".contains(&c))
        && !value.contains("..")
}
fn account() -> Result<Sensitive, String> {
    read(&account_path()?)
        .filter(|v| {
            safe(v["user"].as_str().unwrap_or_default())
                && safe(v["token"].as_str().unwrap_or_default())
        })
        .map(Sensitive)
        .ok_or_else(|| "Sign in to RetroAchievements first.".into())
}
fn account_cache(user: &str) -> Result<PathBuf, String> {
    if !safe(user) {
        return Err("Invalid account name".into());
    }
    Ok(cache_root()?.join(user.to_ascii_lowercase()))
}
fn authenticated(operation: &str, extra: &[(&str, &str)]) -> Result<Value, String> {
    let a = account()?;
    let mut fields = vec![
        ("r", operation),
        ("u", text(&a, "user")?),
        ("t", text(&a, "token")?),
    ];
    fields.extend_from_slice(extra);
    api(&fields)
}
fn update_config(old: &str, values: &[(&str, &str)]) -> String {
    let mut out = old
        .lines()
        .filter(|line| {
            !line
                .split_once('=')
                .is_some_and(|(key, _)| values.iter().any(|(wanted, _)| key.trim() == *wanted))
        })
        .map(|s| format!("{s}\n"))
        .collect::<String>();
    for (key, value) in values {
        out.push_str(&format!("{key} = \"{value}\"\n"));
    }
    out
}
fn configure(user: &str, token: &str) -> Result<(), String> {
    if (!user.is_empty() && !safe(user)) || (!token.is_empty() && !safe(token)) {
        return Err("Invalid account credentials".into());
    }
    // RetroArch saves its in-memory settings on exit; prevent it overwriting a newly linked account.
    if std::fs::read_dir("/proc")
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .any(|p| {
            std::fs::read_to_string(p.path().join("comm")).is_ok_and(|s| s.trim() == "retroarch")
        })
    {
        return Err("Close RetroArch, then try again to finish configuring your account.".into());
    }
    let installation = crate::find::installation().ok_or("RetroArch is not installed")?;
    let dir =
        crate::find::config_dir(&installation).ok_or("RetroArch configuration is unavailable")?;
    configure_at(&dir, user, token)
}

fn read_config(path: &Path) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(_) => {
            Err("RetroArch configuration could not be read; no settings were replaced".into())
        }
    }
}

fn configure_at(dir: &Path, user: &str, token: &str) -> Result<(), String> {
    let config = dir.join("retroarch.cfg");
    let keychain = dir.join("retroarch-keychain.cfg");
    let private = if keychain.exists() {
        &keychain
    } else {
        &config
    };
    let values = [
        ("cheevos_username", user),
        ("cheevos_token", token),
        ("cheevos_password", ""),
    ];
    // Read every affected file before changing either of them.
    let current = read_config(&config)?;
    let old = if private == &config {
        current.clone()
    } else {
        read_config(private)?
    };
    private_write(private, update_config(&old, &values).as_bytes())?;
    let old = if private == &config {
        update_config(&current, &values)
    } else {
        current
    };
    let mut changes = vec![(
        "cheevos_enable",
        if token.is_empty() { "false" } else { "true" },
    )];
    if private != &config {
        changes.extend([
            ("cheevos_username", ""),
            ("cheevos_token", ""),
            ("cheevos_password", ""),
        ]);
    }
    private_write(&config, update_config(&old, &changes).as_bytes())
}

#[derive(Clone, Serialize, Deserialize)]
struct Game {
    id: u32,
    #[serde(default)]
    at: u64,
    title: String,
    console: String,
    /// The mark to draw beside that console's name, as the shell looks it up.
    /// Carried so the column can be grouped by machine without the shell
    /// having to keep a second table of consoles — see `report::Console::glyph`.
    #[serde(default)]
    glyph: Option<String>,
    path: String,
    picture: Option<String>,
    total: Option<u32>,
    unlocked: Option<u32>,
    hardcore: Option<u32>,
    issue: Option<String>,
}
/// This table's own console names against RetroAchievements' console numbers.
///
/// Read both ways round, which is why it is a table rather than the `match` it
/// used to be: a ROM on the disk is asked about by number, and the account's
/// own collection arrives *as* numbers and has to be turned back into a console
/// somebody recognises. A console missing from here is a console this
/// integration says it cannot identify rather than one it guesses at — see
/// [`console_id`]'s callers, which report the limitation.
const CONSOLE_NUMBERS: &[(&str, u32)] = &[
    ("megadrive", 1),
    ("n64", 2),
    ("snes", 3),
    ("gb", 4),
    ("gba", 5),
    ("gbc", 6),
    ("nes", 7),
    ("pce", 8),
    ("segacd", 9),
    ("32x", 10),
    ("sms", 11),
    ("psx", 12),
    ("lynx", 13),
    ("ngp", 14),
    ("gg", 15),
    ("gc", 16),
    ("jaguar", 17),
    ("nds", 18),
    ("wii", 19),
    ("ps2", 21),
    ("pokemini", 24),
    ("atari2600", 25),
    ("dos", 26),
    ("arcade", 27),
    ("vb", 28),
    ("msx", 29),
    ("c64", 30),
    ("sg1000", 33),
    ("amiga", 35),
    ("saturn", 39),
    ("dreamcast", 40),
    ("psp", 41),
    ("3do", 43),
    ("colecovision", 44),
    ("intellivision", 45),
    ("vectrex", 46),
    ("pcfx", 49),
    ("atari5200", 50),
    ("atari7800", 51),
    ("wonderswan", 53),
    ("zxspectrum", 59),
    ("3ds", 62),
    ("fds", 81),
];

/// The stem this package files a console's drawing under — `nes` out of
/// `lxb:console-nes` — which is the name [`CONSOLE_NUMBERS`] is keyed by.
fn stem(glyph: &str) -> &str {
    glyph
        .strip_prefix("console-")
        .unwrap_or(glyph)
        .trim_start_matches("lxb:console-")
}

/// What RetroAchievements calls the console a folder of this name is.
fn console_id(key: &str) -> Option<u32> {
    let machine = crate::consoles::machine(key)?;
    let wanted = stem(machine.glyph);
    CONSOLE_NUMBERS
        .iter()
        .find_map(|(name, id)| (*name == wanted).then_some(*id))
}

/// The console one of those numbers is, as this package names it.
///
/// `None` where the number is one of the several dozen consoles
/// RetroAchievements hosts that nothing in [`crate::consoles`] has a folder
/// name or a drawing for. The caller says so rather than inventing a name.
fn console_named(id: u32) -> Option<&'static crate::consoles::Machine> {
    let wanted = CONSOLE_NUMBERS
        .iter()
        .find_map(|(name, number)| (*number == id).then_some(*name))?;
    crate::consoles::CONSOLES
        .iter()
        .find(|machine| stem(machine.glyph) == wanted)
}
extern "C" {
    fn lxb_achievement_hash(
        output: *mut std::ffi::c_char,
        console: u32,
        path: *const std::ffi::c_char,
        data: *const u8,
        size: usize,
    ) -> i32;
}
fn hash(path: &Path, console: u32) -> Result<String, String> {
    use std::os::unix::ffi::OsStrExt;
    let mut cpath = CString::new(path.as_os_str().as_bytes()).map_err(|_| "Invalid ROM path")?;
    let mut output = [0u8; 33];
    let mut data = Vec::new();
    // Arcade ZIP identities are names; other ZIPs must contain one unambiguous ROM.
    if console != 27
        && path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
    {
        let file = std::fs::File::open(path).map_err(|_| "ROM could not be opened")?;
        file.take(128 * 1024 * 1024 + 1)
            .read_to_end(&mut data)
            .map_err(|_| "ROM could not be read")?;
        if data.len() > 128 * 1024 * 1024 {
            return Err("Archive is too large to identify".into());
        }
        let (name, rom) = crate::zip::single_rom(&data)?;
        cpath = CString::new(name).map_err(|_| "Invalid archived ROM name")?;
        data = rom;
    }
    // C owns no Rust allocation and keeps neither pointer beyond this call.
    let ok = unsafe {
        lxb_achievement_hash(
            output.as_mut_ptr().cast(),
            console,
            cpath.as_ptr(),
            if data.is_empty() {
                std::ptr::null()
            } else {
                data.as_ptr()
            },
            data.len(),
        )
    };
    if ok == 0 {
        return Err("This ROM format could not be identified for achievements".into());
    }
    String::from_utf8(output[..32].to_vec()).map_err(|_| "Invalid ROM hash".into())
}
/// Include descriptor dependencies: replacing a track must invalidate its CUE/M3U identity.
fn fingerprint(path: &Path, console: u32) -> Option<String> {
    use std::hash::{Hash, Hasher};
    fn add(
        path: &Path,
        depth: u8,
        seen: &mut BTreeSet<PathBuf>,
        sum: &mut std::collections::hash_map::DefaultHasher,
    ) {
        if depth > 8 || !seen.insert(path.to_path_buf()) {
            return;
        }
        path.hash(sum);
        let Ok(meta) = std::fs::metadata(path) else {
            "missing".hash(sum);
            return;
        };
        meta.len().hash(sum);
        meta.modified().ok().hash(sum);
        let extension = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let parent = path.parent().unwrap_or(Path::new("."));
        if extension == "ccd" {
            add(&path.with_extension("img"), depth + 1, seen, sum);
            add(&path.with_extension("sub"), depth + 1, seen, sum);
        }
        if !matches!(extension.as_str(), "cue" | "gdi" | "m3u") {
            return;
        }
        let mut text = String::new();
        if std::fs::File::open(path)
            .and_then(|f| f.take(131072).read_to_string(&mut text))
            .is_err()
        {
            return;
        }
        text.hash(sum);
        for line in text.lines().take(128) {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let named = if extension == "m3u" {
                Some(line)
            } else if let Some(file) = line
                .strip_prefix("FILE ")
                .or_else(|| line.strip_prefix("file "))
            {
                Some(file.trim())
            } else if extension == "gdi" {
                line.splitn(5, char::is_whitespace).nth(4).map(str::trim)
            } else {
                None
            };
            if let Some(named) = named {
                let named = if extension == "m3u" {
                    named
                } else if let Some(quoted) = named.strip_prefix('"') {
                    quoted.split('"').next().unwrap_or("")
                } else {
                    named.split_whitespace().next().unwrap_or("")
                };
                if !named.is_empty() {
                    add(&parent.join(named), depth + 1, seen, sum);
                }
            }
        }
    }
    std::fs::metadata(path).ok()?;
    let mut sum = std::collections::hash_map::DefaultHasher::new();
    console.hash(&mut sum);
    add(path, 0, &mut BTreeSet::new(), &mut sum);
    Some(format!("v2:{:016x}", sum.finish()))
}

fn library(folder: &Path) -> Result<(), String> {
    let a = account()?;
    let cache = account_cache(text(&a, "user")?)?;
    let collection = crate::scan::library(folder);
    if collection.unreadable.is_some() {
        return Err("The ROM folder could not be read".into());
    }
    let mappings_path = cache_root()?.join("mappings.json");
    let mut mappings = read(&mappings_path).unwrap_or(json!({}));
    let old: Vec<Game> = read(&cache.join("library.json"))
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    let mut games = Vec::new();
    let mut seen = BTreeSet::new();
    for console in collection.consoles {
        let system = console_id(&console.key);
        let progress = if let Some(id) = system {
            Some(authenticated("allprogress", &[("c", &id.to_string())])?)
        } else {
            None
        };
        for rom in console.roms {
            let path = Path::new(&rom.path);
            let mut game = Game {
                id: 0,
                at: 0,
                title: rom.title,
                console: console.title.clone(),
                glyph: console.glyph.clone(),
                path: rom.path.clone(),
                picture: rom.boxart,
                total: None,
                unlocked: None,
                hardcore: None,
                issue: None,
            };
            let identity = fingerprint(path, system.unwrap_or(0));
            let mapping = &mappings[&rom.path];
            let cached = identity.as_deref() == mapping["identity"].as_str()
                && mapping["at"]
                    .as_u64()
                    .is_some_and(|t| now().saturating_sub(t) < 86400 * 7);
            let matched = if cached {
                Ok(mapping["id"].as_u64().unwrap_or(0) as u32)
            } else if let Some(system) = system {
                hash(path, system)
                    .and_then(|hash| api(&[("r", "gameid"), ("m", &hash)]))
                    .and_then(|v| {
                        v["GameID"]
                            .as_u64()
                            .map(|id| id as u32)
                            .ok_or("Missing game identity".into())
                    })
            } else {
                Err("This console is not supported by the achievement matcher".into())
            };
            match matched {
                Ok(id) => {
                    game.id = id;
                    let at = if cached {
                        mapping["at"].as_u64().unwrap_or(now())
                    } else {
                        now()
                    };
                    mappings[&rom.path] = json!({"identity":identity,"id":id,"at":at});
                    if id == 0 {
                        game.issue = Some("This ROM is not recognized by RetroAchievements".into());
                    }
                }
                Err(why) => {
                    if identity.as_deref() == mapping["identity"].as_str()
                        && mapping["id"].as_u64().unwrap_or(0) > 0
                    {
                        game.id = mapping["id"].as_u64().unwrap() as u32;
                    } else {
                        game.issue = Some(why);
                    }
                }
            }
            if game.id != 0 {
                if !seen.insert(game.id) {
                    continue;
                }
                if let Some(p) = progress
                    .as_ref()
                    .and_then(|v| v["Response"].get(game.id.to_string()))
                {
                    game.at = now();
                    game.total = p["Achievements"].as_u64().map(|n| n as u32);
                    game.unlocked = p["Unlocked"].as_u64().map(|n| n as u32);
                    game.hardcore = p["UnlockedHardcore"].as_u64().map(|n| n as u32);
                } else if let Some(previous) = old.iter().find(|g| g.id == game.id) {
                    game.total = previous.total;
                    game.unlocked = previous.unlocked;
                    game.hardcore = previous.hardcore;
                    game.at = previous.at;
                }
                if game.total.is_none() {
                    if let Ok(page) = page(game.id, false) {
                        game.at = page["at"].as_u64().unwrap_or(0);
                        game.total = page["achievements"].as_array().map(|a| a.len() as u32);
                        game.unlocked = page["unlocked"].as_u64().map(|n| n as u32);
                        game.hardcore = page["hardcore"].as_u64().map(|n| n as u32);
                    }
                }
            }
            emit(json!({"event":"game","game":game}));
            games.push(game);
            if games.len() % 16 == 1 {
                save(&mappings_path, &mappings)?;
                let mut partial = old.clone();
                for game in &games {
                    if let Some(previous) = partial.iter_mut().find(|g| g.path == game.path) {
                        *previous = game.clone();
                    } else {
                        partial.push(game.clone());
                    }
                }
                save(
                    &cache.join("library.json"),
                    &serde_json::to_value(partial).map_err(|_| "Invalid library")?,
                )?;
            }
        }
    }
    save(&mappings_path, &mappings)?;
    save(
        &cache.join("library.json"),
        &serde_json::to_value(&games).map_err(|_| "Invalid library")?,
    )?;
    emit(json!({"event":"library","games":games}));
    Ok(())
}
/// One picture off RetroAchievements' media host, kept in `kept`.
///
/// `shelf` is the folder the site holds it in and `kept` the one this cache
/// does; they are named separately because the second of them is a path
/// somebody's disk already has files under. `published` is the address the
/// site gave for it and is used only when it is on that host under that shelf;
/// otherwise the address is composed here. A remote response that is not a PNG
/// is not written at all, and two megabytes is the most any of these is
/// allowed to be — both because a drawing on a row is the one thing here that
/// a stranger's server decides the size of.
fn picture(shelf: &str, kept: &str, filename: &str, published: Option<&str>) -> Option<PathBuf> {
    let path = cache_root().ok()?.join(kept).join(filename);
    if path.is_file() {
        return Some(path);
    }
    let host = format!("https://media.retroachievements.org/{shelf}/");
    let fallback = format!("{host}{filename}");
    let url = published
        .filter(|url| url.starts_with(&host))
        .unwrap_or(&fallback);
    let mut response = agent().get(url).call().ok()?;
    let bytes = response
        .body_mut()
        .with_config()
        .limit(2 * 1024 * 1024)
        .read_to_vec()
        .ok()?;
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return None;
    }
    private_write(&path, &bytes).ok()?;
    Some(path)
}
/// One game on somebody's RetroAchievements account.
///
/// Not a [`Game`], and deliberately a second record rather than that one with
/// its fields made optional: a `Game` is a file on this disk that has been
/// matched to a set, and everything about it — the path, the box art the
/// thumbnail server drew, the reason a match failed — is about the file. This
/// is the other way round. It is a set the account has earned something in,
/// and whether there is a ROM for it anywhere is not part of it.
#[derive(Serialize, Deserialize, Clone, Default)]
struct Owned {
    id: u32,
    #[serde(default)]
    at: u64,
    title: String,
    console: String,
    console_id: u32,
    #[serde(default)]
    glyph: Option<String>,
    /// The site's own mark for the game: a small square, the same one it draws
    /// beside an achievement. Kept as what a row wears where there is no cover
    /// to be had, and never confused with one — a column of squares in
    /// portrait cards is what this replaced.
    #[serde(default)]
    icon: Option<String>,
    /// The cover, off libretro's thumbnail server — the very picture a ROM of
    /// this game sitting in the folder would wear. The site publishes covers
    /// on its own pages and answers none of them through the emulator API, and
    /// this shell already fetches box art for every console it can name, so
    /// the picture a row wants is one it can already get. See [`crate::art`].
    #[serde(default)]
    cover: Option<String>,
    #[serde(default)]
    total: Option<u32>,
    #[serde(default)]
    unlocked: Option<u32>,
    #[serde(default)]
    hardcore: Option<u32>,
}

/// How long to leave between the requests one sweep is made of.
///
/// RetroAchievements answers a burst of these with 429 and nothing else — it
/// was asked forty-three times in three seconds during this work and refused
/// the last of them — so the sweep is paced rather than retried. Nobody is
/// waiting on it: the column already holds the last one off the disk and fills
/// in behind them.
const PACE: Duration = Duration::from_millis(400);

/// How long a whole sweep stands before every console is asked about again.
const SWEEP_FOR: u64 = 6 * 3600;

/// The highest console number a whole sweep asks about.
///
/// A number rather than [`CONSOLE_NUMBERS`], which is the *other* question:
/// that table is the consoles this integration can run a ROM for, and a game
/// the account earned achievements in somewhere else is still that account's
/// game — nothing in this column launches anything. RetroAchievements hosts
/// around a hundred consoles and adds to them; a number it has never used
/// answers `Success` with nothing in it rather than an error, so the headroom
/// costs one small request each and the sweep does not go stale every time the
/// site takes on another machine. A console the table cannot name gives a row
/// with no console on it rather than no row.
const CONSOLES_AT_MOST: u32 = 120;

/// Give one game its name and the site's own mark for it.
///
/// Once per game for as long as the cache lasts, because neither ever changes:
/// `allprogress` answers in numbers, and a number is not a row. A game that
/// cannot be named is left unnamed rather than called after its number, and
/// [`collection`] keeps it out of the column until a later sweep can name it.
fn identify(owned: &mut Owned) {
    let Ok(data) = authenticated("patch", &[("g", &owned.id.to_string())]) else {
        return;
    };
    let patch = &data["PatchData"];
    if let Some(title) = patch["Title"].as_str().filter(|t| !t.is_empty()) {
        owned.title = title.to_string();
    }
    if let Some(at) = game_picture(patch["ImageIcon"].as_str(), patch["ImageIconURL"].as_str()) {
        owned.icon = Some(at.display().to_string());
    }
}

/// The cover for one game the account has played, if libretro has drawn one.
///
/// The same fetch a ROM in the folder goes through and into the same cache, so
/// a person who has both gets one picture rather than two — see
/// [`crate::art::fetch`], which is also where the trick of matching
/// `Tekken 4` against `Tekken 4 (Europe, Australia) (En,Fr,De,Es,It) (v2.00)`
/// lives. Only the box art is kept: the snap is the picture a shelf stands
/// behind its cursor, and nothing in the Trophies column has a place for one.
fn cover(agent: &ureq::Agent, shelves: &crate::art::Shelves, title: &str) -> Option<PathBuf> {
    let cache = crate::art::cache()?;
    if shelves.is_empty() {
        return None;
    }
    crate::art::fetch(agent, &cache, shelves, title).boxart
}

/// Everything the account has earned an achievement in, whatever is on the disk.
///
/// This is the half of the Trophies column that is somebody's RetroAchievements
/// profile rather than their ROM folder, and it is the half most people see:
/// achievements are earned on a machine, and the machine they were earned on is
/// not necessarily this one. [`library`] is the other half and neither replaces
/// the other — a ROM that is here *and* has been played appears once, as the
/// ROM, because that row can say which file it is.
///
/// There is no request on the site that answers "which games has this account
/// played". `allprogress` answers it one console at a time, so the sweep is
/// every console this package can name, paced by [`PACE`] and kept for
/// [`SWEEP_FOR`]. In between, only the consoles already known to hold something
/// are asked again, which is four or five requests rather than forty-three.
/// A console that fails is left as it was rather than emptied, and the sweep is
/// not stamped as done, so the next one is a full one.
fn collection() -> Result<(), String> {
    let a = account()?;
    let cache = account_cache(text(&a, "user")?)?;
    let path = cache.join("collection.json");
    let stored = read(&path).unwrap_or(json!({}));
    let known: Vec<Owned> = serde_json::from_value(stored["games"].clone()).unwrap_or_default();
    let swept = stored["at"].as_u64().unwrap_or(0);
    let whole = now().saturating_sub(swept) >= SWEEP_FOR;
    let asking: Vec<u32> = if whole {
        (1..=CONSOLES_AT_MOST).collect()
    } else {
        known
            .iter()
            .map(|game| game.console_id)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    };
    let mut found: Vec<Owned> = Vec::new();
    let mut settled: BTreeSet<u32> = BTreeSet::new();
    let mut refused = None;
    for (nth, console) in asking.iter().enumerate() {
        if nth > 0 {
            std::thread::sleep(PACE);
        }
        let progress = authenticated("allprogress", &[("c", &console.to_string())]);
        settled.insert(*console);
        let before = found.len();
        match progress {
            Ok(progress) => {
                let machine = console_named(*console);
                // One listing per console and only where a game on it wants a
                // cover, which is the same rule the ROM scan follows and for
                // the same reason: a shelf is a few thousand names, and most of
                // the hundred and twenty consoles asked about hold nothing.
                let mut shelves = None;
                for (game, counts) in progress["Response"].as_object().into_iter().flatten() {
                    let unlocked = counts["Unlocked"].as_u64().unwrap_or(0);
                    let hardcore = counts["UnlockedHardcore"].as_u64().unwrap_or(0);
                    // Every set on the console comes back; the account's own
                    // are the ones it has unlocked something in.
                    if unlocked == 0 && hardcore == 0 {
                        continue;
                    }
                    let Ok(id) = game.parse::<u32>() else {
                        continue;
                    };
                    let mut owned = known
                        .iter()
                        .find(|game| game.id == id)
                        .cloned()
                        .unwrap_or_default();
                    owned.id = id;
                    owned.at = now();
                    owned.console_id = *console;
                    owned.console = machine.map_or_else(String::new, |m| m.title.to_string());
                    owned.glyph = machine.map(|m| m.glyph.to_string());
                    owned.total = counts["Achievements"].as_u64().map(|n| n as u32);
                    owned.unlocked = Some(unlocked.max(hardcore) as u32);
                    owned.hardcore = Some(hardcore as u32);
                    if owned.title.is_empty() || owned.icon.is_none() {
                        std::thread::sleep(PACE);
                        identify(&mut owned);
                    }
                    // And the cover, which is a different server and a
                    // different question: libretro has one for most of what
                    // anybody has played and none at all for a console it has
                    // never listed, and a title it does not carry is a miss
                    // this asks about again next time rather than a fact.
                    if owned.cover.is_none() && !owned.title.is_empty() {
                        let shelves = shelves.get_or_insert_with(|| {
                            crate::art::cache().map_or_else(
                                || crate::art::shelves_here(Path::new(""), None),
                                |cache| crate::art::shelves(&agent(), &cache, machine, false),
                            )
                        });
                        owned.cover = cover(&agent(), shelves, &owned.title)
                            .map(|at| at.display().to_string());
                    }
                    if !owned.title.is_empty() {
                        emit(json!({"event":"owned","game":owned}));
                    }
                    found.push(owned);
                }
            }
            // Leave this console as it was rather than report the account has
            // lost it, and make sure the next sweep is a whole one.
            Err(why) => {
                refused = Some(why);
                found.extend(
                    known
                        .iter()
                        .filter(|game| game.console_id == *console)
                        .cloned(),
                );
            }
        }
        // Written as the sweep goes rather than at the end of it: opening a
        // game cancels this, and what has already been asked for should not
        // have to be asked for twice. Only where this console said something,
        // because most of the forty-three say nothing at all.
        if found.len() != before {
            save(
                &path,
                &json!({"at": swept, "games": gathered(&found, &known, &settled)}),
            )?;
        }
    }
    let games = gathered(&found, &known, &settled);
    if games.is_empty() {
        if let Some(why) = refused {
            return Err(why);
        }
    }
    save(
        &path,
        &json!({"at": if refused.is_none() && whole { now() } else { swept }, "games": games}),
    )?;
    emit(json!({"event":"collection","games":games
        .into_iter()
        .filter(|game| !game.title.is_empty())
        .collect::<Vec<_>>()}));
    Ok(())
}

/// What the sweep has found so far, with every console it has not reached left
/// exactly as the last one left it.
fn gathered(found: &[Owned], known: &[Owned], settled: &BTreeSet<u32>) -> Vec<Owned> {
    let mut games: Vec<Owned> = found.to_vec();
    games.extend(
        known
            .iter()
            .filter(|game| !settled.contains(&game.console_id))
            .cloned(),
    );
    games.sort_by_key(|game| game.id);
    games.dedup_by_key(|game| game.id);
    games
}

fn badge(name: &str, locked: bool, published: Option<&str>) -> Option<PathBuf> {
    if !safe(name) {
        return None;
    }
    picture(
        "Badge",
        "badges",
        &format!("{name}{}.png", if locked { "_lock" } else { "" }),
        published,
    )
}
/// A game's own small picture, named the way the site's patch names it.
///
/// `icon` is a path on the media host — `/Images/131219.png` — and only its
/// last part is used, checked by [`safe`] the way a badge's name is, because a
/// remote string is deciding a filename on this disk.
fn game_picture(icon: Option<&str>, published: Option<&str>) -> Option<PathBuf> {
    let filename = icon?.rsplit('/').next()?;
    let stem = filename.strip_suffix(".png")?;
    if !safe(stem) {
        return None;
    }
    picture("Images", "icons", filename, published)
}
/// Where RetroAchievements' own reserved identities begin.
///
/// Everything the site has ever published sits far below this; the range above
/// it is what a client may invent locally and what the server injects as a
/// notice. See [`parse_page`].
const LOCAL_ACHIEVEMENT: u64 = 100_000_000;

fn parse_page(data: &Value, soft: &Value, hard: &Value) -> Result<Value, String> {
    let soft: BTreeSet<u64> = soft["UserUnlocks"]
        .as_array()
        .ok_or("Missing achievement progress")?
        .iter()
        .filter_map(Value::as_u64)
        .collect();
    let hard: BTreeSet<u64> = hard["UserUnlocks"]
        .as_array()
        .ok_or("Missing Hardcore progress")?
        .iter()
        .filter_map(Value::as_u64)
        .collect();
    let definitions = data["PatchData"]["Achievements"]
        .as_array()
        .ok_or("Missing achievement definitions")?;
    let mut achievements = Vec::new();
    for a in definitions {
        if a["Flags"].as_u64() != Some(3) {
            continue;
        }
        let id = a["ID"].as_u64().ok_or("Missing achievement ID")?;
        // RetroAchievements writes its own notices into every set it hands to a
        // client it does not recognise — "Warning: Unknown Emulator", worth no
        // points and never unlockable. They are addressed to the emulator, and
        // this is not one: nothing here awards anything, and RetroArch, which
        // does, gets its own copy of the message. Listing them would be this
        // column inventing an achievement nobody can earn and counting it
        // against the set's total. The server files them above every real
        // identity, which is how they are told apart.
        if id >= LOCAL_ACHIEVEMENT {
            continue;
        }
        achievements.push(json!({"id":id,"title":text(a,"Title")?,"description":a["Description"].as_str().unwrap_or(""),"points":a["Points"].as_u64().unwrap_or(0),"badge":a["BadgeName"].as_str().unwrap_or(""),"url":a["BadgeURL"],"locked_url":a["BadgeLockedURL"],"unlocked":soft.contains(&id)||hard.contains(&id),"hardcore":hard.contains(&id)}));
    }
    let unlocked = achievements
        .iter()
        .filter(|a| a["unlocked"] == true)
        .count();
    let hardcore = achievements
        .iter()
        .filter(|a| a["hardcore"] == true)
        .count();
    // Stable partition preserves the set's order within each group.
    achievements.sort_by_key(|a| a["unlocked"] != true);
    Ok(json!({"achievements":achievements,"unlocked":unlocked,"hardcore":hardcore,"at":now()}))
}
fn page(id: u32, icons: bool) -> Result<Value, String> {
    let a = account()?;
    let path = account_cache(text(&a, "user")?)?.join(format!("{id}.json"));
    let old = read(&path);
    let mut page = if !icons
        && old.as_ref().is_some_and(|v| {
            v["at"]
                .as_u64()
                .is_some_and(|t| now().saturating_sub(t) < 300)
        }) {
        old.clone().unwrap()
    } else {
        let result = (|| {
            let game = id.to_string();
            let data = authenticated("patch", &[("g", &game)])?;
            let soft = authenticated("unlocks", &[("g", &game), ("h", "0")])?;
            let hard = authenticated("unlocks", &[("g", &game), ("h", "1")])?;
            parse_page(&data, &soft, &hard)
        })();
        match result {
            Ok(page) => page,
            Err(e) => {
                if let Some(page) = old {
                    page
                } else {
                    return Err(e);
                }
            }
        }
    };
    // A refreshed page keeps already cached badges on its first event.
    if let Some(achievements) = page["achievements"].as_array_mut() {
        for achievement in achievements {
            let name = achievement["badge"].as_str().unwrap_or_default();
            if safe(name) {
                let filename = format!(
                    "{name}{}.png",
                    if achievement["unlocked"] == true {
                        ""
                    } else {
                        "_lock"
                    }
                );
                let path = cache_root()?.join("badges").join(filename);
                if path.is_file() {
                    achievement["picture"] = json!(path);
                }
            }
        }
    }
    if icons {
        // Send the page before icons: they arrive progressively and never block browsing.
        emit(json!({"event":"page","id":id,"page":page}));
        let count = page["achievements"].as_array().map_or(0, Vec::len);
        for at in 0..count {
            let achievement = &mut page["achievements"][at];
            if let Some(picture) = badge(
                achievement["badge"].as_str().unwrap_or(""),
                achievement["unlocked"] != true,
                achievement[if achievement["unlocked"] == true {
                    "url"
                } else {
                    "locked_url"
                }]
                .as_str(),
            ) {
                achievement["picture"] = json!(picture);
                emit(
                    json!({"event":"icon","id":id,"achievement":achievement["id"],"picture":picture}),
                );
            }
            if at % 8 == 0 {
                save(&path, &page)?;
            }
        }
    }
    save(&path, &page)?;
    Ok(page)
}
pub fn forget() {
    if let Ok(path) = account_path() {
        let _ = std::fs::remove_file(path);
    }
}

pub fn run() {
    let result = (|| -> Result<(), String> {
        let mut input = String::new();
        std::io::stdin()
            .take(16384)
            .read_to_string(&mut input)
            .map_err(|_| "Cannot read achievement request")?;
        let parsed = serde_json::from_str(&input).map(Sensitive);
        // Clear the serialized copy promptly; request is never Debug-printed.
        unsafe {
            input.as_bytes_mut().fill(0);
        }
        let request = parsed.map_err(|_| "Invalid achievement request")?;
        match text(&request, "op")? {
            "login" => {
                let response = Sensitive(api(&[
                    ("r", "login2"),
                    ("u", text(&request, "user")?),
                    ("p", text(&request, "password")?),
                ])?);
                emit(
                    json!({"event":"login","user":text(&response,"User")?,"token":text(&response,"Token")?}),
                );
            }
            "activate" => {
                let user = text(&request, "user")?;
                let token = text(&request, "token")?;
                configure(user, token)?;
                save(&account_path()?, &json!({"user":user,"token":token}))?;
                emit(json!({"event":"account","user":user}));
            }
            "logout" => {
                configure("", "")?;
                let path = account_path()?;
                if path.exists() {
                    std::fs::remove_file(path).map_err(|_| "Could not remove saved account")?;
                }
                emit(json!({"event":"account","user":null}));
            }
            "status" => {
                let a = account().ok();
                let user = a.as_ref().and_then(|a| a["user"].as_str());
                emit(json!({"event":"account","user":user}));
                if let Some(user) = user {
                    let cache = account_cache(user)?;
                    let games = read(&cache.join("library.json")).unwrap_or(json!([]));
                    emit(json!({"event":"library","games":games}));
                    // The account's own collection is answered off the disk
                    // too, so a shell that has just started draws the column
                    // before the sweep has asked the site anything.
                    if let Some(collection) = read(&cache.join("collection.json")) {
                        emit(json!({"event":"collection","games":collection["games"]
                            .as_array()
                            .map(|games| games
                                .iter()
                                .filter(|game| game["title"].as_str().is_some_and(|t| !t.is_empty()))
                                .cloned()
                                .collect::<Vec<_>>())
                            .unwrap_or_default()}));
                    }
                    for file in std::fs::read_dir(cache)
                        .ok()
                        .into_iter()
                        .flatten()
                        .filter_map(Result::ok)
                    {
                        if let Some(id) = file
                            .path()
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .and_then(|s| s.parse::<u32>().ok())
                        {
                            if let Some(page) = read(&file.path()) {
                                emit(json!({"event":"page","id":id,"page":page}));
                            }
                        }
                    }
                }
            }
            "library" => library(Path::new(text(&request, "folder")?))?,
            "collection" => collection()?,
            "page" => {
                let id = request["id"]
                    .as_u64()
                    .filter(|id| *id > 0 && *id <= u32::MAX as u64)
                    .ok_or("Invalid game ID")? as u32;
                emit(json!({"event":"page","id":id,"page":page(id,true)?}));
            }
            _ => {
                return Err(
                    "This RetroArch helper does not support that achievement request".into(),
                )
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        emit(json!({"event":"error","message":error}));
    }
    emit(json!({"event":"done"}));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn config_preserves_unrelated_settings_and_removes_duplicates() {
        assert_eq!(
            update_config(
                "video_driver = \"vulkan\"\ncheevos_token = \"old\"\ncheevos_token = \"older\"\n",
                &[("cheevos_token", "new")]
            ),
            "video_driver = \"vulkan\"\ncheevos_token = \"new\"\n"
        );
    }
    #[test]
    fn hardcore_counts_once_and_keeps_set_order() {
        let data = json!({"PatchData":{"Achievements":[{"ID":1,"Title":"One","Flags":3},{"ID":2,"Title":"Two","Flags":3},{"ID":3,"Title":"Unofficial","Flags":5}]}});
        let p = parse_page(
            &data,
            &json!({"UserUnlocks":[]}),
            &json!({"UserUnlocks":[2]}),
        )
        .unwrap();
        assert_eq!(p["unlocked"], 1);
        assert_eq!(p["hardcore"], 1);
        assert_eq!(p["achievements"][0]["id"], 2);
        assert_eq!(p["achievements"].as_array().unwrap().len(), 2);
    }
    #[test]
    fn the_sites_own_notice_is_not_an_achievement() {
        // What `patch` hands back to a client it does not recognise, verbatim.
        let data = json!({"PatchData":{"Achievements":[
            {"ID":101000001,"Title":"Warning: Unknown Emulator","Flags":3,"Points":0},
            {"ID":275425,"Title":"You Disappoint Me","Flags":3,"Points":2},
        ]}});
        let p = parse_page(
            &data,
            &json!({"UserUnlocks":[]}),
            &json!({"UserUnlocks":[]}),
        )
        .unwrap();
        assert_eq!(p["achievements"].as_array().unwrap().len(), 1);
        assert_eq!(p["achievements"][0]["id"], 275425);
    }
    #[test]
    fn every_console_number_names_a_console_back() {
        for (name, id) in CONSOLE_NUMBERS {
            let machine = console_named(*id)
                .unwrap_or_else(|| panic!("{id} ({name}) names no console in the table"));
            assert_eq!(stem(machine.glyph), *name);
            assert_eq!(console_id(machine.aliases[0]), Some(*id));
        }
    }
    #[test]
    fn a_console_the_sweep_never_reached_keeps_its_games() {
        let played = |id: u32, console: u32| Owned {
            id,
            console_id: console,
            title: format!("Game {id}"),
            ..Owned::default()
        };
        let known = vec![played(1, 7), played(2, 12)];
        // The PlayStation half was asked about and answered; the NES half was
        // never reached, and what it held stands.
        let games = gathered(&[played(2, 12)], &known, &BTreeSet::from([12]));
        assert_eq!(
            games.iter().map(|g| g.id).collect::<Vec<_>>(),
            vec![1, 2],
            "a sweep that stopped early must not empty the rest of the account"
        );
        // Asked about and answered with nothing is the game being gone.
        let games = gathered(&[], &known, &BTreeSet::from([7, 12]));
        assert!(games.is_empty());
    }
    #[test]
    fn missing_progress_is_not_an_empty_set() {
        assert!(parse_page(&json!({}), &json!({}), &json!({})).is_err());
    }
    #[test]
    fn nes_identity_ignores_container_header() {
        let at = std::env::temp_dir().join(format!("lxb-ra-hash-{}", std::process::id()));
        std::fs::create_dir_all(&at).unwrap();
        let raw = at.join("raw.nes");
        let headed = at.join("headed.nes");
        let data = vec![0x42; 16384];
        std::fs::write(&raw, &data).unwrap();
        let mut header = vec![0u8; 16];
        header[..4].copy_from_slice(b"NES\x1a");
        header[4] = 1;
        header.extend_from_slice(&data);
        std::fs::write(&headed, &header).unwrap();
        let plain = hash(&raw, 7).unwrap();
        assert_eq!(plain.len(), 32);
        assert_eq!(plain, hash(&headed, 7).unwrap());
        std::fs::remove_dir_all(at).unwrap();
    }
    #[test]
    fn changing_a_track_invalidates_the_playlist() {
        let dir = std::env::temp_dir().join(format!("lxb-ra-tracks-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let playlist = dir.join("game.m3u");
        std::fs::write(&playlist, "game.cue\n").unwrap();
        std::fs::write(dir.join("game.cue"), "FILE \"track one.bin\" BINARY\n").unwrap();
        std::fs::write(dir.join("track one.bin"), b"one").unwrap();
        let old = fingerprint(&playlist, 12);
        std::fs::write(dir.join("track one.bin"), b"a different track").unwrap();
        assert_ne!(old, fingerprint(&playlist, 12));
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn configuring_both_retroarch_formats_preserves_other_settings() {
        let dir = std::env::temp_dir().join(format!("lxb-ra-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("retroarch.cfg");
        std::fs::write(&config,"video_driver = \"vulkan\"\ncheevos_hardcore_mode_enable = \"false\"\ncheevos_password = \"old\"\n").unwrap();
        configure_at(&dir, "Alice", "first_token").unwrap();
        let saved = std::fs::read_to_string(&config).unwrap();
        assert!(saved.contains("cheevos_token = \"first_token\""));
        assert!(saved.contains("cheevos_password = \"\""));
        assert!(saved.contains("cheevos_hardcore_mode_enable = \"false\""));
        assert_eq!(
            std::fs::metadata(&config).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let keychain = dir.join("retroarch-keychain.cfg");
        std::fs::write(&keychain, "webdav_password = \"preserved\"\n").unwrap();
        configure_at(&dir, "Bob", "second_token").unwrap();
        let saved = std::fs::read_to_string(&config).unwrap();
        assert!(!saved.contains("first_token"));
        assert!(!saved.contains("second_token"));
        let secrets = std::fs::read_to_string(&keychain).unwrap();
        assert!(secrets.contains("second_token"));
        assert!(secrets.contains("preserved"));
        configure_at(&dir, "", "").unwrap();
        assert!(!std::fs::read_to_string(&keychain)
            .unwrap()
            .contains("second_token"));
        assert!(std::fs::read_to_string(&config)
            .unwrap()
            .contains("cheevos_enable = \"false\""));
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn unreadable_configuration_is_never_replaced() {
        let dir =
            std::env::temp_dir().join(format!("lxb-ra-invalid-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("retroarch.cfg");
        std::fs::write(&config, [0xff]).unwrap();
        assert!(configure_at(&dir, "Alice", "token").is_err());
        assert_eq!(std::fs::read(&config).unwrap(), [0xff]);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn account_files_are_private_and_atomic() {
        let dir = std::env::temp_dir().join(format!("lxb-ra-private-{}", std::process::id()));
        let file = dir.join("account.json");
        private_write(&file, b"first").unwrap();
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        private_write(&file, b"second").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"second");
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn consoles_and_credentials() {
        assert_eq!(console_id("snes"), Some(3));
        assert_eq!(console_id("PlayStation"), Some(12));
        assert!(!safe("../account"));
        assert!(!safe("user\ncheevos_enable"));
    }
}
