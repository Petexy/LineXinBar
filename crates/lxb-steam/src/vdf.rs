//! Valve's key-values, as much of it as reading an installed library takes.
//!
//! Two files on the disk say what is installed: `libraryfolders.vdf`, which
//! lists where the libraries are, and one `appmanifest_<id>.acf` per installed
//! game inside each of them. Both are text key-values — a quoted key followed
//! by a quoted value or by a brace-delimited block of more of the same:
//!
//! ```text
//! "AppState"
//! {
//!     "appid"      "440"
//!     "name"       "A Game"
//!     "InstalledDepots"
//!     {
//!         "441"    { "manifest"  "12345" }
//!     }
//! }
//! ```
//!
//! Parsed here rather than taken as a dependency for the reason the shell
//! parses `.desktop` files itself: this is the whole grammar, it has not moved
//! in twenty years, and what is wanted from it is four keys.
//!

use std::collections::BTreeMap;

/// One node: either a value or a block of named nodes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    Value(String),
    Block(BTreeMap<String, Node>),
}

impl Node {
    /// The value at `path`, walking down through blocks.
    ///
    /// Keys are matched without regard to case, because Valve's own files are
    /// inconsistent about it — `appid` and `AppID` both occur — and a reader
    /// that matched exactly would silently find nothing on half of them.
    pub fn get(&self, path: &[&str]) -> Option<&Node> {
        let mut node = self;
        for step in path {
            let Node::Block(block) = node else {
                return None;
            };
            node = block
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(step))
                .map(|(_, value)| value)?;
        }
        Some(node)
    }

    /// The string at `path`, if there is one there.
    pub fn string(&self, path: &[&str]) -> Option<&str> {
        match self.get(path)? {
            Node::Value(value) => Some(value),
            Node::Block(_) => None,
        }
    }

    /// The number at `path`. Every number in these files is written as a
    /// string, so this is the string parsed.
    pub fn number(&self, path: &[&str]) -> Option<u64> {
        self.string(path)?.trim().parse().ok()
    }

    /// The block at `path`, making every step of it that is not there yet.
    ///
    /// Case is matched the way [`Self::get`] matches it, so a file the Steam
    /// client wrote as `Software` does not gain a second `software` beside it —
    /// which it would read as a different key and this crate would then read as
    /// whichever came first.
    ///
    /// `None` only when a step of the path is already a value rather than a
    /// block, which is a file that does not mean what the caller thinks and
    /// must not be rewritten on a guess.
    pub fn make(&mut self, path: &[&str]) -> Option<&mut BTreeMap<String, Node>> {
        let mut node = self;
        for step in path {
            let Node::Block(block) = node else {
                return None;
            };
            let key = block
                .keys()
                .find(|key| key.eq_ignore_ascii_case(step))
                .cloned()
                .unwrap_or_else(|| (*step).to_string());
            node = block
                .entry(key)
                .or_insert_with(|| Node::Block(BTreeMap::new()));
        }
        match node {
            Node::Block(block) => Some(block),
            Node::Value(_) => None,
        }
    }

    /// Put one value at `path`, making the blocks above it. Returns whether it
    /// could be written; see [`Self::make`] for the one case it cannot.
    pub fn set(&mut self, path: &[&str], value: impl Into<String>) -> bool {
        let Some((last, above)) = path.split_last() else {
            return false;
        };
        let Some(block) = self.make(above) else {
            return false;
        };
        let key = block
            .keys()
            .find(|key| key.eq_ignore_ascii_case(last))
            .cloned()
            .unwrap_or_else(|| (*last).to_string());
        block.insert(key, Node::Value(value.into()));
        true
    }

    /// The entries of the block at `path`, in key order.
    pub fn block(&self, path: &[&str]) -> impl Iterator<Item = (&String, &Node)> {
        match self.get(path) {
            Some(Node::Block(block)) => block.iter(),
            _ => EMPTY.iter(),
        }
    }
}

/// What [`Node::block`] iterates when there is no block there. A `static` so
/// the iterator types on both arms match without boxing.
static EMPTY: BTreeMap<String, Node> = BTreeMap::new();

/// Parse a whole file. The top level is a block, whatever its one key is
/// called — `libraryfolders` in one of these files and `AppState` in the other
/// — so what comes back is that outer block with its name still on it.
///
/// Never fails, and deliberately: a file that stops making sense is read as
/// far as it went, and what was read still stands. Steam writes both of these
/// files in place while it is downloading, so a manifest caught mid-write is
/// the ordinary case rather than a corrupt one — and the right answer to half
/// a manifest is that this game is not installed *yet*, not that the whole
/// library could not be read. A caller asks for the keys it wants and gets
/// nothing for the ones that are not there, which is the same answer a file
/// with nothing in it gives.
pub fn parse(text: &str) -> Node {
    let mut input = Input {
        bytes: text.as_bytes(),
        at: 0,
    };
    let mut top = BTreeMap::new();
    while let Some(key) = input.token() {
        let Some(value) = input.value() else { break };
        top.insert(key, value);
    }
    Node::Block(top)
}

/// Write a tree back out in the form Valve's own files are written in.
///
/// Tabs for the indent and two of them between a key and its value, because
/// these files are not only read here: the Steam client reads and rewrites the
/// same ones, and a file that comes back looking like the one it wrote is a
/// file whose diff, when somebody looks, is the one line that changed.
///
/// Only the two characters Valve's own writer escapes are escaped. Nothing in
/// what this crate writes — an account name, a token, a number — can contain
/// either, but a writer that silently dropped them would be one that produced
/// an unreadable file rather than a wrong one.
pub fn text(node: &Node) -> String {
    let mut out = String::new();
    match node {
        // The outermost block is written as its own entries rather than as a
        // block with no name, which is the shape `parse` hands back.
        Node::Block(block) => {
            for (key, value) in block {
                entry(&mut out, key, value, 0);
            }
        }
        Node::Value(value) => quoted(&mut out, value),
    }
    out
}

fn entry(out: &mut String, key: &str, node: &Node, depth: usize) {
    let indent = "\t".repeat(depth);
    out.push_str(&indent);
    quoted(out, key);
    match node {
        Node::Value(value) => {
            out.push_str("\t\t");
            quoted(out, value);
            out.push('\n');
        }
        Node::Block(block) => {
            out.push('\n');
            out.push_str(&indent);
            out.push_str("{\n");
            for (key, value) in block {
                entry(out, key, value, depth + 1);
            }
            out.push_str(&indent);
            out.push_str("}\n");
        }
    }
}

fn quoted(out: &mut String, value: &str) {
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            other => out.push(other),
        }
    }
    out.push('"');
}

struct Input<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Input<'_> {
    /// The next key or value: a quoted string, or a bare run of non-space for
    /// the unquoted keys these files occasionally carry.
    ///
    /// `None` at the end of the file and at a closing brace, which the caller
    /// consumes: those are the two places a block stops.
    fn token(&mut self) -> Option<String> {
        self.space();
        match *self.bytes.get(self.at)? {
            b'"' => {
                self.at += 1;
                // Collected as bytes and decoded once at the end, rather than
                // a character at a time. A game called `Street Fighter™ 6`
                // has three bytes in that trademark sign, and a reader that
                // pushed each of them as its own `char` would produce
                // `Street Fighterâ¢ 6` — the classic mistake, and one that is
                // invisible until somebody owns a game with an accent in it.
                let mut out: Vec<u8> = Vec::new();
                loop {
                    let byte = *self.bytes.get(self.at)?;
                    self.at += 1;
                    match byte {
                        b'"' => return Some(String::from_utf8_lossy(&out).into_owned()),
                        b'\\' => {
                            let escaped = *self.bytes.get(self.at)?;
                            self.at += 1;
                            match escaped {
                                b'n' => out.push(b'\n'),
                                b't' => out.push(b'\t'),
                                b'"' => out.push(b'"'),
                                b'\\' => out.push(b'\\'),
                                // A backslash before anything else is a
                                // backslash: Windows paths in these files are
                                // written with single ones.
                                other => out.extend_from_slice(&[b'\\', other]),
                            }
                        }
                        other => out.push(other),
                    }
                }
            }
            b'{' | b'}' => None,
            _ => {
                let start = self.at;
                while self
                    .bytes
                    .get(self.at)
                    .is_some_and(|byte| !byte.is_ascii_whitespace() && !b"{}\"".contains(byte))
                {
                    self.at += 1;
                }
                (self.at > start)
                    .then(|| String::from_utf8_lossy(&self.bytes[start..self.at]).into_owned())
                    .filter(|token| !token.is_empty())
            }
        }
    }

    /// What a key is followed by: another token, or a block.
    fn value(&mut self) -> Option<Node> {
        self.space();
        if self.bytes.get(self.at) == Some(&b'{') {
            self.at += 1;
            let mut block = BTreeMap::new();
            loop {
                self.space();
                match self.bytes.get(self.at) {
                    Some(b'}') => {
                        self.at += 1;
                        return Some(Node::Block(block));
                    }
                    // The end of the file inside a block: what was read is
                    // kept. See `parse` — a manifest being written is the
                    // ordinary reason for this.
                    None => return Some(Node::Block(block)),
                    _ => {}
                }
                // Anything that is not a key here — a stray brace, a file that
                // stops — ends the block with what it has, for the reason
                // `parse` never fails.
                let Some(key) = self.token() else {
                    return Some(Node::Block(block));
                };
                let Some(value) = self.value() else {
                    return Some(Node::Block(block));
                };
                block.insert(key, value);
            }
        }
        self.token().map(Node::Value)
    }

    /// Whitespace and `//` comments, which these files carry.
    fn space(&mut self) {
        loop {
            while self.bytes.get(self.at).is_some_and(u8::is_ascii_whitespace) {
                self.at += 1;
            }
            if self.bytes.get(self.at..self.at + 2) == Some(b"//") {
                while self.bytes.get(self.at).is_some_and(|byte| *byte != b'\n') {
                    self.at += 1;
                }
                continue;
            }
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A manifest of the shape Steam writes, read for the four things this
    /// crate wants out of one.
    ///
    /// Deliberately not a copy of a file off this machine: nothing here may
    /// depend on what happens to be installed where this was written, and a
    /// fixture taken from one developer's disk is how that dependency arrives
    /// unnoticed. These are made-up ids and a made-up name.
    const MANIFEST: &str = r#"
"AppState"
{
	"appid"		"999000"
	"universe"		"1"
	"name"		"An Invented Game"
	"StateFlags"		"4"
	"installdir"		"Invented Game"
	"LastUpdated"		"1700000000"
	"SizeOnDisk"		"1234567890"
	"InstalledDepots"
	{
		"999001"
		{
			"manifest"		"111"
			"size"		"1234567890"
		}
	}
}
"#;

    #[test]
    fn a_manifest_reads_back_as_what_is_installed() {
        let node = parse(MANIFEST);
        assert_eq!(node.number(&["AppState", "appid"]), Some(999000));
        assert_eq!(node.string(&["AppState", "name"]), Some("An Invented Game"));
        assert_eq!(
            node.string(&["AppState", "installdir"]),
            Some("Invented Game")
        );
        assert_eq!(node.number(&["AppState", "SizeOnDisk"]), Some(1234567890));
        assert_eq!(node.number(&["AppState", "StateFlags"]), Some(4));
    }

    /// Valve's own files disagree with themselves about capitals, so keys are
    /// matched without regard to it.
    #[test]
    fn keys_are_matched_whatever_case_they_were_written_in() {
        let node = parse(MANIFEST);
        assert_eq!(node.number(&["appstate", "APPID"]), Some(999000));
        assert_eq!(node.number(&["AppState", "sizeondisk"]), Some(1234567890));
    }

    /// The library list is a block of blocks, which is how the paths and the
    /// apps in each one are found.
    #[test]
    fn the_library_list_reads_back_as_its_folders() {
        let text = r#"
"libraryfolders"
{
	"0"
	{
		"path"		"/somewhere/Steam"
		"apps"
		{
			"999000"		"1234567890"
		}
	}
	"1"
	{
		"path"		"/somewhere else/A Library"
		"apps"
		{
			"999002"		"22"
		}
	}
}
"#;
        let node = parse(text);
        let paths: Vec<&str> = node
            .block(&["libraryfolders"])
            .filter_map(|(_, folder)| folder.string(&["path"]))
            .collect();
        assert_eq!(paths, vec!["/somewhere/Steam", "/somewhere else/A Library"]);
    }

    /// A file Steam is halfway through writing parses as far as it got, so a
    /// game being installed reads as not-yet-installed rather than taking the
    /// whole library down with it.
    #[test]
    fn a_truncated_file_keeps_what_it_managed_to_say() {
        for cut in 1..MANIFEST.len() {
            let node = parse(&MANIFEST[..cut]);
            if let Some(name) = node.string(&["AppState", "name"]) {
                assert_eq!(name, "An Invented Game");
            }
        }
    }

    /// Comments and unquoted keys both occur in these files.
    #[test]
    fn comments_and_bare_keys_are_read() {
        let node = parse(
            r#"
            // which library this is
            "libraryfolders"
            {
                contentstatsid  "1234"   // and what it is called
            }
            "#,
        );
        assert_eq!(
            node.number(&["libraryfolders", "contentstatsid"]),
            Some(1234)
        );
    }

    /// A quoted string keeps what was in it, escapes included — a library on a
    /// path with a quote in its name is still that path.
    #[test]
    fn escapes_inside_a_string_are_undone() {
        let node = parse(r#""a" { "path" "/tmp/a \"quoted\" name\\here" }"#);
        assert_eq!(
            node.string(&["a", "path"]),
            Some(r#"/tmp/a "quoted" name\here"#)
        );
    }

    /// Asking for something that is not there is nothing, not a panic and not
    /// a wrong answer from a neighbouring key.
    #[test]
    fn a_missing_path_is_nothing() {
        let node = parse(MANIFEST);
        assert_eq!(node.string(&["AppState", "nothing"]), None);
        assert_eq!(node.string(&["nothing", "at", "all"]), None);
        assert_eq!(
            node.string(&["AppState", "InstalledDepots"]),
            None,
            "a block is not a string"
        );
        assert_eq!(node.block(&["AppState", "nothing"]).count(), 0);
    }
}
