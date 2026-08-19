//! Application discovery: XDG desktop entries, grouped into Plasma-style
//! categories.
//!
//! The `.desktop` format is a small INI dialect, and the parts we need (the
//! `Desktop Entry` group, localised names, `Exec` field codes) are stable and
//! well specified, so it is parsed here rather than pulled in as a dependency.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use crate::theme::Color;

/// A launchable application.
#[derive(Debug, Clone)]
pub struct App {
    pub name: String,
    pub comment: Option<String>,
    pub icon: Option<String>,
    /// `Exec` with field codes already stripped.
    pub exec: String,
    pub terminal: bool,
    pub categories: Vec<String>,
    /// `MimeType`: what this application says it can open. Read for one
    /// question only — which program one of the user's own files should be
    /// handed to — and answered out of the catalogue rather than by asking a
    /// tool, since the catalogue has already parsed every entry on the
    /// machine. See [`crate::media::opening`].
    pub mime_types: Vec<String>,
    pub path: PathBuf,
    /// `StartupWMClass`: what this application's windows will call themselves,
    /// stated by the application itself. Only 16 of the 235 entries installed
    /// on the machine this was written on set it, so it is the best answer
    /// rather than the only one — see [`App::window_names`].
    pub wm_class: Option<String>,
}

impl App {
    /// Every name a window of this application might go by, best first.
    ///
    /// Asked before starting anything, to find out whether this application is
    /// already running. There is no registry mapping a desktop entry to the
    /// name its windows use, so this is the conventional guess every desktop
    /// makes: what the entry declares, then the entry's own file name (which
    /// is what a well-behaved application derives its app_id from), then the
    /// program it runs.
    pub fn window_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        let mut add = |name: Option<String>| {
            if let Some(name) = name.filter(|name| !name.trim().is_empty()) {
                let known = |seen: &String| seen.eq_ignore_ascii_case(&name);
                if !names.iter().any(known) {
                    names.push(name);
                }
            }
        };

        add(self.wm_class.clone());
        add(self
            .path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::to_string));
        add(program_name(&self.exec));
        names
    }

    /// Whether a window calling itself `app_id` is one of this application's.
    ///
    /// The names this entry could go by, against the one the window gave,
    /// under [`same_application`]'s rule.
    pub fn owns_window(&self, app_id: &str) -> bool {
        self.window_names()
            .iter()
            .any(|name| same_application(name, app_id))
    }
}

/// Whether two window names are the same application's.
///
/// Loose in two directions, because the same application is spelled
/// differently depending on who is doing the spelling: case, since an X11
/// class is conventionally capitalised (`Steam`) where a desktop entry is not,
/// and the trailing component of a reverse-DNS name, since an application
/// shipped as `org.mozilla.firefox` still runs `firefox`. Both are what every
/// other desktop matches on, and the cost of being wrong is bounded: the user
/// gets the window they already had instead of a second copy.
///
/// A name against nothing is never a match. An application that never said
/// what it is has told us nothing to match on, and treating one silence as
/// equal to another would file every nameless window in the session under one
/// application.
pub fn same_application(one: &str, other: &str) -> bool {
    let (one, other) = (one.trim(), other.trim());
    if one.is_empty() || other.is_empty() {
        return false;
    }
    identity(one).eq_ignore_ascii_case(&identity(other))
}

/// The part of a name that says which application it is.
///
/// Two things are taken off it, and the second exists because the first, left
/// alone, was wrong about every program in the session that runs under Wine.
///
/// A Windows program is named by its file, and a file's extension is not its
/// identity: `Affinity.exe` is Affinity. So `.exe` comes off, which is also
/// what lets a window calling itself `Affinity.exe` match the entry that calls
/// the program `affinity`.
///
/// And the reverse-DNS rule below has to be told which names it is for.
/// Reading "the bit after the last dot" off *any* name makes the extension the
/// identity — every `.exe` in existence becomes the same application, and a
/// game's audio stream is then filed under whichever `.exe` the machine
/// happens to have a desktop entry for. That is not hypothetical: it put a
/// running game's volume in the mixer under Affinity's name and Affinity's
/// icon, on a machine where Affinity was not running. A reverse-DNS name has a
/// vendor between the domain and the program — `org.mozilla.firefox`,
/// `app.zen_browser.zen` — so two dots is what says this rule applies, and a
/// single-dotted `Haste.x86_64` is left whole.
fn identity(name: &str) -> String {
    let name = match name.len().checked_sub(4) {
        Some(cut) if cut > 0 && name[cut..].eq_ignore_ascii_case(".exe") => &name[..cut],
        _ => name,
    };
    if name.matches('.').count() >= 2 {
        if let Some(tail) = name.rsplit('.').next().filter(|tail| !tail.is_empty()) {
            return tail.to_string();
        }
    }
    name.to_string()
}

/// The name of the program an `Exec` line runs, without its path or arguments.
fn program_name(exec: &str) -> Option<String> {
    let program = exec.split_whitespace().next()?;
    // An `Exec` that starts with an environment wrapper names the program
    // further along; the wrapper is nobody's window name.
    let program = match program.rsplit('/').next()? {
        "env" | "sh" | "bash" | "flatpak" => exec.split_whitespace().nth(1)?,
        _ => program,
    };
    program
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty() && !name.starts_with('-'))
        .map(str::to_string)
}

/// One row of a column.
///
/// A column is a tree rather than a list, as the original cross media bar's
/// were: a row is something to launch, a file of the user's own, a subcategory
/// holding a column of its own, or one of a set of values the shell is set to.
/// One type rather than four, because a row is drawn the same way whichever it
/// is — an icon, a title, and a line under it — and the bar's whole job is that
/// they all sit in one column together.
#[derive(Debug, Clone)]
pub enum Entry {
    App(App),
    /// A piece of music, a film or a photograph found under the user's home
    /// directory. Not an [`App`] wearing a file's name: the two answer
    /// different questions — nothing installed it, nothing uninstalls it, and
    /// no window will ever call itself by its name — and the rows that would
    /// offer to do those things to it are the reason it is its own kind of row.
    ///
    /// Shared with the library it came from rather than copied out of it; see
    /// [`crate::media::Shelved`].
    Media(crate::media::Shelved),
    /// One file, in the folder it is actually in.
    ///
    /// The third kind of row about a file on this disk, and it is a third kind
    /// for the reason the second is: what the shelves hold is what the user
    /// *has*, gathered by kind from everywhere at once, and what this holds is
    /// what is in one directory. The two answer different questions and they
    /// carry different things — a shelved song is titled without its extension
    /// and says which folder it came from, neither of which is any use to
    /// somebody standing in that folder. See [`crate::files::Item`].
    File(crate::files::Item),
    Folder(Folder),
    Choice(Choice),
    /// A value set by sliding rather than by picking: the one row of the
    /// column it is in, with the whole range under the cursor at once.
    ///
    /// For a setting whose answers are a *scale* rather than a set. A colour
    /// temperature is the one this exists for: every hundred kelvin between
    /// candlelight and daylight is a sensible answer, and a column offering
    /// them as rows would be forty-five of them — a list nobody can scan,
    /// standing for a quantity that has no steps in it to begin with.
    ///
    /// The same object as the guide's quick-settings bars, stood on end. There
    /// it lies along a row in a sidebar and is set by dragging; here it fills
    /// a column of the bar and is set by Up and Down, which is what those two
    /// mean everywhere else in a column. Left still leaves, because Left is
    /// how every column is left.
    Bar(Bar),
    /// The field at the head of a shelf of the user's own files, and the row
    /// that empties it.
    ///
    /// A row rather than a control drawn over the column, because on this bar
    /// a row is the only thing there is. The user reaches it by pressing Up
    /// from the first file, presses it with the same button that opens a file,
    /// and it sits where anything standing over a list sits — at the top of it.
    /// Nothing new had to be learnt to find it.
    Search(Search),
    /// Steam itself, at the head of the Games column: the way in to somebody's
    /// library, and afterwards the way back out of it.
    ///
    /// Not an [`App`] wearing Steam's name, although a machine with Steam
    /// installed does have a `.desktop` file for it. What that row would do is
    /// start a program; what this one does is sign an *account* in, which is
    /// something the shell holds and the desktop entry knows nothing about. So
    /// the two are not the same row, and where both would exist this one takes
    /// the other's place — see [`offer_steam`].
    Steam(Service),
    /// One title in somebody's Steam library.
    ///
    /// Its own kind of row for the reason a file of the user's own is: nothing
    /// on this machine installed it, no `.desktop` file describes it, the
    /// things that can be done to it are Steam's rather than the package
    /// manager's, and half of them are not on the disk at all. What it shares
    /// with an application is only that pressing it starts something.
    Game(Game),
    /// The row under Settings > System that puts up what this machine is.
    ///
    /// Its own kind of row because the three it might have been are each
    /// wrong in the same way. A [`Choice`] carrying a setting would move a
    /// mark and write a file, and there is nothing here to set. A `Choice`
    /// carrying none is inert on purpose — that is the row for a value the
    /// shell can show but not change, and it must stay unpressable. And a
    /// [`Folder`] opens a column, which is a list of answers; what is behind
    /// this is a panel of facts, which is not a list of anything.
    ///
    /// So what it is, is a door: it carries what the row itself says, and what
    /// is behind it. See [`Facts`], and `Shell::show_facts`.
    Facts(Facts),
    /// A value that is *typed* rather than picked off a list or slid along a
    /// bar.
    ///
    /// The third way a value is set in this tree, and it exists for the same
    /// reason [`Bar`] does: there are values a column of alternatives cannot
    /// hold. A colour temperature is a scale, which is what a bar is for; an
    /// IP address is neither a scale nor a set — it is four numbers and a
    /// prefix, of which every one is as likely as any other, and no list
    /// anybody could write would have the user's on it.
    ///
    /// So it is a door too, like [`Entry::Facts`]: the press raises a panel
    /// with a field on it and the on-screen keyboard under that, because on a
    /// console there is nothing else to type with. What comes back does not go
    /// through [`crate::model::Cursor::choose`] — nothing here is chosen and no
    /// mark moves — which is also why it is not a [`Choice`] carrying a
    /// setting: choosing one of those clears the mark off every other `Choice`
    /// in the column, and a row that is pressed to open a field would take the
    /// tick off the value that really is in force.
    Typed(Typed),
}

/// A value that is typed into, as the tree writes the row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Typed {
    pub title: String,
    /// Which connection this value belongs to, in the name its owner would use
    /// for it — a wireless network's own name, or what a wired profile is
    /// filed under.
    ///
    /// On the panel and nowhere else. A field raised over the bar covers the
    /// trail that would otherwise say whose value it is, and "Address" on its
    /// own is the same panel whether the user walked in through Wi-Fi or
    /// through the socket on the back of the machine. Addressing belongs to a
    /// profile rather than to the shell, so which profile is not a detail —
    /// it is the whole of what the value is about.
    pub whose: String,
    /// What it is set to now, or that it is not set to anything.
    pub comment: String,
    pub icon: String,
    /// What is in it, which is what the field opens with. Empty for a value
    /// nobody has set.
    pub value: String,
    /// Which value it is, and whose. Carried rather than looked up on the
    /// press, because by then the panel is what is on screen and the row it
    /// grew out of may have been rebuilt underneath it.
    pub about: crate::settings::Typing,
}

/// A row that opens onto a panel of values to read rather than onto a column.
///
/// Two of them today — what this machine is, and what a network interface was
/// given — and they are one kind of row rather than two because the argument
/// for them is the same argument twice. What differs is only where the values
/// come from, which is [`About`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    pub title: String,
    pub comment: String,
    pub icon: String,
    /// What the panel says, and where it comes from.
    pub about: About,
}

/// Where the values behind one of these rows come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum About {
    /// This machine, read from `/proc` at the moment the row is pressed.
    ///
    /// Read on the press rather than carried here, because it is read from
    /// files that change under the shell: a memory total kept in the bar would
    /// go stale in a tree that is rebuilt for every other reason but this one,
    /// and reading it costs a fraction of a millisecond. See [`crate::machine`].
    Machine,
    /// A list of named values, gathered where the row was built.
    ///
    /// Carried rather than read on the press, and that is the honest division
    /// between the two: what is behind this one is not a file the shell can go
    /// and read, it is the answer a worker last got from somewhere else — so
    /// the row is rebuilt when *that* changes, which is the same moment every
    /// other row of its page is. See [`crate::network`].
    Listed(Vec<(String, String)>),
}

/// The Steam row, as the head of the Games column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    /// The account that is signed in, if one is. What tells the press what to
    /// do: sign in, or step over to the column that signing in built.
    pub account: Option<String>,
    /// The line under the name, which is the account or the offer.
    comment: String,
}

impl Service {
    fn new(account: Option<String>) -> Service {
        Service {
            comment: match account.as_deref() {
                Some(account) => format!("Signed in as {account}"),
                None => "Sign in to play your Steam library here".to_string(),
            },
            account,
        }
    }
}

/// One title in the Steam column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Game {
    pub app_id: u32,
    pub name: String,
    /// What goes under the name: whether it is here, how big it is, how long
    /// it has been played. Built where the library is, because that is where
    /// the numbers are.
    pub note: String,
    /// Whether it is on the disk and can be started right now.
    pub installed: bool,
    /// Whether Steam is fetching it at this moment.
    pub updating: bool,
    /// Whether there is a Valve client on this machine to start it with.
    /// Without one, nothing in the Steam column can be played or fetched, and
    /// the row says so rather than doing nothing.
    pub steam_client: bool,
}

/// The field at the head of a shelf, or the row beneath it that clears the
/// field.
///
/// One kind of row for both, because they are one thing: the search a column
/// is under, offered as the two presses that can be made about it. Splitting
/// them into two variants would put the shelf, the query and the counts on
/// both of them and leave nothing to say they were about the same search.
#[derive(Debug, Clone)]
pub struct Search {
    /// What this field narrows, so a press on the row knows what to ask.
    pub of: Searched,
    /// What is being searched for, as the user typed it. Empty for a shelf
    /// nobody has searched, which is the state every column starts in.
    ///
    /// The one thing on this row the shell writes to directly. Everything else
    /// arrives from the worker with the rows it built; this is what has been
    /// typed, and it has to be on screen on the frame the key was pressed
    /// rather than on the frame the shelf has finished being narrowed.
    pub query: String,
    /// The line under the row, worked out where the shelf is because it counts
    /// the shelf. It therefore lags the query by one delivery while somebody is
    /// typing — as do the rows below it, which is the point: the field says
    /// what has been asked, and everything under it says what has been found so
    /// far. The two are never inconsistent with each other, only with the
    /// future.
    note: String,
    pub role: Role,
}

/// What a field at the head of a column is a search *of*.
///
/// Two answers and they are answered in two different places, which is the
/// whole reason this is a type. A shelf is half a million files held on a
/// worker, so narrowing one is a message to that worker and the rows come back
/// when they are ready. A folder is one directory on the disk, so narrowing one
/// is reading it again — which costs a `readdir` and a `stat` per row kept, and
/// is done on the frame the letter was typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Searched {
    Shelf(crate::media::Kind),
    Folder,
}

impl Searched {
    /// The shelf this is, if it is one. `None` for a folder, which is what
    /// keeps everything the media library does away from the explorer's rows.
    pub fn shelf(self) -> Option<crate::media::Kind> {
        match self {
            Searched::Shelf(kind) => Some(kind),
            Searched::Folder => None,
        }
    }

    /// What a list of what it holds is called, in a sentence: "audio files",
    /// "images", "items".
    fn plural(self) -> &'static str {
        match self {
            Searched::Shelf(kind) => kind.plural(),
            // Not "files": what a folder holds is folders as well, and a field
            // saying "3 of 40 files match" over a column of directories would
            // be counting something the user cannot see.
            Searched::Folder => "items",
        }
    }
}

/// Which of the two rows a [`Search`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The field itself: pressing it raises the keyboard to type into.
    Field,
    /// Empties the field, and is only on the column while there is something
    /// in it to empty. A search cleared by backspacing would be ten presses of
    /// one key on a board driven with a thumb.
    Clear,
}

impl Search {
    /// The field at the head of a column that holds `found` things, `matched`
    /// of which the query has kept.
    fn field(of: Searched, query: &str, matched: usize, found: usize) -> Search {
        Search {
            of,
            query: query.to_string(),
            note: if query.is_empty() {
                match of {
                    Searched::Shelf(kind) => format!("Search {} by name", kind.plural()),
                    Searched::Folder => "Search this folder by name".to_string(),
                }
            } else {
                crate::media::search_note(of.plural(), matched, found)
            },
            role: Role::Field,
        }
    }

    /// The row under it that empties it.
    fn clear(of: Searched, query: &str, found: usize) -> Search {
        Search {
            of,
            query: query.to_string(),
            note: format!("Show all {found} {}", of.plural()),
            role: Role::Clear,
        }
    }

    /// What the row is called.
    ///
    /// The query itself, once there is one — the field *is* the row, so what
    /// it holds is what it says. The word "Search" only stands in while it is
    /// empty, which is exactly when there is nothing else for the row to be.
    fn label(&self) -> &str {
        match self.role {
            Role::Clear => "Clear search",
            Role::Field if self.query.is_empty() => "Search",
            Role::Field => &self.query,
        }
    }

    fn icon(&self) -> &'static str {
        match self.role {
            Role::Field => crate::icons::SEARCH,
            Role::Clear => crate::icons::SEARCH_CLEAR,
        }
    }
}

/// A subcategory: a column of its own, stepped into from the row that names it.
#[derive(Debug, Clone)]
pub struct Folder {
    pub title: String,
    pub comment: Option<String>,
    /// One of the shell's own glyphs, and looked up as one — without the
    /// missing-icon fallback an application gets. A subcategory that came out
    /// as the generic executable icon would read as an application that cannot
    /// be launched rather than as a way further in.
    pub icon: Option<String>,
    pub entries: Vec<Entry>,
    /// Where on the disk this column comes from, for the subcategories that
    /// are a place rather than a list the shell wrote.
    ///
    /// `None` for every subcategory in the tree but the file explorer's, which
    /// is the only part of the bar whose columns are not known until somebody
    /// asks for them: a folder is read on the press that opens it and thrown
    /// away when the user walks past it. See [`crate::files`], and
    /// [`crate::model::Cursor::open_place`] for where the reading happens.
    pub place: Option<crate::files::Place>,
    /// Whether this is the one in force, where the column it stands in is a set
    /// of alternatives and this one has more inside it.
    ///
    /// The one row shape in the tree that is both an answer and a way further
    /// in, and it exists because a wireless network is both. The column under
    /// Networks asks which network this radio is on; the network it is on is
    /// also the only one whose address and name servers there is anything to
    /// say about, because those belong to the profile it is connected by. So
    /// that row carries the tick every other value in force carries — see
    /// [`Entry::chosen`], which is what draws it and what opens a column on it
    /// — and stepping into it is what the press does instead of joining a
    /// network the radio is already on.
    ///
    /// `false` everywhere else, which is every subcategory in the shell but
    /// that one: a folder in a list of folders is not an answer to anything.
    pub chosen: bool,
}

/// One of a set of alternatives, exactly one of which is in force.
///
/// The row a settings list is made of. It is a leaf: choosing it changes what
/// the shell is set to rather than opening anything.
#[derive(Debug, Clone)]
pub struct Choice {
    pub title: String,
    pub comment: Option<String>,
    pub icon: Option<String>,
    /// What this row stands for, when the setting is a colour. The atlas
    /// multiplies a quad's colour into the texel it samples, so a plain white
    /// swatch drawn in this comes out as the colour itself — which is the one
    /// label a colour cannot be given in words.
    pub swatch: Option<Color>,
    /// Whether this is the one currently in force.
    pub chosen: bool,
    /// Whether pressing this row *does* something rather than answering the
    /// question its column asks.
    ///
    /// Almost nothing in the tree is one of these. A settings column is a set
    /// of alternatives and every row in it is an answer, which is what makes
    /// the mark meaningful: it is on the answer in force, and pressing another
    /// row moves it. A row that acts is not in that set — Forget removes a
    /// saved network, and nothing about the column is different afterwards
    /// except that the row is gone.
    ///
    /// So it never takes the mark, and pressing it never takes the mark off
    /// whatever is holding it — see [`crate::model::Cursor::choose`]. Without
    /// this, pressing Forget would put a tick on a press, which is not a state
    /// anything can be in, and would silently un-mark the value beside it.
    pub acts: bool,
    /// What choosing this row does. `None` for a value the shell can show but
    /// not change, which stays inert rather than taking the mark off a row
    /// that describes something true.
    pub setting: Option<crate::settings::Setting>,
}

/// A value on a scale, and the two steps either side of where it stands.
///
/// Rebuilt from the live setting every time the column is, so the row on screen
/// always carries what pressing Up and Down would do *from here*. That is what
/// keeps the sliding out of the model entirely: the bar holds no state of its
/// own, and a press is the same "apply this setting" every other row in the
/// tree performs.
#[derive(Debug, Clone)]
pub struct Bar {
    /// What the value reads as — `4000 K`. The title, because on a bar the
    /// number *is* the row: the name of the setting is on the row this column
    /// was opened from, one step to the left and still on screen.
    pub title: String,
    /// What that value means, in the words a number cannot carry.
    pub comment: Option<String>,
    /// Where the handle stands, 0 at the foot of the track and 1 at its head.
    pub fill: f32,
    /// The colour the filled part is drawn in, when the value has one of its
    /// own. A colour temperature does: the bar is then a picture of what the
    /// screen is about to look like, which no number and no word can be.
    pub swatch: Option<Color>,
    /// What one step up the track applies, and one step down. `None` at either
    /// end of the range, which is what makes the bar stop there.
    pub up: Option<crate::settings::Setting>,
    pub down: Option<crate::settings::Setting>,
    /// Every value the bar can be set to, the foot of the track first. What a
    /// press *along* the groove picks from: a direction knows only the step
    /// either side of where the handle is, and a click has landed somewhere the
    /// handle is not.
    ///
    /// Evenly spaced, which is what [`Bar::fill`] already says — the handle
    /// stands at the same share of the track as the value does of the range —
    /// so where a click landed and which of these it asks for are the same
    /// question. Carried as the whole list rather than as a range and a step
    /// because a setting is a setting: the shell applies one of these exactly
    /// as it applies the row above it, and nothing here has to know what a
    /// kelvin is.
    pub steps: Vec<crate::settings::Setting>,
}

impl Bar {
    /// Which step of the bar stands at `level` along the track — 0 at the foot
    /// and 1 at the head.
    ///
    /// Rounded, so each step owns the half of the track on either side of it
    /// and there is nowhere on the groove that belongs to no value.
    fn step_at(&self, level: f32) -> Option<usize> {
        let last = self.steps.len().checked_sub(1)?;
        Some((level.clamp(0.0, 1.0) * last as f32).round() as usize)
    }

    /// What a press at `level` along the groove applies.
    ///
    /// `None` where the press asks for nothing: a bar with no steps in it, and
    /// a press that landed on the step the handle is already standing on —
    /// which is what aiming at a value and missing by a pixel looks like, and
    /// is not a change to apply, write down and make a noise about.
    pub fn at(&self, level: f32) -> Option<crate::settings::Setting> {
        let step = self.step_at(level)?;
        if self.step_at(self.fill) == Some(step) {
            return None;
        }
        self.steps.get(step).copied()
    }
}

/// A top-level XMB column.
#[derive(Debug, Clone)]
pub struct Category {
    pub id: &'static str,
    pub title: &'static str,
    /// Icon name looked up in the icon theme.
    pub icon: &'static str,
    pub entries: Vec<Entry>,
}

/// The shell's own column, for settings that belong to LineXinBar itself rather
/// than to anything installed on the system.
///
/// Always present and always first, the way the real XMB opens on Settings.
/// It is deliberately not part of [`CATEGORY_TABLE`]: nothing on disk is
/// classified into it, so it is not a destination for `.desktop` files.
pub const SHELL_SETTINGS: (&str, &str, &str) =
    ("settings", "Settings", crate::icons::CATEGORY_SETTINGS);

/// The columns that hold something the shell found rather than something
/// installed, and are therefore named in more than one place.
const MULTIMEDIA: &str = "multimedia";
const GRAPHICS: &str = "graphics";
/// System is named for a third reason again: the row it carries is not
/// something found or something installed, but a way in to the disk itself,
/// and it is on the column whether or not this machine has a single system
/// tool on it. See [`subcategories`] and the retain at the end of [`assemble`].
const SYSTEM: &str = "system";
/// Games is here for a different reason from the other two: nothing is
/// *found* for it, but the Steam row goes at its head whether or not a single
/// game is installed, so the column has to be nameable from outside the table.
const GAMES: &str = "games";

/// Where installed applications go, in XMB order.
///
/// Each entry lists the XDG main categories that map onto it, and the first
/// match wins. `Settings` and `System` share a column, as they do in Plasma —
/// its menu has no Settings menu of its own, and the shell's own Settings
/// column is not somewhere an installed application belongs.
const CATEGORY_TABLE: &[(&str, &str, &str, &[&str])] = &[
    (
        SYSTEM,
        "System",
        crate::icons::CATEGORY_SYSTEM,
        &["Settings", "System"],
    ),
    (
        MULTIMEDIA,
        "Multimedia",
        crate::icons::CATEGORY_MULTIMEDIA,
        &["AudioVideo", "Audio", "Video"],
    ),
    (
        GRAPHICS,
        "Graphics",
        crate::icons::CATEGORY_GRAPHICS,
        &["Graphics"],
    ),
    (
        "internet",
        "Internet",
        crate::icons::CATEGORY_INTERNET,
        &["Network"],
    ),
    (
        "office",
        "Office",
        crate::icons::CATEGORY_OFFICE,
        &["Office"],
    ),
    (GAMES, "Games", crate::icons::CATEGORY_GAMES, &["Game"]),
    (
        "development",
        "Development",
        crate::icons::CATEGORY_DEVELOPMENT,
        &["Development"],
    ),
    (
        "education",
        "Education & Science",
        crate::icons::CATEGORY_EDUCATION,
        &["Education", "Science"],
    ),
    (
        "utilities",
        "Utilities",
        crate::icons::CATEGORY_UTILITIES,
        &["Utility"],
    ),
    ("other", "Other", crate::icons::CATEGORY_OTHER, &[]),
];

/// Which column each shelf of the user's own files hangs in, and what its row
/// is called there.
///
/// The one table that says a kind of file belongs under a particular column,
/// so the row, the glyph, the walk's own sorting and the place a new column is
/// made all read it rather than each carrying their own copy.
const SHELVES: &[(&str, &str, crate::media::Kind)] = &[
    (MULTIMEDIA, "Music", crate::media::Kind::Audio),
    (MULTIMEDIA, "Video", crate::media::Kind::Video),
    (GRAPHICS, "Images", crate::media::Kind::Image),
];

/// What the row a kind of file hangs on is called.
///
/// Read out of [`SHELVES`] rather than written down a second time, so the row
/// on the bar and every panel that names it cannot come to disagree.
pub fn shelf_title(kind: crate::media::Kind) -> &'static str {
    SHELVES
        .iter()
        .find(|(_, _, own)| *own == kind)
        .map(|(_, title, _)| *title)
        .unwrap_or_default()
}

/// The rows a column carries of its own, before anything on disk is filed into
/// it.
///
/// Multimedia is one column over two subjects, and which of the two an
/// *application* belongs to is a question `.desktop` files answer badly: the
/// spec requires `AudioVideo` alongside `Audio` or `Video` but never the
/// reverse, so an entry is free to say `AudioVideo` and stop — and the
/// best-known ones do. Splitting the column on that would put VLC in whichever
/// half won a coin toss, so the players stay in the column itself.
///
/// The rows hold what the machine can answer for without guessing: the user's
/// own music, films and photographs, gathered from under their home directory
/// by [`crate::media`] and hung here as they are found. A file's kind is its
/// extension and nothing else has to be inferred from it.
///
/// Graphics carries one row rather than two, because there is one subject
/// under it. It is the same kind of row all the same — a way in to what the
/// user has, standing above the tools that make more of it.
fn subcategories(id: &str) -> Vec<Entry> {
    if id == SYSTEM {
        return vec![files_row()];
    }
    SHELVES
        .iter()
        .filter(|(column, ..)| *column == id)
        .map(|(_, title, kind)| {
            Entry::Folder(Folder {
                title: title.to_string(),
                // What an empty shelf says while the walk is still on its first
                // pass, which is what these rows are on the first frame of
                // every session. Replaced by the library's own note as it fills.
                comment: Some(crate::media::note(*kind, 0, false)),
                icon: Some(kind.glyph().to_string()),
                entries: Vec::new(),
                place: None,
                chosen: false,
            })
        })
        .collect()
}

/// The row at the head of System that opens the disk.
///
/// It hangs in System rather than in a column of its own, and rather than in
/// Utilities where a file manager's `.desktop` file would land. What is under
/// it is not a tool and not a document: it is the machine's own storage — the
/// disks, and what is on them — which is the same subject as the rest of that
/// column and is filed with it for the same reason the console filed a memory
/// card there.
///
/// Empty until it is stepped into. What the disks are is a question about the
/// moment it is asked — a drive plugged in during a session has to be on it —
/// so the answer is not built here; see [`crate::files::volumes`].
fn files_row() -> Entry {
    Entry::Folder(Folder {
        title: "Files".to_string(),
        comment: Some("Your folder, this machine, and anything plugged in".to_string()),
        icon: Some(crate::icons::CATEGORY_FILES.to_string()),
        entries: Vec::new(),
        place: Some(crate::files::Place::Volumes),
        chosen: false,
    })
}

/// The rows a shelf of the user's own files makes: the search at the head of
/// the column, and then whatever the search has left of it.
///
/// Here rather than in [`crate::media`] because what a row *is* belongs to the
/// bar, and called from there because of when it has to happen: this is one
/// allocation the size of the collection and a write per file, and it is done
/// on the worker that already holds the files rather than on the thread that
/// draws. See [`crate::media::Made`].
///
/// `listing` is what the search has kept and `found` is how many there are
/// altogether, so a column that has been narrowed to nothing still carries the
/// two rows that say why — a user left looking at an empty column with no
/// field in it would have no way back to their own files but the one they
/// could not see.
///
/// A shelf with nothing on it at all gets neither row. There is nothing there
/// to search, and a column holding only the offer to search it is a column
/// worth stepping into for nothing.
pub fn media_rows(
    listing: Vec<crate::media::Shelved>,
    kind: crate::media::Kind,
    query: &str,
    found: usize,
) -> Vec<Entry> {
    let mut rows = Vec::with_capacity(listing.len() + 2);
    head(
        &mut rows,
        Searched::Shelf(kind),
        query,
        listing.len(),
        found,
    );
    rows.extend(listing.into_iter().map(Entry::Media));
    rows
}

/// The same for a column of the file explorer: the field at the head of it, and
/// then whatever the search has left of the folder.
///
/// The same two rows for the same reasons, which is the point — a field is a
/// field wherever it is on this bar, reached by pressing Up from the top of the
/// list, typed into with the same board, and emptied by the row under it. What
/// differs is only where the narrowing happens; see [`Searched`].
///
/// `found` is everything the folder holds, so a column narrowed to nothing
/// still carries the two rows that say why. A folder with nothing in it at all
/// gets neither: there is nothing there to search, and a column holding only
/// the offer to search it is a column worth stepping into for nothing.
pub fn place_rows(listing: Vec<Entry>, query: &str, found: usize) -> Vec<Entry> {
    let mut rows = Vec::with_capacity(listing.len() + 2);
    head(&mut rows, Searched::Folder, query, listing.len(), found);
    rows.extend(listing);
    rows
}

/// The rows that stand over a list: the field, and the row that empties it.
fn head(rows: &mut Vec<Entry>, of: Searched, query: &str, matched: usize, found: usize) {
    if found == 0 {
        return;
    }
    rows.push(Entry::Search(Search::field(of, query, matched, found)));
    if !query.is_empty() {
        rows.push(Entry::Search(Search::clear(of, query, found)));
    }
}

/// Which shelf of the user's own files a column is, if it is one of the three.
///
/// Asked of the rows rather than of the row they hang under, because that is
/// what a cursor standing in a column has in front of it. The field at the head
/// is what answers: nothing but a shelf carries one, and it names the shelf it
/// searches — see [`media_rows`].
///
/// `None` for every other column, and for a shelf with nothing on it at all,
/// which carries no rows and cannot be stepped into.
pub fn shelf_shown(entries: &[Entry]) -> Option<crate::media::Kind> {
    match entries.first() {
        Some(Entry::Search(search)) => search.of.shelf(),
        _ => None,
    }
}

/// How many rows at the head of a column are the search rather than what the
/// column is a list *of*.
///
/// A column of music opens on music. The field stands over the list in the
/// place anything standing over a list stands, and is reached by pressing Up
/// from the top of it — which is the one direction nothing else was using, and
/// where a person looks for the thing above the first thing. Opening *on* it
/// would make every visit to a shelf start by stepping over a control the user
/// did not ask for.
pub fn head_rows(entries: &[Entry]) -> usize {
    entries
        .iter()
        .take_while(|entry| entry.search().is_some())
        .count()
}

/// Put `query` on the field at the head of whichever column searches `kind`,
/// without waiting for the worker to narrow anything.
///
/// What makes the field a field. Everything else about the column is built
/// where the files are and arrives a moment later; the letter that was just
/// typed has to be on the next frame, and this is the whole of how it gets
/// there. Returns whether a field was found to write to.
pub fn set_search_text(categories: &mut [Category], kind: crate::media::Kind, query: &str) -> bool {
    for category in categories {
        for entry in &mut category.entries {
            let Some(rows) = entry.entries_mut() else {
                continue;
            };
            // The head of the column or nowhere: the field is the first row of
            // the shelf it belongs to, and a scan of half a million files
            // looking for it would be the one thing this exists to avoid.
            let Some(Entry::Search(search)) = rows.first_mut() else {
                continue;
            };
            if search.of != Searched::Shelf(kind) {
                continue;
            }
            search.query = query.to_string();
            return true;
        }
    }
    false
}

/// Hang a shelf the worker has finished on the row that holds it.
///
/// The library is the truth and the tree is a copy of it, rather than the rows
/// owning what they hold: the catalogue is rebuilt whenever something is
/// installed or removed, and a list of the user's music that a package removal
/// emptied would be a strange way to answer for the disk.
///
/// Returns where a column had to be *made*, if one was. A machine with
/// photographs on it but no graphics application installed has no Graphics
/// column at scan time — there was nothing to put in it — and the first file
/// found is what earns it one. The caller has to know, because every display's
/// cursor is standing in a bar that has just grown a column.
///
/// The rows that were there come back in [`Hung::worn`], whole, for the caller
/// to hand back to the worker rather than let go of on this thread; see
/// [`crate::media::Library::discard`].
pub fn shelve_media(categories: &mut Vec<Category>, made: crate::media::Made) -> Hung {
    let mut hung = Hung::default();
    let Some((id, _, kind)) = SHELVES.iter().find(|(_, _, kind)| *kind == made.kind) else {
        return hung;
    };

    let at = match categories.iter().position(|column| column.id == *id) {
        Some(at) => at,
        // Nothing found of this kind, so nothing to make a column for.
        // Deliberately not "nothing found of any kind it holds": a column
        // conjured for a row that would be empty is a column with nothing
        // in it to reach.
        None if made.rows.is_empty() => return hung,
        None => {
            let (id, title, icon, _) = CATEGORY_TABLE
                .iter()
                .find(|(own, ..)| own == id)
                .expect("every shelf names a column of the table");
            let at = column_place(categories, id);
            categories.insert(
                at,
                Category {
                    id,
                    title,
                    icon,
                    entries: subcategories(id),
                },
            );
            hung.column = Some(at);
            at
        }
    };

    let found = categories[at]
        .entries
        .iter_mut()
        .find_map(|entry| match entry {
            Entry::Folder(folder) if folder.icon.as_deref() == Some(kind.glyph()) => Some(folder),
            _ => None,
        });
    if let Some(folder) = found {
        folder.comment = Some(made.note);
        hung.worn = std::mem::replace(&mut folder.entries, made.rows);
    }
    hung
}

/// Lift the shelves out of a catalogue that is about to be thrown away.
///
/// The rows of the user's own files are the only thing in the tree that did not
/// come off the disk with the desktop entries, so a rescan — something was
/// installed, something was removed — would otherwise drop them and leave the
/// columns empty until the walk next came round. Taken whole and handed to
/// [`shelve_media`] against the new catalogue, which is a move of three vectors
/// rather than the collection's worth of work rebuilding them would be.
pub fn carried_media(categories: &mut [Category]) -> Vec<crate::media::Made> {
    let mut carried = Vec::new();
    for (_, _, kind) in SHELVES {
        for category in categories.iter_mut() {
            let found = category.entries.iter_mut().find_map(|entry| match entry {
                Entry::Folder(folder) if folder.icon.as_deref() == Some(kind.glyph()) => {
                    Some(folder)
                }
                _ => None,
            });
            let Some(folder) = found else {
                continue;
            };
            if folder.entries.is_empty() {
                continue;
            }
            carried.push(crate::media::Made {
                kind: *kind,
                rows: std::mem::take(&mut folder.entries),
                note: folder.comment.clone().unwrap_or_default(),
                // Filled in by the caller, which is the only one holding the
                // library that knows.
                orders: crate::media::Orders::default(),
            });
        }
    }
    carried
}

/// Take the row for `path` off every column that holds one, because the file is
/// not on the disk any more. Says whether there was one.
///
/// The bar's own copy only. The shelf it was built from is the worker's, and is
/// told separately — see [`crate::media::Library::forget`] — because the answer
/// the user is owed is the row leaving the screen on the frame they deleted it,
/// and waiting for a shelf of half a million rows to be rebuilt and sent back
/// is not that.
pub fn forget_file(categories: &mut [Category], path: &std::path::Path) -> bool {
    let mut dropped = false;
    for category in categories {
        dropped |= forget_below(&mut category.entries, path);
    }
    dropped
}

/// The walk that does it, one column at a time.
///
/// Recursive, and it does not stop at the first row it drops: the same file can
/// be on the bar twice over — once on the shelf the walk shelved it on, once in
/// the folder the explorer is listing — and a deletion that took the row the
/// user was looking at and left the other would be a shell that had half
/// understood.
fn forget_below(entries: &mut Vec<Entry>, path: &std::path::Path) -> bool {
    let before = entries.len();
    entries.retain(|row| match row {
        Entry::Media(file) => file.path != path,
        Entry::File(file) => file.path != path,
        _ => true,
    });
    let mut dropped = entries.len() != before;
    for entry in entries {
        if let Entry::Folder(folder) = entry {
            dropped |= forget_below(&mut folder.entries, path);
        }
    }
    dropped
}

/// The column somebody's Steam library hangs in.
///
/// Deliberately not part of [`CATEGORY_TABLE`], for the reason the shell's own
/// Settings column is not: nothing on disk is classified into it. It is a
/// consequence of an account being signed in, and it goes away again when that
/// account does.
const STEAM: (&str, &str, &str) = ("steam", "Steam", crate::icons::CATEGORY_STEAM);

/// What that column is called on the bar, for the one thing outside this
/// module that has to find it: pressing the Steam row takes the display to it.
pub fn steam_column() -> &'static str {
    STEAM.0
}

/// And what it is called on screen, for the panel that asks what order to list
/// it in — the column and not the game the menu was raised over, because what
/// is being ordered is the whole library.
pub fn steam_title() -> &'static str {
    STEAM.1
}

/// Whether the shell's own Steam row is on this bar.
///
/// Which is the same question as whether this session does Steam at all: the
/// row goes up as the shell starts and stays up signed in or out, and only a
/// session started with `--no-steam` is without one. Asked before a bar is
/// rebuilt from a fresh scan, so that what Steam put on it can be put back.
pub fn steam_offered(categories: &[Category]) -> bool {
    categories.iter().any(|column| {
        column
            .entries
            .iter()
            .any(|entry| matches!(entry, Entry::Steam(_)))
    })
}

/// Take Valve's client's own `.desktop` entry off the bar, wherever the scan
/// filed it.
///
/// Two rows called Steam — one starting a program, one signing an account in —
/// is the kind of thing a user has to press to tell apart; and of the two, the
/// shell's own is the one that leads somewhere, since the client itself is
/// still one row of the menu raised on it away. So the shell's row replaces the
/// client's rather than standing beside it.
///
/// Every column and not just Games, which is the whole reason this is a sweep
/// of its own rather than a line of [`offer_steam`]. Valve's file declares
/// `Categories=Network;FileTransfer;Game`, and the first main category that
/// matches decides the column — so on an ordinary machine the client's row is
/// not in Games at all but in Internet, where the row replacing it never
/// looked. A column left with nothing to start goes with it, by the rule every
/// scanned column is kept or dropped by; see [`assemble`].
///
/// Called once for each catalogue built from the disk, and only on a session
/// that has a Steam row of its own to offer: one started with `--no-steam`
/// keeps the client's entry, because there it is the only way to reach Steam.
pub fn hide_steam_client(categories: &mut Vec<Category>) {
    let mut emptied = Vec::new();
    for (at, column) in categories.iter_mut().enumerate() {
        let before = column.entries.len();
        column
            .entries
            .retain(|entry| !entry.app().is_some_and(|app| app.owns_window("steam")));
        // Only a column this took something out of can have been emptied by
        // it, which is also what keeps the shell's own Settings column — which
        // has nothing launchable in it and never an application — out of this.
        if column.entries.len() != before && !column.has_launchable() {
            emptied.push(at);
        }
    }
    // Back to front, so each index still means the column it was read from.
    for at in emptied.into_iter().rev() {
        categories.remove(at);
    }
}

/// Put the Steam row at the head of the Games column, or take it away.
///
/// `account` is who is signed in, if anybody. Returns where a column had to be
/// *made* — a machine with no game installed has no Games column, and the
/// offer to sign in to Steam is enough to earn it one, because from that row
/// the whole library is one press away.
///
/// The row is rebuilt rather than edited, because what it says is built from
/// the account and there is nothing else on it. The client's own entry is not
/// this function's business — it is taken off the bar wherever it landed, by
/// [`hide_steam_client`], as each catalogue is built.
pub fn offer_steam(categories: &mut Vec<Category>, account: Option<String>) -> Shifted {
    let mut shifted = Shifted::default();

    let at = match categories.iter().position(|column| column.id == GAMES) {
        Some(at) => at,
        None => {
            let (id, title, icon, _) = CATEGORY_TABLE
                .iter()
                .find(|(own, ..)| *own == GAMES)
                .expect("the Games column is in the table");
            let at = column_place(categories, id);
            categories.insert(
                at,
                Category {
                    id,
                    title,
                    icon,
                    entries: Vec::new(),
                },
            );
            shifted.added = Some(at);
            at
        }
    };

    let column = &mut categories[at];
    // The row this is replacing, if it is already there.
    column
        .entries
        .retain(|entry| !matches!(entry, Entry::Steam(_)));
    // At the head of the column, above the applications: it is the way in to
    // a whole other column, and a way in belongs where the eye lands.
    column
        .entries
        .insert(0, Entry::Steam(Service::new(account)));
    shifted
}

/// Hang somebody's Steam library in a column of its own, or take the column
/// away when there is no longer one to hang.
///
/// The rows arrive already in the order they go in — installed first, each
/// half alphabetical — because that ordering belongs to the library and not to
/// the bar; see [`lxb_steam::library::sorted`].
///
/// Returns what this did to the shape of the bar, because every display's
/// cursor is standing in it.
pub fn shelve_steam(categories: &mut Vec<Category>, games: Vec<Entry>) -> Shifted {
    let mut shifted = Shifted::default();
    let standing = categories.iter().position(|column| column.id == STEAM.0);

    match (standing, games.is_empty()) {
        // Nothing to show and no column showing it: the ordinary state of a
        // machine nobody has signed in on.
        (None, true) => {}
        // Signed out, or a library that has become empty. The column goes with
        // it rather than standing there empty — a column with nothing in it is
        // dead space to scroll past, which is the same rule every scanned
        // column is kept or dropped by.
        (Some(at), true) => {
            categories.remove(at);
            shifted.removed = Some(at);
        }
        (Some(at), false) => categories[at].entries = games,
        (None, false) => {
            let (id, title, icon) = STEAM;
            let at = column_place(categories, id);
            categories.insert(
                at,
                Category {
                    id,
                    title,
                    icon,
                    entries: games,
                },
            );
            shifted.added = Some(at);
        }
    }
    shifted
}

/// What putting a column on the bar, or taking one off it, disturbed.
///
/// Never both at once: each of the two functions that returns one of these
/// does one thing to the bar. Two fields rather than a signed number because
/// the two are different events for a cursor — one is a column that has moved
/// under it and one is a column that may have been *under* it.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Shifted {
    pub added: Option<usize>,
    pub removed: Option<usize>,
}

/// What hanging a shelf on the bar disturbed.
#[derive(Debug, Default)]
pub struct Hung {
    /// Where a column had to be made, if one was — every display's cursor is
    /// standing in a bar that has just grown one.
    pub column: Option<usize>,
    /// The rows the new ones replaced, taken whole rather than emptied out:
    /// moving half a million of them one at a time is the copy this hands back
    /// to the worker to avoid.
    pub worn: Vec<Entry>,
}

/// Where a column belongs among the columns there already are.
///
/// The bar is in [`CATEGORY_TABLE`] order, so this is the first column that
/// belongs *after* this one — or the end, when there is none. The shell's own
/// Settings column is not in the table and is therefore never landed in front
/// of, which is the whole of what it needs from this.
fn column_place(categories: &[Category], id: &str) -> usize {
    let mine = rank(id);
    categories
        .iter()
        .position(|column| rank(column.id).is_some_and(|other| other > mine.unwrap_or_default()))
        .unwrap_or(categories.len())
}

/// How far along the bar a column belongs, in half-steps.
///
/// The table's own order, doubled, so that a column which is not in the table
/// can sit *between* two that are without either of them having to move.
/// There is exactly one such column: Steam, which belongs immediately after
/// Games because that is what it is a library of — a person who has just been
/// looking at what is installed and steps right lands in what they own.
fn rank(id: &str) -> Option<usize> {
    if id == STEAM.0 {
        return rank(GAMES).map(|games| games + 1);
    }
    CATEGORY_TABLE
        .iter()
        .position(|(own, ..)| *own == id)
        .map(|place| place * 2)
}

impl App {
    /// Parse one `.desktop` file. Returns `None` for entries that should not
    /// appear in a menu (hidden, `NoDisplay`, non-application types).
    pub fn from_file(path: &Path) -> Option<App> {
        let raw = std::fs::read_to_string(path).ok()?;
        Self::parse(&raw, path)
    }

    fn parse(raw: &str, path: &Path) -> Option<App> {
        let mut in_entry = false;
        let mut fields: BTreeMap<String, String> = BTreeMap::new();

        for line in raw.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.starts_with('[') {
                // Only the main group matters; actions and other groups are skipped.
                in_entry = line == "[Desktop Entry]";
                continue;
            }
            if !in_entry {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                fields
                    .entry(key.trim().to_string())
                    .or_insert_with(|| value.trim().to_string());
            }
        }

        if fields.get("Type").map(String::as_str) != Some("Application") {
            return None;
        }
        if is_true(fields.get("NoDisplay")) || is_true(fields.get("Hidden")) {
            return None;
        }

        let name = localised(&fields, "Name")?;
        let exec = strip_field_codes(fields.get("Exec")?);
        if exec.trim().is_empty() {
            return None;
        }

        if !shown_in(&fields, &current_desktops()) {
            return None;
        }

        let categories = fields
            .get("Categories")
            .map(|c| {
                c.split(';')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

        let mime_types = fields
            .get("MimeType")
            .map(|list| {
                list.split(';')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

        Some(App {
            name,
            comment: localised(&fields, "Comment"),
            icon: fields.get("Icon").cloned(),
            exec,
            terminal: is_true(fields.get("Terminal")),
            categories,
            mime_types,
            path: path.to_path_buf(),
            wm_class: fields.get("StartupWMClass").cloned(),
        })
    }

    /// Which column this app belongs in.
    fn category_id(&self) -> &'static str {
        for (id, _, _, xdg) in CATEGORY_TABLE {
            if xdg
                .iter()
                .any(|c| self.categories.iter().any(|own| own == c))
            {
                return id;
            }
        }
        "other"
    }
}

impl Entry {
    /// What the row is called.
    pub fn title(&self) -> &str {
        match self {
            Entry::App(app) => &app.name,
            Entry::Media(file) => &file.title,
            Entry::File(file) => &file.name,
            Entry::Folder(folder) => &folder.title,
            Entry::Choice(choice) => &choice.title,
            Entry::Bar(bar) => &bar.title,
            Entry::Search(search) => search.label(),
            Entry::Steam(_) => "Steam",
            Entry::Game(game) => &game.name,
            Entry::Facts(facts) => &facts.title,
            Entry::Typed(typed) => &typed.title,
        }
    }

    /// The line under the title, when there is one to say.
    pub fn comment(&self) -> Option<&str> {
        match self {
            Entry::App(app) => app.comment.as_deref(),
            // Where it was found, which for a music collection is the only
            // thing telling the album's copy of a track from the compilation's.
            Entry::Media(file) => Some(&file.folder),
            // How big it is and when it was written, which is what the folder
            // it is in cannot say: the folder is the column the user is
            // standing in and is on the screen already.
            Entry::File(file) => Some(file.note.as_str()).filter(|note| !note.is_empty()),
            Entry::Folder(folder) => folder.comment.as_deref(),
            Entry::Choice(choice) => choice.comment.as_deref(),
            Entry::Bar(bar) => bar.comment.as_deref(),
            Entry::Search(search) => Some(&search.note),
            Entry::Steam(service) => Some(&service.comment),
            Entry::Game(game) => Some(&game.note),
            Entry::Facts(facts) => Some(&facts.comment),
            Entry::Typed(typed) => Some(&typed.comment),
        }
    }

    pub fn icon(&self) -> Option<&str> {
        match self {
            Entry::App(app) => app.icon.as_deref(),
            Entry::Media(file) => Some(file.kind.glyph()),
            Entry::File(file) => Some(file.glyph),
            Entry::Folder(folder) => folder.icon.as_deref(),
            Entry::Choice(choice) => choice.icon.as_deref(),
            // The track is the drawing. A glyph beside it would be the name of
            // the setting again, which is on the row this column was opened
            // from and has not gone anywhere.
            Entry::Bar(_) => None,
            Entry::Search(search) => Some(search.icon()),
            Entry::Steam(_) | Entry::Game(_) => Some(crate::icons::STEAM),
            Entry::Facts(facts) => Some(&facts.icon),
            Entry::Typed(typed) => Some(&typed.icon),
        }
    }

    /// The search this row is about, if it is one of the two that are.
    pub fn search(&self) -> Option<&Search> {
        match self {
            Entry::Search(search) => Some(search),
            _ => None,
        }
    }

    /// The column this row opens into, if it opens into one.
    pub fn entries(&self) -> Option<&[Entry]> {
        match self {
            Entry::Folder(folder) => Some(&folder.entries),
            _ => None,
        }
    }

    /// The same column, to be changed: the shell's own rows hold state — which
    /// value is in force — and moving that mark means writing to the tree the
    /// bar is drawn from.
    pub fn entries_mut(&mut self) -> Option<&mut [Entry]> {
        match self {
            Entry::Folder(folder) => Some(&mut folder.entries),
            _ => None,
        }
    }

    /// What this row would launch, if launching is what it does.
    pub fn app(&self) -> Option<&App> {
        match self {
            Entry::App(app) => Some(app),
            _ => None,
        }
    }

    /// The Steam title this row is, if it is one.
    pub fn game(&self) -> Option<&Game> {
        match self {
            Entry::Game(game) => Some(game),
            _ => None,
        }
    }

    /// Whether this is the Steam row itself, and what it knows about the
    /// account.
    pub fn service(&self) -> Option<&Service> {
        match self {
            Entry::Steam(service) => Some(service),
            _ => None,
        }
    }

    /// The file this row stands for, if it stands for one of the user's own.
    pub fn media(&self) -> Option<&crate::media::File> {
        match self {
            Entry::Media(file) => Some(file.as_ref()),
            _ => None,
        }
    }

    /// The file this row stands for, if it is one the explorer found in a
    /// folder rather than one the walk shelved.
    pub fn file(&self) -> Option<&crate::files::Item> {
        match self {
            Entry::File(file) => Some(file),
            _ => None,
        }
    }

    /// The same, as the shared handle the shelf holds — for telling one row
    /// from another across a list that has been rebuilt, where comparing the
    /// handles is comparing two pointers and comparing the files means
    /// comparing two paths.
    pub fn shelved(&self) -> Option<&crate::media::Shelved> {
        match self {
            Entry::Media(file) => Some(file),
            _ => None,
        }
    }

    /// Whether pressing this row starts something: an application, or a player
    /// for a file. A subcategory leads somewhere and a value means something;
    /// neither is a process.
    pub fn starts_something(&self) -> bool {
        // A game keeps the catalogue meaningful whichever half of the column
        // it is in. A compatible installed one starts directly; another
        // answers with its concrete compatibility/install limitation. The
        // Steam service row itself only raises a panel, which is why the Games
        // column is separately exempt from being dropped; see `offer_steam`.
        matches!(
            self,
            Entry::App(_) | Entry::Media(_) | Entry::File(_) | Entry::Game(_)
        )
    }

    /// The colour this row stands for — see [`Choice::swatch`].
    pub fn swatch(&self) -> Option<Color> {
        match self {
            Entry::Choice(choice) => choice.swatch,
            Entry::Bar(bar) => bar.swatch,
            _ => None,
        }
    }

    /// The bar this row is, if it is one.
    pub fn bar(&self) -> Option<&Bar> {
        match self {
            Entry::Bar(bar) => Some(bar),
            _ => None,
        }
    }

    /// Whether this row is the value its column is currently set to.
    pub fn chosen(&self) -> bool {
        match self {
            Entry::Choice(choice) => choice.chosen,
            // A subcategory that is also one of a set of answers — see
            // [`Folder::chosen`]. It is asked here rather than only where the
            // tick is drawn so that the two things being chosen means happen
            // for it as well: the mark on the row, and the column opening on it
            // rather than on its first row.
            Entry::Folder(folder) => folder.chosen,
            _ => false,
        }
    }

    /// Whether pressing this row does a thing rather than answering the
    /// question its column asks — see [`Choice::acts`].
    ///
    /// Read in two places, and they are the two places the shell decides
    /// something on the user's behalf: which row a press marks, and which row
    /// the cursor is put on when a column is rewritten under it. Neither may
    /// land on one of these.
    pub fn acts(&self) -> bool {
        match self {
            Entry::Choice(choice) => choice.acts,
            _ => false,
        }
    }

    /// The setting this value would apply, if it is an editable value.
    pub fn setting(&self) -> Option<crate::settings::Setting> {
        match self {
            Entry::Choice(choice) => choice.setting,
            _ => None,
        }
    }

    /// What this row puts on screen, if it is one of the rows that opens onto
    /// a panel of values rather than onto a column.
    ///
    /// Asked where a press is being answered, alongside [`Entry::service`] and
    /// [`Entry::game`]: these are the rows of the bar whose press raises a
    /// panel rather than starting a process. See `Shell::start_selection`.
    pub fn facts(&self) -> Option<&Facts> {
        match self {
            Entry::Facts(facts) => Some(facts),
            _ => None,
        }
    }

    /// The value this row is typed into, if it is one of the rows that is.
    ///
    /// Asked where a press is being answered, beside [`Entry::facts`] and for
    /// the same reason: both raise a panel rather than starting a process, and
    /// neither goes through the choosing that moves a mark.
    pub fn typed(&self) -> Option<&Typed> {
        match self {
            Entry::Typed(typed) => Some(typed),
            _ => None,
        }
    }
}

/// Hand every entry in `entries` to `visit`, including those inside
/// subcategories.
///
/// The bar's columns are trees, so anything that has to see all of them — the
/// icon atlas being filled, an application being counted, a handler for a file
/// being looked for — has to walk rather than iterate.
///
/// What the visitor is handed borrows the catalogue rather than the walk, so a
/// caller may keep it: finding something in the tree is one of the things this
/// is for, and a search that could only answer "yes" would need a second walk
/// to say what it found.
pub fn walk<'a>(entries: &'a [Entry], visit: &mut impl FnMut(&'a Entry)) {
    for entry in entries {
        visit(entry);
        if let Some(children) = entry.entries() {
            walk(children, visit);
        }
    }
}

impl Category {
    /// Whether there is anything anywhere in this column that a press would
    /// start — an application, or one of the user's own files — subcategories
    /// included.
    ///
    /// Not the same as having no rows: the shell's own Settings column is full
    /// of rows and holds nothing that can be launched.
    pub fn has_launchable(&self) -> bool {
        let mut found = false;
        walk(&self.entries, &mut |entry| {
            found |= entry.starts_something()
        });
        found
    }

    /// How many applications this column holds, subcategories included.
    pub fn apps(&self) -> usize {
        let mut count = 0;
        walk(&self.entries, &mut |entry| {
            count += usize::from(entry.app().is_some())
        });
        count
    }

    /// What to say when this column has nothing in it.
    ///
    /// Only ever seen in the shell's own column, since a scanned one with no
    /// applications in it is dropped rather than drawn.
    pub fn empty_note(&self) -> &'static str {
        if self.id == SHELL_SETTINGS.0 {
            "LineXinBar's own settings will live here"
        } else {
            "No applications in this category"
        }
    }
}

fn is_true(value: Option<&String>) -> bool {
    value
        .map(|v| v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Whether `desktops` — the environments this session claims to be — allow an
/// entry to appear in a menu.
///
/// `OnlyShowIn` restricts an entry to the desktops it names and `NotShowIn`
/// bars it from them; both are `;`-separated lists, matched against the
/// `:`-separated names in `XDG_CURRENT_DESKTOP`. An entry naming neither is
/// shown everywhere.
///
/// Comparison ignores case. The spec's registered names are upper case by
/// convention rather than by rule, and entries in the wild are written both
/// ways for the same desktop.
fn shown_in(fields: &BTreeMap<String, String>, desktops: &[String]) -> bool {
    let names_this_session = |value: &String| {
        value
            .split(';')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .any(|name| desktops.iter().any(|ours| ours.eq_ignore_ascii_case(name)))
    };

    if fields
        .get("OnlyShowIn")
        .is_some_and(|v| !names_this_session(v))
    {
        return false;
    }
    !fields.get("NotShowIn").is_some_and(names_this_session)
}

/// The desktop names this session answers to.
///
/// LineXinBar's session sets `XDG_CURRENT_DESKTOP=LineXinBar`, and so does the
/// compositor for everything it launches, so the shell sees the same identity
/// nested as it does on its own. Nothing further is claimed on its behalf: an
/// entry written for one specific other desktop is written for that desktop's
/// session, not for this one.
fn current_desktops() -> Vec<String> {
    std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .split(':')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect()
}

/// Prefer a plain `Name`; localisation is left to the user's locale only when
/// an exact match exists, since partial matching tends to pick the wrong one.
fn localised(fields: &BTreeMap<String, String>, key: &str) -> Option<String> {
    if let Some(locale) = current_locale() {
        if let Some(value) = fields.get(&format!("{key}[{locale}]")) {
            return Some(value.clone());
        }
        // `pt_BR` also matches a bare `pt` entry.
        if let Some((lang, _)) = locale.split_once('_') {
            if let Some(value) = fields.get(&format!("{key}[{lang}]")) {
                return Some(value.clone());
            }
        }
    }
    fields.get(key).cloned()
}

fn current_locale() -> Option<String> {
    for var in ["LC_MESSAGES", "LC_ALL", "LANG"] {
        if let Ok(value) = std::env::var(var) {
            let value = value.split('.').next().unwrap_or("").to_string();
            if !value.is_empty() && value != "C" && value != "POSIX" {
                return Some(value);
            }
        }
    }
    None
}

/// Remove `%f`, `%U`, ... from an `Exec` line.
///
/// We launch applications with no arguments, so every field code expands to
/// nothing. `%%` is an escaped literal percent.
fn strip_field_codes(exec: &str) -> String {
    let mut out = String::with_capacity(exec.len());
    let mut chars = exec.chars().peekable();

    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('%') => out.push('%'),
            // Known codes all expand to nothing for an argument-less launch.
            Some('f' | 'F' | 'u' | 'U' | 'd' | 'D' | 'n' | 'N' | 'i' | 'c' | 'k' | 'v' | 'm') => {}
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }

    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Scan the system for applications and group them into columns.
///
/// Entries earlier in the search path win, so a user's override in
/// `~/.local/share/applications` replaces the system copy of the same id.
pub fn scan() -> Vec<Category> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut apps: Vec<App> = Vec::new();

    for dir in crate::xdg_data_dirs("applications") {
        collect_from_dir(&dir, &dir, &mut seen, &mut apps);
    }

    assemble(apps)
}

/// Sort discovered applications into the bar's columns.
///
/// Split from [`scan`] so the arrangement can be exercised without a
/// filesystem to arrange.
fn assemble(apps: Vec<App>) -> Vec<Category> {
    let (id, title, icon) = SHELL_SETTINGS;
    let shell_settings = Category {
        id,
        title,
        icon,
        entries: crate::settings::column(),
    };

    let mut sorted: Vec<Vec<App>> = CATEGORY_TABLE.iter().map(|_| Vec::new()).collect();
    for app in apps {
        let id = app.category_id();
        if let Some(index) = CATEGORY_TABLE.iter().position(|(own, ..)| *own == id) {
            sorted[index].push(app);
        }
    }

    let mut categories: Vec<Category> = CATEGORY_TABLE
        .iter()
        .zip(&mut sorted)
        .map(|((id, title, icon, _), apps)| {
            apps.sort_by_key(|a| a.name.to_lowercase());
            let mut entries = subcategories(id);
            entries.extend(apps.drain(..).map(Entry::App));
            Category {
                id,
                title,
                icon,
                entries,
            }
        })
        .collect();

    // Empty columns would just be dead space to scroll past. Measured in what
    // can be started rather than in rows, because a column now carries rows of
    // its own: Multimedia with nothing installed under it is two empty
    // subcategories, which is still a column with nothing in it to reach. It
    // earns its place back the moment the walk finds a file to put in one —
    // see [`shelve_media`].
    //
    // The shell's own is exempt: it is a fixed part of the bar rather than a
    // consequence of what happens to be installed, and nothing in it launches.
    //
    // System is exempt on the same terms. Its Files row is a fixed part of the
    // bar — every machine has a disk — and it holds nothing launchable until
    // somebody has walked down to a file, which is exactly the state a column
    // dropped here would never let them reach.
    categories.retain(|column| column.id == SYSTEM || column.has_launchable());
    categories.insert(0, shell_settings);
    categories
}

/// Recurse into a directory, tracking the desktop-file id so duplicates across
/// search paths collapse to one entry.
fn collect_from_dir(root: &Path, dir: &Path, seen: &mut HashSet<String>, apps: &mut Vec<App>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_from_dir(root, &path, seen, apps);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
            continue;
        }

        // The id is the path below the search root, with `/` turned into `-`.
        let id = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('/', "-");
        if !seen.insert(id) {
            continue;
        }

        if let Some(app) = App::from_file(&path) {
            apps.push(app);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(raw: &str) -> Option<App> {
        App::parse(raw, Path::new("/tmp/test.desktop"))
    }

    // --- Steam --------------------------------------------------------------

    /// One made-up title, as the shell holds it.
    fn game(app_id: u32, name: &str, installed: bool) -> Entry {
        Entry::Game(Game {
            app_id,
            name: name.to_string(),
            note: if installed {
                "Installed"
            } else {
                "Not installed"
            }
            .to_string(),
            installed,
            updating: false,
            steam_client: true,
        })
    }

    /// A catalogue with one application in the Games column, and one in a
    /// column after it, so that where things land can be seen.
    fn catalogue() -> Vec<Category> {
        assemble(vec![
            App::parse(
                "[Desktop Entry]\nType=Application\nName=A Puzzle\nExec=puzzle\nCategories=Game;\n",
                Path::new("/usr/share/applications/puzzle.desktop"),
            )
            .expect("a well-formed entry"),
            App::parse(
                "[Desktop Entry]\nType=Application\nName=An Editor\nExec=edit\nCategories=Development;\n",
                Path::new("/usr/share/applications/edit.desktop"),
            )
            .expect("a well-formed entry"),
        ])
    }

    fn column<'a>(categories: &'a [Category], id: &str) -> Option<&'a Category> {
        categories.iter().find(|column| column.id == id)
    }

    /// The Steam row goes at the head of the Games column, and says which
    /// account it is about.
    #[test]
    fn the_steam_row_stands_at_the_head_of_games() {
        let mut categories = catalogue();
        assert_eq!(offer_steam(&mut categories, None), Shifted::default());

        let games = column(&categories, GAMES).expect("the Games column");
        assert!(matches!(games.entries.first(), Some(Entry::Steam(_))));
        assert_eq!(games.entries[0].title(), "Steam");
        assert_eq!(
            games.entries[0].comment(),
            Some("Sign in to play your Steam library here")
        );
        assert_eq!(
            games.entries[1].title(),
            "A Puzzle",
            "the row went in above"
        );

        // Signed in, the same row says whose library it leads to — and there
        // is still only one of it.
        offer_steam(&mut categories, Some("someone".to_string()));
        let games = column(&categories, GAMES).expect("the Games column");
        assert_eq!(games.entries[0].comment(), Some("Signed in as someone"));
        assert_eq!(
            games
                .entries
                .iter()
                .filter(|row| row.service().is_some())
                .count(),
            1
        );
    }

    /// A machine with no game installed has no Games column to put the row in,
    /// and the offer to sign in is enough to earn it one: the whole library is
    /// one press from that row.
    #[test]
    fn the_row_earns_games_a_column_on_a_machine_with_no_games() {
        let mut categories = assemble(vec![App::parse(
            "[Desktop Entry]\nType=Application\nName=An Editor\nExec=edit\nCategories=Development;\n",
            Path::new("/usr/share/applications/edit.desktop"),
        )
        .expect("a well-formed entry")]);
        assert!(column(&categories, GAMES).is_none(), "nothing to put in it");

        let shifted = offer_steam(&mut categories, None);
        let at = shifted.added.expect("a column was made");
        assert_eq!(categories[at].id, GAMES);
        assert!(
            at < categories
                .iter()
                .position(|c| c.id == "development")
                .unwrap(),
            "the column landed out of the bar's order"
        );
    }

    /// Where a `.desktop` entry for the Steam client exists, this row takes
    /// its place. Two rows called Steam — one starting a program, one signing
    /// an account in — is something a user would have to press to tell apart.
    #[test]
    fn the_row_replaces_the_steam_clients_own_entry() {
        let mut categories = assemble(vec![
            App::parse(
                "[Desktop Entry]\nType=Application\nName=Steam\nExec=/usr/bin/steam %U\nCategories=Game;\n",
                Path::new("/usr/share/applications/steam.desktop"),
            )
            .expect("a well-formed entry"),
            App::parse(
                "[Desktop Entry]\nType=Application\nName=Steam (Runtime)\nExec=steam-runtime\nCategories=Game;\n",
                Path::new("/usr/share/applications/steam-runtime.desktop"),
            )
            .expect("a well-formed entry"),
        ]);
        hide_steam_client(&mut categories);
        offer_steam(&mut categories, None);

        let games = column(&categories, GAMES).expect("the Games column");
        let rows: Vec<&str> = games.entries.iter().map(Entry::title).collect();
        assert_eq!(
            rows,
            vec!["Steam", "Steam (Runtime)"],
            "there are two rows called Steam, or the wrong one went"
        );
        assert!(
            games.entries[0].service().is_some(),
            "the client's own entry is still on the bar beside this row"
        );
        assert!(
            games.entries[1].app().is_some(),
            "a different program that happens to start with Steam was taken out"
        );
    }

    /// Valve's own file, verbatim in the part that matters: it declares three
    /// main categories and `Network` is the one that wins, so the client's row
    /// is filed under Internet and a sweep that only looked in Games would
    /// leave it on the bar beside the shell's own.
    #[test]
    fn the_clients_entry_goes_from_whatever_column_it_was_filed_in() {
        let mut categories = assemble(vec![
            App::parse(
                "[Desktop Entry]\nType=Application\nName=Steam\nExec=/usr/bin/steam %U\nCategories=Network;FileTransfer;Game;\n",
                Path::new("/usr/share/applications/steam.desktop"),
            )
            .expect("a well-formed entry"),
            App::parse(
                "[Desktop Entry]\nType=Application\nName=Browser\nExec=browse\nCategories=Network;\n",
                Path::new("/usr/share/applications/browse.desktop"),
            )
            .expect("a well-formed entry"),
        ]);
        assert_eq!(
            column(&categories, "internet")
                .expect("the Internet column")
                .apps(),
            2,
            "the client's entry was filed somewhere other than Internet"
        );

        hide_steam_client(&mut categories);
        offer_steam(&mut categories, Some("someone".to_string()));

        let internet = column(&categories, "internet").expect("the Internet column");
        let rows: Vec<&str> = internet.entries.iter().map(Entry::title).collect();
        assert_eq!(rows, vec!["Browser"], "the client's entry is still listed");
        let games = column(&categories, GAMES).expect("the Games column");
        assert!(
            games.entries[0].service().is_some(),
            "the shell's own row did not go up in its place"
        );
    }

    /// And the column it was the only thing in goes with it, by the rule every
    /// scanned column is kept or dropped by: a column with nothing left in it
    /// to start is dead space to scroll past.
    #[test]
    fn a_column_the_client_was_alone_in_goes_with_it() {
        let mut categories = assemble(vec![App::parse(
            "[Desktop Entry]\nType=Application\nName=Steam\nExec=/usr/bin/steam %U\nCategories=Network;FileTransfer;Game;\n",
            Path::new("/usr/share/applications/steam.desktop"),
        )
        .expect("a well-formed entry")]);
        assert!(column(&categories, "internet").is_some(), "nothing to drop");

        hide_steam_client(&mut categories);

        assert!(
            column(&categories, "internet").is_none(),
            "an empty Internet column was left on the bar"
        );
        assert!(
            column(&categories, SHELL_SETTINGS.0).is_some(),
            "the shell's own column has nothing launchable in it and was dropped"
        );
    }

    /// The row is what says a session does Steam at all — the question asked
    /// before a bar rebuilt from a fresh scan is given back what Steam put on
    /// it. A session started with `--no-steam` has no row and gets none.
    #[test]
    fn the_row_is_what_says_this_session_does_steam() {
        let mut categories = catalogue();
        assert!(!steam_offered(&categories));
        offer_steam(&mut categories, None);
        assert!(steam_offered(&categories));
    }

    /// The library becomes a column of its own, immediately after Games —
    /// which is where somebody who has just looked at what is installed will
    /// step next.
    #[test]
    fn the_library_becomes_the_column_after_games() {
        let mut categories = catalogue();
        offer_steam(&mut categories, Some("someone".to_string()));

        let shifted = shelve_steam(
            &mut categories,
            vec![game(1, "Installed", true), game(2, "Owned", false)],
        );
        let at = shifted.added.expect("a column was made");
        assert_eq!(shifted.removed, None);
        assert_eq!(categories[at].id, steam_column());
        assert_eq!(categories[at].title, "Steam");
        assert_eq!(categories[at - 1].id, GAMES, "it did not land after Games");
        assert!(categories[at].has_launchable());

        // A second delivery replaces the rows rather than making a second
        // column.
        let shifted = shelve_steam(&mut categories, vec![game(1, "Installed", true)]);
        assert_eq!(shifted, Shifted::default());
        assert_eq!(categories[at].entries.len(), 1);
    }

    /// Signing out takes the column away rather than leaving an empty one to
    /// scroll past — the same rule every scanned column is kept or dropped by.
    #[test]
    fn an_empty_library_has_no_column() {
        let mut categories = catalogue();
        offer_steam(&mut categories, Some("someone".to_string()));
        let at = shelve_steam(&mut categories, vec![game(1, "Installed", true)])
            .added
            .expect("a column was made");

        let shifted = shelve_steam(&mut categories, Vec::new());
        assert_eq!(shifted.removed, Some(at));
        assert!(column(&categories, steam_column()).is_none());

        // And doing it again is not news.
        assert_eq!(
            shelve_steam(&mut categories, Vec::new()),
            Shifted::default()
        );
    }

    /// Both Steam rows are drawn from the shell's own glyphs, so a machine
    /// with no icon theme still has a column it can read.
    #[test]
    fn every_steam_row_wears_a_built_in_glyph() {
        let mut categories = catalogue();
        offer_steam(&mut categories, None);
        shelve_steam(&mut categories, vec![game(1, "Installed", true)]);

        let steam = column(&categories, steam_column()).expect("the Steam column");
        assert_eq!(steam.icon, crate::icons::CATEGORY_STEAM);
        assert_eq!(steam.entries[0].icon(), Some(crate::icons::STEAM));
        let games = column(&categories, GAMES).expect("the Games column");
        assert_eq!(games.entries[0].icon(), Some(crate::icons::STEAM));

        let built_in: Vec<&str> = crate::icons::BUILTIN
            .iter()
            .map(|(name, _)| *name)
            .collect();
        assert!(built_in.contains(&crate::icons::CATEGORY_STEAM));
        assert!(built_in.contains(&crate::icons::STEAM));
    }

    /// A title keeps the catalogue non-empty whichever half of its column it
    /// is in, while the row that signs in does not pretend to be a title.
    #[test]
    fn a_title_starts_something_and_the_service_row_does_not() {
        assert!(game(1, "Here", true).starts_something());
        assert!(game(2, "Not here", false).starts_something());
        assert!(!Entry::Steam(Service::new(None)).starts_something());
        assert!(!Entry::Steam(Service::new(Some("someone".to_string()))).starts_something());
    }

    #[test]
    fn parses_a_basic_entry() {
        let app = parse(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=Text Editor\n\
             Comment=Edit text\n\
             Exec=gedit %U\n\
             Icon=accessories-text-editor\n\
             Categories=Utility;TextEditor;\n",
        )
        .unwrap();

        assert_eq!(app.name, "Text Editor");
        assert_eq!(app.exec, "gedit");
        assert_eq!(app.icon.as_deref(), Some("accessories-text-editor"));
        assert_eq!(app.category_id(), "utilities");
    }

    /// The real entries, as installed on the machine this was written for.
    /// Pressing any of these tiles while the application is running has to
    /// find the window rather than start a second copy.
    #[test]
    fn a_running_window_is_recognised_from_its_desktop_entry() {
        // Firefox declares the answer outright.
        let firefox = App::parse(
            "[Desktop Entry]\nType=Application\nName=Firefox\n\
             Exec=/usr/lib/firefox/firefox %u\nStartupWMClass=firefox\n",
            Path::new("/usr/share/applications/firefox.desktop"),
        )
        .unwrap();
        assert!(firefox.owns_window("firefox"));
        // Flatpaks of the same application name themselves in reverse DNS.
        assert!(firefox.owns_window("org.mozilla.firefox"));
        assert!(!firefox.owns_window("chromium"));

        // Steam declares nothing, and its X11 class is capitalised where the
        // entry is not.
        let steam = App::parse(
            "[Desktop Entry]\nType=Application\nName=Steam\nExec=/usr/bin/steam %U\n",
            Path::new("/usr/share/applications/steam.desktop"),
        )
        .unwrap();
        assert!(steam.owns_window("Steam"));
        assert!(steam.owns_window("steam"));

        // And an entry whose file name says nothing is still matched by the
        // program it runs.
        let dolphin = App::parse(
            "[Desktop Entry]\nType=Application\nName=Files\nExec=dolphin %u\n",
            Path::new("/usr/share/applications/org.kde.dolphin.desktop"),
        )
        .unwrap();
        assert!(dolphin.owns_window("org.kde.dolphin"));
        assert!(dolphin.owns_window("dolphin"));
    }

    /// The other direction, which is the one that costs the user something: a
    /// tile that matched the wrong window would refuse to start the
    /// application and raise somebody else's instead.
    #[test]
    fn an_unrelated_window_is_not_this_application() {
        let app = App::parse(
            "[Desktop Entry]\nType=Application\nName=Text Editor\nExec=gedit %U\n",
            Path::new("/usr/share/applications/gedit.desktop"),
        )
        .unwrap();
        assert!(!app.owns_window("kate"));
        assert!(!app.owns_window("org.gnome.TextEditor"));
        // A window whose client named itself nothing is evidence of nothing.
        assert!(!app.owns_window(""));
        assert!(!app.owns_window("   "));
    }

    /// A wrapper is not a window name: every flatpak would otherwise be the
    /// same application, and everything started through a shell would be
    /// `sh`.
    #[test]
    fn the_program_behind_a_wrapper_is_what_counts() {
        let flatpak = App::parse(
            "[Desktop Entry]\nType=Application\nName=Zen\n\
             Exec=flatpak run app.zen_browser.zen @@u %u @@\n",
            Path::new("/tmp/app.zen_browser.zen.desktop"),
        )
        .unwrap();
        assert!(flatpak.owns_window("app.zen_browser.zen"));
        assert!(!flatpak.owns_window("flatpak"));

        let wrapped = App::parse(
            "[Desktop Entry]\nType=Application\nName=Thing\nExec=env FOO=1 thing\n",
            Path::new("/tmp/thing-entry.desktop"),
        )
        .unwrap();
        assert!(!wrapped.owns_window("env"));
    }

    /// Everything under Wine calls itself `something.exe`, and the extension
    /// is not the application. Read as a reverse-DNS tail it made every
    /// Windows program in the session one application: a game's audio stream
    /// arrives at the mixer as `Restory.exe`, the only entry on this machine
    /// declaring an `.exe` window class is Affinity's, and the running game's
    /// volume was drawn under Affinity's name and icon.
    #[test]
    fn one_windows_program_is_not_another() {
        let affinity = App::parse(
            "[Desktop Entry]\nType=Application\nName=Affinity\n\
             Exec=sh -c /usr/bin/affinity\nStartupWMClass=Affinity.exe\n",
            Path::new("/home/user/.local/share/applications/affinity.desktop"),
        )
        .unwrap();

        assert!(
            !affinity.owns_window("Restory.exe"),
            "a game under Proton is not the one graphics editor that runs under Wine"
        );
        assert!(!affinity.owns_window("d3ddriverquery64.exe"));
        assert!(!same_application("photo.exe", "designer.exe"));

        // And the application does still own its own windows — including the
        // one spelling the extension off, which the old rule got wrong too.
        assert!(affinity.owns_window("Affinity.exe"));
        assert!(affinity.owns_window("affinity.exe"));
        assert!(affinity.owns_window("Affinity"));
    }

    /// The reverse-DNS rule this is all about is untouched: it is for a name
    /// with a vendor in the middle, not for anything that merely has a dot.
    #[test]
    fn a_reverse_dns_name_still_finds_its_program() {
        assert!(same_application("org.mozilla.firefox", "firefox"));
        assert!(same_application("app.zen_browser.zen", "zen"));
        assert!(same_application("org.kde.dolphin", "Dolphin"));
        // A Unity game's class is its binary, dot and all, and its last
        // component is an architecture rather than a name.
        assert!(!same_application("Haste.x86_64", "Celeste.x86_64"));
    }

    #[test]
    fn skips_hidden_and_non_applications() {
        assert!(
            parse("[Desktop Entry]\nType=Application\nName=X\nExec=x\nNoDisplay=true\n").is_none()
        );
        assert!(
            parse("[Desktop Entry]\nType=Application\nName=X\nExec=x\nHidden=true\n").is_none()
        );
        assert!(parse("[Desktop Entry]\nType=Link\nName=X\nURL=http://x\n").is_none());
        assert!(parse("[Desktop Entry]\nType=Application\nName=X\n").is_none());
    }

    fn fields(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn desktop_scoping_is_matched_rather_than_assumed() {
        let ours = ["LineXinBar".to_string()];

        // An entry naming this desktop is ours to show, whichever way round it
        // is written, and whatever else it lists alongside.
        assert!(shown_in(&fields(&[("OnlyShowIn", "LineXinBar;")]), &ours));
        assert!(shown_in(
            &fields(&[("OnlyShowIn", "KDE;linexinbar;")]),
            &ours
        ));
        assert!(shown_in(&fields(&[("NotShowIn", "KDE;GNOME;")]), &ours));

        // And one written for somebody else's session is not.
        assert!(!shown_in(&fields(&[("OnlyShowIn", "KDE;")]), &ours));
        assert!(!shown_in(&fields(&[("NotShowIn", "LineXinBar;")]), &ours));

        // Both keys at once: each has to be satisfied.
        let both = fields(&[("OnlyShowIn", "LineXinBar;"), ("NotShowIn", "LineXinBar;")]);
        assert!(!shown_in(&both, &ours));

        // An empty list names no desktop, so it can only exclude.
        assert!(!shown_in(&fields(&[("OnlyShowIn", "")]), &ours));
        assert!(shown_in(&fields(&[("NotShowIn", "")]), &ours));

        // Saying nothing means everywhere, including a session that has no
        // identity at all to match against.
        assert!(shown_in(&fields(&[]), &ours));
        assert!(shown_in(&fields(&[]), &[]));
        assert!(!shown_in(&fields(&[("OnlyShowIn", "KDE;")]), &[]));
    }

    #[test]
    fn several_session_desktops_all_count() {
        // `XDG_CURRENT_DESKTOP` is a list, and an entry naming any one of its
        // names belongs to this session.
        let ours = ["LineXinBar".to_string(), "KDE".to_string()];
        assert!(shown_in(&fields(&[("OnlyShowIn", "KDE;")]), &ours));
        assert!(!shown_in(&fields(&[("NotShowIn", "KDE;")]), &ours));
    }

    #[test]
    fn the_shell_settings_column_is_always_first_and_always_there() {
        let empty = assemble(Vec::new());
        assert_eq!(
            empty.iter().map(|c| c.id).collect::<Vec<_>>(),
            ["settings", SYSTEM],
            "nothing installed leaves the shell's own two"
        );
        assert_eq!(empty[0].id, "settings");
        assert_eq!(empty[0].title, "Settings");
        // Rows of its own, none of which is an application: the column is the
        // shell's own controls rather than anything found on disk.
        assert!(!empty[0].entries.is_empty());
        assert_eq!(empty[0].apps(), 0);

        // Nothing found on disk lands in it, and it keeps its place ahead of
        // everything that was.
        let app =
            parse("[Desktop Entry]\nType=Application\nName=X\nExec=x\nCategories=Settings;\n")
                .unwrap();
        let categories = assemble(vec![app]);
        assert_eq!(
            categories.iter().map(|c| c.id).collect::<Vec<_>>(),
            ["settings", "system"]
        );
        assert_eq!(categories[0].apps(), 0);
        assert_eq!(categories[1].apps(), 1);
    }

    /// An application inside a subcategory is still an application in that
    /// column: everything that counts or catalogues one has to walk the tree
    /// rather than read the top of it.
    #[test]
    fn a_column_counts_what_its_subcategories_hold() {
        let buried = Category {
            id: "games",
            title: "Games",
            icon: "applications-games",
            entries: vec![Entry::Folder(Folder {
                title: "Emulators".into(),
                comment: None,
                icon: None,
                entries: vec![Entry::App(
                    parse("[Desktop Entry]\nType=Application\nName=X\nExec=x\n").unwrap(),
                )],
                place: None,
                chosen: false,
            })],
        };
        assert!(buried.has_launchable());
        assert_eq!(buried.apps(), 1);

        let hollow = Category {
            entries: vec![Entry::Folder(Folder {
                title: "Emulators".into(),
                comment: None,
                icon: None,
                entries: Vec::new(),
                place: None,
                chosen: false,
            })],
            ..buried.clone()
        };
        assert!(
            !hollow.has_launchable(),
            "a subcategory is not an application"
        );
    }

    #[test]
    fn an_empty_column_says_which_kind_of_empty_it_is() {
        let categories = assemble(Vec::new());
        assert_eq!(
            categories[0].empty_note(),
            "LineXinBar's own settings will live here"
        );

        let scanned = Category {
            id: "games",
            title: "Games",
            icon: "applications-games",
            entries: Vec::new(),
        };
        assert_eq!(scanned.empty_note(), "No applications in this category");
    }

    #[test]
    fn ignores_keys_outside_the_main_group() {
        let app = parse(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=Real\n\
             Exec=real\n\
             [Desktop Action new]\n\
             Name=Action\n\
             Exec=other\n",
        )
        .unwrap();
        assert_eq!(app.name, "Real");
        assert_eq!(app.exec, "real");
    }

    #[test]
    fn strips_field_codes() {
        assert_eq!(strip_field_codes("prog %U"), "prog");
        assert_eq!(strip_field_codes("prog %f --flag"), "prog --flag");
        assert_eq!(strip_field_codes("prog 100%% done"), "prog 100% done");
        assert_eq!(strip_field_codes("prog -i %i -c %c"), "prog -i -c");
    }

    #[test]
    fn settings_and_system_share_a_column() {
        // As in Plasma, whose menu has no Settings menu of its own. The bar's
        // Settings column belongs to the shell, not to installed software.
        for raw in ["System;Settings", "Settings", "System"] {
            let app = parse(&format!(
                "[Desktop Entry]\nType=Application\nName=X\nExec=x\nCategories={raw};\n"
            ))
            .unwrap();
            assert_eq!(app.category_id(), "system", "for {raw}");
        }
    }

    #[test]
    fn unclassifiable_entries_fall_through_to_other() {
        // Unknown categories fall through to Other.
        let app = parse("[Desktop Entry]\nType=Application\nName=X\nExec=x\nCategories=Weird;\n")
            .unwrap();
        assert_eq!(app.category_id(), "other");

        // No categories at all also lands in Other.
        let app = parse("[Desktop Entry]\nType=Application\nName=X\nExec=x\n").unwrap();
        assert_eq!(app.category_id(), "other");
    }

    #[test]
    fn audio_and_video_fold_into_multimedia() {
        for raw in ["AudioVideo", "Audio", "Video"] {
            let app = parse(&format!(
                "[Desktop Entry]\nType=Application\nName=X\nExec=x\nCategories={raw};\n"
            ))
            .unwrap();
            assert_eq!(app.category_id(), "multimedia", "for {raw}");
        }
    }

    /// Multimedia is divided in the column rather than in the classifier: the
    /// two subcategories are there whatever is installed, and nothing is filed
    /// into them yet.
    #[test]
    fn multimedia_carries_its_two_subcategories() {
        let app =
            parse("[Desktop Entry]\nType=Application\nName=Player\nExec=x\nCategories=Audio;\n")
                .unwrap();
        let categories = assemble(vec![app]);
        let multimedia = categories.iter().find(|c| c.id == "multimedia").unwrap();

        let titles: Vec<&str> = multimedia.entries.iter().map(Entry::title).collect();
        assert_eq!(titles, ["Music", "Video", "Player"]);

        // Rows of the column, not applications in it — and empty, so the
        // application is still the only thing there is to launch.
        for row in &multimedia.entries[..2] {
            assert!(row.app().is_none());
            assert!(row.entries().is_some_and(<[Entry]>::is_empty));
        }
        assert_eq!(multimedia.apps(), 1);

        // Both are drawn with a glyph of the shell's own: a subcategory left
        // to the icon theme's fallback reads as an application that will not
        // start.
        assert_eq!(
            multimedia.entries[0].icon(),
            Some(crate::icons::CATEGORY_MUSIC)
        );
        assert_eq!(
            multimedia.entries[1].icon(),
            Some(crate::icons::CATEGORY_VIDEO)
        );
    }

    /// And they are structure rather than content: a machine with no
    /// multimedia application on it and nothing found on its disk has no
    /// Multimedia column, exactly as before they existed.
    #[test]
    fn two_empty_subcategories_are_not_a_column() {
        let categories = assemble(vec![parse(
            "[Desktop Entry]\nType=Application\nName=X\nExec=x\nCategories=Office;\n",
        )
        .unwrap()]);
        assert_eq!(
            categories.iter().map(|c| c.id).collect::<Vec<_>>(),
            ["settings", SYSTEM, "office"]
        );
    }

    /// System is the one scanned column that stands with nothing installed in
    /// it, because what it carries is not something installed: every machine
    /// has a disk, so the row that opens it is always there.
    #[test]
    fn the_files_row_is_on_system_whether_or_not_anything_is_installed() {
        for catalogue in [assemble(Vec::new()), catalogue()] {
            let system = column(&catalogue, SYSTEM).expect("System is always a column");
            let files = match &system.entries[0] {
                Entry::Folder(folder) => folder,
                other => panic!("expected the Files row at the head, got {other:?}"),
            };
            assert_eq!(files.title, "Files");
            assert_eq!(files.icon.as_deref(), Some(crate::icons::CATEGORY_FILES));
            assert_eq!(
                files.place,
                Some(crate::files::Place::Volumes),
                "it is read on the press that opens it, not now"
            );
            assert!(files.entries.is_empty(), "and it holds nothing until then");
        }
    }

    fn found(path: &str) -> crate::media::Shelved {
        std::sync::Arc::new(crate::media::File::at(Path::new(path)).expect("a listable file"))
    }

    /// Hang these files on the bar the way the worker's deliveries do — one
    /// shelf at a time — and say where a column had to be made.
    fn hang(categories: &mut Vec<Category>, files: Vec<crate::media::Shelved>) -> Vec<usize> {
        crate::media::made_from(files)
            .into_iter()
            .filter_map(|made| shelve_media(categories, made).column)
            .collect()
    }

    /// What the rows are now for: the user's own files, in order, with the row
    /// above saying how many there are.
    #[test]
    fn the_rows_hold_the_files_the_walk_found() {
        let player =
            parse("[Desktop Entry]\nType=Application\nName=Player\nExec=x\nCategories=Audio;\n")
                .unwrap();
        let editor =
            parse("[Desktop Entry]\nType=Application\nName=Paint\nExec=p\nCategories=Graphics;\n")
                .unwrap();
        let mut categories = assemble(vec![player, editor]);
        let files = || {
            vec![
                found("/home/x/Music/zebra.mp3"),
                found("/home/x/Music/apple.flac"),
                found("/home/x/Videos/holiday.mkv"),
                found("/home/x/Pictures/sunset.jpg"),
                found("/home/x/Desktop/Screenshot.png"),
            ]
        };

        // Both columns were already there, so nothing had to be made.
        assert!(hang(&mut categories, files()).is_empty());

        // Graphics gets its one row, above the tools, holding the pictures.
        let graphics = categories.iter().find(|c| c.id == GRAPHICS).unwrap();
        let titles: Vec<&str> = graphics.entries.iter().map(Entry::title).collect();
        assert_eq!(titles, ["Images", "Paint"]);
        assert_eq!(
            graphics.entries[0].icon(),
            Some(crate::icons::CATEGORY_IMAGES)
        );
        assert_eq!(
            graphics.entries[0].comment(),
            Some("2 images in your home folder")
        );
        let images: Vec<&str> = graphics.entries[0]
            .entries()
            .unwrap()
            .iter()
            .map(Entry::title)
            .collect();
        assert_eq!(
            images,
            ["Search", "Screenshot", "sunset"],
            "the field, and then the pictures alphabetically from any folder"
        );
        assert_eq!(graphics.apps(), 1, "the editor, and not the pictures");

        let multimedia = categories.iter().find(|c| c.id == MULTIMEDIA).unwrap();

        let music = multimedia.entries[0].entries().unwrap();
        let titles: Vec<&str> = music.iter().map(Entry::title).collect();
        assert_eq!(
            titles,
            ["Search", "apple", "zebra"],
            "alphabetical, not as found"
        );
        assert_eq!(
            multimedia.entries[0].comment(),
            Some("2 audio files in your home folder")
        );
        assert_eq!(
            multimedia.entries[1].comment(),
            Some("1 video file in your home folder")
        );

        // A file is a row that starts something, and is not an application:
        // nothing installed it and nothing here would offer to remove it.
        assert!(music[1].starts_something());
        assert!(music[1].app().is_none());
        assert!(music[1].media().is_some());
        // The field above them is none of those things. It starts nothing, so
        // the button that opens a file cannot open it by accident, and it is
        // not a file, so nothing that acts on one can act on it.
        assert!(!music[0].starts_something());
        assert!(music[0].media().is_none());
        assert_eq!(multimedia.apps(), 1, "the player, and not the music");

        // Publishing again replaces what is there rather than doubling it.
        hang(&mut categories, files());
        let multimedia = categories.iter().find(|c| c.id == MULTIMEDIA).unwrap();
        assert_eq!(multimedia.entries[0].entries().unwrap().len(), 3);
    }

    /// Music on a machine with no media player installed still deserves
    /// somewhere to be, and the column it earns stands where it always does.
    #[test]
    fn a_file_alone_earns_the_column_back() {
        let office =
            parse("[Desktop Entry]\nType=Application\nName=X\nExec=x\nCategories=Office;\n")
                .unwrap();
        let system =
            parse("[Desktop Entry]\nType=Application\nName=Y\nExec=y\nCategories=System;\n")
                .unwrap();
        let mut categories = assemble(vec![office, system]);
        assert_eq!(
            categories.iter().map(|c| c.id).collect::<Vec<_>>(),
            ["settings", "system", "office"]
        );

        assert_eq!(hang(&mut categories, vec![found("/home/x/a.mp3")]), vec![2]);
        assert_eq!(
            categories.iter().map(|c| c.id).collect::<Vec<_>>(),
            ["settings", "system", MULTIMEDIA, "office"],
            "in the bar's own order, not on the end"
        );
        assert!(categories[2].has_launchable());
        // The one song, with the field above it.
        assert_eq!(categories[2].entries[0].entries().unwrap().len(), 2);

        // And an empty library never makes one.
        let mut bare = assemble(Vec::new());
        assert!(hang(&mut bare, Vec::new()).is_empty());
        assert_eq!(bare.len(), 2, "the shell's own column, and System's Files");
    }

    /// Both columns can be earned in the same pass, and the second index is
    /// worked out in the bar the first one has already changed — which is why
    /// a cursor has to replay them in order.
    #[test]
    fn two_columns_can_arrive_together_and_are_reported_in_order() {
        let mut categories = assemble(vec![parse(
            "[Desktop Entry]\nType=Application\nName=X\nExec=x\nCategories=Office;\n",
        )
        .unwrap()]);
        assert_eq!(
            hang(
                &mut categories,
                vec![found("/home/x/a.mp3"), found("/home/x/b.png")]
            ),
            vec![2, 3]
        );
        assert_eq!(
            categories.iter().map(|c| c.id).collect::<Vec<_>>(),
            ["settings", SYSTEM, MULTIMEDIA, GRAPHICS, "office"]
        );

        // A kind with nothing found never conjures the column that holds it.
        let mut only_pictures = assemble(Vec::new());
        assert_eq!(
            hang(&mut only_pictures, vec![found("/home/x/b.png")]),
            vec![2]
        );
        assert_eq!(
            only_pictures.iter().map(|c| c.id).collect::<Vec<_>>(),
            ["settings", SYSTEM, GRAPHICS]
        );
    }

    /// The rows a shelf carries above its files, and when it carries them.
    #[test]
    fn a_shelf_is_headed_by_the_field_that_searches_it() {
        let songs = vec![found("/home/x/a.mp3"), found("/home/x/b.mp3")];

        // Unsearched: the field alone, saying what it is for. Nothing offers to
        // clear a search nobody has made.
        let rows = media_rows(songs.clone(), crate::media::Kind::Audio, "", 2);
        let titles: Vec<&str> = rows.iter().map(Entry::title).collect();
        assert_eq!(titles, ["Search", "a", "b"]);
        assert_eq!(rows[0].comment(), Some("Search audio files by name"));
        assert_eq!(rows[0].icon(), Some(crate::icons::SEARCH));

        // Searched: the field says what was typed rather than the word
        // "Search", and the way back to the whole shelf is the row under it.
        let rows = media_rows(vec![songs[0].clone()], crate::media::Kind::Audio, "a", 2);
        let titles: Vec<&str> = rows.iter().map(Entry::title).collect();
        assert_eq!(titles, ["a", "Clear search", "a"]);
        assert_eq!(rows[0].comment(), Some("1 of 2 audio files matches"));
        assert_eq!(rows[1].comment(), Some("Show all 2 audio files"));
        assert_eq!(rows[1].icon(), Some(crate::icons::SEARCH_CLEAR));

        // A search that found nothing keeps both rows all the same. A column
        // emptied of everything including the way out of it would be a place
        // the user could reach and not leave.
        let rows = media_rows(Vec::new(), crate::media::Kind::Audio, "zzz", 2);
        let titles: Vec<&str> = rows.iter().map(Entry::title).collect();
        assert_eq!(titles, ["zzz", "Clear search"]);
        assert_eq!(rows[0].comment(), Some("No audio files match"));

        // And a shelf with nothing on it at all carries neither: there is
        // nothing to search, and a column holding only the offer to search it
        // is a column worth opening for nothing.
        assert!(media_rows(Vec::new(), crate::media::Kind::Audio, "", 0).is_empty());
    }

    /// That field is also how a column says what it *is*, which is what tells
    /// the shell somebody has just stepped into a shelf and the disk is worth
    /// another look.
    #[test]
    fn a_column_of_the_users_own_files_says_which_shelf_it_is() {
        let songs = vec![found("/home/x/a.mp3")];
        let rows = media_rows(songs, crate::media::Kind::Audio, "", 1);
        assert_eq!(shelf_shown(&rows), Some(crate::media::Kind::Audio));

        // Narrowed to nothing, it is still the shelf it was: the field stands
        // whatever the search left under it.
        let rows = media_rows(Vec::new(), crate::media::Kind::Image, "zzz", 4);
        assert_eq!(shelf_shown(&rows), Some(crate::media::Kind::Image));

        // A column of applications is not one, and neither is a shelf so empty
        // it carries no rows — which is also a column nothing can step into.
        let installed = parse(
            "[Desktop Entry]\nType=Application\nName=Files\nExec=files\nCategories=Utility;\n",
        )
        .expect("a desktop entry");
        let columns = assemble(vec![installed]);
        let utilities = columns
            .iter()
            .find(|column| column.id == "utilities")
            .expect("the column the entry asked for");
        assert_eq!(shelf_shown(&utilities.entries), None);
        assert_eq!(shelf_shown(&[]), None);
    }

    /// The letter that has just been typed goes onto the field without waiting
    /// for the shelf behind it to be narrowed.
    #[test]
    fn what_is_typed_reaches_the_field_before_the_rows_do() {
        let mut categories = assemble(Vec::new());
        hang(
            &mut categories,
            vec![found("/home/x/song.mp3"), found("/home/x/pic.png")],
        );

        assert!(set_search_text(
            &mut categories,
            crate::media::Kind::Audio,
            "rad"
        ));
        let music = categories
            .iter()
            .find(|category| category.id == MULTIMEDIA)
            .unwrap();
        let rows = music.entries[0].entries().unwrap();
        assert_eq!(rows[0].title(), "rad");
        // Only the field moved. The rows under it are the ones the worker last
        // built, and they stay exactly as they were until it sends more —
        // which is what keeps the list and the count it is described by from
        // ever disagreeing with each other.
        assert_eq!(rows[1].title(), "song");
        assert_eq!(rows[0].comment(), Some("Search audio files by name"));

        // The shelf that was not being typed into is untouched.
        let pictures = categories
            .iter()
            .find(|category| category.id == GRAPHICS)
            .unwrap();
        assert_eq!(pictures.entries[0].entries().unwrap()[0].title(), "Search");

        // A kind with no column on the bar has no field to write to, and says
        // so rather than pretending it wrote one.
        assert!(!set_search_text(
            &mut categories,
            crate::media::Kind::Video,
            "x"
        ));
    }

    /// The column is made wherever it belongs, including at both ends.
    #[test]
    fn the_column_lands_in_the_bars_own_order() {
        let column = |id: &'static str| Category {
            id,
            title: "X",
            icon: "x",
            entries: Vec::new(),
        };
        // Only the shell's own, which is not in the table and is never landed
        // in front of.
        assert_eq!(column_place(&[column("settings")], MULTIMEDIA), 1);
        // Before everything that comes after it, after everything that does not.
        assert_eq!(
            column_place(&[column("settings"), column("games")], MULTIMEDIA),
            1
        );
        assert_eq!(
            column_place(&[column("settings"), column("system")], MULTIMEDIA),
            2
        );
        // And Graphics sits behind Multimedia, as the table has it.
        assert_eq!(
            column_place(&[column("settings"), column(MULTIMEDIA)], GRAPHICS),
            2
        );
        assert_eq!(
            column_place(&[column("settings"), column("internet")], GRAPHICS),
            1
        );
    }
}
