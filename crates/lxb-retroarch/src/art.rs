//! The pictures of somebody's own games, from the place RetroArch gets its
//! own.
//!
//! A column of ROMs drawn as forty identical marks says only how many files are
//! in a folder. What makes it a shelf of *games* is the cover on each row and
//! the picture standing behind the one under the cursor — which is exactly what
//! the Steam column has, and there is no reason a game somebody dumped
//! themselves should be the poor relation of one they bought.
//!
//! ```text
//! https://thumbnails.libretro.com/
//!     <System Name>/
//!         Named_Boxarts/<Game Name>.png     the cover
//!         Named_Snaps/<Game Name>.png       a screenshot
//!         Named_Titles/<Game Name>.png      the title screen
//! ```
//!
//! Two of the three are fetched. The box art is the cover, the snap is the
//! picture behind the display, and the title screen is a third picture of the
//! same game that nothing here has a place for.
//!
//! ## The name is the whole problem
//!
//! `<Game Name>` is the name libretro's database gives the game, which is the
//! No-Intro or Redump name of the dump: `Tekken 6 (USA) (En,Fr,De,Es,It,Ru)`.
//! Somebody who dumped their own disc called the file `Tekken 6.iso`. RetroArch
//! asks for the file's name and takes the 404, which is why a hand-sorted
//! collection shows no artwork in RetroArch either.
//!
//! So the name is not guessed. The listing of a shelf is fetched once — a few
//! thousand names, a couple of hundred kilobytes compressed — kept on the disk,
//! and every game in the folder is matched against it here, with the tags that
//! say which dump it is taken off both sides first. `Tekken 6.iso` and
//! `Tekken 6 (USA) (En,Fr,De,Es,It,Ru).png` reduce to the same nine letters and
//! meet. See [`key`], which is the whole of the trick, and [`rank`], which
//! decides between the several dumps that reduce to it.
//!
//! ## What is on this disk afterwards
//!
//! ```text
//! $XDG_CACHE_HOME/linexinbar/retroarch-art/
//!     .shelves/<System Name>.list        the names, one per line
//!     <System Name>/Named_Boxarts/<Game Name>.png
//!     <System Name>/Named_Snaps/<Game Name>.png
//!     <System Name>/Named_Snaps/<Game Name>.none    the server has none
//! ```
//!
//! libretro's own layout, under this shell's cache rather than in RetroArch's
//! configuration: the emulator's thumbnail folder is the emulator's, and a
//! shell that wrote into it would be changing what somebody sees in a program
//! it does not own.
//!
//! The empty `.none` file is what stops a game with a cover and no screenshot
//! from asking for that screenshot once a session for the rest of the machine's
//! life. It is written where the server said 404, which is a fact about the
//! game; a network that was down writes nothing and is asked again.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::consoles::{self, Machine};
use crate::report::{Artwork, Picturing, Rom, PROTOCOL};

/// Where libretro publishes the pictures.
const SERVER: &str = "https://thumbnails.libretro.com";

/// How long one request may take. A cover is under a megabyte; the ceiling is
/// for a line that has stopped rather than for the pictures.
const PATIENCE: Duration = Duration::from_secs(120);

/// The largest thing that may come down under the name of a picture.
const CEILING: u64 = 32 * 1024 * 1024;

/// How long a shelf's listing is believed before it is fetched again.
///
/// Long, because what it is a listing *of* is a database of dumps of games that
/// came out decades ago: what changes between one fortnight and the next is
/// somebody adding artwork for a title that had none, and the cost of hearing
/// about it a fortnight late is that one game keeps the mark it already had.
const FRESH: Duration = Duration::from_secs(14 * 24 * 60 * 60);

/// Which picture of a game is meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Piece {
    /// The cover, which is what a row *is*.
    Boxart,
    /// A screenshot, which is what stands behind the display.
    Snap,
}

impl Piece {
    /// The folder libretro keeps it in.
    fn folder(self) -> &'static str {
        match self {
            Piece::Boxart => "Named_Boxarts",
            Piece::Snap => "Named_Snaps",
        }
    }
}

/// What one game's pictures came to.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Pictures {
    pub boxart: Option<PathBuf>,
    pub snap: Option<PathBuf>,
}

impl Pictures {
    pub fn is_empty(&self) -> bool {
        self.boxart.is_none() && self.snap.is_none()
    }
}

/// The names on one console's shelves, and which shelf each came off.
///
/// Held as `(shelf, name)` rather than as a map per shelf because that is the
/// question asked of it: a game is looked for across every shelf the console
/// has at once, and the answer has to say which one it was found on so the
/// picture can be fetched from there.
pub struct Shelves {
    listing: Vec<(&'static str, String)>,
}

impl Shelves {
    pub fn is_empty(&self) -> bool {
        self.listing.is_empty()
    }

    /// The best name for this game, and the shelf it is on.
    ///
    /// `None` where nothing on any shelf reduces to the same thing, which is
    /// the ordinary answer for a homebrew ROM, a translation patch somebody
    /// applied, or a game nobody has drawn a cover for.
    pub fn look_for(&self, title: &str) -> Option<(&'static str, &str)> {
        let wanted = key(title);
        if wanted.is_empty() {
            return None;
        }
        let tags = tags(title);
        self.listing
            .iter()
            .filter(|(_, name)| key(name) == wanted)
            .min_by_key(|(_, name)| rank(name, &tags))
            .map(|(shelf, name)| (*shelf, name.as_str()))
    }
}

/// A name reduced to what two people spelling it differently would still agree
/// on.
///
/// Three things happen to it, in this order, and each of them is one way the
/// same game gets written down twice:
///
/// * **the tags come off.** `(USA)`, `(En,Fr,De)`, `[!]`, `(Rev 1)` say which
///   *dump* this is, and the person who dumped their own disc wrote none of
///   them. They are what [`rank`] chooses between afterwards, so they are not
///   thrown away — only taken out of the question of which game this is.
/// * **the article comes back to the front.** A database sorts by title, so it
///   files a game as `Legend of Zelda, The`; a person names the file the way
///   the box does. Moved on both sides, so the two meet in the middle.
/// * **the punctuation goes.** `Tekken: Dark Resurrection`,
///   `Tekken - Dark Resurrection` and `Tekken Dark Resurrection` are one game
///   written by three people, and a colon is a character no filesystem the
///   database was built on would take.
///
/// What is deliberately *not* done is anything clever with numbers. Turning
/// roman numerals into figures would make `Final Fantasy VII` meet
/// `Final Fantasy 7` and would also make `Mega Man X` meet `Mega Man 10`, which
/// is a different game — and a wrong cover is worse than none, because a wrong
/// one is not obviously wrong.
pub fn key(name: &str) -> String {
    let mut out = String::new();
    for part in untagged(name).split(" - ") {
        let part = article_first(part.trim());
        for character in part.chars() {
            match character {
                '&' => out.push_str("and"),
                character if character.is_alphanumeric() => {
                    out.extend(character.to_lowercase());
                }
                _ => {}
            }
        }
    }
    out
}

/// The name with every `(...)` and `[...]` group taken out of it.
fn untagged(name: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    for character in name.chars() {
        match character {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(character),
            _ => {}
        }
    }
    out
}

/// `Legend of Zelda, The` as `The Legend of Zelda`.
///
/// Only the three English articles, and only where the whole tail after the
/// last comma is one of them: a database writes exactly `, The`, and anything
/// else that ends in a comma and a word is a title with a comma in it.
fn article_first(part: &str) -> String {
    let Some((head, tail)) = part.rsplit_once(", ") else {
        return part.to_string();
    };
    let article = tail.trim();
    if ["the", "a", "an", "les", "la", "le", "der", "die", "das"]
        .contains(&article.to_lowercase().as_str())
    {
        return format!("{article} {head}");
    }
    part.to_string()
}

/// Everything inside `(...)` and `[...]`, lowercased — which is the half of a
/// name that says *which dump* it is.
fn tags(name: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    for character in name.chars() {
        match character {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            _ if depth > 0 => out.extend(character.to_lowercase()),
            _ => {}
        }
    }
    out
}

/// How much this dump is wanted, smallest first.
///
/// Several names reduce to the same game and one of them has to be chosen: a
/// PlayStation Portable shelf has two `Tekken 6` and eleven
/// `Tekken - Dark Resurrection`. What separates them is three things, in the
/// order somebody would sort them by:
///
/// 1. **whether it is the game at all.** A beta, a demo, a prototype and a
///    sample are not what somebody put in their folder — unless they said so in
///    their own file's name, which is the one case where they are exactly what
///    was asked for.
/// 2. **the region.** The one the file asked for if it asked; otherwise a
///    world release, then the United States, then Europe, then Japan. Not a
///    judgement about anybody: it is the order a shelf of English-language
///    covers comes in, and the cover is what this is for.
/// 3. **how much else is on the name.** Between two dumps that are equal
///    otherwise, the plainer name is the one that is more likely to be the
///    plain release.
fn rank(name: &str, wanted: &str) -> (usize, u8, usize, String) {
    /// The marks that say a dump is not the release everybody played.
    const ODD: &[&str] = &["beta", "demo", "proto", "sample", "hack", "pirate", "unl"];
    /// In the order a shelf of English-language covers comes in.
    const REGIONS: &[&str] = &["world", "usa", "europe", "japan"];

    let tags = tags(name);
    // How far this dump is from what the file asked for, counted both ways: a
    // demo loses to the game it is a demo of, and wins where the file itself
    // says "demo" — which is the one case where a demo is exactly what somebody
    // put in their folder.
    let odd = ODD
        .iter()
        .filter(|mark| tags.contains(*mark) != wanted.contains(*mark))
        .count();
    let region = if REGIONS
        .iter()
        .any(|region| wanted.contains(region) && tags.contains(region))
    {
        0
    } else {
        REGIONS
            .iter()
            .position(|region| tags.contains(region))
            .map_or(8, |place| place as u8 + 1)
    };
    // The name itself last, so that a shelf holding two dumps this cannot
    // otherwise separate is answered the same way twice — a cover that changed
    // between two runs for no reason anybody could see would be worse than
    // either of them.
    (odd, region, name.chars().count(), name.to_string())
}

/// Where this shell keeps what it has fetched.
///
/// `None` on a machine with neither `XDG_CACHE_HOME` nor `HOME`, which is a
/// machine with nowhere to put a picture — and is answered by fetching none
/// rather than by writing somewhere nobody asked for.
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
        .map(|cache| cache.join("linexinbar").join("retroarch-art"))
}

/// Where one picture is, or would be.
fn picture_at(cache: &Path, shelf: &str, piece: Piece, name: &str) -> PathBuf {
    cache.join(shelf).join(piece.folder()).join(name)
}

/// The pictures already on this disk for one game.
///
/// No network at all: it is a look at the shelves' listings, a reduction of the
/// game's name, and two `stat`s. That is what lets every scan answer with the
/// artwork it already has without asking libretro anything.
pub fn already(cache: &Path, shelves: &Shelves, title: &str) -> Pictures {
    let Some((shelf, name)) = shelves.look_for(title) else {
        return Pictures::default();
    };
    let held = |piece: Piece| {
        let at = picture_at(cache, shelf, piece, &format!("{name}.png"));
        at.is_file().then_some(at)
    };
    Pictures {
        boxart: held(Piece::Boxart),
        snap: held(Piece::Snap),
    }
}

/// The listings for one console's shelves, off the disk where they are fresh
/// and off the server where they are not.
///
/// `again` fetches whatever is there, which is what the row under Settings that
/// asks for the pictures a second time is for: a game that had no cover when
/// the folder was set up may have one now, and the only thing standing between
/// somebody and it is a listing this process believes.
pub fn shelves(
    agent: &ureq::Agent,
    cache: &Path,
    machine: Option<&'static Machine>,
    again: bool,
) -> Shelves {
    for shelf in machine.map(|machine| machine.shelves).unwrap_or_default() {
        let at = listing_file(cache, shelf);
        if !again && fresh(&at) {
            continue;
        }
        match fetch_listing(agent, shelf) {
            Ok(names) => store_listing(&at, &names),
            // Not fatal, and not even worth failing the game over: what is on
            // the disk from a fortnight ago is a perfectly good answer to which
            // games exist, and a machine that is off the air keeps whatever
            // artwork it already fetched.
            Err(why) => eprintln!("art: {shelf} could not be listed: {why}"),
        }
    }
    shelves_here(cache, machine)
}

/// The listings already on this disk, and nothing else.
///
/// It takes no agent and that is the point rather than a convenience: every
/// scan calls this, a scan runs whenever the shell starts, and a scan that
/// could reach the network would be the bar waiting on somebody's line to draw
/// a row. There is no argument to get wrong.
pub fn shelves_here(cache: &Path, machine: Option<&'static Machine>) -> Shelves {
    let mut listing = Vec::new();
    for shelf in machine.map(|machine| machine.shelves).unwrap_or_default() {
        let Ok(held) = std::fs::read_to_string(listing_file(cache, shelf)) else {
            continue;
        };
        listing.extend(
            held.lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(|line| (*shelf, line.to_string())),
        );
    }
    Shelves { listing }
}

/// Where one shelf's listing is kept.
fn listing_file(cache: &Path, shelf: &str) -> PathBuf {
    cache.join(".shelves").join(format!("{shelf}.list"))
}

/// Whether a listing on the disk is recent enough to be believed.
fn fresh(at: &Path) -> bool {
    std::fs::metadata(at)
        .and_then(|facts| facts.modified())
        .ok()
        .and_then(|when| SystemTime::now().duration_since(when).ok())
        .is_some_and(|age| age < FRESH)
}

/// Fetch the pictures for one game, and answer with what it now has.
///
/// Every failure short of "there is nowhere to write" is an absent picture
/// rather than an error: a game whose cover would not come down keeps the mark
/// it had, which is what the column looked like before any of this existed.
pub fn fetch(agent: &ureq::Agent, cache: &Path, shelves: &Shelves, title: &str) -> Pictures {
    let Some((shelf, name)) = shelves.look_for(title) else {
        return Pictures::default();
    };
    let file = format!("{name}.png");
    let mut got = Pictures::default();
    for piece in [Piece::Boxart, Piece::Snap] {
        let at = picture_at(cache, shelf, piece, &file);
        if at.is_file() {
            got.set(piece, Some(at));
            continue;
        }
        // The server has already been asked about this one and had none.
        if at.with_extension("none").is_file() {
            continue;
        }
        match one(agent, shelf, piece, &file, &at) {
            Ok(true) => got.set(piece, Some(at)),
            // A picture that is not there is a fact about the game, written
            // down so it is never asked about again.
            Ok(false) => mark_absent(&at),
            Err(why) => eprintln!("art: {name} ({shelf}) — {why}"),
        }
    }
    got
}

impl Pictures {
    fn set(&mut self, piece: Piece, at: Option<PathBuf>) {
        match piece {
            Piece::Boxart => self.boxart = at,
            Piece::Snap => self.snap = at,
        }
    }
}

/// Fetch one picture. `Ok(false)` where the server has none.
fn one(
    agent: &ureq::Agent,
    shelf: &str,
    piece: Piece,
    file: &str,
    into: &Path,
) -> Result<bool, String> {
    let url = format!(
        "{SERVER}/{}/{}/{}",
        encoded(shelf),
        piece.folder(),
        encoded(file)
    );
    let response = agent
        .get(&url)
        .call()
        .map_err(|err| format!("it could not be fetched: {err}"))?;
    let status = response.status().as_u16();
    // 403 as well as 404: a path the server has never heard of answers either
    // way depending on which edge is asked, and both mean the same thing about
    // the game.
    if status == 404 || status == 403 {
        return Ok(false);
    }
    if !(200..300).contains(&status) {
        return Err(format!("the server answered {status}"));
    }

    let mut reader = response.into_body().into_reader().take(CEILING + 1);
    let mut raw = Vec::new();
    reader
        .read_to_end(&mut raw)
        .map_err(|err| format!("the download stopped: {err}"))?;
    if raw.len() as u64 > CEILING {
        return Err("what is coming down is far too large to be a picture".to_string());
    }
    // The eight bytes every PNG begins with. Worth checking for the reason a
    // core's ELF header is: this is written to somebody's disk under a name
    // that says what it is, and a proxy that answered a picture with a login
    // page must not leave that there for every later run to find and believe.
    if !raw.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err("what came down is not a picture".to_string());
    }
    write_file(into, &raw)?;
    Ok(true)
}

/// Say, on the disk, that the server has no such picture.
///
/// An empty file at the same name with `.none` after it. Its failure is worth a
/// line and nothing more: what it costs is one 404 next time.
fn mark_absent(at: &Path) {
    if let Err(err) = write_file(&at.with_extension("none"), &[]) {
        eprintln!("art: {} could not be marked absent: {err}", at.display());
    }
}

/// The names on one shelf, off the server.
///
/// The listing is an HTML index — the server offers nothing else — so what is
/// read out of it is the `href` of every `.png` and no more. Nothing else on
/// the page is parsed, and a page that is not what was expected comes back
/// empty rather than as something to act on.
fn fetch_listing(agent: &ureq::Agent, shelf: &str) -> Result<Vec<String>, String> {
    let url = format!("{SERVER}/{}/{}/", encoded(shelf), Piece::Boxart.folder());
    let mut response = agent
        .get(&url)
        .call()
        .map_err(|err| format!("the server could not be reached: {err}"))?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(format!("the server answered {status}"));
    }
    let page = response
        .body_mut()
        .with_config()
        // A shelf is a few thousand names; the largest is about four megabytes
        // of HTML before it is compressed on the wire.
        .limit(64 * 1024 * 1024)
        .read_to_string()
        .map_err(|err| format!("the listing could not be read: {err}"))?;
    let names = listed(&page);
    if names.is_empty() {
        return Err("the listing came back with no games in it".to_string());
    }
    Ok(names)
}

/// Every game named on one page of the server's index.
fn listed(page: &str) -> Vec<String> {
    let mut names = Vec::new();
    for rest in page.split("href=\"").skip(1) {
        let Some((link, _)) = rest.split_once('"') else {
            continue;
        };
        // The column headers of the index itself are links too, and so is the
        // way back up out of the folder.
        let Some(file) = link.strip_suffix(".png") else {
            continue;
        };
        if file.contains('/') {
            continue;
        }
        names.push(decoded(file));
    }
    names.sort();
    names.dedup();
    names
}

/// Keep one shelf's listing where the next run will find it.
fn store_listing(at: &Path, names: &[String]) {
    let mut body = names.join("\n");
    body.push('\n');
    if let Err(err) = write_file(at, body.as_bytes()) {
        eprintln!("art: {} could not be kept: {err}", at.display());
    }
}

/// Write a file, making its folder and renaming it into place.
///
/// The rename is what makes a half-written picture impossible to find: another
/// process reading this cache never sees part of a file under a name that says
/// it is whole.
fn write_file(at: &Path, bytes: &[u8]) -> Result<(), String> {
    let Some(folder) = at.parent() else {
        return Err("it has nowhere to go".to_string());
    };
    std::fs::create_dir_all(folder)
        .map_err(|err| format!("{} could not be made: {err}", folder.display()))?;
    let part = at.with_extension("part");
    std::fs::write(&part, bytes).map_err(|err| format!("it could not be written: {err}"))?;
    std::fs::rename(&part, at).map_err(|err| {
        let _ = std::fs::remove_file(&part);
        format!("it could not be put in place: {err}")
    })
}

/// One name, as it goes into a URL.
///
/// Everything but the unreserved set, because a game's name holds spaces,
/// brackets, commas and ampersands and the server keeps its folders spelt
/// exactly as the database does.
fn encoded(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for byte in name.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            byte => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// One name, as it comes out of one.
fn decoded(link: &str) -> String {
    let raw = link.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(raw.len());
    let mut at = 0;
    while at < raw.len() {
        let hex = (at + 2 < raw.len() && raw[at] == b'%')
            .then(|| std::str::from_utf8(&raw[at + 1..at + 3]).ok())
            .flatten()
            .and_then(|pair| u8::from_str_radix(pair, 16).ok());
        match hex {
            Some(byte) => {
                out.push(byte);
                at += 3;
            }
            None => {
                out.push(raw[at]);
                at += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The one connection this run makes, kept for the whole of it.
///
/// Held rather than built per picture: a collection is hundreds of small files
/// off one host, and a fresh TLS session for each of them is most of the cost
/// of the picture — as well as being an impolite way to treat somebody's free
/// server.
fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        // A game with no cover answers 404, and that is an answer this acts on
        // rather than an error to be raised.
        .http_status_as_error(false)
        .timeout_global(Some(PATIENCE))
        .user_agent(concat!("LineXinBar/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// Fetch the pictures for a whole collection, saying what is happening as it
/// happens.
///
/// `consoles` is the library as [`crate::scan`] read it, and `only` narrows it
/// to particular games by their paths — which is what the menu row over one
/// game asks for. `again` throws away what is known about which names exist and
/// asks the server afresh.
///
/// Returns whether anything at all was found. Every line written is one JSON
/// object and the last is always `Done` or `Failed`, so a shell watching this
/// stream knows it is over without watching the process too.
pub fn run(
    out: &mut impl Write,
    consoles: &[crate::report::Console],
    only: &[String],
    again: bool,
) -> bool {
    let Some(cache) = cache() else {
        tell(
            out,
            Picturing::Failed,
            "",
            "",
            0,
            0,
            "There is nowhere to keep pictures",
            &Pictures::default(),
        );
        return false;
    };

    let wanted: Vec<(&crate::report::Console, &Rom)> = consoles
        .iter()
        .flat_map(|console| console.roms.iter().map(move |rom| (console, rom)))
        .filter(|(_, rom)| only.is_empty() || only.contains(&rom.path))
        .collect();
    let of = wanted.len() as u32;
    let agent = agent();
    tell(
        out,
        Picturing::Looking,
        "",
        "",
        0,
        of,
        "Looking for pictures",
        &Pictures::default(),
    );

    let mut found = 0u32;
    let mut shelf: Option<(String, Shelves)> = None;
    for (at, (console, rom)) in wanted.iter().enumerate() {
        let at = at as u32 + 1;
        // One listing per console rather than one per game, which is the whole
        // reason the library arrives sorted by console: a folder of four
        // hundred games would otherwise fetch the same few thousand names four
        // hundred times.
        if shelf.as_ref().is_none_or(|(key, _)| key != &console.key) {
            let machine = consoles::machine(&console.key);
            shelf = Some((console.key.clone(), shelves(&agent, &cache, machine, again)));
        }
        let Some((_, shelves)) = shelf.as_ref() else {
            continue;
        };
        if shelves.is_empty() {
            continue;
        }
        tell(
            out,
            Picturing::Fetching,
            &console.key,
            &rom.path,
            at,
            of,
            &rom.title,
            &Pictures::default(),
        );
        let got = fetch(&agent, &cache, shelves, &rom.title);
        if !got.is_empty() {
            found += 1;
        }
        tell(
            out,
            Picturing::Fetching,
            &console.key,
            &rom.path,
            at,
            of,
            &rom.title,
            &got,
        );
    }

    let note = match found {
        0 => "No pictures for these games".to_string(),
        1 => "One game got its pictures".to_string(),
        many => format!("{many} games got their pictures"),
    };
    eprintln!("art: {note} ({of} asked about)");
    tell(
        out,
        Picturing::Done,
        "",
        "",
        of,
        of,
        &note,
        &Pictures::default(),
    );
    found > 0
}

#[allow(clippy::too_many_arguments)]
fn tell(
    out: &mut impl Write,
    stage: Picturing,
    console: &str,
    rom: &str,
    at: u32,
    of: u32,
    note: &str,
    got: &Pictures,
) {
    let line = Artwork {
        protocol: PROTOCOL,
        stage,
        console: console.to_string(),
        rom: rom.to_string(),
        at,
        of,
        note: note.to_string(),
        boxart: got
            .boxart
            .as_ref()
            .map(|at| at.to_string_lossy().into_owned()),
        snap: got
            .snap
            .as_ref()
            .map(|at| at.to_string_lossy().into_owned()),
    };
    match serde_json::to_string(&line) {
        Ok(line) => {
            let _ = writeln!(out, "{line}");
            let _ = out.flush();
        }
        Err(err) => eprintln!("art: could not write the answer: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shelf(names: &[&str]) -> Shelves {
        Shelves {
            listing: names
                .iter()
                .map(|name| ("Sony - PlayStation Portable", (*name).to_string()))
                .collect(),
        }
    }

    /// The case this whole module exists for: somebody dumped their own disc
    /// and called the file what the box says, and the database calls it what
    /// the dump is.
    #[test]
    fn a_game_named_by_hand_finds_the_dump_it_is() {
        let shelves = shelf(&[
            "Tekken 6 (USA) (En,Fr,De,Es,It,Ru)",
            "Tekken - Dark Resurrection (Europe) (En,Fr,De,Es,It)",
            "Pachinka Mania P - CR Tekkenden Tough (Japan) (v1.01)",
        ]);
        assert_eq!(
            shelves.look_for("Tekken 6").map(|(_, name)| name),
            Some("Tekken 6 (USA) (En,Fr,De,Es,It,Ru)")
        );
        assert_eq!(
            shelves
                .look_for("Tekken - Dark Resurrection")
                .map(|(_, name)| name),
            Some("Tekken - Dark Resurrection (Europe) (En,Fr,De,Es,It)")
        );
    }

    /// A colon is a character no filesystem the database was built on would
    /// take, so the same game is written three ways by three people.
    #[test]
    fn the_punctuation_between_a_title_and_its_subtitle_does_not_matter() {
        let one = key("Tekken: Dark Resurrection");
        assert_eq!(one, key("Tekken - Dark Resurrection"));
        assert_eq!(one, key("Tekken Dark Resurrection"));
    }

    /// A database sorts by title and a person names the file the way the box
    /// does. Both are moved, so the two meet in the middle.
    #[test]
    fn a_title_filed_under_its_article_meets_one_written_the_way_it_is_said() {
        assert_eq!(
            key("Legend of Zelda, The - A Link to the Past (USA)"),
            key("The Legend of Zelda - A Link to the Past")
        );
    }

    /// A name that reduces to nothing at all matches nothing, rather than
    /// matching the first game whose name also reduces to nothing.
    #[test]
    fn a_name_with_no_letters_in_it_finds_nothing() {
        assert_eq!(shelf(&["(USA)"]).look_for("(Europe)"), None);
    }

    /// Several dumps reduce to one game and one of them has to be chosen. A
    /// demo is not what somebody put in their folder.
    #[test]
    fn a_demo_loses_to_the_game_it_is_a_demo_of() {
        let shelves = shelf(&[
            "Tekken 6 (USA) (Demo)",
            "Tekken 6 (Japan)",
            "Tekken 6 (USA) (En,Fr,De,Es,It,Ru)",
        ]);
        assert_eq!(
            shelves.look_for("Tekken 6").map(|(_, name)| name),
            Some("Tekken 6 (USA) (En,Fr,De,Es,It,Ru)")
        );
    }

    /// Unless it is what was asked for, which is the one case where a demo is
    /// exactly the game in somebody's folder.
    #[test]
    fn a_demo_wins_when_the_file_says_it_is_one() {
        let shelves = shelf(&["Tekken 6 (USA) (Demo)", "Tekken 6 (USA)"]);
        assert_eq!(
            shelves.look_for("Tekken 6 (Demo)").map(|(_, name)| name),
            Some("Tekken 6 (USA) (Demo)")
        );
    }

    /// The region the file asked for beats the order this would otherwise put
    /// them in.
    #[test]
    fn the_region_on_the_file_is_the_one_that_is_chosen() {
        let shelves = shelf(&["Tekken 6 (USA)", "Tekken 6 (Japan)", "Tekken 6 (Europe)"]);
        assert_eq!(
            shelves.look_for("Tekken 6 (Japan)").map(|(_, name)| name),
            Some("Tekken 6 (Japan)")
        );
        // And with nothing asked for, the shelf's own order.
        assert_eq!(
            shelves.look_for("Tekken 6").map(|(_, name)| name),
            Some("Tekken 6 (USA)")
        );
    }

    /// Two dumps this cannot separate must be answered the same way twice: a
    /// cover that changed between two runs for no reason anybody could see
    /// would be worse than either of them.
    #[test]
    fn a_choice_this_cannot_make_is_still_made_the_same_way_every_time() {
        let names = ["Doom (USA) (Rev 1)", "Doom (USA) (Rev 2)"];
        let one = shelf(&names)
            .look_for("Doom")
            .map(|(_, name)| name.to_string());
        let other = shelf(&[names[1], names[0]])
            .look_for("Doom")
            .map(|(_, name)| name.to_string());
        assert_eq!(one, other);
        assert!(one.is_some());
    }

    /// A game nothing on the shelf reduces to gets no picture, rather than the
    /// nearest thing on it: a wrong cover is worse than none, because a wrong
    /// one is not obviously wrong.
    #[test]
    fn a_game_the_shelf_has_never_heard_of_gets_nothing() {
        let shelves = shelf(&["Tekken 6 (USA)", "Tekken 2 (USA)"]);
        assert_eq!(shelves.look_for("Tekken 7"), None);
        assert_eq!(shelves.look_for("smb"), None);
    }

    /// The names come out of an HTML index, which is the only listing the
    /// server offers — and nothing else on that page is read as a game.
    #[test]
    fn only_the_pictures_are_read_out_of_the_index() {
        let page = r#"
            <a href="?C=N;O=D">Name</a>
            <a href="/">Parent Directory</a>
            <a href="Tekken%206%20%28USA%29%20%28En%2CFr%29.png">Tekken 6</a>
            <a href="Sub%20Folder/">a folder</a>
        "#;
        assert_eq!(listed(page), vec!["Tekken 6 (USA) (En,Fr)".to_string()]);
    }

    /// Round trip: a name with every character a game's name actually holds
    /// survives being put into a URL and read back out of one.
    #[test]
    fn a_name_survives_being_spelt_for_a_server() {
        let name = "Tekken 6 (USA) (En,Fr,De,Es,It,Ru) & Co. [!].png";
        assert_eq!(decoded(&encoded(name)), name);
        assert!(!encoded(name).contains(' '));
    }

    /// Every console this helper knows names a shelf that libretro's server
    /// actually has. A spelling one character out is a console whose every game
    /// draws no cover, with nothing in the log to say why — so the list is
    /// checked here rather than found out on somebody's television.
    #[test]
    fn every_console_names_a_shelf() {
        for machine in consoles::CONSOLES {
            assert!(
                !machine.shelves.is_empty(),
                "{} names no shelf",
                machine.title
            );
            for shelf in machine.shelves {
                assert!(
                    !shelf.is_empty() && !shelf.ends_with('/'),
                    "{} names {shelf:?}",
                    machine.title
                );
            }
        }
    }
}

#[cfg(test)]
mod drawings {
    use super::super::consoles::CONSOLES;

    /// Every console's mark names a drawing this package actually ships, and
    /// every drawing it ships belongs to a console.
    ///
    /// The two halves are one thing said from both ends, and both have gone
    /// wrong before in this tree: a name with no file behind it is a column
    /// wearing the fallback pad with nothing in the log to say why, and a file
    /// with no name in front of it is a cell of the shell's atlas spent on a
    /// drawing nothing ever asks for.
    ///
    /// Read off the directory rather than from a list, because a list would be
    /// the third place the same fact is written down.
    #[test]
    fn every_console_mark_is_a_drawing_that_ships() {
        let at = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("glyphs");
        let mut shipped: Vec<String> = std::fs::read_dir(&at)
            .expect("the glyph directory")
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                let stem = path.file_stem()?.to_str()?;
                (path.extension()? == "svg" && stem.starts_with("console-"))
                    .then(|| format!("lxb:{stem}"))
            })
            .collect();
        shipped.sort();

        for machine in CONSOLES {
            assert!(
                shipped.iter().any(|had| had == machine.glyph),
                "{} wears {}, and no such drawing is in {}",
                machine.title,
                machine.glyph,
                at.display()
            );
        }
        for drawing in &shipped {
            assert!(
                CONSOLES.iter().any(|machine| machine.glyph == drawing),
                "{drawing} is shipped and no console wears it"
            );
        }
        assert_eq!(shipped.len(), CONSOLES.len());
    }

    /// A screen is drawn to the shape of the panel that was behind it.
    ///
    /// This is the one measurement in a console drawing that a person checks
    /// without meaning to, and it is where this set went wrong first: the DS
    /// was given letterbox screens, which no DS ever had — both of its panels
    /// are 256 by 192, the same four to three as a television of the time — and
    /// the Advance was given a square one, when 240 by 160 is the whole reason
    /// that machine turned sideways.
    ///
    /// So the numbers live here, next to the drawings, and a redraw that
    /// disagrees with the hardware fails rather than shipping.  The tolerance
    /// is wide because a drawing measures the *bezel* and the figure below is
    /// the panel inside it, and the two are never quite the same shape.
    #[test]
    fn every_screen_is_the_shape_its_panel_was() {
        // The drawing, the opening in it, and what the machine displayed.
        const PANELS: &[(&str, &str, f32)] = &[
            ("console-gb.svg", "screen", 160.0 / 144.0),
            ("console-gbc.svg", "screen", 160.0 / 144.0),
            ("console-gba.svg", "screen", 240.0 / 160.0),
            ("console-nds.svg", "upper", 256.0 / 192.0),
            ("console-nds.svg", "lower", 256.0 / 192.0),
            ("console-3ds.svg", "upper", 400.0 / 240.0),
            ("console-3ds.svg", "lower", 320.0 / 240.0),
            ("console-psp.svg", "screen", 480.0 / 272.0),
            ("console-gg.svg", "screen", 4.0 / 3.0),
            ("console-lynx.svg", "screen", 160.0 / 102.0),
            ("console-ngp.svg", "screen", 160.0 / 152.0),
            ("console-wonderswan.svg", "screen", 224.0 / 144.0),
            ("console-pokemini.svg", "screen", 96.0 / 64.0),
            // Nine inches of tube stood on its end, which is why every game on
            // a Vectrex is played up the screen rather than across it.
            ("console-vectrex.svg", "screen", 3.0 / 4.0),
            ("console-amiga.svg", "pane", 4.0 / 3.0),
        ];
        const SLACK: f32 = 0.12;

        let at = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("glyphs");
        for (file, opening, panel) in PANELS {
            let drawing = std::fs::read_to_string(at.join(file)).expect("a drawing");
            let line = drawing
                .lines()
                .find(|line| line.contains(&format!("id=\"{opening}\"")))
                .unwrap_or_else(|| panic!("{file} has no opening called {opening}"));
            let measure = |what: &str| -> f32 {
                let from = line
                    .find(&format!("{what}=\""))
                    .unwrap_or_else(|| panic!("{file}: {opening} has no {what}"))
                    + what.len()
                    + 2;
                line[from..]
                    .split('"')
                    .next()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or_else(|| panic!("{file}: {opening} has an unreadable {what}"))
            };
            let drawn = measure("width") / measure("height");
            assert!(
                (drawn - panel).abs() <= SLACK,
                "{file}: {opening} is drawn {drawn:.3} wide for its height, \
                 and the panel was {panel:.3}"
            );
        }
    }

    /// And every one of them says it ships as the shape of itself.
    ///
    /// Without that marker the shell rasterises the file as a *picture* and
    /// draws it flat, next to forty-four marks made of water. The shell's own
    /// set has the same test; see `icons::tests::a_glyph_can_ship_as_the_shape_of_itself`.
    #[test]
    fn every_drawing_ships_as_the_shape_of_itself() {
        let at = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("glyphs");
        for entry in std::fs::read_dir(&at)
            .expect("the glyph directory")
            .flatten()
        {
            let path = entry.path();
            if path.extension().is_none_or(|kind| kind != "svg") {
                continue;
            }
            let drawing = std::fs::read_to_string(&path).expect("a drawing");
            assert!(
                drawing.contains("lxb:shape"),
                "{} does not say it is a shape",
                path.display()
            );
            // Pure white and nothing else: the atlas multiplies the quad's own
            // colour in, and a drawing that painted its own would be the one
            // mark on the bar that does not take the palette.
            assert!(
                !drawing.contains("stop-color") && !drawing.contains("Gradient"),
                "{} paints its own material",
                path.display()
            );
        }
    }
}
