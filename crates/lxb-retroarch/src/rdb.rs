//! RetroArch's own database of games, read for one question: what is the game
//! with this catalogue number called?
//!
//! RetroArch ships one file per system — `Sony - PlayStation 2.rdb` — which is
//! what its own scanner names a disc from, and a name out of it is the name
//! libretro's thumbnail server files that game's pictures under. So a disc
//! named here is a disc whose cover can be found, which a disc named by its
//! volume label (`TEKKEN4`) is not.
//!
//! ## The format
//!
//! ```text
//!   "RARCHDB\0"   eight bytes
//!   u64           where the trailing metadata is, big-endian
//!   map, map, …   one MessagePack map per game
//!   nil           the end of the games
//!   {count: n}    the metadata
//! ```
//!
//! A game's map holds `name`, `serial`, the checksums and a dozen other things,
//! and may hold `serial` twice — libretro merges its sources and keeps both.
//! Only strings, byte strings, integers and nested maps and arrays ever appear,
//! and this reads exactly the MessagePack those are written in; anything else
//! stops the read rather than being guessed past.

use std::path::Path;

const MAGIC: &[u8] = b"RARCHDB\0";

/// Every name filed under any of `serials`, in the order the database has
/// them.
///
/// The comparison ignores case and everything but letters and digits: the
/// discs and the database agree on a catalogue number's characters far more
/// often than on its dashes and spaces.
pub fn names_for(database: &Path, serials: &[String]) -> Vec<String> {
    let Ok(bytes) = std::fs::read(database) else {
        return Vec::new();
    };
    let wanted: Vec<String> = serials.iter().map(|serial| squeezed(serial)).collect();
    if wanted.iter().all(String::is_empty) {
        return Vec::new();
    }
    let mut names = Vec::new();
    for game in games(&bytes) {
        let Some(name) = game.name else {
            continue;
        };
        if game
            .serials
            .iter()
            .any(|serial| wanted.contains(&squeezed(serial)))
            && !names.contains(&name)
        {
            names.push(name);
        }
    }
    names
}

/// A catalogue number reduced to what two spellings of it share.
fn squeezed(serial: &str) -> String {
    serial
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// One game's two fields this reads.
#[derive(Debug, Default)]
struct Game {
    name: Option<String>,
    serials: Vec<String>,
}

/// Every game in a database, as far as it reads.
fn games(bytes: &[u8]) -> Vec<Game> {
    let mut found = Vec::new();
    if !bytes.starts_with(MAGIC) {
        return found;
    }
    let mut reader = Reader { bytes, at: 16 };
    loop {
        match reader.peek() {
            // The end of the games, or the end of the file.
            None | Some(0xc0) => break,
            Some(_) => {}
        }
        match reader.game() {
            Some(game) => found.push(game),
            None => break,
        }
    }
    found
}

/// A MessagePack value, only as far as this needs one.
enum Value<'a> {
    Text(&'a [u8]),
    Other,
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let slice = self.bytes.get(self.at..self.at.checked_add(count)?)?;
        self.at += count;
        Some(slice)
    }

    fn byte(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn number(&mut self, width: usize) -> Option<usize> {
        let raw = self.take(width)?;
        Some(
            raw.iter()
                .fold(0usize, |sum, &byte| (sum << 8) | usize::from(byte)),
        )
    }

    /// One game's map: its name and every serial, the rest skipped.
    fn game(&mut self) -> Option<Game> {
        let pairs = self.map_length()?;
        let mut game = Game::default();
        for _ in 0..pairs {
            let key = match self.value()? {
                Value::Text(key) => key,
                Value::Other => {
                    self.value()?;
                    continue;
                }
            };
            let value = self.value()?;
            if let Value::Text(text) = value {
                match key {
                    b"name" => game.name = Some(String::from_utf8_lossy(text).into_owned()),
                    b"serial" => game
                        .serials
                        .push(String::from_utf8_lossy(text).into_owned()),
                    _ => {}
                }
            }
        }
        Some(game)
    }

    fn map_length(&mut self) -> Option<usize> {
        match self.byte()? {
            tag @ 0x80..=0x8f => Some(usize::from(tag & 0x0f)),
            0xde => self.number(2),
            0xdf => self.number(4),
            _ => None,
        }
    }

    /// One value, read past; its bytes where it is text.
    fn value(&mut self) -> Option<Value<'a>> {
        let tag = self.byte()?;
        let text = |reader: &mut Reader<'a>, length: usize| reader.take(length).map(Value::Text);
        match tag {
            0x00..=0x7f | 0xe0..=0xff | 0xc0 | 0xc2 | 0xc3 => Some(Value::Other),
            0xa0..=0xbf => text(self, usize::from(tag & 0x1f)),
            // str8 and bin8, str16 and bin16, str32 and bin32: a serial is
            // written as either, depending on which source it came from.
            0xd9 | 0xc4 => {
                let length = self.number(1)?;
                text(self, length)
            }
            0xda | 0xc5 => {
                let length = self.number(2)?;
                text(self, length)
            }
            0xdb | 0xc6 => {
                let length = self.number(4)?;
                text(self, length)
            }
            0xcc | 0xd0 => self.take(1).map(|_| Value::Other),
            0xcd | 0xd1 => self.take(2).map(|_| Value::Other),
            0xce | 0xd2 | 0xca => self.take(4).map(|_| Value::Other),
            0xcf | 0xd3 | 0xcb => self.take(8).map(|_| Value::Other),
            0x90..=0x9f => self.skip_items(usize::from(tag & 0x0f)),
            0xdc => {
                let items = self.number(2)?;
                self.skip_items(items)
            }
            0xdd => {
                let items = self.number(4)?;
                self.skip_items(items)
            }
            0x80..=0x8f => self.skip_items(usize::from(tag & 0x0f) * 2),
            0xde => {
                let pairs = self.number(2)?;
                self.skip_items(pairs * 2)
            }
            0xdf => {
                let pairs = self.number(4)?;
                self.skip_items(pairs * 2)
            }
            _ => None,
        }
    }

    fn skip_items(&mut self, items: usize) -> Option<Value<'a>> {
        for _ in 0..items {
            self.value()?;
        }
        Some(Value::Other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(tag: u8, bytes: &[u8]) -> Vec<u8> {
        let mut out = vec![tag, bytes.len() as u8];
        out.extend(bytes);
        out
    }

    fn key(name: &str) -> Vec<u8> {
        let mut out = vec![0xa0 | name.len() as u8];
        out.extend(name.as_bytes());
        out
    }

    /// Two games laid out as RetroArch lays them out — including a `serial`
    /// written twice and one written as a byte string — and the trailer.
    fn database() -> Vec<u8> {
        let mut bytes = MAGIC.to_vec();
        bytes.extend(0u64.to_be_bytes());
        // Tekken 4, with a checksum and a size to be read past.
        bytes.push(0x85);
        bytes.extend(key("name"));
        bytes.extend(text(
            0xd9,
            b"Tekken 4 (Europe, Australia) (En,Fr,De,Es,It) (v2.00)",
        ));
        bytes.extend(key("size"));
        bytes.extend([0xce, 0, 0, 0, 1]);
        bytes.extend(key("crc"));
        bytes.extend(text(0xc4, &[0xf4, 0x8f, 0x99, 0x4a]));
        bytes.extend(key("serial"));
        bytes.extend(text(0xc4, b"SCES-50878"));
        bytes.extend(key("serial"));
        bytes.extend(text(0xc4, b"SCES-50878"));
        // Something else.
        bytes.push(0x82);
        bytes.extend(key("name"));
        bytes.extend(text(0xd9, b"eJay Clubworld (USA)"));
        bytes.extend(key("serial"));
        bytes.extend(text(0xd9, b"SLUS-20789"));
        bytes.push(0xc0);
        bytes.push(0x81);
        bytes.extend(key("count"));
        bytes.push(2);
        bytes
    }

    #[test]
    fn a_game_is_named_by_its_catalogue_number() {
        let at = std::env::temp_dir().join(format!("lxb-rdb-test-{}", std::process::id()));
        std::fs::write(&at, database()).expect("written");
        let names = names_for(&at, &["SCES-50878".to_string()]);
        let _ = std::fs::remove_file(&at);
        assert_eq!(
            names,
            ["Tekken 4 (Europe, Australia) (En,Fr,De,Es,It) (v2.00)"]
        );
    }

    #[test]
    fn a_number_is_matched_whatever_its_dashes() {
        assert_eq!(squeezed("MK-81020"), squeezed("mk81020"));
        let games = games(&database());
        assert_eq!(games.len(), 2);
        assert_eq!(games[1].serials, ["SLUS-20789"]);
    }

    #[test]
    fn a_file_that_is_not_a_database_has_nothing_in_it() {
        assert!(games(b"not a database at all").is_empty());
    }
}
