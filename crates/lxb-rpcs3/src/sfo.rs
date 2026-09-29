//! `PARAM.SFO`: what a PlayStation 3 game says about itself.
//!
//! Every disc, every package and every installed game has one, and it is the
//! only place a game's name, its serial, its version and its kind are written
//! down. The format is Sony's PSF: a little-endian header, a table of entries,
//! a block of NUL-terminated key names and a block of values. Everything is
//! read out of it once and nothing is written back.
//!
//! See <https://www.psdevwiki.com/ps3/PARAM.SFO> and RPCS3's `Loader/PSF.cpp`.

use std::collections::BTreeMap;

/// One game's `PARAM.SFO`, as the strings and numbers it holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sfo {
    strings: BTreeMap<String, String>,
    numbers: BTreeMap<String, u32>,
}

/// The value formats PSF has, from its entries' `data_fmt`.
const UTF8_SPECIAL: u16 = 0x0004;
const UTF8: u16 = 0x0204;
const INTEGER: u16 = 0x0404;

impl Sfo {
    /// Read one, or `None` for bytes that are not a PSF at all.
    ///
    /// A malformed entry is skipped rather than failing the whole file: a game
    /// with one odd value still has a name.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 20 || &bytes[..4] != b"\0PSF" {
            return None;
        }
        let keys = le32(bytes, 8)? as usize;
        let values = le32(bytes, 12)? as usize;
        let count = le32(bytes, 16)? as usize;
        let mut sfo = Sfo::default();
        for index in 0..count.min(4096) {
            let at = 20 + index * 16;
            let Some(entry) = bytes.get(at..at + 16) else {
                break;
            };
            let key_offset = u16::from_le_bytes([entry[0], entry[1]]) as usize;
            let format = u16::from_le_bytes([entry[2], entry[3]]);
            let length = u32::from_le_bytes(entry[4..8].try_into().ok()?) as usize;
            let value_offset = u32::from_le_bytes(entry[12..16].try_into().ok()?) as usize;
            let Some(key) = c_string(bytes, keys + key_offset) else {
                continue;
            };
            let start = values + value_offset;
            let Some(value) = bytes.get(start..start + length) else {
                continue;
            };
            match format {
                UTF8 | UTF8_SPECIAL => {
                    let text = value.split(|b| *b == 0).next().unwrap_or_default();
                    sfo.strings
                        .insert(key, String::from_utf8_lossy(text).into_owned());
                }
                INTEGER if value.len() >= 4 => {
                    sfo.numbers
                        .insert(key, u32::from_le_bytes(value[..4].try_into().ok()?));
                }
                _ => {}
            }
        }
        Some(sfo)
    }

    /// A string value, trimmed, or `None` where it is absent or empty.
    pub fn string(&self, key: &str) -> Option<&str> {
        self.strings
            .get(key)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
    }

    #[cfg(test)]
    pub fn number(&self, key: &str) -> Option<u32> {
        self.numbers.get(key).copied()
    }

    /// The serial, `BLUS30359`: what RPCS3 calls a game by, where an installed
    /// one lives under `dev_hdd0/game`, and what its caches are named after.
    pub fn title_id(&self) -> Option<&str> {
        self.string("TITLE_ID")
    }

    /// The name the game gives itself where the console's language has none of
    /// its own.
    pub fn title(&self) -> Option<String> {
        self.string("TITLE").map(one_line)
    }

    /// What kind of thing this is: `DG` a disc game, `HG` a game installed to
    /// the hard disk, `GD` game data — an update or add-on, which is not a game
    /// anybody starts — and a dozen rarer ones.
    pub fn category(&self) -> Option<&str> {
        self.string("CATEGORY")
    }

    /// The game's own version, `01.02`, which an update raises.
    pub fn app_version(&self) -> Option<&str> {
        self.string("APP_VER")
    }

    /// The name in every language the game translated it into, keyed the way
    /// the shell names its own catalogs — `de`, `en-GB`, `pt-BR`, `zh-CN`.
    ///
    /// A PS3 shows a game under the console's language when the game has a
    /// name in it, and `TITLE_nn` is where those are: `nn` is the console's own
    /// language number (see [`LANGUAGES`]). Empty for the many games that only
    /// ever have one name.
    pub fn titles(&self) -> BTreeMap<String, String> {
        LANGUAGES
            .iter()
            .filter_map(|(number, language)| {
                let name = self.string(&format!("TITLE_{number:02}"))?;
                Some((language.to_string(), one_line(name)))
            })
            .collect()
    }
}

/// The PS3's language numbers — the `nn` of `TITLE_nn` — and the language
/// each one is, in the tags the shell's catalogs are named with.
///
/// From the console's own system-language setting, as psdevwiki lists it.
/// Languages the PS3 had and the shell has not (Italian, Dutch, …) are here
/// too: the shell picks from what a game offers, and a language it does not
/// ship today may be one it ships next year.
pub const LANGUAGES: &[(u8, &str)] = &[
    (0, "ja"),
    (1, "en-US"),
    (2, "fr"),
    (3, "es"),
    (4, "de"),
    (5, "it"),
    (6, "nl"),
    (7, "pt-PT"),
    (8, "ru"),
    (9, "ko"),
    (10, "zh-TW"),
    (11, "zh-CN"),
    (12, "fi"),
    (13, "sv"),
    (14, "da"),
    (15, "no"),
    (16, "pl"),
    (17, "pt-BR"),
    (18, "en-GB"),
    (19, "tr"),
];

/// A game's name on one line. Some write a line break into their title so the
/// PS3 would wrap it in a particular place; a row in the shell is one line and
/// wraps nothing.
fn one_line(name: &str) -> String {
    name.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn le32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn c_string(bytes: &[u8], at: usize) -> Option<String> {
    let rest = bytes.get(at..)?;
    let end = rest.iter().position(|b| *b == 0)?;
    std::str::from_utf8(&rest[..end]).ok().map(str::to_string)
}

/// A PSF built from a list of values, for the tests of this module and of every
/// module that reads a game.
#[cfg(test)]
pub fn build(entries: &[(&str, &str)], numbers: &[(&str, u32)]) -> Vec<u8> {
    let mut keys = Vec::new();
    let mut values = Vec::new();
    let mut table = Vec::new();
    let mut all: Vec<(&str, Vec<u8>, u16)> = entries
        .iter()
        .map(|(key, value)| {
            let mut bytes = value.as_bytes().to_vec();
            bytes.push(0);
            (*key, bytes, UTF8)
        })
        .collect();
    all.extend(
        numbers
            .iter()
            .map(|(key, value)| (*key, value.to_le_bytes().to_vec(), INTEGER)),
    );
    // PSF keeps its keys sorted, and so does this.
    all.sort_by(|a, b| a.0.cmp(b.0));
    for (key, value, format) in &all {
        table.extend_from_slice(&(keys.len() as u16).to_le_bytes());
        table.extend_from_slice(&format.to_le_bytes());
        table.extend_from_slice(&(value.len() as u32).to_le_bytes());
        let max = value.len().next_multiple_of(4) as u32;
        table.extend_from_slice(&max.to_le_bytes());
        table.extend_from_slice(&(values.len() as u32).to_le_bytes());
        keys.extend_from_slice(key.as_bytes());
        keys.push(0);
        values.extend_from_slice(value);
        values.resize(values.len().next_multiple_of(4), 0);
    }
    keys.resize(keys.len().next_multiple_of(4), 0);
    let key_start = 20 + table.len();
    let value_start = key_start + keys.len();
    let mut out = Vec::new();
    out.extend_from_slice(b"\0PSF");
    out.extend_from_slice(&0x0101u32.to_le_bytes());
    out.extend_from_slice(&(key_start as u32).to_le_bytes());
    out.extend_from_slice(&(value_start as u32).to_le_bytes());
    out.extend_from_slice(&(all.len() as u32).to_le_bytes());
    out.extend_from_slice(&table);
    out.extend_from_slice(&keys);
    out.extend_from_slice(&values);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_name_the_serial_and_the_kind() {
        let bytes = build(
            &[
                ("TITLE", "TEKKEN 6"),
                ("TITLE_ID", "BLUS30359"),
                ("CATEGORY", "DG"),
                ("APP_VER", "01.00"),
            ],
            &[("PARENTAL_LEVEL", 5)],
        );
        let sfo = Sfo::parse(&bytes).expect("a PSF");
        assert_eq!(sfo.title().as_deref(), Some("TEKKEN 6"));
        assert_eq!(sfo.title_id(), Some("BLUS30359"));
        assert_eq!(sfo.category(), Some("DG"));
        assert_eq!(sfo.app_version(), Some("01.00"));
        assert_eq!(sfo.number("PARENTAL_LEVEL"), Some(5));
    }

    #[test]
    fn titles_are_keyed_by_the_shells_language_names() {
        let bytes = build(
            &[
                ("TITLE", "skate 3"),
                ("TITLE_02", "skate 3 FR"),
                ("TITLE_16", "skate 3 PL"),
                ("TITLE_18", "skate\n3"),
            ],
            &[],
        );
        let titles = Sfo::parse(&bytes).expect("a PSF").titles();
        assert_eq!(titles.get("fr").map(String::as_str), Some("skate 3 FR"));
        assert_eq!(titles.get("pl").map(String::as_str), Some("skate 3 PL"));
        // A title the game broke across two lines is one line here.
        assert_eq!(titles.get("en-GB").map(String::as_str), Some("skate 3"));
        assert!(!titles.contains_key("de"));
    }

    #[test]
    fn what_is_not_a_psf_is_refused() {
        assert!(Sfo::parse(b"\x89PNG\r\n\x1a\n and then some").is_none());
        assert!(Sfo::parse(b"\0PSF").is_none());
    }

    #[test]
    fn an_empty_value_is_no_value() {
        let bytes = build(&[("TITLE", "  "), ("TITLE_ID", "NPEA00019")], &[]);
        let sfo = Sfo::parse(&bytes).expect("a PSF");
        assert_eq!(sfo.title(), None);
        assert_eq!(sfo.title_id(), Some("NPEA00019"));
    }
}
