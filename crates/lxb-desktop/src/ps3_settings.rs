//! Settings > Games > PlayStation 3: RPCS3's own settings, and the shell's
//! controller mapping for it.
//!
//! Everything RPCS3 has a setting for is written where RPCS3 keeps it — its
//! `config.yml`, the file its own Settings window writes — and read back from
//! there every time the page is built, so the page and RPCS3's window can never
//! disagree. The shell keeps no copy. The same argument the RetroArch page
//! makes for `retroarch.cfg`, and the one the user made for every setting that
//! belongs to something else: the owner's setting, changed where the owner
//! reads it.
//!
//! Two things are the shell's own, and live in its settings file: which PS3
//! button each of the pad's buttons is (the shell writes RPCS3's controller
//! file itself before every game, see [`crate::ps3::input_config`]), and
//! whether somebody has chosen the console's language — until they have, it
//! follows the shell's.
//!
//! ## RPCS3's file
//!
//! YAML, written by yaml-cpp: one `Key: value` per line, two spaces of indent
//! per level, sections as `Key:` on a line of their own. A setting here is a
//! path, `Video/Resolution Scale`. The file is edited line by line, so every
//! other line in it — RPCS3's hundred other settings — stays byte for byte as
//! RPCS3 wrote it. A key the file does not have yet (a file from an older
//! RPCS3, or none at all before RPCS3 has first started) is added at the end of
//! its section, and RPCS3 fills in everything else it needs with its defaults.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// RPCS3's configuration folder, once the helper has said where it is.
static CONFIG: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Say where RPCS3's configuration is — or that there is no RPCS3.
pub fn known_config(config: Option<PathBuf>) {
    *CONFIG.lock().unwrap() = config;
}

/// RPCS3's `config.yml`, where there is an RPCS3.
fn config_file() -> Option<PathBuf> {
    CONFIG
        .lock()
        .unwrap()
        .as_ref()
        .map(|dir| dir.join("config.yml"))
}

/// RPCS3's settings as the file has them now, or `None` where there is no
/// RPCS3 to have settings. A file RPCS3 has not written yet is empty, and
/// every setting then reads as RPCS3's default.
pub fn read() -> Option<Settings> {
    let at = config_file()?;
    Some(Settings {
        text: std::fs::read_to_string(at).unwrap_or_default(),
    })
}

/// Write `pairs` into RPCS3's `config.yml`. Whether it was written.
pub fn write(pairs: &[(&str, &str)]) -> bool {
    let Some(at) = config_file() else {
        tracing::warn!("RPCS3's settings were asked to change with no RPCS3");
        return false;
    };
    write_to(&at, pairs)
}

fn write_to(at: &Path, pairs: &[(&str, &str)]) -> bool {
    let mut text = std::fs::read_to_string(at).unwrap_or_default();
    for (path, value) in pairs {
        text = set(&text, path, value);
    }
    let written = at
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(at, text));
    match written {
        Ok(()) => {
            tracing::info!(?pairs, "RPCS3's settings");
            true
        }
        Err(err) => {
            tracing::warn!(at = %at.display(), %err, "RPCS3's settings could not be written");
            false
        }
    }
}

/// One reading of RPCS3's `config.yml`.
pub struct Settings {
    text: String,
}

impl Settings {
    /// The value at `path`, or RPCS3's own default for it where the file does
    /// not say — see [`DEFAULTS`].
    pub fn get(&self, path: &str) -> String {
        get(&self.text, path).unwrap_or_else(|| {
            DEFAULTS
                .iter()
                .find(|(key, _)| *key == path)
                .map(|(_, value)| (*value).to_string())
                .unwrap_or_default()
        })
    }

    /// Whether every one of `pairs` is what the file says now.
    pub fn is(&self, pairs: &[(&str, &str)]) -> bool {
        pairs.iter().all(|(path, value)| self.get(path) == *value)
    }
}

/// What RPCS3 uses for each setting this page offers where its file does not
/// say — its own defaults (`Emu/system_config.h`).
const DEFAULTS: [(&str, &str); 9] = [
    ("Video/Renderer", "Vulkan"),
    ("Video/Resolution Scale", "100"),
    ("Video/Aspect ratio", "16:9"),
    ("Video/Stretch To Display Area", "false"),
    ("Video/Frame limit", "Auto"),
    ("Video/VSync Mode", "Disabled"),
    ("Video/Performance Overlay/Enabled", "false"),
    ("System/Language", "English (US)"),
    ("Miscellaneous/Show trophy popups", "true"),
];

/// The key a line sets, where it sets one, and how far it is indented.
fn key_of(line: &str) -> Option<(usize, &str, &str)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    let body = &line[indent..];
    if body.is_empty() || body.starts_with('#') || body.starts_with('-') {
        return None;
    }
    let (key, rest) = match body.split_once(": ") {
        Some((key, rest)) => (key, rest),
        None => (body.strip_suffix(':')?, ""),
    };
    Some((indent, unquote(key), rest.trim()))
}

fn unquote(text: &str) -> &str {
    text.strip_prefix('"')
        .and_then(|text| text.strip_suffix('"'))
        .unwrap_or(text)
}

/// Where each part of `path` is: the line each key is on, as far down the
/// path as the file goes, and where the block the next part belongs in ends.
fn walk(lines: &[&str], path: &[&str]) -> (Vec<usize>, usize) {
    let mut found = Vec::new();
    let mut start = 0;
    let mut end = lines.len();
    for (depth, part) in path.iter().enumerate() {
        let indent = depth * 2;
        let mut at = None;
        for (index, line) in lines.iter().enumerate().take(end).skip(start) {
            let Some((here, key, _)) = key_of(line) else {
                continue;
            };
            if here < indent {
                break;
            }
            if here == indent && key == *part {
                at = Some(index);
                break;
            }
        }
        let Some(at) = at else {
            return (found, end);
        };
        found.push(at);
        // The block under this key: every line after it indented further.
        start = at + 1;
        end = lines[start..]
            .iter()
            .position(|line| key_of(line).is_some_and(|(here, _, _)| here <= indent))
            .map_or(lines.len(), |offset| start + offset);
    }
    (found, end)
}

/// The value at `path` (`Video/Resolution Scale`), where the file has one.
pub fn get(text: &str, path: &str) -> Option<String> {
    let parts: Vec<&str> = path.split('/').collect();
    let lines: Vec<&str> = text.lines().collect();
    let (found, _) = walk(&lines, &parts);
    if found.len() != parts.len() {
        return None;
    }
    let (_, _, value) = key_of(lines[*found.last()?])?;
    Some(unquote(value).to_string())
}

/// `text` with the value at `path` set to `value`, and every other line as it
/// was.
pub fn set(text: &str, path: &str, value: &str) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let (found, end) = {
        let borrowed: Vec<&str> = lines.iter().map(String::as_str).collect();
        walk(&borrowed, &parts)
    };
    let written = yaml_value(value);
    if found.len() == parts.len() {
        let at = *found.last().expect("a whole path");
        let indent = (parts.len() - 1) * 2;
        lines[at] = format!(
            "{}{}: {written}",
            " ".repeat(indent),
            parts[parts.len() - 1]
        );
    } else {
        // The rest of the path, at the end of the deepest block that is there
        // — before any blank lines that part it from what follows.
        let mut at = end;
        while at > found.last().map_or(0, |line| line + 1) && lines[at - 1].trim().is_empty() {
            at -= 1;
        }
        let mut added = Vec::new();
        for (depth, part) in parts.iter().enumerate().skip(found.len()) {
            let indent = " ".repeat(depth * 2);
            if depth + 1 == parts.len() {
                added.push(format!("{indent}{part}: {written}"));
            } else {
                added.push(format!("{indent}{part}:"));
            }
        }
        lines.splice(at..at, added);
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// A value as yaml-cpp would write it: plain, unless it is empty.
fn yaml_value(value: &str) -> String {
    if value.is_empty() {
        "\"\"".to_string()
    } else {
        value.to_string()
    }
}

// --- the console's language ----------------------------------------------------

/// Every language a PlayStation 3 can be set to: RPCS3's name for it, and its
/// own name for itself, which is how a console lists languages — somebody
/// looking for their own finds it whatever the console is set to now.
pub const LANGUAGES: [(&str, &str); 20] = [
    ("Japanese", "日本語"),
    ("English (US)", "English (United States)"),
    ("English (UK)", "English (United Kingdom)"),
    ("French", "Français"),
    ("Spanish", "Español"),
    ("German", "Deutsch"),
    ("Italian", "Italiano"),
    ("Dutch", "Nederlands"),
    ("Portuguese (Portugal)", "Português (Portugal)"),
    ("Portuguese (Brazil)", "Português (Brasil)"),
    ("Russian", "Русский"),
    ("Korean", "한국어"),
    ("Chinese (Traditional)", "中文（繁體）"),
    ("Chinese (Simplified)", "中文（简体）"),
    ("Finnish", "Suomi"),
    ("Swedish", "Svenska"),
    ("Danish", "Dansk"),
    ("Norwegian", "Norsk"),
    ("Polish", "Polski"),
    ("Turkish", "Türkçe"),
];

/// The console language nearest the shell's: its own where a PS3 had it, and
/// British English for Hindi, which a PS3 did not have.
pub fn language_for(shell: &str) -> &'static str {
    match shell {
        "en-US" => "English (US)",
        "de" => "German",
        "es" => "Spanish",
        "fr" => "French",
        "pl" => "Polish",
        "pt-BR" => "Portuguese (Brazil)",
        "ru" => "Russian",
        "zh-CN" => "Chinese (Simplified)",
        _ => "English (UK)",
    }
}

/// Before a game: the console speaks the shell's language, where nobody has
/// chosen one for it on the page.
pub fn follow_the_shells_language(config: &Path) {
    if crate::settings::ps3_language_chosen() {
        return;
    }
    let wanted = language_for(crate::i18n::spoken().key());
    let at = config.join("config.yml");
    let now = std::fs::read_to_string(&at).unwrap_or_default();
    if get(&now, "System/Language").as_deref() != Some(wanted) {
        write_to(&at, &[("System/Language", wanted)]);
    }
}

// --- the controller's buttons --------------------------------------------------

/// The PS3's buttons that can be moved, in the order a PS3's own settings
/// listed them: RPCS3's key for each, and the message that names it. The
/// D-pad and the sticks are the pad's own and stay where they are.
pub const PS3_BUTTONS: [(&str, &str); 12] = [
    ("Cross", "ps3-button-cross"),
    ("Circle", "ps3-button-circle"),
    ("Square", "ps3-button-square"),
    ("Triangle", "ps3-button-triangle"),
    ("L1", "ps3-button-l1"),
    ("R1", "ps3-button-r1"),
    ("L2", "ps3-button-l2"),
    ("R2", "ps3-button-r2"),
    ("L3", "ps3-button-l3"),
    ("R3", "ps3-button-r3"),
    ("Start", "ps3-button-start"),
    ("Select", "ps3-button-select"),
];

/// The pad's buttons a PS3 button can be on: SDL's name for each, as RPCS3
/// writes it, the message that names it by where it is, and its mark.
pub const PAD_BUTTONS: [(&str, &str, &str); 12] = [
    ("South", "pad-button-south", crate::icons::PAD_SOUTH),
    ("East", "pad-button-east", crate::icons::PAD_EAST),
    ("West", "pad-button-west", crate::icons::PAD_WEST),
    ("North", "pad-button-north", crate::icons::PAD_NORTH),
    (
        "LB",
        "pad-button-left-bumper",
        crate::icons::PAD_LEFT_BUMPER,
    ),
    (
        "RB",
        "pad-button-right-bumper",
        crate::icons::PAD_RIGHT_BUMPER,
    ),
    (
        "LT",
        "pad-button-left-trigger",
        crate::icons::PAD_LEFT_TRIGGER,
    ),
    (
        "RT",
        "pad-button-right-trigger",
        crate::icons::PAD_RIGHT_TRIGGER,
    ),
    ("LS", "pad-button-left-stick", crate::icons::PAD_STICK_LEFT),
    ("RS", "pad-button-right-stick", crate::icons::PAD_STICK),
    ("Start", "pad-button-start", crate::icons::PAD_START),
    ("Back", "pad-button-back", crate::icons::PAD_SELECT),
];

/// Where each PS3 button is on a pad nobody has changed anything on — RPCS3's
/// own SDL defaults, in [`PS3_BUTTONS`]' order.
pub const DEFAULT_BUTTONS: [&str; 12] = [
    "South", "East", "West", "North", "LB", "RB", "LT", "RT", "LS", "RS", "Start", "Back",
];

/// Which pad button every PS3 button is on now: the defaults, with whatever
/// has been moved on the page over them.
pub fn buttons() -> Vec<(&'static str, String)> {
    let moved = crate::settings::ps3_buttons();
    PS3_BUTTONS
        .iter()
        .zip(DEFAULT_BUTTONS)
        .map(|((button, _), default)| {
            let on = moved
                .get(*button)
                .filter(|to| PAD_BUTTONS.iter().any(|(name, _, _)| name == to))
                .cloned()
                .unwrap_or_else(|| default.to_string());
            (*button, on)
        })
        .collect()
}

/// The message naming a pad button, by SDL's name for it.
pub fn pad_button_name(sdl: &str) -> String {
    PAD_BUTTONS
        .iter()
        .find(|(name, _, _)| *name == sdl)
        .map(|(_, message, _)| crate::i18n::text(message).to_string())
        .unwrap_or_else(|| sdl.to_string())
}

/// The buttons that have been moved, for the settings file: only those that
/// are not where RPCS3 would have put them.
pub fn moved(buttons: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    buttons
        .iter()
        .filter(|(button, to)| {
            PS3_BUTTONS
                .iter()
                .zip(DEFAULT_BUTTONS)
                .any(|((name, _), default)| name == button && default != to.as_str())
        })
        .map(|(button, to)| (button.clone(), to.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape of RPCS3's own file, cut down.
    const FILE: &str = "Core:\n  PPU Decoder: Recompiler (LLVM)\nVideo:\n  Renderer: Vulkan\n  Resolution: 1280x720\n  Aspect ratio: 16:9\n  Performance Overlay:\n    Enabled: false\n    Font size: 10\n  Vulkan:\n    Adapter: \"\"\nAudio:\n  Renderer: Cubeb\nSystem:\n  Language: English (US)\n";

    #[test]
    fn a_setting_is_read_by_its_path() {
        assert_eq!(get(FILE, "Video/Renderer").as_deref(), Some("Vulkan"));
        assert_eq!(get(FILE, "Audio/Renderer").as_deref(), Some("Cubeb"));
        assert_eq!(get(FILE, "Video/Aspect ratio").as_deref(), Some("16:9"));
        assert_eq!(
            get(FILE, "Video/Performance Overlay/Enabled").as_deref(),
            Some("false")
        );
        assert_eq!(get(FILE, "Video/Vulkan/Adapter").as_deref(), Some(""));
        assert_eq!(
            get(FILE, "System/Language").as_deref(),
            Some("English (US)")
        );
        assert_eq!(get(FILE, "Video/Resolution Scale"), None);
        // A key of the same name at another depth is not it.
        assert_eq!(get(FILE, "Enabled"), None);
    }

    #[test]
    fn a_setting_is_changed_and_nothing_else_is() {
        let changed = set(FILE, "Video/Renderer", "OpenGL");
        assert_eq!(
            changed,
            FILE.replace("  Renderer: Vulkan", "  Renderer: OpenGL")
        );
        // Audio's renderer is another setting.
        assert_eq!(get(&changed, "Audio/Renderer").as_deref(), Some("Cubeb"));
        let nested = set(FILE, "Video/Performance Overlay/Enabled", "true");
        assert_eq!(
            nested,
            FILE.replace("    Enabled: false", "    Enabled: true")
        );
        let language = set(FILE, "System/Language", "Polish");
        assert_eq!(get(&language, "System/Language").as_deref(), Some("Polish"));
    }

    #[test]
    fn a_setting_the_file_lacks_is_added_to_its_section() {
        let added = set(FILE, "Video/Resolution Scale", "200");
        assert_eq!(
            get(&added, "Video/Resolution Scale").as_deref(),
            Some("200")
        );
        assert!(added.contains("    Adapter: \"\"\n  Resolution Scale: 200\nAudio:\n"));
        let section = set(FILE, "Miscellaneous/Show trophy popups", "false");
        assert!(section
            .ends_with("  Language: English (US)\nMiscellaneous:\n  Show trophy popups: false\n"));
        let empty = set("", "Video/Performance Overlay/Enabled", "true");
        assert_eq!(empty, "Video:\n  Performance Overlay:\n    Enabled: true\n");
        assert_eq!(
            get(&empty, "Video/Performance Overlay/Enabled").as_deref(),
            Some("true")
        );
    }

    #[test]
    fn what_the_file_does_not_say_is_rpcs3s_default() {
        let settings = Settings {
            text: FILE.to_string(),
        };
        assert_eq!(settings.get("Video/Resolution Scale"), "100");
        assert!(settings.is(&[
            ("Video/Aspect ratio", "16:9"),
            ("Video/Stretch To Display Area", "false")
        ]));
        assert!(!settings.is(&[("Video/Renderer", "OpenGL")]));
    }

    #[test]
    fn the_console_speaks_the_shells_language_where_it_can() {
        assert_eq!(language_for("pl"), "Polish");
        assert_eq!(language_for("en-US"), "English (US)");
        assert_eq!(language_for("en-GB"), "English (UK)");
        assert_eq!(language_for("hi"), "English (UK)");
        for tag in ["de", "es", "fr", "pt-BR", "ru", "zh-CN"] {
            assert!(
                LANGUAGES.iter().any(|(name, _)| *name == language_for(tag)),
                "{tag}"
            );
        }
    }

    #[test]
    fn only_a_moved_button_is_remembered() {
        let buttons = BTreeMap::from([
            ("Cross".to_string(), "East".to_string()),
            ("Circle".to_string(), "East".to_string()),
        ]);
        assert_eq!(
            moved(&buttons),
            BTreeMap::from([("Cross".to_string(), "East".to_string())])
        );
    }
}
