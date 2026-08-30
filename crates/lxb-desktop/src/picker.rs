//! The panel an application outside this session asks a file question through.
//!
//! Every other walk of the disk in this shell is a walk of the *bar*: Files is
//! a column, choosing a wallpaper is a column, and carrying a file somewhere is
//! the same column mirrored. That works because in all of them the shell is the
//! thing on screen and the bar is what the user is already standing in.
//!
//! This one is not like that. It arrives while somebody is in the middle of
//! something else — a browser wanting a photograph to upload, a game wanting a
//! save folder — and the thing on screen is that application, filling the
//! display. There is no bar to walk. So the question comes as a panel over the
//! top of whatever is there, covering [`SHARE`] of it and leaving the rest
//! showing, which is what says that the application is still underneath and
//! still waiting.
//!
//! ## It is Files, framed
//!
//! What is inside the panel is the explorer and nothing invented: the same
//! [`crate::files::listing`], the same rows, the same folders-first order, the
//! same search field at the head of a column, the same New folder row where the
//! folder can be written to. And it walks the same way — Right steps into a
//! folder and opens a column beside the one it came from, Left steps back out,
//! and the trail of columns across the panel *is* the path. Somebody who has
//! opened Files once already knows how to drive this.
//!
//! Three things are its own, and each of them is because the question came from
//! outside:
//!
//! * **A head row that answers.** For every purpose but "one file", the answer
//!   is not a row of the listing — it is the folder being stood in, or the set
//!   that has been ticked — so there is a row at the top of every column that
//!   ends the question. The same shape [`crate::transfer`]'s Paste row has, and
//!   for the same reason.
//! * **The kinds.** An application may say it only wants images; when it does,
//!   the files that are not are left out, and which kind is in force is one row
//!   of the panel's own menu. See [`Kind`].
//! * **Nothing here destroys anything.** There is no Delete, no Rename, no Copy
//!   and no Move. An application's file question is not a file manager, and a
//!   panel raised by a web page should not be one press from emptying a folder.
//!   New folder is the one exception, because a save that cannot make a folder
//!   is a save that can only ever go where something already is.
//!
//! ## The trash is not offered
//!
//! For the reason [`crate::files::Place::Trash`] gives and nothing more: every
//! walk that is *choosing* something is choosing a file to keep, and the trash
//! is where things go that the user has said they do not want. A picker that
//! offered it would be offering an answer that disappears the next time the
//! trash is emptied.

use std::path::{Path, PathBuf};

use crate::apps::Entry;
use crate::files::{self, Place, Shows};

/// How much of the display the panel covers, on its longest side.
///
/// Eight tenths, which is the number the panel exists to be: enough that a
/// listing is a listing rather than a peephole, and short of the whole screen,
/// so the application underneath is visibly still there and visibly still
/// waiting. A chooser that filled the display would be indistinguishable from
/// the shell having taken over.
pub const SHARE: f32 = 0.8;

/// How many columns of the walk are drawn at once.
///
/// Three, which is the trail this panel is wide enough to read: the folder
/// being stood in, the one it was opened from, and one more behind that. Deeper
/// than that the columns are narrower than a file name and the trail stops
/// being legible, which is the thing it is there for.
pub const COLUMNS: usize = 3;

/// What the application wants chosen.
///
/// `lxb_shell_v1`'s own `picking` enum, transcribed. One enum rather than a set
/// of flags because the four are four different questions and exactly one of
/// them is being asked at a time — and because what differs between them is the
/// head row, which is one row and cannot be four.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum For {
    /// One file that is already there. The only purpose whose answer is a row
    /// of the listing rather than a head row.
    OneFile,
    /// Any number of files that are already there.
    ManyFiles,
    /// A folder, whatever is in it.
    AFolder,
    /// A folder and a name to write into it.
    ANewFile,
}

impl For {
    /// Whether the files are listed at all. They are not where the answer is a
    /// folder: a column offering rows that cannot be pressed, with the folder
    /// somebody is looking for somewhere in the middle of their music, is the
    /// argument [`Shows::Folders`] already makes.
    fn lists_files(self) -> bool {
        !matches!(self, For::AFolder)
    }

    /// Whether a new folder may be made from here. Only where the answer is
    /// somewhere to *write*: a brand new empty folder is not an answer to
    /// "which file", and a row offering to make one in a panel raised by a web
    /// page is a row nobody asked for.
    fn makes_folders(self) -> bool {
        matches!(self, For::AFolder | For::ANewFile)
    }

    /// What the row that ends the question says when the application did not
    /// say. Nothing at all for one file, which is answered by pressing the file.
    fn accept(self) -> Option<&'static str> {
        match self {
            For::OneFile => None,
            For::ManyFiles => Some("Open"),
            For::AFolder => Some("Use this folder"),
            For::ANewFile => Some("Save here"),
        }
    }

    /// How the panel says what it is for, where the application gave no title
    /// of its own.
    fn asking(self) -> &'static str {
        match self {
            For::OneFile => "wants a file",
            For::ManyFiles => "wants some files",
            For::AFolder => "wants a folder",
            For::ANewFile => "wants somewhere to save",
        }
    }
}

/// One pattern of one kind of file, as the application spelled it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pattern {
    /// A shell pattern matched against the name — `*.png`.
    Glob(String),
    /// A media type matched against what the file is — `image/png`, or
    /// `image/*` for all of them.
    Mime(String),
}

impl Pattern {
    /// Whether one file is of this pattern.
    fn matches(&self, name: &str, mime: &str) -> bool {
        match self {
            // Case-folded, because a pattern is written `*.png` and the file on
            // somebody's camera is called `IMG_0001.JPG`. Every desktop's
            // chooser does this and an application that meant otherwise has no
            // way of saying so.
            Pattern::Glob(pattern) => globbed(&pattern.to_lowercase(), &name.to_lowercase()),
            Pattern::Mime(wanted) => match wanted.split_once('/') {
                Some((group, "*")) => mime.split_once('/').map(|(is, _)| is) == Some(group),
                _ => wanted == mime,
            },
        }
    }
}

/// Whether `name` is of `pattern`, where `*` stands for any run of characters
/// and `?` for exactly one.
///
/// A bracket expression — `[abc]`, `[a-z]`, `[!abc]` — stands for one character
/// out of a set, which is `fnmatch`'s own third wildcard.
///
/// Written out rather than pulled from a crate because it is this function: the
/// patterns that reach it are the ones written into desktop files and portal
/// calls. The brackets were left out of the first cut on the grounds that
/// `[0-9]` in a file filter is vanishingly rare — which was simply wrong, and
/// wrong in the one way that matters. Firefox spells *every* filter it sends
/// this way, because GTK's own matcher is case-sensitive and a bracket per
/// letter is how it asks for a case-insensitive one: an upload of a photograph
/// arrives here asking for `*.[pP][nN][gG]`, and a matcher that read the
/// brackets as characters answered that a folder full of photographs held none.
/// Captured off the session bus; see the tests below, which are that capture.
fn globbed(pattern: &str, name: &str) -> bool {
    let (pattern, name): (Vec<char>, Vec<char>) =
        (pattern.chars().collect(), name.chars().collect());
    // The classic two-cursor walk: `star` and `back` remember the last `*` and
    // where the name had got to when it was reached, so a mismatch later can
    // give that `*` one more character and try again. Linear in the name, and
    // no recursion to run away on a pattern of forty stars.
    let (mut p, mut n) = (0usize, 0usize);
    let (mut star, mut back) = (None, 0usize);
    while n < name.len() {
        // Where the pattern carries on, if what is at `p` is what is at `n`.
        // One answer for all four kinds of pattern element, because a bracket
        // is several characters of pattern against one of name and the two
        // cursors have to be allowed to move by different amounts.
        let step = match pattern.get(p) {
            Some('*') => {
                star = Some(p);
                back = n;
                p += 1;
                continue;
            }
            Some('?') => Some(p + 1),
            Some('[') => match bracketed(&pattern, p, name[n]) {
                Some((true, next)) => Some(next),
                Some((false, _)) => None,
                // A bracket that is never closed is the character it is, which
                // is what `fnmatch` does with one and the only reading that
                // cannot lose a file to a typo in an application's filter.
                None => (name[n] == '[').then_some(p + 1),
            },
            Some(character) => (*character == name[n]).then_some(p + 1),
            None => None,
        };
        match step {
            Some(next) => {
                p = next;
                n += 1;
            }
            None => match star {
                Some(at) => {
                    p = at + 1;
                    back += 1;
                    n = back;
                }
                None => return false,
            },
        }
    }
    // Trailing stars match the empty rest of the name; anything else does not.
    pattern[p..].iter().all(|character| *character == '*')
}

/// Whether the bracket expression that opens at `pattern[p]` accepts
/// `character`, and where the pattern carries on after it.
///
/// `None` where the bracket is never closed, which is not an expression at all.
///
/// POSIX's own three corners are kept, because an application's filter is
/// written against `fnmatch` and not against this: a `]` straight after the
/// opening bracket is that character rather than the end of the set, a `-` with
/// nothing after it is that character rather than a range, and `!` or `^` in
/// the first place turns the whole set inside out.
fn bracketed(pattern: &[char], p: usize, character: char) -> Option<(bool, usize)> {
    let mut at = p + 1;
    let inverted = matches!(pattern.get(at), Some('!') | Some('^'));
    if inverted {
        at += 1;
    }
    let (mut hit, mut first) = (false, true);
    loop {
        let this = *pattern.get(at)?;
        if this == ']' && !first {
            return Some((hit != inverted, at + 1));
        }
        first = false;
        match (pattern.get(at + 1), pattern.get(at + 2)) {
            (Some('-'), Some(last)) if *last != ']' => {
                hit |= this <= character && character <= *last;
                at += 3;
            }
            _ => {
                hit |= this == character;
                at += 1;
            }
        }
    }
}

/// One kind of file the application will accept: what it is called, and every
/// pattern that is it.
///
/// The portal sends one pattern per protocol request; they are gathered back
/// into kinds here, because a kind is one row of the panel's menu however many
/// patterns are behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kind {
    pub name: String,
    pub patterns: Vec<Pattern>,
}

impl Kind {
    /// Add one pattern to the list, opening a new kind where the name is new.
    ///
    /// The order is the order the application gave, which is the order the
    /// panel offers them in and the order the answer counts them by.
    pub fn gather(kinds: &mut Vec<Kind>, name: String, pattern: Pattern) {
        if let Some(kind) = kinds.iter_mut().find(|kind| kind.name == name) {
            kind.patterns.push(pattern);
            return;
        }
        kinds.push(Kind {
            name,
            patterns: vec![pattern],
        });
    }

    fn matches(&self, name: &str, mime: &str) -> bool {
        self.patterns
            .iter()
            .any(|pattern| pattern.matches(name, mime))
    }
}

/// The whole of one question, as it arrived.
#[derive(Debug, Clone)]
pub struct Asked {
    /// The portal's own number for it, quoted back in the answer.
    pub id: u32,
    /// What the application calls itself. Empty is possible and is said out
    /// loud rather than drawn as a blank.
    pub app_id: String,
    pub purpose: For,
    /// What the application called the question. Never trusted for anything but
    /// its own line of text.
    pub title: String,
    /// The word the application wants on the row that answers, or empty for
    /// this shell's own.
    pub accept: String,
    /// What a new file is called to begin with.
    pub name: String,
    /// A folder to open in, if the application named one this shell could find.
    pub at: Option<PathBuf>,
    pub kinds: Vec<Kind>,
}

/// One row of one column.
///
/// Its own kind rather than [`crate::apps::Entry`], for the reason
/// [`crate::transfer::Row`] is its own: that enum is the *bar's*, twenty-odd
/// variants of things this panel can never show, and a picker that took it
/// would be a picker with twenty arms that cannot be reached. What is here is
/// the six rows a file question has.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Row {
    /// Makes a folder in the one this column lists.
    NewFolder,
    /// The name a new file is to be given.
    Name,
    /// Ends the question: with this folder, or with what has been ticked.
    Answer,
    /// The field at the head of the listing.
    Search,
    /// The row that empties it.
    Clear,
    Folder {
        name: String,
        note: Option<String>,
        path: PathBuf,
    },
    File {
        name: String,
        note: String,
        path: PathBuf,
        /// What the file is, as [`files::listing`] worked it out. Carried
        /// rather than derived again from the name because it is what says
        /// whether a picture of this row is worth making, and that is asked
        /// once a frame for every row on screen.
        mime: &'static str,
        glyph: &'static str,
    },
}

impl Row {
    fn path(&self) -> Option<&Path> {
        match self {
            Row::Folder { path, .. } | Row::File { path, .. } => Some(path),
            _ => None,
        }
    }
}

/// One column: a place, and what is in it.
#[derive(Debug)]
struct Level {
    /// What this column lists. `None` for the first one, which is the disks
    /// there are to look in rather than a folder on any of them.
    at: Option<PathBuf>,
    rows: Vec<Row>,
    selected: usize,
    /// Where the column is drawn, as a row index — the chosen row's, eased
    /// after it, and how fast it is travelling. The bar's own arrangement: the
    /// selection stays on the cross and the list slides under it, rather than
    /// the list paging when the cursor reaches an edge. See [`crate::ui::build`].
    position: f32,
    speed: f32,
    /// What has been typed into this column's field.
    query: String,
    /// Which of the nine orders this listing could actually be put in, so the
    /// Sort panel can grey the rest. The explorer's own answer, carried through
    /// — see [`crate::media::Sort::orders`].
    orders: crate::media::Orders,
    /// What the column is of, drawn over it.
    title: String,
}

/// One row, as the drawing wants it.
#[derive(Debug, Clone)]
pub struct Face {
    pub title: String,
    pub note: Option<String>,
    pub glyph: &'static str,
    /// Whether it is only there to be read: a file in a walk that is choosing a
    /// folder, a row that cannot be taken from where the user is standing.
    pub quiet: bool,
    /// Whether it carries a tick, which is only ever true while several files
    /// are being chosen.
    pub ticked: bool,
    /// The file this row would show a picture *of*, where it is one a picture
    /// can be made of at all.
    ///
    /// The drawing asks the atlas and falls back to [`Self::glyph`], so a row
    /// whose picture has not arrived yet — or never will — is a row with a mark
    /// on it rather than a hole. See [`crate::ui::SlotLookup::thumbnail`].
    pub preview: Option<PathBuf>,
}

/// One column, as the drawing wants it: everything but the rows, which are
/// fetched one at a time through [`Picker::row_face`].
#[derive(Debug, Clone)]
pub struct Column {
    /// How deep it is, counted from the disks — which is what says where it
    /// stands along the trail.
    pub level: usize,
    /// How many rows it holds altogether.
    pub rows: usize,
    /// Which of them the cursor is on. Every column has one, whether or not it
    /// is the column being stood in: a column the path runs *through* keeps the
    /// row it was opened from, which is the trail.
    pub selected: usize,
    /// Where the column is drawn, as a row index — [`Self::selected`] eased
    /// after it, so the list slides under the cross rather than jumping.
    pub position: f32,
    /// What the column is of.
    pub title: String,
}

/// Which of the panel's fields the keyboard is going into.
///
/// Three, and they are three because they are typed in three different places
/// and mean three different things — narrowing the column, naming the file
/// being written, naming a folder being made. A single "the field" would be a
/// state whose every use began by asking which one it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    /// The field at the head of the column being stood in.
    Search,
    /// The name the file being saved will have.
    Name,
    /// The name of a folder being made in the one being stood in.
    NewFolder,
}

/// What a press on a row asks the shell to do.
///
/// An enum rather than the picker doing it, because half of these need things
/// the picker has not got: the on-screen keyboard, the compositor, and the
/// portal's own answer. The half it *can* do — stepping into a folder, ticking
/// a row, changing a kind — it has already done by the time this comes back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Act {
    /// Nothing to do. The press was taken and dealt with here.
    Nothing,
    /// Open the on-screen keyboard on this column's search field.
    Search,
    /// Open it on the name of the file being saved.
    Name,
    /// Open it on the name of a folder to make in this one.
    MakeFolder(PathBuf),
    /// End the question with these files.
    Answer(Vec<PathBuf>),
    /// The row cannot be taken, and this is what it says.
    Blocked(&'static str),
}

/// The panel itself.
pub struct Picker {
    asked: Asked,
    /// Which kind of file is in force, or nothing for the whole disk. Opens on
    /// the first one the application offered, which is the one it meant.
    kind: Option<usize>,
    levels: Vec<Level>,
    /// Which column the cursor stands in.
    depth: usize,
    /// The files that have been ticked, where several are being chosen. Ordered
    /// and unique, so the application is handed the same set however it was
    /// gathered.
    ticked: Vec<PathBuf>,
    /// What the file being saved is called so far.
    name: String,
    /// How the folders are read: the order, and whether the dotfiles are in.
    /// Held rather than asked for on every read, because it is what a re-read
    /// after the menu changed one of them has to be done with.
    how: files::How,
    /// Which field the keyboard is going into, if any. The caret is drawn on
    /// the row it names, which is what says a key is a letter rather than a
    /// button.
    typing: Option<Field>,
    /// The name of the folder being made, while one is being named.
    ///
    /// Held here rather than on the row for the reason [`Searching`] holds a
    /// query: the column is read again on the frame the folder appears, and a
    /// field that read itself off the listing would lose what had been typed.
    ///
    /// [`Searching`]: crate::Searching
    making: String,
    /// Where the trail of columns is drawn, as a level index — the column being
    /// stood in, eased after it, so stepping in and out *slides* rather than
    /// jumping, and how fast it is travelling.
    /// [`crate::transfer::Transfer::depth`]'s own, and the bar's before that.
    depth_linear: f32,
    depth_speed: f32,
    /// How much of the way in the panel is: 0 off the screen, 1 landed.
    open: bool,
    arrival: f32,
}

/// How long the panel takes to arrive, and to fold away again.
///
/// The dialog's own, and the same constant rather than the same number written
/// twice, because it is the same gesture: something with its own answer in it
/// taking the middle of the screen over whatever was there.
pub const PANEL_IN: f32 = crate::ui::DIALOG_PANEL_IN;

impl Picker {
    /// Put a question on screen, or say that it cannot be.
    ///
    /// `None` where there is nowhere to start — a machine with no home
    /// directory the shell can find and no root it can read, which is a session
    /// that has bigger problems, but is still an answer rather than a panel
    /// with an empty column in it. See [`crate::model::Cursor::enter`], where
    /// the rule that an empty column cannot be stood in comes from.
    pub fn open(asked: Asked, how: files::How) -> Option<Self> {
        let mut picker = Self {
            kind: (!asked.kinds.is_empty()).then_some(0),
            name: asked.name.clone(),
            asked,
            levels: Vec::new(),
            depth: 0,
            ticked: Vec::new(),
            typing: None,
            making: String::new(),
            how,
            depth_linear: 0.0,
            depth_speed: 0.0,
            open: true,
            arrival: 0.0,
        };
        picker.levels.push(picker.disks());
        if picker.levels[0].rows.is_empty() {
            return None;
        }
        // Where the application asked to open, if it asked and the shell can
        // get there. A folder it cannot reach is simply not walked to — the
        // panel opens on the disks, which is where it would have opened anyway.
        if let Some(at) = picker.asked.at.clone() {
            picker.walk_to(&at);
        }
        picker.settle();
        Some(picker)
    }

    /// The question's own number, for the answer to quote back.
    pub fn id(&self) -> u32 {
        self.asked.id
    }

    pub fn purpose(&self) -> For {
        self.asked.purpose
    }

    /// Whether there is anything for Start — or Enter — to approve.
    ///
    /// Every question but "one file", where the file *is* the answer and the
    /// press on it is the whole act. A legend offering Approve there would name
    /// a button that does nothing, which is worse than naming no button.
    pub fn can_be_approved(&self) -> bool {
        self.asked.purpose.accept().is_some()
    }

    /// End the question from wherever the cursor is standing, as the row that
    /// answers would if it were pressed.
    ///
    /// The row is still there and still says what it will do; this is the same
    /// act reached by a button instead of by walking to it, which is what makes
    /// a save two presses rather than a walk back up the column every time.
    pub fn approve(&self) -> Act {
        self.answer()
    }

    /// The line across the top of the panel: who is asking, and for what.
    ///
    /// The application's own title where it gave one, because an application
    /// that called its dialog "Choose a picture to upload" has said something
    /// this shell cannot work out for itself. Its name and the purpose
    /// otherwise — and "An application" where it did not even say that, rather
    /// than a sentence beginning with a blank.
    pub fn heading(&self) -> String {
        let named = match self.asked.app_id.trim() {
            "" => "An application".to_string(),
            // The reverse-DNS spelling is written for a machine to match on.
            // What is left after the last dot is the part anybody recognises.
            named => crate::app_id_name(named).unwrap_or_else(|| named.to_string()),
        };
        match self.asked.title.trim() {
            "" => format!("{named} {}", self.asked.purpose.asking()),
            title => format!("{named}: {title}"),
        }
    }

    /// The kinds the application offered, in the order it offered them.
    pub fn kinds(&self) -> &[Kind] {
        &self.asked.kinds
    }

    /// Which kind is in force, or nothing for the whole disk.
    pub fn kind(&self) -> Option<usize> {
        self.kind
    }

    /// What the line at the bottom says the panel is showing.
    pub fn showing(&self) -> &str {
        match self.kind.and_then(|index| self.asked.kinds.get(index)) {
            Some(kind) => &kind.name,
            None => "Everything",
        }
    }

    /// Look at a different kind of file. The columns are read again, because
    /// which rows there are is what a kind decides.
    pub fn show_kind(&mut self, kind: Option<usize>) -> bool {
        let kind = kind.filter(|index| *index < self.asked.kinds.len());
        if kind == self.kind {
            return false;
        }
        self.kind = kind;
        self.reread();
        true
    }

    /// Read the folders again, whatever changed: a kind, the sort order,
    /// whether the dotfiles are in, or a folder that has just been made.
    pub fn read_again(&mut self, how: files::How) {
        self.how = how;
        self.reread();
    }

    /// What the file being saved is called so far.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// What has been typed into the field of the column being stood in.
    pub fn query(&self) -> &str {
        &self.levels[self.depth].query
    }

    /// Narrow the column being stood in. The folder is read again, which is
    /// what a search of a directory *is* — see [`crate::apps::Searched`].
    pub fn search(&mut self, query: String) {
        if self.levels[self.depth].query == query {
            return;
        }
        self.levels[self.depth].query = query;
        let level = self.depth;
        self.read(level);
        self.settle();
    }

    /// Which of the nine orders the column being stood in could be put in, for
    /// the Sort panel to grey the rest.
    pub fn orders(&self) -> crate::media::Orders {
        self.levels[self.depth].orders
    }

    /// The folder being stood in, which is where a new one would be made and
    /// where a save would go.
    pub fn here(&self) -> Option<&Path> {
        self.levels[self.depth].at.as_deref()
    }

    /// Where the walk is, written out for the foot of the panel.
    ///
    /// The **whole path**, not a shortened one and not `~`: what an application
    /// is handed is a path, and a chooser that showed a folder's name alone has
    /// told the user nothing that tells two folders called `Downloads` apart.
    /// The first column is the one place there is no path — it is the disks
    /// there are to look in, not a folder on any of them — so it says what it
    /// is called instead, which is the same words its own heading uses.
    pub fn location(&self) -> String {
        match self.here() {
            Some(at) => at.to_string_lossy().into_owned(),
            None => self.levels[self.depth].title.clone(),
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Whether it is drawn at all: open, or still folding away.
    pub fn is_on_screen(&self) -> bool {
        self.open || self.arrival > 0.0
    }

    pub fn is_animating(&self) -> bool {
        let arriving = if self.open {
            self.arrival < 1.0
        } else {
            self.arrival > 0.0
        };
        arriving || self.columns_are_moving()
    }

    /// How much of the way in it is, eased.
    pub fn arrived(&self) -> f32 {
        crate::ui::ease(self.arrival)
    }

    /// Start folding away. The question is not answered by this — the shell
    /// answers it, and it answers it at once rather than when the panel has
    /// finished leaving, because an application waiting on a file should not
    /// wait on an animation.
    pub fn close(&mut self) -> bool {
        if !self.open {
            return false;
        }
        self.open = false;
        true
    }

    pub fn animate(&mut self, dt: f32) -> bool {
        let step = dt / PANEL_IN;
        let was = self.arrival;
        self.arrival = if self.open {
            (self.arrival + step).min(1.0)
        } else {
            (self.arrival - step).max(0.0)
        };
        let mut moved = self.arrival != was;
        // And the two springs the columns ride, which are the bar's own: the
        // list sliding under the cross, and the trail sliding sideways as the
        // walk goes in and out. Critically damped, so a second press part-way
        // carries the first one's momentum on rather than starting again from
        // rest — [`crate::transfer::Transfer::animate`]'s own arithmetic, at
        // the same two rates.
        let spring = |at: f32, speed: f32, target: f32, rate: f32| {
            let (at, speed) = lxb_protocol::overview::spring(
                at as f64,
                speed as f64,
                target as f64,
                rate as f64,
                dt as f64,
            );
            (at as f32, speed as f32)
        };
        (self.depth_linear, self.depth_speed) = spring(
            self.depth_linear,
            self.depth_speed,
            self.depth as f32,
            DEPTH_EASE_RATE,
        );
        for level in &mut self.levels {
            let target = level.selected as f32;
            (level.position, level.speed) = spring(level.position, level.speed, target, EASE_RATE);
            if (target - level.position).abs() <= SETTLED && level.speed.abs() <= SETTLED {
                level.position = target;
                level.speed = 0.0;
            }
        }
        if !self.columns_are_moving() {
            self.depth_linear = self.depth as f32;
            self.depth_speed = 0.0;
        }
        moved |= self.columns_are_moving();
        moved
    }

    /// Whether any of those springs is still travelling, which is what says the
    /// panel needs another frame.
    fn columns_are_moving(&self) -> bool {
        if (self.depth as f32 - self.depth_linear).abs() > SETTLED
            || self.depth_speed.abs() > SETTLED
        {
            return true;
        }
        self.levels.iter().any(|level| {
            level.speed.abs() > SETTLED || (level.selected as f32 - level.position).abs() > SETTLED
        })
    }

    // -- what is being typed --------------------------------------------------

    /// Put the caret in one of the panel's fields.
    ///
    /// A folder being named starts from nothing every time, because there is no
    /// earlier name for it to start from — unlike the other two, which are
    /// carrying on with what is already there.
    pub fn type_into(&mut self, field: Field) {
        if field == Field::NewFolder {
            self.making.clear();
        }
        self.typing = Some(field);
    }

    pub fn typing(&self) -> Option<Field> {
        self.typing
    }

    /// Take the caret out, leaving whatever was typed where it was typed.
    pub fn stop_typing(&mut self) -> bool {
        self.typing.take().is_some()
    }

    /// What is in the field being typed into.
    pub fn typed(&self) -> &str {
        match self.typing {
            Some(Field::Search) => self.query(),
            Some(Field::Name) => &self.name,
            Some(Field::NewFolder) => &self.making,
            None => "",
        }
    }

    /// Put what has been typed into the field the caret is in.
    ///
    /// Narrowing the column is the one of the three that costs anything: it is
    /// a `readdir`, on the frame the letter was pressed, which is what a search
    /// of a directory *is* — see [`crate::apps::Searched`].
    pub fn set_typed(&mut self, text: String) {
        match self.typing {
            Some(Field::Search) => self.search(text),
            Some(Field::Name) => self.name = text,
            Some(Field::NewFolder) => self.making = text,
            None => {}
        }
    }

    /// The name of the folder being made, as it stands.
    pub fn making(&self) -> &str {
        &self.making
    }

    // -- walking ------------------------------------------------------------

    /// Move the cursor within the column it is in.
    pub fn move_selection(&mut self, delta: i32) -> bool {
        let level = &mut self.levels[self.depth];
        if level.rows.is_empty() {
            return false;
        }
        let last = level.rows.len() as i32 - 1;
        let wanted = (level.selected as i32 + delta).clamp(0, last) as usize;
        if wanted == level.selected {
            return false;
        }
        level.selected = wanted;
        self.settle();
        true
    }

    /// Put the cursor on one row of the column being stood in, counted from the
    /// top of the whole column.
    pub fn point_at(&mut self, row: usize) -> bool {
        let level = &mut self.levels[self.depth];
        if row >= level.rows.len() || row == level.selected {
            return false;
        }
        level.selected = row;
        self.settle();
        true
    }

    /// Step into the folder under the cursor, opening a column beside this one.
    ///
    /// The columns past this one are dropped, because the trail is the path the
    /// user is standing in and not the whole of where they have been — the same
    /// rule Files keeps; see [`crate::model::Cursor::open_place`]. A folder
    /// stepped back out of and into again is therefore read afresh, which is
    /// also what Files does and what makes the column agree with the disk.
    pub fn step_in(&mut self) -> bool {
        let Some(Row::Folder { path, .. }) = self.row().cloned() else {
            return false;
        };
        // The one shape the bar cannot show: a column with nothing in it. It is
        // refused here rather than opened, so the cursor is never stranded
        // somewhere it cannot be moved out of. A folder that lists as empty
        // still has its head rows, so this only bites where there are none at
        // all — a folder nothing can be made in, holding nothing this question
        // will take.
        let level = self.build(Some(&path));
        if level.rows.is_empty() {
            return false;
        }
        self.levels.truncate(self.depth + 1);
        self.levels.push(level);
        self.depth += 1;
        self.settle();
        true
    }

    /// Step back out to the column behind this one. False at the disks, which
    /// is as far out as there is to go — the way out of the panel is Back, not
    /// another Left.
    pub fn step_out(&mut self) -> bool {
        if self.depth == 0 {
            return false;
        }
        self.depth -= 1;
        self.settle();
        true
    }

    /// Take the press on the row under the cursor.
    pub fn press(&mut self) -> Act {
        let Some(row) = self.row().cloned() else {
            return Act::Nothing;
        };
        match row {
            Row::Search => Act::Search,
            Row::Clear => {
                self.search(String::new());
                Act::Nothing
            }
            Row::Name => Act::Name,
            Row::NewFolder => match self.here() {
                Some(at) => Act::MakeFolder(at.to_path_buf()),
                None => Act::Nothing,
            },
            Row::Answer => self.answer(),
            Row::Folder { .. } => {
                self.step_in();
                Act::Nothing
            }
            Row::File { path, name, .. } => match self.asked.purpose {
                // The file *is* the answer, and there is nothing else to press.
                For::OneFile => Act::Answer(vec![path]),
                // A tick, because the answer is a set and this row is one of
                // it. The row that ends the question is at the top of the
                // column.
                For::ManyFiles => {
                    self.tick(&path);
                    Act::Nothing
                }
                // Saving over something that is already there: pressing it puts
                // its name in the field, which is what every other desktop's
                // chooser does and what somebody pressing a file in a Save
                // panel means. It does not save — that is still the head row,
                // and a press that overwrote a file without a second one would
                // be a file destroyed by a mis-aimed thumbstick.
                For::ANewFile => {
                    self.name = name;
                    Act::Nothing
                }
                // Not listed at all, so not reachable.
                For::AFolder => Act::Nothing,
            },
        }
    }

    /// What the row that ends the question answers with, pressed in the column
    /// being stood in.
    fn answer(&self) -> Act {
        self.answer_in(self.depth)
    }

    /// The same, asked of any column of the trail — which is how the row of a
    /// column further out knows what it would say if it were pressed.
    fn answer_in(&self, level: usize) -> Act {
        match self.asked.purpose {
            For::OneFile => Act::Nothing,
            For::ManyFiles => {
                if self.ticked.is_empty() {
                    return Act::Blocked("Nothing has been chosen yet");
                }
                Act::Answer(self.ticked.clone())
            }
            For::AFolder => match self.levels[level].at.as_deref() {
                Some(at) => Act::Answer(vec![at.to_path_buf()]),
                // The disks are not a folder anybody can be given.
                None => Act::Blocked("Step into one of these first"),
            },
            For::ANewFile => {
                let Some(at) = self.levels[level].at.as_deref() else {
                    return Act::Blocked("Step into a folder first");
                };
                let name = self.name.trim();
                if name.is_empty() {
                    return Act::Blocked("It needs a name first");
                }
                // A name with a separator in it is a path, and a path is
                // somewhere other than the folder the user walked to. The two
                // that are not names at all go the same way.
                if name.contains('/') || name == "." || name == ".." {
                    return Act::Blocked("That is not a name");
                }
                Act::Answer(vec![at.join(name)])
            }
        }
    }

    /// Tick a file, or take the tick off it.
    fn tick(&mut self, path: &Path) {
        match self.ticked.iter().position(|held| held == path) {
            Some(index) => {
                self.ticked.remove(index);
            }
            // In order, so the application is handed the same set however it
            // was gathered — the argument [`crate::marks`] makes for keying its
            // own by path.
            None => match self
                .ticked
                .binary_search_by(|held| held.as_path().cmp(path))
            {
                Ok(_) => {}
                Err(index) => self.ticked.insert(index, path.to_path_buf()),
            },
        }
    }

    /// How many files have been ticked.
    pub fn ticked(&self) -> usize {
        self.ticked.len()
    }

    // -- what is on screen --------------------------------------------------

    /// The columns to draw, deepest last, at most [`COLUMNS`] of them.
    ///
    /// The column being stood in is always the last one shown, so the trail
    /// runs off the left of the panel as the walk gets deeper — the bar's own
    /// arrangement, where the columns behind are the ones already stepped
    /// through.
    ///
    /// Metadata only. The *rows* are fetched one at a time through
    /// [`Self::row_face`], because a column can hold ten thousand of them and
    /// only the handful the panel is tall enough to show are ever drawn — the
    /// bar's own bound; see [`crate::ui`]'s `rows_in_view`.
    pub fn columns(&self) -> Vec<Column> {
        let last = self.depth;
        let first = last.saturating_sub(COLUMNS - 1);
        (first..=last)
            .map(|level| {
                let column = &self.levels[level];
                Column {
                    level,
                    rows: column.rows.len(),
                    selected: column.selected,
                    position: column.position,
                    title: column.title.clone(),
                }
            })
            .collect()
    }

    /// The files on screen that are worth making a picture of: the rows around
    /// each visible column's cursor, and only the kinds a picture can be made
    /// of at all.
    ///
    /// The shell asks this once a frame and hands it to the thumbnail worker
    /// alongside the bar's own rows — see `Shell::rows_worth_having`. Bounded
    /// the same way that is, and for the same reason: a column can hold ten
    /// thousand files, and a folder of photographs decoded end to end because
    /// somebody scrolled past it is the one way this could cost anything.
    ///
    /// The columns behind are in, not only the one being stood in. They are on
    /// screen, and a photograph that lost its picture the moment it was stepped
    /// past would be a trail that says less about where the user has been than
    /// the column they came from did.
    pub fn pictures_worth_having(&self) -> Vec<PathBuf> {
        /// How many rows either side of the cursor are worth having ready.
        /// `Shell::rows_worth_having`'s own number.
        const REACH: usize = 4;

        let mut wanted = Vec::new();
        for column in self.columns() {
            let level = &self.levels[column.level];
            let from = level.selected.saturating_sub(REACH);
            let to = (level.selected + REACH + 1).min(level.rows.len());
            for row in &level.rows[from..to] {
                if let Row::File { path, mime, .. } = row {
                    if crate::media::has_picture(mime) {
                        wanted.push(path.clone());
                    }
                }
            }
        }
        wanted
    }

    /// How far along the trail the columns are drawn, eased.
    pub fn depth(&self) -> f32 {
        self.depth_linear
    }

    /// Which column the walk is *in* — where [`Self::depth`] is on its way to,
    /// rather than where it has got to.
    ///
    /// What the drawing measures a name's room by, and it has to be this one:
    /// how far a name may run is a question about the panel the walk has landed
    /// on, and asking it of a trail still sliding gave every name on the column
    /// being stepped back into the room of a column it had already left. They
    /// were drawn cut short, with the ellipsis of a name too long to fit, and
    /// then sprang out whole at the end of the slide. Nothing about a name
    /// changes while a column travels, so nothing about its room may either.
    pub fn standing(&self) -> usize {
        self.depth
    }

    /// One row of one column, written out — or nothing where the column has
    /// been read again under the drawing and is shorter than it was.
    pub fn row_face(&self, level: usize, index: usize) -> Option<Face> {
        Some(self.face(self.levels.get(level)?.rows.get(index)?, level))
    }

    /// One row of column `level`, written out.
    ///
    /// The column has to be named, not assumed to be the one being stood in:
    /// every column of the trail carries a field of its own and a row that
    /// answers of its own, and a face built from the standing column's state
    /// would print that column's search text — and its caret — across the whole
    /// trail. It did, and the picture showed two fields being typed into at
    /// once.
    fn face(&self, row: &Row, level: usize) -> Face {
        match row {
            Row::NewFolder => Face {
                title: match self.typing_in(level) {
                    Some(Field::NewFolder) => format!("{}|", self.making),
                    _ => "New folder".to_string(),
                },
                note: Some(match self.typing_in(level) {
                    Some(Field::NewFolder) => "What to call it".to_string(),
                    _ => "Make one here".to_string(),
                }),
                glyph: crate::icons::NEW_FOLDER,
                quiet: false,
                ticked: false,
                preview: None,
            },
            Row::Name => Face {
                title: match (self.typing_in(level), self.name.trim()) {
                    // The caret, drawn as the bar's own fields draw one: what
                    // has been typed so far with a bar after it, rather than a
                    // separate control that appears over the row.
                    (Some(Field::Name), name) => format!("{name}|"),
                    (_, "") => "Untitled".to_string(),
                    (_, name) => name.to_string(),
                },
                note: Some("What to call it".to_string()),
                glyph: crate::icons::RENAME,
                quiet: self.typing_in(level) != Some(Field::Name) && self.name.trim().is_empty(),
                ticked: false,
                preview: None,
            },
            Row::Answer => {
                let answer = self.answer_in(level);
                let title = match self.asked.accept.trim() {
                    "" => self.asked.purpose.accept().unwrap_or("Choose").to_string(),
                    // The application's own word, which is the one thing on
                    // this panel it gets to write. It is drawn and nothing
                    // more: it names no command and reaches nothing.
                    given => given.to_string(),
                };
                let blocked = matches!(answer, Act::Blocked(_) | Act::Nothing);
                Face {
                    title,
                    note: Some(match answer {
                        Act::Blocked(why) => why.to_string(),
                        _ => self.answering(level),
                    }),
                    glyph: crate::icons::CHOSEN,
                    // A row that cannot be taken keeps less of the ink, which
                    // is what says "not from here" — the picker's own rule for
                    // a blocked Paste; see [`crate::transfer`].
                    quiet: blocked,
                    ticked: false,
                    preview: None,
                }
            }
            Row::Search => Face {
                title: match (self.typing_in(level), self.levels[level].query.as_str()) {
                    (Some(Field::Search), query) => format!("{query}|"),
                    (_, "") => "Search".to_string(),
                    (_, query) => query.to_string(),
                },
                note: Some(self.searched(level)),
                glyph: crate::icons::SEARCH,
                quiet: false,
                ticked: false,
                preview: None,
            },
            Row::Clear => Face {
                title: "Clear".to_string(),
                note: Some("Show everything again".to_string()),
                glyph: crate::icons::SEARCH_CLEAR,
                quiet: false,
                ticked: false,
                preview: None,
            },
            Row::Folder { name, note, .. } => Face {
                title: name.clone(),
                note: note.clone(),
                glyph: crate::icons::FILE_FOLDER,
                quiet: false,
                ticked: false,
                preview: None,
            },
            Row::File {
                name,
                note,
                path,
                mime,
                glyph,
            } => Face {
                title: name.clone(),
                note: Some(note.clone()),
                glyph,
                // A file is drawn quieter than a folder for the reason the
                // transfer picker draws one quieter: the rows that lead
                // somewhere are the shape of the thing. Except where it is the
                // answer, which is every purpose but choosing a folder — and a
                // folder walk does not list files at all.
                quiet: false,
                ticked: self.ticked.iter().any(|held| held == path),
                // Only the kinds a picture can be made of: the rest of a
                // folder is documents and archives, and a row that asked for
                // one would put a worker to work finding nothing slowly. The
                // explorer's own test, so a photograph met here is previewed on
                // exactly the terms it is previewed on in Files.
                preview: crate::media::has_picture(mime).then(|| path.clone()),
            },
        }
    }

    /// Which field the caret is in, as far as column `level` is concerned.
    ///
    /// Nothing at all for any column but the one being stood in: there is one
    /// keyboard and it is going into one row, and a trail whose every column
    /// drew a caret would be a panel claiming to be typed into three times
    /// over. It did, and the picture showed exactly that.
    fn typing_in(&self, level: usize) -> Option<Field> {
        (level == self.depth).then_some(self.typing).flatten()
    }

    /// What the row that ends column `level` says under itself.
    fn answering(&self, level: usize) -> String {
        match self.asked.purpose {
            For::ManyFiles => match self.ticked() {
                1 => "1 file chosen".to_string(),
                many => format!("{many} files chosen"),
            },
            For::AFolder | For::ANewFile => {
                match self.levels[level].at.as_deref().and_then(Path::file_name) {
                    Some(name) => format!("In {}", name.to_string_lossy()),
                    None => "In this folder".to_string(),
                }
            }
            For::OneFile => String::new(),
        }
    }

    /// The line under column `level`'s search field: what has been found, out
    /// of what there is.
    fn searched(&self, level: usize) -> String {
        let level = &self.levels[level];
        let listed = level.rows.iter().filter(|row| row.path().is_some()).count();
        match level.query.is_empty() {
            true => "Narrow this folder".to_string(),
            false => format!("{listed} found"),
        }
    }

    /// The row the cursor is on.
    fn row(&self) -> Option<&Row> {
        let level = &self.levels[self.depth];
        level.rows.get(level.selected)
    }

    // -- reading the disk ---------------------------------------------------

    /// The first column: the user's own folder, the machine, and anything
    /// plugged in.
    ///
    /// [`files::volumes`] built with the disk being browsed, and then the trash
    /// taken off it — which is the one row of that list this panel must not
    /// offer. See this module's own head for why, and note that it is dropped
    /// here rather than asked for with a narrower [`Shows`]: the narrower ones
    /// would take the New folder row away with it.
    fn disks(&self) -> Level {
        let found = files::volumes("", Shows::Everything);
        let rows = found
            .rows
            .into_iter()
            .filter(|entry| !matches!(entry_place(entry), Some(Place::Trash)))
            .filter_map(|entry| self.row_of(entry))
            .collect();
        Level {
            at: None,
            rows,
            selected: 0,
            position: 0.0,
            speed: 0.0,
            query: String::new(),
            orders: found.orders,
            title: "This machine".to_string(),
        }
    }

    /// Which row a column opens on: the row that saves where something is being
    /// saved, and the first of the listing everywhere else — or the last of the
    /// head rows where there is no listing at all.
    ///
    /// **Saving is the exception, and it is the user's own.** Walking into a
    /// folder in order to write into it is a walk whose whole point is the
    /// folder that has just been reached, so that is what the column rests on:
    /// Save here, with the name already under it. The rest of this shell keeps
    /// [`crate::transfer`]'s rule — never open on the row that acts, because a
    /// press of Accept out of habit would hand an application something nobody
    /// chose — and it keeps it here for the other three questions, where what is
    /// being chosen really is a row of the listing and the folder is only the
    /// way to it. A save is the one question where the folder *is* the answer
    /// and pressing it again cannot lose anything: what it does is write a file
    /// under a name the user can read on the row above.
    ///
    /// With one step back: a save with nothing to call it yet rests on the name
    /// instead, because that is the thing standing between the user and the
    /// answer, and the row that answers would only be able to say so.
    fn opens_on(&self, rows: &[Row]) -> usize {
        if self.asked.purpose == For::ANewFile {
            if let Some(answers) = rows.iter().position(|row| *row == Row::Answer) {
                let named = rows.iter().position(|row| *row == Row::Name);
                return match self.name.trim().is_empty() {
                    true => named.unwrap_or(answers),
                    false => answers,
                };
            }
        }
        rows.iter()
            .position(|row| row.path().is_some())
            .unwrap_or_else(|| rows.len().saturating_sub(1))
    }

    /// One column of the walk: the disks, or one folder.
    fn build(&self, at: Option<&Path>) -> Level {
        let Some(at) = at else {
            return self.disks();
        };
        let query = String::new();
        let found = files::listing(at, &query, self.how, Shows::Everything);
        let can_make = found
            .rows
            .iter()
            .any(|entry| matches!(entry, Entry::Make(_)));
        let orders = found.orders;
        let mut rows = self.head_rows(can_make);
        rows.extend(
            found
                .rows
                .into_iter()
                .filter_map(|entry| self.row_of(entry)),
        );
        let selected = self.opens_on(&rows);
        Level {
            selected,
            position: selected as f32,
            speed: 0.0,
            at: Some(at.to_path_buf()),
            rows,
            query,
            orders,
            title: at
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| at.to_string_lossy().into_owned()),
        }
    }

    /// The rows that stand over a folder's listing.
    ///
    /// The order is [`crate::apps::place_rows`]' own and it is the same order
    /// for the same reason: New folder above everything, because it is the row
    /// that is not about the list at all, and the field directly over the files,
    /// so Up from the first one still lands on the search it lands on
    /// everywhere else in this shell. The two between them are this panel's,
    /// and they go where Paste goes — over the field, because they are what
    /// ends the question rather than what narrows the list.
    fn head_rows(&self, can_make: bool) -> Vec<Row> {
        let mut rows = Vec::with_capacity(5);
        if can_make && self.asked.purpose.makes_folders() {
            rows.push(Row::NewFolder);
        }
        if self.asked.purpose == For::ANewFile {
            rows.push(Row::Name);
        }
        if self.asked.purpose.accept().is_some() {
            rows.push(Row::Answer);
        }
        rows.push(Row::Search);
        if !self.levels.is_empty() && !self.levels[self.depth].query.is_empty() {
            rows.push(Row::Clear);
        }
        rows
    }

    /// One of the explorer's own rows, as one of this panel's — or nothing,
    /// where it is a row this panel does not carry.
    ///
    /// The field and the New folder row that [`files::listing`] puts at the
    /// head of its own columns are among the nothings: this panel builds those
    /// itself, because where they go depends on which of the four questions is
    /// being asked and that is not something the explorer knows.
    fn row_of(&self, entry: Entry) -> Option<Row> {
        match entry {
            Entry::Folder(folder) => {
                let path = match folder.place {
                    Some(Place::Directory(path, _)) => path,
                    // The trash is dropped above, and nothing else in a
                    // listing carries any other kind of place.
                    _ => return None,
                };
                Some(Row::Folder {
                    name: folder.title,
                    note: folder.comment,
                    path,
                })
            }
            Entry::File(item) => {
                if !self.asked.purpose.lists_files() {
                    return None;
                }
                if !self.wants(&item.name, item.mime) {
                    return None;
                }
                Some(Row::File {
                    name: item.name,
                    note: item.note,
                    path: item.path,
                    mime: item.mime,
                    glyph: item.glyph,
                })
            }
            _ => None,
        }
    }

    /// Whether one file is of the kind in force. Everything is, where none is.
    fn wants(&self, name: &str, mime: &str) -> bool {
        match self.kind.and_then(|index| self.asked.kinds.get(index)) {
            Some(kind) => kind.matches(name, mime),
            None => true,
        }
    }

    /// Read every column again, keeping where the cursor is standing.
    ///
    /// By path rather than by index, because the listing is what has changed: a
    /// row that was third can be tenth after the dotfiles are switched on, and
    /// a cursor put back on the third row would be a cursor that moved because
    /// the user asked to see more.
    fn reread(&mut self) {
        for level in 0..self.levels.len() {
            self.read(level);
        }
        self.settle();
    }

    /// One column again, in place.
    fn read(&mut self, level: usize) {
        let standing = self.levels[level]
            .rows
            .get(self.levels[level].selected)
            .and_then(|row| row.path())
            .map(Path::to_path_buf);
        let at = self.levels[level].at.clone();
        let query = self.levels[level].query.clone();
        let mut built = match at.as_deref() {
            Some(at) => {
                let found = files::listing(at, &query, self.how, Shows::Everything);
                let can_make = found
                    .rows
                    .iter()
                    .any(|entry| matches!(entry, Entry::Make(_)));
                let orders = found.orders;
                let mut rows = self.head_rows(can_make);
                rows.extend(
                    found
                        .rows
                        .into_iter()
                        .filter_map(|entry| self.row_of(entry)),
                );
                Level {
                    at: at.to_path_buf().into(),
                    rows,
                    selected: 0,
                    position: 0.0,
                    speed: 0.0,
                    query: query.clone(),
                    orders,
                    title: self.levels[level].title.clone(),
                }
            }
            None => self.disks(),
        };
        built.query = query;
        // Where the cursor was, if that row is still there. The head row it
        // falls back to is the one every column has.
        built.selected = standing
            .and_then(|was| built.rows.iter().position(|row| row.path() == Some(&was)))
            .unwrap_or_else(|| {
                self.levels[level]
                    .selected
                    .min(built.rows.len().saturating_sub(1))
            });
        self.levels[level] = built;
    }

    /// Open the walk on a folder the application named, as far down it as the
    /// shell can actually get.
    ///
    /// Each step is the ordinary one — build the column, find the row that
    /// leads on, step into it — so a path that runs through somewhere
    /// unreadable simply stops there, with the columns that did open standing.
    /// Nothing is invented: what the user sees is a walk the shell could have
    /// made itself.
    fn walk_to(&mut self, at: &Path) {
        // Which of the disks this path is under, longest first: `$HOME` is
        // inside `/`, and a path in the user's own folder should open under
        // Home rather than four columns down from Root.
        let mut best: Option<(usize, PathBuf)> = None;
        for (index, row) in self.levels[0].rows.iter().enumerate() {
            let Some(root) = row.path() else { continue };
            if !at.starts_with(root) {
                continue;
            }
            let deeper = best
                .as_ref()
                .is_none_or(|(_, held)| root.components().count() > held.components().count());
            if deeper {
                best = Some((index, root.to_path_buf()));
            }
        }
        let Some((row, root)) = best else { return };
        self.levels[0].selected = row;
        if !self.step_in() {
            return;
        }
        let Ok(rest) = at.strip_prefix(&root) else {
            return;
        };
        let mut walked = root;
        for part in rest.components() {
            walked = walked.join(part);
            let level = &mut self.levels[self.depth];
            let Some(index) = level
                .rows
                .iter()
                .position(|row| row.path() == Some(walked.as_path()))
            else {
                return;
            };
            level.selected = index;
            if !self.step_in() {
                return;
            }
        }
    }

    /// Keep every column's chosen row inside the rows it actually has, after a
    /// listing has been read again under it.
    fn settle(&mut self) {
        for level in &mut self.levels {
            level.selected = level.selected.min(level.rows.len().saturating_sub(1));
        }
    }
}

/// How fast the panel's columns settle.
///
/// The bar's own two rates and the folder picker's, and deliberately the same
/// numbers rather than numbers of this panel's own: a person answering an
/// application's file question has been walking the bar's columns all session,
/// and a column walked here must not answer at a different speed because it is
/// being walked for somebody else.
const EASE_RATE: f32 = 19.0;
const DEPTH_EASE_RATE: f32 = 14.0;
const SETTLED: f32 = 0.001;

/// The place one of the explorer's rows leads to, if it leads anywhere.
fn entry_place(entry: &Entry) -> Option<&Place> {
    match entry {
        Entry::Folder(folder) => folder.place.as_ref(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asked(purpose: For) -> Asked {
        Asked {
            id: 1,
            app_id: "org.example.Thing".to_string(),
            purpose,
            title: String::new(),
            accept: String::new(),
            name: String::new(),
            at: None,
            kinds: Vec::new(),
        }
    }

    #[test]
    fn a_glob_walks_the_name_without_running_away() {
        assert!(globbed("*.png", "a.png"));
        assert!(globbed("*.png", ".png"));
        assert!(!globbed("*.png", "a.png.txt"));
        assert!(globbed("*.tar.*", "everything.tar.gz"));
        assert!(globbed("?.txt", "a.txt"));
        assert!(!globbed("?.txt", "ab.txt"));
        assert!(globbed("*", "anything at all"));
        // The pathological pattern a recursive matcher hangs on.
        assert!(!globbed(
            "*a*a*a*a*a*a*a*a*b",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        ));
    }

    /// Every pattern below was read off the session bus while a real Firefox
    /// opened a real upload — the one this shell answered with an empty column.
    #[test]
    fn firefoxs_own_filters_are_bracket_expressions_and_they_match() {
        // `*.[pP][nN][gG]` is how a case-insensitive `*.png` is spelled to a
        // matcher that has no other way of asking, and it is what arrives here.
        let images = Kind {
            name: "Image Files".to_string(),
            patterns: [
                "*.[jJ][pP][gG]",
                "*.[pP][nN][gG]",
                "*.[gG][iI][fF]",
                "*.[aA][vV][iI][fF]",
            ]
            .into_iter()
            .map(|pattern| Pattern::Glob(pattern.to_string()))
            .collect(),
        };
        assert!(images.matches("Screenshot_20260622_021031.png", "image/png"));
        assert!(images.matches("IMG_0001.JPG", "image/jpeg"));
        assert!(images.matches("3x.gif", "image/gif"));
        assert!(images.matches("4x.avif", "image/avif"));
        assert!(!images.matches("notes.txt", "text/plain"));
        // And the other kind that came with it, which is the way out of any
        // filter at all: one star.
        let everything = Kind {
            name: "All Files".to_string(),
            patterns: vec![Pattern::Glob("*".to_string())],
        };
        assert!(everything.matches("notes.txt", "text/plain"));
    }

    #[test]
    fn a_bracket_is_one_character_out_of_a_set() {
        assert!(globbed("[abc].txt", "b.txt"));
        assert!(!globbed("[abc].txt", "d.txt"));
        // Ranges, and the two ways of turning a set inside out.
        assert!(globbed("track-[0-9].flac", "track-7.flac"));
        assert!(!globbed("track-[0-9].flac", "track-x.flac"));
        assert!(globbed("[!x]y", "ay"));
        assert!(!globbed("[!x]y", "xy"));
        assert!(globbed("[^x]y", "ay"));
        // POSIX's three corners: a `]` first is that character, a trailing `-`
        // is that character, and a bracket never closed is a bracket.
        assert!(globbed("[]a]b", "]b"));
        assert!(globbed("[]a]b", "ab"));
        assert!(globbed("[a-]b", "-b"));
        assert!(globbed("[abc", "[abc"));
        assert!(!globbed("[abc", "a"));
        // One character and exactly one, so a set never swallows a run.
        assert!(!globbed("[ab]", "ab"));
        // And it backtracks through a star like every other element does.
        assert!(globbed("*.[pP][nN][gG]", "a.b.png"));
    }

    #[test]
    fn a_kind_matches_by_name_or_by_what_the_file_is() {
        let kind = Kind {
            name: "Images".to_string(),
            patterns: vec![
                Pattern::Glob("*.png".to_string()),
                Pattern::Mime("image/*".to_string()),
            ],
        };
        assert!(kind.matches("a.png", "application/octet-stream"));
        // Case-folded: a camera writes JPG and a filter is written jpg.
        assert!(kind.matches("A.PNG", "application/octet-stream"));
        assert!(kind.matches("photo.jpeg", "image/jpeg"));
        assert!(!kind.matches("notes.txt", "text/plain"));
    }

    #[test]
    fn patterns_gather_into_one_kind_per_name_in_the_order_they_came() {
        let mut kinds = Vec::new();
        Kind::gather(&mut kinds, "Images".into(), Pattern::Glob("*.png".into()));
        Kind::gather(&mut kinds, "Films".into(), Pattern::Glob("*.mkv".into()));
        Kind::gather(&mut kinds, "Images".into(), Pattern::Glob("*.jpg".into()));
        assert_eq!(kinds.len(), 2);
        assert_eq!(kinds[0].name, "Images");
        assert_eq!(kinds[0].patterns.len(), 2);
        assert_eq!(kinds[1].name, "Films");
    }

    #[test]
    fn the_head_rows_are_the_ones_the_question_needs() {
        let picker = Picker::open(asked(For::OneFile), files::How::plain()).unwrap();
        // Opening one file has no row that ends the question: the file is the
        // answer, and a second row saying so would be a row nobody presses.
        assert_eq!(picker.head_rows(true), vec![Row::Search]);

        let picker = Picker::open(asked(For::ANewFile), files::How::plain()).unwrap();
        assert_eq!(
            picker.head_rows(true),
            vec![Row::NewFolder, Row::Name, Row::Answer, Row::Search]
        );
        // And nowhere that cannot be written to.
        assert_eq!(
            picker.head_rows(false),
            vec![Row::Name, Row::Answer, Row::Search]
        );

        let picker = Picker::open(asked(For::AFolder), files::How::plain()).unwrap();
        assert_eq!(
            picker.head_rows(true),
            vec![Row::NewFolder, Row::Answer, Row::Search]
        );

        // Choosing several files makes no folders: the answer is a set of
        // things that are already there.
        let picker = Picker::open(asked(For::ManyFiles), files::How::plain()).unwrap();
        assert_eq!(picker.head_rows(true), vec![Row::Answer, Row::Search]);
    }

    #[test]
    fn the_trash_is_never_one_of_the_disks() {
        let picker = Picker::open(asked(For::OneFile), files::How::plain()).unwrap();
        let disks = picker.columns().pop().unwrap();
        let titles: Vec<String> = (0..disks.rows)
            .filter_map(|row| picker.row_face(disks.level, row))
            .map(|face| face.title)
            .collect();
        assert!(
            !titles.iter().any(|title| title == "Trash"),
            "a walk that is choosing something must not be offered the trash: {titles:?}"
        );
    }

    #[test]
    fn a_name_that_is_a_path_is_refused_rather_than_written() {
        let mut picker = Picker::open(asked(For::ANewFile), files::How::plain()).unwrap();
        picker.type_into(Field::Name);
        picker.set_typed("../../.bashrc".to_string());
        // Standing on the disks, so the folder is the first thing missing.
        assert!(matches!(picker.answer(), Act::Blocked(_)));
    }

    #[test]
    fn ticking_the_same_file_twice_takes_the_tick_off() {
        let mut picker = Picker::open(asked(For::ManyFiles), files::How::plain()).unwrap();
        let path = PathBuf::from("/tmp/a");
        picker.tick(&path);
        assert_eq!(picker.ticked(), 1);
        picker.tick(&path);
        assert_eq!(picker.ticked(), 0);
        assert!(matches!(picker.answer(), Act::Blocked(_)));
    }

    /// A scratch directory of this test's own, so nothing here reads the disk
    /// of whoever is running the suite.
    fn scratch(name: &str) -> Option<PathBuf> {
        let dir = std::env::temp_dir().join(format!("lxb-picker-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir)
    }

    /// A walk two columns deep into a folder of this test's own.
    fn walked(purpose: For, at: &Path) -> Option<Picker> {
        let mut asked = asked(purpose);
        asked.at = Some(at.to_path_buf());
        let picker = Picker::open(asked, files::How::plain())?;
        // Only where the temporary directory is somewhere the disks lead: on a
        // machine where it is not, there is nothing to assert about.
        (picker.columns().len() > 1).then_some(picker)
    }

    /// The caret and the search text belong to the column being stood in, and
    /// to no other. Every column of the trail carries a field of its own, and a
    /// face built from the standing column's state printed that column's text —
    /// and its caret — across the whole trail. The picture showed two fields
    /// being typed into at once.
    #[test]
    fn only_the_column_being_stood_in_is_being_typed_into() {
        let Some(dir) = scratch("caret") else { return };
        std::fs::create_dir(dir.join("inside")).unwrap();
        let Some(mut picker) = walked(For::OneFile, &dir) else {
            return;
        };
        picker.type_into(Field::Search);
        picker.set_typed("abc".to_string());

        let columns = picker.columns();
        let titles = |column: &Column| -> Vec<String> {
            (0..column.rows)
                .filter_map(|row| picker.row_face(column.level, row))
                .map(|face| face.title)
                .collect()
        };
        let (standing, behind) = columns.split_last().unwrap();
        assert!(
            titles(standing).iter().any(|title| title == "abc|"),
            "the column being stood in carries the caret"
        );
        for column in behind {
            let seen = titles(column);
            assert!(
                !seen.iter().any(|title| title.ends_with('|')),
                "no column behind it does: {seen:?}"
            );
            assert!(
                !seen.iter().any(|title| title == "abc"),
                "nor its text: {seen:?}"
            );
        }
    }

    /// Walking into a folder in order to save into it rests on the row that
    /// saves. The user asked for this outright: every other question opens on
    /// the first row of the listing, and a save that did so put three folders
    /// between somebody and the only thing they had come for.
    #[test]
    fn a_save_opens_on_the_row_that_saves() {
        let Some(dir) = scratch("save-here") else {
            return;
        };
        std::fs::create_dir(dir.join("inside")).unwrap();
        std::fs::write(dir.join("already.txt"), b"").unwrap();

        let mut saving = asked(For::ANewFile);
        saving.at = Some(dir.clone());
        saving.name = "page.html".to_string();
        let Some(picker) = Picker::open(saving, files::How::plain()) else {
            return;
        };
        if picker.columns().len() < 2 {
            return;
        }
        let face = picker
            .row_face(picker.standing(), picker.levels[picker.standing()].selected)
            .expect("the row it rests on");
        assert_eq!(face.title, "Save here");

        // With nothing to call it yet, the name is what stands between the user
        // and that row, so that is where it rests instead.
        let mut unnamed = asked(For::ANewFile);
        unnamed.at = Some(dir.clone());
        let Some(picker) = Picker::open(unnamed, files::How::plain()) else {
            return;
        };
        let face = picker
            .row_face(picker.standing(), picker.levels[picker.standing()].selected)
            .expect("the row it rests on");
        assert_eq!(face.note.as_deref(), Some("What to call it"));

        // And no other question moved: choosing a file still opens on the file.
        let Some(picker) = walked(For::OneFile, &dir) else {
            return;
        };
        let face = picker
            .row_face(picker.standing(), picker.levels[picker.standing()].selected)
            .expect("the row it rests on");
        assert_eq!(face.title, "inside");
    }

    #[test]
    fn the_heading_says_who_is_asking_even_when_nobody_said() {
        let mut nameless = asked(For::OneFile);
        nameless.app_id = String::new();
        let picker = Picker::open(nameless, files::How::plain()).unwrap();
        assert!(picker.heading().starts_with("An application"));

        let picker = Picker::open(asked(For::AFolder), files::How::plain()).unwrap();
        assert_eq!(picker.heading(), "Thing wants a folder");
    }
}
